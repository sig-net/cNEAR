use crate::cli::Network;
use crate::config::DeploymentConfig;
use crate::credentials::{
    credentials_dir, load_credentials, persist_generated_credentials, Credentials,
};
use crate::rpc::{
    access_key, account, finalized_snapshot, full_access, has_contract_state, verify_ownership,
    JsonRpcClient, RpcSnapshot,
};
use crate::transaction::{is_unresolved_transaction, NonceTracker, Transaction};
use anyhow::{anyhow, bail, Context, Result};
use near_crypto::{KeyType, SecretKey};
use near_primitives::hash::CryptoHash;
use near_primitives::types::AccountId;
use serde_json::json;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

#[derive(Clone, Debug)]
struct AccountHandle {
    credentials: Credentials,
    created: bool,
    needs_initialization: bool,
    credential_path: Option<PathBuf>,
}

async fn ensure_account(
    client: &JsonRpcClient,
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
    if let Err(error) = Transaction::new(client, &config.signer, account_id.clone(), nonce_tracker)
        .create_account(config.initial_balance, credentials.secret_key.public_key())
        .send()
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

/// Delete the given accounts in order (callers pass token before controller),
/// sending remaining balances to the signer. Accounts not created by this run
/// are skipped; credential files for deleted accounts are removed.
async fn cleanup_accounts(
    client: &JsonRpcClient,
    config: &DeploymentConfig,
    handles: &[&AccountHandle],
    nonce_tracker: &mut NonceTracker,
) -> Result<()> {
    let mut first_error = None;
    for handle in handles {
        if !handle.created {
            continue;
        }
        if let Err(error) = Transaction::new(
            client,
            &handle.credentials,
            handle.credentials.account_id.clone(),
            nonce_tracker,
        )
        .delete_account(config.signer.account_id.clone())
        .send()
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

/// Accounts prepared for deployment. Each handle records whether this run
/// created the account, which drives test-mode cleanup.
struct PreparedAccounts {
    controller: AccountHandle,
    token: AccountHandle,
}

/// Ensure both target accounts exist. If the token cannot be prepared after the
/// controller already was, the controller is cleaned up (in test mode) before
/// the error is returned.
async fn prepare_accounts(
    client: &JsonRpcClient,
    config: &DeploymentConfig,
    snapshot: RpcSnapshot,
    nonce_tracker: &mut NonceTracker,
) -> Result<PreparedAccounts> {
    let controller = ensure_account(
        client,
        config,
        snapshot,
        &config.controller_id,
        nonce_tracker,
    )
    .await?;
    match ensure_account(client, config, snapshot, &config.token_id, nonce_tracker).await {
        Ok(token) => Ok(PreparedAccounts { controller, token }),
        Err(error) => {
            if config.test_mode && !is_unresolved_transaction(&error) {
                if let Err(cleanup_error) =
                    cleanup_accounts(client, config, &[&controller], nonce_tracker).await
                {
                    return Err(error.context(format!("cleanup also failed: {cleanup_error}")));
                }
            } else if config.test_mode {
                eprintln!(
                    "skipping cleanup because a transaction outcome is unresolved; inspect its hash before retrying"
                );
            }
            Err(error)
        }
    }
}

/// Delete the accounts created by this run in reverse dependency order (token
/// first), returning the first error encountered.
async fn cleanup_created_accounts(
    client: &JsonRpcClient,
    config: &DeploymentConfig,
    accounts: &PreparedAccounts,
    nonce_tracker: &mut NonceTracker,
) -> Result<()> {
    cleanup_accounts(
        client,
        config,
        &[&accounts.token, &accounts.controller],
        nonce_tracker,
    )
    .await
}

/// Deploy and initialize the contracts on the prepared accounts, transfer token
/// ownership to the controller, and verify it.
async fn deploy_contracts(
    client: &JsonRpcClient,
    config: &DeploymentConfig,
    accounts: &PreparedAccounts,
    nonce_tracker: &mut NonceTracker,
) -> Result<()> {
    let controller_credentials = &accounts.controller.credentials;
    let token_credentials = &accounts.token.credentials;
    Transaction::new(
        client,
        controller_credentials,
        config.controller_id.clone(),
        nonce_tracker,
    )
    .deploy(
        config.controller_wasm.bytes.clone(),
        accounts
            .controller
            .needs_initialization
            .then_some(("new", json!({ "dao": config.signer.account_id }))),
    )
    .send()
    .await?;
    Transaction::new(client, token_credentials, config.token_id.clone(), nonce_tracker)
        .deploy(
            config.token_wasm.bytes.clone(),
            accounts.token.needs_initialization.then_some((
                "new",
                json!({
                    "owner_id": config.signer.account_id,
                    "total_supply": config.total_supply.as_yoctonear().to_string(),
                    "metadata": { "spec": "ft-1.0.0", "name": config.token_name, "symbol": config.token_symbol, "decimals": config.token_decimals },
                    "latest_price": config.initial_price.as_yoctonear().to_string(),
                }),
            )),
        )
        .send()
        .await?;
    if accounts.token.needs_initialization {
        Transaction::new(
            client,
            &config.signer,
            config.token_id.clone(),
            nonce_tracker,
        )
        .call("owner_set", json!({ "new_owner": config.controller_id }), 1)
        .send()
        .await?;
    }
    verify_ownership(client, &config.token_id, &config.controller_id).await
}

pub async fn deploy(config: &DeploymentConfig) -> Result<()> {
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
    let mut nonce_tracker = NonceTracker::default();
    let accounts = prepare_accounts(&client, config, snapshot, &mut nonce_tracker).await?;

    if let Err(error) = deploy_contracts(&client, config, &accounts, &mut nonce_tracker).await {
        if config.test_mode && !is_unresolved_transaction(&error) {
            if let Err(cleanup_error) =
                cleanup_created_accounts(&client, config, &accounts, &mut nonce_tracker).await
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
        cleanup_created_accounts(&client, config, &accounts, &mut nonce_tracker).await?;
    }
    Ok(())
}
