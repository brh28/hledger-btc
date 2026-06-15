use crate::annotate::AnnotationType;
use crate::text::{extract_tag, extract_int_tag, extract_address_from_account};

pub fn set_label(
    journal_content: &str,
    type_: &AnnotationType,
    ref_: &str,
    label: &str,
) -> String {
    match type_ {
        AnnotationType::Tx => set_tx_description(journal_content, ref_, label),
        AnnotationType::Output => match ref_.split_once(':') {
            Some((txid, vout)) => set_posting_freetext(journal_content, Some(txid), "vout", vout, label),
            None => journal_content.to_string(),
        },
        AnnotationType::Input => match ref_.split_once(':') {
            Some((txid, idx)) => set_posting_freetext(journal_content, Some(txid), "input", idx, label),
            None => journal_content.to_string(),
        },
        AnnotationType::Addr => set_posting_freetext(journal_content, None, "addr", ref_, label),
    }
}

fn set_tx_description(journal_content: &str, txid: &str, description: &str) -> String {
    let mut result: Vec<String> = Vec::new();
    for line in journal_content.lines() {
        if !line.starts_with(' ') && !line.starts_with('\t') && !line.is_empty()
            && extract_tag(line, "txid").as_deref() == Some(txid)
        {
            result.push(replace_description(line, description));
        } else {
            result.push(line.to_string());
        }
    }
    finish(journal_content, result)
}

fn set_posting_freetext(
    journal_content: &str,
    txid_filter: Option<&str>,
    tag_key: &str,
    tag_val: &str,
    label: &str,
) -> String {
    let mut current_txid: Option<String> = None;
    let mut result: Vec<String> = Vec::new();

    for line in journal_content.lines() {
        let out = if line.is_empty() {
            current_txid = None;
            line.to_string()
        } else if line.starts_with(' ') || line.starts_with('\t') {
            let matches = if tag_key == "addr" {
                extract_address_from_account(line).as_deref() == Some(tag_val)
            } else {
                extract_int_tag(line, tag_key).as_deref() == Some(tag_val)
                    && txid_filter.map_or(true, |t| current_txid.as_deref() == Some(t))
            };
            if matches { replace_comment_freetext(line, label) } else { line.to_string() }
        } else {
            if let Some(txid) = extract_tag(line, "txid") {
                current_txid = Some(txid);
            }
            line.to_string()
        };
        result.push(out);
    }
    finish(journal_content, result)
}

fn replace_description(line: &str, new_desc: &str) -> String {
    let desc_start = line.find(" * ").or_else(|| line.find(" ! "))
        .map(|p| p + 3)
        .unwrap_or_else(|| line.find(' ').map(|p| p + 1).unwrap_or(0));
    let desc_end = line[desc_start..].find("  ;")
        .map(|p| desc_start + p)
        .unwrap_or(line.len());
    format!("{}{}{}", &line[..desc_start], new_desc, &line[desc_end..])
}

fn replace_comment_freetext(line: &str, new_text: &str) -> String {
    let bytes = line.as_bytes();
    let semi = (1..bytes.len()).find(|&i| bytes[i] == b';' && bytes[i - 1] == b' ');
    match semi {
        Some(pos) => {
            let content = &line[pos + 1..]; // includes leading space(s) after ';'
            let trimmed = content.trim_start();
            let tag_pos = find_first_tag_start(trimmed);
            let prefix = &content[..content.len() - trimmed.len()];
            if tag_pos == trimmed.len() {
                // no tags, just replace/set free text
                format!("{};{}{}", &line[..pos], prefix, new_text)
            } else {
                // preserve existing tags after new free text
                format!("{};{}{}  {}", &line[..pos], prefix, new_text, &trimmed[tag_pos..])
            }
        }
        None => format!("{}  ; {}", line, new_text),
    }
}

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


fn finish(original: &str, lines: Vec<String>) -> String {
    let mut out = lines.join("\n");
    if original.ends_with('\n') { out.push('\n'); }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TXID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ADDR: &str = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq";

    #[test]
    fn sets_tx_description() {
        let journal = format!("2024-01-15 * Incoming BTC  ; txid:{TXID}\n    income:unknown\n");
        let result = set_label(&journal, &AnnotationType::Tx, TXID, "Coinbase reward");
        assert!(result.contains("* Coinbase reward  ;"));
        assert!(!result.contains("Incoming BTC"));
    }

    #[test]
    fn preserves_tags_after_description_change() {
        let journal = format!("2024-01-15 * Incoming BTC  ; txid:{TXID}, label:old\n    income:unknown\n");
        let result = set_label(&journal, &AnnotationType::Tx, TXID, "Coinbase reward");
        assert!(result.contains("* Coinbase reward  ; txid:"));
        assert!(result.contains("label:old"));
    }

    #[test]
    fn sets_output_freetext_no_existing_comment() {
        let journal = format!("2024-01-15 * Incoming BTC  ; txid:{TXID}\n    assets:bitcoin:{ADDR}    100 sat  ; vout:1\n    income:unknown\n");
        let result = set_label(&journal, &AnnotationType::Output, &format!("{TXID}:1"), "savings deposit");
        assert!(result.contains("; savings deposit  vout:1"));
    }

    #[test]
    fn sets_output_freetext_replaces_existing() {
        let journal = format!("2024-01-15 * Incoming BTC  ; txid:{TXID}\n    assets:bitcoin:{ADDR}    100 sat  ; old text  vout:1\n    income:unknown\n");
        let result = set_label(&journal, &AnnotationType::Output, &format!("{TXID}:1"), "savings deposit");
        assert!(result.contains("; savings deposit  vout:1"));
        assert!(!result.contains("old text"));
    }

    #[test]
    fn sets_output_freetext_preserves_extra_tags() {
        let journal = format!("2024-01-15 * Incoming BTC  ; txid:{TXID}\n    assets:bitcoin:{ADDR}    100 sat  ; vout:1, lot:20260608\n    income:unknown\n");
        let result = set_label(&journal, &AnnotationType::Output, &format!("{TXID}:1"), "savings deposit");
        assert!(result.contains("savings deposit"));
        assert!(result.contains("vout:1"));
        assert!(result.contains("lot:20260608"));
    }

    #[test]
    fn sets_addr_freetext() {
        let journal = format!("2024-01-15 * Incoming BTC  ; txid:{TXID}\n    assets:bitcoin:{ADDR}    100 sat  ; vout:1\n    income:unknown\n");
        let result = set_label(&journal, &AnnotationType::Addr, ADDR, "my savings");
        assert!(result.contains("; my savings  vout:1"));
    }

    #[test]
    fn no_match_leaves_unchanged() {
        let other = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let journal = format!("2024-01-15 * Incoming BTC  ; txid:{TXID}\n    income:unknown\n");
        let result = set_label(&journal, &AnnotationType::Tx, other, "should not appear");
        assert_eq!(result, journal);
    }
}
