use near_sdk::serde_json::json;
use near_workspaces::types::NearToken;

async fn deploy_token(
    owner: &near_workspaces::Account,
) -> Result<near_workspaces::Contract, Box<dyn std::error::Error>> {
    let token_wasm = std::fs::read("./target/near/fungible_token.wasm")?;
    let token_exec = owner
        .create_subaccount("token")
        .initial_balance(NearToken::from_near(10))
        .transact()
        .await?;

    let token_account = token_exec.result;
    let token_deploy = token_account.deploy(&token_wasm).await?;
    let token = token_deploy.result;

    owner
        .call(token.id(), "new")
        .args_json(json!({
            "owner_id": owner.id(),
            "total_supply": "1000000000000000",
            "metadata": {
                "spec": "ft-1.0.0",
                "name": "Test",
                "symbol": "TEST",
                "decimals": 24,
            }
        }))
        .transact()
        .await?
        .into_result()?;

    Ok(token)
}

#[tokio::test]
async fn test_token_control_methods() -> Result<(), Box<dyn std::error::Error>> {
    let worker = near_workspaces::sandbox().await?;
    let owner = worker.dev_create_account().await?;

    let token = deploy_token(&owner).await?;
    let token_id = token.id().clone();

    // Test pause
    owner
        .call(&token_id, "pause")
        .transact()
        .await?
        .into_result()?;

    let is_paused: bool = owner.call(&token_id, "is_paused").view().await?.json()?;
    assert!(is_paused, "Should be paused");

    // Test unpause
    owner
        .call(&token_id, "unpause")
        .transact()
        .await?
        .into_result()?;

    let is_paused: bool = owner.call(&token_id, "is_paused").view().await?.json()?;
    assert!(!is_paused, "Should not be paused");

    // Test freeze
    let user = owner
        .create_subaccount("user")
        .initial_balance(NearToken::from_near(1))
        .transact()
        .await?
        .result;
    let user_id = user.id().clone();

    owner
        .call(&token_id, "freeze_account")
        .args_json(json!({"account_id": &user_id}))
        .transact()
        .await?
        .into_result()?;

    let is_frozen: bool = owner
        .call(&token_id, "is_frozen")
        .args_json(json!({"account_id": &user_id}))
        .view()
        .await?
        .json()?;

    assert!(is_frozen, "Should be frozen");

    // Test unfreeze
    owner
        .call(&token_id, "unfreeze_account")
        .args_json(json!({"account_id": &user_id}))
        .transact()
        .await?
        .into_result()?;

    let is_frozen: bool = owner
        .call(&token_id, "is_frozen")
        .args_json(json!({"account_id": &user_id}))
        .view()
        .await?
        .json()?;

    assert!(!is_frozen, "Should not be frozen");

    println!("✓ All token control tests passed");

    Ok(())
}

#[tokio::test]
async fn test_pause_blocks_transfers() -> Result<(), Box<dyn std::error::Error>> {
    let worker = near_workspaces::sandbox().await?;
    let owner = worker.dev_create_account().await?;

    let token = deploy_token(&owner).await?;
    let token_id = token.id().clone();

    let user = owner
        .create_subaccount("user")
        .initial_balance(NearToken::from_near(5))
        .transact()
        .await?
        .result;
    let user_id = user.id().clone();

    owner
        .call(&token_id, "storage_deposit")
        .args_json(json!({"account_id": &user_id}))
        .deposit(NearToken::from_near(1))
        .transact()
        .await?
        .into_result()?;

    // Pause
    owner
        .call(&token_id, "pause")
        .transact()
        .await?
        .into_result()?;

    // Transfer should fail
    let xfer = owner
        .call(&token_id, "ft_transfer")
        .args_json(json!({"receiver_id": &user_id, "amount": "1000000000000"}))
        .deposit(NearToken::from_yoctonear(1))
        .transact()
        .await?;

    assert!(xfer.is_failure(), "Transfer should fail when paused");

    // Unpause
    owner
        .call(&token_id, "unpause")
        .transact()
        .await?
        .into_result()?;

    // Transfer should succeed
    let xfer = owner
        .call(&token_id, "ft_transfer")
        .args_json(json!({"receiver_id": &user_id, "amount": "1000000000000"}))
        .deposit(NearToken::from_yoctonear(1))
        .transact()
        .await?;

    assert!(xfer.is_success(), "Transfer should succeed after unpause");

    println!("✓ Pause blocking transfers works");

    Ok(())
}

#[tokio::test]
async fn test_freeze_prevents_transfers() -> Result<(), Box<dyn std::error::Error>> {
    let worker = near_workspaces::sandbox().await?;
    let owner = worker.dev_create_account().await?;

    let token = deploy_token(&owner).await?;
    let token_id = token.id().clone();

    let user = owner
        .create_subaccount("user")
        .initial_balance(NearToken::from_near(5))
        .transact()
        .await?
        .result;
    let user_id = user.id().clone();

    owner
        .call(&token_id, "storage_deposit")
        .args_json(json!({"account_id": &user_id}))
        .deposit(NearToken::from_near(1))
        .transact()
        .await?
        .into_result()?;

    // Give tokens
    owner
        .call(&token_id, "force_ft_transfer")
        .args_json(json!({
            "sender_id": owner.id(),
            "receiver_id": &user_id,
            "amount": "1000000000000",
        }))
        .deposit(NearToken::from_yoctonear(1))
        .transact()
        .await?
        .into_result()?;

    // Freeze
    owner
        .call(&token_id, "freeze_account")
        .args_json(json!({"account_id": &user_id}))
        .transact()
        .await?
        .into_result()?;

    // Transfer should fail
    let xfer = user
        .call(&token_id, "ft_transfer")
        .args_json(json!({"receiver_id": owner.id(), "amount": "100000000000"}))
        .deposit(NearToken::from_yoctonear(1))
        .transact()
        .await?;

    assert!(xfer.is_failure(), "Frozen account transfer should fail");

    // Unfreeze
    owner
        .call(&token_id, "unfreeze_account")
        .args_json(json!({"account_id": &user_id}))
        .transact()
        .await?
        .into_result()?;

    // Transfer should succeed
    let xfer = user
        .call(&token_id, "ft_transfer")
        .args_json(json!({"receiver_id": owner.id(), "amount": "100000000000"}))
        .deposit(NearToken::from_yoctonear(1))
        .transact()
        .await?;

    assert!(
        xfer.is_success(),
        "Unfrozen account transfer should succeed"
    );

    println!("✓ Freeze preventing transfers works");

    Ok(())
}
