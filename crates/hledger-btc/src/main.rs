use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::BufWriter;
use std::io::Read;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

use std::collections::BTreeMap;
use hledger_btc_core::{annotate::{Annotation, AnnotationType}, export, import, journal, label, receive, scan::WalletSource, source::Source, trace};
use hledger_btc_core::config::WalletConfig;

mod config;
mod feeds;
mod pipeline;

#[derive(Parser)]
#[command(name = "hledger-btc", about = "Bitcoin accounting for hledger")]
struct Cli {
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Config file path (default: ~/.config/hledger-btc/config.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan all configured wallets on-chain and write new entries to the journal
    Scan {
        /// Journal file to read for dedup; falls back to LEDGER_FILE, then ~/.hledger.journal
        #[arg(short = 'f', long = "file")]
        journal: Option<PathBuf>,

        /// Journal file to append new entries to; defaults to the value of -f/--file
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,

        /// Skip reconciling existing entries with novel source data
        #[arg(long)]
        no_reconcile: bool,
    },
    /// Import data into the journal
    Import {
        /// Journal file; falls back to LEDGER_FILE, then ~/.hledger.journal
        #[arg(short = 'f', long = "file")]
        journal: Option<PathBuf>,

        /// Output file for new entries; defaults to -f/--file (unused by `labels`)
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,

        /// Skip reconciling existing entries with novel source data
        #[arg(long)]
        no_reconcile: bool,

        #[command(subcommand)]
        subcommand: ImportSubcommand,
    },
    /// Record a receiving address as a receivable in the journal
    Receive {
        /// Journal file to append entry to; falls back to LEDGER_FILE, then ~/.hledger.journal
        #[arg(short = 'f', long = "file")]
        journal: Option<PathBuf>,

        /// Bitcoin address to record
        #[arg(long)]
        address: String,

        /// Base account for the receivable posting; overrides config base_account
        #[arg(long)]
        account: Option<String>,

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
    /// Print the visibility footprint for a given address
    Trace {
        /// Bitcoin address to trace
        address: String,

        /// Journal file; falls back to LEDGER_FILE, then ~/.hledger.journal
        #[arg(short = 'f', long = "file")]
        journal: Option<PathBuf>,
    },
    /// Export hledger journal to BIP329 labels
    Export {
        /// Journal file to read labels from; falls back to LEDGER_FILE, then ~/.hledger.journal
        #[arg(short = 'f', long = "file")]
        journal: Option<PathBuf>,

        /// Output JSONL file; writes to stdout if omitted
        output: Option<PathBuf>,
    },
    /// Set a label on a transaction, address, or posting
    Label {
        /// Journal file; falls back to LEDGER_FILE, then ~/.hledger.journal
        #[arg(short = 'f', long = "file")]
        journal: Option<PathBuf>,

        #[command(subcommand)]
        subcommand: LabelSubcommand,
    },
    /// Set a tag on a transaction, address, or posting
    Tag {
        /// Journal file; falls back to LEDGER_FILE, then ~/.hledger.journal
        #[arg(short = 'f', long = "file")]
        journal: Option<PathBuf>,

        /// Replace existing tag value instead of skipping already-tagged entries
        #[arg(long = "override")]
        override_existing: bool,

        #[command(subcommand)]
        subcommand: TagSubcommand,
    },
    /// Manage the hledger-btc configuration
    Config {
        #[command(subcommand)]
        subcommand: ConfigSubcommand,
    },
}

#[derive(Subcommand)]
enum ImportSubcommand {
    /// Apply a BIP329 label file to the journal
    Labels {
        /// BIP329 JSONL file to read labels from; reads stdin if omitted
        file: Option<PathBuf>,

        /// Replace existing label tags instead of skipping already-labelled entries
        #[arg(long = "override")]
        override_existing: bool,
    },
    /// Import from a third-party feed provider
    Feed {
        #[command(subcommand)]
        provider: FeedProvider,
    },
}

#[derive(Subcommand)]
enum FeedProvider {
    #[cfg(feature = "phoenix")]
    /// Import from a Phoenix Lightning wallet CSV export
    Phoenix {
        /// Path to Phoenix CSV export
        #[arg(long)]
        path: PathBuf,
        /// Account sub-segment for journal postings (default: "phoenix")
        #[arg(long)]
        name: Option<String>,
    },
    #[cfg(feature = "coinbase")]
    /// Import from Coinbase via API
    Coinbase {
        /// Path to CDP API key file; overrides config
        #[arg(long)]
        key_file: Option<PathBuf>,
        /// Account sub-segment and config entry name (default: first coinbase entry in config)
        #[arg(long)]
        name: Option<String>,
    },
    #[cfg(feature = "cashapp")]
    /// Import from Cash App CSV export
    Cashapp {
        /// Path to Cash App CSV export
        #[arg(long)]
        path: PathBuf,
        /// Account sub-segment for journal postings (default: "cashapp")
        #[arg(long)]
        name: Option<String>,
    },
}

#[derive(Subcommand)]
enum LabelSubcommand {
    /// Label a transaction by txid
    Tx { txid: String, label: String },
    /// Label an address
    Addr { address: String, label: String },
    /// Label a transaction output (ref: txid:vout)
    Output { ref_: String, label: String },
    /// Label a transaction input (ref: txid:index)
    Input { ref_: String, label: String },
}

#[derive(Subcommand)]
enum TagSubcommand {
    /// Tag a transaction by txid (key=value ...)
    Tx { txid: String, assignments: Vec<String> },
    /// Tag an address (key=value ...)
    Addr { address: String, assignments: Vec<String> },
    /// Tag a transaction output (ref: txid:vout, key=value ...)
    Output { ref_: String, assignments: Vec<String> },
    /// Tag a transaction input (ref: txid:index, key=value ...)
    Input { ref_: String, assignments: Vec<String> },
}

#[derive(Subcommand)]
enum ConfigSubcommand {
    /// Print the config file path
    Path,
    /// Print the current configuration
    Show,
    /// Set configuration fields
    Set {
        /// Bitcoin network: bitcoin, testnet, signet, regtest
        #[arg(long)]
        network: Option<String>,
        /// Electrum server URL, e.g. ssl://electrum.blockstream.info:50002
        #[arg(long)]
        server_url: Option<String>,
        /// Client type (default: electrum)
        #[arg(long)]
        client_type: Option<String>,
        /// Base account prefix for all wallet and feed postings
        #[arg(long)]
        base_account: Option<String>,
    },
    /// Manage wallets in the configuration
    Wallet {
        #[command(subcommand)]
        subcommand: WalletSubcommand,
    },
    /// Manage feeds in the configuration
    Feed {
        #[command(subcommand)]
        subcommand: FeedSubcommand,
    },
}

#[derive(Subcommand)]
enum FeedSubcommand {
    /// List configured feeds
    List,
}

#[derive(Subcommand)]
enum WalletSubcommand {
    /// Add a wallet to the configuration
    Add {
        /// Wallet name
        #[arg(long)]
        name: String,
        /// External xpub descriptor, e.g. wpkh([df9d4f28/84h/0h/0h]xpub.../0/*)
        #[arg(long)]
        descriptor: String,
    },
    /// Remove a wallet from the configuration
    Remove {
        /// Wallet name to remove
        #[arg(long)]
        name: String,
    },
}

fn write_annotation(journal_path: &PathBuf, annotation: Annotation, override_existing: bool) -> Result<()> {
    if !journal_path.exists() {
        anyhow::bail!("journal file does not exist: {}", journal_path.display());
    }
    let content = std::fs::read_to_string(journal_path)?;
    let updated = import::annotate_journal(&content, &annotation, override_existing);
    std::fs::write(journal_path, updated)?;
    Ok(())
}

fn parse_assignments(assignments: Vec<String>) -> Result<BTreeMap<String, String>> {
    assignments.into_iter().map(|a| {
        let (k, v) = a.split_once('=')
            .ok_or_else(|| anyhow::anyhow!("expected key=value, got '{a}'"))?;
        Ok((k.to_string(), v.to_string()))
    }).collect()
}

fn resolve_journal(explicit: Option<PathBuf>) -> PathBuf {
    explicit
        .or_else(|| std::env::var("LEDGER_FILE").ok().map(PathBuf::from))
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".hledger.journal")
        })
}

fn resolve_config(explicit: Option<PathBuf>) -> PathBuf {
    explicit.unwrap_or_else(config::config_path)
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

    let config_path = resolve_config(cli.config);

    match cli.command {
        Command::Scan { journal, output, no_reconcile } => {
            let config = config::load(&config_path)?;
            let cfg = config.to_core();
            let srcs: Vec<Box<dyn Source + '_>> = cfg.wallets.iter()
                .filter(|w| !w.archived)
                .map(|w| Box::new(WalletSource { cfg: &cfg, wallet: w }) as Box<dyn Source + '_>)
                .collect();
            let journal_path = resolve_journal(journal);
            let output_path = output.unwrap_or_else(|| journal_path.clone());
            pipeline::run_pipeline(&srcs, &journal_path, &output_path, !no_reconcile)?;
        }
        Command::Import { journal, output, no_reconcile, subcommand } => {
            let journal_path = resolve_journal(journal);
            match subcommand {
                ImportSubcommand::Labels { file, override_existing } => {
                    if !journal_path.exists() {
                        anyhow::bail!("journal file does not exist: {}", journal_path.display());
                    }
                    let bip329_content = match file {
                        Some(path) => std::fs::read_to_string(path)?,
                        None => {
                            let mut buf = String::new();
                            std::io::stdin().read_to_string(&mut buf)?;
                            buf
                        }
                    };
                    let journal_content = std::fs::read_to_string(&journal_path)?;
                    let updated = import::import_from_str(&journal_content, &bip329_content, override_existing)?;
                    std::fs::write(&journal_path, updated)?;
                    tracing::info!("labels imported into {}", journal_path.display());
                }
                ImportSubcommand::Feed { provider } => {
                    let config = config::load(&config_path).ok();
                    let base = config.as_ref()
                        .map(|c| c.base_account.clone())
                        .unwrap_or_else(|| journal::Account::new("assets"));

                    let feed: Box<dyn Source + 'static> = match provider {
                        #[cfg(feature = "phoenix")]
                        FeedProvider::Phoenix { path, name } => {
                            let account = base.append(name.as_deref().unwrap_or("phoenix"));
                            Box::new(hledger_btc_phoenix::PhoenixFeed::new(path, account))
                        }
                        #[cfg(feature = "cashapp")]
                        FeedProvider::Cashapp { path, name } => {
                            let account = base.append(name.as_deref().unwrap_or("cashapp"));
                            Box::new(hledger_btc_cashapp::CashAppFeed::new(path, account))
                        }
                        #[cfg(feature = "coinbase")]
                        FeedProvider::Coinbase { key_file, name } => match key_file {
                            Some(kf) => {
                                let account = base.append(name.as_deref().unwrap_or("coinbase"));
                                Box::new(hledger_btc_coinbase::CoinbaseFeed::new(&kf, account)?)
                            }
                            None => {
                                let config = config.ok_or_else(|| anyhow::anyhow!(
                                    "no --key-file given and no config found; use --key-file to import without configuring"
                                ))?;
                                let entry = config.find_feed("coinbase", name.as_deref())?;
                                feeds::build_feed(&config.to_core(), entry)?
                            }
                        },
                    };

                    let srcs: Vec<Box<dyn Source + '_>> = vec![feed];
                    let output_path = output.unwrap_or_else(|| journal_path.clone());
                    pipeline::run_pipeline(&srcs, &journal_path, &output_path, !no_reconcile)?;
                }
            }
        }
        Command::Receive { journal, address, account, date, description, amount, unit_price, total_cost } => {
            let resolved_account = account.or_else(|| {
                config::load(&config_path).ok().map(|c| c.base_account.to_string())
            });

            let price = match (unit_price, total_cost) {
                (Some(p), _) => Some(journal::PriceAnnotation::Unit(p)),
                (_, Some(c)) => Some(journal::PriceAnnotation::Total(c)),
                _ => None,
            };

            let entry = receive::receive(receive::ReceiveParams {
                address,
                account: resolved_account,
                date: date.unwrap_or_else(|| chrono::Local::now().date_naive()),
                description: description.unwrap_or_else(|| "Awaiting Payment".to_string()),
                amount_sat: amount.unwrap_or(0),
                price,
            });

            let journal_path = resolve_journal(journal);
            let file = std::fs::OpenOptions::new().create(true).append(true).open(&journal_path)?;
            journal::write_entries(&[entry], &mut BufWriter::new(file))?;
        }
        Command::Trace { address, journal } => {
            let journal_path = resolve_journal(journal);
            let output = std::process::Command::new("hledger")
                .args(["-f", journal_path.to_str().unwrap(), "print"])
                .output()?;
            if !output.status.success() {
                anyhow::bail!("hledger print failed: {}", String::from_utf8_lossy(&output.stderr));
            }
            let content = String::from_utf8(output.stdout)?;
            let blocks = trace::trace(&content, &address);
            if blocks.is_empty() {
                println!("Address not found in journal: {address}");
            } else {
                for block in &blocks {
                    println!("{block}\n");
                }
            }
        }
        Command::Export { journal, output } => {
            let journal_path = resolve_journal(journal);
            let hledger_out = std::process::Command::new("hledger")
                .args(["-f", journal_path.to_str().unwrap(), "print"])
                .output()?;
            if !hledger_out.status.success() {
                anyhow::bail!("hledger print failed: {}", String::from_utf8_lossy(&hledger_out.stderr));
            }
            let journal_content = String::from_utf8(hledger_out.stdout)?;
            let bip329_output = export::export_to_string(&journal_content)?;

            match output {
                Some(path) => std::fs::write(&path, bip329_output)?,
                None => print!("{bip329_output}"),
            }
        }
        Command::Label { journal, subcommand } => {
            let journal_path = resolve_journal(journal);
            if !journal_path.exists() {
                anyhow::bail!("journal file does not exist: {}", journal_path.display());
            }
            let (type_, ref_, lbl) = match subcommand {
                LabelSubcommand::Tx     { txid, label }    => (AnnotationType::Tx,     txid,    label),
                LabelSubcommand::Addr   { address, label } => (AnnotationType::Addr,   address, label),
                LabelSubcommand::Output { ref_, label }    => (AnnotationType::Output, ref_,    label),
                LabelSubcommand::Input  { ref_, label }    => (AnnotationType::Input,  ref_,    label),
            };
            let content = std::fs::read_to_string(&journal_path)?;
            let updated = label::set_label(&content, &type_, &ref_, &lbl);
            std::fs::write(&journal_path, updated)?;
        }
        Command::Tag { journal, override_existing, subcommand } => {
            let journal_path = resolve_journal(journal);
            let (type_, ref_, assignments) = match subcommand {
                TagSubcommand::Tx     { txid, assignments }    => (AnnotationType::Tx,     txid,    assignments),
                TagSubcommand::Addr   { address, assignments } => (AnnotationType::Addr,   address, assignments),
                TagSubcommand::Output { ref_, assignments }    => (AnnotationType::Output, ref_,    assignments),
                TagSubcommand::Input  { ref_, assignments }    => (AnnotationType::Input,  ref_,    assignments),
            };
            let annotation = Annotation { type_, ref_, label: None, tags: parse_assignments(assignments)? };
            write_annotation(&journal_path, annotation, override_existing)?;
        }
        Command::Config { subcommand } => match subcommand {
            ConfigSubcommand::Path => {
                println!("{}", config_path.display());
            }
            ConfigSubcommand::Show => {
                let content = std::fs::read_to_string(&config_path)
                    .map_err(|_| anyhow::anyhow!("config not found at {}", config_path.display()))?;
                print!("{content}");
            }
            ConfigSubcommand::Set { network, server_url, client_type, base_account } => {
                let mut value: toml::Value = if config_path.exists() {
                    let content = std::fs::read_to_string(&config_path)?;
                    toml::from_str(&content)?
                } else {
                    toml::Value::Table(toml::map::Map::new())
                };

                let table = value.as_table_mut()
                    .ok_or_else(|| anyhow::anyhow!("invalid config format"))?;

                if let Some(v) = base_account {
                    table.insert("base_account".to_string(), toml::Value::String(v));
                }

                if network.is_some() || server_url.is_some() || client_type.is_some() {
                    let scan = table.entry("scan".to_string())
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
                    let scan_table = scan.as_table_mut()
                        .ok_or_else(|| anyhow::anyhow!("'scan' in config must be a table"))?;
                    if let Some(v) = network     { scan_table.insert("network".to_string(),    toml::Value::String(v)); }
                    if let Some(v) = server_url  { scan_table.insert("server_url".to_string(), toml::Value::String(v)); }
                    if let Some(v) = client_type { scan_table.insert("client_type".to_string(), toml::Value::String(v)); }
                }

                let scan_table = table.get("scan").and_then(|s| s.as_table());
                for field in ["network", "server_url"] {
                    anyhow::ensure!(
                        scan_table.map(|t| t.contains_key(field)).unwrap_or(false),
                        "missing required field 'scan.{field}' — set it with --{}",
                        field.replace('_', "-")
                    );
                }

                if let Some(parent) = config_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&config_path, toml::to_string_pretty(&value)?)?;
                println!("config written to {}", config_path.display());
            }
            ConfigSubcommand::Feed { subcommand: feed_sub } => match feed_sub {
                FeedSubcommand::List => {
                    let config = config::load(&config_path)?;
                    if !config.wallets.is_empty() {
                        let names: Vec<&str> = config.wallets.iter().map(|w| w.name.as_str()).collect();
                        println!("electrum (built-in): wallets {}", names.join(", "));
                    }
                    for f in &config.feeds {
                        println!("{} ({})", f.name, f.provider);
                    }
                }
            },
            ConfigSubcommand::Wallet { subcommand: wallet_sub } => match wallet_sub {
                WalletSubcommand::Add { name, descriptor } => {
                    let mut config = config::load(&config_path)?;

                    if config.wallets.iter().any(|w| w.name == name) {
                        anyhow::bail!("wallet '{name}' already exists in config");
                    }

                    config.wallets.push(WalletConfig {
                        name: name.clone(),
                        ext_descriptor: descriptor,
                        int_descriptor: None,
                        state_file: None,
                        archived: false,
                    });

                    config.write(&config_path)?;
                    println!("wallet '{name}' added");
                }
                WalletSubcommand::Remove { name } => {
                    let mut config = config::load(&config_path)?;

                    let before = config.wallets.len();
                    config.wallets.retain(|w| w.name != name);

                    if config.wallets.len() == before {
                        anyhow::bail!("wallet '{name}' not found in config");
                    }

                    config.write(&config_path)?;
                    println!("wallet '{name}' removed");
                }
            },
        },
    }

    Ok(())
}
