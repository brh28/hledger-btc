use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use anyhow::Result;

use crate::annotate::{Annotation, AnnotationType};

pub fn import_from_str(
    journal_content: &str,
    bip329_content: &str,
    override_existing: bool,
) -> Result<String> {
    let maps = AnnotationMaps::from_jsonl(bip329_content)?;
    Ok(maps.apply_to(journal_content, override_existing))
}

pub fn annotate_journal(
    journal_content: &str,
    annotation: &Annotation,
    override_existing: bool,
) -> String {
    AnnotationMaps::from_annotation(annotation).apply_to(journal_content, override_existing)
}

#[derive(Default)]
struct AnnotationMaps {
    tx_labels:     HashMap<String, String>,
    tx_tags:       HashMap<String, BTreeMap<String, String>>,
    addr_labels:   HashMap<String, String>,
    addr_tags:     HashMap<String, BTreeMap<String, String>>,
    output_labels: HashMap<String, String>,
    output_tags:   HashMap<String, BTreeMap<String, String>>,
    input_labels:  HashMap<String, String>,
    input_tags:    HashMap<String, BTreeMap<String, String>>,
}

impl AnnotationMaps {
    fn from_jsonl(bip329_content: &str) -> Result<Self> {
        let mut maps = Self::default();
        for line in bip329_content.lines().filter(|l| !l.trim().is_empty()) {
            let value: serde_json::Value = serde_json::from_str(line)
                .map_err(|e| anyhow::anyhow!("BIP329 parse error: {e}"))?;
            let type_ = value.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let ref_ = match value.get("ref").and_then(|r| r.as_str()) {
                Some(r) => r.to_string(),
                None => continue,
            };
            let label = value.get("label").and_then(|l| l.as_str()).map(String::from);
            let tags: BTreeMap<String, String> = value
                .get("tags")
                .and_then(|t| serde_json::from_value(t.clone()).ok())
                .unwrap_or_default();
            let (labels_map, tags_map) = match type_ {
                "tx"     => (&mut maps.tx_labels,     &mut maps.tx_tags),
                "addr"   => (&mut maps.addr_labels,   &mut maps.addr_tags),
                "output" => (&mut maps.output_labels, &mut maps.output_tags),
                "input"  => (&mut maps.input_labels,  &mut maps.input_tags),
                _ => continue,
            };
            if let Some(l) = label { labels_map.insert(ref_.clone(), l); }
            if !tags.is_empty() { tags_map.insert(ref_, tags); }
        }
        Ok(maps)
    }

    fn from_annotation(annotation: &Annotation) -> Self {
        let mut maps = Self::default();
        let ref_ = annotation.ref_.clone();
        let (labels_map, tags_map) = match annotation.type_ {
            AnnotationType::Tx     => (&mut maps.tx_labels,     &mut maps.tx_tags),
            AnnotationType::Addr   => (&mut maps.addr_labels,   &mut maps.addr_tags),
            AnnotationType::Output => (&mut maps.output_labels, &mut maps.output_tags),
            AnnotationType::Input  => (&mut maps.input_labels,  &mut maps.input_tags),
        };
        if let Some(l) = &annotation.label { labels_map.insert(ref_.clone(), l.clone()); }
        if !annotation.tags.is_empty() { tags_map.insert(ref_, annotation.tags.clone()); }
        maps
    }

    fn apply_to(&self, journal_content: &str, override_existing: bool) -> String {
        let mut current_txid: Option<String> = None;
        let mut result_lines: Vec<String> = Vec::new();

        for line in journal_content.lines() {
            let out = if line.is_empty() {
                current_txid = None;
                line.to_string()
            } else if line.starts_with(' ') || line.starts_with('\t') {
                if let (Some(vout), Some(txid)) = (extract_int_tag(line, "vout"), &current_txid) {
                    let ref_ = format!("{txid}:{vout}");
                    apply_posting_annotation(line, self.output_labels.get(&ref_).map(String::as_str), self.output_tags.get(&ref_), override_existing)
                } else if let (Some(idx), Some(txid)) = (extract_int_tag(line, "input"), &current_txid) {
                    let ref_ = format!("{txid}:{idx}");
                    apply_posting_annotation(line, self.input_labels.get(&ref_).map(String::as_str), self.input_tags.get(&ref_), override_existing)
                } else {
                    let addr = extract_tag(line, "address")
                        .or_else(|| extract_address_from_account(line));
                    if let Some(addr) = addr {
                        apply_posting_annotation(line, self.addr_labels.get(&addr).map(String::as_str), self.addr_tags.get(&addr), override_existing)
                    } else {
                        line.to_string()
                    }
                }
            } else {
                if let Some(txid) = extract_tag(line, "txid") {
                    current_txid = Some(txid.clone());
                    apply_tx_annotation(line, self.tx_labels.get(&txid).map(String::as_str), self.tx_tags.get(&txid), override_existing)
                } else {
                    line.to_string()
                }
            };
            result_lines.push(out);
        }

        let mut result = result_lines.join("\n");
        if journal_content.ends_with('\n') {
            result.push('\n');
        }
        result
    }
}

fn apply_tx_annotation(
    line: &str,
    label: Option<&str>,
    tags: Option<&BTreeMap<String, String>>,
    override_existing: bool,
) -> String {
    let mut result = line.to_string();
    if let Some(l) = label {
        result = set_description(&result, l);
    }
    if let Some(t) = tags {
        for (k, v) in t {
            result = set_tag(&result, k, v, override_existing);
        }
    }
    result
}

fn apply_posting_annotation(
    line: &str,
    label: Option<&str>,
    tags: Option<&BTreeMap<String, String>>,
    override_existing: bool,
) -> String {
    let mut result = line.to_string();
    if let Some(l) = label {
        result = set_comment_text(&result, l, override_existing);
    }
    if let Some(t) = tags {
        for (k, v) in t {
            result = set_tag(&result, k, v, override_existing);
        }
    }
    result
}

fn set_description(line: &str, label: &str) -> String {
    let (head, tail) = line.find("  ;")
        .map(|p| (&line[..p], &line[p..]))
        .unwrap_or((line, ""));
    let offset = find_description_offset(head);
    format!("{}{}{}", &head[..offset], label, tail)
}

fn find_description_offset(head: &str) -> usize {
    let after_date = head.len().min(11); // "YYYY-MM-DD "
    let rest = &head[after_date..];
    if rest.starts_with("* ") || rest.starts_with("! ") {
        after_date + 2
    } else {
        after_date
    }
}

fn set_comment_text(line: &str, text: &str, override_existing: bool) -> String {
    if let Some(pos) = line.find("  ; ") {
        let prefix = &line[..pos];
        let comment = &line[pos + 4..];
        let tag_start = find_tag_start(comment);
        let existing_free = comment[..tag_start].trim_end();
        let tags = comment[tag_start..].trim_start();
        if !existing_free.is_empty() && !override_existing {
            return line.to_string();
        }
        if tags.is_empty() {
            format!("{}  ; {}", prefix, text)
        } else {
            format!("{}  ; {}  {}", prefix, text, tags)
        }
    } else {
        format!("{}  ; {}", line, text)
    }
}

fn find_tag_start(comment: &str) -> usize {
    let mut search_from = 0;
    while let Some(colon_rel) = comment[search_from..].find(':') {
        let colon = search_from + colon_rel;
        let word_start = comment[..colon]
            .rfind(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|p| p + 1)
            .unwrap_or(0);
        let word = &comment[word_start..colon];
        if !word.is_empty() && word.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return word_start;
        }
        search_from = colon + 1;
    }
    comment.len()
}

fn set_tag(line: &str, key: &str, value: &str, override_existing: bool) -> String {
    let needle = format!("{key}:");
    if let Some(start) = line.find(&needle) {
        if !override_existing {
            return line.to_string();
        }
        let val_start = start + needle.len();
        let val_end = line[val_start..]
            .find([',', '\n'])
            .map(|i| val_start + i)
            .unwrap_or(line.len());
        return format!("{}{}{}", &line[..val_start], value, &line[val_end..]);
    }

    if line.contains("  ; ") {
        format!("{}, {}:{}", line, key, value)
    } else {
        format!("{}  ; {}:{}", line, key, value)
    }
}

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
    let end = rest.find(',').unwrap_or(rest.len());
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

#[cfg(test)]
mod tests {
    use super::*;

    const TXID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ADDR: &str = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq";

    fn bip329_tx(txid: &str, label: &str) -> String {
        format!(r#"{{"type":"tx","ref":"{txid}","label":"{label}"}}"#)
    }

    fn bip329_tx_with_tags(txid: &str, label: &str, tags: &str) -> String {
        format!(r#"{{"type":"tx","ref":"{txid}","label":"{label}","tags":{{{tags}}}}}"#)
    }

    fn bip329_addr(addr: &str, label: &str) -> String {
        format!(r#"{{"type":"addr","ref":"{addr}","label":"{label}"}}"#)
    }

    fn bip329_output(txid: &str, vout: usize, label: &str) -> String {
        format!(r#"{{"type":"output","ref":"{txid}:{vout}","label":"{label}"}}"#)
    }

    fn bip329_output_with_tags(txid: &str, vout: usize, label: &str, tags: &str) -> String {
        format!(r#"{{"type":"output","ref":"{txid}:{vout}","label":"{label}","tags":{{{tags}}}}}"#)
    }

    fn bip329_input(txid: &str, idx: usize, label: &str) -> String {
        format!(r#"{{"type":"input","ref":"{txid}:{idx}","label":"{label}"}}"#)
    }

    fn journal_with_tx(txid: &str) -> String {
        format!("2024-01-15 * Mining reward  ; txid:{txid}\n    assets:bitcoin:wallet    100000 sat\n    income:bitcoin\n")
    }

    fn journal_with_addr(txid: &str, addr: &str) -> String {
        format!("2024-01-15 * Mining reward  ; txid:{txid}\n    assets:bitcoin:wallet    100000 sat  ; address:{addr}\n    income:bitcoin\n")
    }

    fn journal_with_vout(txid: &str, addr: &str, vout: usize) -> String {
        format!("2024-01-15 * Mining reward  ; txid:{txid}\n    assets:bitcoin:wallet:{addr}    100000 sat  ; vout:{vout}\n    income:bitcoin\n")
    }

    fn journal_with_input(txid: &str, addr: &str, idx: usize) -> String {
        format!("2024-01-15 * Outgoing BTC  ; txid:{txid}\n    assets:bitcoin:wallet:{addr}    -50000 sat  ; input:{idx}\n    expenses:unknown\n")
    }

    #[test]
    fn injects_tx_label() {
        let result = import_from_str(&journal_with_tx(TXID), &bip329_tx(TXID, "coinbase"), false).unwrap();
        assert!(result.contains("* coinbase  ; txid:"));
    }

    #[test]
    fn injects_tx_tags() {
        let bip329 = bip329_tx_with_tags(TXID, "coinbase", r#""lot":"20260608""#);
        let result = import_from_str(&journal_with_tx(TXID), &bip329, false).unwrap();
        assert!(result.contains("* coinbase  ; txid:"));
        assert!(result.contains("lot:20260608"));
    }

    #[test]
    fn injects_address_label() {
        let result = import_from_str(&journal_with_addr(TXID, ADDR), &bip329_addr(ADDR, "my wallet"), false).unwrap();
        assert!(result.contains(&format!("; my wallet  address:{ADDR}")));
    }

    #[test]
    fn injects_address_label_via_account_name() {
        let journal = format!(
            "2024-01-15 * Mining reward  ; txid:{TXID}\n    assets:bitcoin:wallet:{ADDR}    100000 sat\n    income:bitcoin\n"
        );
        let result = import_from_str(&journal, &bip329_addr(ADDR, "my wallet"), false).unwrap();
        assert!(result.contains("; my wallet\n"));
    }

    #[test]
    fn injects_output_label() {
        let result = import_from_str(&journal_with_vout(TXID, ADDR, 1), &bip329_output(TXID, 1, "savings deposit"), false).unwrap();
        assert!(result.contains("; savings deposit  vout:"));
    }

    #[test]
    fn injects_output_tags() {
        let bip329 = bip329_output_with_tags(TXID, 1, "savings deposit", r#""lot":"20260608""#);
        let result = import_from_str(&journal_with_vout(TXID, ADDR, 1), &bip329, false).unwrap();
        assert!(result.contains("; savings deposit  vout:"));
        assert!(result.contains("lot:20260608"));
    }

    #[test]
    fn injects_input_label() {
        let result = import_from_str(&journal_with_input(TXID, ADDR, 0), &bip329_input(TXID, 0, "payment to Alice"), false).unwrap();
        assert!(result.contains("; payment to Alice  input:"));
    }

    #[test]
    fn output_vout_mismatch_leaves_unchanged() {
        let journal = journal_with_vout(TXID, ADDR, 1);
        let result = import_from_str(&journal, &bip329_output(TXID, 2, "wrong vout"), false).unwrap();
        assert!(!result.contains("label:"));
    }

    #[test]
    fn no_match_leaves_journal_unchanged() {
        let other = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let result = import_from_str(&journal_with_tx(TXID), &bip329_tx(other, "some label"), false).unwrap();
        assert_eq!(result, journal_with_tx(TXID));
    }

    #[test]
    fn tx_label_always_replaces_description() {
        let journal = format!("2024-01-15 * Mining reward  ; txid:{TXID}\n    income:bitcoin\n");
        let result = import_from_str(&journal, &bip329_tx(TXID, "replacement"), false).unwrap();
        assert!(result.contains("* replacement  ;"));
    }

    #[test]
    fn preserves_existing_tag_when_no_override() {
        let journal = format!("2024-01-15 * Mining reward  ; txid:{TXID}, lot:original\n    income:bitcoin\n");
        let bip329 = bip329_tx_with_tags(TXID, "x", r#""lot":"new""#);
        let result = import_from_str(&journal, &bip329, false).unwrap();
        assert!(result.contains("lot:original"));
        assert!(!result.contains("lot:new"));
    }

    #[test]
    fn replaces_existing_tag_when_override() {
        let journal = format!("2024-01-15 * Mining reward  ; txid:{TXID}, lot:original\n    income:bitcoin\n");
        let bip329 = bip329_tx_with_tags(TXID, "x", r#""lot":"new""#);
        let result = import_from_str(&journal, &bip329, true).unwrap();
        assert!(result.contains("lot:new"));
        assert!(!result.contains("lot:original"));
    }

    #[test]
    fn preserves_trailing_newline() {
        let journal = journal_with_tx(TXID);
        assert!(journal.ends_with('\n'));
        let result = import_from_str(&journal, &bip329_tx(TXID, "coinbase"), false).unwrap();
        assert!(result.ends_with('\n'));
    }

    #[test]
    fn annotate_journal_tx_label() {
        let annotation = Annotation {
            type_: AnnotationType::Tx,
            ref_: TXID.to_string(),
            label: Some("coinbase".to_string()),
            tags: BTreeMap::new(),
        };
        let result = annotate_journal(&journal_with_tx(TXID), &annotation, false);
        assert!(result.contains("* coinbase  ; txid:"));
    }

    #[test]
    fn annotate_journal_output_label() {
        let annotation = Annotation {
            type_: AnnotationType::Output,
            ref_: format!("{TXID}:1"),
            label: Some("savings".to_string()),
            tags: BTreeMap::new(),
        };
        let result = annotate_journal(&journal_with_vout(TXID, ADDR, 1), &annotation, false);
        assert!(result.contains("; savings  vout:"));
    }
}
