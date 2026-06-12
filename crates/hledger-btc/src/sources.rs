use std::collections::HashSet;
use std::io::BufWriter;
use std::path::Path;
use anyhow::Result;

use hledger_btc_core::config::{Config, SourceConfig};
use hledger_btc_core::journal;
use hledger_btc_core::scan::ElectrumSource;
use hledger_btc_core::source::{self, FileSource, Source};
use hledger_btc_lightning::phoenix;

/// Builds one Source per configured input: the built-in electrum scanner plus
/// everything declared in [[sources]]. This match is the registry of known
/// source types; new types (and their feature gates) are added here.
pub fn build(cfg: &Config) -> Result<Vec<Box<dyn Source + '_>>> {
    let mut sources: Vec<Box<dyn Source + '_>> = Vec::new();
    if !cfg.wallets.is_empty() {
        sources.push(Box::new(ElectrumSource { cfg }));
    }
    let mut names = HashSet::from(["electrum".to_string()]);
    for sc in &cfg.sources {
        anyhow::ensure!(names.insert(sc.name.clone()), "duplicate source name '{}'", sc.name);
        sources.push(build_one(cfg, sc)?);
    }
    Ok(sources)
}

fn build_one(cfg: &Config, sc: &SourceConfig) -> Result<Box<dyn Source + 'static>> {
    match sc.type_.as_str() {
        "lightning.phoenix" => {
            let account = sc.account_name(&cfg.base_account);
            Ok(Box::new(FileSource::new(
                sc.name.clone(),
                sc.path.clone(),
                move |file| phoenix::parse(file, account.as_str()),
            )))
        }
        other => anyhow::bail!("unknown source type '{other}' for source '{}'", sc.name),
    }
}

/// The scan pipeline: collect from all sources, merge across them, dedup
/// against the journal, append what's new, and report what was skipped.
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
