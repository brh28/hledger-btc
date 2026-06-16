use std::io::BufWriter;
use std::path::Path;
use anyhow::Result;

use hledger_btc_core::{journal, reconcile, source};
use hledger_btc_core::source::Source;

pub fn run_pipeline(
    sources: &[Box<dyn Source + '_>],
    journal_path: &Path,
    output_path: &Path,
    do_reconcile: bool,
) -> Result<()> {
    let collected = source::collect(sources);
    for (name, err) in &collected.failures {
        eprintln!("warning: feed '{name}' failed: {err:#}");
    }
    if !sources.is_empty() && collected.failures.len() == sources.len() {
        anyhow::bail!("all feeds failed");
    }

    let merged = journal::merge_entries(collected.entries);

    let journal_content = if journal_path.exists() {
        Some(std::fs::read_to_string(journal_path)?)
    } else {
        None
    };

    let known = if journal_content.is_some() {
        let out = std::process::Command::new("hledger")
            .args(["-f", journal_path.to_str().unwrap(), "print"])
            .output()?;
        anyhow::ensure!(
            out.status.success(),
            "hledger print failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        source::KnownKeys::parse(&out.stdout[..], &collected.provider_keys)?
    } else {
        source::KnownKeys::default()
    };

    let plan = source::plan(merged, &known, &collected.provider_keys);

    let mut reconciled = 0usize;
    let mut conflicts = 0usize;

    if do_reconcile && !plan.notices.is_empty() {
        if let Some(content) = journal_content {
            let (updated, result) = reconcile::reconcile(&content, &plan.notices);
            reconciled = result.applied.len();
            conflicts = result.conflicts.len();
            if reconciled > 0 {
                std::fs::write(journal_path, updated)?;
            }
            for n in &result.conflicts {
                println!(
                    "conflict: {}:{} has novel source(s) [{}] but no unknown leg — edit manually",
                    n.key, n.value, n.novel_sources.join(", ")
                );
            }
        }
    } else {
        for notice in &plan.notices {
            println!("notice: {notice}");
        }
    }

    if !plan.new_entries.is_empty() {
        let file = std::fs::OpenOptions::new().create(true).append(true).open(output_path)?;
        journal::write_entries(&plan.new_entries, &mut BufWriter::new(file))?;
    }

    println!(
        "{} new, {} already recorded, {} reconciled, {} conflicts",
        plan.new_entries.len(),
        plan.already_recorded,
        reconciled,
        conflicts,
    );
    Ok(())
}
