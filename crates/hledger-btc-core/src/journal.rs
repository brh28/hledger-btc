use std::collections::HashSet;
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use anyhow::Result;
use chrono::NaiveDate;

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
    pub amount_sat: Option<i64>,
    pub price: Option<PriceAnnotation>,
    pub tags: TagMap,
}

impl Posting {
    pub fn with_amount(account: impl Into<String>, amount_sat: i64) -> Self {
        Posting { account: account.into(), amount_sat: Some(amount_sat), price: None, tags: TagMap::new() }
    }

    pub fn auto_balance(account: impl Into<String>) -> Self {
        Posting { account: account.into(), amount_sat: None, price: None, tags: TagMap::new() }
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

pub fn read_tag_values(reader: impl Read, tag: &str) -> Result<HashSet<String>> {
    let prefix = format!("{tag}:");
    let mut values = HashSet::new();
    for line in BufReader::new(reader).lines() {
        let line = line?;
        if line.starts_with(' ') || line.starts_with('\t') || line.is_empty() {
            continue;
        }
        if let Some(pos) = line.find(&prefix) {
            let rest = &line[pos + prefix.len()..];
            let end = rest.find([',', ' ']).unwrap_or(rest.len());
            values.insert(rest[..end].to_string());
        }
    }
    Ok(values)
}

pub fn read_txids(reader: impl Read) -> Result<HashSet<String>> {
    read_tag_values(reader, "txid")
}

/// Merges entries that share a txid (inter-wallet transfers) into a single entry.
/// Auto-balance postings are dropped; if the combined explicit postings don't net to
/// zero, a single auto-balance counterpart is re-added.
pub fn merge_by_txid(entries: Vec<JournalEntry>) -> Vec<JournalEntry> {
    let mut order: Vec<String> = Vec::new();
    let mut grouped: std::collections::HashMap<String, Vec<JournalEntry>> = std::collections::HashMap::new();

    for entry in entries {
        let txid = entry.tags.0.iter()
            .find(|(k, _)| k == "txid")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        if !grouped.contains_key(&txid) {
            order.push(txid.clone());
        }
        grouped.entry(txid).or_default().push(entry);
    }

    let mut result: Vec<JournalEntry> = order.into_iter()
        .map(|txid| {
            let mut group = grouped.remove(&txid).unwrap();
            if group.len() == 1 {
                group.remove(0)
            } else {
                merge_wallet_entries(group)
            }
        })
        .collect();

    result.sort_by_key(|e| e.date);
    result
}

fn merge_wallet_entries(entries: Vec<JournalEntry>) -> JournalEntry {
    let date = entries[0].date;
    let tags = entries[0].tags.clone();

    let mut postings: Vec<Posting> = entries.into_iter()
        .flat_map(|e| e.postings.into_iter())
        .filter(|p| p.amount_sat.is_some())
        .collect();

    let sum: i64 = postings.iter().filter_map(|p| p.amount_sat).sum();
    if sum != 0 {
        postings.push(Posting::auto_balance(if sum < 0 { "expenses:unknown" } else { "income:unknown" }));
    }

    JournalEntry { date, description: "Transfer".to_string(), tags, postings }
}

pub fn write_entries(entries: &[JournalEntry], writer: &mut dyn Write) -> Result<()> {
    for entry in entries {
        write_entry(entry, writer)?;
    }
    Ok(())
}

fn write_entry(entry: &JournalEntry, w: &mut dyn Write) -> Result<()> {
    write!(w, "{} * {}", entry.date, entry.description)?;
    if !entry.tags.is_empty() {
        write!(w, "  ; {}", entry.tags)?;
    }
    writeln!(w)?;

    for posting in &entry.postings {
        match posting.amount_sat {
            Some(sats) => {
                let price = match &posting.price {
                    Some(PriceAnnotation::Unit(p)) => format!(" @ {p}"),
                    Some(PriceAnnotation::Total(c)) => format!(" @@ {c}"),
                    None => String::new(),
                };
                let amount = fmt_sats(sats);
                if posting.tags.is_empty() {
                    writeln!(w, "    {}    {} sat{}", posting.account, amount, price)?;
                } else {
                    writeln!(w, "    {}    {} sat{}  ; {}", posting.account, amount, price, posting.tags)?;
                }
            }
            None => writeln!(w, "    {}", posting.account)?,
        }
    }

    writeln!(w)?;
    Ok(())
}

fn fmt_sats(sats: i64) -> String {
    let s = sats.unsigned_abs().to_string();
    let with_commas = s.as_bytes().rchunks(3)
        .rev()
        .map(std::str::from_utf8)
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join(",");
    if sats < 0 { format!("-{with_commas}") } else { with_commas }
}
