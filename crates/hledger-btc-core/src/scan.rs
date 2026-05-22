use anyhow::Result;
use bdk_electrum::{BdkElectrumClient, electrum_client};
use bdk_wallet::Wallet;
use bdk_wallet::bitcoin::{Address, Network, Transaction};
use bdk_wallet::chain::ChainPosition;

use crate::config::WalletConfig;
use crate::journal::{JournalEntry, Posting, TagMap};

const STOP_GAP: usize = 20;
const BATCH_SIZE: usize = 5;

pub fn scan(config: &WalletConfig) -> Result<Vec<JournalEntry>> {
    let network: Network = config.network.into();

    tracing::info!("creating wallet '{}' on {:?}", config.wallet, network);
    let mut wallet = Wallet::create(config.ext_descriptor.clone(), config.int_descriptor())
        .network(network)
        .create_wallet_no_persist()?;

    tracing::info!("connecting to {}", config.server_url);
    let client = BdkElectrumClient::new(electrum_client::Client::new(&config.server_url)?);

    // fetch_prev_txouts=true fetches every input's previous output into the TxGraph so that
    // wallet.calculate_fee() works on outgoing transactions with external inputs.
    tracing::info!("scanning blockchain (stop_gap={STOP_GAP})…");
    let update = client.full_scan(wallet.start_full_scan(), STOP_GAP, BATCH_SIZE, true)?;
    wallet.apply_update(update)?;

    let base = config.account_name();
    let mut entries: Vec<JournalEntry> = wallet
        .transactions()
        .filter_map(|tx| {
            let ChainPosition::Confirmed { anchor, .. } = tx.chain_position else {
                return None;
            };
            let date = chrono::DateTime::from_timestamp(anchor.confirmation_time as i64, 0)?
                .date_naive();
            build_entry(tx.tx_node.tx.as_ref(), tx.tx_node.txid.to_string(), date, &wallet, &base, network)
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
    for output in &tx.output {
        if wallet.is_mine(output.script_pubkey.clone()) {
            if let Ok(addr) = Address::from_script(&output.script_pubkey, network) {
                postings.push(Posting::with_amount(
                    format!("{base}:{addr}"),
                    output.value.to_sat() as i64,
                ));
            }
        }
    }

    // Negative postings: inputs spending wallet UTXOs.
    for input in &tx.input {
        let prev_txid = input.previous_output.txid;
        let prev_vout = input.previous_output.vout as usize;
        if let Some(prev_tx) = wallet.tx_graph().get_tx(prev_txid) {
            if let Some(prev_out) = prev_tx.output.get(prev_vout) {
                if wallet.is_mine(prev_out.script_pubkey.clone()) {
                    if let Ok(addr) = Address::from_script(&prev_out.script_pubkey, network) {
                        postings.push(Posting::with_amount(
                            format!("{base}:{addr}"),
                            -(prev_out.value.to_sat() as i64),
                        ));
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

    // For outgoing transactions, add an on-chain fee posting.
    if net < 0 {
        if let Ok(fee) = wallet.calculate_fee(tx) {
            let fee_sat = fee.to_sat() as i64;
            if fee_sat > 0 {
                postings.push(Posting::with_amount("expenses:fees:onchain", fee_sat));
            }
        }
    }

    // Auto-balance counterpart — hledger fills in the missing amount.
    postings.push(Posting::auto_balance(counterpart));

    Some(JournalEntry {
        date,
        description: description.to_string(),
        tags: TagMap::new().add("txid", txid),
        postings,
    })
}
