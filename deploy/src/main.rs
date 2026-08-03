use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, ValueEnum};
use near_api::advanced::{ExecuteSignedTransaction, TransactionableOrSigned};
use near_api::errors::ExecuteTransactionError;
use near_api::types::transaction::actions::{
    AddKeyAction, CreateAccountAction, DeleteAccountAction, DeployContractAction,
    FunctionCallAction,
};
use near_api::types::{
    AccessKey as ApiAccessKey, AccessKeyPermission as ApiAccessKeyPermission,
    AccountId as ApiAccountId, Action as ApiAction, CryptoHash as ApiCryptoHash, NearGas,
    NearToken, PublicKey as ApiPublicKey, SecretKey as ApiSecretKey,
};
use near_api::{NetworkConfig, RPCEndpoint, Signer, Transaction};
use near_api_types::transaction::result::TransactionResult as ApiTransactionResult;
use near_crypto::{KeyType, PublicKey, SecretKey};
use near_jsonrpc_client::{
    errors::{JsonRpcError, JsonRpcServerError},
    methods, JsonRpcClient,
};
use near_jsonrpc_primitives::types::query::QueryResponseKind;
use near_jsonrpc_primitives::types::query::RpcQueryError;
use near_jsonrpc_primitives::types::transactions::{RpcTransactionError, TransactionInfo};
use near_primitives::account::AccessKeyPermission;
use near_primitives::hash::CryptoHash;
use near_primitives::types::{AccountId, BlockId, BlockReference, FunctionArgs, Nonce, StoreKey};
use near_primitives::views::QueryRequest;
use near_primitives::views::{FinalExecutionStatus, TxExecutionStatus};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const DEFAULT_TOKEN_WASM: &str = "target/near/fungible_token.wasm";
const DEFAULT_CONTROLLER_WASM: &str = "target/near/aurora-controller-factory.wasm";
const DEFAULT_TOTAL_SUPPLY: u128 = 1_000_000_000_000_000;
const DEFAULT_INITIAL_PRICE: u128 = 1_000_000_000_000_000_000_000_000;
const DEFAULT_GAS: u64 = 300_000_000_000_000;
const YOCTO_PER_NEAR: u128 = 1_000_000_000_000_000_000_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Network {
    Testnet,
    Mainnet,
}

impl Network {
    fn rpc_url(self) -> &'static str {
        match self {
            Self::Testnet => "https://rpc.testnet.near.org",
            Self::Mainnet => "https://rpc.mainnet.near.org",
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "cnear-deploy",
    about = "Securely deploy cNEAR contracts without near CLI"
)]
struct Cli {
    #[arg(long, value_enum, default_value_t = Network::Testnet)]
    network: Network,
    #[arg(long)]
    signer_id: Option<String>,
    #[arg(long, value_name = "PATH")]
    credentials: Option<PathBuf>,
    #[arg(long, default_value = DEFAULT_CONTROLLER_WASM)]
    controller_wasm: PathBuf,
    #[arg(long, default_value = DEFAULT_TOKEN_WASM)]
    token_wasm: PathBuf,
    #[arg(long)]
    controller_id: Option<String>,
    #[arg(long)]
    token_id: Option<String>,
    #[arg(long, default_value = "cNEAR")]
    token_name: String,
    #[arg(long, default_value = "cNEAR")]
    token_symbol: String,
    #[arg(long, default_value_t = 24)]
    token_decimals: u8,
    #[arg(long, default_value_t = DEFAULT_TOTAL_SUPPLY)]
    total_supply: u128,
    #[arg(long, default_value_t = DEFAULT_INITIAL_PRICE)]
    initial_price: u128,
    #[arg(long, default_value = "10")]
    initial_balance: String,
    #[arg(long)]
    redeploy: bool,
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    test_mode: bool,
    #[arg(long)]
    yes: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct CredentialFile {
    account_id: AccountId,
    public_key: PublicKey,
    private_key: SecretKey,
    #[serde(default)]
    #[serde(rename = "connection_info")]
    _connection_info: Option<Value>,
}

#[derive(Clone, Debug)]
struct Credentials {
    account_id: AccountId,
    secret_key: SecretKey,
}

#[derive(Clone, Debug)]
struct AccountHandle {
    credentials: Credentials,
    created: bool,
    needs_initialization: bool,
    credential_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct WasmArtifact {
    bytes: Vec<u8>,
    sha256: String,
}

#[derive(Clone, Debug)]
struct DeploymentConfig {
    network: Network,
    signer: Credentials,
    controller_id: AccountId,
    token_id: AccountId,
    controller_wasm: WasmArtifact,
    token_wasm: WasmArtifact,
    token_name: String,
    token_symbol: String,
    token_decimals: u8,
    total_supply: u128,
    initial_price: u128,
    initial_balance: u128,
    redeploy: bool,
    dry_run: bool,
    test_mode: bool,
    yes: bool,
}

#[derive(Debug, Deserialize)]
struct AccountView {
    #[serde(rename = "amount")]
    _amount: String,
    #[serde(rename = "locked")]
    _locked: String,
    code_hash: CryptoHash,
}

#[derive(Debug, Deserialize)]
struct AccessKeyView {
    nonce: Nonce,
    permission: AccessKeyPermission,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct NonceKey {
    account_id: AccountId,
    public_key: String,
}

#[derive(Default, Debug)]
struct NonceTracker {
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

    async fn next_nonce(
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

    fn confirmed(&mut self, credentials: &Credentials, used_nonce: Nonce) -> Result<()> {
        self.merge_rpc_nonce(credentials, used_nonce).map(|_| ())
    }

    async fn refresh(
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

    fn invalidate(&mut self, credentials: &Credentials) {
        self.next_nonces.remove(&Self::key(credentials));
    }
}

fn parse_account_id(value: &str, field: &str) -> Result<AccountId> {
    value
        .parse()
        .with_context(|| format!("invalid {field} account ID: {value:?}"))
}

fn parse_near_amount(value: &str) -> Result<u128> {
    let value = value.trim();
    if value.is_empty() || value.starts_with('-') || value.starts_with('+') {
        bail!("initial balance must be a non-negative decimal amount in NEAR");
    }
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty() || !whole.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("initial balance must be a non-negative decimal amount in NEAR");
    }
    if fraction.len() > 24 || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("initial balance must contain at most 24 fractional digits");
    }
    let whole = whole
        .parse::<u128>()
        .context("initial balance is too large")?;
    let fraction_value = if fraction.is_empty() {
        0
    } else {
        let padded = format!("{fraction:0<24}");
        padded
            .parse::<u128>()
            .context("invalid fractional NEAR amount")?
    };
    whole
        .checked_mul(YOCTO_PER_NEAR)
        .and_then(|amount| amount.checked_add(fraction_value))
        .ok_or_else(|| anyhow!("initial balance is too large"))
}

fn read_wasm(path: &Path) -> Result<WasmArtifact> {
    let metadata =
        fs::metadata(path).with_context(|| format!("cannot stat WASM {}", path.display()))?;
    if !metadata.is_file() {
        bail!("WASM path is not a regular file: {}", path.display());
    }
    if metadata.len() < 8 {
        bail!("WASM file is too small: {}", path.display());
    }
    let bytes = fs::read(path).with_context(|| format!("cannot read WASM {}", path.display()))?;
    if bytes.get(0..4) != Some(b"\0asm") {
        bail!("file is not a WebAssembly binary: {}", path.display());
    }
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(WasmArtifact {
        bytes,
        sha256: hex::encode(hasher.finalize()),
    })
}

fn credentials_dir(network: Network) -> Result<PathBuf> {
    if let Ok(root) = std::env::var("NEAR_CREDENTIALS") {
        return Ok(PathBuf::from(root).join(match network {
            Network::Testnet => "testnet",
            Network::Mainnet => "mainnet",
        }));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
    Ok(PathBuf::from(home)
        .join(".near-credentials")
        .join(match network {
            Network::Testnet => "testnet",
            Network::Mainnet => "mainnet",
        }))
}

fn credential_path(
    network: Network,
    signer_id: Option<&AccountId>,
    explicit: Option<&Path>,
) -> Result<PathBuf> {
    Ok(match explicit {
        Some(path) => path.to_path_buf(),
        None => credentials_dir(network)?.join(format!(
            "{}.json",
            signer_id
                .ok_or_else(|| anyhow!("signer ID is required for implicit credential path"))?
        )),
    })
}

#[cfg(unix)]
fn validate_private_file_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let actual = fs::symlink_metadata(path)?.permissions().mode() & 0o777;
    if actual != mode {
        bail!(
            "credential file {} must have permissions {mode:03o}, found {actual:03o}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_file_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

fn load_credentials(path: &Path, expected_account: Option<&AccountId>) -> Result<Credentials> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("credential file not found: {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("credential path must not be a symlink: {}", path.display());
    }
    if !metadata.is_file() {
        bail!("credential path is not a regular file: {}", path.display());
    }
    validate_private_file_mode(path, 0o600)?;
    let raw = fs::read(path)
        .with_context(|| format!("cannot read credential file {}", path.display()))?;
    let file: CredentialFile =
        serde_json::from_slice(&raw).context("invalid NEAR credential JSON")?;
    if file.public_key != file.private_key.public_key() {
        bail!("credential public_key does not match private_key");
    }
    if let Some(expected) = expected_account {
        if &file.account_id != expected {
            bail!("credential account_id does not match requested signer");
        }
    }
    Ok(Credentials {
        account_id: file.account_id,
        secret_key: file.private_key,
    })
}

fn list_credentials(network: Network) -> Result<Vec<PathBuf>> {
    let dir = credentials_dir(network)?;
    let mut paths = fs::read_dir(&dir)
        .with_context(|| format!("credentials directory not found: {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|item| item.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    paths.sort();
    if paths.is_empty() {
        bail!("no JSON credentials found in {}", dir.display());
    }
    Ok(paths)
}

fn select_signer(cli: &Cli) -> Result<Credentials> {
    let expected = cli
        .signer_id
        .as_deref()
        .map(|value| parse_account_id(value, "signer"))
        .transpose()?;
    let path = credential_path(cli.network, expected.as_ref(), cli.credentials.as_deref())?;
    if cli.credentials.is_some() || expected.is_some() {
        return load_credentials(&path, expected.as_ref());
    }
    let paths = list_credentials(cli.network)?;
    for (index, path) in paths.iter().enumerate() {
        println!(
            "  {}) {}",
            index + 1,
            path.file_stem().unwrap_or_default().to_string_lossy()
        );
    }
    print!("Select signer [1]: ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let selected = input.trim().parse::<usize>().unwrap_or(1);
    let selected = paths
        .get(selected.saturating_sub(1))
        .ok_or_else(|| anyhow!("invalid signer selection"))?;
    load_credentials(selected, None)
}

fn build_config(cli: Cli) -> Result<DeploymentConfig> {
    if cli.token_decimals == 0 {
        bail!("token decimals must be between 1 and 255");
    }
    if cli.token_name.trim().is_empty() || cli.token_symbol.trim().is_empty() {
        bail!("token name and symbol must not be empty");
    }
    let signer = select_signer(&cli)?;
    let controller_id = match cli.controller_id.as_deref() {
        Some(value) => parse_account_id(value, "controller")?,
        None => parse_account_id(&format!("controller.{}", signer.account_id), "controller")?,
    };
    let token_id = match cli.token_id.as_deref() {
        Some(value) => parse_account_id(value, "token")?,
        None => parse_account_id(&format!("token.{}", signer.account_id), "token")?,
    };
    if cli.total_supply == 0 {
        bail!("total supply must be greater than zero");
    }
    if cli.initial_price == 0 {
        bail!("initial price must be greater than zero");
    }
    let initial_balance = parse_near_amount(&cli.initial_balance)?;
    Ok(DeploymentConfig {
        network: cli.network,
        signer,
        controller_id,
        token_id,
        controller_wasm: read_wasm(&cli.controller_wasm)?,
        token_wasm: read_wasm(&cli.token_wasm)?,
        token_name: cli.token_name,
        token_symbol: cli.token_symbol,
        token_decimals: cli.token_decimals,
        total_supply: cli.total_supply,
        initial_price: cli.initial_price,
        initial_balance,
        redeploy: cli.redeploy,
        dry_run: cli.dry_run,
        test_mode: cli.test_mode,
        yes: cli.yes,
    })
}

type QueryRpcError = JsonRpcError<RpcQueryError>;

#[derive(Clone, Copy, Debug)]
struct RpcSnapshot {
    block_hash: CryptoHash,
}

impl RpcSnapshot {
    fn block_reference(self) -> BlockReference {
        BlockReference::BlockId(BlockId::Hash(self.block_hash))
    }
}

async fn query(
    client: &JsonRpcClient,
    block_reference: BlockReference,
    request: QueryRequest,
) -> std::result::Result<QueryResponseKind, QueryRpcError> {
    client
        .call(methods::query::RpcQueryRequest {
            block_reference,
            request,
        })
        .await
        .map(|response| response.kind)
}

async fn finalized_snapshot(client: &JsonRpcClient) -> Result<RpcSnapshot> {
    let response = client
        .call(methods::block::RpcBlockRequest {
            block_reference: BlockReference::Finality(near_primitives::types::Finality::Final),
        })
        .await
        .context("could not query finalized preflight block")?;
    Ok(RpcSnapshot {
        block_hash: response.header.hash,
    })
}

async fn account(
    client: &JsonRpcClient,
    block_reference: BlockReference,
    account_id: &AccountId,
) -> Result<Option<AccountView>> {
    match query(
        client,
        block_reference,
        QueryRequest::ViewAccount {
            account_id: account_id.clone(),
        },
    )
    .await
    {
        Ok(QueryResponseKind::ViewAccount(view)) => {
            Ok(Some(serde_json::from_value(serde_json::to_value(view)?)?))
        }
        Ok(_) => bail!("RPC returned an unexpected account response"),
        Err(JsonRpcError::ServerError(JsonRpcServerError::HandlerError(
            RpcQueryError::UnknownAccount { .. },
        ))) => Ok(None),
        Err(error) => Err(anyhow!("NEAR RPC account query failed: {error}")),
    }
}

async fn has_contract_state(
    client: &JsonRpcClient,
    block_reference: BlockReference,
    account_id: &AccountId,
) -> Result<bool> {
    match query(
        client,
        block_reference,
        QueryRequest::ViewState {
            account_id: account_id.clone(),
            prefix: StoreKey::from(Vec::<u8>::new()),
            include_proof: false,
        },
    )
    .await
    {
        Ok(QueryResponseKind::ViewState(state)) => Ok(!state.values.is_empty()),
        Ok(_) => bail!("RPC returned an unexpected contract-state response"),
        Err(JsonRpcError::ServerError(JsonRpcServerError::HandlerError(
            RpcQueryError::NoContractCode { .. },
        ))) => Ok(false),
        Err(error) => Err(anyhow!("NEAR RPC contract-state query failed: {error}")),
    }
}

async fn access_key(
    client: &JsonRpcClient,
    block_reference: BlockReference,
    credentials: &Credentials,
) -> Result<AccessKeyView> {
    match query(
        client,
        block_reference,
        QueryRequest::ViewAccessKey {
            account_id: credentials.account_id.clone(),
            public_key: credentials.secret_key.public_key(),
        },
    )
    .await
    .context("NEAR RPC access-key query failed")?
    {
        QueryResponseKind::AccessKey(view) => {
            Ok(serde_json::from_value(serde_json::to_value(view)?)?)
        }
        _ => bail!("RPC returned an unexpected access-key response"),
    }
}

fn full_access(permission: &AccessKeyPermission) -> bool {
    matches!(permission, AccessKeyPermission::FullAccess)
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

fn api_network(network: Network) -> NetworkConfig {
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

async fn block_hash(client: &JsonRpcClient) -> Result<CryptoHash> {
    let response = client
        .call(methods::block::RpcBlockRequest {
            block_reference: BlockReference::Finality(near_primitives::types::Finality::Final),
        })
        .await
        .context("could not query final block")?;
    Ok(response.header.hash)
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
struct UnresolvedTransaction {
    tx_hash: CryptoHash,
}

#[derive(Debug)]
struct ConfirmedTransactionFailure {
    tx_hash: CryptoHash,
    detail: String,
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

fn is_unresolved_transaction(error: &anyhow::Error) -> bool {
    error.downcast_ref::<UnresolvedTransaction>().is_some()
}

fn is_confirmed_transaction_failure(error: &anyhow::Error) -> bool {
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
    let signer_id: AccountId = signed
        .transaction
        .signer_id()
        .to_string()
        .parse()
        .map_err(|error| anyhow!("could not convert near-api signer account ID: {error}"))?;
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

fn create_account_actions(amount: u128, public_key: PublicKey) -> Result<Vec<ApiAction>> {
    let api_public_key: ApiPublicKey = public_key
        .to_string()
        .parse()
        .context("could not convert generated public key for near-api")?;
    Ok(vec![
        ApiAction::CreateAccount(CreateAccountAction {}),
        ApiAction::Transfer(near_api::types::transaction::actions::TransferAction {
            deposit: NearToken::from_yoctonear(amount),
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

fn deploy_actions(wasm: Vec<u8>, method: Option<&str>, args: Value) -> Result<Vec<ApiAction>> {
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

fn call_action(method: &str, args: Value, deposit: u128) -> ApiAction {
    ApiAction::FunctionCall(Box::new(FunctionCallAction {
        method_name: method.to_string(),
        args: serde_json::to_vec(&args).expect("call arguments are serializable"),
        gas: NearGas::from_gas(DEFAULT_GAS),
        deposit: NearToken::from_yoctonear(deposit),
    }))
}

fn delete_action(beneficiary_id: AccountId) -> Result<ApiAction> {
    Ok(ApiAction::DeleteAccount(DeleteAccountAction {
        beneficiary_id: api_account_id(&beneficiary_id)?,
    }))
}

async fn send_actions(
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

fn persist_generated_credentials(
    path: &Path,
    account_id: AccountId,
    secret_key: SecretKey,
) -> Result<Credentials> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
    }
    let public_key = secret_key.public_key();
    let contents = serde_json::to_vec_pretty(&json!({
        "account_id": account_id,
        "public_key": public_key,
        "private_key": secret_key,
    }))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(path)
        .with_context(|| format!("cannot create credential file {}", path.display()))?;
    file.write_all(&contents)?;
    file.sync_all()?;
    Ok(Credentials {
        account_id,
        secret_key,
    })
}

async fn ensure_account(
    client: &JsonRpcClient,
    network: &NetworkConfig,
    config: &DeploymentConfig,
    snapshot: RpcSnapshot,
    account_id: &AccountId,
    nonce_tracker: &mut NonceTracker,
) -> Result<AccountHandle> {
    let path = credentials_dir(config.network)?.join(format!("{account_id}.json"));
    if let Some(state) = account(client, snapshot.block_reference(), account_id).await? {
        let key = access_key(client, snapshot.block_reference(), &config.signer)
            .await
            .with_context(|| "signer access key is unavailable at the preflight snapshot")?;
        if !full_access(&key.permission) {
            bail!("signer access key is not full access at the preflight snapshot");
        }
        let credentials = load_credentials(&path, Some(account_id)).with_context(|| {
            format!(
                "existing account {account_id} requires a matching 0600 full-access credential file"
            )
        })?;
        let account_key = access_key(client, snapshot.block_reference(), &credentials)
            .await
            .with_context(|| {
                format!("credential for existing account {account_id} is not an active key")
            })?;
        if !full_access(&account_key.permission) {
            bail!("credential for existing account {account_id} is not full access");
        }
        return Ok(AccountHandle {
            credentials,
            created: false,
            needs_initialization: state.code_hash == CryptoHash::default(),
            credential_path: None,
        });
    }

    let secret_key = SecretKey::from_random(KeyType::ED25519);
    let credentials = persist_generated_credentials(&path, account_id.clone(), secret_key)?;
    if let Err(error) = send_actions(
        client,
        network,
        &config.signer,
        account_id.clone(),
        create_account_actions(config.initial_balance, credentials.secret_key.public_key())?,
        nonce_tracker,
    )
    .await
    {
        let _ = fs::remove_file(&path);
        return Err(error).with_context(|| format!("could not create account {account_id}"));
    }
    Ok(AccountHandle {
        credentials,
        created: true,
        needs_initialization: true,
        credential_path: Some(path),
    })
}

async fn cleanup_accounts(
    client: &JsonRpcClient,
    network: &NetworkConfig,
    config: &DeploymentConfig,
    token: Option<&AccountHandle>,
    controller: Option<&AccountHandle>,
    nonce_tracker: &mut NonceTracker,
) -> Result<()> {
    let mut first_error = None;
    for handle in [token, controller].into_iter().flatten() {
        if !handle.created {
            continue;
        }
        if let Err(error) = send_actions(
            client,
            network,
            &handle.credentials,
            handle.credentials.account_id.clone(),
            vec![delete_action(config.signer.account_id.clone())?],
            nonce_tracker,
        )
        .await
        {
            let unresolved = is_unresolved_transaction(&error);
            first_error.get_or_insert(error);
            // Do not submit another transaction from any account while the previous
            // cleanup transaction has an unknown outcome.
            if unresolved {
                break;
            }
        } else if let Some(path) = &handle.credential_path {
            if let Err(error) = fs::remove_file(path) {
                first_error.get_or_insert(anyhow!(
                    "deleted account {} but could not remove generated credential file {}: {error}",
                    handle.credentials.account_id,
                    path.display()
                ));
            }
        }
    }
    first_error.map_or(Ok(()), |error| {
        Err(error).context("one or more cleanup transactions failed")
    })
}

async fn deploy(config: &DeploymentConfig) -> Result<()> {
    println!("network: {:?}", config.network);
    println!("signer: {}", config.signer.account_id);
    println!("controller WASM hash: {}", config.controller_wasm.sha256);
    println!("token WASM hash: {}", config.token_wasm.sha256);
    if config.network == Network::Mainnet && !config.yes && !config.dry_run {
        print!("Type 'mainnet' to confirm deployment: ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if input.trim() != "mainnet" {
            bail!("mainnet deployment not confirmed");
        }
    }
    let client = JsonRpcClient::connect(config.network.rpc_url());
    let network = api_network(config.network);
    let snapshot = finalized_snapshot(&client).await?;
    access_key(&client, snapshot.block_reference(), &config.signer).await?;
    let controller = account(&client, snapshot.block_reference(), &config.controller_id).await?;
    let token = account(&client, snapshot.block_reference(), &config.token_id).await?;
    for (id, state) in [
        (&config.controller_id, controller),
        (&config.token_id, token),
    ] {
        if let Some(state) = state {
            let has_state = if state.code_hash == CryptoHash::default() {
                has_contract_state(&client, snapshot.block_reference(), id).await?
            } else {
                false
            };
            if has_state {
                bail!("account {id} already contains contract state; refusing automatic redeploy");
            }
            if !config.redeploy && state.code_hash != CryptoHash::default() {
                bail!("account {id} already has deployed code; use --redeploy explicitly");
            }
        }
    }
    if config.dry_run {
        println!("dry run: no transactions submitted");
        return Ok(());
    }
    let mut controller_account = None;
    let mut token_account = None;
    let mut nonce_tracker = NonceTracker::default();
    let deployment = async {
        let controller = ensure_account(
            &client,
            &network,
            config,
            snapshot,
            &config.controller_id,
            &mut nonce_tracker,
        )
        .await?;
        controller_account = Some(controller);
        let token = ensure_account(
            &client,
            &network,
            config,
            snapshot,
            &config.token_id,
            &mut nonce_tracker,
        )
        .await?;
        token_account = Some(token);
        let controller_credentials = &controller_account
            .as_ref()
            .expect("controller account set")
            .credentials;
        let token_credentials = &token_account
            .as_ref()
            .expect("token account set")
            .credentials;
        send_actions(
            &client,
            &network,
            controller_credentials,
            config.controller_id.clone(),
            deploy_actions(
                config.controller_wasm.bytes.clone(),
                controller_account
                    .as_ref()
                    .expect("controller account set")
                    .needs_initialization
                    .then_some("new"),
                json!({ "dao": config.signer.account_id }),
            )?,
            &mut nonce_tracker,
        )
        .await?;
        send_actions(
            &client,
            &network,
            token_credentials,
            config.token_id.clone(),
            deploy_actions(
                config.token_wasm.bytes.clone(),
                token_account
                    .as_ref()
                    .expect("token account set")
                    .needs_initialization
                    .then_some("new"),
                json!({
                    "owner_id": config.signer.account_id,
                    "total_supply": config.total_supply.to_string(),
                    "metadata": { "spec": "ft-1.0.0", "name": config.token_name, "symbol": config.token_symbol, "decimals": config.token_decimals },
                    "latest_price": config.initial_price.to_string(),
                }),
            )?,
            &mut nonce_tracker,
        )
        .await?;
        if token_account
            .as_ref()
            .expect("token account set")
            .needs_initialization
        {
            send_actions(
                &client,
                &network,
                &config.signer,
                config.token_id.clone(),
                vec![call_action(
                    "owner_set",
                    json!({ "new_owner": config.controller_id }),
                    1,
                )],
                &mut nonce_tracker,
            )
            .await?;
        }
        verify_ownership(&client, &config.token_id, &config.controller_id).await
    };
    if let Err(error) = deployment.await {
        if config.test_mode && !is_unresolved_transaction(&error) {
            if let Err(cleanup_error) = cleanup_accounts(
                &client,
                &network,
                config,
                token_account.as_ref(),
                controller_account.as_ref(),
                &mut nonce_tracker,
            )
            .await
            {
                return Err(error.context(format!("cleanup also failed: {cleanup_error}")));
            }
        } else if config.test_mode {
            eprintln!(
                "skipping cleanup because a transaction outcome is unresolved; inspect its hash before retrying"
            );
        }
        return Err(error);
    }
    if config.test_mode {
        cleanup_accounts(
            &client,
            &network,
            config,
            token_account.as_ref(),
            controller_account.as_ref(),
            &mut nonce_tracker,
        )
        .await?;
    }
    Ok(())
}

async fn verify_ownership(
    client: &JsonRpcClient,
    token_id: &AccountId,
    expected: &AccountId,
) -> Result<()> {
    let kind = query(
        client,
        BlockReference::Finality(near_primitives::types::Finality::Final),
        QueryRequest::CallFunction {
            account_id: token_id.clone(),
            method_name: "owner_get".to_string(),
            args: FunctionArgs::from(b"{}".to_vec()),
        },
    )
    .await
    .context("NEAR RPC ownership query failed")?;
    let QueryResponseKind::CallResult(result) = kind else {
        bail!("RPC returned an unexpected owner response");
    };
    let actual: AccountId =
        serde_json::from_slice(&result.result).context("owner_get returned invalid JSON")?;
    if &actual != expected {
        bail!("ownership verification failed: expected {expected}, got {actual}");
    }
    println!("ownership verified: {actual}");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = build_config(cli)?;
    deploy(&config).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::{Builder, NamedTempFile};

    #[test]
    fn account_ids_are_strictly_validated() {
        assert!(parse_account_id("alice.testnet", "signer").is_ok());
        assert!(parse_account_id("ALICE.testnet", "signer").is_err());
        assert!(parse_account_id("not an account", "signer").is_err());
    }

    #[test]
    fn numeric_values_reject_invalid_input() {
        assert_eq!("42".parse::<u128>().unwrap(), 42);
        assert!("0".parse::<u128>().is_ok());
        assert!("-1".parse::<u128>().is_err());
        assert_eq!(parse_near_amount("1").unwrap(), YOCTO_PER_NEAR);
        assert_eq!(parse_near_amount("0.000000000000000000000001").unwrap(), 1);
        assert!(parse_near_amount("-1").is_err());
    }

    #[test]
    fn credentials_require_matching_keys_and_secure_permissions() {
        let secret_key = SecretKey::from_seed(KeyType::ED25519, "credential-test");
        let account_id: AccountId = "alice.testnet".parse().unwrap();
        let mut file = Builder::new().suffix(".json").tempfile().unwrap();
        let payload = json!({
            "account_id": account_id,
            "public_key": secret_key.public_key(),
            "private_key": secret_key,
        });
        file.write_all(serde_json::to_string(&payload).unwrap().as_bytes())
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        let credentials = load_credentials(file.path(), Some(&account_id)).unwrap();
        assert_eq!(credentials.account_id, account_id);

        let other_key = SecretKey::from_seed(KeyType::ED25519, "credential-mismatch");
        let mismatched_payload = json!({
            "account_id": account_id,
            "public_key": other_key.public_key(),
            "private_key": secret_key,
        });
        fs::write(
            file.path(),
            serde_json::to_vec(&mismatched_payload).unwrap(),
        )
        .unwrap();
        assert!(load_credentials(file.path(), Some(&account_id)).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.as_file()
                .set_permissions(fs::Permissions::from_mode(0o644))
                .unwrap();
            assert!(load_credentials(file.path(), Some(&account_id)).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn credentials_reject_symlinks() {
        use std::os::unix::fs::symlink;
        let secret_key = SecretKey::from_seed(KeyType::ED25519, "symlink-test");
        let account_id: AccountId = "alice.testnet".parse().unwrap();
        let target = NamedTempFile::new().unwrap();
        target.as_file().set_len(0).unwrap();
        let payload = json!({
            "account_id": account_id,
            "public_key": secret_key.public_key(),
            "private_key": secret_key,
        });
        fs::write(target.path(), serde_json::to_vec(&payload).unwrap()).unwrap();
        use std::os::unix::fs::PermissionsExt;
        target
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let link = directory.path().join("credential.json");
        symlink(target.path(), &link).unwrap();
        assert!(load_credentials(&link, Some(&account_id)).is_err());
    }

    #[test]
    fn wasm_validation_checks_magic_and_hash() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"\0asm\x01\0\0\0").unwrap();
        let artifact = read_wasm(file.path()).unwrap();
        assert_eq!(artifact.bytes.len(), 8);
        assert_eq!(artifact.sha256.len(), 64);
    }

    #[test]
    fn ownership_json_is_typed() {
        let value: AccountId = serde_json::from_slice(br#""alice.testnet""#).unwrap();
        assert_eq!(value.as_str(), "alice.testnet");
        assert!(serde_json::from_slice::<AccountId>(br#""bad id""#).is_err());
    }

    #[test]
    fn snapshot_uses_a_specific_block_hash() {
        let hash = CryptoHash::hash_bytes(b"preflight");
        let snapshot = RpcSnapshot { block_hash: hash };
        assert_eq!(
            snapshot.block_reference(),
            BlockReference::BlockId(BlockId::Hash(hash))
        );
    }

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
            vec![call_action("owner_set", json!({}), 1)],
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
