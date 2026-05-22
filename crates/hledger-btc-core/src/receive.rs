use anyhow::Result;
use bdk_wallet::{KeychainKind, Wallet};
use chrono::NaiveDate;

use crate::config::WalletConfig;
use crate::journal::{JournalEntry, Posting, PriceAnnotation, TagMap};
use crate::persist::WalletStore;

pub struct ReceiveParams {
    pub date: NaiveDate,
    pub description: String,
    pub amount_sat: i64,
    pub price: Option<PriceAnnotation>,
}

pub fn receive(config: &WalletConfig, params: ReceiveParams) -> Result<JournalEntry> {
    let mut db = WalletStore::load_or_create(&config.state_path())?;
    let mut wallet = match Wallet::load()
        .descriptor(KeychainKind::External, Some(config.ext_descriptor.clone()))
        .descriptor(KeychainKind::Internal, Some(config.int_descriptor()))
        .load_wallet(&mut db)?
    {
        Some(w) => w,
        None => Wallet::create(config.ext_descriptor.clone(), config.int_descriptor())
            .network(config.network.into())
            .create_wallet(&mut db)?,
    };
    let address = wallet.reveal_next_address(KeychainKind::External).address.to_string();
    wallet.persist(&mut db)?;

    let base = config.account_name();
    Ok(JournalEntry {
        date: params.date,
        description: params.description,
        tags: TagMap::new().add("address", &address),
        postings: vec![
            Posting::with_amount(format!("{base}:receivable:{address}"), params.amount_sat)
                .with_price(params.price),
            Posting::auto_balance("income:btc:unclassified"),
        ],
    })
}
