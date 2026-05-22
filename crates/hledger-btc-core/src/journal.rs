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
}

impl fmt::Display for TagMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.0.iter().map(|(k, v)| format!("{k}:{v}")).collect::<Vec<_>>().join(", ");
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone)]
pub struct Posting {
    pub account: String,
    /// `None` means hledger auto-balances this posting.
    pub amount_sat: Option<i64>,
    pub tags: TagMap,
}

impl Posting {
    pub fn with_amount(account: impl Into<String>, amount_sat: i64) -> Self {
        Posting { account: account.into(), amount_sat: Some(amount_sat), tags: TagMap::new() }
    }

    pub fn auto_balance(account: impl Into<String>) -> Self {
        Posting { account: account.into(), amount_sat: None, tags: TagMap::new() }
    }
}

#[derive(Debug, Clone)]
pub struct JournalEntry {
    pub date: NaiveDate,
    pub description: String,
    pub tags: TagMap,
    pub postings: Vec<Posting>,
}

pub fn read_txids(reader: impl Read) -> Result<HashSet<String>> {
    let mut txids = HashSet::new();
    for line in BufReader::new(reader).lines() {
        let line = line?;
        if line.starts_with(' ') || line.starts_with('\t') || line.is_empty() {
            continue;
        }
        if let Some(pos) = line.find("txid:") {
            let rest = &line[pos + 5..];
            let end = rest.find([',', ' ']).unwrap_or(rest.len());
            txids.insert(rest[..end].to_string());
        }
    }
    Ok(txids)
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
                if posting.tags.is_empty() {
                    writeln!(w, "    {}    {} SAT", posting.account, sats)?;
                } else {
                    writeln!(w, "    {}    {} SAT  ; {}", posting.account, sats, posting.tags)?;
                }
            }
            None => writeln!(w, "    {}", posting.account)?,
        }
    }

    writeln!(w)?;
    Ok(())
}
