use std::io::Read;
use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;

use hledger_btc_core::journal::{JournalEntry, Posting, PriceAnnotation, TagMap};
use hledger_btc_core::money::Money;
use hledger_btc_core::source::FeedEntry;

#[derive(Deserialize)]
struct Row {
    #[serde(rename = "Date")]
    date: String,
    #[serde(rename = "Reference Code")]
    reference_code: String,
    #[serde(rename = "Transaction Type")]
    transaction_type: String,
    #[serde(rename = "Sent Amount")]
    sent_amount: String,
    #[serde(rename = "Sent Currency")]
    sent_currency: String,
    #[serde(rename = "Received Amount")]
    received_amount: String,
    #[serde(rename = "Received Currency")]
    received_currency: String,
    #[serde(rename = "Fee Amount")]
    fee_amount: String,
    #[serde(rename = "Fee Currency")]
    _fee_currency: String,
    #[serde(rename = "Total Amount")]
    _total_amount: String,
    #[serde(rename = "Total Currency")]
    _total_currency: String,
    #[serde(rename = "Method")]
    method: String,
    #[serde(rename = "Source")]
    _source: String,
    #[serde(rename = "Destination")]
    _destination: String,
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
    let date = NaiveDate::parse_from_str(&row.date[..10], "%Y-%m-%d")
        .with_context(|| format!("invalid date: '{}'", row.date))?;
    let id = row.reference_code.trim().to_string();
    let desc = row.transaction_type.trim().to_string();

    match row.method.trim() {
        "ACH Bank Transfer" => Ok(None),
        "Bitcoin Balance" => trade_entry(row, date, account, &id, &desc),
        "Interest Earned" => interest_entry(row, date, account, &id, &desc).map(Some),
        "Lightning" => lightning_entry(row, date, account, &id, &desc),
        "On Chain" => onchain_entry(row, date, account, &id, &desc),
        other => {
            tracing::warn!("unknown River method: {other:?}, skipping");
            Ok(None)
        }
    }
}

fn trade_entry(row: &Row, date: NaiveDate, account: &str, id: &str, desc: &str) -> Result<Option<FeedEntry>> {
    match (row.sent_currency.trim(), row.received_currency.trim()) {
        ("USD", "BTC") => buy_entry(row, date, account, id, desc).map(Some),
        ("BTC", "USD") => sell_entry(row, date, account, id, desc).map(Some),
        (s, r) => {
            tracing::warn!("unexpected River trade currencies {s} → {r}, skipping");
            Ok(None)
        }
    }
}

fn buy_entry(row: &Row, date: NaiveDate, account: &str, id: &str, desc: &str) -> Result<FeedEntry> {
    let sat = btc_to_sat(row.received_amount.trim())?;
    let cost_usd = parse_decimal(row.sent_amount.trim())?;
    let fee_usd = parse_decimal(row.fee_amount.trim())?.abs();

    let price = (cost_usd > 0.0).then(|| PriceAnnotation::Total(format!("${cost_usd:.2}")));

    let mut postings = vec![Posting::with_amount(format!("{account}:btc"), sat).with_price(price)];
    if fee_usd > 0.0 {
        postings.push(Posting::with_money("expenses:fees:river", Money::parse(&format!("{fee_usd:.2}"), "USD")?));
    }
    postings.push(Posting::auto_balance(format!("{account}:usd")));

    Ok(FeedEntry::provider("river_id", id.to_string(), JournalEntry {
        date,
        description: desc.to_string(),
        tags: TagMap::new(),
        postings,
    }))
}

fn sell_entry(row: &Row, date: NaiveDate, account: &str, id: &str, desc: &str) -> Result<FeedEntry> {
    let sat = -btc_to_sat(row.sent_amount.trim())?.abs();
    let gross_usd = parse_decimal(row.received_amount.trim())?;
    let fee_usd = parse_decimal(row.fee_amount.trim())?.abs();

    let price = (gross_usd > 0.0).then(|| PriceAnnotation::Total(format!("${gross_usd:.2}")));

    let mut postings = vec![Posting::with_amount(format!("{account}:btc"), sat).with_price(price)];
    if fee_usd > 0.0 {
        postings.push(Posting::with_money("expenses:fees:river", Money::parse(&format!("{fee_usd:.2}"), "USD")?));
    }
    postings.push(Posting::auto_balance(format!("{account}:usd")));

    Ok(FeedEntry::provider("river_id", id.to_string(), JournalEntry {
        date,
        description: desc.to_string(),
        tags: TagMap::new(),
        postings,
    }))
}

fn interest_entry(row: &Row, date: NaiveDate, account: &str, id: &str, desc: &str) -> Result<FeedEntry> {
    let sat = btc_to_sat(row.received_amount.trim())?;
    Ok(FeedEntry::provider("river_id", id.to_string(), JournalEntry {
        date,
        description: desc.to_string(),
        tags: TagMap::new(),
        postings: vec![
            Posting::with_amount(format!("{account}:btc"), sat),
            Posting::auto_balance("income:interest:river"),
        ],
    }))
}

// Sent Amount = principal; Fee Amount = routing fee (both BTC). Total debit =
// principal + fee so the auto-balanced leg captures just the payment amount.
fn lightning_entry(row: &Row, date: NaiveDate, account: &str, id: &str, desc: &str) -> Result<Option<FeedEntry>> {
    if let Some(principal_sat) = parse_btc_nonzero(row.sent_amount.trim())? {
        let fee_sat = parse_sat_opt(row.fee_amount.trim())?.abs();
        let total = -(principal_sat + fee_sat);
        let mut postings = vec![Posting::with_amount(format!("{account}:btc"), total)];
        if fee_sat > 0 {
            postings.push(Posting::with_amount("expenses:fees:river", fee_sat));
        }
        postings.push(Posting::auto_balance("expenses:unknown"));
        Ok(Some(FeedEntry::provider("river_id", id.to_string(), JournalEntry {
            date, description: desc.to_string(), tags: TagMap::new(), postings,
        })))
    } else {
        let sat = btc_to_sat(row.received_amount.trim())?;
        Ok(Some(FeedEntry::provider("river_id", id.to_string(), JournalEntry {
            date,
            description: desc.to_string(),
            tags: TagMap::new(),
            postings: vec![
                Posting::with_amount(format!("{account}:btc"), sat),
                Posting::auto_balance("income:unknown"),
            ],
        })))
    }
}

// Sent Amount = principal; Fee Amount = miner fee (both BTC). Total debit =
// principal + fee so the auto-balanced leg captures just the withdrawn amount.
fn onchain_entry(row: &Row, date: NaiveDate, account: &str, id: &str, desc: &str) -> Result<Option<FeedEntry>> {
    if let Some(principal_sat) = parse_btc_nonzero(row.sent_amount.trim())? {
        let fee_sat = parse_sat_opt(row.fee_amount.trim())?.abs();
        let total = -(principal_sat + fee_sat);
        let mut postings = vec![Posting::with_amount(format!("{account}:btc"), total)];
        if fee_sat > 0 {
            postings.push(Posting::with_amount("expenses:fees:river", fee_sat));
        }
        postings.push(Posting::auto_balance("expenses:unknown"));
        Ok(Some(FeedEntry::provider("river_id", id.to_string(), JournalEntry {
            date, description: desc.to_string(), tags: TagMap::new(), postings,
        })))
    } else {
        let sat = btc_to_sat(row.received_amount.trim())?;
        Ok(Some(FeedEntry::provider("river_id", id.to_string(), JournalEntry {
            date,
            description: desc.to_string(),
            tags: TagMap::new(),
            postings: vec![
                Posting::with_amount(format!("{account}:btc"), sat),
                Posting::auto_balance("income:unknown"),
            ],
        })))
    }
}

fn parse_decimal(s: &str) -> Result<f64> {
    if s.is_empty() {
        return Ok(0.0);
    }
    s.parse::<f64>().with_context(|| format!("invalid amount: '{s}'"))
}

fn parse_btc_nonzero(s: &str) -> Result<Option<i64>> {
    if s.is_empty() {
        return Ok(None);
    }
    let sat = btc_to_sat(s)?;
    Ok(if sat == 0 { None } else { Some(sat) })
}

fn parse_sat_opt(s: &str) -> Result<i64> {
    if s.is_empty() {
        return Ok(0);
    }
    btc_to_sat(s)
}

fn btc_to_sat(s: &str) -> Result<i64> {
    if s.is_empty() {
        return Ok(0);
    }
    let neg = s.starts_with('-');
    let s = if neg { &s[1..] } else { s };
    let mut parts = s.splitn(2, '.');
    let int_part: i64 = parts.next().unwrap_or("0").parse().context("invalid BTC amount")?;
    let frac_str = parts.next().unwrap_or("");
    let frac_padded = format!("{frac_str:0<8}");
    let frac_part: i64 = frac_padded[..8].parse().context("invalid BTC fractional part")?;
    let sat = int_part * 100_000_000 + frac_part;
    Ok(if neg { -sat } else { sat })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hledger_btc_core::source::EntryKind;

    const ACCOUNT: &str = "assets:river";
    const HEADER: &str = "Date,Reference Code,Transaction Type,Sent Amount,Sent Currency,Received Amount,Received Currency,Fee Amount,Fee Currency,Total Amount,Total Currency,Method,Source,Destination";

    fn csv(rows: &[&str]) -> String {
        format!("{HEADER}\n{}\n", rows.join("\n"))
    }

    fn ref_code(e: &FeedEntry) -> &str {
        match &e.kind {
            EntryKind::Provider { id, .. } => id.as_str(),
            _ => panic!("expected internal"),
        }
    }

    #[test]
    fn parses_buy() {
        let data = csv(&["2024-07-01 13:57:16,REF001,Buy,100.00,USD,0.00100000,BTC,0.50,USD,100.50,USD,Bitcoin Balance,,"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(ref_code(e), "REF001");
        assert_eq!(e.journal.description, "Buy");
        assert_eq!(e.journal.postings[0].amount, Some(Money::sat(100_000)));
        assert!(matches!(&e.journal.postings[0].price, Some(PriceAnnotation::Total(s)) if s == "$100.00"));
        assert_eq!(e.journal.postings[1].account, "expenses:fees:river");
        assert!(e.journal.postings[2].amount.is_none());
    }

    #[test]
    fn buy_with_no_fee_omits_fee_posting() {
        let data = csv(&["2024-07-01 13:57:16,REF002,Buy,100.00,USD,0.00100000,BTC,,USD,100.00,USD,Bitcoin Balance,,"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        assert_eq!(entries[0].journal.postings.len(), 2);
        assert!(entries[0].journal.postings[1].amount.is_none());
    }

    #[test]
    fn parses_sell() {
        let data = csv(&["2024-07-01 13:57:16,REF003,Sell,0.00100000,BTC,98.00,USD,0.50,USD,97.50,USD,Bitcoin Balance,,"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        assert_eq!(entries[0].journal.postings[0].amount, Some(Money::sat(-100_000)));
        assert!(matches!(&entries[0].journal.postings[0].price, Some(PriceAnnotation::Total(s)) if s == "$98.00"));
        assert_eq!(entries[0].journal.postings[1].account, "expenses:fees:river");
        assert!(entries[0].journal.postings[2].amount.is_none());
    }

    #[test]
    fn parses_interest() {
        let data = csv(&["2024-07-01 13:57:16,REF004,Interest,,,0.00001000,BTC,,,0.00001000,BTC,Interest Earned,,"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        assert_eq!(entries[0].journal.postings[0].amount, Some(Money::sat(1_000)));
        assert_eq!(entries[0].journal.postings[1].account, "income:interest:river");
    }

    #[test]
    fn parses_lightning_send() {
        let data = csv(&["2024-07-01 13:57:16,REF005,Lightning Send,0.00010000,BTC,,BTC,0.00000002,BTC,0.00010002,BTC,Lightning,,"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        assert_eq!(entries[0].journal.postings[0].amount, Some(Money::sat(-10_002)));
        assert_eq!(entries[0].journal.postings[1].account, "expenses:fees:river");
        assert_eq!(entries[0].journal.postings[1].amount, Some(Money::sat(2)));
        assert_eq!(entries[0].journal.postings[2].account, "expenses:unknown");
        assert!(entries[0].journal.postings[2].amount.is_none());
    }

    #[test]
    fn parses_lightning_receive() {
        let data = csv(&["2024-07-01 13:57:16,REF006,Lightning Receive,,BTC,0.00005000,BTC,,,0.00005000,BTC,Lightning,,"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        assert_eq!(entries[0].journal.postings[0].amount, Some(Money::sat(5_000)));
        assert_eq!(entries[0].journal.postings[1].account, "income:unknown");
    }

    #[test]
    fn parses_onchain_withdrawal() {
        let data = csv(&["2024-07-01 13:57:16,REF007,Withdrawal,0.01000000,BTC,,BTC,0.00001000,BTC,0.01001000,BTC,On Chain,,"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        assert_eq!(entries[0].journal.postings[0].amount, Some(Money::sat(-1_001_000)));
        assert_eq!(entries[0].journal.postings[1].amount, Some(Money::sat(1_000)));
        assert!(entries[0].journal.postings[2].amount.is_none());
    }

    #[test]
    fn parses_onchain_deposit() {
        let data = csv(&["2024-07-01 13:57:16,REF008,Deposit,,BTC,0.05000000,BTC,,,0.05000000,BTC,On Chain,,"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        assert_eq!(entries[0].journal.postings[0].amount, Some(Money::sat(5_000_000)));
        assert_eq!(entries[0].journal.postings[1].account, "income:unknown");
    }

    #[test]
    fn skips_ach_transfer() {
        let data = csv(&["2024-07-01 13:57:16,REF009,Bank Deposit,500.00,USD,,,,,500.00,USD,ACH Bank Transfer,,"]);
        assert!(parse(data.as_bytes(), ACCOUNT).unwrap().is_empty());
    }

    #[test]
    fn sorts_by_date() {
        let data = csv(&[
            "2024-07-02 00:00:00,REF010,Buy,10.00,USD,0.00010000,BTC,,,10.00,USD,Bitcoin Balance,,",
            "2024-07-01 00:00:00,REF011,Buy,5.00,USD,0.00005000,BTC,,,5.00,USD,Bitcoin Balance,,",
        ]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        assert_eq!(ref_code(&entries[0]), "REF011");
        assert_eq!(ref_code(&entries[1]), "REF010");
    }
}
