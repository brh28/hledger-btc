use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::BufWriter;
use tracing_subscriber::EnvFilter;

use hledger_btc_core::{config, journal, receive, scan};

#[derive(Parser)]
#[command(name = "hledger-btc", about = "Bitcoin accounting for hledger")]
struct Cli {
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan confirmed transactions for all wallets and write hledger journals
    Scan {
        /// Config file path (default: ~/.config/hledger-btc/config.toml)
        #[arg(long)]
        config: Option<std::path::PathBuf>,
    },
    /// Generate a receiving address and record it in the journal as a receivable
    Receive {
        /// Config file path (default: ~/.config/hledger-btc/config.toml)
        #[arg(long)]
        config: Option<std::path::PathBuf>,
        /// Wallet to derive the address from (required if multiple wallets are configured)
        #[arg(long)]
        wallet: Option<String>,
        /// Date of the entry (default: today, format: YYYY-MM-DD)
        #[arg(long)]
        date: Option<chrono::NaiveDate>,
        /// Description for the journal entry (default: "Awaiting Payment")
        #[arg(long)]
        description: Option<String>,
        /// Expected amount in satoshis (default: 0)
        #[arg(long)]
        amount: Option<i64>,
        /// Per-unit price annotation, e.g. "USD 0.00045" (mutually exclusive with --total-cost)
        #[arg(long, conflicts_with = "total_cost")]
        unit_price: Option<String>,
        /// Total cost annotation, e.g. "USD 45" (mutually exclusive with --unit-price)
        #[arg(long, conflicts_with = "unit_price")]
        total_cost: Option<String>,
    },
    /// Print the transaction history for a given address
    Trace {
        /// Bitcoin address to trace
        address: String,
        /// Config file path (default: ~/.config/hledger-btc/config.toml)
        #[arg(long)]
        config: Option<std::path::PathBuf>,
    },
    /// Import BIP329 labels into hledger journal
    Import {
        #[arg(short, long)]
        file: Option<std::path::PathBuf>,
    },
    /// Export hledger journal to BIP329 labels
    Export {
        #[arg(short, long)]
        file: Option<std::path::PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let filter = match cli.verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .init();

    match cli.command {
        Command::Scan { config: config_file } => {
            let config_path = config_file.unwrap_or_else(config::config_path);
            let cfg = config::load(&config_path)?;

            for wallet_cfg in cfg.wallets.values() {
                let entries = scan::scan(wallet_cfg)?;

                match &wallet_cfg.journal_file {
                    Some(path) => {
                        let known = if path.exists() {
                            journal::read_txids(std::fs::File::open(path)?)?
                        } else {
                            std::collections::HashSet::new()
                        };
                        let new_entries: Vec<_> = entries.into_iter()
                            .filter(|e| {
                                e.tags.0.iter()
                                    .find(|(k, _)| k == "txid")
                                    .map_or(true, |(_, v)| !known.contains(v))
                            })
                            .collect();
                        tracing::info!("{} new entries, {} already in journal", new_entries.len(), known.len());
                        if !new_entries.is_empty() {
                            let file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
                            journal::write_entries(&new_entries, &mut BufWriter::new(file))?;
                        }
                    }
                    None => {
                        journal::write_entries(&entries, &mut std::io::stdout())?;
                    }
                }
            }
        }
        Command::Receive { config: config_file, wallet, date, description, amount, unit_price, total_cost } => {
            let config_path = config_file.unwrap_or_else(config::config_path);
            let cfg = config::load(&config_path)?;

            let wallet_cfg = match wallet {
                Some(ref name) => cfg.wallets.get(name)
                    .ok_or_else(|| anyhow::anyhow!("wallet '{name}' not found in config"))?,
                None => match cfg.wallets.len() {
                    0 => anyhow::bail!("no wallets configured"),
                    1 => cfg.wallets.values().next().unwrap(),
                    _ => anyhow::bail!("multiple wallets configured, specify one with --wallet"),
                },
            };

            let price = match (unit_price, total_cost) {
                (Some(p), _) => Some(journal::PriceAnnotation::Unit(p)),
                (_, Some(c)) => Some(journal::PriceAnnotation::Total(c)),
                _ => None,
            };

            let params = receive::ReceiveParams {
                date: date.unwrap_or_else(|| chrono::Local::now().date_naive()),
                description: description.unwrap_or_else(|| "Awaiting Payment".to_string()),
                amount_sat: amount.unwrap_or(0),
                price,
            };

            let entry = receive::receive(wallet_cfg, params)?;

            if let Some((_, addr)) = entry.tags.0.iter().find(|(k, _)| k == "address") {
                eprintln!("address: {addr}");
            }

            match &wallet_cfg.journal_file {
                Some(path) => {
                    let file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
                    journal::write_entries(&[entry], &mut BufWriter::new(file))?;
                }
                None => journal::write_entries(&[entry], &mut std::io::stdout())?,
            }
        }
        Command::Trace { address: _, config: _ } => {
            todo!("trace")
        }
        Command::Import { file: _ } => {
            todo!("import")
        }
        Command::Export { file: _ } => {
            todo!("export")
        }
    }

    Ok(())
}
