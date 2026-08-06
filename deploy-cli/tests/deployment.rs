//! End-to-end test of the deployment against a sandbox.
//!
//! The deployment's own checks abort on failure, so a green run here means the
//! deployment registered the token with the controller (TOB-CNEAR-5) and
//! verified ownership actually moved (TOB-CNEAR-6). The assertions afterwards
//! state those outcomes explicitly, so a regression names what broke.

use cnear_deploy::{deploy_with_signer, store_near_cli_credentials, Config, NetworkName};
use near_workspaces::types::NearToken;
use serde_json::json;

const DECIMALS: u8 = 24;
const SUPPLY_TOKENS: u128 = 100_000_000_000_000;

#[tokio::test]
async fn deployment_registers_and_transfers_ownership() -> anyhow::Result<()> {
    // Cargo runs tests from the package directory, but the wasm the deployment
    // reads is built into the workspace target directory.
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("deploy-cli lives in the workspace")
        .to_path_buf();
    if std::env::var_os("CARGO_TARGET_DIR").is_none() {
        std::env::set_var("CARGO_TARGET_DIR", workspace.join("target"));
    }

    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    let signer = root
        .create_subaccount("signer")
        .initial_balance(NearToken::from_near(200))
        .transact()
        .await?
        .into_result()?;

    // The controller pays storage for its own code plus the release blob it is
    // asked to hold (~4 NEAR each at current sizes); the token pays for its code.
    let controller = signer
        .create_subaccount("controller")
        .initial_balance(NearToken::from_near(50))
        .transact()
        .await?
        .into_result()?;
    let token = signer
        .create_subaccount("token")
        .initial_balance(NearToken::from_near(50))
        .transact()
        .await?
        .into_result()?;

    // The deployment loads the accounts it operates on from the credentials
    // directory, so give it one containing exactly these accounts.
    let creds = tempdir()?;
    let network_dir = creds.join("testnet");
    std::fs::create_dir_all(&network_dir)?;
    for account in [&signer, &controller, &token] {
        store_near_cli_credentials(account, &network_dir)?;
    }
    std::env::set_var("NEAR_CREDENTIALS", &creds);

    let config = Config {
        network: NetworkName::Testnet,
        signer_id: signer.id().clone(),
        controller_id: controller.id().clone(),
        token_id: token.id().clone(),
        token_name: "cNEAR".to_owned(),
        token_symbol: "cNEAR".to_owned(),
        token_decimals: DECIMALS,
        total_supply_tokens: SUPPLY_TOKENS,
        total_supply_yocto: SUPPLY_TOKENS * 10u128.pow(u32::from(DECIMALS)),
        initial_price: NearToken::from_near(1).as_yoctonear(),
        initial_balance: NearToken::from_near(10),
        dry_run: false,
        test_mode: false,
    };
    let expected_supply = config.total_supply_yocto.to_string();

    deploy_with_signer(worker.clone(), signer.clone(), config).await?;

    // Ownership moved, and moved to the controller rather than staying with the
    // deployer's key.
    let owner: String = signer.call(token.id(), "owner_get").view().await?.json()?;
    assert_eq!(
        owner,
        controller.id().to_string(),
        "the controller must own the token after deployment"
    );

    // The controller knows about the deployment, without which every
    // controller-mediated upgrade is rejected.
    let deployment: Option<serde_json::Value> = signer
        .call(controller.id(), "get_deployment")
        .args_json(json!({ "account_id": token.id() }))
        .view()
        .await?
        .json()?;
    assert!(
        deployment.is_some(),
        "the deployment must be registered with the controller"
    );

    // The supply is the whole-token amount scaled by the decimals, not the
    // whole-token number taken as atomic units.
    let supply: String = signer
        .call(token.id(), "ft_total_supply")
        .view()
        .await?
        .json()?;
    assert_eq!(
        supply, expected_supply,
        "total supply must be scaled by the decimals"
    );

    Ok(())
}

fn tempdir() -> anyhow::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join(format!("cnear-deploy-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
