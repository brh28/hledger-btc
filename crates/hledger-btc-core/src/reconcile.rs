use std::collections::HashSet;

use crate::journal::{format_posting, Posting, DEDUP_KEYS};
use crate::source::Notice;
use crate::text::{append_tag_if_absent, extract_tag};

pub struct ReconcileResult {
    pub applied: Vec<Notice>,
    pub conflicts: Vec<Notice>,
}

/// Amends-in-place journal entries that have matching reconcile notices.
///
/// For each notice: if the matching entry contains an `income:unknown` or
/// `expenses:unknown` auto-balance placeholder, that leg is removed and the
/// novel source's explicit postings are added in its place; novel source stamps
/// are appended to the header. If no placeholder exists (the entry was manually
/// edited), the entry is left unchanged and a conflict is reported.
pub fn reconcile(journal_content: &str, notices: &[Notice]) -> (String, ReconcileResult) {
    let mut applied = Vec::new();
    let mut conflicts = Vec::new();
    let trailing_nl = journal_content.ends_with('\n');

    let mut segments: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();

    for line in journal_content.lines() {
        if line.is_empty() {
            if !current.is_empty() {
                let (out, ap, co) = process_block(&current, notices);
                segments.push(out);
                if let Some(n) = ap { applied.push(n); }
                if let Some(n) = co { conflicts.push(n); }
                current.clear();
            }
            segments.push(String::new());
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        let (out, ap, co) = process_block(&current, notices);
        segments.push(out);
        if let Some(n) = ap { applied.push(n); }
        if let Some(n) = co { conflicts.push(n); }
    }

    let mut result = segments.join("\n");
    if trailing_nl { result.push('\n'); }

    (result, ReconcileResult { applied, conflicts })
}

fn process_block(lines: &[&str], notices: &[Notice]) -> (String, Option<Notice>, Option<Notice>) {
    let header = match lines.first() {
        Some(h) if !h.starts_with(' ') && !h.starts_with('\t') => *h,
        _ => return (lines.join("\n"), None, None),
    };

    let Some(notice) = find_matching_notice(header, notices) else {
        return (lines.join("\n"), None, None);
    };

    // Find the auto-balance placeholder leg, if any.
    let unknown_idx = lines.iter().enumerate().skip(1)
        .find(|(_, l)| is_unknown_leg(l))
        .map(|(i, _)| i);

    let Some(unknown_idx) = unknown_idx else {
        // Entry has been manually edited — report conflict, leave unchanged.
        return (lines.join("\n"), None, Some(notice.clone()));
    };

    let placeholder_account = {
        let t = lines[unknown_idx].trim();
        t.split("  ;").next().unwrap_or(t).trim_end()
    };

    // Collect the accounts already explicitly present in this block.
    let existing_accounts: HashSet<&str> = lines.iter().skip(1)
        .filter(|l| (l.starts_with(' ') || l.starts_with('\t')) && !is_unknown_leg(l))
        .filter_map(|l| {
            let t = l.trim();
            let end = t.find("  ").unwrap_or(t.len());
            if end == 0 { None } else { Some(t[..end].trim_end()) }
        })
        .collect();

    // Novel postings: explicit postings in the incoming entry whose account
    // is not already present in the journal block.
    let novel_postings: Vec<&Posting> = notice.entry.postings.iter()
        .filter(|p| p.amount.is_some())
        .filter(|p| !existing_accounts.contains(p.account.as_str()))
        .collect();

    // Incoming auto-balance leg: include if it differs from the placeholder
    // being removed. This covers residual imbalance — e.g. a Coinbase withdrawal
    // debits 100,000 SAT but the wallet only receives 99,500 SAT; the 500 SAT
    // fee leaves the merged entry needing an `expenses:unknown` auto-balance even
    // after the original `income:unknown` placeholder is removed.
    let incoming_auto_balance: Option<&Posting> = notice.entry.postings.iter()
        .find(|p| p.amount.is_none() && p.account != placeholder_account);

    let mut new_lines: Vec<String> = Vec::new();

    // Header with novel source stamps.
    let mut new_header = header.to_string();
    for source in &notice.novel_sources {
        new_header = append_tag_if_absent(&new_header, "source", source);
    }
    new_lines.push(new_header);

    // Existing posting lines, minus the placeholder.
    for (i, line) in lines.iter().enumerate().skip(1) {
        if i != unknown_idx {
            new_lines.push(line.to_string());
        }
    }

    // Novel explicit postings from the incoming entry.
    for p in novel_postings {
        new_lines.push(format_posting(p));
    }

    // Residual auto-balance from incoming entry (different account from removed placeholder).
    if let Some(ab) = incoming_auto_balance {
        new_lines.push(format_posting(ab));
    }

    (new_lines.join("\n"), Some(notice.clone()), None)
}

fn find_matching_notice(header: &str, notices: &[Notice]) -> Option<Notice> {
    for key in DEDUP_KEYS {
        if let Some(val) = extract_tag(header, key) {
            if let Some(n) = notices.iter().find(|n| n.key == *key && n.value == val) {
                return Some(n.clone());
            }
        }
    }
    None
}

fn is_unknown_leg(line: &str) -> bool {
    if !line.starts_with(' ') && !line.starts_with('\t') { return false; }
    let t = line.trim();
    // Match the bare account name or the account followed by a tag comment
    // (e.g. `expenses:unknown  ; address:bc1q...`).
    let account = t.split("  ;").next().unwrap_or(t).trim_end();
    account == "income:unknown" || account == "expenses:unknown"
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{JournalEntry, TagMap};
    use chrono::NaiveDate;

    const TXID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 6, 12).unwrap()
    }

    fn make_notice(key: &str, value: &str, novel: &[&str], recorded: &[&str], entry: JournalEntry) -> Notice {
        Notice {
            key: key.to_string(),
            value: value.to_string(),
            novel_sources: novel.iter().map(|s| s.to_string()).collect(),
            recorded_sources: recorded.iter().map(|s| s.to_string()).collect(),
            entry,
        }
    }

    fn swap_out_notice() -> Notice {
        // Merged entry: electrum saw the spend, phoenix saw the lightning credit.
        let entry = JournalEntry {
            date: date(),
            description: "Transfer".to_string(),
            tags: TagMap::new()
                .add("txid", TXID)
                .add("source", "electrum")
                .add("source", "phoenix"),
            postings: vec![
                Posting::with_amount(format!("assets:bitcoin:savings:addr1"), -100_000),
                Posting::with_amount("expenses:fees:onchain", 500),
                Posting::with_amount("assets:bitcoin:lightning:phoenix", 99_500),
            ],
            status: Some(true),
        };
        make_notice("txid", TXID, &["phoenix"], &["electrum"], entry)
    }

    fn existing_journal() -> String {
        format!(
            "2026-06-12 * Outgoing BTC  ; txid:{TXID}, source:electrum\n\
             \tassets:bitcoin:savings:addr1    -100000 SAT  ; input:0\n\
             \texpenses:fees:onchain    500 SAT\n\
             \texpenses:unknown\n"
        )
    }

    #[test]
    fn replaces_placeholder_with_novel_posting() {
        let (out, result) = reconcile(&existing_journal(), &[swap_out_notice()]);
        assert_eq!(result.applied.len(), 1);
        assert!(result.conflicts.is_empty());
        assert!(!out.contains("expenses:unknown"));
        assert!(out.contains("assets:bitcoin:lightning:phoenix"));
        assert!(out.contains("99,500 sat"));
    }

    #[test]
    fn stamps_novel_source_on_header() {
        let (out, _) = reconcile(&existing_journal(), &[swap_out_notice()]);
        assert!(out.contains("source:electrum, source:phoenix") || out.contains("source:phoenix"));
        // idempotent: electrum not doubled
        assert_eq!(out.matches("source:electrum").count(), 1);
    }

    #[test]
    fn preserves_existing_postings_and_inline_tags() {
        let (out, _) = reconcile(&existing_journal(), &[swap_out_notice()]);
        assert!(out.contains("expenses:fees:onchain"));
        assert!(out.contains("input:0"));
    }

    #[test]
    fn conflict_when_no_placeholder() {
        let manually_edited = format!(
            "2026-06-12 * Outgoing BTC  ; txid:{TXID}, source:electrum\n\
             \tassets:bitcoin:savings:addr1    -100000 SAT  ; input:0\n\
             \texpenses:fees:onchain    500 SAT\n\
             \tassets:bitcoin:lightning:phoenix    99500 SAT\n"
        );
        let (out, result) = reconcile(&manually_edited, &[swap_out_notice()]);
        assert!(result.applied.is_empty());
        assert_eq!(result.conflicts.len(), 1);
        assert_eq!(out, manually_edited);
    }

    #[test]
    fn unmatched_entry_unchanged() {
        let other = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let journal = format!(
            "2026-06-12 * Outgoing BTC  ; txid:{other}, source:electrum\n\
             \tassets:bitcoin:savings:addr1    -50000 SAT\n\
             \texpenses:unknown\n"
        );
        let (out, result) = reconcile(&journal, &[swap_out_notice()]);
        assert!(result.applied.is_empty());
        assert!(result.conflicts.is_empty());
        assert_eq!(out, journal);
    }

    #[test]
    fn income_unknown_replaced() {
        let journal = format!(
            "2026-06-12 * Incoming BTC  ; txid:{TXID}, source:electrum\n\
             \tassets:bitcoin:wallet:addr1    50000 SAT  ; vout:0\n\
             \tincome:unknown\n"
        );
        let incoming = JournalEntry {
            date: date(),
            description: "Transfer".to_string(),
            tags: TagMap::new()
                .add("txid", TXID)
                .add("source", "electrum")
                .add("source", "coinbase"),
            postings: vec![
                Posting::with_amount("assets:bitcoin:wallet:addr1", 50_000),
                Posting::with_amount("assets:coinbase", -50_000),
            ],
            status: Some(true),
        };
        let notice = make_notice("txid", TXID, &["coinbase"], &["electrum"], incoming);
        let (out, result) = reconcile(&journal, &[notice]);
        assert_eq!(result.applied.len(), 1);
        assert!(!out.contains("income:unknown"));
        assert!(out.contains("assets:coinbase"));
    }

    #[test]
    fn multiple_entries_only_matching_one_updated() {
        let other = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let journal = format!(
            "2026-06-12 * Outgoing BTC  ; txid:{TXID}, source:electrum\n\
             \tassets:bitcoin:savings:addr1    -100000 SAT\n\
             \texpenses:unknown\n\
             \n\
             2026-06-13 * Incoming BTC  ; txid:{other}, source:electrum\n\
             \tassets:bitcoin:wallet:addr2    50000 SAT\n\
             \tincome:unknown\n"
        );
        let (out, result) = reconcile(&journal, &[swap_out_notice()]);
        assert_eq!(result.applied.len(), 1);
        assert!(!out.contains(&format!("txid:{TXID}, source:electrum\n")) ||
                 out.contains("source:phoenix"));
        // second entry untouched
        assert!(out.contains("income:unknown"));
    }

    #[test]
    fn preserves_trailing_newline() {
        let journal = existing_journal();
        assert!(journal.ends_with('\n'));
        let (out, _) = reconcile(&journal, &[swap_out_notice()]);
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn withdrawal_fee_residual_auto_balance_included() {
        // Coinbase withdraws 100,000 SAT; wallet receives 99,500 SAT (500 SAT fee).
        // Electrum saw only the receive: assets:bitcoin:wallet:addr +99,500 + income:unknown.
        // Coinbase import notice has: assets:coinbase:btc -100,000 + expenses:unknown (residual).
        // After reconcile: income:unknown removed, coinbase posting added, expenses:unknown kept.
        let journal = format!(
            "2026-06-12 * Incoming BTC  ; txid:{TXID}, source:electrum\n\
             \tassets:bitcoin:wallet:addr    99500 SAT  ; vout:0\n\
             \tincome:unknown\n"
        );
        let incoming = JournalEntry {
            date: date(),
            description: "Withdraw".to_string(),
            tags: TagMap::new()
                .add("txid", TXID)
                .add("source", "electrum")
                .add("source", "coinbase"),
            postings: vec![
                Posting::with_amount("assets:coinbase:btc", -100_000),
                Posting::auto_balance("expenses:unknown"),
            ],
            status: Some(true),
        };
        let notice = make_notice("txid", TXID, &["coinbase"], &["electrum"], incoming);
        let (out, result) = reconcile(&journal, &[notice]);
        assert_eq!(result.applied.len(), 1);
        assert!(!out.contains("income:unknown"), "income:unknown placeholder should be removed");
        assert!(out.contains("assets:coinbase:btc"), "coinbase posting should be added");
        assert!(out.contains("expenses:unknown"), "fee residual auto-balance should be kept");
    }

    #[test]
    fn source_stamp_idempotent_if_already_present() {
        let already_stamped = format!(
            "2026-06-12 * Outgoing BTC  ; txid:{TXID}, source:electrum, source:phoenix\n\
             \tassets:bitcoin:savings:addr1    -100000 SAT\n\
             \texpenses:unknown\n"
        );
        let (out, _) = reconcile(&already_stamped, &[swap_out_notice()]);
        assert_eq!(out.matches("source:phoenix").count(), 1);
    }
}
