use crate::cli::{Cli, Network, YOCTO_PER_NEAR};
use crate::credentials::{parse_account_id, select_signer, Credentials};
use anyhow::{anyhow, bail, Context, Result};
use near_primitives::types::AccountId;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

#[derive(Clone, Debug)]
pub struct WasmArtifact {
    pub bytes: Vec<u8>,
    pub sha256: String,
}

#[derive(Clone, Debug)]
pub struct DeploymentConfig {
    pub network: Network,
    pub signer: Credentials,
    pub controller_id: AccountId,
    pub token_id: AccountId,
    pub controller_wasm: WasmArtifact,
    pub token_wasm: WasmArtifact,
    pub token_name: String,
    pub token_symbol: String,
    pub token_decimals: u8,
    pub total_supply: u128,
    pub initial_price: u128,
    pub initial_balance: u128,
    pub redeploy: bool,
    pub dry_run: bool,
    pub test_mode: bool,
    pub yes: bool,
}

pub fn parse_near_amount(value: &str) -> Result<u128> {
    let value = value.trim();
    if value.is_empty() || value.starts_with('-') || value.starts_with('+') {
        bail!("initial balance must be a non-negative decimal amount in NEAR");
    }
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty() || !whole.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("initial balance must be a non-negative decimal amount in NEAR");
    }
    if fraction.len() > 24 || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("initial balance must contain at most 24 fractional digits");
    }
    let whole = whole
        .parse::<u128>()
        .context("initial balance is too large")?;
    let fraction_value = if fraction.is_empty() {
        0
    } else {
        let padded = format!("{fraction:0<24}");
        padded
            .parse::<u128>()
            .context("invalid fractional NEAR amount")?
    };
    whole
        .checked_mul(YOCTO_PER_NEAR)
        .and_then(|amount| amount.checked_add(fraction_value))
        .ok_or_else(|| anyhow!("initial balance is too large"))
}

fn read_wasm(path: &Path) -> Result<WasmArtifact> {
    let metadata =
        fs::metadata(path).with_context(|| format!("cannot stat WASM {}", path.display()))?;
    if !metadata.is_file() {
        bail!("WASM path is not a regular file: {}", path.display());
    }
    if metadata.len() < 8 {
        bail!("WASM file is too small: {}", path.display());
    }
    let bytes = fs::read(path).with_context(|| format!("cannot read WASM {}", path.display()))?;
    if bytes.get(0..4) != Some(b"\0asm") {
        bail!("file is not a WebAssembly binary: {}", path.display());
    }
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(WasmArtifact {
        bytes,
        sha256: hex::encode(hasher.finalize()),
    })
}

pub fn build_config(cli: Cli) -> Result<DeploymentConfig> {
    if cli.token_decimals == 0 {
        bail!("token decimals must be between 1 and 255");
    }
    if cli.token_name.trim().is_empty() || cli.token_symbol.trim().is_empty() {
        bail!("token name and symbol must not be empty");
    }
    let signer = select_signer(
        cli.network,
        cli.signer_id.as_deref(),
        cli.credentials.as_deref(),
    )?;
    let controller_id = match cli.controller_id.as_deref() {
        Some(value) => parse_account_id(value, "controller")?,
        None => parse_account_id(&format!("controller.{}", signer.account_id), "controller")?,
    };
    let token_id = match cli.token_id.as_deref() {
        Some(value) => parse_account_id(value, "token")?,
        None => parse_account_id(&format!("token.{}", signer.account_id), "token")?,
    };
    if cli.total_supply == 0 {
        bail!("total supply must be greater than zero");
    }
    if cli.initial_price == 0 {
        bail!("initial price must be greater than zero");
    }
    let initial_balance = parse_near_amount(&cli.initial_balance)?;
    Ok(DeploymentConfig {
        network: cli.network,
        signer,
        controller_id,
        token_id,
        controller_wasm: read_wasm(&cli.controller_wasm)?,
        token_wasm: read_wasm(&cli.token_wasm)?,
        token_name: cli.token_name,
        token_symbol: cli.token_symbol,
        token_decimals: cli.token_decimals,
        total_supply: cli.total_supply,
        initial_price: cli.initial_price,
        initial_balance,
        redeploy: cli.redeploy,
        dry_run: cli.dry_run,
        test_mode: cli.test_mode,
        yes: cli.yes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn wasm_validation_checks_magic_and_hash() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"\0asm\x01\0\0\0").unwrap();
        let artifact = read_wasm(file.path()).unwrap();
        assert_eq!(artifact.bytes.len(), 8);
        assert_eq!(artifact.sha256.len(), 64);
    }

    #[test]
    fn wasm_validation_rejects_non_wasm() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"not wasm bytes...").unwrap();
        assert!(read_wasm(file.path()).is_err());
    }

    #[test]
    fn numeric_values_reject_invalid_input() {
        assert_eq!(parse_near_amount("1").unwrap(), YOCTO_PER_NEAR);
        assert_eq!(parse_near_amount("0.000000000000000000000001").unwrap(), 1);
        assert!(parse_near_amount("-1").is_err());
    }
}
