use std::io::Read;
use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;

use hledger_btc_core::journal::{JournalEntry, Posting, TagMap};
use hledger_btc_core::source::FeedEntry;

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

pub fn parse(reader: impl Read, account: &str) -> Result<Vec<FeedEntry>> {
    let mut reader = csv::Reader::from_reader(reader);

    let mut entries = Vec::new();
    for result in reader.deserialize::<PhoenixRow>() {
        let row = result.context("failed to parse CSV row")?;
        if let Some(entry) = row_to_entry(&row, account)? {
            entries.push(entry);
        }
    }

    entries.sort_by_key(|e| e.journal.date);
    Ok(entries)
}

fn row_to_entry(row: &PhoenixRow, account: &str) -> Result<Option<FeedEntry>> {
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
            FeedEntry::lightning(row.payment_hash.clone(), JournalEntry {
                date,
                description,
                tags: TagMap::new(),
                postings,
                status: Some(true),
            })
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
            FeedEntry::lightning(row.payment_hash.clone(), JournalEntry {
                date,
                description,
                tags: TagMap::new(),
                postings,
                status: Some(true),
            })
        }
        "swap_in" => {
            let amount_sat = row.amount_msat / 1000;
            let mut postings = vec![Posting::with_amount(account, amount_sat)];
            if row.mining_fee_sat > 0 {
                postings.push(Posting::with_amount("expenses:fees:onchain", row.mining_fee_sat));
            }
            postings.push(Posting::auto_balance("income:unknown"));
            FeedEntry::onchain(row.tx_id.clone(), JournalEntry {
                date,
                description: "Swap In".to_string(),
                tags: TagMap::new(),
                postings,
                status: Some(true),
            })
        }
        "swap_out" => {
            // amount_msat is the full debit from the lightning balance and
            // already includes the mining fee; the on-chain side receives
            // |amount| - mining_fee, which the auto-balance leg picks up.
            let amount_sat = row.amount_msat / 1000;
            let service_fee_sat = row.service_fee_msat / 1000;
            let mut postings = vec![Posting::with_amount(account, amount_sat)];
            if row.mining_fee_sat > 0 {
                postings.push(Posting::with_amount("expenses:fees:onchain", row.mining_fee_sat));
            }
            if service_fee_sat > 0 {
                postings.push(Posting::with_amount("expenses:fees:lightning", service_fee_sat));
            }
            postings.push(Posting::auto_balance("expenses:unknown"));
            FeedEntry::onchain(row.tx_id.clone(), JournalEntry {
                date,
                description: "Swap Out".to_string(),
                tags: TagMap::new(),
                postings,
                status: Some(true),
            })
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

#[cfg(test)]
mod tests {
    use super::*;
    use hledger_btc_core::journal::sum_commodity;
    use hledger_btc_core::money::Money;
    use hledger_btc_core::source::EntryKind;

    const HEADER: &str = "date,id,type,amount_msat,amount_fiat,fee_credit_msat,mining_fee_sat,mining_fee_fiat,service_fee_msat,service_fee_fiat,payment_hash,tx_id,destination,description";

    const ACCOUNT: &str = "assets:bitcoin:lightning:phoenix";

    fn csv(rows: &[&str]) -> String {
        format!("{HEADER}\n{}\n", rows.join("\n"))
    }

    #[test]
    fn parses_lightning_received_with_service_fee() {
        let data = csv(&["2026-05-01T10:00:00Z,1,lightning_received,100000000,10 USD,0,0,0 USD,1000000,0.1 USD,ph1,,dest,coffee refund"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert!(matches!(&e.kind, EntryKind::Lightning { payment_hash } if payment_hash == "ph1"));
        assert_eq!(e.journal.description, "coffee refund");
        // net into wallet = 100_000 sat gross - 1_000 sat fee
        assert_eq!(e.journal.postings[0].amount, Some(Money::sat(99_000)));
        assert_eq!(e.journal.postings[1].account, "expenses:fees:lightning");
        assert_eq!(e.journal.postings[1].amount, Some(Money::sat(1_000)));
        assert_eq!(e.journal.postings[2].account, "income:unknown");
    }

    #[test]
    fn parses_lightning_sent_default_description() {
        let data = csv(&["2026-05-02T09:00:00Z,2,lightning_sent,-50000000,-5 USD,0,0,0 USD,2000000,0.2 USD,ph2,,dest,"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        let e = &entries[0];
        assert!(matches!(&e.kind, EntryKind::Lightning { payment_hash } if payment_hash == "ph2"));
        assert_eq!(e.journal.description, "Lightning Sent");
        // total deducted = 50_000 payment + 2_000 fee
        assert_eq!(e.journal.postings[0].amount, Some(Money::sat(-52_000)));
        assert_eq!(e.journal.postings.last().unwrap().account, "expenses:unknown");
    }

    #[test]
    fn parses_swap_in_with_txid() {
        let data = csv(&["2026-05-03T08:00:00Z,3,swap_in,200000000,20 USD,0,500,0.05 USD,0,0 USD,,tx1,,"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        let e = &entries[0];
        assert!(matches!(&e.kind, EntryKind::OnChain { txid } if txid == "tx1"));
        assert_eq!(e.journal.description, "Swap In");
        assert_eq!(e.journal.postings[0].amount, Some(Money::sat(200_000)));
        assert_eq!(e.journal.postings[1].account, "expenses:fees:onchain");
        assert_eq!(e.journal.postings[1].amount, Some(Money::sat(500)));
    }

    #[test]
    fn parses_swap_out_fee_included_in_amount() {
        let data = csv(&["2026-06-12T08:00:00Z,5,swap_out,-100000000,-10 USD,0,500,0.05 USD,0,0 USD,,tx2,bc1qdest,"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        let e = &entries[0];
        assert!(matches!(&e.kind, EntryKind::OnChain { txid } if txid == "tx2"));
        assert_eq!(e.journal.description, "Swap Out");
        // full debit from lightning, fee included
        assert_eq!(e.journal.postings[0].amount, Some(Money::sat(-100_000)));
        assert_eq!(e.journal.postings[1].account, "expenses:fees:onchain");
        assert_eq!(e.journal.postings[1].amount, Some(Money::sat(500)));
        // auto-balance receives |amount| - fee = 99_500 on-chain
        let last = e.journal.postings.last().unwrap();
        assert_eq!(last.account, "expenses:unknown");
        assert!(last.amount.is_none());
        // explicit postings sum to -(on-chain received)
        assert_eq!(sum_commodity(&e.journal.postings, "SAT"), Money::sat(-99_500));
    }

    #[test]
    fn skips_unknown_payment_type() {
        let data = csv(&["2026-05-04T08:00:00Z,4,channel_close,1000,0 USD,0,0,0 USD,0,0 USD,,,,"]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn sorts_entries_by_date() {
        let data = csv(&[
            "2026-05-02T09:00:00Z,2,lightning_sent,-50000000,-5 USD,0,0,0 USD,0,0 USD,ph2,,dest,",
            "2026-05-01T10:00:00Z,1,lightning_received,100000000,10 USD,0,0,0 USD,0,0 USD,ph1,,dest,",
        ]);
        let entries = parse(data.as_bytes(), ACCOUNT).unwrap();
        assert!(matches!(&entries[0].kind, EntryKind::Lightning { payment_hash } if payment_hash == "ph1"));
        assert!(matches!(&entries[1].kind, EntryKind::Lightning { payment_hash } if payment_hash == "ph2"));
    }
}
