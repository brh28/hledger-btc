mod phoenix;

use std::path::PathBuf;
use anyhow::{Context, Result};
use serde::Deserialize;

use hledger_btc_core::journal::{Account, JournalEntry};
use hledger_btc_core::source::Source;

pub struct PhoenixSource {
    path: PathBuf,
    account: Account,
}

impl PhoenixSource {
    pub fn new(path: PathBuf, account: Account) -> Self {
        Self { path, account }
    }
}

impl Source for PhoenixSource {
    fn name(&self) -> &str {
        "lightning.phoenix"
    }

    fn entries(&self) -> Result<Vec<JournalEntry>> {
        let file = std::fs::File::open(&self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        phoenix::parse(file, self.account.as_str())
    }
}

#[derive(Deserialize)]
struct PhoenixConfig {
    path: PathBuf,
}

pub fn build(config: &toml::Table, account: Account) -> Result<Box<dyn Source + 'static>> {
    let cfg: PhoenixConfig = toml::Value::Table(config.clone())
        .try_into()
        .context("invalid lightning.phoenix config")?;
    Ok(Box::new(PhoenixSource::new(cfg.path, account)))
}
