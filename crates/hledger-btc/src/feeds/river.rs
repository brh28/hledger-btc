mod parse;

use std::path::PathBuf;
use anyhow::{Context, Result};
use serde::Deserialize;

use hledger_btc_core::journal::Account;
use hledger_btc_core::source::{FeedEntry, Source};

pub struct RiverFeed {
    path: PathBuf,
    account: Account,
}

impl RiverFeed {
    pub fn new(path: PathBuf, account: Account) -> Self {
        Self { path, account }
    }
}

impl Source for RiverFeed {
    fn name(&self) -> &str {
        "river"
    }

    fn entries(&self) -> Result<Vec<FeedEntry>> {
        let file = std::fs::File::open(&self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        parse::parse(file, self.account.as_str())
    }
}

#[derive(Deserialize)]
struct RiverConfig {
    path: PathBuf,
}

pub fn build(config: &toml::Table, account: Account) -> Result<Box<dyn Source + 'static>> {
    let cfg: RiverConfig = toml::Value::Table(config.clone())
        .try_into()
        .context("invalid river config")?;
    Ok(Box::new(RiverFeed::new(cfg.path, account)))
}
