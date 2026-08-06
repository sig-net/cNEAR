use near_sdk::serde_json::json;
use near_workspaces::types::NearToken;
use std::path::PathBuf;

const INITIAL_PRICE: u128 = 1612;

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
            },
            "latest_price": INITIAL_PRICE.to_string(),
        }))
        .transact()
        .await?
        .into_result()?;

    Ok(token)
}

fn sha256_hex(bytes: &[u8]) -> String {
    near_sdk::env::sha256(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Register a release with the controller: metadata first, then the blob.
async fn register_release(
    dao: &near_workspaces::Account,
    controller: &near_workspaces::Contract,
    wasm: Vec<u8>,
    version: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let hash = sha256_hex(&wasm);

    dao.call(controller.id(), "add_release_info")
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "hash": &hash,
            "version": version,
            "is_latest": true,
            "downgrade_hash": null,
            "description": "cNEAR",
        }))
        .transact()
        .await?
        .into_result()?;

    dao.call(controller.id(), "add_release_blob")
        .deposit(NearToken::from_yoctonear(1))
        .args(wasm)
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    Ok(hash)
}

/// Deploy the token *through* the controller, mirroring the production flow:
/// the controller creates the account, deploys the blob and calls `new`, and
/// records the deployment in its own registry.
async fn deploy_token_via_controller(
    dao: &near_workspaces::Account,
    controller: &near_workspaces::Contract,
) -> Result<near_workspaces::AccountId, Box<dyn std::error::Error>> {
    let token_wasm = get_wasm_path("fungible_token")?;
    let hash = register_release(dao, controller, token_wasm, "1.0.0").await?;

    // `deploy` creates the account, so it must be a subaccount of the controller.
    let token_id: near_workspaces::AccountId = format!("token.{}", controller.id()).parse()?;

    let result = dao
        .call(controller.id(), "deploy")
        .deposit(NearToken::from_near(10))
        .args_json(json!({
            "new_contract_id": &token_id,
            "init_method": "new",
            "init_args": {
                "owner_id": controller.id(),
                "total_supply": "1000000000000000",
                "metadata": {
                    "spec": "ft-1.0.0",
                    "name": "Test",
                    "symbol": "TEST",
                    "decimals": 24,
                },
                "latest_price": INITIAL_PRICE.to_string(),
            },
            "blob_hash": &hash,
        }))
        .max_gas()
        .transact()
        .await?;

    if !result.is_success() {
        eprintln!("controller deploy failed: {result:#?}");
    }
    result.into_result()?;

    Ok(token_id)
}

/// Gas forwarded to the token for a delegated call.
const DELEGATED_CALL_GAS: &str = "20000000000000";

/// The production topology: a DAO-controlled controller which owns a token it
/// deployed itself. Returns the DAO account, the controller, and the token id.
async fn setup_controller_owned_token(
    worker: &near_workspaces::Worker<impl near_workspaces::DevNetwork>,
) -> Result<
    (
        near_workspaces::Account,
        near_workspaces::Contract,
        near_workspaces::AccountId,
    ),
    Box<dyn std::error::Error>,
> {
    let dao = worker.dev_create_account().await?;
    let controller = deploy_controller(&dao).await?;
    let token_id = deploy_token_via_controller(&dao, &controller).await?;
    Ok((dao, controller, token_id))
}

/// Invoke an owner-only token method the way governance does in production:
/// the DAO asks the controller to delegate the call to the token it owns.
async fn governance_call(
    dao: &near_workspaces::Account,
    controller: &near_workspaces::Contract,
    token_id: &near_workspaces::AccountId,
    method: &str,
    args: near_sdk::serde_json::Value,
    deposit_yocto: u128,
) -> Result<near_workspaces::result::ExecutionFinalResult, Box<dyn std::error::Error>> {
    let args_b64: near_sdk::json_types::Base64VecU8 = near_sdk::serde_json::to_vec(&args)?.into();

    let result = dao
        .call(controller.id(), "delegate_execution")
        // delegate_execution needs a non-zero deposit and forwards `amount` on.
        .deposit(NearToken::from_yoctonear(deposit_yocto.max(1)))
        .args_json(json!({
            "receiver_id": token_id,
            "actions": [{
                "function_name": method,
                "arguments": args_b64,
                "amount": deposit_yocto.to_string(),
                "gas": DELEGATED_CALL_GAS,
            }],
        }))
        .max_gas()
        .transact()
        .await?;

    if !result.is_success() {
        eprintln!("delegated {method} failed: {result:#?}");
    }
    Ok(result)
}

/// Pause via the controller's dedicated pause delegation.
async fn governance_pause(
    dao: &near_workspaces::Account,
    controller: &near_workspaces::Contract,
    token_id: &near_workspaces::AccountId,
) -> Result<(), Box<dyn std::error::Error>> {
    dao.call(controller.id(), "delegate_pause")
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "receiver_id": token_id,
            "pause_method_name": "pause_contract",
        }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;
    Ok(())
}

async fn governance_unpause(
    dao: &near_workspaces::Account,
    controller: &near_workspaces::Contract,
    token_id: &near_workspaces::AccountId,
) -> Result<(), Box<dyn std::error::Error>> {
    governance_call(dao, controller, token_id, "unpause", json!({}), 0)
        .await?
        .into_result()?;
    Ok(())
}

async fn governance_freeze(
    dao: &near_workspaces::Account,
    controller: &near_workspaces::Contract,
    token_id: &near_workspaces::AccountId,
    account_id: &near_workspaces::AccountId,
) -> Result<(), Box<dyn std::error::Error>> {
    governance_call(
        dao,
        controller,
        token_id,
        "freeze_account",
        json!({ "account_id": account_id }),
        0,
    )
    .await?
    .into_result()?;
    Ok(())
}

async fn governance_unfreeze(
    dao: &near_workspaces::Account,
    controller: &near_workspaces::Contract,
    token_id: &near_workspaces::AccountId,
    account_id: &near_workspaces::AccountId,
) -> Result<(), Box<dyn std::error::Error>> {
    governance_call(
        dao,
        controller,
        token_id,
        "unfreeze_account",
        json!({ "account_id": account_id }),
        0,
    )
    .await?
    .into_result()?;
    Ok(())
}

/// Move tokens with the owner's force-transfer power (requires 1 yoctoNEAR).
async fn governance_force_transfer(
    dao: &near_workspaces::Account,
    controller: &near_workspaces::Contract,
    token_id: &near_workspaces::AccountId,
    sender_id: &near_workspaces::AccountId,
    receiver_id: &near_workspaces::AccountId,
    amount: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    governance_call(
        dao,
        controller,
        token_id,
        "force_ft_transfer",
        json!({
            "sender_id": sender_id,
            "receiver_id": receiver_id,
            "amount": amount,
        }),
        1,
    )
    .await?
    .into_result()?;
    Ok(())
}

/// Create a user account and register it for storage with the token.
async fn create_registered_user(
    dao: &near_workspaces::Account,
    token_id: &near_workspaces::AccountId,
    name: &str,
) -> Result<near_workspaces::Account, Box<dyn std::error::Error>> {
    let user = dao
        .create_subaccount(name)
        .initial_balance(NearToken::from_near(5))
        .transact()
        .await?
        .result;

    user.call(token_id, "storage_deposit")
        .args_json(json!({}))
        .deposit(NearToken::from_millinear(10))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    Ok(user)
}

async fn is_paused(
    caller: &near_workspaces::Account,
    token_id: &near_workspaces::AccountId,
) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(caller.call(token_id, "is_paused").view().await?.json()?)
}

async fn is_frozen(
    caller: &near_workspaces::Account,
    token_id: &near_workspaces::AccountId,
    account_id: &near_workspaces::AccountId,
) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(caller
        .call(token_id, "is_frozen")
        .args_json(json!({ "account_id": account_id }))
        .view()
        .await?
        .json()?)
}

/// A plain (non-owner) transfer, i.e. the path pause and freeze actually gate.
async fn user_transfer(
    user: &near_workspaces::Account,
    token_id: &near_workspaces::AccountId,
    receiver_id: &near_workspaces::AccountId,
    amount: &str,
) -> Result<near_workspaces::result::ExecutionFinalResult, Box<dyn std::error::Error>> {
    Ok(user
        .call(token_id, "ft_transfer")
        .args_json(json!({ "receiver_id": receiver_id, "amount": amount }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?)
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

/// The production flow: the controller deploys the token itself. The deployed
/// contract must actually be callable afterwards.
#[tokio::test]
async fn test_controller_deploys_working_token() -> Result<(), Box<dyn std::error::Error>> {
    let worker = near_workspaces::sandbox().await?;
    let dao = worker.dev_create_account().await?;
    let controller = deploy_controller(&dao).await?;

    let token_id = deploy_token_via_controller(&dao, &controller).await?;

    // The controller transaction reporting success is not enough: call the
    // deployed contract and require a sane answer.
    let owner: String = dao
        .call(&token_id, "owner_get")
        .view()
        .await
        .map_err(|e| format!("token is not callable after controller deploy: {e:#?}"))?
        .json()?;
    assert_eq!(
        owner,
        controller.id().to_string(),
        "controller-deployed token must be owned by the controller"
    );

    // The registry must know about the deployment, otherwise upgrades are rejected.
    let deployment: Option<near_sdk::serde_json::Value> = dao
        .call(controller.id(), "get_deployment")
        .args_json(json!({"account_id": &token_id}))
        .view()
        .await?
        .json()?;
    assert!(
        deployment.is_some(),
        "controller must record its own deployment"
    );

    Ok(())
}

/// TOB-CNEAR-7: a controller-mediated upgrade must leave a *working* contract.
///
/// The controller Borsh-serializes (code, state_migration_gas) when it calls
/// `upgrade`, but cNEAR's `upgrade` deploys its entire raw input, so the
/// serialization envelope is installed as contract code. The controller
/// transaction still reports success, which is why this test asserts on the
/// health of the token afterwards rather than on the upgrade receipt.
#[tokio::test]
async fn test_controller_upgrade_leaves_working_token() -> Result<(), Box<dyn std::error::Error>> {
    let worker = near_workspaces::sandbox().await?;
    let dao = worker.dev_create_account().await?;
    let controller = deploy_controller(&dao).await?;

    let token_id = deploy_token_via_controller(&dao, &controller).await?;

    // Sanity check: the token works before we upgrade it.
    let owner: String = dao.call(&token_id, "owner_get").view().await?.json()?;
    assert_eq!(owner, controller.id().to_string());

    // Upgrade to the same code through the controller.
    let token_wasm = get_wasm_path("fungible_token")?;
    let hash = sha256_hex(&token_wasm);
    let upgrade = dao
        .call(controller.id(), "unrestricted_upgrade")
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "contract_id": &token_id,
            "hash": &hash,
            "state_migration_gas": null,
        }))
        .max_gas()
        .transact()
        .await?;
    println!(
        "controller upgrade transaction succeeded: {}",
        upgrade.is_success()
    );

    // The actual requirement: the token still executes after the upgrade.
    let post_upgrade = dao.call(&token_id, "owner_get").view().await;
    assert!(
        post_upgrade.is_ok(),
        "token is not executable after a controller-mediated upgrade \
         (controller reported success = {}): {:#?}",
        upgrade.is_success(),
        post_upgrade.unwrap_err()
    );
    let owner: String = post_upgrade?.json()?;
    assert_eq!(
        owner,
        controller.id().to_string(),
        "ownership must survive the upgrade"
    );

    // And it must still be upgradeable, i.e. governance can recover.
    let second = dao
        .call(controller.id(), "unrestricted_upgrade")
        .deposit(NearToken::from_yoctonear(1))
        .args_json(json!({
            "contract_id": &token_id,
            "hash": &hash,
            "state_migration_gas": null,
        }))
        .max_gas()
        .transact()
        .await?;
    assert!(
        second.is_success(),
        "contract must remain upgradeable after an upgrade: {:#?}",
        second
    );

    Ok(())
}

#[tokio::test]
async fn test_token_control_methods() -> Result<(), Box<dyn std::error::Error>> {
    let worker = near_workspaces::sandbox().await?;
    let (dao, controller, token_id) = setup_controller_owned_token(&worker).await?;
    let user = create_registered_user(&dao, &token_id, "user").await?;

    governance_pause(&dao, &controller, &token_id).await?;
    assert!(is_paused(&dao, &token_id).await?, "should be paused");

    governance_unpause(&dao, &controller, &token_id).await?;
    assert!(!is_paused(&dao, &token_id).await?, "should not be paused");

    governance_freeze(&dao, &controller, &token_id, user.id()).await?;
    assert!(
        is_frozen(&dao, &token_id, user.id()).await?,
        "should be frozen"
    );

    governance_unfreeze(&dao, &controller, &token_id, user.id()).await?;
    assert!(
        !is_frozen(&dao, &token_id, user.id()).await?,
        "should not be frozen"
    );

    Ok(())
}

#[tokio::test]
async fn test_pause_blocks_transfers() -> Result<(), Box<dyn std::error::Error>> {
    let worker = near_workspaces::sandbox().await?;
    let (dao, controller, token_id) = setup_controller_owned_token(&worker).await?;
    let alice = create_registered_user(&dao, &token_id, "alice").await?;
    let bob = create_registered_user(&dao, &token_id, "bob").await?;

    // The controller holds the initial supply, so fund alice from there.
    governance_force_transfer(
        &dao,
        &controller,
        &token_id,
        controller.id(),
        alice.id(),
        "1000000000000",
    )
    .await?;

    governance_pause(&dao, &controller, &token_id).await?;
    let blocked = user_transfer(&alice, &token_id, bob.id(), "1000").await?;
    assert!(
        blocked.is_failure(),
        "transfer should fail while the token is paused"
    );

    governance_unpause(&dao, &controller, &token_id).await?;
    let allowed = user_transfer(&alice, &token_id, bob.id(), "1000").await?;
    assert!(
        allowed.is_success(),
        "transfer should succeed once unpaused: {:#?}",
        allowed
    );

    Ok(())
}

#[tokio::test]
async fn test_freeze_prevents_transfers() -> Result<(), Box<dyn std::error::Error>> {
    let worker = near_workspaces::sandbox().await?;
    let (dao, controller, token_id) = setup_controller_owned_token(&worker).await?;
    let alice = create_registered_user(&dao, &token_id, "alice").await?;
    let bob = create_registered_user(&dao, &token_id, "bob").await?;

    governance_force_transfer(
        &dao,
        &controller,
        &token_id,
        controller.id(),
        alice.id(),
        "1000000000000",
    )
    .await?;

    governance_freeze(&dao, &controller, &token_id, alice.id()).await?;
    let blocked = user_transfer(&alice, &token_id, bob.id(), "1000").await?;
    assert!(
        blocked.is_failure(),
        "a frozen account should not be able to transfer"
    );

    // The owner is exempt from the freeze and can still move the funds.
    governance_force_transfer(&dao, &controller, &token_id, alice.id(), bob.id(), "1000").await?;

    governance_unfreeze(&dao, &controller, &token_id, alice.id()).await?;
    let allowed = user_transfer(&alice, &token_id, bob.id(), "1000").await?;
    assert!(
        allowed.is_success(),
        "transfer should succeed once unfrozen: {:#?}",
        allowed
    );

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
    let token_owner: String = owner.call(token.id(), "owner_get").view().await?.json()?;
    assert_eq!(
        token_owner.as_str(),
        controller_id.as_str(),
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
    let token_owner_check: String = owner.call(token.id(), "owner_get").view().await?.json()?;
    assert_eq!(token_owner_check, controller_id.to_string());

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
    let token_owner: String = owner.call(token.id(), "owner_get").view().await?.json()?;
    assert_eq!(
        token_owner.as_str(),
        controller_id.as_str(),
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

    // A successful controller transaction does not mean the target account
    // contains executable code (TOB-CNEAR-7) - verify by calling the token.
    let post_upgrade = owner.call(&token_id, "owner_get").view().await;
    assert!(
        post_upgrade.is_ok(),
        "token must still execute after the upgrade: {:#?}",
        post_upgrade.unwrap_err()
    );

    println!("✓ Controller successfully upgraded token via release mechanism");

    println!("\n✅ All controller delegation methods work:");
    println!("   1. delegate_pause → token.pause_contract() → paused");
    println!("   2. delegate_execution → token.freeze_account() → frozen");
    println!("   3. add_release_info + add_release_blob + upgrade → token upgraded");

    Ok(())
}
