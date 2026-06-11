use std::path::Path;
use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;

use hledger_btc_core::journal::{JournalEntry, Posting, TagMap};

#[derive(Debug, Deserialize)]
struct PhoenixRow {
    date: String,
    #[allow(dead_code)]
    id: String,
    #[serde(rename = "type")]
    payment_type: String,
    amount_msat: i64,
    #[allow(dead_code)]
    amount_fiat: String,
    #[allow(dead_code)]
    fee_credit_msat: i64,
    mining_fee_sat: i64,
    #[allow(dead_code)]
    mining_fee_fiat: String,
    service_fee_msat: i64,
    #[allow(dead_code)]
    service_fee_fiat: String,
    payment_hash: String,
    tx_id: String,
    #[allow(dead_code)]
    destination: String,
    description: String,
}

pub fn import(path: &Path, account: &str) -> Result<Vec<JournalEntry>> {
    let mut reader = csv::Reader::from_path(path)
        .with_context(|| format!("failed to open {}", path.display()))?;

    let mut entries = Vec::new();
    for result in reader.deserialize::<PhoenixRow>() {
        let row = result.context("failed to parse CSV row")?;
        if let Some(entry) = row_to_entry(&row, account)? {
            entries.push(entry);
        }
    }

    entries.sort_by_key(|e| e.date);
    Ok(entries)
}

fn row_to_entry(row: &PhoenixRow, account: &str) -> Result<Option<JournalEntry>> {
    let date = NaiveDate::parse_from_str(&row.date[..10], "%Y-%m-%d")
        .with_context(|| format!("invalid date: {}", row.date))?;

    let entry = match row.payment_type.as_str() {
        "lightning_received" => {
            let amount_sat = row.amount_msat / 1000;
            let service_fee_sat = row.service_fee_msat / 1000;
            let description = non_empty(&row.description)
                .unwrap_or("Lightning Received")
                .to_string();
            // Net into wallet = gross amount minus any fee Phoenix deducted
            let mut postings = vec![Posting::with_amount(account, amount_sat - service_fee_sat)];
            if service_fee_sat > 0 {
                postings.push(Posting::with_amount("expenses:fees:lightning", service_fee_sat));
            }
            postings.push(Posting::auto_balance("income:unknown"));
            JournalEntry {
                date,
                description,
                tags: TagMap::new().add("payment_hash", &row.payment_hash),
                postings,
            }
        }
        "lightning_sent" => {
            let amount_sat = row.amount_msat / 1000; // negative
            let service_fee_sat = row.service_fee_msat / 1000;
            let description = non_empty(&row.description)
                .unwrap_or("Lightning Sent")
                .to_string();
            // Total deducted from wallet = payment amount + fee
            let mut postings =
                vec![Posting::with_amount(account, amount_sat - service_fee_sat)];
            if service_fee_sat > 0 {
                postings.push(Posting::with_amount("expenses:fees:lightning", service_fee_sat));
            }
            postings.push(Posting::auto_balance("expenses:unknown"));
            JournalEntry {
                date,
                description,
                tags: TagMap::new().add("payment_hash", &row.payment_hash),
                postings,
            }
        }
        "swap_in" => {
            let amount_sat = row.amount_msat / 1000;
            let mut postings = vec![Posting::with_amount(account, amount_sat)];
            if row.mining_fee_sat > 0 {
                postings.push(Posting::with_amount("expenses:fees:onchain", row.mining_fee_sat));
            }
            postings.push(Posting::auto_balance("assets:bitcoin"));
            JournalEntry {
                date,
                description: "Swap In".to_string(),
                tags: TagMap::new().add("txid", &row.tx_id),
                postings,
            }
        }
        other => {
            tracing::warn!("unknown phoenix payment type: {other}, skipping");
            return Ok(None);
        }
    };

    Ok(Some(entry))
}

fn non_empty(s: &str) -> Option<&str> {
    if s.is_empty() { None } else { Some(s) }
}
