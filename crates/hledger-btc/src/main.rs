use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::BufWriter;
use tracing_subscriber::EnvFilter;

use hledger_btc_core::{config, journal, sync};

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
    /// Sync confirmed transactions for all wallets and write hledger journals
    Sync {
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
        Command::Sync { config: config_file } => {
            let config_path = config_file.unwrap_or_else(config::config_path);
            let cfg = config::load(&config_path)?;

            for wallet_cfg in cfg.wallets.values() {
                let entries = sync::sync(wallet_cfg)?;

                let mut writer: Box<dyn std::io::Write> = match &wallet_cfg.journal_file {
                    Some(path) => Box::new(BufWriter::new(std::fs::File::create(path)?)),
                    None => Box::new(std::io::stdout()),
                };
                journal::write_journal(&entries, &mut writer)?;
            }
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
