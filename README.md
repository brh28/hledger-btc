# hledger-btc

A Bitcoin accounting add-on for [hledger](https://hledger.org), written in Rust.

Bitcoin is the public ledger. Hledger is your personal ledger. `hledger-btc` bridges the two. It
scans your wallets via Electrum, writes confirmed transactions as double-entry
journal entries — helping you track cost-basis', balances, cash flows, etc.

## Features

- **`scan`** — fetch transactions from all configured data sources (on-chain wallets via Electrum, Lightning wallet exports) and write new entries to your journal, merging entries that describe the same transaction across sources
- **`label`** — set the description or posting note on a transaction, address, or output/input
- **`tag`** — attach hledger tags (`key:value`) to a transaction, address, or output/input
- **`import`** — import [BIP329](https://github.com/bitcoin/bips/blob/master/bip-0329.mediawiki) labels into your journal
- **`export`** — export your journal to BIP329 label format
- **`receive`** — record a receiving address as a receivable in the journal
- **`config`** — manage the electrum server, wallet, and data source configuration
- **`trace`** — recursively print transactions associated with an address to trace it's history

## Usage

```bash
# Scan the blockchain for transactions
hledger-btc scan

# Label a transaction (sets the description field)
hledger-btc label tx <txid> "Coinbase reward"

# Label an output or input (sets the posting free-text comment)
hledger-btc label output <txid>:<vout> "savings deposit"
hledger-btc label input <txid>:<index> "spending from savings"
hledger-btc label addr <address> "cold storage"

# Tag a transaction or posting with key:value data
hledger-btc tag tx <txid> lot=20260608
hledger-btc tag output <txid>:<vout> lot=20260608 cost=45000
hledger-btc tag addr <address> lot=20260608

# Import BIP329 labels into your journal
hledger-btc import labels.jsonl

# Export journal to BIP329 (stdout, or -o for a file)
hledger-btc export -o labels.jsonl

# Scan a single source (e.g. while testing a new export file)
hledger-btc scan --source phoenix

# Record an expected incoming payment
hledger-btc receive --address bc1q... --description "Invoice 3" --amount 100000 --total-cost 'USD 500.00'

# Trace the visibility footprint of an address
hledger-btc trace bc1q...

# Config management
hledger-btc config path
hledger-btc config show
hledger-btc config set --network bitcoin --server-url ssl://electrum.blockstream.info:50002
hledger-btc config wallet add --name savings --descriptor "wpkh([df9d4f28/84h/0h/0h]xpub.../0/*)"
hledger-btc config wallet remove --name savings
hledger-btc config source list
```

### Journal file resolution

All commands that read or write a journal use this fallback chain, mirroring hledger:

1. `-f`/`--file` argument
2. `LEDGER_FILE` environment variable
3. `~/.hledger.journal`

`scan` also accepts `-o`/`--output` to write new entries to a different file than the one used for dedup (useful for year-split journal setups).

## Installation

From source:

```bash
git clone https://github.com/brh28/hledger-btc
cd hledger-btc
cargo install --path crates/hledger-btc
```

## Configuration

Config lives at `~/.config/hledger-btc/config.toml`.

```toml
network    = "bitcoin"               # bitcoin | testnet | signet | regtest
server_url = "ssl://electrum.blockstream.info:50002"
# client_type  = "electrum"         # optional, default: electrum
# base_account = "assets:bitcoin"   # optional, default: assets:bitcoin

[[wallets]]
wallet         = "savings"
ext_descriptor = "wpkh([df9d4f28/84h/0h/0h]xpub.../0/*)"
# int_descriptor is optional — derived from ext_descriptor if omitted

[[wallets]]
wallet         = "spending"
ext_descriptor = "wpkh([ab1c2d3e/84h/0h/0h]xpub.../0/*)"

[[sources]]
name = "phoenix"
type = "lightning.phoenix"
path = "/home/me/sync/phoenix-export.csv"
```

`base_account` is the account prefix for all wallet and `receive` postings. Each
wallet's account defaults to `<base_account>:<wallet>`, e.g. `assets:bitcoin:savings`.

### Data sources

Beyond the built-in Electrum wallet scan, `scan` reads every `[[sources]]` entry:
a `name` (unique; stamped on entries as the `source:` tag), a `type` identifying
the file format, and a `path` to read. Automation that fetches fresh data (a
synced Phoenix export, an exchange API script) should write to `path` before
`scan` runs — e.g. `fetch-exchange > /tmp/data.csv && hledger-btc scan`.

Supported types:

| Type | Format | Account |
|---|---|---|
| `lightning.phoenix` | Phoenix wallet CSV export | `<base_account>:lightning:<name>` |

Entries from different sources that share a `txid` or `payment_hash` (e.g. an
on-chain transaction also seen by a Lightning swap) are merged into a single
journal entry. If a source reports data for a transaction already in the
journal, `scan` skips it and prints a notice rather than duplicating it.

## Give it a try

A working example is provided in [`config.toml.example`](config.toml.example).

1. Set config: `cp config.toml.example ~/.config/hledger-btc/config.toml`
2. Run: `cargo run -- scan`
3. Verify:
```
➜ alias hl-test="hledger -f /tmp/testwallet.journal"
➜ hl-test bal
         3,355,645 sat  expenses:fees:onchain
         2,227,326 sat  expenses:unknown
        -5,582,971 sat  income:unknown
--------------------
                   0

➜ hl-test print bc1qfp32zz2wenptc9nvu7v9qedhf8vdkufljq8qzx
2026-05-02 * Outgoing BTC  ; txid:9f3e90d36c37cc5025dce7a3fedabcace7e6391470642e148a4927ba268b47>
    assets:bitcoin:testwallet:bc1qfp32zz2wenptc9nvu7v9qedhf8vdkufljq8qzx       -4,000 sat  ; input:0
    expenses:fees:onchain                                                        3,960 sat
    expenses:unknown

2026-05-02 * Incoming BTC  ; txid:8cae3bef307ca4b3bf7a6461d94352e98b38a39a6a39205ad5528fddcf49fa>
    assets:bitcoin:testwallet:bc1qfp32zz2wenptc9nvu7v9qedhf8vdkufljq8qzx        4,000 sat  ; vout:1
    income:unknown
```

## Design

### Per-address sub-accounts

Each Bitcoin address becomes a sub-account under the wallet account (e.g.
`assets:bitcoin:savings:bc1q...`). This makes it possible to track which
address holds or spent funds, audit individual UTXOs, and produce accurate
per-address balance reports in hledger.

### sat accounting

All amounts are recorded in satoshis to avoid floating-point imprecision.

### Machine-managed fields

`scan` writes structural tags that `label`, `tag`, `import`, and `export` depend on.
**Do not remove or rename these fields in the journal:**

| Field | Where | Purpose |
|---|---|---|
| `txid:` | transaction comment | links entries across commands and sources |
| `payment_hash:` | transaction comment | links Lightning entries across sources |
| `source:` | transaction comment | records which source(s) produced the entry; drives scan dedup |
| `vout:N` | output posting comment | identifies the transaction output (outpoint index) |
| `input:N` | input posting comment | identifies the transaction input being spent |
| address sub-account | posting account name | e.g. `assets:bitcoin:savings:bc1q...` |

Everything else — the description, posting free-text, and any user-defined tags — is safe to edit freely. Use `label` and `tag` commands rather than hand-editing to reduce the risk of accidentally modifying structural fields.

### BIP329

[BIP329](https://github.com/bitcoin/bips/blob/master/bip-0329.mediawiki) is a
standard JSONL format for wallet labels. `import` annotates existing journal
entries with labels as hledger tags; `export` reads those tags back out.

All four BIP329 record types are supported:

| Type | Ref format | Maps to |
|---|---|---|
| `tx` | txid | transaction description |
| `addr` | address | posting free-text comment |
| `output` | txid:vout | output posting free-text comment |
| `input` | txid:index | input posting free-text comment |

Extra hledger tags (e.g. `lot:20260608`) are round-tripped via a non-spec
`tags` field that other BIP329 clients will safely ignore. Records with neither
a label nor tags are omitted from export.

## Project status

| Phase | Status | Description |
|---|---|---|
| 1 — Scaffold | ✅ | Workspace, CLI, config, logging |
| 2 — Scan | ✅ | Electrum scan, per-address postings, fee extraction |
| 3 — Receive | ✅ | Receivable journal entries |
| 4 — BIP329 Import | ✅ | BIP329 → hledger journal |
| 5 — BIP329 Export | ✅ | hledger journal → BIP329 |
| 6 — Label / Tag | ✅ | CLI commands for annotating transactions and postings |
| 7 — Trace | ✅ | Per-address visibility footprint |
| 8 — Tests | 🔲 | Integration tests against regtest |
| 9 — Polish | 🔲 | CI, crates.io publish |

## Dependencies

| Crate | Purpose |
|---|---|
| `bdk_wallet` | Descriptor parsing, address derivation, fee calculation |
| `bdk_electrum` | Electrum blockchain backend |
| `bdk_file_store` | Persistent wallet state (keychain index, UTXO graph) |
| `bip329` | BIP329 record types and JSONL serialization |
| `clap` | CLI argument parsing |
| `serde` + `toml` | Config serialization |
| `serde_json` | BIP329 JSONL serialization |
| `chrono` | Date formatting |
| `dirs` | Platform config directory |
| `anyhow` + `thiserror` | Error handling |
| `tracing` + `tracing-subscriber` | Structured logging |

## License

MIT OR Apache-2.0
