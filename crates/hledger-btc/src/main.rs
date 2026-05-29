use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::BufWriter;
use std::process::Stdio;
use tracing_subscriber::EnvFilter;

use std::io::Read;

use hledger_btc_core::{config, export, import, journal, receive, scan};

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
        /// Path to JSONL file; reads stdin if omitted
        #[arg(short, long)]
        file: Option<std::path::PathBuf>,

        /// Wallet name to associate with all imported annotations;
        /// defaults to the filename stem when --file is provided,
        /// required when reading from stdin
        #[arg(short, long)]
        wallet: Option<String>,

        /// Replace existing label tags instead of skipping already-labelled entries
        #[arg(long = "override")]
        override_existing: bool,

        /// Config file path (default: ~/.config/hledger-btc/config.toml)
        #[arg(long)]
        config: Option<std::path::PathBuf>,
    },
    /// Export hledger journal to BIP329 labels
    Export {
        /// Output JSONL file; writes to stdout if omitted
        #[arg(short, long)]
        file: Option<std::path::PathBuf>,

        /// Wallet to export (required if multiple wallets are configured)
        #[arg(short, long)]
        wallet: Option<String>,

        /// Config file path (default: ~/.config/hledger-btc/config.toml)
        #[arg(long)]
        config: Option<std::path::PathBuf>,
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

            // Scan all wallets first, then merge so inter-wallet transfers become
            // a single transaction instead of being deduplicated away.
            let mut raw: Vec<journal::JournalEntry> = Vec::new();
            for wallet_cfg in cfg.wallets.values() {
                raw.extend(scan::scan(wallet_cfg)?);
            }
            let all_entries = journal::merge_by_txid(raw);

            // Use the first wallet with a journal_file as the write target.
            match cfg.wallets.values().find(|w| w.journal_file.is_some()) {
                None => {
                    journal::write_entries(&all_entries, &mut std::io::stdout())?;
                }
                Some(write_cfg) => {
                    let read_path = write_cfg.journal_file.as_ref().unwrap();
                    let write_path = write_cfg.output_file.as_ref().unwrap_or(read_path);

                    tracing::info!("reading known txids via hledger from {:?}", read_path);
                    let known = {
                        let child = std::process::Command::new("hledger")
                            .args(["-f", read_path.to_str().unwrap(), "print"])
                            .stdout(Stdio::piped())
                            .spawn()?;
                        journal::read_txids(child.stdout.unwrap())?
                    };

                    let new_entries: Vec<_> = all_entries.into_iter()
                        .filter(|e| {
                            e.tags.0.iter()
                                .find(|(k, _)| k == "txid")
                                .map_or(true, |(_, v)| !known.contains(v))
                        })
                        .collect();
                    tracing::info!("{} new entries, {} already in journal", new_entries.len(), known.len());
                    if !new_entries.is_empty() {
                        let file = std::fs::OpenOptions::new().create(true).append(true).open(write_path)?;
                        journal::write_entries(&new_entries, &mut BufWriter::new(file))?;
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
        Command::Import { file, wallet, override_existing, config: config_file } => {
            let config_path = config_file.unwrap_or_else(config::config_path);
            let cfg = config::load(&config_path)?;

            let wallet_name: String = match (&file, &wallet) {
                (_, Some(w)) => w.clone(),
                (Some(path), None) => path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(String::from)
                    .ok_or_else(|| anyhow::anyhow!("could not derive wallet name from file path"))?,
                (None, None) => anyhow::bail!("--wallet is required when reading from stdin"),
            };

            let wallet_cfg = cfg.wallets.get(&wallet_name)
                .ok_or_else(|| anyhow::anyhow!("wallet '{wallet_name}' not found in config"))?;

            let journal_path = wallet_cfg.journal_file.as_ref()
                .ok_or_else(|| anyhow::anyhow!(
                    "wallet '{}' has no journal_file configured", wallet_cfg.wallet
                ))?;

            if !journal_path.exists() {
                anyhow::bail!("journal file does not exist: {}", journal_path.display());
            }

            let bip329_content = match &file {
                Some(path) => std::fs::read_to_string(path)?,
                None => {
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf)?;
                    buf
                }
            };

            let journal_content = std::fs::read_to_string(journal_path)?;
            let updated = import::import_from_str(&journal_content, &bip329_content, override_existing)?;
            std::fs::write(journal_path, updated)?;

            tracing::info!("labels imported into {}", journal_path.display());
        }
        Command::Export { file, wallet, config: config_file } => {
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

            let journal_path = wallet_cfg.journal_file.as_ref()
                .ok_or_else(|| anyhow::anyhow!(
                    "wallet '{}' has no journal_file configured", wallet_cfg.wallet
                ))?;

            if !journal_path.exists() {
                anyhow::bail!("journal file does not exist: {}", journal_path.display());
            }

            let journal_content = std::fs::read_to_string(journal_path)?;
            let bip329_output = export::export_to_string(&journal_content)?;

            match file {
                Some(path) => std::fs::write(&path, bip329_output)?,
                None => print!("{bip329_output}"),
            }
        }
    }

    Ok(())
}
