mod actions;
mod cli;
mod config;
mod credentials;
mod rpc;
mod transaction;

use crate::actions::deploy;
use crate::cli::Cli;
use crate::config::build_config;
use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = build_config(cli)?;
    deploy(&config).await
}
