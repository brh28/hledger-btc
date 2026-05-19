# hledger-btc

A Bitcoin accounting add-on for [hledger](https://hledger.org), written in Rust.

`hledger-btc` bridges your Bitcoin wallet and your hledger journal. It fetches
transaction history from the blockchain and appends entries to your journal,
with hledger as the single source of truth. UTXOs and balances are derived from
the ledger rather than maintained in a separate wallet database.

## Features

- **`hledger-btc sync`** — fetch new transactions from the blockchain and append them to your hledger journal
- **`hledger-btc import`** — import [BIP329](https://github.com/bitcoin/bips/blob/master/bip-0329.mediawiki) labels into your journal
- **`hledger-btc export`** — export your journal to BIP329 label format

Transactions are recorded in `SAT` (satoshis) and include `TotalPrice`/`TotalCost`
annotations where available.

## Design

### hledger as source of truth

Unlike a traditional wallet, `hledger-btc` does not maintain its own UTXO set
or transaction database. Your hledger journal is the source of truth. On each
sync, the tool derives your address set from your descriptor, queries the
blockchain backend for transaction history, and appends any entries not already
present in the journal.

### BIP329

[BIP329](https://github.com/bitcoin/bips/blob/master/bip-0329.mediawiki) is a
standard JSONL format for wallet labels. `hledger-btc import` reads BIP329
records and maps them to hledger journal entries. `hledger-btc export` produces
BIP329 output from your journal. `xpub` records are stripped from export output
by default.

### SAT accounting

All amounts are recorded in satoshis to avoid floating point imprecision:

```journal
commodity 1000000000 SAT

2024-01-15 Received payment
    assets:bitcoin        500000 SAT @@ $142.50
    income:bitcoin
```

## Configuration

Config is stored at `~/.config/hledger-btc/config.toml`. This file may contain
sensitive information (descriptors); ensure it is readable only by your user
(`chmod 600`). The tool will warn on startup if permissions are too open.

```toml
[wallet]
network = "bitcoin"  # bitcoin | testnet | signet | regtest

[wallet.descriptor]
source = "env"
key = "HLEDGER_BTC_DESCRIPTOR"

[sync]
backend = "electrum"  # electrum | esplora
url = "tcp://my-electrum-server:50001"

[journal]
file = "/home/user/finances/bitcoin.journal"
account = "assets:bitcoin"
```

## Installation

```bash
cargo install hledger-btc
```

Or from source:

```bash
git clone https://github.com/yourname/hledger-btc
cd hledger-btc
cargo install --path crates/hledger-btc
```

## Usage

```bash

# Sync new transactions from the blockchain
hledger-btc sync

# Import BIP329 labels (from file or stdin)
hledger-btc import --file labels.jsonl
cat labels.jsonl | hledger-btc import

# Export journal to BIP329 (to file or stdout)
hledger-btc export --file labels.jsonl
hledger-btc export > labels.jsonl
```

## Project plan

### ✅ Phase 1 — Project scaffold

- Cargo workspace with lib/bin split (`hledger-btc-core` + `hledger-btc`)
- CLI skeleton with `clap` (`init`, `sync`, `import`, `export` subcommands)
- Typed config with `serde`/`toml` — `Network` and `Backend` enums, `SecretSource` enum
- `SecretSource` resolution (`literal`, `env`, `keyring`, `bitwarden_cli`)
- File permission check on config load (Unix)
- `tracing` + `tracing-subscriber` wired to `-v` verbosity flag
- `dirs` for platform-appropriate config path

### 🔲 Phase 2 — Wallet & sync

- Connect to Electrum backend via `bdk_esplora` or `bdk_electrum`
- Derive addresses from descriptor
- Fetch transaction history for derived addresses
- Diff against existing journal entries
- Append new transactions to journal in SAT with optional `@@` cost annotation

### 🔲 Phase 3 — Import (BIP329 → hledger)

- BIP329 JSONL parser
- Map `tx`, `addr`, `output` records to hledger journal entries
- Price feed integration for `TotalPrice`/`TotalCost`

### 🔲 Phase 4 — Export (hledger → BIP329)

- hledger journal reader
- Map journal transactions to BIP329 label records
- Strip `xpub` records from output by default
- Round-trip fidelity tests (import → export → diff)

### 🔲 Phase 5 — Polish & release

- `hledger-btc check` — audit journal against live chain state
- README, man page
- GitHub Actions CI (Linux, macOS, Windows)
- `crates.io` publish

## Dependencies

| Crate | Purpose |
|---|---|
| `bdk_wallet` | Descriptor parsing, address derivation |
| `bdk_esplora` | Blockchain backend client |
| `clap` | CLI argument parsing |
| `serde` + `toml` | Config serialization |
| `keyring` | OS keychain integration |
| `dirs` | Platform config directory |
| `anyhow` + `thiserror` | Error handling |
| `tracing` + `tracing-subscriber` | Structured logging |

## License

MIT OR Apache-2.0
