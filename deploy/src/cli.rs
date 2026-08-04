use clap::{Parser, ValueEnum};
use near_token::NearToken;
use std::path::PathBuf;

pub const DEFAULT_TOKEN_WASM: &str = "target/near/fungible_token.wasm";
pub const DEFAULT_CONTROLLER_WASM: &str = "target/near/aurora-controller-factory.wasm";

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Network {
    Testnet,
    Mainnet,
}

impl Network {
    pub fn rpc_url(self) -> &'static str {
        match self {
            Self::Testnet => "https://rpc.testnet.near.org",
            Self::Mainnet => "https://rpc.mainnet.near.org",
        }
    }

    pub fn subdir(self) -> &'static str {
        match self {
            Self::Testnet => "testnet",
            Self::Mainnet => "mainnet",
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "cnear-deploy",
    about = "Securely deploy cNEAR contracts without near CLI"
)]
pub enum Cli {
    /// Deploy cNEAR contracts (default when no subcommand is given)
    Deploy(DeployArgs),
    /// Delete accounts and send remaining balances to a beneficiary
    #[command(aliases = ["clean-up", "delete-accounts"])]
    CleanAccounts(CleanAccountsArgs),
}

#[derive(Parser, Debug)]
pub struct DeployArgs {
    #[arg(long, value_enum, default_value_t = Network::Testnet)]
    pub network: Network,
    #[arg(long)]
    pub signer_id: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub credentials: Option<PathBuf>,
    #[arg(long, default_value = DEFAULT_CONTROLLER_WASM)]
    pub controller_wasm: PathBuf,
    #[arg(long, default_value = DEFAULT_TOKEN_WASM)]
    pub token_wasm: PathBuf,
    #[arg(long)]
    pub controller_id: Option<String>,
    #[arg(long)]
    pub token_id: Option<String>,
    #[arg(long, default_value = "Controlled NEAR")]
    pub token_name: String,
    #[arg(long, default_value = "cNEAR")]
    pub token_symbol: String,
    #[arg(long, default_value_t = 24)]
    pub token_decimals: u8,
    #[arg(
        long,
        default_value = "0.000000001 NEAR",
        help = "Token total supply, as a decimal amount with a near-token unit (e.g. 0.000000001 NEAR, 1000000000000000 YN)"
    )]
    pub total_supply: NearToken,
    #[arg(
        long,
        default_value = "1 NEAR",
        help = "Initial indicative price in yoctoNEAR, as a decimal amount with a near-token unit (e.g. 1 NEAR)"
    )]
    pub initial_price: NearToken,
    #[arg(
        long,
        default_value = "10 NEAR",
        help = "Initial balance for new accounts, as a decimal amount with a near-token unit (e.g. 10 NEAR, 0.5 N, 1 MILLINEAR)"
    )]
    pub initial_balance: NearToken,
    #[arg(long)]
    pub redeploy: bool,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub test_mode: bool,
    #[arg(long)]
    pub yes: bool,
}

/// Translate the legacy `deploy.sh` positional form onto the typed subcommand
/// CLI, so `just deploy test`, `just deploy testnet`, `just deploy mainnet`,
/// and a bare signer after the network word all keep working. The program name
/// is kept at index 0; an empty argument list is passed through unchanged.
///
/// - `test` → `deploy --network testnet --test-mode`
/// - `testnet`/`mainnet` → `deploy --network <net>`
/// - a following non-flag word becomes `--signer-id <account>`
/// - a leading `-` (or unknown word) defaults to the `deploy` subcommand
/// - `deploy`/`clean-accounts`/`help` are passed through as explicit subcommands
pub fn translate_legacy_args(args: Vec<String>) -> Vec<String> {
    let Some(first) = args.get(1).map(String::as_str) else {
        return args;
    };
    let program = args[0].clone();
    let mut out = Vec::new();
    match first {
        "test" => {
            out.extend([
                program,
                "deploy".into(),
                "--network".into(),
                "testnet".into(),
                "--test-mode".into(),
            ]);
        }
        "testnet" | "mainnet" => {
            out.extend([program, "deploy".into(), "--network".into(), first.into()]);
        }
        // Explicit subcommands (including the clean-accounts aliases) pass through.
        "deploy" | "clean-accounts" | "clean-up" | "delete-accounts" | "help" => {
            return args;
        }
        _ => {
            // Unknown word: a bare account is treated as the signer; a leading
            // flag defaults to the deploy subcommand so typed flags work.
            out.push(program);
            out.push("deploy".into());
            if !first.starts_with('-') {
                out.push("--signer-id".into());
            }
            out.extend(args.iter().skip(1).cloned());
            return out;
        }
    }
    // Optional bare signer (non-flag) after the network word → --signer-id.
    for rest in args.iter().skip(2) {
        if !rest.starts_with('-') && !out.iter().any(|arg| arg == "--signer-id") {
            out.push("--signer-id".into());
        }
        out.push(rest.clone());
    }
    out
}

#[derive(Parser, Debug)]
pub struct CleanAccountsArgs {
    #[arg(long, value_enum, default_value_t = Network::Testnet)]
    pub network: Network,
    #[arg(long)]
    pub signer_id: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub credentials: Option<PathBuf>,
    #[arg(long)]
    pub controller_id: Option<String>,
    #[arg(long)]
    pub token_id: Option<String>,
    #[arg(long)]
    pub beneficiary: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_subdirs_match_credentials_layout() {
        assert_eq!(Network::Testnet.subdir(), "testnet");
        assert_eq!(Network::Mainnet.subdir(), "mainnet");
    }

    #[test]
    fn network_rpc_urls_are_stable() {
        assert_eq!(Network::Testnet.rpc_url(), "https://rpc.testnet.near.org");
        assert_eq!(Network::Mainnet.rpc_url(), "https://rpc.mainnet.near.org");
    }

    #[test]
    fn cli_defaults_are_sane() {
        use clap::Parser;
        let cli = Cli::parse_from(["cnear-deploy", "deploy"]);
        let Cli::Deploy(ref args) = cli else {
            panic!("expected Deploy variant")
        };
        assert_eq!(args.network, Network::Testnet);
        assert_eq!(args.token_symbol, "cNEAR");
        assert_eq!(args.token_decimals, 24);
        assert_eq!(
            args.total_supply,
            NearToken::from_yoctonear(1_000_000_000_000_000)
        );
        assert_eq!(args.initial_price, NearToken::from_near(1));
        assert_eq!(args.initial_balance, NearToken::from_near(10));
    }

    #[test]
    fn cli_accepts_near_token_amounts() {
        use clap::Parser;
        let cli = Cli::parse_from([
            "cnear-deploy",
            "deploy",
            "--total-supply",
            "0.000000001 NEAR",
            "--initial-price",
            "1 NEAR",
            "--initial-balance",
            "0.5 N",
        ]);
        let Cli::Deploy(ref args) = cli else {
            panic!("expected Deploy variant")
        };
        assert_eq!(
            args.total_supply,
            NearToken::from_yoctonear(1_000_000_000_000_000)
        );
        assert_eq!(args.initial_price, NearToken::from_near(1));
        assert_eq!(args.initial_balance, NearToken::from_millinear(500));
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn legacy_test_maps_to_ephemeral_testnet() {
        assert_eq!(
            translate_legacy_args(argv(&["cnear-deploy", "test"])),
            argv(&[
                "cnear-deploy",
                "deploy",
                "--network",
                "testnet",
                "--test-mode"
            ])
        );
        assert_eq!(
            translate_legacy_args(argv(&[
                "cnear-deploy",
                "test",
                "alice.testnet",
                "--dry-run"
            ])),
            argv(&[
                "cnear-deploy",
                "deploy",
                "--network",
                "testnet",
                "--test-mode",
                "--signer-id",
                "alice.testnet",
                "--dry-run"
            ])
        );
    }

    #[test]
    fn legacy_network_words_map_to_typed_network() {
        assert_eq!(
            translate_legacy_args(argv(&["cnear-deploy", "testnet"])),
            argv(&["cnear-deploy", "deploy", "--network", "testnet"])
        );
        assert_eq!(
            translate_legacy_args(argv(&["cnear-deploy", "mainnet"])),
            argv(&["cnear-deploy", "deploy", "--network", "mainnet"])
        );
        assert_eq!(
            translate_legacy_args(argv(&["cnear-deploy", "mainnet", "bob.near", "--dry-run"])),
            argv(&[
                "cnear-deploy",
                "deploy",
                "--network",
                "mainnet",
                "--signer-id",
                "bob.near",
                "--dry-run"
            ])
        );
    }

    #[test]
    fn legacy_flags_and_subcommands_pass_through() {
        // Explicit subcommands are untouched.
        assert_eq!(
            translate_legacy_args(argv(&["cnear-deploy", "clean-accounts"])),
            argv(&["cnear-deploy", "clean-accounts"])
        );
        // Aliases of clean-accounts also pass through untouched.
        assert_eq!(
            translate_legacy_args(argv(&["cnear-deploy", "clean-up"])),
            argv(&["cnear-deploy", "clean-up"])
        );
        assert_eq!(
            translate_legacy_args(argv(&["cnear-deploy", "deploy", "--yes"])),
            argv(&["cnear-deploy", "deploy", "--yes"])
        );
        // A leading flag defaults to the deploy subcommand.
        assert_eq!(
            translate_legacy_args(argv(&["cnear-deploy", "--network", "mainnet", "--yes"])),
            argv(&["cnear-deploy", "deploy", "--network", "mainnet", "--yes"])
        );
        // A bare account without a network word is treated as the signer.
        assert_eq!(
            translate_legacy_args(argv(&["cnear-deploy", "alice.testnet"])),
            argv(&["cnear-deploy", "deploy", "--signer-id", "alice.testnet"])
        );
        // No arguments at all are passed through.
        assert_eq!(
            translate_legacy_args(argv(&["cnear-deploy"])),
            argv(&["cnear-deploy"])
        );
    }
}
