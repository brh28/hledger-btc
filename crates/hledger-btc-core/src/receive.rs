use chrono::NaiveDate;

use crate::journal::{JournalEntry, Posting, PriceAnnotation, TagMap};

pub struct ReceiveParams {
    pub address: String,
    pub account: Option<String>,
    pub date: NaiveDate,
    pub description: String,
    /// Stored as `expected:<sat>` tag, not as a posting amount (cash accounting).
    pub expected_sat: Option<i64>,
    pub price: Option<PriceAnnotation>,
    pub credit_account: Option<String>,
    pub extra_tags: Vec<(String, String)>,
}

pub fn receive(params: ReceiveParams) -> JournalEntry {
    let addr = &params.address;
    let base = params.account.as_deref().unwrap_or("assets:bitcoin");
    let income = params.credit_account.as_deref().unwrap_or("income:unknown");

    let mut tags = TagMap::new().add("address", addr);
    for (k, v) in &params.extra_tags {
        tags.push(k, v);
    }
    if let Some(expected) = params.expected_sat {
        tags.push("expected", expected.to_string());
    }

    JournalEntry {
        date: params.date,
        description: params.description,
        tags,
        postings: vec![
            Posting::with_amount(format!("{base}:receivable:{addr}"), 0)
                .with_price(params.price),
            Posting::auto_balance(income),
        ],
        status: Some(false),
    }
}
