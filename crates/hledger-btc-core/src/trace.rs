use std::collections::{HashMap, HashSet, VecDeque};
use std::str::FromStr;
use chrono::NaiveDate;

/// Traces the visibility footprint of `start_address` through the journal.
/// Returns the raw hledger journal text blocks for all reachable entries, sorted by date.
pub fn trace(journal_content: &str, start_address: &str) -> Vec<String> {
    let raw = parse_journal(journal_content);
    let ordered_txids = breadth_first_search(&raw, start_address);
    let blocks = extract_raw_blocks(journal_content);
    ordered_txids.into_iter().filter_map(|txid| blocks.get(&txid).cloned()).collect()
}

// ── Graph traversal ───────────────────────────────────────────────────────────

struct RawEntry {
    date: NaiveDate,
    txid: String,
    addresses: Vec<String>,
}

// BFS chosen over DFS so that transactions close to the seed address are
// discovered before distant ones, producing output that is roughly chronological
// even before the final date sort.
fn breadth_first_search(entries: &[RawEntry], start_address: &str) -> Vec<String> {
    let mut txid_to_idx: HashMap<&str, usize> = HashMap::new();
    let mut addr_to_txids: HashMap<&str, Vec<&str>> = HashMap::new();

    for (i, entry) in entries.iter().enumerate() {
        txid_to_idx.insert(&entry.txid, i);
        for addr in &entry.addresses {
            addr_to_txids.entry(addr).or_default().push(&entry.txid);
        }
    }

    let mut visited_txids: HashSet<&str> = HashSet::new();
    let mut visited_addrs: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    let mut ordered: Vec<&str> = Vec::new();

    queue.push_back(start_address);

    while let Some(addr) = queue.pop_front() {
        if !visited_addrs.insert(addr) {
            continue;
        }
        for txid in addr_to_txids.get(addr).into_iter().flatten() {
            if !visited_txids.insert(txid) {
                continue;
            }
            ordered.push(txid);
            let idx = txid_to_idx[txid];
            for a in &entries[idx].addresses {
                if !visited_addrs.contains(a.as_str()) {
                    queue.push_back(a);
                }
            }
        }
    }

    ordered.sort_by_key(|txid| entries[txid_to_idx[txid]].date);
    ordered.into_iter().map(|s| s.to_string()).collect()
}

// ── Raw block extraction ──────────────────────────────────────────────────────

fn extract_raw_blocks(content: &str) -> HashMap<String, String> {
    let mut result: HashMap<String, String> = HashMap::new();
    let mut current_txid: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();

    for line in content.lines() {
        if line.is_empty() {
            if let Some(txid) = current_txid.take() {
                result.insert(txid, current_lines.join("\n"));
            }
            current_lines.clear();
        } else {
            if !line.starts_with(' ') && !line.starts_with('\t') {
                current_txid = extract_tag(line, "txid");
            }
            current_lines.push(line);
        }
    }
    if let Some(txid) = current_txid {
        if !current_lines.is_empty() {
            result.insert(txid, current_lines.join("\n"));
        }
    }
    result
}

// ── Journal parsing (for BFS only) ───────────────────────────────────────────

fn parse_journal(content: &str) -> Vec<RawEntry> {
    let mut entries: Vec<RawEntry> = Vec::new();
    let mut pending: Option<(NaiveDate, String, Vec<String>)> = None;

    for line in content.lines() {
        if line.is_empty() {
            if let Some((date, txid, addresses)) = pending.take() {
                if !addresses.is_empty() {
                    entries.push(RawEntry { date, txid, addresses });
                }
            }
            continue;
        }

        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some((_, _, ref mut addresses)) = pending {
                if let Some(addr) = extract_address_from_account(line) {
                    addresses.push(addr);
                }
            }
        } else {
            if let Some((date, txid, addresses)) = pending.take() {
                if !addresses.is_empty() {
                    entries.push(RawEntry { date, txid, addresses });
                }
            }
            pending = parse_header_line(line).map(|(d, t)| (d, t, Vec::new()));
        }
    }
    if let Some((date, txid, addresses)) = pending {
        if !addresses.is_empty() {
            entries.push(RawEntry { date, txid, addresses });
        }
    }
    entries
}

fn parse_header_line(line: &str) -> Option<(NaiveDate, String)> {
    let (date_str, _) = line.split_once(' ')?;
    let date = NaiveDate::from_str(date_str).ok()?;
    let txid = extract_tag(line, "txid")?;
    Some((date, txid))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn extract_address_from_account(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let end = trimmed.find("  ").or_else(|| trimmed.find('\t')).unwrap_or(trimmed.len());
    let account = trimmed[..end].trim_end();
    let last = account.split(':').last()?;
    bdk_wallet::bitcoin::Address::from_str(last).ok().map(|_| last.to_string())
}

fn extract_tag(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}:");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find([',', ' ', '\n']).unwrap_or(rest.len());
    let val = rest[..end].trim();
    if val.is_empty() { None } else { Some(val.to_string()) }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const ADDR_A: &str = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq";
    const ADDR_B: &str = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
    const TXID1: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TXID2: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const TXID3: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    fn recv(addr: &str, txid: &str) -> String {
        format!(
            "2024-01-10 * Incoming BTC  ; txid:{txid}\n    assets:bitcoin:wallet:{addr}    50,000 sat  ; vout:0\n    income:unknown\n\n"
        )
    }

    fn spend(from: &str, change: &str, txid: &str) -> String {
        format!(
            "2024-01-20 * Outgoing BTC  ; txid:{txid}\n    assets:bitcoin:wallet:{from}    -50,000 sat  ; input:0\n    assets:bitcoin:wallet:{change}    49,000 sat  ; vout:1\n    expenses:unknown\n\n"
        )
    }

    #[test]
    fn single_unspent_address() {
        let blocks = trace(&recv(ADDR_A, TXID1), ADDR_A);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].contains(TXID1));
        assert!(blocks[0].contains(ADDR_A));
    }

    #[test]
    fn follows_change_address() {
        let journal = recv(ADDR_A, TXID1) + &spend(ADDR_A, ADDR_B, TXID2);
        let blocks = trace(&journal, ADDR_A);
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].contains(TXID1));
        assert!(blocks[1].contains(TXID2));
    }

    #[test]
    fn consolidation_pulls_in_co_input() {
        let consolidation = format!(
            "2024-01-20 * Consolidation  ; txid:{TXID2}\n    assets:bitcoin:wallet:{ADDR_A}    -50,000 sat  ; input:0\n    assets:bitcoin:wallet:{ADDR_B}    -30,000 sat  ; input:1\n    assets:bitcoin:wallet:{ADDR_A}    79,500 sat  ; vout:0\n    expenses:unknown\n\n"
        );
        let bob_recv = format!(
            "2024-01-12 * Bob payment  ; txid:{TXID3}\n    assets:bitcoin:wallet:{ADDR_B}    30,000 sat  ; vout:0\n    income:unknown\n\n"
        );
        let journal = recv(ADDR_A, TXID1) + &bob_recv + &consolidation;
        let blocks = trace(&journal, ADDR_A);
        let all = blocks.join("\n");
        assert!(all.contains(TXID1), "should include A's receive");
        assert!(all.contains(TXID2), "should include consolidation");
        assert!(all.contains(TXID3), "co-input should pull in Bob's receive");
    }

    #[test]
    fn unknown_address_returns_empty() {
        let blocks = trace(&recv(ADDR_A, TXID1), ADDR_B);
        assert!(blocks.is_empty());
    }

    #[test]
    fn results_sorted_by_date() {
        let journal = spend(ADDR_A, ADDR_B, TXID2) + &recv(ADDR_A, TXID1);
        let blocks = trace(&journal, ADDR_A);
        assert!(blocks[0].contains(TXID1));
        assert!(blocks[1].contains(TXID2));
    }

    #[test]
    fn no_duplicate_entries() {
        let journal = recv(ADDR_A, TXID1) + &spend(ADDR_A, ADDR_B, TXID2);
        let blocks = trace(&journal, ADDR_A);
        let txid2_count = blocks.iter().filter(|b| b.contains(TXID2)).count();
        assert_eq!(txid2_count, 1);
    }

    #[test]
    fn raw_text_preserved() {
        let journal = recv(ADDR_A, TXID1);
        let blocks = trace(&journal, ADDR_A);
        assert!(blocks[0].contains("Incoming BTC"));
        assert!(blocks[0].contains("income:unknown"));
    }
}
