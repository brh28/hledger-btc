use anyhow::Result;
use serde::{Deserialize, Serialize};

use hledger_btc_core::config::Config;
use hledger_btc_core::journal::Account;
use hledger_btc_core::source::Source;

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
        "phoenix" => hledger_btc_phoenix::build(&entry.config, entry.account_name(&cfg.base_account)),
        #[cfg(feature = "coinbase")]
        "coinbase" => hledger_btc_coinbase::build(&entry.config, entry.account_name(&cfg.base_account)),
        other => anyhow::bail!("unknown feed provider '{other}'"),
    }
}
