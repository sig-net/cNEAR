use crate::credentials::Credentials;
use crate::rpc::{access_key, block_hash, full_access};
use anyhow::{anyhow, bail, Result};
use near_crypto::{InMemorySigner, PublicKey};
use near_gas::NearGas;
use near_jsonrpc_client::JsonRpcClient;
use near_jsonrpc_client::{
    errors::{JsonRpcError, JsonRpcServerError},
    methods,
};
use near_jsonrpc_primitives::types::transactions::{RpcTransactionError, TransactionInfo};
use near_primitives::account::{AccessKey, AccessKeyPermission};
use near_primitives::action::{
    Action, AddKeyAction, CreateAccountAction, DeleteAccountAction, DeployContractAction,
    FunctionCallAction, TransferAction,
};
use near_primitives::hash::CryptoHash;
use near_primitives::transaction::{
    SignedTransaction, Transaction as NearTransaction, TransactionV0,
};
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

/// Build and sign a transaction over the native near-primitives type stack.
/// Signing is a plain operation (no network), so this is a synchronous `fn`.
fn make_transaction(
    signer: &Credentials,
    receiver_id: AccountId,
    nonce: Nonce,
    block_hash: CryptoHash,
    actions: Vec<Action>,
) -> SignedTransaction {
    let crypto_signer =
        InMemorySigner::from_secret_key(signer.account_id.clone(), signer.secret_key.clone());
    let transaction = NearTransaction::V0(TransactionV0 {
        signer_id: signer.account_id.clone(),
        public_key: crypto_signer.public_key(),
        nonce,
        receiver_id,
        block_hash,
        actions,
    });
    // Sign the canonical borsh hash; `SignedTransaction::new` recomputes the
    // same hash, so `get_hash()` below always matches what was signed.
    let (hash, _) = transaction.get_hash_and_size();
    let signature = crypto_signer.sign(&hash.0);
    SignedTransaction::new(signature, transaction)
}

const DEFAULT_GAS: NearGas = NearGas::from_tgas(300);

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

/// Only `InvalidTransaction` proves the node rejected the transaction before it
/// could enter the pool, so the nonce was never consumed. Every other broadcast
/// error (`TimeoutError`, `UnknownTransaction`, `RequestRouted`,
/// `DoesNotTrackShard`, `InternalError`, transport errors) means the outcome is
/// unknown and must be reconciled by hash instead.
fn is_pre_broadcast_rejection(error: &JsonRpcError<RpcTransactionError>) -> bool {
    matches!(
        error,
        JsonRpcError::ServerError(JsonRpcServerError::HandlerError(
            RpcTransactionError::InvalidTransaction { .. }
        ))
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

async fn submit(client: &JsonRpcClient, signed: SignedTransaction) -> Result<SubmissionOutcome> {
    // The transaction is presigned once and broadcast exactly once — deliberately no retry.
    // Capture its hash before the single broadcast call; a transport timeout is ambiguous
    // and must reconcile this exact hash instead of resubmitting.
    let tx_hash = signed.get_hash();
    let signer_id = signed.transaction.signer_id().clone();
    let result = tokio::time::timeout(
        TRANSACTION_BROADCAST_TIMEOUT,
        client.call(methods::broadcast_tx_commit::RpcBroadcastTxCommitRequest {
            signed_transaction: signed,
        }),
    )
    .await;

    match result {
        Ok(Ok(outcome)) => match outcome.status {
            FinalExecutionStatus::SuccessValue(_) => Ok(SubmissionOutcome {
                was_ambiguous: false,
            }),
            FinalExecutionStatus::Failure(error) => Err(ConfirmedTransactionFailure {
                tx_hash,
                detail: format!("{error:?}"),
            }
            .into()),
            FinalExecutionStatus::NotStarted | FinalExecutionStatus::Started => {
                // The node returned before a final outcome was available.
                reconcile_transaction(client, tx_hash, signer_id).await?;
                Ok(SubmissionOutcome {
                    was_ambiguous: true,
                })
            }
        },
        Err(_) => {
            eprintln!(
                "broadcast timed out for transaction {tx_hash}; querying status before deciding"
            );
            reconcile_transaction(client, tx_hash, signer_id).await?;
            Ok(SubmissionOutcome {
                was_ambiguous: true,
            })
        }
        Ok(Err(error)) if is_pre_broadcast_rejection(&error) => {
            Err(anyhow!("transaction rejected before broadcast: {error}"))
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

/// A builder for sending a single batch of actions as one signed transaction.
///
/// Configure the receiver at `new`, chain action methods such as
/// `.create_account(...)`, `.deploy(...)`, `.call(...)`, or
/// `.delete_account(...)`, then `.send().await`. Transactions are broadcast
/// exactly once (no automatic retry), reconciled by hash if the outcome is
/// ambiguous, and the local nonce tracker advances only after confirmation.
pub struct Transaction<'a> {
    client: &'a JsonRpcClient,
    signer: &'a Credentials,
    receiver: AccountId,
    actions: Vec<Action>,
    nonce_tracker: &'a mut NonceTracker,
}

impl<'a> Transaction<'a> {
    pub fn new(
        client: &'a JsonRpcClient,
        signer: &'a Credentials,
        receiver: AccountId,
        nonce_tracker: &'a mut NonceTracker,
    ) -> Self {
        Self {
            client,
            signer,
            receiver,
            actions: Vec::new(),
            nonce_tracker,
        }
    }

    /// Append actions that create the receiver as a subaccount of the signer,
    /// funding it with `amount` and attaching a full-access key for `public_key`.
    pub fn create_account(mut self, amount: NearToken, public_key: PublicKey) -> Self {
        self.actions
            .push(Action::CreateAccount(CreateAccountAction {}));
        self.actions.push(Action::Transfer(TransferAction {
            deposit: amount.as_yoctonear(),
        }));
        self.actions.push(Action::AddKey(Box::new(AddKeyAction {
            public_key,
            access_key: AccessKey {
                nonce: 0,
                permission: AccessKeyPermission::FullAccess,
            },
        })));
        self
    }

    /// Append actions that deploy `wasm` to the receiver, optionally followed by
    /// an initialization `FunctionCall` with `init`'s method name and args.
    pub fn deploy(mut self, wasm: Vec<u8>, init: Option<(&str, Value)>) -> Self {
        self.actions
            .push(Action::DeployContract(DeployContractAction { code: wasm }));
        if let Some((method, args)) = init {
            self.actions
                .push(Action::FunctionCall(Box::new(FunctionCallAction {
                    method_name: method.to_string(),
                    args: serde_json::to_vec(&args).expect("deployment arguments are serializable"),
                    gas: DEFAULT_GAS.as_gas(),
                    deposit: 0,
                })));
        }
        self
    }

    /// Append a `FunctionCall` to `method` with `args` and `deposit` yoctoNEAR.
    pub fn call(mut self, method: &str, args: Value, deposit: u128) -> Self {
        self.actions
            .push(Action::FunctionCall(Box::new(FunctionCallAction {
                method_name: method.to_string(),
                args: serde_json::to_vec(&args).expect("call arguments are serializable"),
                gas: DEFAULT_GAS.as_gas(),
                deposit,
            })));
        self
    }

    /// Append a `DeleteAccount` action deleting the receiver and sending its
    /// remaining balance to `beneficiary_id`.
    pub fn delete_account(mut self, beneficiary_id: AccountId) -> Self {
        self.actions
            .push(Action::DeleteAccount(DeleteAccountAction {
                beneficiary_id,
            }));
        self
    }

    /// Sign and broadcast exactly once, then reconcile by hash if the outcome is
    /// ambiguous. Advances the local nonce tracker only after confirmation.
    pub async fn send(self) -> Result<()> {
        let nonce = self
            .nonce_tracker
            .next_nonce(self.client, self.signer)
            .await?;
        let tx = make_transaction(
            self.signer,
            self.receiver,
            nonce,
            block_hash(self.client).await?,
            self.actions,
        );
        let result = submit(self.client, tx).await;
        match settle_nonce(self.nonce_tracker, self.signer, nonce, result).await? {
            PostSubmit::NeedsRefresh => {
                // Reconcile the local value with RPC after an ambiguous broadcast.
                self.nonce_tracker.refresh(self.client, self.signer).await?;
            }
            PostSubmit::Confirmed => {}
        }
        Ok(())
    }
}

/// What `Transaction::send` must do after the nonce tracker has been settled.
#[derive(Debug, Eq, PartialEq)]
enum PostSubmit {
    Confirmed,
    NeedsRefresh,
}

/// Settle the local nonce tracker after a submit outcome. The submit `result`
/// is injected as a seam so tests can drive every path without touching the
/// network. The ambiguous path confirms the nonce and asks the caller to
/// reconcile the tracker with RPC before submitting anything else.
async fn settle_nonce(
    nonce_tracker: &mut NonceTracker,
    signer: &Credentials,
    nonce: Nonce,
    result: Result<SubmissionOutcome>,
) -> Result<PostSubmit> {
    match result {
        Ok(outcome) => {
            // Only advance after the original transaction has been confirmed. A lagging RPC
            // replica may still report the previous access-key nonce at this point.
            nonce_tracker.confirmed(signer, nonce)?;
            if outcome.was_ambiguous {
                Ok(PostSubmit::NeedsRefresh)
            } else {
                Ok(PostSubmit::Confirmed)
            }
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
    use near_crypto::{KeyType, SecretKey};

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
    fn signed_hash_is_preserved_for_reconciliation() {
        let secret = SecretKey::from_seed(KeyType::ED25519, "hash-preservation-test");
        let signer = Credentials {
            account_id: "alice.testnet".parse().unwrap(),
            secret_key: secret,
        };
        let receiver: AccountId = "bob.testnet".parse().unwrap();
        let client = JsonRpcClient::connect("http://localhost:3030");
        let mut tracker = NonceTracker::default();
        let actions = Transaction::new(&client, &signer, receiver.clone(), &mut tracker)
            .call("owner_set", serde_json::json!({}), 1)
            .actions;
        let signed = make_transaction(&signer, receiver, 1, CryptoHash::default(), actions);
        // The signature is over the canonical borsh hash, and `get_hash()`
        // returns exactly that hash — what reconciliation queries by.
        let expected_hash = signed.transaction.get_hash_and_size().0;
        assert_eq!(signed.get_hash(), expected_hash);
        assert!(signed
            .signature
            .verify(&expected_hash.0, &signer.secret_key.public_key()));
    }

    #[test]
    fn transaction_builder_action_methods_append_native_actions() {
        let secret = SecretKey::from_seed(KeyType::ED25519, "native-actions-test");
        let signer = Credentials {
            account_id: "alice.testnet".parse().unwrap(),
            secret_key: secret,
        };
        let receiver: AccountId = "bob.testnet".parse().unwrap();
        let client = JsonRpcClient::connect("http://localhost:3030");
        let mut tracker = NonceTracker::default();

        let actions = Transaction::new(&client, &signer, receiver.clone(), &mut tracker)
            .create_account(NearToken::from_yoctonear(1), signer.secret_key.public_key())
            .actions;
        assert_eq!(actions.len(), 3);
        assert!(matches!(actions[0], Action::CreateAccount(_)));
        assert!(matches!(actions[2], Action::AddKey(_)));

        let actions = Transaction::new(&client, &signer, receiver.clone(), &mut tracker)
            .deploy(vec![1, 2, 3], Some(("new", serde_json::json!({}))))
            .actions;
        assert_eq!(actions.len(), 2);
        assert!(matches!(actions[0], Action::DeployContract(_)));

        let actions = Transaction::new(&client, &signer, receiver.clone(), &mut tracker)
            .call("owner_set", serde_json::json!({}), 1)
            .actions;
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::FunctionCall(_)));

        let actions = Transaction::new(&client, &signer, receiver, &mut tracker)
            .delete_account("beneficiary.testnet".parse().unwrap())
            .actions;
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::DeleteAccount(_)));
    }

    fn key(credentials: &Credentials) -> NonceKey {
        NonceTracker::key(credentials)
    }

    #[tokio::test]
    async fn transaction_builder_success_advances_nonce_without_refresh() {
        let secret = SecretKey::from_seed(KeyType::ED25519, "tx-success-test");
        let credentials = Credentials {
            account_id: "alice.testnet".parse().unwrap(),
            secret_key: secret,
        };
        let mut tracker = NonceTracker::default();
        tracker.merge_rpc_nonce(&credentials, 7).unwrap();
        assert_eq!(tracker.next_nonces[&key(&credentials)], 8);

        let post = settle_nonce(
            &mut tracker,
            &credentials,
            8,
            Ok(SubmissionOutcome {
                was_ambiguous: false,
            }),
        )
        .await
        .unwrap();

        assert_eq!(post, PostSubmit::Confirmed);
        assert_eq!(tracker.next_nonces[&key(&credentials)], 9);
    }

    #[tokio::test]
    async fn transaction_builder_ambiguous_outcome_confirms_then_needs_refresh() {
        let secret = SecretKey::from_seed(KeyType::ED25519, "tx-ambiguous-test");
        let credentials = Credentials {
            account_id: "alice.testnet".parse().unwrap(),
            secret_key: secret,
        };
        let mut tracker = NonceTracker::default();
        tracker.merge_rpc_nonce(&credentials, 7).unwrap();

        let post = settle_nonce(
            &mut tracker,
            &credentials,
            8,
            Ok(SubmissionOutcome {
                was_ambiguous: true,
            }),
        )
        .await
        .unwrap();

        assert_eq!(post, PostSubmit::NeedsRefresh);
        // The nonce was consumed before the caller's RPC refresh step, so even a later
        // refresh failure cannot reuse this nonce for another transaction.
        assert_eq!(tracker.next_nonces[&key(&credentials)], 9);
        // A lagging replica that still reports the previous nonce must not move the
        // locally confirmed value backwards.
        tracker.merge_rpc_nonce(&credentials, 7).unwrap();
        assert_eq!(tracker.next_nonces[&key(&credentials)], 9);
    }

    #[tokio::test]
    async fn transaction_builder_confirmed_failure_consumes_nonce() {
        let secret = SecretKey::from_seed(KeyType::ED25519, "tx-failure-test");
        let credentials = Credentials {
            account_id: "alice.testnet".parse().unwrap(),
            secret_key: secret,
        };
        let mut tracker = NonceTracker::default();
        tracker.merge_rpc_nonce(&credentials, 7).unwrap();

        let result = settle_nonce(
            &mut tracker,
            &credentials,
            8,
            Err(ConfirmedTransactionFailure {
                tx_hash: CryptoHash::default(),
                detail: "execution failed".to_string(),
            }
            .into()),
        )
        .await;

        let error = result.unwrap_err();
        assert!(is_confirmed_transaction_failure(&error));
        // A finalized execution failure still consumed the nonce.
        assert_eq!(tracker.next_nonces[&key(&credentials)], 9);
    }

    #[tokio::test]
    async fn transaction_builder_unknown_error_invalidates_nonce() {
        let secret = SecretKey::from_seed(KeyType::ED25519, "tx-invalidate-test");
        let credentials = Credentials {
            account_id: "alice.testnet".parse().unwrap(),
            secret_key: secret,
        };
        let mut tracker = NonceTracker::default();
        tracker.merge_rpc_nonce(&credentials, 7).unwrap();

        let result = settle_nonce(
            &mut tracker,
            &credentials,
            8,
            Err(UnresolvedTransaction {
                tx_hash: CryptoHash::default(),
            }
            .into()),
        )
        .await;

        assert!(result.is_err());
        // The cached nonce must never be reused for another transaction.
        assert!(tracker.next_nonces.is_empty());
    }
}
