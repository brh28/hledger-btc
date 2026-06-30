use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::{BufRead, BufReader, Read};
use anyhow::Result;

use crate::journal::{JournalEntry, DEDUP_KEYS};

/// Tag stamped on every entry recording which source produced it.
pub const SOURCE_TAG: &str = "source";

/// A data source that produces feed entries.
pub trait Source {
    fn name(&self) -> &str;
    fn entries(&self) -> Result<Vec<FeedEntry>>;
}

/// The dedup identity of a feed entry. `collect()` reads this to stamp the
/// correct tag automatically — feed implementors must not stamp dedup tags
/// manually on the inner `JournalEntry`.
#[derive(Clone)]
pub enum EntryKind {
    /// On-chain transaction. Stamps `txid:<value>` and participates in
    /// cross-source reconciliation with wallet scan entries.
    OnChain { txid: String },
    /// Lightning payment. Stamps `payment_hash:<value>` and participates in
    /// cross-source reconciliation with Phoenix entries.
    Lightning { payment_hash: String },
    /// Provider-assigned ID with no on-chain footprint (e.g. a trade or
    /// exchange-internal transfer). Stamps `<key>:<value>`. Use only for
    /// entries that will never appear in a wallet scan — using this for
    /// withdrawals/deposits silently breaks cross-source reconciliation.
    Provider { key: &'static str, id: String },
}

/// A journal entry produced by a feed, paired with its dedup identity.
#[derive(Clone)]
pub struct FeedEntry {
    pub journal: JournalEntry,
    pub kind: EntryKind,
}

impl FeedEntry {
    pub fn onchain(txid: String, journal: JournalEntry) -> Self {
        Self { journal, kind: EntryKind::OnChain { txid } }
    }

    pub fn lightning(payment_hash: String, journal: JournalEntry) -> Self {
        Self { journal, kind: EntryKind::Lightning { payment_hash } }
    }

    pub fn provider(key: &'static str, id: String, journal: JournalEntry) -> Self {
        Self { journal, kind: EntryKind::Provider { key, id } }
    }
}

pub struct Collected {
    pub entries: Vec<JournalEntry>,
    pub failures: Vec<(String, anyhow::Error)>,
    /// Dedup key names declared by Provider entries across all sources.
    pub provider_keys: Vec<&'static str>,
}

/// Collects entries from all sources, stamping each entry with its dedup tag
/// (from `EntryKind`) and `source:<name>`. A failing source is reported but
/// not fatal, so one unreachable source doesn't stop the others from recording.
pub fn collect<'a>(sources: &[Box<dyn Source + 'a>]) -> Collected {
    let mut entries = Vec::new();
    let mut failures = Vec::new();
    let mut provider_keys: Vec<&'static str> = Vec::new();

    for source in sources {
        match source.entries() {
            Ok(batch) => {
                for mut feed_entry in batch {
                    match &feed_entry.kind {
                        EntryKind::OnChain { txid } => {
                            feed_entry.journal.tags.push("txid", txid.clone());
                            feed_entry.journal.tags.push(SOURCE_TAG, source.name());
                        }
                        EntryKind::Lightning { payment_hash } => {
                            feed_entry.journal.tags.push("payment_hash", payment_hash.clone());
                            feed_entry.journal.tags.push(SOURCE_TAG, source.name());
                        }
                        EntryKind::Provider { key, id } => {
                            feed_entry.journal.tags.push(*key, id.clone());
                            if !provider_keys.contains(key) {
                                provider_keys.push(key);
                            }
                        }
                    }
                    entries.push(feed_entry.journal);
                }
            }
            Err(err) => failures.push((source.name().to_string(), err)),
        }
    }

    Collected { entries, failures, provider_keys }
}

/// Dedup-key values already present in the journal, and which sources are
/// stamped on each. Parsed from transaction header lines of `hledger print`
/// output. An empty source set means the entry predates source stamping.
#[derive(Debug, Default)]
pub struct KnownKeys(HashMap<(String, String), HashSet<String>>);

impl KnownKeys {
    pub fn parse(reader: impl Read, provider_keys: &[&'static str]) -> Result<Self> {
        let mut map: HashMap<(String, String), HashSet<String>> = HashMap::new();
        for line in BufReader::new(reader).lines() {
            let line = line?;
            if line.starts_with(' ') || line.starts_with('\t') || line.is_empty() {
                continue;
            }
            let sources = extract_all(&line, SOURCE_TAG);
            for key in DEDUP_KEYS.iter().copied().chain(provider_keys.iter().copied()) {
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
/// Checks both universal dedup keys (`DEDUP_KEYS`) and provider-specific keys
/// collected from `Provider` entries. A known entry whose sources are all
/// already stamped on the journal entry is skipped silently; a known entry
/// contributing a novel source produces a notice.
pub fn plan(entries: Vec<JournalEntry>, known: &KnownKeys, provider_keys: &[&'static str]) -> ScanPlan {
    let mut new_entries = Vec::new();
    let mut already_recorded = 0;
    let mut notices = Vec::new();

    for entry in entries {
        let mut matched: Option<(String, String)> = None;
        let mut recorded: HashSet<String> = HashSet::new();
        for key in DEDUP_KEYS.iter().copied().chain(provider_keys.iter().copied()) {
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
        entries: Vec<FeedEntry>,
    }

    impl Source for Dummy {
        fn name(&self) -> &str { self.name }
        fn entries(&self) -> Result<Vec<FeedEntry>> {
            Ok(self.entries.clone())
        }
    }

    fn journal(tags: TagMap) -> JournalEntry {
        JournalEntry {
            date: NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
            description: "Test".to_string(),
            tags,
            postings: vec![Posting::with_amount("assets:test", 100)],
            status: Some(true),
        }
    }

    fn stamped(tags: TagMap, source: &str) -> JournalEntry {
        let mut e = journal(tags);
        e.tags.push(SOURCE_TAG, source);
        e
    }

    #[test]
    fn collect_stamps_source_and_dedup_tags() {
        let sources: Vec<Box<dyn Source>> = vec![Box::new(Dummy {
            name: "phoenix",
            entries: vec![FeedEntry::lightning("ph1".to_string(), journal(TagMap::new()))],
        })];
        let collected = collect(&sources);
        assert!(collected.failures.is_empty());
        assert_eq!(collected.entries[0].tags.get(SOURCE_TAG), Some("phoenix"));
        assert_eq!(collected.entries[0].tags.get("payment_hash"), Some("ph1"));
    }

    #[test]
    fn collect_stamps_provider_key_without_source_tag() {
        let sources: Vec<Box<dyn Source>> = vec![Box::new(Dummy {
            name: "coinbase",
            entries: vec![FeedEntry::provider("coinbase_id", "ord1".to_string(), journal(TagMap::new()))],
        })];
        let collected = collect(&sources);
        assert_eq!(collected.entries[0].tags.get("coinbase_id"), Some("ord1"));
        assert_eq!(collected.entries[0].tags.get(SOURCE_TAG), None);
        assert_eq!(collected.provider_keys, vec!["coinbase_id"]);
    }

    #[test]
    fn collect_continues_past_failing_source() {
        struct Failing;
        impl Source for Failing {
            fn name(&self) -> &str { "broken" }
            fn entries(&self) -> Result<Vec<FeedEntry>> {
                anyhow::bail!("unreachable")
            }
        }
        let sources: Vec<Box<dyn Source>> = vec![
            Box::new(Failing),
            Box::new(Dummy {
                name: "ok",
                entries: vec![FeedEntry::onchain("t1".to_string(), journal(TagMap::new()))],
            }),
        ];
        let collected = collect(&sources);
        assert_eq!(collected.failures.len(), 1);
        assert_eq!(collected.failures[0].0, "broken");
        assert_eq!(collected.entries.len(), 1);
    }

    #[test]
    fn parses_known_keys_with_sources() {
        let journal_text = "\
2026-05-02 * Incoming BTC  ; txid:abc, source:electrum
    assets:bitcoin:w    4,000 sat  ; vout:1
    income:unknown

2026-05-03 * Zap  ; payment_hash:ph1, source:phoenix, source:electrum
    assets:bitcoin:lightning    100 sat
";
        let known = KnownKeys::parse(journal_text.as_bytes(), &[]).unwrap();
        let abc = known.0.get(&("txid".to_string(), "abc".to_string())).unwrap();
        assert_eq!(abc.len(), 1);
        assert!(abc.contains("electrum"));
        let ph = known.0.get(&("payment_hash".to_string(), "ph1".to_string())).unwrap();
        assert_eq!(ph.len(), 2);
    }

    #[test]
    fn parses_known_keys_with_provider_key() {
        let journal_text = "2026-05-02 * Trade  ; coinbase_id:ord1\n    assets:coinbase:usd    -100 USD\n";
        let known = KnownKeys::parse(journal_text.as_bytes(), &["coinbase_id"]).unwrap();
        assert!(known.0.contains_key(&("coinbase_id".to_string(), "ord1".to_string())));
    }

    #[test]
    fn plan_appends_unknown_entry() {
        let known = KnownKeys::default();
        let plan = plan(vec![stamped(TagMap::new().add("txid", "t1"), "electrum")], &known, &[]);
        assert_eq!(plan.new_entries.len(), 1);
        assert_eq!(plan.already_recorded, 0);
        assert!(plan.notices.is_empty());
    }

    #[test]
    fn plan_skips_silently_when_source_already_stamped() {
        let mut known = KnownKeys::default();
        known.0.insert(("txid".into(), "t1".into()), HashSet::from(["electrum".to_string()]));
        let plan = plan(vec![stamped(TagMap::new().add("txid", "t1"), "electrum")], &known, &[]);
        assert!(plan.new_entries.is_empty());
        assert_eq!(plan.already_recorded, 1);
        assert!(plan.notices.is_empty());
    }

    #[test]
    fn plan_notices_novel_source_for_known_key() {
        let mut known = KnownKeys::default();
        known.0.insert(("txid".into(), "t1".into()), HashSet::from(["electrum".to_string()]));
        let plan = plan(vec![stamped(TagMap::new().add("txid", "t1"), "coinbase")], &known, &[]);
        assert!(plan.new_entries.is_empty());
        assert_eq!(plan.already_recorded, 1);
        assert_eq!(plan.notices.len(), 1);
        assert_eq!(plan.notices[0].novel_sources, vec!["coinbase"]);
        assert_eq!(plan.notices[0].recorded_sources, vec!["electrum"]);
    }

    #[test]
    fn plan_uses_provider_key_for_dedup() {
        let mut known = KnownKeys::default();
        known.0.insert(("coinbase_id".into(), "ord1".into()), HashSet::new());
        let plan = plan(
            vec![journal(TagMap::new().add("coinbase_id", "ord1"))],
            &known,
            &["coinbase_id"],
        );
        assert!(plan.new_entries.is_empty());
        assert_eq!(plan.already_recorded, 1);
        assert!(plan.notices.is_empty());
    }

    #[test]
    fn plan_skips_legacy_unstamped_entries_silently() {
        let mut known = KnownKeys::default();
        known.0.insert(("txid".into(), "t1".into()), HashSet::new());
        let plan = plan(vec![stamped(TagMap::new().add("txid", "t1"), "phoenix")], &known, &[]);
        assert!(plan.new_entries.is_empty());
        assert_eq!(plan.already_recorded, 1);
        assert!(plan.notices.is_empty());
    }
}
