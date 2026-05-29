use std::collections::HashMap;
use std::str::FromStr;
use anyhow::Result;
use bdk_wallet::bitcoin::{Address, Txid};
use bip329::{AddressRecord, Label, Labels, TransactionRecord};

/// Read a journal file's content and produce a BIP329 JSONL string.
///
/// Transaction records are built from `txid:` tags on header lines.
/// Address records are built from `address:` tags on both posting lines
/// (scan entries) and header lines (receive/receivable entries).
/// `label:` tags are carried through when present.
/// Returns an error if the same ref appears with conflicting label values.
pub fn export_to_string(journal_content: &str) -> Result<String> {
    let mut tx_map: HashMap<String, Option<String>> = HashMap::new();
    let mut addr_map: HashMap<String, Option<String>> = HashMap::new();

    for line in journal_content.lines() {
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }

        let is_posting = line.starts_with(' ') || line.starts_with('\t');
        let label = extract_tag(line, "label");

        if is_posting {
            if let Some(addr) = extract_tag(line, "address") {
                insert_or_check(&mut addr_map, addr, label, "address")?;
            }
        } else {
            if let Some(txid) = extract_tag(line, "txid") {
                insert_or_check(&mut tx_map, txid, label.clone(), "txid")?;
            }
            // receive entries carry address: on the header line, not txid:
            if let Some(addr) = extract_tag(line, "address") {
                insert_or_check(&mut addr_map, addr, label, "address")?;
            }
        }
    }

    let mut labels: Vec<Label> = Vec::new();

    for (txid_str, label) in &tx_map {
        let txid = Txid::from_str(txid_str)
            .map_err(|e| anyhow::anyhow!("invalid txid '{txid_str}': {e}"))?;
        labels.push(Label::Transaction(TransactionRecord {
            ref_: txid,
            label: label.clone(),
            origin: None,
        }));
    }

    for (addr_str, label) in &addr_map {
        let addr = Address::from_str(addr_str)
            .map_err(|e| anyhow::anyhow!("invalid address '{addr_str}': {e}"))?;
        labels.push(Label::Address(AddressRecord {
            ref_: addr,
            label: label.clone(),
        }));
    }

    Labels::from(labels)
        .export()
        .map_err(|e| anyhow::anyhow!("BIP329 export error: {e}"))
}

fn extract_tag(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}:");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find([',', ' ', '\t']).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

fn insert_or_check(
    map: &mut HashMap<String, Option<String>>,
    key: String,
    label: Option<String>,
    kind: &str,
) -> Result<()> {
    match map.get(&key) {
        None => { map.insert(key, label); }
        Some(existing) if *existing == label => {}
        Some(existing) => anyhow::bail!(
            "conflicting labels for {kind} {key}: {:?} vs {:?}",
            existing,
            label,
        ),
    }
    Ok(())
}
