use crate::cli::{Network, DEFAULT_GAS};
use crate::credentials::{parse_account_id, Credentials};
use crate::rpc::{access_key, block_hash, full_access};
use anyhow::{anyhow, bail, Context, Result};
use near_api::advanced::{ExecuteSignedTransaction, TransactionableOrSigned};
use near_api::errors::ExecuteTransactionError;
use near_api::types::transaction::actions::{
    AddKeyAction, CreateAccountAction, DeleteAccountAction, DeployContractAction,
    FunctionCallAction,
};
use near_api::types::{
    AccessKey as ApiAccessKey, AccessKeyPermission as ApiAccessKeyPermission,
    AccountId as ApiAccountId, Action as ApiAction, CryptoHash as ApiCryptoHash, NearGas,
    PublicKey as ApiPublicKey, SecretKey as ApiSecretKey,
};
use near_api::{NetworkConfig, RPCEndpoint, Signer, Transaction};
use near_api_types::transaction::result::TransactionResult as ApiTransactionResult;
use near_crypto::{PublicKey, SecretKey};
use near_jsonrpc_client::JsonRpcClient;
use near_jsonrpc_client::{
    errors::{JsonRpcError, JsonRpcServerError},
    methods,
};
use near_jsonrpc_primitives::types::transactions::{RpcTransactionError, TransactionInfo};
use near_primitives::hash::CryptoHash;
use near_primitives::types::{AccountId, BlockReference, Nonce};
use near_primitives::views::{FinalExecutionStatus, TxExecutionStatus};
use near_token::NearToken;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct NonceKey {
    account_id: AccountId,
    public_key: String,
}

#[derive(Default, Debug)]
pub struct NonceTracker {
    // Stores the next nonce to use, keyed by the exact signing account/key pair.
    next_nonces: HashMap<NonceKey, Nonce>,
}

impl NonceTracker {
    fn key(credentials: &Credentials) -> NonceKey {
        NonceKey {
            account_id: credentials.account_id.clone(),
            public_key: credentials.secret_key.public_key().to_string(),
        }
    }

    fn next_after(nonce: Nonce) -> Result<Nonce> {
        nonce
            .checked_add(1)
            .ok_or_else(|| anyhow!("transaction nonce overflow"))
    }

    fn merge_rpc_nonce(&mut self, credentials: &Credentials, rpc_nonce: Nonce) -> Result<Nonce> {
        let next_nonce = Self::next_after(rpc_nonce)?;
        let entry = self
            .next_nonces
            .entry(Self::key(credentials))
            .or_insert(next_nonce);
        if *entry < next_nonce {
            *entry = next_nonce;
        }
        Ok(*entry)
    }

    pub async fn next_nonce(
        &mut self,
        client: &JsonRpcClient,
        credentials: &Credentials,
    ) -> Result<Nonce> {
        let key = Self::key(credentials);
        if let Some(nonce) = self.next_nonces.get(&key).copied() {
            return Ok(nonce);
        }
        let access_key = access_key(
            client,
            BlockReference::Finality(near_primitives::types::Finality::Final),
            credentials,
        )
        .await?;
        if !full_access(&access_key.permission) {
            bail!("signer access key is not full access");
        }
        self.merge_rpc_nonce(credentials, access_key.nonce)
    }

    pub fn confirmed(&mut self, credentials: &Credentials, used_nonce: Nonce) -> Result<()> {
        self.merge_rpc_nonce(credentials, used_nonce).map(|_| ())
    }

    pub async fn refresh(
        &mut self,
        client: &JsonRpcClient,
        credentials: &Credentials,
    ) -> Result<Nonce> {
        let access_key = access_key(
            client,
            BlockReference::Finality(near_primitives::types::Finality::Final),
            credentials,
        )
        .await?;
        if !full_access(&access_key.permission) {
            bail!("signer access key is not full access");
        }
        self.merge_rpc_nonce(credentials, access_key.nonce)
    }

    pub fn invalidate(&mut self, credentials: &Credentials) {
        self.next_nonces.remove(&Self::key(credentials));
    }
}

fn api_account_id(account_id: &AccountId) -> Result<ApiAccountId> {
    account_id
        .to_string()
        .parse()
        .with_context(|| format!("could not convert account ID {account_id} for near-api"))
}

fn api_crypto_hash(hash: CryptoHash) -> Result<ApiCryptoHash> {
    hash.to_string()
        .parse()
        .context("could not convert block hash for near-api")
}

fn api_secret_key(secret_key: &SecretKey) -> Result<ApiSecretKey> {
    secret_key
        .to_string()
        .parse()
        .context("could not convert signing key for near-api")
}

pub fn api_network(network: Network) -> NetworkConfig {
    let endpoint = match network {
        Network::Testnet => RPCEndpoint::testnet(),
        Network::Mainnet => RPCEndpoint::mainnet(),
    }
    .with_retries(1);
    let mut config = match network {
        Network::Testnet => NetworkConfig::testnet(),
        Network::Mainnet => NetworkConfig::mainnet(),
    };
    config.rpc_endpoints = vec![endpoint];
    config
}

async fn make_transaction(
    signer: &Credentials,
    receiver_id: AccountId,
    nonce: Nonce,
    block_hash: CryptoHash,
    actions: Vec<ApiAction>,
) -> Result<ExecuteSignedTransaction> {
    let api_signer = Signer::from_secret_key(api_secret_key(&signer.secret_key)?)
        .context("could not create near-api signer")?;
    let mut transaction = Transaction::construct(
        api_account_id(&signer.account_id)?,
        api_account_id(&receiver_id)?,
    );
    for action in actions {
        transaction = transaction.add_action(action);
    }
    transaction
        .with_signer(api_signer.clone())
        .presign_offline(
            api_signer
                .get_public_key()
                .await
                .context("could not read near-api public key")?,
            api_crypto_hash(block_hash)?,
            nonce,
        )
        .await
        .map_err(|error: ExecuteTransactionError| anyhow!("near-api signing failed: {error}"))
}

const TRANSACTION_STATUS_POLL_ATTEMPTS: usize = 30;
const TRANSACTION_STATUS_POLL_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
const TRANSACTION_STATUS_QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const TRANSACTION_BROADCAST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug)]
pub struct UnresolvedTransaction {
    pub tx_hash: CryptoHash,
}

#[derive(Debug)]
pub struct ConfirmedTransactionFailure {
    pub tx_hash: CryptoHash,
    pub detail: String,
}

impl std::fmt::Display for ConfirmedTransactionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "transaction {} executed but failed: {}",
            self.tx_hash, self.detail
        )
    }
}

impl std::error::Error for ConfirmedTransactionFailure {}

impl std::fmt::Display for UnresolvedTransaction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "transaction {} remains unresolved after broadcast",
            self.tx_hash
        )
    }
}

impl std::error::Error for UnresolvedTransaction {}

pub fn is_unresolved_transaction(error: &anyhow::Error) -> bool {
    error.downcast_ref::<UnresolvedTransaction>().is_some()
}

pub fn is_confirmed_transaction_failure(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ConfirmedTransactionFailure>()
        .is_some()
}

fn is_pre_broadcast_near_api_error(error: &ExecuteTransactionError) -> bool {
    matches!(
        error,
        ExecuteTransactionError::ArgumentValidationError(_)
            | ExecuteTransactionError::PreQueryError(_)
            | ExecuteTransactionError::ValidationError(_)
            | ExecuteTransactionError::MetaSignError(_)
            | ExecuteTransactionError::SignerError(_)
            | ExecuteTransactionError::DataConversionError(_)
    )
}

fn is_retryable_status_error(error: &JsonRpcError<RpcTransactionError>) -> bool {
    matches!(
        error,
        JsonRpcError::TransportError(_)
            | JsonRpcError::ServerError(JsonRpcServerError::HandlerError(
                RpcTransactionError::TimeoutError | RpcTransactionError::UnknownTransaction { .. }
            ))
    )
}

async fn reconcile_transaction(
    client: &JsonRpcClient,
    tx_hash: CryptoHash,
    signer_id: AccountId,
) -> Result<()> {
    for attempt in 1..=TRANSACTION_STATUS_POLL_ATTEMPTS {
        let response = tokio::time::timeout(
            TRANSACTION_STATUS_QUERY_TIMEOUT,
            client.call(methods::tx::RpcTransactionStatusRequest {
                transaction_info: TransactionInfo::TransactionId {
                    tx_hash,
                    sender_account_id: signer_id.clone(),
                },
                wait_until: TxExecutionStatus::Final,
            }),
        )
        .await;

        match response {
            Err(_) => {}
            Ok(Ok(response)) => {
                let Some(outcome) = response.final_execution_outcome else {
                    if attempt == TRANSACTION_STATUS_POLL_ATTEMPTS {
                        break;
                    }
                    tokio::time::sleep(TRANSACTION_STATUS_POLL_DELAY).await;
                    continue;
                };
                match &outcome.into_outcome().status {
                    FinalExecutionStatus::SuccessValue(_) => return Ok(()),
                    FinalExecutionStatus::Failure(error) => {
                        return Err(ConfirmedTransactionFailure {
                            tx_hash,
                            detail: format!("{error:?}"),
                        }
                        .into());
                    }
                    FinalExecutionStatus::NotStarted | FinalExecutionStatus::Started => {}
                }
            }
            Ok(Err(error)) if is_retryable_status_error(&error) => {}
            Ok(Err(error)) => {
                eprintln!("could not query status for transaction {tx_hash}: {error}");
                return Err(UnresolvedTransaction { tx_hash }.into());
            }
        }

        if attempt < TRANSACTION_STATUS_POLL_ATTEMPTS {
            tokio::time::sleep(TRANSACTION_STATUS_POLL_DELAY).await;
        }
    }

    Err(UnresolvedTransaction { tx_hash }.into())
}

#[derive(Debug, Eq, PartialEq)]
struct SubmissionOutcome {
    was_ambiguous: bool,
}

async fn submit(
    client: &JsonRpcClient,
    signed_transaction: ExecuteSignedTransaction,
    network: &NetworkConfig,
) -> Result<SubmissionOutcome> {
    // The transaction is presigned once. Capture its hash before the single near-api broadcast;
    // a transport timeout is ambiguous and must reconcile this exact hash instead of resubmitting.
    let signed = match &signed_transaction.transaction {
        TransactionableOrSigned::Signed((signed, _)) => signed,
        TransactionableOrSigned::Transactionable(_) => {
            bail!("near-api transaction was not presigned")
        }
    };
    let tx_hash: CryptoHash = signed
        .get_hash()
        .to_string()
        .parse()
        .map_err(|error| anyhow!("could not convert near-api transaction hash: {error}"))?;
    let signer_id: AccountId = parse_account_id(signed.transaction.signer_id().as_ref(), "signer")?;
    let result = tokio::time::timeout(
        TRANSACTION_BROADCAST_TIMEOUT,
        signed_transaction.send_to(network),
    )
    .await;

    match result {
        Ok(Ok(ApiTransactionResult::Full(final_result))) if final_result.is_success() => {
            Ok(SubmissionOutcome {
                was_ambiguous: false,
            })
        }
        Ok(Ok(ApiTransactionResult::Full(_))) => Err(ConfirmedTransactionFailure {
            tx_hash,
            detail: "final execution status reported failure".to_string(),
        }
        .into()),
        Ok(Ok(ApiTransactionResult::Pending { .. })) => {
            reconcile_transaction(client, tx_hash, signer_id).await?;
            Ok(SubmissionOutcome {
                was_ambiguous: true,
            })
        }
        Err(_) => {
            eprintln!(
                "broadcast timed out for transaction {tx_hash}; querying status before deciding"
            );
            reconcile_transaction(client, tx_hash, signer_id).await?;
            Ok(SubmissionOutcome {
                was_ambiguous: true,
            })
        }
        Ok(Err(error)) if is_pre_broadcast_near_api_error(&error) => {
            Err(anyhow!("near-api transaction preparation failed: {error}"))
        }
        Ok(Err(error)) => {
            eprintln!(
                "broadcast response for transaction {tx_hash} was ambiguous: {error}; querying status"
            );
            reconcile_transaction(client, tx_hash, signer_id).await?;
            Ok(SubmissionOutcome {
                was_ambiguous: true,
            })
        }
    }
}

pub fn create_account_actions(amount: NearToken, public_key: PublicKey) -> Result<Vec<ApiAction>> {
    let api_public_key: ApiPublicKey = public_key
        .to_string()
        .parse()
        .context("could not convert generated public key for near-api")?;
    Ok(vec![
        ApiAction::CreateAccount(CreateAccountAction {}),
        ApiAction::Transfer(near_api::types::transaction::actions::TransferAction {
            deposit: amount,
        }),
        ApiAction::AddKey(Box::new(AddKeyAction {
            public_key: api_public_key,
            access_key: ApiAccessKey {
                nonce: 0.into(),
                permission: ApiAccessKeyPermission::FullAccess,
            },
        })),
    ])
}

pub fn deploy_actions(wasm: Vec<u8>, method: Option<&str>, args: Value) -> Result<Vec<ApiAction>> {
    let mut actions = vec![ApiAction::DeployContract(DeployContractAction {
        code: wasm,
    })];
    if let Some(method) = method {
        actions.push(ApiAction::FunctionCall(Box::new(FunctionCallAction {
            method_name: method.to_string(),
            args: serde_json::to_vec(&args).expect("deployment arguments are serializable"),
            gas: NearGas::from_gas(DEFAULT_GAS),
            deposit: NearToken::from_yoctonear(0),
        })));
    }
    Ok(actions)
}

pub fn call_action(method: &str, args: Value, deposit: u128) -> ApiAction {
    ApiAction::FunctionCall(Box::new(FunctionCallAction {
        method_name: method.to_string(),
        args: serde_json::to_vec(&args).expect("call arguments are serializable"),
        gas: NearGas::from_gas(DEFAULT_GAS),
        deposit: NearToken::from_yoctonear(deposit),
    }))
}

pub fn delete_action(beneficiary_id: AccountId) -> Result<ApiAction> {
    Ok(ApiAction::DeleteAccount(DeleteAccountAction {
        beneficiary_id: api_account_id(&beneficiary_id)?,
    }))
}

pub async fn send_actions(
    client: &JsonRpcClient,
    network: &NetworkConfig,
    signer: &Credentials,
    receiver: AccountId,
    actions: Vec<ApiAction>,
    nonce_tracker: &mut NonceTracker,
) -> Result<()> {
    let nonce = nonce_tracker.next_nonce(client, signer).await?;
    let tx = make_transaction(signer, receiver, nonce, block_hash(client).await?, actions).await?;
    match submit(client, tx, network).await {
        Ok(outcome) => {
            // Only advance after the original transaction has been confirmed. A lagging RPC
            // replica may still report the previous access-key nonce at this point.
            nonce_tracker.confirmed(signer, nonce)?;
            if outcome.was_ambiguous {
                // Reconcile the local value with RPC after an ambiguous broadcast. merge_rpc_nonce
                // retains the locally confirmed value if the replica is behind.
                nonce_tracker.refresh(client, signer).await?;
            }
            Ok(())
        }
        Err(error) if is_confirmed_transaction_failure(&error) => {
            // A finalized execution failure consumed the nonce even though the action failed.
            nonce_tracker.confirmed(signer, nonce)?;
            Err(error)
        }
        Err(error) => {
            // The transaction may have been accepted even when reconciliation failed. Never use
            // the cached nonce for another transaction until it has been recovered from RPC.
            nonce_tracker.invalidate(signer);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Network;
    use near_crypto::KeyType;

    #[test]
    fn nonce_tracker_reuses_and_advances_local_nonce() {
        let secret = SecretKey::from_seed(KeyType::ED25519, "nonce-tracker-test");
        let credentials = Credentials {
            account_id: "alice.testnet".parse().unwrap(),
            secret_key: secret,
        };
        let mut tracker = NonceTracker::default();

        assert_eq!(tracker.merge_rpc_nonce(&credentials, 7).unwrap(), 8);
        assert_eq!(tracker.next_nonces.len(), 1);
        assert_eq!(tracker.next_nonces.values().copied().next(), Some(8));
        assert_eq!(tracker.merge_rpc_nonce(&credentials, 7).unwrap(), 8);
        tracker.confirmed(&credentials, 8).unwrap();
        assert_eq!(tracker.next_nonces.values().copied().next(), Some(9));

        // A stale RPC value must not move the local next nonce backwards.
        assert_eq!(tracker.merge_rpc_nonce(&credentials, 7).unwrap(), 9);
        tracker.invalidate(&credentials);
        assert!(tracker.next_nonces.is_empty());
    }

    #[test]
    fn near_api_network_uses_one_broadcast_attempt() {
        assert_eq!(api_network(Network::Testnet).rpc_endpoints[0].retries, 1);
        assert_eq!(api_network(Network::Mainnet).rpc_endpoints[0].retries, 1);
    }

    #[tokio::test]
    async fn near_api_signed_hash_is_preserved_for_reconciliation() {
        let secret = SecretKey::from_seed(KeyType::ED25519, "near-api-hash-test");
        let signer = Credentials {
            account_id: "alice.testnet".parse().unwrap(),
            secret_key: secret,
        };
        let receiver: AccountId = "bob.testnet".parse().unwrap();
        let transaction = make_transaction(
            &signer,
            receiver,
            1,
            CryptoHash::default(),
            vec![call_action("owner_set", serde_json::json!({}), 1)],
        )
        .await
        .unwrap();
        let TransactionableOrSigned::Signed((signed, _)) = transaction.transaction else {
            panic!("transaction was not presigned");
        };
        let converted: CryptoHash = signed.get_hash().to_string().parse().unwrap();
        assert_eq!(converted.to_string(), signed.get_hash().to_string());
    }

    #[test]
    fn near_api_action_builders_use_typed_values() {
        let secret = SecretKey::from_seed(KeyType::ED25519, "near-api-actions-test");
        let public_key: ApiPublicKey = secret.public_key().to_string().parse().unwrap();
        let actions = vec![
            ApiAction::CreateAccount(CreateAccountAction {}),
            ApiAction::Transfer(near_api::types::transaction::actions::TransferAction {
                deposit: NearToken::from_yoctonear(1),
            }),
            ApiAction::AddKey(Box::new(AddKeyAction {
                public_key,
                access_key: ApiAccessKey {
                    nonce: 0.into(),
                    permission: ApiAccessKeyPermission::FullAccess,
                },
            })),
        ];
        assert_eq!(actions.len(), 3);
    }
}
