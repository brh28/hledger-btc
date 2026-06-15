use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use anyhow::{Context, Result};

use crate::journal::{JournalEntry, DEDUP_KEYS};

/// Tag stamped on every entry recording which source produced it.
pub const SOURCE_TAG: &str = "source";

/// A data source that produces journal entries: the electrum scanner,
/// a lightning wallet export, an exchange fetcher, etc.
pub trait Source {
    fn name(&self) -> &str;
    fn entries(&self) -> Result<Vec<JournalEntry>>;
}

/// A file-backed source: opens its path and hands the file to a
/// format-specific parser, so format crates only need to expose a parse
/// function.
pub struct FileSource<F> {
    name: String,
    path: PathBuf,
    parse: F,
}

impl<F> FileSource<F>
where
    F: Fn(std::fs::File) -> Result<Vec<JournalEntry>>,
{
    pub fn new(name: impl Into<String>, path: PathBuf, parse: F) -> Self {
        FileSource { name: name.into(), path, parse }
    }
}

impl<F> Source for FileSource<F>
where
    F: Fn(std::fs::File) -> Result<Vec<JournalEntry>>,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn entries(&self) -> Result<Vec<JournalEntry>> {
        let file = std::fs::File::open(&self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        (self.parse)(file)
    }
}

pub struct Collected {
    pub entries: Vec<JournalEntry>,
    pub failures: Vec<(String, anyhow::Error)>,
}

/// Collects entries from all sources, stamping each entry with `source:<name>`.
/// A failing source is reported, not fatal, so one unreachable source doesn't
/// stop the others from being recorded.
pub fn collect<'a>(sources: &[Box<dyn Source + 'a>]) -> Collected {
    let mut entries = Vec::new();
    let mut failures = Vec::new();
    for source in sources {
        match source.entries() {
            Ok(mut batch) => {
                for entry in &mut batch {
                    entry.tags.push(SOURCE_TAG, source.name());
                }
                entries.extend(batch);
            }
            Err(err) => failures.push((source.name().to_string(), err)),
        }
    }
    Collected { entries, failures }
}

/// Dedup-key values already present in the journal, and which sources are
/// stamped on each. Parsed from transaction header lines of `hledger print`
/// output. An empty source set means the entry predates source stamping.
#[derive(Debug, Default)]
pub struct KnownKeys(HashMap<(String, String), HashSet<String>>);

impl KnownKeys {
    pub fn parse(reader: impl Read) -> Result<Self> {
        let mut map: HashMap<(String, String), HashSet<String>> = HashMap::new();
        for line in BufReader::new(reader).lines() {
            let line = line?;
            if line.starts_with(' ') || line.starts_with('\t') || line.is_empty() {
                continue;
            }
            let sources = extract_all(&line, SOURCE_TAG);
            for key in DEDUP_KEYS {
                if let Some(value) = extract_first(&line, key) {
                    map.entry((key.to_string(), value))
                        .or_default()
                        .extend(sources.iter().cloned());
                }
            }
        }
        Ok(KnownKeys(map))
    }
}

#[derive(Clone)]
pub struct Notice {
    pub key: String,
    pub value: String,
    pub novel_sources: Vec<String>,
    pub recorded_sources: Vec<String>,
    /// The full incoming entry (post-merge) carried for Phase 2 reconcile.
    pub entry: JournalEntry,
}

impl fmt::Display for Notice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "source '{}' has data for {}:{} already recorded by {}",
            self.novel_sources.join(", "),
            self.key,
            self.value,
            self.recorded_sources.join(", ")
        )
    }
}

pub struct ScanPlan {
    pub new_entries: Vec<JournalEntry>,
    /// Entries skipped because a dedup key is already in the journal
    /// (includes the ones that produced notices).
    pub already_recorded: usize,
    /// Skipped entries where a source had data the journal doesn't credit it
    /// for — candidates for reconciliation.
    pub notices: Vec<Notice>,
}

/// Splits collected entries into new ones to append and known ones to skip.
/// A known entry whose sources are all already stamped on the journal entry
/// (or whose journal entry predates stamping) is skipped silently; a known
/// entry contributing a novel source produces a notice.
pub fn plan(entries: Vec<JournalEntry>, known: &KnownKeys) -> ScanPlan {
    let mut new_entries = Vec::new();
    let mut already_recorded = 0;
    let mut notices = Vec::new();

    for entry in entries {
        let mut matched: Option<(String, String)> = None;
        let mut recorded: HashSet<String> = HashSet::new();
        for key in DEDUP_KEYS {
            if let Some(value) = entry.tags.get(key) {
                if let Some(sources) = known.0.get(&(key.to_string(), value.to_string())) {
                    if matched.is_none() {
                        matched = Some((key.to_string(), value.to_string()));
                    }
                    recorded.extend(sources.iter().cloned());
                }
            }
        }

        let Some((key, value)) = matched else {
            new_entries.push(entry);
            continue;
        };

        already_recorded += 1;
        let novel: Vec<String> = entry.tags.0.iter()
            .filter(|(k, v)| k == SOURCE_TAG && !recorded.contains(v))
            .map(|(_, v)| v.clone())
            .collect();
        if !recorded.is_empty() && !novel.is_empty() {
            let mut recorded_sources: Vec<String> = recorded.into_iter().collect();
            recorded_sources.sort();
            notices.push(Notice { key, value, novel_sources: novel, recorded_sources, entry });
        }
    }

    ScanPlan { new_entries, already_recorded, notices }
}

fn extract_first(line: &str, key: &str) -> Option<String> {
    extract_all(line, key).into_iter().next()
}

fn extract_all(line: &str, key: &str) -> Vec<String> {
    let needle = format!("{key}:");
    let mut values = Vec::new();
    let mut rest = line;
    while let Some(pos) = rest.find(&needle) {
        let start = pos + needle.len();
        let tail = &rest[start..];
        let end = tail.find([',', ' ']).unwrap_or(tail.len());
        if end > 0 {
            values.push(tail[..end].to_string());
        }
        rest = &tail[end..];
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Posting, TagMap};
    use chrono::NaiveDate;

    struct Dummy {
        name: &'static str,
        entries: Vec<JournalEntry>,
    }

    impl Source for Dummy {
        fn name(&self) -> &str { self.name }
        fn entries(&self) -> Result<Vec<JournalEntry>> {
            Ok(self.entries.clone())
        }
    }

    fn entry(tags: TagMap) -> JournalEntry {
        JournalEntry {
            date: NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
            description: "Test".to_string(),
            tags,
            postings: vec![Posting::with_amount("assets:test", 100)],
        }
    }

    fn stamped(tags: TagMap, source: &str) -> JournalEntry {
        let mut e = entry(tags);
        e.tags.push(SOURCE_TAG, source);
        e
    }

    #[test]
    fn collect_stamps_source_tag() {
        let sources: Vec<Box<dyn Source>> = vec![Box::new(Dummy {
            name: "phoenix",
            entries: vec![entry(TagMap::new().add("payment_hash", "ph1"))],
        })];
        let collected = collect(&sources);
        assert!(collected.failures.is_empty());
        assert_eq!(collected.entries[0].tags.get(SOURCE_TAG), Some("phoenix"));
    }

    #[test]
    fn collect_continues_past_failing_source() {
        struct Failing;
        impl Source for Failing {
            fn name(&self) -> &str { "broken" }
            fn entries(&self) -> Result<Vec<JournalEntry>> {
                anyhow::bail!("unreachable")
            }
        }
        let sources: Vec<Box<dyn Source>> = vec![
            Box::new(Failing),
            Box::new(Dummy { name: "ok", entries: vec![entry(TagMap::new().add("txid", "t1"))] }),
        ];
        let collected = collect(&sources);
        assert_eq!(collected.failures.len(), 1);
        assert_eq!(collected.failures[0].0, "broken");
        assert_eq!(collected.entries.len(), 1);
    }

    #[test]
    fn parses_known_keys_with_sources() {
        let journal = "\
2026-05-02 * Incoming BTC  ; txid:abc, source:electrum
    assets:bitcoin:w    4,000 sat  ; vout:1
    income:unknown

2026-05-03 * Zap  ; payment_hash:ph1, source:phoenix, source:electrum
    assets:bitcoin:lightning    100 sat
";
        let known = KnownKeys::parse(journal.as_bytes()).unwrap();
        let abc = known.0.get(&("txid".to_string(), "abc".to_string())).unwrap();
        assert_eq!(abc.len(), 1);
        assert!(abc.contains("electrum"));
        let ph = known.0.get(&("payment_hash".to_string(), "ph1".to_string())).unwrap();
        assert_eq!(ph.len(), 2);
    }

    #[test]
    fn plan_appends_unknown_entry() {
        let known = KnownKeys::default();
        let plan = plan(vec![stamped(TagMap::new().add("txid", "t1"), "electrum")], &known);
        assert_eq!(plan.new_entries.len(), 1);
        assert_eq!(plan.already_recorded, 0);
        assert!(plan.notices.is_empty());
    }

    #[test]
    fn plan_skips_silently_when_source_already_stamped() {
        let mut known = KnownKeys::default();
        known.0.insert(("txid".into(), "t1".into()), HashSet::from(["electrum".to_string()]));
        let plan = plan(vec![stamped(TagMap::new().add("txid", "t1"), "electrum")], &known);
        assert!(plan.new_entries.is_empty());
        assert_eq!(plan.already_recorded, 1);
        assert!(plan.notices.is_empty());
    }

    #[test]
    fn plan_notices_novel_source_for_known_key() {
        let mut known = KnownKeys::default();
        known.0.insert(("txid".into(), "t1".into()), HashSet::from(["electrum".to_string()]));
        let plan = plan(vec![stamped(TagMap::new().add("txid", "t1"), "coinbase")], &known);
        assert!(plan.new_entries.is_empty());
        assert_eq!(plan.already_recorded, 1);
        assert_eq!(plan.notices.len(), 1);
        assert_eq!(plan.notices[0].novel_sources, vec!["coinbase"]);
        assert_eq!(plan.notices[0].recorded_sources, vec!["electrum"]);
    }

    #[test]
    fn plan_skips_legacy_unstamped_entries_silently() {
        let mut known = KnownKeys::default();
        known.0.insert(("txid".into(), "t1".into()), HashSet::new());
        let plan = plan(vec![stamped(TagMap::new().add("txid", "t1"), "phoenix")], &known);
        assert!(plan.new_entries.is_empty());
        assert_eq!(plan.already_recorded, 1);
        assert!(plan.notices.is_empty());
    }

}
