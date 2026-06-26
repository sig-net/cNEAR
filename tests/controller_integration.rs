use near_sdk::serde_json::json;
use near_workspaces::types::NearToken;
use std::path::PathBuf;

/// Resolve wasm path - check CARGO_TARGET_DIR, fallback to ./target
fn get_wasm_path(contract_name: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .unwrap_or_else(|_| "./target".to_string());
    
    let wasm_path = PathBuf::from(&target_dir)
        .join("near")
        .join(format!("{}.wasm", contract_name));
    
    std::fs::read(&wasm_path)
        .map_err(|e| format!("Failed to read {}: {}", wasm_path.display(), e).into())
}

async fn deploy_token(
    owner: &near_workspaces::Account,
    token_owner_id: &near_sdk::AccountId,
) -> Result<near_workspaces::Contract, Box<dyn std::error::Error>> {
    let token_wasm = get_wasm_path("fungible_token")?;
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
            "owner_id": token_owner_id,
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

async fn deploy_controller(
    owner: &near_workspaces::Account,
) -> Result<near_workspaces::Contract, Box<dyn std::error::Error>> {
    let controller_wasm = get_wasm_path("aurora-controller-factory")?;
    let controller_exec = owner
        .create_subaccount("controller")
        .initial_balance(NearToken::from_near(10))
        .transact()
        .await?;

    let controller_account = controller_exec.result;
    let controller_deploy = controller_account.deploy(&controller_wasm).await?;
    let controller = controller_deploy.result;

    Ok(controller)
}

#[tokio::test]
async fn test_token_control_methods() -> Result<(), Box<dyn std::error::Error>> {
    let worker = near_workspaces::sandbox().await?;
    let owner = worker.dev_create_account().await?;

    let token = deploy_token(&owner, owner.id()).await?;
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

    let token = deploy_token(&owner, owner.id()).await?;
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

    let token = deploy_token(&owner, owner.id()).await?;
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

#[tokio::test]
async fn test_controller_with_token() -> Result<(), Box<dyn std::error::Error>> {
    let worker = near_workspaces::sandbox().await?;
    let owner = worker.dev_create_account().await?;

    // Deploy controller first
    let _controller = deploy_controller(&owner).await?;
    let controller_id = _controller.id().clone();

    // Deploy token with controller as owner
    let token = deploy_token(&owner, &controller_id).await?;

    // CRITICAL: Verify token is deployed with CORRECT owner
    let token_owner: Option<String> = owner.call(token.id(), "get_owner").view().await?.json()?;
    assert_eq!(
        token_owner.as_ref().map(|s| s.as_str()),
        Some(controller_id.as_str()),
        "Token owner MUST be controller (not deployer)"
    );

    // Verify owner checks work: deployer (owner account) should NOT be able to pause
    let pause_result = owner
        .call(token.id(), "pause")
        .transact()
        .await;

    // This should fail because owner != controller
    assert!(
        pause_result.is_err() || pause_result.unwrap().is_failure(),
        "Non-owner should not be able to pause token"
    );

    // Verify owner field is correctly set by trying to get it again
    let token_owner_check: Option<String> = owner.call(token.id(), "get_owner").view().await?.json()?;
    assert_eq!(token_owner_check, Some(controller_id.to_string()));

    println!("✓ Token deployed with controller as owner - ownership model correct");

    Ok(())
}
