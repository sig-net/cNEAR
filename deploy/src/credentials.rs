use crate::cli::Network;
use anyhow::{anyhow, bail, Context, Result};
use near_crypto::{PublicKey, SecretKey};
use near_primitives::types::AccountId;
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize)]
struct CredentialFile {
    account_id: AccountId,
    public_key: PublicKey,
    private_key: SecretKey,
    #[serde(default)]
    #[serde(rename = "connection_info")]
    _connection_info: Option<Value>,
}

#[derive(Clone, Debug)]
pub struct Credentials {
    pub account_id: AccountId,
    pub secret_key: SecretKey,
}

pub fn credentials_dir(network: Network) -> Result<PathBuf> {
    if let Ok(root) = std::env::var("NEAR_CREDENTIALS") {
        return Ok(PathBuf::from(root).join(network.subdir()));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
    Ok(PathBuf::from(home)
        .join(".near-credentials")
        .join(network.subdir()))
}

pub fn credential_path(
    network: Network,
    signer_id: Option<&AccountId>,
    explicit: Option<&Path>,
) -> Result<PathBuf> {
    Ok(match explicit {
        Some(path) => path.to_path_buf(),
        None => credentials_dir(network)?.join(format!(
            "{}.json",
            signer_id
                .ok_or_else(|| anyhow!("signer ID is required for implicit credential path"))?
        )),
    })
}

#[cfg(unix)]
fn validate_private_file_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let actual = fs::symlink_metadata(path)?.permissions().mode() & 0o777;
    if actual != mode {
        bail!(
            "credential file {} must have permissions {mode:03o}, found {actual:03o}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_file_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

pub fn load_credentials(path: &Path, expected_account: Option<&AccountId>) -> Result<Credentials> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("credential file not found: {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("credential path must not be a symlink: {}", path.display());
    }
    if !metadata.is_file() {
        bail!("credential path is not a regular file: {}", path.display());
    }
    validate_private_file_mode(path, 0o600)?;
    let raw = fs::read(path)
        .with_context(|| format!("cannot read credential file {}", path.display()))?;
    let file: CredentialFile =
        serde_json::from_slice(&raw).context("invalid NEAR credential JSON")?;
    if file.public_key != file.private_key.public_key() {
        bail!("credential public_key does not match private_key");
    }
    if let Some(expected) = expected_account {
        if &file.account_id != expected {
            bail!("credential account_id does not match requested signer");
        }
    }
    Ok(Credentials {
        account_id: file.account_id,
        secret_key: file.private_key,
    })
}

fn list_credentials(network: Network) -> Result<Vec<PathBuf>> {
    let dir = credentials_dir(network)?;
    let mut paths = fs::read_dir(&dir)
        .with_context(|| format!("credentials directory not found: {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|item| item.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    paths.sort();
    if paths.is_empty() {
        bail!("no JSON credentials found in {}", dir.display());
    }
    Ok(paths)
}

/// Resolve the signer credentials, prompting for a selection when the caller
/// provided neither an explicit credentials path nor a signer account ID.
pub fn select_signer(
    network: Network,
    signer_id: Option<&str>,
    credentials: Option<&Path>,
) -> Result<Credentials> {
    let expected = signer_id
        .map(|value| parse_account_id(value, "signer"))
        .transpose()?;
    let path = credential_path(network, expected.as_ref(), credentials)?;
    if credentials.is_some() || expected.is_some() {
        return load_credentials(&path, expected.as_ref());
    }
    let paths = list_credentials(network)?;
    for (index, path) in paths.iter().enumerate() {
        println!(
            "  {}) {}",
            index + 1,
            path.file_stem().unwrap_or_default().to_string_lossy()
        );
    }
    print!("Select signer [1]: ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let selected = input.trim().parse::<usize>().unwrap_or(1);
    let selected = paths
        .get(selected.saturating_sub(1))
        .ok_or_else(|| anyhow!("invalid signer selection"))?;
    load_credentials(selected, None)
}

pub fn parse_account_id(value: &str, field: &str) -> Result<AccountId> {
    value
        .parse()
        .with_context(|| format!("invalid {field} account ID: {value:?}"))
}

pub fn persist_generated_credentials(
    path: &Path,
    account_id: AccountId,
    secret_key: SecretKey,
) -> Result<Credentials> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
    }
    let public_key = secret_key.public_key();
    let contents = serde_json::to_vec_pretty(&json!({
        "account_id": account_id,
        "public_key": public_key,
        "private_key": secret_key,
    }))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(path)
        .with_context(|| format!("cannot create credential file {}", path.display()))?;
    file.write_all(&contents)?;
    file.sync_all()?;
    Ok(Credentials {
        account_id,
        secret_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::{Builder, NamedTempFile};

    #[test]
    fn account_ids_are_strictly_validated() {
        assert!(parse_account_id("alice.testnet", "signer").is_ok());
        assert!(parse_account_id("ALICE.testnet", "signer").is_err());
        assert!(parse_account_id("not an account", "signer").is_err());
    }

    #[test]
    fn credentials_require_matching_keys_and_secure_permissions() {
        let secret_key = SecretKey::from_seed(near_crypto::KeyType::ED25519, "credential-test");
        let account_id: AccountId = "alice.testnet".parse().unwrap();
        let mut file = Builder::new().suffix(".json").tempfile().unwrap();
        let payload = json!({
            "account_id": account_id,
            "public_key": secret_key.public_key(),
            "private_key": secret_key,
        });
        file.write_all(serde_json::to_string(&payload).unwrap().as_bytes())
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        let credentials = load_credentials(file.path(), Some(&account_id)).unwrap();
        assert_eq!(credentials.account_id, account_id);

        let other_key = SecretKey::from_seed(near_crypto::KeyType::ED25519, "credential-mismatch");
        let mismatched_payload = json!({
            "account_id": account_id,
            "public_key": other_key.public_key(),
            "private_key": secret_key,
        });
        fs::write(
            file.path(),
            serde_json::to_vec(&mismatched_payload).unwrap(),
        )
        .unwrap();
        assert!(load_credentials(file.path(), Some(&account_id)).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.as_file()
                .set_permissions(fs::Permissions::from_mode(0o644))
                .unwrap();
            assert!(load_credentials(file.path(), Some(&account_id)).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn credentials_reject_symlinks() {
        use std::os::unix::fs::symlink;
        let secret_key = SecretKey::from_seed(near_crypto::KeyType::ED25519, "symlink-test");
        let account_id: AccountId = "alice.testnet".parse().unwrap();
        let target = NamedTempFile::new().unwrap();
        target.as_file().set_len(0).unwrap();
        let payload = json!({
            "account_id": account_id,
            "public_key": secret_key.public_key(),
            "private_key": secret_key,
        });
        fs::write(target.path(), serde_json::to_vec(&payload).unwrap()).unwrap();
        use std::os::unix::fs::PermissionsExt;
        target
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let link = directory.path().join("credential.json");
        symlink(target.path(), &link).unwrap();
        assert!(load_credentials(&link, Some(&account_id)).is_err());
    }
}
