use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::BufWriter;
use std::io::Read;
use std::path::PathBuf;
use std::process::Stdio;
use tracing_subscriber::EnvFilter;

use std::collections::BTreeMap;
use hledger_btc_core::{annotate::{Annotation, AnnotationType}, config, export, import, journal, label, receive, scan, trace};
use hledger_btc_lightning::phoenix;

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
    /// Scan confirmed transactions for all wallets and write hledger journals
    Scan {
        /// Journal file to read for dedup; falls back to LEDGER_FILE, then ~/.hledger.journal
        #[arg(short = 'f', long = "file")]
        journal: Option<PathBuf>,

        /// Journal file to append new entries to; defaults to the value of -f/--file
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
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
    /// Import BIP329 labels into hledger journal
    Import {
        /// Journal file to annotate; falls back to LEDGER_FILE, then ~/.hledger.journal
        #[arg(short = 'f', long = "file")]
        journal: Option<PathBuf>,

        /// BIP329 JSONL file to read labels from; reads stdin if omitted
        labels: Option<PathBuf>,

        /// Replace existing label tags instead of skipping already-labelled entries
        #[arg(long = "override")]
        override_existing: bool,
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

        /// Replace existing label instead of skipping already-labelled entries
        #[arg(long = "override")]
        override_existing: bool,

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
    /// Import Lightning wallet data into hledger journal
    Lightning {
        #[command(subcommand)]
        subcommand: LightningSubcommand,
    },
    /// Manage the hledger-btc configuration file
    Config {
        #[command(subcommand)]
        subcommand: ConfigSubcommand,
    },
}

#[derive(Subcommand)]
enum LightningSubcommand {
    /// Import Lightning payment history from a CSV or external source
    Import {
        #[command(subcommand)]
        subcommand: LightningImportSubcommand,
    },
}

#[derive(Subcommand)]
enum LightningImportSubcommand {
    /// Import a Phoenix wallet CSV export
    Phoenix {
        /// CSV file exported from Phoenix
        csv: PathBuf,

        /// Journal file to read for dedup; falls back to LEDGER_FILE, then ~/.hledger.journal
        #[arg(short = 'f', long = "file")]
        journal: Option<PathBuf>,

        /// Journal file to append new entries to; defaults to -f
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,

        /// Wallet name; derives account as {base_account}:lightning:{name} (default: phoenix)
        #[arg(long, default_value = "phoenix")]
        name: String,
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
    /// Set top-level configuration fields
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
        /// Base account prefix for all wallet postings (default: assets:bitcoin)
        #[arg(long)]
        base_account: Option<String>,
    },
    /// Manage wallets in the configuration
    Wallet {
        #[command(subcommand)]
        subcommand: WalletSubcommand,
    },
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

fn write_config(path: &PathBuf, cfg: &config::Config) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, toml::to_string_pretty(cfg)?)
        .map_err(Into::into)
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
        Command::Scan { journal, output } => {
            let cfg = config::load(&config_path)?;

            let mut raw: Vec<journal::JournalEntry> = Vec::new();
            for wallet_cfg in &cfg.wallets {
                raw.extend(scan::scan(&cfg, wallet_cfg)?);
            }
            let all_entries = journal::merge_by_txid(raw);

            let journal_path = resolve_journal(journal);
            let output_path = output.unwrap_or_else(|| journal_path.clone());

            tracing::info!("reading known txids via hledger from {:?}", journal_path);
            let known = {
                let child = std::process::Command::new("hledger")
                    .args(["-f", journal_path.to_str().unwrap(), "print"])
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
                let file = std::fs::OpenOptions::new().create(true).append(true).open(&output_path)?;
                journal::write_entries(&new_entries, &mut BufWriter::new(file))?;
            }
        }
        Command::Receive { journal, address, account, date, description, amount, unit_price, total_cost } => {
            let resolved_account = account.or_else(|| {
                config::load(&config_path).ok().map(|cfg| cfg.base_account)
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
        Command::Import { journal, labels, override_existing } => {
            let journal_path = resolve_journal(journal);

            if !journal_path.exists() {
                anyhow::bail!("journal file does not exist: {}", journal_path.display());
            }

            let bip329_content = match labels {
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
        Command::Export { journal, output } => {
            let journal_path = resolve_journal(journal);

            if !journal_path.exists() {
                anyhow::bail!("journal file does not exist: {}", journal_path.display());
            }

            let journal_content = std::fs::read_to_string(&journal_path)?;
            let bip329_output = export::export_to_string(&journal_content)?;

            match output {
                Some(path) => std::fs::write(&path, bip329_output)?,
                None => print!("{bip329_output}"),
            }
        }
        Command::Label { journal, subcommand, .. } => {
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
        Command::Lightning { subcommand } => match subcommand {
            LightningSubcommand::Import { subcommand } => match subcommand {
                LightningImportSubcommand::Phoenix { csv, journal, output, name } => {
                    let cfg = config::load(&config_path)?;
                    let account = format!("{}:lightning:{name}", cfg.base_account);

                    let journal_path = resolve_journal(journal);
                    let output_path = output.unwrap_or_else(|| journal_path.clone());

                    let content = if journal_path.exists() {
                        let out = std::process::Command::new("hledger")
                            .args(["-f", journal_path.to_str().unwrap(), "print"])
                            .output()?
                            .stdout;
                        String::from_utf8(out)?
                    } else {
                        String::new()
                    };
                    let known_payment_hashes =
                        journal::read_tag_values(content.as_bytes(), "payment_hash")?;
                    let known_txids = journal::read_tag_values(content.as_bytes(), "txid")?;

                    let all_entries = phoenix::import(&csv, &account)?;
                    let new_entries: Vec<_> = all_entries
                        .into_iter()
                        .filter(|e| {
                            let ph = e.tags.get("payment_hash");
                            let txid = e.tags.get("txid");
                            ph.map_or(true, |v| !known_payment_hashes.contains(v))
                                && txid.map_or(true, |v| !known_txids.contains(v))
                        })
                        .collect();

                    tracing::info!("{} new entries to write", new_entries.len());
                    if !new_entries.is_empty() {
                        let file = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&output_path)?;
                        journal::write_entries(&new_entries, &mut BufWriter::new(file))?;
                    }
                }
            }
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

                if let Some(v) = network     { table.insert("network".to_string(),      toml::Value::String(v)); }
                if let Some(v) = server_url  { table.insert("server_url".to_string(),   toml::Value::String(v)); }
                if let Some(v) = client_type { table.insert("client_type".to_string(),  toml::Value::String(v)); }
                if let Some(v) = base_account { table.insert("base_account".to_string(), toml::Value::String(v)); }

                for field in ["network", "server_url"] {
                    if !table.contains_key(field) {
                        anyhow::bail!("missing required field '{field}' — set it with --{}", field.replace('_', "-"));
                    }
                }

                if let Some(parent) = config_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&config_path, toml::to_string_pretty(&value)?)?;
                println!("config written to {}", config_path.display());
            }
            ConfigSubcommand::Wallet { subcommand: wallet_sub } => match wallet_sub {
                WalletSubcommand::Add { name, descriptor } => {
                    let mut cfg = config::load(&config_path)?;

                    if cfg.wallets.iter().any(|w| w.wallet == name) {
                        anyhow::bail!("wallet '{name}' already exists in config");
                    }

                    cfg.wallets.push(config::WalletConfig {
                        wallet: name.clone(),
                        ext_descriptor: descriptor,
                        int_descriptor: None,
                        state_file: None,
                    });

                    write_config(&config_path, &cfg)?;
                    println!("wallet '{name}' added");
                }
                WalletSubcommand::Remove { name } => {
                    let mut cfg = config::load(&config_path)?;

                    let before = cfg.wallets.len();
                    cfg.wallets.retain(|w| w.wallet != name);

                    if cfg.wallets.len() == before {
                        anyhow::bail!("wallet '{name}' not found in config");
                    }

                    write_config(&config_path, &cfg)?;
                    println!("wallet '{name}' removed");
                }
            },
        },
    }

    Ok(())
}
