# Adding a Feed

A feed is any off-chain data source (e.g an exchange api, a lightning wallet export, etc) that produces journal entries in hledger. Feeds are reconciled against on-chain and lightning sources using the `txid` and `payment_hash`, respectively. Here's how to make a new feed:

## Step 1 — Create the crate

```
crates/hledger-btc-<name>/
  Cargo.toml
  src/lib.rs
  src/<name>.rs
```

**`Cargo.toml`**
```toml
[package]
name = "hledger-btc-<name>"
version = "0.1.0"
edition = "2024"

[dependencies]
hledger-btc-core = { path = "../hledger-btc-core" }
anyhow.workspace = true
chrono.workspace = true
serde.workspace = true
toml.workspace = true
tracing.workspace = true
csv.workspace = true            # CSV feeds
# serde_json.workspace = true   # API feeds
# ureq.workspace = true         # API feeds
```

## Step 2 — Write the parser (`src/<name>.rs`)

```rust
use std::io::Read;
use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use hledger_btc_core::journal::{JournalEntry, Posting, TagMap};
use hledger_btc_core::money::Money;
use hledger_btc_core::source::FeedEntry;

#[derive(Deserialize)]
struct Row {
    date: String,
    // field names must match CSV headers exactly
}

pub fn parse(reader: impl Read, account: &str) -> Result<Vec<FeedEntry>> {
    let mut rdr = csv::Reader::from_reader(reader);
    let mut entries = Vec::new();
    for result in rdr.deserialize::<Row>() {
        let row = result.context("failed to parse CSV row")?;
        if let Some(entry) = row_to_entry(&row, account)? {
            entries.push(entry);
        }
    }
    entries.sort_by_key(|e| e.journal.date);
    Ok(entries)
}

fn row_to_entry(row: &Row, account: &str) -> Result<Option<FeedEntry>> {
    let journal = JournalEntry {
        date: NaiveDate::parse_from_str(&row.date[..10], "%Y-%m-%d")?,
        description: "...".to_string(),
        tags: TagMap::new(),
        postings: vec![
            Posting::with_amount(account, amount_sat), // i64 satoshis; negative = debit
            Posting::with_money("expenses:fees:<name>", Money::parse("1.50", "USD")?),
            Posting::auto_balance("expenses:unknown"),
        ],
    };

    Ok(Some(FeedEntry::onchain(row.txid.clone(), journal)))
    // Ok(Some(FeedEntry::lightning(row.payment_hash.clone(), journal)))
    // Ok(Some(FeedEntry::internal("<name>_id", row.id.clone(), journal)))
    // Return Ok(None) + tracing::warn! for unrecognized row types
}
```

Use `FeedEntry::onchain` for withdrawals/deposits, `FeedEntry::lightning` for Lightning, and `FeedEntry::internal` only for trades (exchange-internal, no on-chain footprint). Using `internal` for withdrawals or deposits silently breaks reconciliation against wallet scans.

To attach a provider ID as an informational tag on an on-chain entry:
```rust
let mut journal = JournalEntry { ... };
journal.tags.push("<name>_id", &row.id);
FeedEntry::onchain(row.txid.clone(), journal)
```

For API feeds see `hledger_btc_coinbase` for an HTTP + JWT example.

## Step 3 — Implement `Source` (`src/lib.rs`)

```rust
mod <name>;

use std::path::PathBuf;
use anyhow::{Context, Result};
use serde::Deserialize;
use hledger_btc_core::journal::Account;
use hledger_btc_core::source::{FeedEntry, Source};

pub struct <Name>Feed { path: PathBuf, account: Account }

impl <Name>Feed {
    pub fn new(path: PathBuf, account: Account) -> Self { Self { path, account } }
}

impl Source for <Name>Feed {
    fn name(&self) -> &str { "<name>" }

    fn entries(&self) -> Result<Vec<FeedEntry>> {
        let file = std::fs::File::open(&self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        <name>::parse(file, self.account.as_str())
    }
}

#[derive(Deserialize)]
struct <Name>Config { path: PathBuf }

pub fn build(config: &toml::Table, account: Account) -> Result<Box<dyn Source + 'static>> {
    let cfg: <Name>Config = toml::Value::Table(config.clone())
        .try_into().context("invalid <name> config")?;
    Ok(Box::new(<Name>Feed::new(cfg.path, account)))
}
```

## Step 4 — Wire into the binary

**`crates/hledger-btc/Cargo.toml`**
```toml
[features]
<name> = ["dep:hledger-btc-<name>"]

[dependencies]
hledger-btc-<name> = { path = "../hledger-btc-<name>", optional = true }
```

**`crates/hledger-btc/src/feeds.rs`** — add a match arm:
```rust
#[cfg(feature = "<name>")]
"<name>" => hledger_btc_<name>::build(&entry.config, entry.account_name(&cfg.base_account)),
```

**`crates/hledger-btc/src/main.rs`** — add to `FeedProvider` enum and `ImportSubcommand::Feed` match:
```rust
// FeedProvider enum:
#[cfg(feature = "<name>")]
/// Import from <Name> CSV export
<Name> {
    #[arg(long)] path: PathBuf,
    #[arg(long)] name: Option<String>,
},

// ImportSubcommand::Feed match:
#[cfg(feature = "<name>")]
FeedProvider::<Name> { path, name } => {
    let account = base.append(name.as_deref().unwrap_or("<name>"));
    Box::new(hledger_btc_<name>::<Name>Feed::new(path, account))
}
```

## Step 5 — Config entry

```toml
[[feeds]]
name = "<name>"
provider = "<name>"
path = "/path/to/export.csv"
```

`name` becomes the account sub-segment. Additional fields are passed to `build()` via `toml::Table` — add them to your config struct.

## Step 6 — Tests

Test the `kind` field — dedup tags are stamped by `collect()`, not the parser.

```rust
use hledger_btc_core::source::EntryKind;

const HEADER: &str = "date,type,...";
fn csv(rows: &[&str]) -> String { format!("{HEADER}\n{}\n", rows.join("\n")) }

#[test]
fn parses_withdrawal_as_onchain() {
    let entries = parse(csv(&["2026-06-01,withdrawal,..."]).as_bytes(), "assets:<name>").unwrap();
    assert!(matches!(&entries[0].kind, EntryKind::OnChain { txid } if txid == "expected_txid"));
}
```

Cover each row type, verify `EntryKind` and ID, confirm unknown types return `Ok(None)`, check sort order.

## Build and test

```sh
cargo build --features <name>
cargo test -p hledger-btc-<name>
cargo run --features <name> -- import feed <name> --path export.csv
```
