mod actions;
mod cli;
mod config;
mod credentials;
mod prompt;
mod rpc;
mod transaction;

use crate::actions::{clean_accounts, deploy};
use crate::cli::{translate_legacy_args, Cli};
use crate::config::{build_clean_config, build_config};
use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    // Map the legacy deploy.sh positional form (`test`/`testnet`/`mainnet`
    // and a bare signer) onto the typed subcommand CLI before parsing.
    let argv = translate_legacy_args(std::env::args().collect());

    // With no arguments at all, or a subcommand with no flags, drive the fully
    // interactive flow — mirroring `just deploy` / `just deploy clean-accounts`.
    let interactive = argv.len() <= 1
        || (argv.len() == 2 && matches!(argv[1].as_str(), "deploy" | "clean-accounts"));

    if interactive {
        return match argv.get(1).map(String::as_str) {
            Some("clean-accounts") => {
                let args = prompt::prompt_clean()?;
                let config = build_clean_config(args)?;
                clean_accounts(&config).await
            }
            _ => {
                let args = prompt::run()?;
                let config = build_config(args)?;
                deploy(&config).await
            }
        };
    }

    let cli = Cli::parse_from(argv);
    match cli {
        Cli::Deploy(args) => {
            let config = build_config(args)?;
            deploy(&config).await
        }
        Cli::CleanAccounts(args) => {
            let config = build_clean_config(args)?;
            clean_accounts(&config).await
        }
    }
}
