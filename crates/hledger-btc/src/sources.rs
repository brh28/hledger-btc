use std::collections::HashSet;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use anyhow::Result;
use serde::Deserialize;

use hledger_btc_core::config::Config;
use hledger_btc_core::journal::{self, Account};
use hledger_btc_core::scan::ElectrumSource;
use hledger_btc_core::source::{self, Source};

#[derive(Deserialize)]
pub struct SourceEntry {
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(flatten)]
    pub config: toml::Table,
}

impl SourceEntry {
    pub fn account_name(&self, base: &Account) -> Account {
        self.type_.split('.').fold(base.clone(), |acc, seg| acc.append(seg))
    }
}

#[derive(Deserialize)]
pub struct FullConfig {
    #[serde(flatten)]
    pub core: Config,
    #[serde(default)]
    pub sources: Vec<SourceEntry>,
}

pub fn load_full(path: &PathBuf) -> Result<FullConfig> {
    anyhow::ensure!(path.exists(), "config not found at {path:?}");
    let raw = std::fs::read_to_string(path)?;
    toml::from_str(&raw).map_err(Into::into)
}

pub fn build<'a>(cfg: &'a Config, entries: &[SourceEntry]) -> Result<Vec<Box<dyn Source + 'a>>> {
    let mut sources: Vec<Box<dyn Source + 'a>> = Vec::new();
    if !cfg.wallets.is_empty() {
        sources.push(Box::new(ElectrumSource { cfg }));
    }
    let mut seen_types = HashSet::from(["electrum".to_string()]);
    for entry in entries {
        anyhow::ensure!(
            seen_types.insert(entry.type_.clone()),
            "duplicate source type '{}'", entry.type_
        );
        let account = entry.account_name(&cfg.base_account);
        sources.push(build_one(entry, account)?);
    }
    Ok(sources)
}

fn build_one(entry: &SourceEntry, account: Account) -> Result<Box<dyn Source + 'static>> {
    match entry.type_.as_str() {
        "lightning.phoenix" => hledger_btc_lightning::build(&entry.config, account),
        #[cfg(feature = "coinbase")]
        "coinbase" => hledger_btc_coinbase::build(&entry.config, account),
        other => anyhow::bail!("unknown source type '{other}'"),
    }
}

pub fn run_pipeline(
    sources: &[Box<dyn Source + '_>],
    journal_path: &Path,
    output_path: &Path,
) -> Result<()> {
    let collected = source::collect(sources);
    for (name, err) in &collected.failures {
        eprintln!("warning: source '{name}' failed: {err:#}");
    }
    if !sources.is_empty() && collected.failures.len() == sources.len() {
        anyhow::bail!("all sources failed");
    }

    let merged = journal::merge_entries(collected.entries);

    let known = if journal_path.exists() {
        let out = std::process::Command::new("hledger")
            .args(["-f", journal_path.to_str().unwrap(), "print"])
            .output()?;
        anyhow::ensure!(
            out.status.success(),
            "hledger print failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        source::KnownKeys::parse(&out.stdout[..])?
    } else {
        source::KnownKeys::default()
    };

    let plan = source::plan(merged, &known);
    for notice in &plan.notices {
        println!("notice: {notice}");
    }
    if !plan.new_entries.is_empty() {
        let file = std::fs::OpenOptions::new().create(true).append(true).open(output_path)?;
        journal::write_entries(&plan.new_entries, &mut BufWriter::new(file))?;
    }
    println!(
        "{} new entries; {} already recorded; {} notices",
        plan.new_entries.len(),
        plan.already_recorded,
        plan.notices.len()
    );
    Ok(())
}
