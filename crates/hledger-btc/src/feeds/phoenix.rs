mod parse;

use std::path::PathBuf;
use anyhow::{Context, Result};
use serde::Deserialize;

use hledger_btc_core::journal::Account;
use hledger_btc_core::source::{FeedEntry, Source};

pub struct PhoenixFeed {
    path: PathBuf,
    account: Account,
}

impl PhoenixFeed {
    pub fn new(path: PathBuf, account: Account) -> Self {
        Self { path, account }
    }
}

impl Source for PhoenixFeed {
    fn name(&self) -> &str {
        "phoenix"
    }

    fn entries(&self) -> Result<Vec<FeedEntry>> {
        let file = std::fs::File::open(&self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        parse::parse(file, self.account.as_str())
    }
}

#[derive(Deserialize)]
struct PhoenixConfig {
    path: PathBuf,
}

pub fn build(config: &toml::Table, account: Account) -> Result<Box<dyn Source + 'static>> {
    let cfg: PhoenixConfig = toml::Value::Table(config.clone())
        .try_into()
        .context("invalid phoenix config")?;
    Ok(Box::new(PhoenixFeed::new(cfg.path, account)))
}
