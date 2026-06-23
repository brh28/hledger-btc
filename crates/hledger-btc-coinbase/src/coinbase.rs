use std::path::Path;
use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::NaiveDate;
use p256::ecdsa::{SigningKey, signature::Signer};
use serde_json::Value;

use hledger_btc_core::journal::{Account, JournalEntry, Posting, PriceAnnotation, TagMap};
use hledger_btc_core::money::Money;
use hledger_btc_core::source::{FeedEntry, Source};

struct Credentials {
    name: String,
    private_key: String,
}

pub struct CoinbaseFeed {
    account: Account,
    creds: Credentials,
}

impl CoinbaseFeed {
    pub fn new(key_file: &Path, account: Account) -> Result<Self> {
        let creds = load_credentials(key_file)?;
        Ok(Self { account, creds })
    }

    fn generate_token(&self, method: &str, path: &str) -> Result<String> {
        let now = chrono::Utc::now().timestamp();
        let header = serde_json::json!({ "alg": "ES256", "kid": self.creds.name });
        let claims = serde_json::json!({
            "sub": self.creds.name,
            "iss": "cdp",
            "nbf": now,
            "exp": now + 120,
            "uri": format!("{method} api.coinbase.com{path}"),
        });
        let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_string(&header)?);
        let claims_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_string(&claims)?);
        let message = format!("{header_b64}.{claims_b64}");

        let pem = self.creds.private_key.replace("\\n", "\n");
        let secret = p256::SecretKey::from_sec1_pem(&pem)
            .context("invalid Coinbase private key")?;
        let signing_key = SigningKey::from(secret);
        let sig: p256::ecdsa::Signature = signing_key.sign(message.as_bytes());
        let sig_b64 = URL_SAFE_NO_PAD.encode(sig.to_bytes());

        Ok(format!("{message}.{sig_b64}"))
    }

    fn get(&self, path: &str) -> Result<Value> {
        let path_only = path.split('?').next().unwrap_or(path);
        let token = self.generate_token("GET", path_only)?;
        let url = format!("https://api.coinbase.com{path}");
        ureq::get(&url)
            .set("Authorization", &format!("Bearer {token}"))
            .call()
            .with_context(|| format!("GET {url} failed"))?
            .into_json()
            .context("failed to parse JSON response")
    }

    fn fetch_orders(&self) -> Result<Vec<FeedEntry>> {
        let account = self.account.as_str().to_string();
        let mut entries = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let path = match &cursor {
                None => "/api/v3/brokerage/orders/historical/batch?product_id=BTC-USD&order_status=FILLED&limit=100".to_string(),
                Some(c) => format!("/api/v3/brokerage/orders/historical/batch?product_id=BTC-USD&order_status=FILLED&limit=100&cursor={c}"),
            };
            let json = self.get(&path)?;
            for order in json["orders"].as_array().context("missing 'orders'")? {
                if let Some(entry) = order_to_entry(order, &account)? {
                    entries.push(entry);
                }
            }
            if json["has_next"].as_bool().unwrap_or(false) {
                cursor = json["cursor"].as_str().map(String::from);
            } else {
                break;
            }
        }
        Ok(entries)
    }

    fn fetch_btc_account_id(&self) -> Result<String> {
        let json = self.get("/v2/accounts?limit=100")?;
        json["data"]
            .as_array()
            .context("missing 'data'")?
            .iter()
            .find(|a| {
                a["currency"]["code"].as_str() == Some("BTC")
                    && a["type"].as_str() == Some("wallet")
            })
            .and_then(|a| a["id"].as_str().map(String::from))
            .context("no BTC wallet account found on Coinbase")
    }

    fn fetch_wallet_transactions(&self, account_id: &str) -> Result<Vec<FeedEntry>> {
        let account = self.account.as_str().to_string();
        let mut entries = Vec::new();
        let base = format!("/v2/accounts/{account_id}/transactions");
        let mut starting_after: Option<String> = None;

        loop {
            let path = match &starting_after {
                None => format!("{base}?limit=100&expand=all"),
                Some(c) => format!("{base}?limit=100&expand=all&starting_after={c}"),
            };
            let json = self.get(&path)?;
            for tx in json["data"].as_array().context("missing 'data'")? {
                if let Some(entry) = wallet_tx_to_entry(tx, &account)? {
                    entries.push(entry);
                }
            }
            starting_after = json["pagination"]["next_uri"]
                .as_str()
                .and_then(|uri| uri.split('?').nth(1))
                .and_then(|qs| {
                    qs.split('&')
                        .find(|p| p.starts_with("starting_after="))
                        .map(|p| p["starting_after=".len()..].to_string())
                });
            if starting_after.is_none() {
                break;
            }
        }
        Ok(entries)
    }
}

impl Source for CoinbaseFeed {
    fn name(&self) -> &str {
        "coinbase"
    }

    fn entries(&self) -> Result<Vec<FeedEntry>> {
        let account_id = self.fetch_btc_account_id()?;
        let mut entries = self.fetch_orders()?;
        entries.extend(self.fetch_wallet_transactions(&account_id)?);
        entries.sort_by_key(|e| e.journal.date);
        Ok(entries)
    }
}

#[derive(serde::Deserialize)]
struct CoinbaseConfig {
    key_file: std::path::PathBuf,
}

pub fn build(config: &toml::Table, account: Account) -> Result<Box<dyn Source + 'static>> {
    let cfg: CoinbaseConfig = toml::Value::Table(config.clone())
        .try_into()
        .context("invalid coinbase config")?;
    Ok(Box::new(CoinbaseFeed::new(&cfg.key_file, account)?))
}

fn load_credentials(key_file: &Path) -> Result<Credentials> {
    let content = std::fs::read_to_string(key_file)
        .with_context(|| format!("failed to read {}", key_file.display()))?;
    let json: Value = serde_json::from_str(&content).context("invalid CDP key file")?;
    Ok(Credentials {
        name: json["name"].as_str().context("CDP key file missing 'name'")?.to_string(),
        private_key: json["privateKey"].as_str().context("CDP key file missing 'privateKey'")?.to_string(),
    })
}

fn order_to_entry(order: &Value, account: &str) -> Result<Option<FeedEntry>> {
    let order_id = order["order_id"].as_str().context("missing order_id")?;
    let side = order["side"].as_str().context("missing side")?;
    let filled_size = order["filled_size"].as_str().context("missing filled_size")?;
    let filled_value = order["filled_value"].as_str().context("missing filled_value")?;
    let total_fees = order["total_fees"].as_str().context("missing total_fees")?;
    let time_str = order["last_fill_time"].as_str().context("missing last_fill_time")?;

    let date = NaiveDate::parse_from_str(&time_str[..10], "%Y-%m-%d")
        .with_context(|| format!("invalid date: {time_str}"))?;

    let btc_sat = btc_to_sat(filled_size)?;
    let btc_cost = Money::parse(filled_value, "USD")?;
    let fee = Money::parse(total_fees, "USD")?;

    let (description, btc_amount) = match side {
        "BUY"  => ("Buy BTC",  btc_sat),
        "SELL" => ("Sell BTC", -btc_sat),
        other => {
            tracing::warn!("unknown order side: {other}, skipping");
            return Ok(None);
        }
    };

    // @@ annotation is rounded to 2dp; USD leg is auto-balanced so hledger
    // computes it from the annotation + fee with no rounding drift.
    let price = Some(PriceAnnotation::Total(format!("{btc_cost}")));

    Ok(Some(FeedEntry::internal("coinbase_id", order_id.to_string(), JournalEntry {
        date,
        description: description.to_string(),
        tags: TagMap::new(),
        postings: vec![
            Posting::with_amount(format!("{}:btc", account), btc_amount).with_price(price),
            Posting::with_money("expenses:fees:coinbase", fee),
            Posting::auto_balance(format!("{}:usd", account)),
        ],
    })))
}

fn wallet_tx_to_entry(tx: &Value, account: &str) -> Result<Option<FeedEntry>> {
    let tx_type = tx["type"].as_str().unwrap_or("");
    let status = tx["status"].as_str().unwrap_or("");
    let currency = tx["amount"]["currency"].as_str().unwrap_or("");

    if status != "completed" || currency != "BTC" || (tx_type != "send" && tx_type != "receive") {
        return Ok(None);
    }

    let id = tx["id"].as_str().context("missing tx id")?;
    let amount_str = tx["amount"]["amount"].as_str().context("missing amount")?;
    let created_at = tx["created_at"].as_str().context("missing created_at")?;

    let date = NaiveDate::parse_from_str(&created_at[..10], "%Y-%m-%d")
        .with_context(|| format!("invalid date: {created_at}"))?;
    let amount_sat = btc_to_sat(amount_str)?;
    let btc_account = format!("{}:{}", account, currency.to_lowercase());
    let is_lightning = tx["network"]["network_name"].as_str() == Some("lightning");
    let txid = tx["network"]["hash"].as_str().filter(|s| !s.is_empty());
    let description = tx["description"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(if tx_type == "send" { "Coinbase Send" } else { "Coinbase Receive" })
        .to_string();
    let balance_account = if amount_sat < 0 { "expenses:unknown" } else { "income:unknown" };
    let fee_sat = if tx_type == "send" && !is_lightning {
        tx["network"]["transaction_fee"]["amount"].as_str()
            .and_then(|s| btc_to_sat(s).ok())
            .filter(|&f| f > 0)
    } else {
        None
    };
    let mut postings = vec![Posting::with_amount(btc_account, amount_sat)];
    if let Some(fee) = fee_sat {
        postings.push(Posting::with_amount("expenses:fees:onchain", fee));
    }
    postings.push(Posting::auto_balance(balance_account));

    if is_lightning && tx_type == "send" {
        // Extract payment_hash from the BOLT11 invoice so this entry can be
        // reconciled against Phoenix (which stamps payment_hash on received payments).
        if let Some(invoice) = tx["to"]["address"].as_str() {
            if let Some(hash) = bolt11_payment_hash(invoice) {
                let mut journal = JournalEntry { date, description, tags: TagMap::new(), postings };
                journal.tags.push("coinbase_id", id);
                return Ok(Some(FeedEntry::lightning(hash, journal)));
            }
            tracing::warn!("could not decode payment_hash from BOLT11 invoice for coinbase_id:{id}");
        }
        // Fall through to internal if invoice missing or undecodable.
        return Ok(Some(FeedEntry::internal("coinbase_id", id.to_string(), JournalEntry {
            date, description, tags: TagMap::new(), postings,
        })));
    }

    if is_lightning {
        // Coinbase Lightning receives expose no payment hash — the API returns
        // only `network: { status }` with no invoice or hash field. These entries
        // carry only coinbase_id and cannot be reconciled against Phoenix.
        tracing::debug!("coinbase_id:{id} is a Lightning receive; no payment_hash available for reconcile");
        return Ok(Some(FeedEntry::internal("coinbase_id", id.to_string(), JournalEntry {
            date, description, tags: TagMap::new(), postings,
        })));
    }

    if let Some(hash) = txid {
        // On-chain: use txid for cross-source reconciliation; stamp coinbase_id
        // as an informational tag for journal reference.
        let mut journal = JournalEntry { date, description, tags: TagMap::new(), postings };
        journal.tags.push("coinbase_id", id);
        if tx_type == "send" {
            if let Some(addr) = tx["to"]["address"].as_str().filter(|s| !s.is_empty()) {
                journal.tags.push("address", addr);
            }
        }
        return Ok(Some(FeedEntry::onchain(hash.to_string(), journal)));
    }

    // No txid and not lightning — use internal dedup.
    Ok(Some(FeedEntry::internal("coinbase_id", id.to_string(), JournalEntry {
        date, description, tags: TagMap::new(), postings,
    })))
}

/// Extracts the payment hash from a BOLT11 invoice by decoding the bech32 data
/// and scanning tagged fields for type `p` (value 1), which is the 256-bit
/// payment hash encoded as 52 five-bit groups.
fn bolt11_payment_hash(invoice: &str) -> Option<String> {
    let invoice = invoice.to_lowercase();
    let sep = invoice.rfind('1')?;
    let encoded = &invoice[sep + 1..];
    // Strip 6-char bech32 checksum.
    let encoded = encoded.get(..encoded.len().checked_sub(6)?)?;

    const CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
    let data: Vec<u8> = encoded.bytes()
        .map(|b| CHARSET.iter().position(|&c| c == b).map(|i| i as u8))
        .collect::<Option<Vec<_>>>()?;

    // Skip 7-group timestamp. Signature (104 groups) + recovery (1) occupy the tail.
    let mut pos = 7usize;
    let tail = data.len().saturating_sub(105);

    while pos + 3 <= tail {
        let field_type = data[pos];
        let field_len = ((data[pos + 1] as usize) << 5) | (data[pos + 2] as usize);
        pos += 3;
        if pos + field_len > data.len() { break; }

        // Type p (bech32 value 1) is the payment hash; it is always 52 groups (260 bits).
        if field_type == 1 && field_len == 52 {
            let groups = &data[pos..pos + 52];
            let mut bytes = [0u8; 32];
            let mut bit_buf = 0u32;
            let mut bit_count = 0u32;
            let mut byte_idx = 0usize;
            for &g in groups {
                bit_buf = ((bit_buf << 5) | g as u32) & 0x1FFF;
                bit_count += 5;
                if bit_count >= 8 {
                    bit_count -= 8;
                    if byte_idx < 32 {
                        bytes[byte_idx] = ((bit_buf >> bit_count) & 0xFF) as u8;
                        byte_idx += 1;
                    }
                }
            }
            if byte_idx == 32 {
                return Some(bytes.iter().map(|b| format!("{b:02x}")).collect());
            }
        }

        pos += field_len;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // Invoice from a real Coinbase Lightning send (captured during API investigation).
    // Payment hash decoded independently via https://lightningdecoder.com for comparison.
    const SAMPLE_INVOICE: &str = "lnbc2m1p4qes04pp5wz5e0p2w4hn7vqhqrcn960fxvgkw8kafuxun300pkcvj6n84ttzqcqzyssp5l7v3r8cjquf63j5eamp4zs2ms87zy242twsm920zqfp8cwgjjl4s9q7sqqqqqqqqqqqqqqqqqqqsqqqqqysgqdqqmqz9gxqyjw5qrzjqwryaup9lh50kkranzgcdnn2fgvx390wgj5jd07rwr3vxeje0glclluk0z4rmzkwrvqqqqlgqqqqqeqqjqj07c8xg5lf4d8qzrq0ja5wp4txyxrhv8hz30q5r8rmdktqqp7qak6jc9w4fhaj4v9c9w5cj8qm60gcz7maggaa8v83dhayh4lsjm74cpf5294w";

    #[test]
    fn bolt11_payment_hash_extracts_32_byte_hex() {
        let hash = bolt11_payment_hash(SAMPLE_INVOICE).expect("should decode");
        assert_eq!(hash.len(), 64, "payment hash should be 32 bytes = 64 hex chars");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "should be hex");
    }

    #[test]
    fn bolt11_payment_hash_uppercase_invoice_ok() {
        let hash_lower = bolt11_payment_hash(SAMPLE_INVOICE).unwrap();
        let hash_upper = bolt11_payment_hash(&SAMPLE_INVOICE.to_uppercase()).unwrap();
        assert_eq!(hash_lower, hash_upper);
    }

    #[test]
    fn bolt11_payment_hash_invalid_returns_none() {
        assert!(bolt11_payment_hash("not_an_invoice").is_none());
        assert!(bolt11_payment_hash("").is_none());
    }

    fn make_tx(tx_type: &str, address: &str, amount: &str, network_hash: &str) -> serde_json::Value {
        make_tx_with_fee(tx_type, address, amount, network_hash, None)
    }

    fn make_tx_with_fee(tx_type: &str, address: &str, amount: &str, network_hash: &str, fee: Option<&str>) -> serde_json::Value {
        let fee_obj = match fee {
            Some(f) => serde_json::json!({ "amount": f, "currency": "BTC" }),
            None => serde_json::Value::Null,
        };
        serde_json::json!({
            "type": tx_type,
            "status": "completed",
            "amount": { "amount": amount, "currency": "BTC" },
            "created_at": "2024-01-15T12:00:00Z",
            "id": "tx-test-id",
            "description": "",
            "to": { "address": address },
            "network": { "hash": network_hash, "network_name": "bitcoin", "transaction_fee": fee_obj }
        })
    }

    #[test]
    fn onchain_send_includes_fee_posting_when_present() {
        let addr = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq";
        let tx = make_tx_with_fee("send", addr, "-0.001", "abcd1234", Some("0.000005"));
        let entry = wallet_tx_to_entry(&tx, "assets:coinbase").unwrap().unwrap();
        let fee = entry.journal.postings.iter()
            .find(|p| p.account == "expenses:fees:onchain")
            .expect("fee posting should be present");
        assert_eq!(fee.amount.as_ref().unwrap().amount, bigdecimal::BigDecimal::from(500));
    }

    #[test]
    fn onchain_send_no_fee_when_absent() {
        let addr = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq";
        let tx = make_tx("send", addr, "-0.001", "abcd1234");
        let entry = wallet_tx_to_entry(&tx, "assets:coinbase").unwrap().unwrap();
        assert!(entry.journal.postings.iter().all(|p| p.account != "expenses:fees:onchain"));
    }

    #[test]
    fn onchain_send_tags_entry_with_destination_address() {
        let addr = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq";
        let tx = make_tx("send", addr, "-0.001", "abcd1234");
        let entry = wallet_tx_to_entry(&tx, "assets:coinbase").unwrap().unwrap();
        assert_eq!(entry.journal.tags.get("address"), Some(addr));
        let balance = entry.journal.postings.iter().find(|p| p.amount.is_none()).unwrap();
        assert!(balance.tags.get("address").is_none(), "address should be on entry header, not posting");
    }

    #[test]
    fn onchain_receive_does_not_tag_entry_with_address() {
        let tx = make_tx("receive", "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq", "0.001", "abcd1234");
        let entry = wallet_tx_to_entry(&tx, "assets:coinbase").unwrap().unwrap();
        assert!(entry.journal.tags.get("address").is_none());
    }

    #[test]
    fn lightning_send_does_not_tag_entry_with_invoice_as_address() {
        let tx = serde_json::json!({
            "type": "send",
            "status": "completed",
            "amount": { "amount": "-0.001", "currency": "BTC" },
            "created_at": "2024-01-15T12:00:00Z",
            "id": "tx-ln-id",
            "description": "",
            "to": { "address": SAMPLE_INVOICE },
            "network": { "hash": "", "network_name": "lightning" }
        });
        let entry = wallet_tx_to_entry(&tx, "assets:coinbase").unwrap().unwrap();
        assert!(entry.journal.tags.get("address").is_none(), "lightning send should not carry BOLT11 as address tag");
    }
}

fn btc_to_sat(s: &str) -> Result<i64> {
    let s = s.trim();
    let neg = s.starts_with('-');
    let s = if neg { &s[1..] } else { s };
    let mut parts = s.splitn(2, '.');
    let int_part: i64 = parts.next().unwrap_or("0").parse().context("invalid BTC amount")?;
    let frac_str = parts.next().unwrap_or("");
    let frac_padded = format!("{frac_str:0<8}");
    let frac_part: i64 = frac_padded[..8].parse().context("invalid BTC fractional part")?;
    let sat = int_part * 100_000_000 + frac_part;
    Ok(if neg { -sat } else { sat })
}
