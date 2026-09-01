//! Tests for the key removal that finalizes a deployment.
//!
//! `finalize` is the most destructive operation in this repository: it deletes
//! the access keys that are the only way to act on an account outside the
//! contract's own authorization. TOB-CNEAR-2 is about it not existing; this is
//! about it doing what it claims.
//!
//! The command itself asks the operator to confirm before it touches anything,
//! so the tests drive `remove_access_keys`, which is the part that does the
//! work.

use cnear_deploy::{remove_access_keys, store_near_cli_credentials};
use near_workspaces::types::NearToken;

fn credentials_dir(name: &str) -> anyhow::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join(format!("cnear-finalize-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[tokio::test]
async fn removes_every_key_from_the_token_account() -> anyhow::Result<()> {
    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    let token = root
        .create_subaccount("token")
        .initial_balance(NearToken::from_near(10))
        .transact()
        .await?
        .into_result()?;

    let creds = credentials_dir("removes")?;
    store_near_cli_credentials(&token, &creds)?;

    // A freshly created account has the full access key it was created with.
    let before = worker.view_access_keys(token.id()).await?;
    assert!(
        !before.is_empty(),
        "precondition: the account should start with a key"
    );

    remove_access_keys(&worker, &creds, token.id(), true).await?;

    // NEP-141 asks that a deployed token hold no access keys, so that the
    // contract's own authorization is the only way to change it.
    let after = worker.view_access_keys(token.id()).await?;
    assert!(
        after.is_empty(),
        "the token account must be keyless afterwards, found {} key(s)",
        after.len()
    );
    Ok(())
}

#[tokio::test]
async fn is_a_no_op_on_an_account_that_is_already_keyless() -> anyhow::Result<()> {
    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    let token = root
        .create_subaccount("token")
        .initial_balance(NearToken::from_near(10))
        .transact()
        .await?
        .into_result()?;

    let creds = credentials_dir("noop")?;
    store_near_cli_credentials(&token, &creds)?;
    remove_access_keys(&worker, &creds, token.id(), true).await?;

    // Running finalization twice must not fail: an operator who is unsure
    // whether it completed should be able to run it again.
    remove_access_keys(&worker, &creds, token.id(), true).await?;

    assert!(worker.view_access_keys(token.id()).await?.is_empty());
    Ok(())
}
