use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub wallets: HashMap<String, WalletConfig>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WalletConfig {
    pub wallet: String,
    pub network: Network,
    pub ext_descriptor: String,
    pub int_descriptor: Option<String>,
    pub client_type: ClientType,
    pub server_url: String,
    pub journal_file: Option<PathBuf>,
    pub state_file: Option<PathBuf>,
    /// hledger account name (default: assets:bitcoin:<wallet>)
    pub account: Option<String>,
}

impl WalletConfig {
    pub fn int_descriptor(&self) -> String {
        self.int_descriptor
            .clone()
            .unwrap_or_else(|| derive_change_descriptor(&self.ext_descriptor))
    }

    pub fn state_path(&self) -> PathBuf {
        self.state_file.clone().unwrap_or_else(|| {
            dirs::data_local_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("hledger-btc")
                .join(format!("{}.db", self.wallet))
        })
    }

    pub fn account_name(&self) -> String {
        self.account
            .clone()
            .unwrap_or_else(|| format!("assets:bitcoin:{}", self.wallet))
    }
}

/// Derives a change (internal) descriptor by replacing the last /0/* with /1/*.
fn derive_change_descriptor(ext: &str) -> String {
    match ext.rfind("/0/*") {
        Some(pos) => format!("{}/1/*{}", &ext[..pos], &ext[pos + 4..]),
        None => ext.to_string(),
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    Bitcoin,
    Testnet,
    Signet,
    Regtest,
}

impl From<Network> for bdk_wallet::bitcoin::Network {
    fn from(n: Network) -> Self {
        match n {
            Network::Bitcoin => bdk_wallet::bitcoin::Network::Bitcoin,
            Network::Testnet => bdk_wallet::bitcoin::Network::Testnet,
            Network::Signet => bdk_wallet::bitcoin::Network::Signet,
            Network::Regtest => bdk_wallet::bitcoin::Network::Regtest,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientType {
    Electrum,
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hledger-btc")
        .join("wallets.toml")
}

pub fn load(path: &PathBuf) -> anyhow::Result<Config> {
    anyhow::ensure!(path.exists(), "config not found at {path:?}");
    check_permissions(path)?;
    let raw = std::fs::read_to_string(path)?;
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
