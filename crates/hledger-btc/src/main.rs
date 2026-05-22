use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::BufWriter;
use tracing_subscriber::EnvFilter;

use hledger_btc_core::{config, journal, scan};

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
    /// Generate or register a receiving address and record it in the journal as a receivable
    Receive {
        /// Config file path (default: ~/.config/hledger-btc/config.toml)
        #[arg(long)]
        config: Option<std::path::PathBuf>,
        /// Bitcoin address to record; if omitted a new address is derived from the wallet
        #[arg(long)]
        address: Option<String>,
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
        Command::Receive { config: _, address: _ } => {
            todo!("receive")
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
