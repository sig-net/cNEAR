use crate::credentials::Credentials;
use anyhow::{anyhow, bail, Context, Result};
use near_jsonrpc_client::errors::{
    JsonRpcError, JsonRpcServerError, JsonRpcTransportSendError, RpcTransportError,
};
use near_jsonrpc_client::methods;
pub use near_jsonrpc_client::JsonRpcClient;
use near_jsonrpc_primitives::types::query::{QueryResponseKind, RpcQueryError};
use near_primitives::account::AccessKeyPermission;
use near_primitives::hash::CryptoHash;
use near_primitives::types::{AccountId, BlockId, BlockReference, FunctionArgs, StoreKey};
use near_primitives::views::QueryRequest;
use serde::Deserialize;

pub type QueryRpcError = JsonRpcError<RpcQueryError>;

/// Upper bound on any single JSON-RPC request so a stalled endpoint surfaces as
/// a clear error instead of an indefinite silent hang (near-jsonrpc-client
/// does not set its own HTTP timeout).
const RPC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

async fn rpc_call<T, E>(
    future: impl std::future::Future<Output = std::result::Result<T, JsonRpcError<E>>>,
) -> std::result::Result<T, JsonRpcError<E>> {
    tokio::time::timeout(RPC_TIMEOUT, future)
        .await
        .map_err(|_| {
            // TransportError is not parameterized by the handler error type, so a
            // single variant works for every RPC method.
            JsonRpcError::TransportError(RpcTransportError::SendError(
                JsonRpcTransportSendError::PayloadSerializeError(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "NEAR JSON-RPC request timed out after {} seconds",
                        RPC_TIMEOUT.as_secs()
                    ),
                )),
            ))
        })?
}

#[derive(Debug, Deserialize)]
pub struct AccountView {
    #[serde(rename = "amount")]
    pub _amount: String,
    #[serde(rename = "locked")]
    pub _locked: String,
    pub code_hash: CryptoHash,
}

#[derive(Debug, Deserialize)]
pub struct AccessKeyView {
    pub nonce: near_primitives::types::Nonce,
    pub permission: AccessKeyPermission,
}

#[derive(Clone, Copy, Debug)]
pub struct RpcSnapshot {
    pub block_hash: CryptoHash,
}

impl RpcSnapshot {
    pub fn block_reference(self) -> BlockReference {
        BlockReference::BlockId(BlockId::Hash(self.block_hash))
    }
}

pub async fn query(
    client: &JsonRpcClient,
    block_reference: BlockReference,
    request: QueryRequest,
) -> std::result::Result<QueryResponseKind, QueryRpcError> {
    rpc_call(client.call(methods::query::RpcQueryRequest {
        block_reference,
        request,
    }))
    .await
    .map(|response| response.kind)
}

pub async fn finalized_snapshot(client: &JsonRpcClient) -> Result<RpcSnapshot> {
    let response = rpc_call(client.call(methods::block::RpcBlockRequest {
        block_reference: BlockReference::Finality(near_primitives::types::Finality::Final),
    }))
    .await
    .context("could not query finalized preflight block")?;
    Ok(RpcSnapshot {
        block_hash: response.header.hash,
    })
}

pub async fn block_hash(client: &JsonRpcClient) -> Result<CryptoHash> {
    let response = rpc_call(client.call(methods::block::RpcBlockRequest {
        block_reference: BlockReference::Finality(near_primitives::types::Finality::Final),
    }))
    .await
    .context("could not query final block")?;
    Ok(response.header.hash)
}

pub async fn account(
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

pub async fn has_contract_state(
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

pub async fn access_key(
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

pub fn full_access(permission: &AccessKeyPermission) -> bool {
    matches!(permission, AccessKeyPermission::FullAccess)
}

/// Verify that a contract view returns the expected account ID.
pub async fn verify_ownership(
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn full_access_detects_full_access_keys() {
        assert!(full_access(&AccessKeyPermission::FullAccess));
        assert!(!full_access(&AccessKeyPermission::FunctionCall(
            near_primitives::account::FunctionCallPermission {
                allowance: None,
                receiver_id: "receiver.testnet".to_string(),
                method_names: vec![],
            }
        )));
    }
}
