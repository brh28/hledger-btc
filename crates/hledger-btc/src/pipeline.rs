use std::io::BufWriter;
use std::path::Path;
use anyhow::Result;

use hledger_btc_core::{journal, source};
use hledger_btc_core::source::Source;

pub fn run_pipeline(
    sources: &[Box<dyn Source + '_>],
    journal_path: &Path,
    output_path: &Path,
) -> Result<()> {
    let collected = source::collect(sources);
    for (name, err) in &collected.failures {
        eprintln!("warning: feed '{name}' failed: {err:#}");
    }
    if !sources.is_empty() && collected.failures.len() == sources.len() {
        anyhow::bail!("all feeds failed");
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
