mod cashapp;

use std::path::PathBuf;
use anyhow::{Context, Result};
use serde::Deserialize;

use hledger_btc_core::journal::Account;
use hledger_btc_core::source::{FeedEntry, Source};

pub struct CashAppFeed {
    path: PathBuf,
    account: Account,
}

impl CashAppFeed {
    pub fn new(path: PathBuf, account: Account) -> Self {
        Self { path, account }
    }
}

impl Source for CashAppFeed {
    fn name(&self) -> &str {
        "cashapp"
    }

    fn entries(&self) -> Result<Vec<FeedEntry>> {
        let file = std::fs::File::open(&self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        cashapp::parse(file, self.account.as_str())
    }
}

#[derive(Deserialize)]
struct CashAppConfig {
    path: PathBuf,
}

pub fn build(config: &toml::Table, account: Account) -> Result<Box<dyn Source + 'static>> {
    let cfg: CashAppConfig = toml::Value::Table(config.clone())
        .try_into()
        .context("invalid cashapp config")?;
    Ok(Box::new(CashAppFeed::new(cfg.path, account)))
}
