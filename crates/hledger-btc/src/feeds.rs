use anyhow::Result;
use serde::{Deserialize, Serialize};

use hledger_btc_core::config::Config;
use hledger_btc_core::journal::Account;
use hledger_btc_core::source::Source;

#[cfg(feature = "phoenix")]
pub mod phoenix;
#[cfg(feature = "coinbase")]
pub mod coinbase;
#[cfg(feature = "cashapp")]
pub mod cashapp;
#[cfg(feature = "river")]
pub mod river;

#[derive(Deserialize, Serialize)]
pub struct FeedConfig {
    pub name: String,
    pub provider: String,
    #[serde(flatten)]
    pub config: toml::Table,
}

impl FeedConfig {
    pub fn account_name(&self, base: &Account) -> Account {
        base.append(&self.name)
    }
}

pub fn build_feed(cfg: &Config, entry: &FeedConfig) -> Result<Box<dyn Source + 'static>> {
    match entry.provider.as_str() {
        #[cfg(feature = "phoenix")]
        "phoenix" => phoenix::build(&entry.config, entry.account_name(&cfg.base_account)),
        #[cfg(feature = "coinbase")]
        "coinbase" => coinbase::build(&entry.config, entry.account_name(&cfg.base_account)),
        #[cfg(feature = "cashapp")]
        "cashapp" => cashapp::build(&entry.config, entry.account_name(&cfg.base_account)),
        #[cfg(feature = "river")]
        "river" => river::build(&entry.config, entry.account_name(&cfg.base_account)),
        #[cfg(not(feature = "phoenix"))]
        "phoenix" => anyhow::bail!("feed provider 'phoenix' is not enabled; reinstall with `--features phoenix`"),
        #[cfg(not(feature = "coinbase"))]
        "coinbase" => anyhow::bail!("feed provider 'coinbase' is not enabled; reinstall with `--features coinbase`"),
        #[cfg(not(feature = "cashapp"))]
        "cashapp" => anyhow::bail!("feed provider 'cashapp' is not enabled; reinstall with `--features cashapp`"),
        #[cfg(not(feature = "river"))]
        "river" => anyhow::bail!("feed provider 'river' is not enabled; reinstall with `--features river`"),
        other => anyhow::bail!("unknown feed provider '{other}'"),
    }
}
