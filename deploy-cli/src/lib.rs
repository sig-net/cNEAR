use anyhow::{anyhow, bail, Context, Result};
use near_workspaces::operations::Function;
use near_workspaces::types::{AccountId, KeyType, NearToken, SecretKey};
use near_workspaces::{Account, Network, Worker};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const BLUE: &str = "\x1b[0;34m";
const GREEN: &str = "\x1b[0;32m";
const RED: &str = "\x1b[0;31m";
const YELLOW: &str = "\x1b[1;33m";
const NC: &str = "\x1b[0m";

const DEFAULT_CONTROLLER_PREFIX: &str = "controller";
const DEFAULT_TOKEN_PREFIX: &str = "token";
const DEFAULT_TOKEN_NAME: &str = "cNEAR";
const DEFAULT_TOKEN_SYMBOL: &str = "cNEAR";
/// One NEAR, and equally one whole cNEAR: both use 24 decimals.
const ONE_NEAR: u128 = NearToken::from_near(1).as_yoctonear();
const DEFAULT_TOKEN_DECIMALS: u8 = 24;
/// The default supply cap is 100 trillion cNEAR (higher than it will ever need to be).
const DEFAULT_TOTAL_SUPPLY_TOKENS: u128 = 100_000_000_000_000;
/// Gas forwarded to the token when the controller delegates a call to it.
const DELEGATED_CALL_GAS: u64 = 20_000_000_000_000;
/// Version recorded for the initial release. Keep in sync with the token crate.
const RELEASE_VERSION: &str = "1.0.0";
/// NEAR charges this many yoctoNEAR per byte of storage.
/// NEAR charges 10^-5 NEAR per byte of storage.
const STORAGE_COST_PER_BYTE: u128 = ONE_NEAR / 100_000;
/// One NEAR per cNEAR.
const DEFAULT_INITIAL_PRICE: u128 = ONE_NEAR;
const DEFAULT_INITIAL_BALANCE: NearToken = NearToken::from_near(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkName {
    Testnet,
    Mainnet,
}

impl NetworkName {
    fn as_str(self) -> &'static str {
        match self {
            Self::Testnet => "testnet",
            Self::Mainnet => "mainnet",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub network: NetworkName,
    pub signer_id: AccountId,
    pub controller_id: AccountId,
    pub token_id: AccountId,
    pub token_name: String,
    pub token_symbol: String,
    pub token_decimals: u8,
    /// Whole tokens, as entered by the operator.
    pub total_supply_tokens: u128,
    /// `total_supply_tokens` scaled by `token_decimals`, in yoctoNEAR; what
    /// the token is actually initialised with.
    pub total_supply_yocto: u128,
    /// yoctoNEAR per whole cNEAR.
    pub initial_price: u128,
    pub initial_balance: NearToken,
    pub dry_run: bool,
    pub test_mode: bool,
}

#[derive(Debug)]
pub struct Options {
    network: Option<NetworkName>,
    signer_id: Option<String>,
    dry_run: bool,
    test_mode: bool,
    interactive: bool,
}

pub fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("deploy") => {}
        Some("finalize") => return finalize_command(&args[1..]),
        _ => {
            print_help();
            if args.is_empty() {
                return Ok(());
            }
            bail!("expected the 'deploy' or 'finalize' command");
        }
    }

    let config = build_config(parse_options(&args[1..])?)?;
    print_configuration(&config);
    if config.dry_run {
        print_dry_run(&config);
        return Ok(());
    }

    validate_wasm_files()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create async runtime")?;
    runtime.block_on(deploy(config))
}

fn print_help() {
    println!(
        "Usage: deploy <deploy|finalize> [testnet|mainnet] [signer_id] [--dry-run] [--test-mode]"
    );
    println!();
    println!("Commands:");
    println!("  deploy             Deploy the controller and token, and register the deployment");
    println!("  finalize           Remove access keys once the functional checks have passed");
    println!();
    println!("Options:");
    println!("  testnet|mainnet    Target network (default: testnet)");
    println!("  signer_id          Account ID to use for deployment");
    println!("  --dry-run          Show commands without executing");
    println!("  --test-mode        Quick deployment to testnet (only prompt for signer)");
    println!();
    println!("Examples:");
    println!("  deploy deploy                              # Interactive mode");
    println!("  deploy deploy testnet                      # Interactive signer selection");
    println!("  deploy deploy testnet alice.testnet        # Full CLI mode");
    println!("  deploy deploy mainnet bob.near --dry-run   # Dry run");
    println!("  deploy finalize mainnet bob.near           # Remove access keys");
    println!();
    println!("Via justfile:");
    println!("  just deploy test                           # Quick testnet deployment");
}

fn parse_options(args: &[String]) -> Result<Options> {
    let mut network = None;
    let mut signer_id = None;
    let mut dry_run = false;
    let mut test_mode = false;

    for arg in args {
        match arg.as_str() {
            "testnet" => network = Some(NetworkName::Testnet),
            "mainnet" => network = Some(NetworkName::Mainnet),
            "test" => {
                network = Some(NetworkName::Testnet);
                test_mode = true;
            }
            "--dry-run" => dry_run = true,
            "--test-mode" => test_mode = true,
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            value if value.starts_with('-') => bail!("unknown option '{value}'"),
            value if signer_id.is_none() => signer_id = Some(value.to_owned()),
            value => bail!("unknown argument '{value}'"),
        }
    }

    Ok(Options {
        network,
        signer_id,
        dry_run,
        test_mode,
        interactive: args.is_empty(),
    })
}

pub fn build_config(options: Options) -> Result<Config> {
    let network = match options.network {
        Some(network) => network,
        None if options.test_mode => NetworkName::Testnet,
        None => prompt_network()?,
    };
    println!("{GREEN}Network: {}{NC}", network.as_str());

    let credentials = credentials_dir(network)?;
    let signer_text = match options.signer_id {
        Some(signer_id) => signer_id,
        None => choose_signer(&credentials, options.test_mode)?,
    };
    let signer_id: AccountId = signer_text
        .parse()
        .with_context(|| format!("invalid signer account ID: {signer_text}"))?;
    println!("{GREEN}Signer: {signer_id}{NC}");

    let signer_path = credentials.join(format!("{signer_id}.json"));
    if signer_path.is_file() {
        println!("{GREEN}✓ Credentials found{NC}");
    } else if options.dry_run {
        println!(
            "{YELLOW}Warning: Credentials file not found: {} (continuing in dry-run){NC}",
            signer_path.display()
        );
    } else {
        bail!("credentials file not found: {}", signer_path.display());
    }

    let defaults = (
        format!("{DEFAULT_CONTROLLER_PREFIX}.{signer_id}"),
        format!("{DEFAULT_TOKEN_PREFIX}.{signer_id}"),
    );
    let values = if options.interactive && !options.test_mode {
        (
            prompt_default("Controller account ID", &defaults.0)?,
            prompt_default("Token account ID", &defaults.1)?,
            prompt_default("Token name", DEFAULT_TOKEN_NAME)?,
            prompt_default("Token symbol", DEFAULT_TOKEN_SYMBOL)?,
            prompt_default("Token decimals", &DEFAULT_TOKEN_DECIMALS.to_string())?
                .parse::<u8>()
                .context("token decimals must be an integer between 0 and 255")?,
            parse_u128(
                &prompt_default(
                    "Total supply (whole tokens)",
                    &DEFAULT_TOTAL_SUPPLY_TOKENS.to_string(),
                )?,
                "total supply",
            )?,
            parse_u128(
                prompt_default(
                    "Initial price in yoctoNEAR",
                    &format!("{DEFAULT_INITIAL_PRICE} (1 NEAR)"),
                )?
                .split_whitespace()
                .next()
                .unwrap_or_default(),
                "initial price",
            )?,
            parse_near_amount(&prompt_default(
                "Initial balance for new accounts in NEAR",
                &DEFAULT_INITIAL_BALANCE.exact_amount_display(),
            )?)?,
        )
    } else {
        (
            defaults.0,
            defaults.1,
            DEFAULT_TOKEN_NAME.to_owned(),
            DEFAULT_TOKEN_SYMBOL.to_owned(),
            DEFAULT_TOKEN_DECIMALS,
            DEFAULT_TOTAL_SUPPLY_TOKENS,
            DEFAULT_INITIAL_PRICE,
            DEFAULT_INITIAL_BALANCE,
        )
    };

    let controller_id = parse_account_id(&values.0, "controller")?;
    let token_id = parse_account_id(&values.1, "token")?;
    if options.test_mode {
        println!("{BLUE}Test mode: Using default configuration{NC}");
    }
    Ok(Config {
        network,
        signer_id,
        controller_id,
        token_id,
        token_name: values.2,
        token_symbol: values.3,
        token_decimals: values.4,
        total_supply_yocto: tokens_to_yocto(values.5, values.4, "total supply")?,
        total_supply_tokens: values.5,
        initial_price: values.6,
        initial_balance: values.7,
        dry_run: options.dry_run,
        test_mode: options.test_mode,
    })
}

pub fn credentials_dir(network: NetworkName) -> Result<PathBuf> {
    let base = env::var_os("NEAR_CREDENTIALS")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".near-credentials")))
        .ok_or_else(|| anyhow!("HOME is not set and NEAR_CREDENTIALS is not set"))?;
    let path = base.join(network.as_str());
    if !path.is_dir() {
        bail!(
            "NEAR credentials directory does not exist: {}\nExpected: ~/.near-credentials/{} or $NEAR_CREDENTIALS/{}",
            path.display(),
            network.as_str(),
            network.as_str()
        );
    }
    Ok(path)
}

fn choose_signer(credentials: &Path, test_mode: bool) -> Result<String> {
    println!(
        "\n{BLUE}Available accounts in {}:{NC}",
        credentials.display()
    );
    let mut accounts = fs::read_dir(credentials)
        .with_context(|| format!("failed to read {}", credentials.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .filter_map(|path| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect::<Vec<_>>();
    accounts.sort();
    if accounts.is_empty() {
        bail!("no account credentials found in {}", credentials.display());
    }
    for (index, account) in accounts.iter().enumerate() {
        println!("  {}) {}", index + 1, account);
    }
    let prompt = if test_mode {
        "Select signer account for test deployment [1]"
    } else {
        "Select account [1]"
    };
    let choice = prompt_line(prompt)?;
    let index = if choice.trim().is_empty() {
        0
    } else {
        choice
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|value| (1..=accounts.len()).contains(value))
            .map(|value| value - 1)
            .ok_or_else(|| anyhow!("invalid choice"))?
    };
    Ok(accounts[index].clone())
}

fn prompt_network() -> Result<NetworkName> {
    println!("{BLUE}Select network:{NC}");
    println!("  1) testnet (default)");
    println!("  2) mainnet");
    match prompt_line("Enter choice [1]")?.trim() {
        "" | "1" => Ok(NetworkName::Testnet),
        "2" => Ok(NetworkName::Mainnet),
        _ => {
            println!("{RED}Invalid choice. Using testnet.{NC}");
            Ok(NetworkName::Testnet)
        }
    }
}

fn prompt_default(label: &str, default: &str) -> Result<String> {
    let value = prompt_line(&format!("{label} [{default}]:"))?;
    Ok(if value.trim().is_empty() {
        default.to_owned()
    } else {
        value.trim().to_owned()
    })
}

fn prompt_line(label: &str) -> Result<String> {
    print!("{label} ");
    io::stdout().flush().context("failed to flush prompt")?;
    let mut value = String::new();
    io::stdin()
        .read_line(&mut value)
        .context("failed to read input")?;
    Ok(value.trim_end_matches(['\r', '\n']).to_owned())
}

fn prompt_yes_no(question: &str) -> Result<bool> {
    let answer = prompt_line(&format!("{question} [y/N]"))?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn parse_account_id(value: &str, label: &str) -> Result<AccountId> {
    value
        .parse()
        .with_context(|| format!("invalid {label} account ID: {value}"))
}

/// Convert a whole-token amount into yoctoNEAR, rejecting anything that cannot
/// be represented rather than silently wrapping.
pub fn tokens_to_yocto(tokens: u128, decimals: u8, label: &str) -> Result<u128> {
    if tokens == 0 {
        bail!("{label} must be greater than zero");
    }
    let multiplier = 10u128
        .checked_pow(u32::from(decimals))
        .ok_or_else(|| anyhow!("{decimals} decimals is too many to represent"))?;
    tokens.checked_mul(multiplier).ok_or_else(|| {
        anyhow!("{label} of {tokens} tokens at {decimals} decimals overflows a u128")
    })
}

fn parse_u128(value: &str, label: &str) -> Result<u128> {
    value
        .parse()
        .with_context(|| format!("invalid {label}: {value}"))
}

fn parse_near_amount(value: &str) -> Result<NearToken> {
    let mut parts = value.trim().split('.');
    let whole = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| anyhow!("missing whole NEAR amount"))?
        .parse::<u128>()?;
    let fraction = parts.next().unwrap_or("");
    if parts.next().is_some()
        || fraction.len() > 24
        || !fraction.chars().all(|c| c.is_ascii_digit())
    {
        bail!("NEAR amount must have at most 24 decimal places");
    }
    let fraction_value = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u128>()? * 10u128.pow((24 - fraction.len()) as u32)
    };
    let yoctonear = whole
        .checked_mul(10u128.pow(24))
        .and_then(|whole| whole.checked_add(fraction_value))
        .ok_or_else(|| anyhow!("NEAR amount is too large"))?;
    Ok(NearToken::from_yoctonear(yoctonear))
}

fn target_dir() -> PathBuf {
    env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"))
}

fn token_wasm() -> PathBuf {
    target_dir().join("near/fungible_token.wasm")
}
fn controller_wasm() -> PathBuf {
    target_dir().join("near/aurora-controller-factory.wasm")
}

fn validate_wasm_files() -> Result<()> {
    if !token_wasm().is_file() {
        bail!(
            "token wasm not found at {}\nRun: just build-token",
            token_wasm().display()
        );
    }
    if !controller_wasm().is_file() {
        bail!(
            "controller wasm not found at {}\nRun: just build-controller",
            controller_wasm().display()
        );
    }
    println!("{GREEN}✓ Wasm files found{NC}");
    Ok(())
}

fn print_configuration(config: &Config) {
    println!("\n{BLUE}=== Deployment Summary ==={NC}");
    println!("Network:           {}", config.network.as_str());
    println!("Signer:            {}", config.signer_id);
    println!("Controller:        {}", config.controller_id);
    println!("Token:             {}", config.token_id);
    println!("Token Name:        {}", config.token_name);
    println!("Token Symbol:      {}", config.token_symbol);
    println!("Token Decimals:    {}", config.token_decimals);
    println!(
        "Total Supply:      {} tokens ({} yoctoNEAR at {} decimals)",
        config.total_supply_tokens, config.total_supply_yocto, config.token_decimals
    );
    println!(
        "Initial Price:     {} yoctoNEAR ({} per cNEAR)",
        config.initial_price,
        NearToken::from_yoctonear(config.initial_price).exact_amount_display()
    );
    println!("Dry Run:           {}", config.dry_run);
    println!();
}

fn print_dry_run(config: &Config) {
    println!("{YELLOW}=== DRY RUN MODE - Commands will be displayed but not executed ==={NC}\n");
    if config.test_mode {
        println!("{GREEN}=== Test Mode: Creating Subaccounts ==={NC}");
        print_command(&format!(
            "near create-account {} --masterAccount {} --initialBalance {} --networkId {}",
            config.controller_id,
            config.signer_id,
            config.initial_balance,
            config.network.as_str()
        ));
        print_command(&format!(
            "near create-account {} --masterAccount {} --initialBalance {} --networkId {}",
            config.token_id,
            config.signer_id,
            config.initial_balance,
            config.network.as_str()
        ));
    } else {
        println!("{GREEN}=== Checking Account Existence ==={NC}");
        println!("{YELLOW}Skipping account existence check in dry-run mode{NC}");
    }
    println!("{GREEN}=== Step 1: Deploy Controller ==={NC}");
    print_command(&format!(
        "near deploy {} {} --initFunction new --initArgs '{{\"dao\":\"{}\"}}' --networkId {}",
        config.controller_id,
        controller_wasm().display(),
        config.signer_id,
        config.network.as_str()
    ));
    println!("{GREEN}=== Step 2: Deploy Token with Signer as Initial Owner ==={NC}");
    print_command(&format!(
        "near deploy {} {} --initFunction new --initArgs '{}' --networkId {}",
        config.token_id,
        token_wasm().display(),
        token_init_args(config),
        config.network.as_str()
    ));
    println!("{GREEN}=== Step 3: Propose Controller as Token Owner ==={NC}");
    print_command(&format!(
        "near call {} owner_set '{{\"new_owner\":\"{}\"}}' --accountId {} --networkId {}",
        config.token_id,
        config.controller_id,
        config.signer_id,
        config.network.as_str()
    ));
    println!("{GREEN}=== Step 4: Accept Token Ownership as Controller ==={NC}");
    print_command(&format!(
        "near call {} delegate_execution '{{\"receiver_id\":\"{}\",\"actions\":[{{\"function_name\":\"owner_accept\",\"arguments\":\"\",\"amount\":\"0\",\"gas\":\"{}\"}}]}}' --accountId {} --networkId {} --amount 0.000000000000000000000001",
        config.controller_id,
        config.token_id,
        DELEGATED_CALL_GAS,
        config.signer_id,
        config.network.as_str()
    ));
    println!("{GREEN}=== Step 5: Register Token Deployment with Controller ==={NC}");
    print_command(&format!(
        "near call {} add_release_info '{{\"hash\":\"<token-wasm-sha256>\",\"version\":\"{}\",\"is_latest\":true}}' --accountId {} --networkId {} --amount 0.000000000000000000000001",
        config.controller_id,
        RELEASE_VERSION,
        config.signer_id,
        config.network.as_str()
    ));
    // The blob is stored in the controller's state, so this call carries a
    // deposit sized to it. Showing it matters: this preview is the only
    // rehearsal a mainnet deployment gets.
    let blob_deposit = fs::metadata(token_wasm())
        .map(|meta| {
            NearToken::from_yoctonear(meta.len() as u128 * STORAGE_COST_PER_BYTE + ONE_NEAR)
        })
        .unwrap_or_else(|_| NearToken::from_near(0));
    print_command(&format!(
        "near call {} add_release_blob --accountId {} --networkId {} --amount {} < {}",
        config.controller_id,
        config.signer_id,
        config.network.as_str(),
        near_amount_arg(blob_deposit),
        token_wasm().display()
    ));
    print_command(&format!(
        "near call {} add_deployment_info '{{\"contract_id\":\"{}\",...}}' --accountId {} --networkId {} --amount 0.000000000000000000000001",
        config.controller_id,
        config.token_id,
        config.signer_id,
        config.network.as_str()
    ));
    print_command(&format!(
        "near view {} get_deployment '{{\"account_id\":\"{}\"}}' --networkId {}",
        config.controller_id,
        config.token_id,
        config.network.as_str()
    ));
    println!("{GREEN}=== Step 6: Verify Ownership ==={NC}");
    print_command(&format!(
        "near view {} owner_get '{{}}' --networkId {}",
        config.token_id,
        config.network.as_str()
    ));
    if config.test_mode {
        println!("\n{YELLOW}=== Test Mode Cleanup (Dry Run) ==={NC}");
        print_command(&format!(
            "near delete-account {} {} --networkId {}",
            config.token_id,
            config.signer_id,
            config.network.as_str()
        ));
        print_command(&format!(
            "near delete-account {} {} --networkId {}",
            config.controller_id,
            config.signer_id,
            config.network.as_str()
        ));
    }
    println!("{YELLOW}=== Dry Run Complete - No changes made ==={NC}");
}

/// `--amount` takes a bare number, but `exact_amount_display` appends " NEAR",
/// so printing it directly yields a command that cannot be pasted into a shell.
fn near_amount_arg(amount: NearToken) -> String {
    amount
        .exact_amount_display()
        .split_whitespace()
        .next()
        .unwrap_or("0")
        .to_owned()
}

fn print_command(command: &str) {
    println!("{BLUE}Command:{NC} {command}");
    println!("{YELLOW}[DRY RUN - not executed]{NC}\n");
}

fn token_init_args(config: &Config) -> serde_json::Value {
    json!({"owner_id": config.signer_id, "total_supply": config.total_supply_yocto.to_string(), "metadata": {"spec": "ft-1.0.0", "name": config.token_name, "symbol": config.token_symbol, "decimals": config.token_decimals}, "latest_price": config.initial_price.to_string()})
}

async fn deploy(config: Config) -> Result<()> {
    match config.network {
        NetworkName::Testnet => deploy_on_worker(near_workspaces::testnet().await?, config).await,
        NetworkName::Mainnet => deploy_on_worker(near_workspaces::mainnet().await?, config).await,
    }
}

async fn deploy_on_worker<W>(worker: Worker<W>, config: Config) -> Result<()>
where
    W: Network + 'static,
{
    let credentials = credentials_dir(config.network)?;
    let signer_path = credentials.join(format!("{}.json", config.signer_id));
    let signer =
        Account::from_file(&signer_path, &worker).context("failed to load signer credentials")?;
    if signer.id() != &config.signer_id {
        bail!(
            "credentials file belongs to {}, not {}",
            signer.id(),
            config.signer_id
        );
    }
    deploy_with_signer(worker, signer, config).await
}

/// The deployment itself, once a signer is in hand. Separated from
/// [`deploy_on_worker`] so tests can drive it against a sandbox without
/// needing credential files on disk.
pub async fn deploy_with_signer<W>(worker: Worker<W>, signer: Account, config: Config) -> Result<()>
where
    W: Network + 'static,
{
    let credentials = credentials_dir(config.network)?;
    let mut created_controller = None;
    let mut created_token = None;
    let setup: Result<(Account, Account)> = async {
        if config.test_mode {
            println!("{GREEN}=== Test Mode: Creating Subaccounts ==={NC}");
            println!("\n{BLUE}Creating controller account...{NC}");
            let controller =
                create_account(&signer, &config.controller_id, config.initial_balance).await?;
            created_controller = Some(controller.clone());
            success();
            println!("\n{BLUE}Creating token account...{NC}");
            let token = create_account(&signer, &config.token_id, config.initial_balance).await?;
            created_token = Some(token.clone());
            success();
            Ok((controller, token))
        } else {
            println!("{GREEN}=== Checking Account Existence ==={NC}");
            let controller = target_account(
                &worker,
                &signer,
                &credentials,
                &config.controller_id,
                config.initial_balance,
                &mut created_controller,
            )
            .await?;
            let token = target_account(
                &worker,
                &signer,
                &credentials,
                &config.token_id,
                config.initial_balance,
                &mut created_token,
            )
            .await?;
            Ok((controller, token))
        }
    }
    .await;
    let (controller, token) = match setup {
        Ok(accounts) => accounts,
        Err(error) => {
            if !cleanup_accounts(created_token, created_controller, &config.signer_id).await {
                eprintln!("{RED}✗ Cleanup was incomplete after setup failure{NC}");
            }
            return Err(error);
        }
    };

    let deployment = deploy_contracts(&signer, &controller, &token, &config).await;
    if let Err(error) = deployment {
        if !cleanup_accounts(created_token, created_controller, &config.signer_id).await {
            eprintln!("{RED}✗ Cleanup was incomplete after deployment failure{NC}");
        }
        return Err(error);
    }

    if config.test_mode {
        println!("\n{YELLOW}=== Test Mode Cleanup ==={NC}");
        println!(
            "Deleting temporary accounts with beneficiary: {BLUE}{}{NC}",
            config.signer_id
        );
        if cleanup_accounts(created_token, created_controller, &config.signer_id).await {
            println!(
                "{GREEN}✓ Cleanup complete - funds returned to {}{NC}",
                config.signer_id
            );
        } else {
            bail!("test mode cleanup was incomplete");
        }
    } else {
        println!("\nNext steps:\n  1. Add release info to controller\n  2. Test pause/freeze via controller\n  3. Test upgrades via controller");
    }
    Ok(())
}

async fn target_account<W>(
    worker: &Worker<W>,
    signer: &Account,
    credentials: &Path,
    id: &AccountId,
    balance: NearToken,
    created: &mut Option<Account>,
) -> Result<Account>
where
    W: Network + 'static,
{
    match worker.view_account(id).await {
        Ok(_) => {
            let path = credentials.join(format!("{id}.json"));
            if !path.is_file() {
                bail!(
                    "account {} exists but credentials are missing at {}",
                    id,
                    path.display()
                );
            }
            let account = Account::from_file(&path, worker)?;
            if account.id() != id {
                bail!("credentials for {} belong to {}", id, account.id());
            }
            println!("{GREEN}✓ Account {id} exists{NC}");
            Ok(account)
        }
        Err(error) if account_does_not_exist(&error) => {
            println!("{YELLOW}Account {id} does not exist{NC}");
            if !prompt_yes_no(&format!(
                "\n{BLUE}Create account {id} with {balance} NEAR?{NC}"
            ))? {
                bail!("account {id} does not exist and was not created; aborting");
            }
            println!("\n{BLUE}Creating account...{NC}");
            let account = create_account(signer, id, balance).await?;
            *created = Some(account.clone());
            store_near_cli_credentials(&account, credentials)?;
            success();
            Ok(account)
        }
        Err(error) => Err(error).context(format!("failed to check account {id}")),
    }
}

/// Detect the RPC "account does not exist" condition. The handler message is nested in the
/// error's source chain (the top-level Display is just "unable to fulfill the query request"),
/// so walk the chain instead of matching the top-level message.
pub fn account_does_not_exist(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut source = Some(error);
    while let Some(err) = source {
        if err.to_string().contains("does not exist while viewing") {
            return true;
        }
        source = err.source();
    }
    false
}

/// Store the credentials of a freshly created account in the near CLI keychain format
/// (`private_key`). near-workspaces' own `store_credentials` writes `secret_key`, which
/// near-cli-rs (e.g. 0.29) cannot parse; near-crypto's `InMemorySigner::from_file` accepts
/// `private_key` via a serde alias, so this single format works for both tools.
pub fn store_near_cli_credentials(account: &Account, dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let path = dir.join(format!("{}.json", account.id()));
    let secret_key = account.secret_key();
    let content = json!({
        "account_id": account.id().as_str(),
        "public_key": secret_key.public_key().to_string(),
        "private_key": secret_key.to_string(),
    })
    .to_string();
    fs::write(&path, content)
        .with_context(|| format!("failed to write credentials to {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    }
    Ok(())
}

async fn create_account(
    signer: &Account,
    target_id: &AccountId,
    initial_balance: NearToken,
) -> Result<Account> {
    let suffix = target_id
        .as_str()
        .strip_suffix(&format!(".{}", signer.id()))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "account {} must be a subaccount of {}",
                target_id,
                signer.id()
            )
        })?;
    let account = signer
        .create_subaccount(suffix)
        .keys(SecretKey::from_random(KeyType::ED25519))
        .initial_balance(initial_balance)
        .transact()
        .await?
        .into_result()
        .context("account creation failed")?;
    if account.id() != target_id {
        bail!(
            "created unexpected account {} instead of {}",
            account.id(),
            target_id
        );
    }
    Ok(account)
}

async fn deploy_contracts(
    signer: &Account,
    controller: &Account,
    token: &Account,
    config: &Config,
) -> Result<()> {
    println!("{GREEN}=== Step 1: Deploy Controller ==={NC}");
    deploy_and_initialize(
        controller,
        controller.id(),
        &fs::read(controller_wasm())?,
        "new",
        json!({"dao": config.signer_id}),
    )
    .await?;
    success();
    println!("{GREEN}=== Step 2: Deploy Token with Signer as Initial Owner ==={NC}");
    deploy_and_initialize(
        token,
        token.id(),
        &fs::read(token_wasm())?,
        "new",
        token_init_args(config),
    )
    .await?;
    success();
    println!("{GREEN}=== Step 3: Propose Controller as Token Owner ==={NC}");
    // Ownership transfer is two-step: this records the proposal only, and the
    // controller has to accept it in step 4. owner_set is not #[payable], so no
    // deposit may be attached.
    signer
        .call(token.id(), "owner_set")
        .args_json(json!({"new_owner": controller.id()}))
        .transact()
        .await?
        .into_result()
        .context("proposing the controller as token owner failed")?;
    success();
    println!("{GREEN}=== Step 4: Accept Token Ownership as Controller ==={NC}");
    // Only the proposed account can accept, so driving this through the
    // controller proves the DAO can operate the controller, and the controller
    // the token, before ownership becomes final.
    signer
        .call(controller.id(), "delegate_execution")
        .args_json(json!({
            "receiver_id": token.id(),
            "actions": [{
                "function_name": "owner_accept",
                "arguments": "",
                "amount": "0",
                "gas": DELEGATED_CALL_GAS.to_string(),
            }],
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()
        .context("controller failed to accept token ownership")?;
    success();
    println!("{GREEN}=== Step 5: Register Token Deployment with Controller ==={NC}");
    register_deployment(signer, controller, token).await?;
    success();
    println!("{GREEN}=== Step 6: Verify Ownership ==={NC}");
    println!("{BLUE}Verifying token owner...{NC}");
    verify_ownership(signer, token.id(), controller.id()).await?;
    println!("{GREEN}=== Deployment Complete ==={NC}");
    println!("Controller: {BLUE}{}{NC}", controller.id());
    println!("Token:      {BLUE}{}{NC}", token.id());
    println!("Owner:      {BLUE}{}{NC}", controller.id());
    Ok(())
}

/// Register the token with the controller's deployment registry.
///
/// The controller looks up a deployment record before it will build an upgrade
/// promise, and panics with "hasn't been deployed" when none exists, so without
/// this every controller-mediated upgrade is rejected (TOB-CNEAR-5). Registering
/// the release itself as well means the currently deployed code is known to the
/// controller, which is what makes a downgrade back to it possible.
/// Remove the access keys that let a key holder bypass the contract's own
/// authorization (TOB-CNEAR-2).
///
/// This is deliberately a separate command rather than part of `deploy`: it must
/// run *after* the release, pause, freeze and upgrade checks, because a keyless
/// contract whose upgrade path does not work cannot be repaired by anyone.
fn finalize_command(args: &[String]) -> Result<()> {
    let options = parse_options(args)?;
    let config = build_config(options)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create async runtime")?;
    runtime.block_on(finalize(config))
}

async fn finalize(config: Config) -> Result<()> {
    match config.network {
        NetworkName::Testnet => finalize_on_worker(near_workspaces::testnet().await?, config).await,
        NetworkName::Mainnet => finalize_on_worker(near_workspaces::mainnet().await?, config).await,
    }
}

async fn finalize_on_worker<W>(worker: Worker<W>, config: Config) -> Result<()>
where
    W: Network + 'static,
{
    let credentials = credentials_dir(config.network)?;

    println!("{YELLOW}Removing every access key from the token account, and any");
    println!("key you do not confirm as required from the controller account.{NC}");
    println!(
        "{RED}Only do this once you have verified that upgrades through the \
controller work. Without a key, a contract whose upgrade path is broken \
cannot be repaired.{NC}\n"
    );
    if !prompt_yes_no("Have the release, pause, freeze and upgrade checks all passed?")? {
        bail!("finalization aborted: run the functional checks first");
    }

    remove_access_keys(&worker, &credentials, &config.token_id, true).await?;
    remove_access_keys(&worker, &credentials, &config.controller_id, false).await?;

    println!("{GREEN}=== Finalization Complete ==={NC}");
    Ok(())
}

/// List an account's access keys, delete the ones the operator confirms, then
/// read the key set back so the result is verified rather than assumed.
pub async fn remove_access_keys<W>(
    worker: &Worker<W>,
    credentials: &Path,
    account_id: &AccountId,
    remove_all: bool,
) -> Result<()>
where
    W: Network + 'static,
{
    println!("{GREEN}=== Access keys on {account_id} ==={NC}");
    let keys = worker
        .view_access_keys(account_id)
        .await
        .with_context(|| format!("failed to list access keys on {account_id}"))?;
    if keys.is_empty() {
        println!("{GREEN}✓ {account_id} already has no access keys{NC}\n");
        return Ok(());
    }
    for key in &keys {
        println!("  {}", key.public_key);
    }

    let path = credentials.join(format!("{account_id}.json"));
    let account = Account::from_file(&path, worker)
        .with_context(|| format!("need credentials for {account_id} at {}", path.display()))?;

    for key in &keys {
        let remove = remove_all
            || prompt_yes_no(&format!(
                "Remove {} from {account_id}? (keep it only if your governance model requires it)",
                key.public_key
            ))?;
        if !remove {
            println!("{YELLOW}  keeping {}{NC}", key.public_key);
            continue;
        }
        account
            .batch(account_id)
            .delete_key(key.public_key.clone())
            .transact()
            .await?
            .into_result()
            .with_context(|| format!("failed to delete {} from {account_id}", key.public_key))?;
        println!("{GREEN}  removed {}{NC}", key.public_key);
    }

    let remaining = worker
        .view_access_keys(account_id)
        .await
        .with_context(|| format!("failed to re-read access keys on {account_id}"))?;
    if remove_all && !remaining.is_empty() {
        bail!(
            "{} still has {} access key(s) after finalization",
            account_id,
            remaining.len()
        );
    }
    println!(
        "{GREEN}✓ {account_id} now has {} access key(s){NC}\n",
        remaining.len()
    );
    Ok(())
}

async fn register_deployment(
    signer: &Account,
    controller: &Account,
    token: &Account,
) -> Result<()> {
    let wasm = fs::read(token_wasm())?;
    let hash = format!("{:x}", Sha256::digest(&wasm));

    signer
        .call(controller.id(), "add_release_info")
        .args_json(json!({
            "hash": &hash,
            "version": RELEASE_VERSION,
            "is_latest": true,
            "downgrade_hash": null,
            "description": "cNEAR",
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()
        .context("registering the release with the controller failed")?;

    // The blob is stored in the controller's state, so the deposit has to cover
    // its storage staking.
    let blob_deposit =
        NearToken::from_yoctonear(wasm.len() as u128 * STORAGE_COST_PER_BYTE + ONE_NEAR);
    signer
        .call(controller.id(), "add_release_blob")
        .args(wasm)
        .deposit(blob_deposit)
        .max_gas()
        .transact()
        .await?
        .into_result()
        .context("uploading the release blob to the controller failed")?;

    signer
        .call(controller.id(), "add_deployment_info")
        .args_json(json!({
            "contract_id": token.id(),
            "deployment_info": {
                "hash": &hash,
                "version": RELEASE_VERSION,
                "deployment_time": 0u64,
                "upgrade_times": {},
                "init_args": "",
            },
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()
        .context("registering the deployment with the controller failed")?;

    // Reporting success without checking the registry is how TOB-CNEAR-5 stayed
    // invisible, so read the record back and check it describes this token.
    let deployment: serde_json::Value = signer
        .call(controller.id(), "get_deployment")
        .args_json(json!({ "account_id": token.id() }))
        .view()
        .await?
        .json()?;
    let recorded_hash = deployment.get("hash").and_then(|h| h.as_str());
    if recorded_hash != Some(hash.as_str()) {
        bail!(
            "deployment registration failed: controller recorded hash {:?}, expected {}",
            recorded_hash,
            hash
        );
    }
    println!("{GREEN}✓ Deployment registered: {hash}{NC}");
    Ok(())
}

/// Read the token's owner and fail unless it is `expected`.
///
/// TOB-CNEAR-6: the script printed a message and carried on, so a deployment
/// whose ownership never moved still reported success and still exited zero.
/// Both the query failing and the owner differing have to stop the deployment.
pub async fn verify_ownership(
    caller: &Account,
    token_id: &AccountId,
    expected: &AccountId,
) -> Result<()> {
    let owner: String = caller
        .call(token_id, "owner_get")
        .view()
        .await
        .context("could not read the token's owner")?
        .json()
        .context("could not parse the token's owner")?;
    if owner != expected.as_str() {
        bail!("ownership verification failed: expected {expected}, got {owner}");
    }
    println!("{GREEN}✓ Ownership verified: {owner}{NC}\n");
    Ok(())
}

async fn deploy_and_initialize(
    actor: &Account,
    target: &AccountId,
    wasm: &[u8],
    function: &str,
    args: serde_json::Value,
) -> Result<()> {
    actor
        .batch(target)
        .deploy(wasm)
        .call(Function::new(function).args_json(args).max_gas())
        .transact()
        .await?
        .into_result()
        .context("contract deployment or initialization failed")?;
    Ok(())
}

async fn cleanup_accounts(
    token: Option<Account>,
    controller: Option<Account>,
    beneficiary: &AccountId,
) -> bool {
    let mut complete = true;
    if let Some(account) = token {
        println!("\n{BLUE}Deleting token account...{NC}");
        match account.delete_account(beneficiary).await {
            Ok(result) if result.is_success() => success(),
            Ok(result) => {
                complete = false;
                eprintln!("{RED}✗ Failed to delete token account: {result:#?}{NC}");
            }
            Err(error) => {
                complete = false;
                eprintln!("{RED}✗ Failed to delete token account: {error:#}{NC}");
            }
        }
    }
    if let Some(account) = controller {
        println!("\n{BLUE}Deleting controller account...{NC}");
        match account.delete_account(beneficiary).await {
            Ok(result) if result.is_success() => success(),
            Ok(result) => {
                complete = false;
                eprintln!("{RED}✗ Failed to delete controller account: {result:#?}{NC}");
            }
            Err(error) => {
                complete = false;
                eprintln!("{RED}✗ Failed to delete controller account: {error:#}{NC}");
            }
        }
    }
    complete
}

fn success() {
    println!("{GREEN}✓ Success{NC}\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_amounts() {
        assert_eq!(
            parse_near_amount("10").unwrap().as_yoctonear(),
            10 * 10u128.pow(24)
        );
        assert_eq!(
            parse_near_amount("1.5").unwrap().as_yoctonear(),
            15 * 10u128.pow(23)
        );
    }
    #[test]
    fn rejects_invalid_amounts() {
        assert!(parse_near_amount("1.0000000000000000000000001").is_err());
        assert!(parse_near_amount("1.2.3").is_err());
    }
    #[test]
    fn parses_test_alias() {
        let options = parse_options(&["test".into(), "--dry-run".into()]).unwrap();
        assert_eq!(options.network, Some(NetworkName::Testnet));
        assert!(options.test_mode && options.dry_run);
    }

    #[test]
    fn bare_dry_run_uses_defaults_without_explicit_mode() {
        let options = parse_options(&["--dry-run".into()]).unwrap();
        assert_eq!(options.network, None);
        assert!(options.dry_run);
        assert!(!options.test_mode);
        assert!(!options.interactive);
    }

    /// TOB-CNEAR-4. The old script treated any output containing "Account" as
    /// proof the account existed, so an RPC failure read as "it is there" and
    /// creation was skipped. The replacement keys off one specific condition,
    /// and everything else has to propagate.
    #[test]
    fn only_the_not_found_condition_counts_as_absence() {
        #[derive(Debug)]
        struct Err(String);
        impl std::fmt::Display for Err {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
        impl std::error::Error for Err {}

        let absent = Err("account alice.near does not exist while viewing".to_owned());
        assert!(account_does_not_exist(&absent));

        // The failures that matter: an unreachable endpoint, a timeout, a
        // rejected request. None of these say anything about the account.
        for message in [
            "unable to fulfill the query request",
            "error sending request for url (https://rpc.mainnet.near.org/)",
            "operation timed out",
            "429 Too Many Requests",
        ] {
            let error = Err(message.to_owned());
            assert!(
                !account_does_not_exist(&error),
                "{message:?} must not be read as the account being absent"
            );
        }
    }

    /// The not-found condition is nested in the error's source chain rather
    /// than in the message the top level prints, so the check has to walk it.
    #[test]
    fn finds_the_not_found_condition_through_a_source_chain() {
        #[derive(Debug)]
        struct Inner;
        impl std::fmt::Display for Inner {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "account bob.near does not exist while viewing")
            }
        }
        impl std::error::Error for Inner {}

        #[derive(Debug)]
        struct Outer(Inner);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "unable to fulfill the query request")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        assert!(account_does_not_exist(&Outer(Inner)));
    }

    /// TOB-CNEAR-3. Entering the supply in atomic units against 24 decimals is
    /// how the original default minted a millionth of a token.
    #[test]
    fn whole_tokens_scale_by_the_decimals() {
        assert_eq!(
            tokens_to_yocto(100_000_000_000_000, 24, "supply").unwrap(),
            100_000_000_000_000 * ONE_NEAR
        );
        assert_eq!(tokens_to_yocto(1, 24, "supply").unwrap(), ONE_NEAR);
    }

    #[test]
    fn a_supply_that_cannot_be_represented_is_rejected() {
        assert!(tokens_to_yocto(0, 24, "supply").is_err(), "zero supply");
        assert!(
            tokens_to_yocto(u128::MAX, 24, "supply").is_err(),
            "overflowing supply must not wrap"
        );
    }
}
