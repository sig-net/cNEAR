mod actions;
mod cli;
mod config;
mod credentials;
mod prompt;
mod rpc;
mod transaction;

use crate::actions::deploy;
use crate::cli::Cli;
use crate::config::build_config;
use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    // No flags → drive the fully interactive configuration flow (mirrors the
    // original deploy.sh). Any flag → typed, non-interactive CLI behavior.
    let cli = if std::env::args().count() == 1 {
        prompt::run()?
    } else {
        Cli::parse()
    };
    let config = build_config(cli)?;
    deploy(&config).await
}
