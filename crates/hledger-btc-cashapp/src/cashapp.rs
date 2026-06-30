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
    #[serde(rename = "Transaction ID")]
    transaction_id: String,
    #[serde(rename = "Transaction Type")]
    transaction_type: String,
    #[serde(rename = "Amount")]
    amount: String,
    #[serde(rename = "Fee")]
    fee: String,
    #[serde(rename = "Asset Amount")]
    asset_amount: String,
    #[serde(rename = "Status")]
    status: String,
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
    if row.status != "COMPLETE" {
        return Ok(None);
    }

    let date = NaiveDate::parse_from_str(&row.date[..10], "%Y-%m-%d")
        .with_context(|| format!("invalid date: '{}'", row.date))?;

    match row.transaction_type.as_str() {
        "Bitcoin Buy"        => buy_entry(row, date, account).map(Some),
        "Bitcoin Sell"       => sell_entry(row, date, account).map(Some),
        "Bitcoin Deposit"    => deposit_entry(row, date, account).map(Some),
        "Bitcoin Withdrawal" => withdrawal_entry(row, date, account).map(Some),
        _ => Ok(None),
    }
}

// Amount = BTC cost (negative); Fee = CashApp fee (negative); Net Amount = Amount + Fee.
// Cost basis on the BTC leg is |Amount|; fee is posted separately.
fn buy_entry(row: &Row, date: NaiveDate, account: &str) -> Result<FeedEntry> {
    let sat = btc_to_sat(row.asset_amount.trim())?;
    let cost_usd = parse_usd(row.amount.trim())?.abs();
    let fee_usd = parse_usd(row.fee.trim())?.abs();

    let price = (cost_usd > 0.0).then(|| PriceAnnotation::Total(format!("{cost_usd:.2} USD")));

    Ok(FeedEntry::provider("cashapp_id", dedup_id(row), JournalEntry {
        date,
        description: "Bitcoin Buy".to_string(),
        tags: TagMap::new(),
        postings: vec![
            Posting::with_amount(format!("{account}:btc"), sat).with_price(price),
            Posting::with_money("expenses:fees:cashapp", Money::parse(&format!("{fee_usd:.2}"), "USD")?),
            Posting::auto_balance(format!("{account}:usd")),
        ],
    }))
}

// Amount = gross proceeds (positive); Fee = fee taken (negative).
// Gross proceeds on the BTC leg is |Amount|; fee posted separately.
fn sell_entry(row: &Row, date: NaiveDate, account: &str) -> Result<FeedEntry> {
    let sat = -btc_to_sat(row.asset_amount.trim())?.abs();
    let gross_usd = parse_usd(row.amount.trim())?.abs();
    let fee_usd = parse_usd(row.fee.trim())?.abs();

    let price = (gross_usd > 0.0).then(|| PriceAnnotation::Total(format!("{gross_usd:.2} USD")));

    Ok(FeedEntry::provider("cashapp_id", dedup_id(row), JournalEntry {
        date,
        description: "Bitcoin Sell".to_string(),
        tags: TagMap::new(),
        postings: vec![
            Posting::with_amount(format!("{account}:btc"), sat).with_price(price),
            Posting::with_money("expenses:fees:cashapp", Money::parse(&format!("{fee_usd:.2}"), "USD")?),
            Posting::auto_balance(format!("{account}:usd")),
        ],
    }))
}

fn deposit_entry(row: &Row, date: NaiveDate, account: &str) -> Result<FeedEntry> {
    let sat = btc_to_sat(row.asset_amount.trim())?.abs();
    Ok(FeedEntry::provider("cashapp_id", dedup_id(row), JournalEntry {
        date,
        description: "Bitcoin Deposit".to_string(),
        tags: TagMap::new(),
        postings: vec![
            Posting::with_amount(format!("{account}:btc"), sat),
            Posting::auto_balance("income:unknown"),
        ],
    }))
}

fn withdrawal_entry(row: &Row, date: NaiveDate, account: &str) -> Result<FeedEntry> {
    let sat = -btc_to_sat(row.asset_amount.trim())?.abs();
    let fee_usd = parse_usd(row.fee.trim())?.abs();

    let mut postings = vec![Posting::with_amount(format!("{account}:btc"), sat)];
    if fee_usd > 0.0 {
        postings.push(Posting::with_money(
            "expenses:fees:cashapp",
            Money::parse(&format!("{fee_usd:.2}"), "USD")?,
        ));
    }
    postings.push(Posting::auto_balance("expenses:unknown"));

    Ok(FeedEntry::provider("cashapp_id", dedup_id(row), JournalEntry {
        date,
        description: "Bitcoin Withdrawal".to_string(),
        tags: TagMap::new(),
        postings,
    }))
}

// CashApp rarely populates Transaction ID; fall back to a content-derived key.
// Components are stripped of spaces and commas because the journal tag parser
// (extract_all in source.rs) splits values at those characters.
fn dedup_id(row: &Row) -> String {
    let id = row.transaction_id.trim();
    if !id.is_empty() {
        return id.to_string();
    }
    let clean = |s: &str| s.replace([' ', ',', '$'], "");
    format!("{}|{}|{}|{}",
        clean(&row.date),
        clean(&row.transaction_type),
        clean(&row.amount),
        clean(&row.asset_amount),
    )
}

fn parse_usd(s: &str) -> Result<f64> {
    let s = s.replace(',', "").replace('$', "");
    if s.is_empty() {
        return Ok(0.0);
    }
    s.parse::<f64>().with_context(|| format!("invalid USD amount: '{s}'"))
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

    const ACCOUNT: &str = "assets:cashapp";
    const HEADER: &str = "Date,Transaction ID,Transaction Type,Currency,Amount,Fee,Net Amount,Asset Type,Asset Price,Asset Amount,Status,Notes,Name of sender/receiver,Account";

    fn csv(rows: &[&str]) -> String {
        format!("{HEADER}\n{}\n", rows.join("\n"))
    }

    #[test]
    fn parses_buy() {
        let data = csv(&["2025-06-20 15:11:20 CDT,,Bitcoin Buy,USD,-$0.75,-$0.25,-$1.00,BTC,\"$104,528.58\",0.00000718,COMPLETE,purchase of BTC 0.00000718,,Cash Balance"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert!(matches!(&e.kind, EntryKind::Provider { key, .. } if *key == "cashapp_id"));
        assert_eq!(e.journal.description, "Bitcoin Buy");
        assert_eq!(e.journal.postings[0].amount, Some(Money::sat(718)));
        assert_eq!(e.journal.postings[1].account, "expenses:fees:cashapp");
        assert!(e.journal.postings[2].amount.is_none());
    }

    #[test]
    fn buy_price_annotation_uses_amount_not_net() {
        let data = csv(&["2025-06-20 15:11:20 CDT,,Bitcoin Buy,USD,-$0.75,-$0.25,-$1.00,BTC,\"$104,528.58\",0.00000718,COMPLETE,,,Cash Balance"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        let price = entries[0].journal.postings[0].price.as_ref().unwrap();
        assert!(matches!(price, PriceAnnotation::Total(s) if s == "0.75 USD"));
    }

    #[test]
    fn parses_sell() {
        let data = csv(&["2025-02-10 12:00:00 CDT,,Bitcoin Sell,USD,$9.80,-$0.20,$9.60,BTC,\"$100,000.00\",0.00010000,COMPLETE,,,Cash Balance"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        assert_eq!(entries[0].journal.postings[0].amount, Some(Money::sat(-10_000)));
        assert_eq!(entries[0].journal.postings[1].account, "expenses:fees:cashapp");
        assert!(entries[0].journal.postings[2].amount.is_none());
    }

    #[test]
    fn parses_deposit() {
        let data = csv(&["2025-03-01 08:00:00 CDT,,Bitcoin Deposit,BTC,,,,$BTC,,0.00050000,COMPLETE,,,Personal"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        assert_eq!(entries[0].journal.postings[0].amount, Some(Money::sat(50_000)));
        assert_eq!(entries[0].journal.postings[1].account, "income:unknown");
    }

    #[test]
    fn parses_withdrawal_no_fee() {
        let data = csv(&["2025-04-01 15:00:00 CDT,,Bitcoin Withdrawal,BTC,,,,$BTC,,0.00100000,COMPLETE,,,Personal"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        assert_eq!(entries[0].journal.postings[0].amount, Some(Money::sat(-100_000)));
        assert_eq!(entries[0].journal.postings.last().unwrap().account, "expenses:unknown");
        assert_eq!(entries[0].journal.postings.len(), 2); // no fee posting
    }

    #[test]
    fn uses_transaction_id_when_present() {
        let data = csv(&["2025-01-21 07:09:01 CDT,CASHID123,Bitcoin Buy,USD,-$0.75,-$0.25,-$1.00,BTC,\"$104,528.58\",0.00000718,COMPLETE,,,Cash Balance"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        assert!(matches!(&entries[0].kind, EntryKind::Provider { id, .. } if id == "CASHID123"));
    }

    #[test]
    fn synthesizes_dedup_id_when_no_transaction_id() {
        let data = csv(&["2025-01-21 07:09:01 CDT,,Bitcoin Buy,USD,-$0.75,-$0.25,-$1.00,BTC,\"$104,528.58\",0.00000718,COMPLETE,,,Cash Balance"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        let id = match &entries[0].kind {
            EntryKind::Provider { id, .. } => id.clone(),
            _ => panic!("expected internal"),
        };
        assert_eq!(id, "2025-01-2107:09:01CDT|BitcoinBuy|-0.75|0.00000718");
    }

    #[test]
    fn skips_non_bitcoin_types() {
        let data = csv(&[
            "2025-01-01 00:00:00 CDT,,P2P,USD,$20.00,$0.00,$20.00,,,0,COMPLETE,,,personal",
            "2025-01-01 00:00:00 CDT,,Cash Card,USD,-$5.00,$0.00,-$5.00,,,0,COMPLETE,,,personal",
        ]);
        assert!(parse(data.as_bytes(), ACCOUNT).unwrap().is_empty());
    }

    #[test]
    fn skips_non_complete() {
        let data = csv(&["2025-01-21 07:09:01 CDT,,Bitcoin Buy,USD,-$0.75,-$0.25,-$1.00,BTC,\"$104,528.58\",0.00000718,Pending,,,Cash Balance"]);
        assert!(parse(data.as_bytes(), ACCOUNT).unwrap().is_empty());
    }

    #[test]
    fn sorts_by_date() {
        let data = csv(&[
            "2025-02-01 00:00:00 CDT,,Bitcoin Buy,USD,-$2.00,-$0.50,-$2.50,BTC,\"$100,000.00\",0.00002000,COMPLETE,,,Cash Balance",
            "2025-01-01 00:00:00 CDT,,Bitcoin Buy,USD,-$1.00,-$0.25,-$1.25,BTC,\"$100,000.00\",0.00001000,COMPLETE,,,Cash Balance",
        ]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        assert_eq!(entries[0].journal.date, NaiveDate::from_ymd_opt(2025, 1, 1).unwrap());
        assert_eq!(entries[1].journal.date, NaiveDate::from_ymd_opt(2025, 2, 1).unwrap());
    }
}
