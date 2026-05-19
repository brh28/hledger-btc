use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;
use std::io::BufWriter;

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
    /// Initialize wallet config
    Init,
    /// Sync transactions from the blockchain
    Sync,
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
        Command::Init => todo!("init"),
        Command::Sync => todo!("sync"),
        Command::Import { file } => {
            let reader: Box<dyn std::io::Read> = match file {
                Some(path) => Box::new(std::fs::File::open(path)?),
                None => Box::new(std::io::stdin()),
            };
            todo!("import")
        }
        Command::Export { file } => {
            let writer: Box<dyn std::io::Write> = match file {
                Some(path) => Box::new(BufWriter::new(std::fs::File::create(path)?)),
                None => Box::new(std::io::stdout()),
            };
            todo!("export")
        }
    }
}
