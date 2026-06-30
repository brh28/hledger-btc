use bigdecimal::ToPrimitive;
use chrono::NaiveDate;

use crate::journal::JournalEntry;
use crate::text::{append_tag_if_absent, extract_tag};

pub struct Receivable {
    pub address: String,
    pub description: String,
    pub extra_tags: Vec<(String, String)>,
    pub credit_account: String,
    pub expected_sat: Option<i64>,
}

/// Parses pending (`!`) entries with an `address:` tag and no `settled:` tag.
pub fn parse_open(text: &str) -> Vec<Receivable> {
    let mut result = Vec::new();
    let mut current: Vec<&str> = Vec::new();

    for line in text.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if !current.is_empty() {
                if let Some(rcv) = try_parse_receivable(&current) {
                    result.push(rcv);
                }
                current.clear();
            }
        } else {
            current.push(line);
        }
    }
    result
}

fn try_parse_receivable(lines: &[&str]) -> Option<Receivable> {
    let header = lines.first()?;
    if !is_pending(header) {
        return None;
    }
    let address = extract_tag(header, "address")?;
    if extract_tag(header, "settled").is_some() {
        return None;
    }

    let description = parse_description(header);
    let all_tags = parse_header_tags(header);
    let extra_tags: Vec<(String, String)> = all_tags.into_iter()
        .filter(|(k, _)| k != "address" && k != "expected")
        .collect();
    let expected_sat = extract_tag(header, "expected")
        .and_then(|v| v.parse::<i64>().ok());
    let credit_account = lines.iter().skip(1)
        .find(|l| is_auto_balance(l))
        .map(|l| l.trim().to_string())
        .unwrap_or_else(|| "income:unknown".to_string());

    Some(Receivable { address, description, extra_tags, credit_account, expected_sat })
}

fn is_pending(line: &str) -> bool {
    let b = line.as_bytes();
    b.len() > 12 && b[10] == b' ' && b[11] == b'!' && b[12] == b' '
}

fn parse_description(header: &str) -> String {
    // "YYYY-MM-DD ! Description  ; tags" — skip 13 chars (date + " ! ")
    if header.len() <= 13 { return String::new(); }
    let rest = &header[13..];
    let end = rest.find("  ;").unwrap_or(rest.len());
    rest[..end].trim().to_string()
}

fn parse_header_tags(header: &str) -> Vec<(String, String)> {
    let Some(pos) = header.find("  ; ") else { return vec![]; };
    let comment = &header[pos + 4..];
    comment.split(", ")
        .filter_map(|pair| pair.split_once(':'))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn is_auto_balance(line: &str) -> bool {
    if !line.starts_with(' ') && !line.starts_with('\t') { return false; }
    !line.trim().contains("  ")
}

/// Carries forward receivable description/tags and replaces `income:unknown`
/// with the credit account. Returns an amount-mismatch notice string if the
/// received amount differs from `expected_sat`.
pub fn apply(entry: &mut JournalEntry, rcv: &Receivable) -> Option<String> {
    entry.description = rcv.description.clone();
    for (k, v) in &rcv.extra_tags {
        entry.tags.push(k, v);
    }
    for posting in &mut entry.postings {
        if posting.amount.is_none() && posting.account == "income:unknown" {
            posting.account = rcv.credit_account.clone();
        }
    }
    if let Some(expected) = rcv.expected_sat {
        if let Some(actual) = actual_sat_for_address(entry, &rcv.address) {
            if actual != expected {
                return Some(format!(
                    "amount mismatch for {}: expected {expected} sat, received {actual} sat",
                    rcv.address
                ));
            }
        }
    }
    None
}

fn actual_sat_for_address(entry: &JournalEntry, address: &str) -> Option<i64> {
    let suffix = format!(":{address}");
    entry.postings.iter()
        .find(|p| p.account.ends_with(&suffix))
        .and_then(|p| p.amount.as_ref())
        .filter(|m| m.commodity == "SAT")
        .and_then(|m| m.amount.to_i64())
}

/// Rewrites `content` to flip `!` → `*` and stamp `settled:<date>` on the
/// pending entry whose `address:` tag matches `address`.
pub fn mark_settled(content: &str, address: &str, date: NaiveDate) -> String {
    let date_str = date.to_string();
    let trailing_nl = content.ends_with('\n');
    let mut result = String::with_capacity(content.len());

    for line in content.lines() {
        let processed = if is_pending(line)
            && extract_tag(line, "address").as_deref() == Some(address)
            && extract_tag(line, "settled").is_none()
        {
            let cleared = line.replacen(" ! ", " * ", 1);
            append_tag_if_absent(&cleared, "settled", &date_str)
        } else {
            line.to_string()
        };
        result.push_str(&processed);
        result.push('\n');
    }

    if !trailing_nl {
        result.pop();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Posting, TagMap};
    use chrono::NaiveDate;

    fn date() -> NaiveDate { NaiveDate::from_ymd_opt(2026, 6, 30).unwrap() }

    fn incoming_entry(address: &str, amount_sat: i64) -> JournalEntry {
        JournalEntry {
            date: date(),
            description: "Incoming BTC".to_string(),
            tags: TagMap::new().add("address", address),
            postings: vec![
                Posting::with_amount(format!("assets:bitcoin:wallet:{address}"), amount_sat),
                Posting::auto_balance("income:unknown"),
            ],
            status: Some(true),
        }
    }

    #[test]
    fn parse_open_finds_pending_receivable() {
        let text = "\
2026-06-30 ! Awaiting Payment  ; address:bc1ptest, expected:1000
    assets:bitcoin:receivable:bc1ptest    0 sat
    income:sales

2026-06-30 * Cleared  ; address:bc1pother
    assets:bitcoin:wallet:bc1pother    500 sat
    income:unknown
";
        let open = parse_open(text);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].address, "bc1ptest");
        assert_eq!(open[0].description, "Awaiting Payment");
        assert_eq!(open[0].credit_account, "income:sales");
        assert_eq!(open[0].expected_sat, Some(1000));
    }

    #[test]
    fn parse_open_excludes_settled() {
        let text = "2026-06-30 ! Settled  ; address:bc1ptest, settled:2026-06-30\n    assets:bitcoin:receivable:bc1ptest    0 sat\n    income:unknown\n";
        assert!(parse_open(text).is_empty());
    }

    #[test]
    fn parse_open_collects_extra_tags() {
        let text = "2026-06-30 ! Invoice  ; address:bc1ptest, client:alice, project:x\n    assets:bitcoin:receivable:bc1ptest    0 sat\n    income:consulting\n";
        let open = parse_open(text);
        assert_eq!(open.len(), 1);
        assert!(open[0].extra_tags.iter().any(|(k, v)| k == "client" && v == "alice"));
        assert!(open[0].extra_tags.iter().any(|(k, v)| k == "project" && v == "x"));
        assert!(!open[0].extra_tags.iter().any(|(k, _)| k == "address"));
    }

    #[test]
    fn apply_carries_description_and_tags() {
        let rcv = Receivable {
            address: "bc1ptest".to_string(),
            description: "Invoice #42".to_string(),
            extra_tags: vec![("client".to_string(), "alice".to_string())],
            credit_account: "income:consulting".to_string(),
            expected_sat: None,
        };
        let mut entry = incoming_entry("bc1ptest", 1000);
        apply(&mut entry, &rcv);
        assert_eq!(entry.description, "Invoice #42");
        assert_eq!(entry.tags.get("client"), Some("alice"));
    }

    #[test]
    fn apply_replaces_income_unknown() {
        let rcv = Receivable {
            address: "bc1ptest".to_string(),
            description: "Payment".to_string(),
            extra_tags: vec![],
            credit_account: "income:sales".to_string(),
            expected_sat: None,
        };
        let mut entry = incoming_entry("bc1ptest", 1000);
        apply(&mut entry, &rcv);
        assert!(entry.postings.iter().any(|p| p.account == "income:sales"));
        assert!(!entry.postings.iter().any(|p| p.account == "income:unknown"));
    }

    #[test]
    fn apply_returns_mismatch_notice() {
        let rcv = Receivable {
            address: "bc1ptest".to_string(),
            description: "Payment".to_string(),
            extra_tags: vec![],
            credit_account: "income:sales".to_string(),
            expected_sat: Some(2000),
        };
        let mut entry = incoming_entry("bc1ptest", 1000);
        let notice = apply(&mut entry, &rcv);
        assert!(notice.is_some());
        assert!(notice.unwrap().contains("2000 sat"));
    }

    #[test]
    fn apply_no_notice_when_amount_matches() {
        let rcv = Receivable {
            address: "bc1ptest".to_string(),
            description: "Payment".to_string(),
            extra_tags: vec![],
            credit_account: "income:sales".to_string(),
            expected_sat: Some(1000),
        };
        let mut entry = incoming_entry("bc1ptest", 1000);
        assert!(apply(&mut entry, &rcv).is_none());
    }

    #[test]
    fn mark_settled_flips_pending_and_stamps_date() {
        let content = "2026-06-30 ! Awaiting Payment  ; address:bc1ptest\n    assets:bitcoin:receivable:bc1ptest    0 sat\n    income:unknown\n";
        let out = mark_settled(content, "bc1ptest", date());
        assert!(out.contains(" * "));
        assert!(!out.contains(" ! "));
        assert!(out.contains("settled:2026-06-30"));
    }

    #[test]
    fn mark_settled_leaves_other_entries_unchanged() {
        let content = "\
2026-06-30 ! Awaiting Payment  ; address:bc1ptest
    assets:bitcoin:receivable:bc1ptest    0 sat
    income:unknown

2026-06-29 ! Other  ; address:bc1pother
    assets:bitcoin:receivable:bc1pother    0 sat
    income:unknown
";
        let out = mark_settled(content, "bc1ptest", date());
        assert!(out.contains("bc1ptest") && out.contains("settled:2026-06-30"));
        // Other receivable unchanged
        assert!(out.contains("2026-06-29 ! Other"));
    }

    #[test]
    fn mark_settled_idempotent() {
        let content = "2026-06-30 * Awaiting Payment  ; address:bc1ptest, settled:2026-06-30\n    assets:bitcoin:receivable:bc1ptest    0 sat\n    income:unknown\n";
        // Already cleared — mark_settled should not match (no "!")
        let out = mark_settled(content, "bc1ptest", date());
        assert_eq!(out, content);
    }
}
