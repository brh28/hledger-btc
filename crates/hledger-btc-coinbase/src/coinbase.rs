use std::path::Path;
use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::NaiveDate;
use p256::ecdsa::{SigningKey, signature::Signer};
use serde_json::Value;

use hledger_btc_core::journal::{Account, JournalEntry, Posting, PriceAnnotation, TagMap};
use hledger_btc_core::money::Money;
use hledger_btc_core::source::Source;

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

    fn fetch_orders(&self) -> Result<Vec<JournalEntry>> {
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

    fn fetch_wallet_transactions(&self, account_id: &str) -> Result<Vec<JournalEntry>> {
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

    fn entries(&self) -> Result<Vec<JournalEntry>> {
        let account_id = self.fetch_btc_account_id()?;
        let mut entries = self.fetch_orders()?;
        entries.extend(self.fetch_wallet_transactions(&account_id)?);
        entries.sort_by_key(|e| e.date);
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

fn order_to_entry(order: &Value, account: &str) -> Result<Option<JournalEntry>> {
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

    Ok(Some(JournalEntry {
        date,
        description: description.to_string(),
        tags: TagMap::new().add("coinbase_id", order_id),
        postings: vec![
            Posting::with_amount(format!("{}:btc", account), btc_amount).with_price(price),
            Posting::with_money("expenses:fees:coinbase", fee),
            Posting::auto_balance(format!("{}:usd", account)),
        ],
    }))
}

fn wallet_tx_to_entry(tx: &Value, account: &str) -> Result<Option<JournalEntry>> {
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
    let txid = tx["network"]["hash"].as_str().filter(|s| !s.is_empty());
    let description = tx["description"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(if tx_type == "send" { "Coinbase Send" } else { "Coinbase Receive" })
        .to_string();
    let balance_account = if amount_sat < 0 { "expenses:unknown" } else { "income:unknown" };

    let mut tags = TagMap::new().add("coinbase_id", id);
    if let Some(hash) = txid {
        tags = tags.add("txid", hash);
    }

    Ok(Some(JournalEntry {
        date,
        description,
        tags,
        postings: vec![
            Posting::with_amount(btc_account, amount_sat),
            Posting::auto_balance(balance_account),
        ],
    }))
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
