use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Write;
use anyhow::Result;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::money::Money;

/// Tags whose values identify the same real-world transaction across wallets
/// and sources; used for merging and journal dedup.
pub const DEDUP_KEYS: &[&str] = &["txid", "payment_hash", "coinbase_id"];

/// An hledger account name; segments are joined with `:`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Account(String);

impl Account {
    pub fn new(s: impl Into<String>) -> Self {
        Account(s.into())
    }

    pub fn append(&self, segment: impl AsRef<str>) -> Account {
        Account(format!("{}:{}", self.0, segment.as_ref()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Account {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<Account> for String {
    fn from(a: Account) -> String {
        a.0
    }
}

#[derive(Debug, Default, Clone)]
pub struct TagMap(pub Vec<(String, String)>);

impl TagMap {
    pub fn new() -> Self {
        TagMap(Vec::new())
    }

    pub fn add(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.0.push((k.into(), v.into()));
        self
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn push(&mut self, k: impl Into<String>, v: impl Into<String>) {
        self.0.push((k.into(), v.into()));
    }
}

impl fmt::Display for TagMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.0.iter().map(|(k, v)| format!("{k}:{v}")).collect::<Vec<_>>().join(", ");
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone)]
pub enum PriceAnnotation {
    /// Per-unit price: `@ PRICE`
    Unit(String),
    /// Total cost: `@@ COST`
    Total(String),
}

#[derive(Debug, Clone)]
pub struct Posting {
    pub account: String,
    /// `None` means hledger auto-balances this posting.
    pub amount: Option<Money>,
    pub price: Option<PriceAnnotation>,
    pub tags: TagMap,
}

impl Posting {
    pub fn with_amount(account: impl Into<String>, amount_sat: i64) -> Self {
        Posting { account: account.into(), amount: Some(Money::sat(amount_sat)), price: None, tags: TagMap::new() }
    }

    pub fn with_money(account: impl Into<String>, money: Money) -> Self {
        Posting { account: account.into(), amount: Some(money), price: None, tags: TagMap::new() }
    }

    pub fn auto_balance(account: impl Into<String>) -> Self {
        Posting { account: account.into(), amount: None, price: None, tags: TagMap::new() }
    }

    pub fn with_price(mut self, price: Option<PriceAnnotation>) -> Self {
        self.price = price;
        self
    }

    pub fn with_tags(mut self, tags: TagMap) -> Self {
        self.tags = tags;
        self
    }
}

#[derive(Debug, Clone)]
pub struct JournalEntry {
    pub date: NaiveDate,
    pub description: String,
    pub tags: TagMap,
    pub postings: Vec<Posting>,
}

/// Sums the amounts of all explicit postings with the given commodity.
pub fn sum_commodity(postings: &[Posting], commodity: &str) -> Money {
    let total = postings.iter()
        .filter_map(|p| p.amount.as_ref().filter(|m| m.commodity == commodity))
        .map(|m| m.amount.clone())
        .sum();
    Money::new(total, commodity)
}

/// Merges entries that share a dedup key value (inter-wallet transfers seen by
/// both wallets, or the same payment seen by different sources) into a single
/// entry carrying the union of tags. Auto-balance postings are dropped; if the
/// combined explicit postings don't net to zero, a single auto-balance
/// counterpart is re-added.
pub fn merge_entries(entries: Vec<JournalEntry>) -> Vec<JournalEntry> {
    let mut groups: Vec<Vec<JournalEntry>> = Vec::new();
    let mut index: HashMap<(String, String), usize> = HashMap::new();

    for entry in entries {
        let keys: Vec<(String, String)> = DEDUP_KEYS.iter()
            .filter_map(|k| entry.tags.get(k).map(|v| (k.to_string(), v.to_string())))
            .collect();

        let mut matched: Vec<usize> = keys.iter().filter_map(|k| index.get(k).copied()).collect();
        matched.sort_unstable();
        matched.dedup();

        let target = match matched.first() {
            Some(&g) => g,
            None => {
                groups.push(Vec::new());
                groups.len() - 1
            }
        };
        // An entry can bridge groups (e.g. share a txid with one and a
        // payment_hash with another); fold the extras into the target.
        for &g in matched.iter().skip(1) {
            let moved = std::mem::take(&mut groups[g]);
            groups[target].extend(moved);
        }
        if matched.len() > 1 {
            for g in index.values_mut() {
                if matched[1..].contains(g) {
                    *g = target;
                }
            }
        }
        for k in keys {
            index.insert(k, target);
        }
        groups[target].push(entry);
    }

    let mut result: Vec<JournalEntry> = groups.into_iter()
        .filter(|g| !g.is_empty())
        .map(|mut g| if g.len() == 1 { g.remove(0) } else { merge_group(g) })
        .collect();
    result.sort_by_key(|e| e.date);
    result
}

fn merge_group(entries: Vec<JournalEntry>) -> JournalEntry {
    let date = entries.iter().map(|e| e.date).min().unwrap();
    let description = if entries.iter().all(|e| e.description == entries[0].description) {
        entries[0].description.clone()
    } else {
        "Transfer".to_string()
    };

    let mut tags = TagMap::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for entry in &entries {
        for (k, v) in &entry.tags.0 {
            if seen.insert((k.clone(), v.clone())) {
                tags.push(k.clone(), v.clone());
            }
        }
    }

    let mut postings: Vec<Posting> = entries.into_iter()
        .flat_map(|e| e.postings.into_iter())
        .filter(|p| p.amount.is_some())
        .collect();

    let sat_sum = sum_commodity(&postings, "SAT");
    if !sat_sum.is_zero() {
        postings.push(Posting::auto_balance(if sat_sum.is_negative() { "expenses:unknown" } else { "income:unknown" }));
    }

    JournalEntry { date, description, tags, postings }
}

pub fn write_entries(entries: &[JournalEntry], writer: &mut dyn Write) -> Result<()> {
    for entry in entries {
        write_entry(entry, writer)?;
    }
    Ok(())
}

fn write_entry(entry: &JournalEntry, w: &mut dyn Write) -> Result<()> {
    let description = entry.description.replace(['\n', '\r'], " ");
    write!(w, "{} * {}", entry.date, description)?;
    if !entry.tags.is_empty() {
        write!(w, "  ; {}", entry.tags)?;
    }
    writeln!(w)?;

    for posting in &entry.postings {
        match &posting.amount {
            Some(money) => {
                let price = match &posting.price {
                    Some(PriceAnnotation::Unit(p)) => format!(" @ {p}"),
                    Some(PriceAnnotation::Total(c)) => format!(" @@ {c}"),
                    None => String::new(),
                };
                if posting.tags.is_empty() {
                    writeln!(w, "    {}    {}{}", posting.account, money, price)?;
                } else {
                    writeln!(w, "    {}    {}{}  ; {}", posting.account, money, price, posting.tags)?;
                }
            }
            None => writeln!(w, "    {}", posting.account)?,
        }
    }

    writeln!(w)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(desc: &str, tags: TagMap, postings: Vec<Posting>) -> JournalEntry {
        JournalEntry {
            date: NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
            description: desc.to_string(),
            tags,
            postings,
        }
    }

    #[test]
    fn merges_entries_sharing_txid() {
        let a = entry("Outgoing BTC", TagMap::new().add("txid", "t1").add("source", "electrum"), vec![
            Posting::with_amount("assets:bitcoin:savings:addr1", -5000),
            Posting::auto_balance("expenses:unknown"),
        ]);
        let b = entry("Swap In", TagMap::new().add("txid", "t1").add("source", "phoenix"), vec![
            Posting::with_amount("assets:bitcoin:lightning:phoenix", 4900),
            Posting::with_amount("expenses:fees:onchain", 100),
            Posting::auto_balance("assets:bitcoin"),
        ]);

        let merged = merge_entries(vec![a, b]);
        assert_eq!(merged.len(), 1);
        let m = &merged[0];
        assert_eq!(m.description, "Transfer");
        assert_eq!(m.postings.len(), 3);
        assert!(m.postings.iter().all(|p| p.amount.is_some()));
        let sources: Vec<_> = m.tags.0.iter().filter(|(k, _)| k == "source").collect();
        assert_eq!(sources.len(), 2);
    }

    #[test]
    fn merges_entries_sharing_payment_hash() {
        let a = entry("Zap", TagMap::new().add("payment_hash", "ph1"), vec![
            Posting::with_amount("assets:bitcoin:lightning:a", -1000),
            Posting::auto_balance("expenses:unknown"),
        ]);
        let b = entry("Zap", TagMap::new().add("payment_hash", "ph1"), vec![
            Posting::with_amount("assets:bitcoin:lightning:b", 1000),
            Posting::auto_balance("income:unknown"),
        ]);

        let merged = merge_entries(vec![a, b]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].description, "Zap");
        assert_eq!(merged[0].postings.len(), 2);
    }

    #[test]
    fn unbalanced_merge_readds_auto_balance() {
        let a = entry("A", TagMap::new().add("txid", "t1"), vec![
            Posting::with_amount("assets:x", -5000),
            Posting::auto_balance("expenses:unknown"),
        ]);
        let b = entry("B", TagMap::new().add("txid", "t1"), vec![
            Posting::with_amount("assets:y", 4000),
        ]);

        let merged = merge_entries(vec![a, b]);
        let last = merged[0].postings.last().unwrap();
        assert_eq!(last.account, "expenses:unknown");
        assert!(last.amount.is_none());
    }

    #[test]
    fn entries_without_shared_keys_stay_separate() {
        let a = entry("A", TagMap::new().add("txid", "t1"), vec![Posting::with_amount("x", 1)]);
        let b = entry("B", TagMap::new().add("txid", "t2"), vec![Posting::with_amount("x", 2)]);
        let c = entry("C", TagMap::new(), vec![Posting::with_amount("x", 3)]);
        let d = entry("D", TagMap::new(), vec![Posting::with_amount("x", 4)]);

        assert_eq!(merge_entries(vec![a, b, c, d]).len(), 4);
    }

    #[test]
    fn entry_bridges_groups_on_different_keys() {
        let a = entry("A", TagMap::new().add("txid", "t1"), vec![Posting::with_amount("x", 1)]);
        let b = entry("B", TagMap::new().add("payment_hash", "ph1"), vec![Posting::with_amount("y", 2)]);
        let bridge = entry("C", TagMap::new().add("txid", "t1").add("payment_hash", "ph1"),
            vec![Posting::with_amount("z", -3)]);

        let merged = merge_entries(vec![a, b, bridge]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].postings.iter().filter(|p| p.amount.is_some()).count(), 3);
    }

    #[test]
    fn duplicate_tag_pairs_dedup_in_merge() {
        let a = entry("A", TagMap::new().add("txid", "t1").add("source", "electrum"),
            vec![Posting::with_amount("x", 1)]);
        let b = entry("B", TagMap::new().add("txid", "t1").add("source", "electrum"),
            vec![Posting::with_amount("y", -1)]);

        let merged = merge_entries(vec![a, b]);
        let txids: Vec<_> = merged[0].tags.0.iter().filter(|(k, _)| k == "txid").collect();
        let sources: Vec<_> = merged[0].tags.0.iter().filter(|(k, _)| k == "source").collect();
        assert_eq!(txids.len(), 1);
        assert_eq!(sources.len(), 1);
    }
}
