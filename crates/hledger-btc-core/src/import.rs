use std::collections::HashMap;
use anyhow::Result;
use bip329::{Label, Labels};

/// Parse a BIP329 JSONL string and apply its labels to `journal_content`.
///
/// Transaction labels are injected as `label:` tags on transaction header lines
/// matched by their `txid:` tag. Address labels are injected on posting lines
/// matched by their `address:` tag.
///
/// If `override_existing` is false, lines that already carry a `label:` tag are
/// left unchanged. If true, existing label values are replaced.
pub fn import_from_str(
    journal_content: &str,
    bip329_content: &str,
    override_existing: bool,
) -> Result<String> {
    let labels = Labels::try_from_str(bip329_content)
        .map_err(|e| anyhow::anyhow!("BIP329 parse error: {e}"))?;

    let mut tx_labels: HashMap<String, String> = HashMap::new();
    let mut addr_labels: HashMap<String, String> = HashMap::new();

    for label in labels.iter() {
        match label {
            Label::Transaction(tx) => {
                if let Some(l) = &tx.label {
                    tx_labels.insert(tx.ref_.to_string(), l.clone());
                }
            }
            Label::Address(addr) => {
                if let Some(l) = &addr.label {
                    addr_labels.insert(addr.ref_.assume_checked_ref().to_string(), l.clone());
                }
            }
            _ => {}
        }
    }

    let annotated: Vec<String> = journal_content
        .lines()
        .map(|line| annotate_line(line, &tx_labels, &addr_labels, override_existing))
        .collect();

    let mut result = annotated.join("\n");
    if journal_content.ends_with('\n') {
        result.push('\n');
    }
    Ok(result)
}

fn annotate_line(
    line: &str,
    tx_labels: &HashMap<String, String>,
    addr_labels: &HashMap<String, String>,
    override_existing: bool,
) -> String {
    if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
        return line.to_string();
    }

    if line.starts_with(' ') || line.starts_with('\t') {
        if let Some(addr) = extract_tag(line, "address") {
            if let Some(label) = addr_labels.get(&addr) {
                return set_label_tag(line, label, override_existing);
            }
        }
    } else if let Some(txid) = extract_tag(line, "txid") {
        if let Some(label) = tx_labels.get(&txid) {
            return set_label_tag(line, label, override_existing);
        }
    }

    line.to_string()
}

fn extract_tag(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}:");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find([',', ' ', '\t']).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

fn set_label_tag(line: &str, label: &str, override_existing: bool) -> String {
    if let Some(label_start) = line.find("label:") {
        if !override_existing {
            return line.to_string();
        }
        let val_start = label_start + "label:".len();
        let val_end = line[val_start..]
            .find([',', '\n'])
            .map(|i| val_start + i)
            .unwrap_or(line.len());
        return format!("{}{}{}", &line[..val_start], label, &line[val_end..]);
    }

    if line.contains("  ; ") {
        format!("{}, label:{}", line, label)
    } else {
        format!("{}  ; label:{}", line, label)
    }
}
