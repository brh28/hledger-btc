use std::path::PathBuf;
use anyhow::Result;
use serde::{Deserialize, Serialize};

use hledger_btc_core::config::{Config, ScanConfig, WalletConfig};
use hledger_btc_core::journal::Account;

use crate::feeds::FeedConfig;

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hledger-btc")
        .join("config.toml")
}

#[derive(Deserialize, Serialize)]
pub struct AppConfig {
    #[serde(default = "default_base_account")]
    pub base_account: Account,
    pub scan: ScanConfig,
    #[serde(default)]
    pub wallets: Vec<WalletConfig>,
    #[serde(default)]
    pub feeds: Vec<FeedConfig>,
}

fn default_base_account() -> Account {
    Account::new("assets")
}

impl AppConfig {
    pub fn to_core(&self) -> Config {
        Config {
            base_account: self.base_account.clone(),
            scan: self.scan.clone(),
            wallets: self.wallets.clone(),
        }
    }

    pub fn find_feed(&self, provider: &str, name: Option<&str>) -> Result<&FeedConfig> {
        match name {
            Some(n) => self.feeds.iter()
                .find(|f| f.name == n && f.provider == provider)
                .ok_or_else(|| anyhow::anyhow!("no '{provider}' feed named '{n}' in config")),
            None => self.feeds.iter()
                .find(|f| f.provider == provider)
                .ok_or_else(|| anyhow::anyhow!(
                    "no '{provider}' feed configured; use --path to import without a config entry"
                )),
        }
    }

    pub fn write(&self, path: &PathBuf) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?).map_err(Into::into)
    }
}

pub fn load(path: &PathBuf) -> Result<AppConfig> {
    anyhow::ensure!(path.exists(), "config not found at {path:?}");
    let raw = std::fs::read_to_string(path)?;
    toml::from_str(&raw).map_err(Into::into)
}
