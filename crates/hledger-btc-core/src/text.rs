use std::str::FromStr;

/// Extracts the value of the first occurrence of `key:` in `line`,
/// stopping at the next comma, space, or end of line.
pub fn extract_tag(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}:");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find([',', ' ']).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// Like `extract_tag` but only matches a purely numeric value.
pub fn extract_int_tag(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}:");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    if end == 0 { return None; }
    Some(rest[..end].to_string())
}

/// If the last segment of the posting account (after the final `:`) is a valid
/// Bitcoin address, returns it; otherwise returns `None`.
pub fn extract_address_from_account(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let end = trimmed.find("  ").or_else(|| trimmed.find('\t')).unwrap_or(trimmed.len());
    let account = trimmed[..end].trim_end();
    let last = account.split(':').last()?;
    bdk_wallet::bitcoin::Address::from_str(last).ok().map(|_| last.to_string())
}

/// Sets `key:value` on `line`. If the tag already exists and
/// `override_existing` is false, the line is returned unchanged. If true,
/// the existing value is replaced.
pub fn set_tag(line: &str, key: &str, value: &str, override_existing: bool) -> String {
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

/// Appends `key:value` to `line` unless that exact `key:value` pair is already
/// present. Unlike `set_tag`, this always adds rather than replacing, which is
/// correct for multi-valued keys like `source:`.
pub fn append_tag_if_absent(line: &str, key: &str, value: &str) -> String {
    let tag = format!("{key}:{value}");
    if line.contains(&tag) {
        return line.to_string();
    }
    if line.contains("  ; ") {
        format!("{}, {}", line, tag)
    } else {
        format!("{}  ; {}", line, tag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_tag_comma_delimited() {
        assert_eq!(extract_tag("2026-01-01 * Foo  ; txid:abc, source:x", "txid"), Some("abc".into()));
    }

    #[test]
    fn extract_tag_at_end_of_line() {
        assert_eq!(extract_tag("2026-01-01 * Foo  ; txid:abc", "txid"), Some("abc".into()));
    }

    #[test]
    fn extract_tag_missing() {
        assert_eq!(extract_tag("2026-01-01 * Foo", "txid"), None);
    }

    #[test]
    fn extract_int_tag_basic() {
        assert_eq!(extract_int_tag("    account    100 SAT  ; vout:1", "vout"), Some("1".into()));
    }

    #[test]
    fn set_tag_adds_when_absent() {
        let out = set_tag("2026-01-01 * Foo  ; txid:abc", "lot", "2026", false);
        assert!(out.ends_with(", lot:2026"));
    }

    #[test]
    fn set_tag_skips_when_present_no_override() {
        let line = "2026-01-01 * Foo  ; txid:abc, lot:old";
        assert_eq!(set_tag(line, "lot", "new", false), line);
    }

    #[test]
    fn set_tag_replaces_when_override() {
        let out = set_tag("2026-01-01 * Foo  ; txid:abc, lot:old", "lot", "new", true);
        assert!(out.contains("lot:new"));
        assert!(!out.contains("lot:old"));
    }

    #[test]
    fn append_tag_if_absent_adds() {
        let out = append_tag_if_absent("2026-01-01 * Foo  ; source:electrum", "source", "phoenix");
        assert!(out.contains("source:electrum, source:phoenix"));
    }

    #[test]
    fn append_tag_if_absent_idempotent() {
        let line = "2026-01-01 * Foo  ; source:electrum, source:phoenix";
        assert_eq!(append_tag_if_absent(line, "source", "phoenix"), line);
    }

    #[test]
    fn append_tag_if_absent_creates_comment() {
        let out = append_tag_if_absent("2026-01-01 * Foo", "source", "electrum");
        assert!(out.contains("  ; source:electrum"));
    }
}
