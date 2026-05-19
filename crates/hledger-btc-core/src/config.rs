use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub wallet: WalletConfig,
    pub sync: SyncConfig,
    pub journal: JournalConfig,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WalletConfig {
    pub descriptor: SecretSource,
    pub network: Network, 
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SyncConfig {
    pub backend: Backend, 
    pub url: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct JournalConfig {
    pub file: PathBuf,
    pub account: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    Bitcoin,
    Testnet,
    Signet,
    Regtest,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    // Esplora,
    Electrum,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "source")]
pub enum SecretSource {
    #[serde(rename = "literal")]
    Literal { value: String },
    #[serde(rename = "env")]
    Env { key: String },
    #[serde(rename = "keyring")]
    Keyring { service: String, key: String },
    #[serde(rename = "bitwarden_cli")]
    BitwardenCli { item_id: String },
}

impl SecretSource {
    pub fn resolve(&self) -> anyhow::Result<String> {
        match self {
            Self::Literal { value } => Ok(value.clone()),
            Self::Env { key } => std::env::var(key).map_err(|_| {
                anyhow::anyhow!("env var `{key}` not set")
            }),
            Self::Keyring { service, key } => {
                let entry = keyring::Entry::new(service, key)?;
                entry.get_password().map_err(Into::into)
            }
            Self::BitwardenCli { item_id } => {
                let out = std::process::Command::new("bw")
                    .args(["get", "password", item_id])
                    .output()?;
                anyhow::ensure!(out.status.success(), "bw exited non-zero");
                Ok(String::from_utf8(out.stdout)?.trim().to_string())
            }
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hledger-btc")
        .join("config.toml")
}

pub fn load() -> anyhow::Result<Config> {
    let path = config_path();
    anyhow::ensure!(path.exists(), "config not found at {path:?} — run `hledger-btc init`");
    check_permissions(&path)?;
    let raw = std::fs::read_to_string(&path)?;
    toml::from_str(&raw).map_err(Into::into)
}

#[cfg(unix)]
fn check_permissions(path: &std::path::Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode();
    if mode & 0o077 != 0 {
        tracing::warn!(
            "config file {path:?} is group/world readable (mode {mode:o}). Run `chmod 600`."
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_permissions(_path: &std::path::Path) -> anyhow::Result<()> {
    Ok(())
}
