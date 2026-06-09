use chrono::NaiveDate;

use crate::journal::{JournalEntry, Posting, PriceAnnotation, TagMap};

pub struct ReceiveParams {
    pub address: String,
    pub account: Option<String>,
    pub date: NaiveDate,
    pub description: String,
    pub amount_sat: i64,
    pub price: Option<PriceAnnotation>,
}

pub fn receive(params: ReceiveParams) -> JournalEntry {
    let addr = &params.address;
    let base = params.account.as_deref().unwrap_or("assets:bitcoin");
    JournalEntry {
        date: params.date,
        description: params.description,
        tags: TagMap::new().add("address", addr),
        postings: vec![
            Posting::with_amount(format!("{base}:receivable:{addr}"), params.amount_sat)
                .with_price(params.price),
            Posting::auto_balance("income:btc:unclassified"),
        ],
    }
}
