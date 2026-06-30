use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use anyhow::Result;
use bdk_wallet::bitcoin::{Address, Txid};
use bip329::{AddressRecord, Label, TransactionRecord};

pub fn export_to_string(journal_content: &str) -> Result<String> {
    // Value: (bip329 label, extra hledger tags)
    let mut tx_map: HashMap<String, (Option<String>, BTreeMap<String, String>)> = HashMap::new();
    let mut addr_map: HashMap<String, (Option<String>, BTreeMap<String, String>)> = HashMap::new();
    // keyed by "txid:index"
    let mut output_map: HashMap<String, (Option<String>, BTreeMap<String, String>)> = HashMap::new();
    let mut input_map: HashMap<String, (Option<String>, BTreeMap<String, String>)> = HashMap::new();

    let mut current_txid: Option<String> = None;

    for line in journal_content.lines() {
        if line.is_empty() {
            current_txid = None;
            continue;
        }
        if line.starts_with(';') || line.starts_with('#') {
            continue;
        }

        let is_posting = line.starts_with(' ') || line.starts_with('\t');

        if is_posting {
            if let (Some(vout), Some(txid)) = (extract_int_tag(line, "vout"), &current_txid) {
                let ref_ = format!("{txid}:{vout}");
                let label = extract_comment_freetext(line);
                let tags = extra_tags(line, &["vout", "label"]);
                output_map.entry(ref_).or_insert((label, tags));
            } else if let (Some(idx), Some(txid)) = (extract_int_tag(line, "input"), &current_txid) {
                let ref_ = format!("{txid}:{idx}");
                let label = extract_comment_freetext(line);
                let tags = extra_tags(line, &["input", "label"]);
                input_map.entry(ref_).or_insert((label, tags));
            } else {
                let addr = extract_tag(line, "address")
                    .or_else(|| extract_address_from_account(line));
                if let Some(addr) = addr {
                    let label = extract_comment_freetext(line);
                    let tags = extra_tags(line, &["address", "label"]);
                    insert_or_check(&mut addr_map, addr, label, tags, "address")?;
                }
            }
        } else {
            let label = extract_description(line);
            if let Some(txid) = extract_tag(line, "txid") {
                current_txid = Some(txid.clone());
                let tags = extra_tags(line, &["txid", "label"]);
                insert_or_check(&mut tx_map, txid, label.clone(), tags, "txid")?;
            }
            if let Some(addr) = extract_tag(line, "address") {
                let tags = extra_tags(line, &["address", "label"]);
                insert_or_check(&mut addr_map, addr, label, tags, "address")?;
            }
        }
    }

    let mut lines: Vec<String> = Vec::new();

    for (txid_str, (label, tags)) in &tx_map {
        if label.is_none() && tags.is_empty() { continue; }
        let txid = Txid::from_str(txid_str)
            .map_err(|e| anyhow::anyhow!("invalid txid '{txid_str}': {e}"))?;
        let record = Label::Transaction(TransactionRecord { ref_: txid, label: label.clone(), origin: None });
        lines.push(to_jsonl(&record, tags)?);
    }

    for (addr_str, (label, tags)) in &addr_map {
        if label.is_none() && tags.is_empty() { continue; }
        let addr = Address::from_str(addr_str)
            .map_err(|e| anyhow::anyhow!("invalid address '{addr_str}': {e}"))?;
        let record = Label::Address(AddressRecord { ref_: addr, label: label.clone() });
        lines.push(to_jsonl(&record, tags)?);
    }

    for (ref_, (label, tags)) in &output_map {
        if label.is_none() && tags.is_empty() { continue; }
        lines.push(to_outpoint_jsonl("output", ref_, label, tags)?);
    }

    for (ref_, (label, tags)) in &input_map {
        if label.is_none() && tags.is_empty() { continue; }
        lines.push(to_outpoint_jsonl("input", ref_, label, tags)?);
    }

    Ok(lines.join("\n"))
}

fn to_outpoint_jsonl(type_: &str, ref_: &str, label: &Option<String>, tags: &BTreeMap<String, String>) -> Result<String> {
    let mut obj = serde_json::Map::new();
    obj.insert("ref".to_string(), serde_json::Value::String(ref_.to_string()));
    obj.insert("type".to_string(), serde_json::Value::String(type_.to_string()));
    if let Some(l) = label {
        obj.insert("label".to_string(), serde_json::Value::String(l.clone()));
    }
    if !tags.is_empty() {
        obj.insert("tags".to_string(), serde_json::to_value(tags)?);
    }
    Ok(serde_json::to_string(&serde_json::Value::Object(obj))?)
}

fn to_jsonl(label: &Label, tags: &BTreeMap<String, String>) -> Result<String> {
    let mut value = serde_json::to_value(label)?;
    if !tags.is_empty() {
        value.as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("unexpected non-object label"))?
            .insert("tags".to_string(), serde_json::to_value(tags)?);
    }
    Ok(serde_json::to_string(&value)?)
}

/// Returns the comment text (after the `;`) for a line whose comment starts with `[whitespace];`.
fn find_comment(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    for i in 1..bytes.len() {
        if bytes[i] == b';' && bytes[i - 1] == b' ' {
            return Some(&line[i + 1..]);
        }
    }
    None
}

/// Description from a transaction line: text between status marker and comment delimiter.
fn extract_description(line: &str) -> Option<String> {
    let without_comment = match find_comment(line).map(|c| line.len() - c.len() - 2) {
        Some(i) => &line[..i],
        None => line,
    };
    let after_date = without_comment.get(10..)?.trim_start();
    let after_status = if after_date.starts_with('*') || after_date.starts_with('!') {
        after_date[1..].trim_start()
    } else {
        after_date
    };
    let desc = after_status.trim_end();
    if desc.is_empty() { None } else { Some(desc.to_string()) }
}

/// Free-text prefix of a posting comment: text before the first `word:` tag.
fn extract_comment_freetext(line: &str) -> Option<String> {
    let comment = find_comment(line)?.trim_start();
    let text_end = find_first_tag_start(comment);
    let text = comment[..text_end].trim_end_matches(|c: char| c == ' ' || c == ',' || c == '\t');
    if text.is_empty() { None } else { Some(text.to_string()) }
}

/// Position of the first `word:` pattern in `s`.
fn find_first_tag_start(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'-' {
            let word_start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'-') {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b':' {
                return word_start;
            }
        } else {
            i += 1;
        }
    }
    s.len()
}

/// All hledger tags from `line`'s comment except those in `exclude`.
/// Handles both comma-separated and space-separated tags.
fn extra_tags(line: &str, exclude: &[&str]) -> BTreeMap<String, String> {
    let Some(comment) = find_comment(line) else { return BTreeMap::new() };
    let mut result = BTreeMap::new();
    let mut rest = comment;

    loop {
        let Some(key_start) = rest.find(|c: char| c.is_alphanumeric() || c == '_' || c == '-') else { break };
        rest = &rest[key_start..];

        let key_end = rest.find(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
            .unwrap_or(rest.len());
        let key = &rest[..key_end];
        rest = &rest[key_end..];

        if !rest.starts_with(':') {
            match rest.find(|c: char| c == ',' || c == ' ' || c == '\t') {
                Some(i) => { rest = &rest[i + 1..]; }
                None => break,
            }
            continue;
        }
        rest = &rest[1..]; // skip ':'

        let val_end = tag_value_end(rest);
        let value = rest[..val_end].trim();

        if !key.is_empty() && !exclude.contains(&key) {
            result.insert(key.to_string(), value.to_string());
        }

        rest = rest[val_end..].trim_start_matches(|c: char| c == ',' || c == ' ' || c == '\t');
    }

    result
}

/// Returns the end index of a tag value in `s`.
/// Values end at a comma, or at whitespace followed by a `word:` pattern (next tag).
fn tag_value_end(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b',' {
            return i;
        }
        if bytes[i] == b' ' || bytes[i] == b'\t' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'-') {
                j += 1;
            }
            if j > i + 1 && j < bytes.len() && bytes[j] == b':' {
                return i;
            }
        }
        i += 1;
    }
    s.len()
}

/// Extracts a Bitcoin address from the last component of a posting's account name.
fn extract_address_from_account(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let end = trimmed.find("  ").or_else(|| trimmed.find('\t')).unwrap_or(trimmed.len());
    let account = trimmed[..end].trim_end();
    let last = account.split(':').last()?;
    Address::from_str(last).ok().map(|_| last.to_string())
}

fn extract_tag(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}:");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = tag_value_end(rest);
    Some(rest[..end].trim_end().to_string())
}

fn extract_int_tag(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}:");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    if end == 0 { return None; }
    Some(rest[..end].to_string())
}

fn insert_or_check(
    map: &mut HashMap<String, (Option<String>, BTreeMap<String, String>)>,
    key: String,
    label: Option<String>,
    tags: BTreeMap<String, String>,
    kind: &str,
) -> Result<()> {
    match map.get(&key) {
        None => { map.insert(key, (label, tags)); }
        Some((existing, _)) if *existing == label => {}
        Some((existing, _)) => anyhow::bail!(
            "conflicting labels for {kind} {key}: {:?} vs {:?}", existing, label,
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::import_from_str;

    const TXID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ADDR: &str = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq";

    #[test]
    fn exports_tx_label() {
        let journal = format!("2024-01-15 * Mining reward  ; txid:{TXID}\n    income:bitcoin\n");
        let out = export_to_string(&journal).unwrap();
        assert!(out.contains(&format!("\"ref\":\"{TXID}\"")));
        assert!(out.contains("\"label\":\"Mining reward\""));
        assert!(out.contains("\"type\":\"tx\""));
    }

    #[test]
    fn exports_extra_tags() {
        let journal = format!("2024-01-15 * Recv  ; txid:{TXID}, lot:20260608\n    income:bitcoin\n");
        let out = export_to_string(&journal).unwrap();
        assert!(out.contains("\"lot\":\"20260608\""));
        let tags_obj: serde_json::Value = serde_json::from_str(&out).unwrap();
        let tags = tags_obj.get("tags").unwrap();
        assert!(tags.get("txid").is_none());
        assert!(tags.get("label").is_none());
    }

    #[test]
    fn no_tags_field_when_empty() {
        let journal = format!("2024-01-15 * Recv  ; txid:{TXID}\n    income:bitcoin\n");
        let out = export_to_string(&journal).unwrap();
        assert!(!out.contains("\"tags\""));
    }

    #[test]
    fn exports_address_label_from_posting() {
        let journal = format!(
            "2024-01-15 * Mining reward  ; txid:{TXID}\n    assets:bitcoin:wallet:{ADDR}    100000 sat  ; my savings\n    income:bitcoin\n"
        );
        let out = export_to_string(&journal).unwrap();
        assert!(out.contains(&format!("\"ref\":\"{ADDR}\"")));
        assert!(out.contains("\"label\":\"my savings\""));
    }

    #[test]
    fn errors_on_conflicting_tx_labels() {
        let journal = format!(
            "2024-01-15 * First  ; txid:{TXID}\n    income:bitcoin\n\n\
             2024-01-16 * Second  ; txid:{TXID}\n    income:bitcoin\n"
        );
        assert!(export_to_string(&journal).is_err());
    }

    #[test]
    fn same_label_repeated_is_not_an_error() {
        let journal = format!(
            "2024-01-15 * Same  ; txid:{TXID}\n    income:bitcoin\n\n\
             2024-01-16 * Same  ; txid:{TXID}\n    income:bitcoin\n"
        );
        assert!(export_to_string(&journal).is_ok());
    }

    #[test]
    fn round_trip_tx_label() {
        let journal = format!("2024-01-15 * Mining reward  ; txid:{TXID}\n    income:bitcoin\n");
        let bip329 = export_to_string(&journal).unwrap();
        let reimported = import_from_str(&journal, &bip329, false).unwrap();
        assert!(reimported.contains("* Mining reward"));
    }

    #[test]
    fn round_trip_extra_tags() {
        let journal = format!("2024-01-15 * Recv  ; txid:{TXID}, lot:20260608\n    income:bitcoin\n");
        let unlabeled = format!("2024-01-15 * Recv  ; txid:{TXID}\n    income:bitcoin\n");
        let bip329 = export_to_string(&journal).unwrap();
        let reimported = import_from_str(&unlabeled, &bip329, false).unwrap();
        assert!(reimported.contains("* Recv"));
        assert!(reimported.contains("lot:20260608"));
    }

    #[test]
    fn exports_output_record() {
        let journal = format!(
            "2024-01-15 * Incoming BTC  ; txid:{TXID}\n    assets:bitcoin:wallet:{ADDR}    100000 sat  ; vout:1, lot:20260608\n    income:unknown\n"
        );
        let out = export_to_string(&journal).unwrap();
        let output_rec: serde_json::Value = out.lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .find(|v| v["type"] == "output")
            .expect("output record");
        assert_eq!(output_rec["ref"], format!("{TXID}:1"));
    }

    #[test]
    fn exports_output_with_tags() {
        let journal = format!(
            "2024-01-15 * Incoming BTC  ; txid:{TXID}\n    assets:bitcoin:wallet:{ADDR}    100000 sat  ; vout:1, lot:20260608\n    income:unknown\n"
        );
        let out = export_to_string(&journal).unwrap();
        let output_rec: serde_json::Value = out.lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .find(|v| v["type"] == "output")
            .expect("output record");
        assert_eq!(output_rec["tags"]["lot"], "20260608");
        assert!(output_rec.get("tags").and_then(|t| t.get("vout")).is_none());
    }

    #[test]
    fn exports_input_record() {
        let journal = format!(
            "2024-01-15 * Outgoing BTC  ; txid:{TXID}\n    assets:bitcoin:wallet:{ADDR}    -50000 sat  ; input:0, lot:20260608\n    expenses:unknown\n"
        );
        let out = export_to_string(&journal).unwrap();
        let input_rec: serde_json::Value = out.lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .find(|v| v["type"] == "input")
            .expect("input record");
        assert_eq!(input_rec["ref"], format!("{TXID}:0"));
    }

    #[test]
    fn exports_space_separated_tags() {
        let journal = format!(
            "2026-05-22 * from Bob the builder  ; this hledger comment kyc:true txid:{TXID}\n    assets:bitcoin:wallet    3861 sat\n    income:unknown\n"
        );
        let out = export_to_string(&journal).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ref"], TXID);
        assert_eq!(v["label"], "from Bob the builder");
        let tags = v.get("tags").expect("tags field should be present");
        assert_eq!(tags["kyc"], "true");
        assert!(tags.get("txid").is_none());
    }

    #[test]
    fn exports_posting_tag_with_single_space_comment() {
        let journal = format!(
            "2024-01-15 * Recv  ; txid:{TXID}\n    assets:bitcoin:wallet:{ADDR}    3861 sat ; lot:20260608\n    income:unknown\n"
        );
        let out = export_to_string(&journal).unwrap();
        let lines: Vec<serde_json::Value> = out.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let addr_rec = lines.iter().find(|v| v["ref"] == ADDR).unwrap();
        assert_eq!(addr_rec["tags"]["lot"], "20260608");
    }

    #[test]
    fn round_trip_address_label() {
        let labeled = format!(
            "2024-01-15 * Mining reward  ; txid:{TXID}\n    assets:bitcoin:wallet:{ADDR}    100000 sat  ; my savings\n    income:bitcoin\n"
        );
        let unlabeled = format!(
            "2024-01-15 * Mining reward  ; txid:{TXID}\n    assets:bitcoin:wallet:{ADDR}    100000 sat\n    income:bitcoin\n"
        );
        let bip329 = export_to_string(&labeled).unwrap();
        let reimported = import_from_str(&unlabeled, &bip329, false).unwrap();
        assert!(reimported.contains("; my savings"));
    }
}
