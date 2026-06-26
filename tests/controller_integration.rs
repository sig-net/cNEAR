use near_sdk::serde_json::json;
use near_workspaces::types::NearToken;
use std::path::PathBuf;

/// Resolve wasm path - check CARGO_TARGET_DIR, fallback to ./target
fn get_wasm_path(contract_name: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let target_dir = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "./target".to_string());

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

    // Initialize controller with owner as DAO (owner will have full permissions)
    let result = controller_account
        .call(controller_account.id(), "new")
        .args_json(json!({
            "dao": owner.id()  // Set owner as DAO so it has permissions
        }))
        .transact()
        .await?;

    if !result.is_success() {
        eprintln!("Controller init failed: {result:#?}");
    }
    result.into_result()?;

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
    let pause_result = owner.call(token.id(), "pause").transact().await;

    // This should fail because owner != controller
    assert!(
        pause_result.is_err() || pause_result.unwrap().is_failure(),
        "Non-owner should not be able to pause token"
    );

    // Verify owner field is correctly set by trying to get it again
    let token_owner_check: Option<String> =
        owner.call(token.id(), "get_owner").view().await?.json()?;
    assert_eq!(token_owner_check, Some(controller_id.to_string()));

    println!("✓ Token deployed with controller as owner - ownership model correct");

    Ok(())
}

#[tokio::test]
async fn test_controller_delegates_token_control() -> Result<(), Box<dyn std::error::Error>> {
    let worker = near_workspaces::sandbox().await?;
    let owner = worker.dev_create_account().await?;

    // Deploy controller first
    let controller = deploy_controller(&owner).await?;
    let controller_id = controller.id().clone();

    // Deploy token with controller as owner
    let token = deploy_token(&owner, &controller_id).await?;
    let token_id = token.id().clone();

    // Test 1: Verify controller IS the owner
    let token_owner: Option<String> = owner.call(token.id(), "get_owner").view().await?.json()?;
    assert_eq!(
        token_owner.as_ref().map(|s| s.as_str()),
        Some(controller_id.as_str()),
        "Token owner must be controller"
    );
    println!("✓ Token owner is controller");

    // Test 2: delegate_pause via controller
    println!("\nTest delegate_pause via controller...");
    let pause_result = owner
        .call(controller.id(), "delegate_pause")
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": token_id.to_string(),
            "pause_method_name": "pause_contract"
        }))
        .max_gas()
        .transact()
        .await?;

    assert!(pause_result.is_success(), "delegate_pause should succeed");

    let is_paused: bool = owner.call(token.id(), "is_paused").view().await?.json()?;
    assert!(is_paused, "Token should be paused after delegate_pause");
    println!("✓ delegate_pause works - token paused via controller");

    // Test 3: delegate_execution to freeze account via controller
    println!("\nTest delegate_execution (freeze_account) via controller...");
    let user = owner
        .create_subaccount("user")
        .initial_balance(NearToken::from_near(1))
        .transact()
        .await?
        .result;
    let user_id = user.id().clone();

    // Build args for freeze_account - expects JSON: {"account_id": "..."}
    let freeze_args_json = json!({"account_id": user_id.to_string()});
    let freeze_args_bytes = near_sdk::serde_json::to_vec(&freeze_args_json)?;
    let freeze_args_b64: near_sdk::json_types::Base64VecU8 = freeze_args_bytes.into();

    let exec_result = owner
        .call(controller.id(), "delegate_execution")
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": token_id.to_string(),
            "actions": vec![json!({
                "function_name": "freeze_account",
                "arguments": freeze_args_b64,
                "amount": "0",
                "gas": "5000000000000"
            })]
        }))
        .max_gas()
        .transact()
        .await?;

    if !exec_result.is_success() {
        eprintln!("delegate_execution failed: {exec_result:#?}");
    }
    assert!(
        exec_result.is_success(),
        "delegate_execution should succeed"
    );

    let is_frozen: bool = owner
        .call(token.id(), "is_frozen")
        .args_json(json!({"account_id": user_id.to_string()}))
        .view()
        .await?
        .json()?;
    assert!(
        is_frozen,
        "Account should be frozen after delegate_execution"
    );
    println!("✓ delegate_execution works - account frozen via controller");

    // Test 4: Controller upgrades token via proper release flow
    println!("\nTest upgrade via controller release mechanism...");

    // Load token wasm
    let token_wasm = get_wasm_path("fungible_token")?;

    // Calculate sha256 hash for release info
    let hash_bytes = near_sdk::env::sha256(&token_wasm);
    let hash = hash_bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>();

    // Step 1: Add release info for current token version
    let result = owner
        .call(controller.id(), "add_release_info")
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "hash": &hash,
            "version": "1.0.0",
            "is_latest": true,
            "downgrade_hash": null,
            "description": "Token contract"
        }))
        .transact()
        .await?;
    assert!(result.is_success(), "add_release_info should succeed");

    // Step 2: Add release blob (wasm bytes)
    let result = owner
        .call(controller.id(), "add_release_blob")
        .deposit(NearToken::from_yoctonear(1))
        .args(token_wasm)
        .max_gas()
        .transact()
        .await?;
    assert!(result.is_success(), "add_release_blob should succeed");

    // Step 3: Register existing token deployment w/ controller
    let deployment_info = json!({
        "hash": &hash,
        "version": "1.0.0",
        "deployment_time": 0u64,
        "upgrade_times": {},
        "init_args": ""
    });

    let result = owner
        .call(controller.id(), "add_deployment_info")
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "contract_id": token_id.to_string(),
            "deployment_info": deployment_info
        }))
        .transact()
        .await?;
    assert!(result.is_success(), "add_deployment_info should succeed");

    // Step 4: Upgrade token (use unrestricted_upgrade since same hash)
    let result = owner
        .call(controller.id(), "unrestricted_upgrade")
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "contract_id": token_id.to_string(),
            "hash": &hash
        }))
        .max_gas()
        .transact()
        .await?;

    if !result.is_success() {
        eprintln!("unrestricted_upgrade failed: {result:#?}");
    }
    assert!(result.is_success(), "unrestricted_upgrade should succeed");

    println!("✓ Controller successfully upgraded token via release mechanism");

    println!("\n✅ All controller delegation methods work:");
    println!("   1. delegate_pause → token.pause_contract() → paused");
    println!("   2. delegate_execution → token.freeze_account() → frozen");
    println!("   3. add_release_info + add_release_blob + upgrade → token upgraded");

    Ok(())
}
