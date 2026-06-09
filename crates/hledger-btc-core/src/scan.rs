use anyhow::Result;
use bdk_electrum::{BdkElectrumClient, electrum_client};
use bdk_wallet::{KeychainKind, Wallet};
use bdk_wallet::bitcoin::{Address, Network, Transaction};
use bdk_wallet::chain::ChainPosition;

use crate::config::{Config, WalletConfig};
use crate::journal::{JournalEntry, Posting, TagMap};
use crate::persist::WalletStore;

const STOP_GAP: usize = 20;
const BATCH_SIZE: usize = 5;

pub fn scan(cfg: &Config, wallet: &WalletConfig) -> Result<Vec<JournalEntry>> {
    let network: Network = cfg.network.into();

    let mut db = WalletStore::load_or_create(&wallet.state_path())?;

    let mut bdk_wallet = match Wallet::load()
        .descriptor(KeychainKind::External, Some(wallet.ext_descriptor.clone()))
        .descriptor(KeychainKind::Internal, Some(wallet.int_descriptor()))
        .load_wallet(&mut db)?
    {
        Some(w) => {
            tracing::info!("loaded wallet '{}' from state ({:?})", wallet.wallet, wallet.state_path());
            w
        }
        None => {
            tracing::info!("initializing new wallet '{}' on {:?}", wallet.wallet, network);
            Wallet::create(wallet.ext_descriptor.clone(), wallet.int_descriptor())
                .network(network)
                .create_wallet(&mut db)?
        }
    };

    tracing::info!("connecting to {}", cfg.server_url);
    let client = BdkElectrumClient::new(electrum_client::Client::new(&cfg.server_url)?);

    tracing::info!("scanning blockchain (stop_gap={STOP_GAP})…");
    let update = client.full_scan(bdk_wallet.start_full_scan(), STOP_GAP, BATCH_SIZE, true)?;
    bdk_wallet.apply_update(update)?;
    bdk_wallet.persist(&mut db)?;

    let base = wallet.account_name(&cfg.base_account);
    let mut entries: Vec<JournalEntry> = bdk_wallet
        .transactions()
        .filter_map(|tx| {
            let ChainPosition::Confirmed { anchor, .. } = tx.chain_position else {
                return None;
            };
            let date = chrono::DateTime::from_timestamp(anchor.confirmation_time as i64, 0)?
                .date_naive();
            build_entry(tx.tx_node.tx.as_ref(), tx.tx_node.txid.to_string(), date, &bdk_wallet, &base, network)
        })
        .collect();

    entries.sort_by_key(|e| e.date);
    tracing::info!("found {} confirmed transactions", entries.len());
    Ok(entries)
}

fn build_entry(
    tx: &Transaction,
    txid: String,
    date: chrono::NaiveDate,
    wallet: &Wallet,
    base: &str,
    network: Network,
) -> Option<JournalEntry> {
    let mut postings: Vec<Posting> = Vec::new();

    // Positive postings: outputs going to wallet addresses.
    for (vout, output) in tx.output.iter().enumerate() {
        if wallet.is_mine(output.script_pubkey.clone()) {
            if let Ok(addr) = Address::from_script(&output.script_pubkey, network) {
                postings.push(Posting::with_amount(
                    format!("{base}:{addr}"),
                    output.value.to_sat() as i64,
                ).with_tags(TagMap::new().add("vout", vout.to_string())));
            }
        }
    }

    // Negative postings: inputs spending wallet UTXOs.
    for (idx, input) in tx.input.iter().enumerate() {
        let prev_txid = input.previous_output.txid;
        let prev_vout = input.previous_output.vout as usize;
        if let Some(prev_tx) = wallet.tx_graph().get_tx(prev_txid) {
            if let Some(prev_out) = prev_tx.output.get(prev_vout) {
                if wallet.is_mine(prev_out.script_pubkey.clone()) {
                    if let Ok(addr) = Address::from_script(&prev_out.script_pubkey, network) {
                        postings.push(Posting::with_amount(
                            format!("{base}:{addr}"),
                            -(prev_out.value.to_sat() as i64),
                        ).with_tags(TagMap::new().add("input", idx.to_string())));
                    }
                }
            }
        }
    }

    if postings.is_empty() {
        return None;
    }

    let net: i64 = postings.iter().filter_map(|p| p.amount_sat).sum();
    let (description, counterpart) = if net >= 0 {
        ("Incoming BTC", "income:unknown")
    } else {
        ("Outgoing BTC", "expenses:unknown")
    };

    if net < 0 {
        if let Ok(fee) = wallet.calculate_fee(tx) {
            let fee_sat = fee.to_sat() as i64;
            if fee_sat > 0 {
                postings.push(Posting::with_amount("expenses:fees:onchain", fee_sat));
            }
        }
    }

    postings.push(Posting::auto_balance(counterpart));

    Some(JournalEntry {
        date,
        description: description.to_string(),
        tags: TagMap::new().add("txid", txid),
        postings,
    })
}
