use crate::cli::{Cli, Network};
use crate::credentials::{parse_account_id, select_signer, Credentials};
use anyhow::{bail, Context, Result};
use near_primitives::types::AccountId;
use near_token::NearToken;
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
    pub total_supply: NearToken,
    pub initial_price: NearToken,
    pub initial_balance: NearToken,
    pub redeploy: bool,
    pub dry_run: bool,
    pub test_mode: bool,
    pub yes: bool,
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
    if cli.total_supply.is_zero() {
        bail!("total supply must be greater than zero");
    }
    if cli.initial_price.is_zero() {
        bail!("initial price must be greater than zero");
    }
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
        initial_balance: cli.initial_balance,
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
}
