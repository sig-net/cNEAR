use crate::cli::{Cli, Network, DEFAULT_CONTROLLER_WASM, DEFAULT_TOKEN_WASM};
use crate::credentials::select_signer;
use anyhow::{anyhow, bail, Result};
use near_token::NearToken;
use std::fmt::Display;
use std::io::{self, Write};
use std::path::PathBuf;
use std::str::FromStr;

/// Read one trimmed line from stdin after printing `prompt`.
fn read_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

/// Parse `input` into `T`, falling back to `default` when empty or blank.
/// Non-empty input must parse using the same format as the matching CLI flag.
fn parse_with_default<T>(input: &str, default: T) -> Result<T>
where
    T: FromStr + Display,
    T::Err: Display,
{
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(default);
    }
    trimmed
        .parse::<T>()
        .map_err(|error| anyhow!("invalid value {input:?}: {error}"))
}

/// Prompt for a value, accepting the shown default on empty input.
fn prompt_with_default<T>(prompt: &str, default: T) -> Result<T>
where
    T: FromStr + Display,
    T::Err: Display,
{
    parse_with_default(&read_line(prompt)?, default)
}

/// Parse a network selection: empty or "1" → testnet, "2" → mainnet.
fn parse_network(input: &str) -> Result<Network> {
    match input.trim() {
        "" | "1" => Ok(Network::Testnet),
        "2" => Ok(Network::Mainnet),
        _ => bail!("invalid network choice: {input:?} (choose 1 or 2)"),
    }
}

/// Run the fully interactive deployment flow. Used when `cnear-deploy` is
/// invoked without any flags; mirrors the original `deploy.sh` prompts. Every
/// value is parsed and validated by the same typed machinery as the matching
/// CLI flag, and the defaults match the flag defaults.
pub fn run() -> Result<Cli> {
    println!("=== Deployment Configuration ===");
    println!("Select network:");
    println!("  1) testnet (default)");
    println!("  2) mainnet");
    let network = parse_network(&read_line("Enter choice [1]: ")?)?;

    let signer = select_signer(network, None, None)?;
    let signer_id = signer.account_id;

    let controller_id = prompt_with_default(
        &format!("Controller account ID [controller.{signer_id}]: "),
        format!("controller.{signer_id}"),
    )?;
    let token_id = prompt_with_default(
        &format!("Token account ID [token.{signer_id}]: "),
        format!("token.{signer_id}"),
    )?;
    let token_name = prompt_with_default(
        "Token name [Controlled NEAR]: ",
        "Controlled NEAR".to_string(),
    )?;
    let token_symbol = prompt_with_default("Token symbol [cNEAR]: ", "cNEAR".to_string())?;
    let token_decimals = prompt_with_default("Token decimals [24]: ", 24_u8)?;
    let total_supply = prompt_with_default(
        "Total supply [0.000000001 NEAR]: ",
        NearToken::from_yoctonear(1_000_000_000_000_000),
    )?;
    let initial_price = prompt_with_default("Initial price [1 NEAR]: ", NearToken::from_near(1))?;
    let initial_balance = prompt_with_default(
        "Initial balance for new accounts [10 NEAR]: ",
        NearToken::from_near(10),
    )?;

    println!();
    println!("=== Deployment Summary ===");
    println!("network:         {network:?}");
    println!("signer:          {signer_id}");
    println!("controller:      {controller_id}");
    println!("token:           {token_id}");
    println!("token name:      {token_name}");
    println!("token symbol:    {token_symbol}");
    println!("token decimals:  {token_decimals}");
    println!(
        "total supply:    {} ({} yoctoNEAR)",
        total_supply,
        total_supply.as_yoctonear()
    );
    println!(
        "initial price:   {} ({} yoctoNEAR)",
        initial_price,
        initial_price.as_yoctonear()
    );
    println!(
        "initial balance: {} ({} yoctoNEAR)",
        initial_balance,
        initial_balance.as_yoctonear()
    );

    Ok(Cli {
        network,
        signer_id: Some(signer_id.to_string()),
        credentials: None,
        controller_wasm: PathBuf::from(DEFAULT_CONTROLLER_WASM),
        token_wasm: PathBuf::from(DEFAULT_TOKEN_WASM),
        controller_id: Some(controller_id),
        token_id: Some(token_id),
        token_name,
        token_symbol,
        token_decimals,
        total_supply,
        initial_price,
        initial_balance,
        redeploy: false,
        dry_run: false,
        test_mode: false,
        yes: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_choice_parses() {
        assert_eq!(parse_network("").unwrap(), Network::Testnet);
        assert_eq!(parse_network("1").unwrap(), Network::Testnet);
        assert_eq!(parse_network("2").unwrap(), Network::Mainnet);
        assert!(parse_network("3").is_err());
        assert!(parse_network("testnet").is_err());
    }

    #[test]
    fn parse_with_default_falls_back_on_blank_input() {
        assert_eq!(parse_with_default("", 24_u8).unwrap(), 24);
        assert_eq!(
            parse_with_default("  ", "cNEAR".to_string()).unwrap(),
            "cNEAR"
        );
    }

    #[test]
    fn parse_with_default_uses_near_token_units() {
        assert_eq!(
            parse_with_default("10 NEAR", NearToken::from_near(1)).unwrap(),
            NearToken::from_near(10)
        );
        assert_eq!(
            parse_with_default("0.5 N", NearToken::from_near(1)).unwrap(),
            NearToken::from_millinear(500)
        );
        assert!(parse_with_default("not an amount", NearToken::from_near(1)).is_err());
    }
}
