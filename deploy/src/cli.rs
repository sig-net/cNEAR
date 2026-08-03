use clap::{Parser, ValueEnum};
use near_token::NearToken;
use std::path::PathBuf;

pub const DEFAULT_TOKEN_WASM: &str = "target/near/fungible_token.wasm";
pub const DEFAULT_CONTROLLER_WASM: &str = "target/near/aurora-controller-factory.wasm";
pub const DEFAULT_GAS: u64 = 300_000_000_000_000;

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
pub struct Cli {
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
        let cli = Cli::parse_from(["cnear-deploy"]);
        assert_eq!(cli.network, Network::Testnet);
        assert_eq!(cli.token_symbol, "cNEAR");
        assert_eq!(cli.token_decimals, 24);
        assert_eq!(
            cli.total_supply,
            NearToken::from_yoctonear(1_000_000_000_000_000)
        );
        assert_eq!(cli.initial_price, NearToken::from_near(1));
        assert_eq!(cli.initial_balance, NearToken::from_near(10));
    }

    #[test]
    fn cli_accepts_near_token_amounts() {
        use clap::Parser;
        let cli = Cli::parse_from([
            "cnear-deploy",
            "--total-supply",
            "0.000000001 NEAR",
            "--initial-price",
            "1 NEAR",
            "--initial-balance",
            "0.5 N",
        ]);
        assert_eq!(
            cli.total_supply,
            NearToken::from_yoctonear(1_000_000_000_000_000)
        );
        assert_eq!(cli.initial_price, NearToken::from_near(1));
        assert_eq!(cli.initial_balance, NearToken::from_millinear(500));
    }
}
