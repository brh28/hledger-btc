# hledger-btc

A Bitcoin accounting add-on for [hledger](https://hledger.org), written in Rust.

`hledger-btc` bridges your Bitcoin wallet and your hledger journal. It fetches
confirmed transaction history from the blockchain and writes entries to your
journal, with hledger as the single source of truth.

## Features

- **`scan`** — fetch confirmed transactions for all configured wallets and write them to hledger journals
- **`receive`** — derive a new receiving address and record it in the journal as a receivable
- **`trace`** — print the transaction history for a given address *(planned)*
- **`import`** — import [BIP329](https://github.com/bitcoin/bips/blob/master/bip-0329.mediawiki) labels into your journal *(planned)*
- **`export`** — export your journal to BIP329 label format *(planned)*

## Usage

```bash
# Scan all wallets defined in ~/.config/hledger-btc/config.toml
hledger-btc scan

# Override config file location
hledger-btc scan --config /path/to/config.toml

# Increase verbosity (-v info, -vv debug, -vvv trace)
hledger-btc -v scan

# Derive a new receiving address and record it as a receivable
hledger-btc receive

# With metadata
hledger-btc receive --description="Invoice 3" --amount=100000 --total-cost='USD 500.00'

# Print transaction history for an address
hledger-btc trace bc1q...
```

## Installation

From source:

```bash
git clone https://github.com/brh28/hledger-btc
cd hledger-btc
cargo install --path crates/hledger-btc
```

## Configuration

Config lives at `~/.config/hledger-btc/wallets.toml`. It may contain private
key material — restrict permissions with `chmod 600`. The tool warns on startup
if the file is group- or world-readable.

Each wallet is a `[wallets.<name>]` section:

```toml
[wallets.mywallet]
wallet       = "mywallet"
network      = "bitcoin"          # bitcoin | testnet | signet | regtest
ext_descriptor = "wpkh([fingerprint/84'/0'/0']xprv.../0/*)"
int_descriptor = "wpkh([fingerprint/84'/0'/0']xprv.../1/*)"  # optional, derived from ext if omitted
client_type  = "electrum"
server_url   = "tcp://my-electrum-server:50001"
journal_file = "/home/user/finances/bitcoin.journal"      # read source for dedup; optional, stdout if omitted
output_file  = "/home/user/finances/2026/bitcoin.journal" # optional, write target; defaults to journal_file
account      = "assets:bitcoin:mywallet"                  # optional, default: assets:bitcoin:<wallet>
```

`journal_file` is always read via `hledger print` (resolving any `include` directives) to determine which transactions are already recorded. New entries are written to `output_file` if set, otherwise back to `journal_file`.


## Give it a try

A working example is provided in [`wallets.toml.example`](wallets.toml.example).

1. Set config: `cp wallets.toml.example  ~/.config/hledger-btc/wallets.toml`
2. Run: `cargo run -- scan` 
3. Verify:
```
➜ alias hl-test="hledger -f /tmp/testwallet.journal" 
➜ hl-test bal
         3355645 SAT  expenses:fees:onchain
         2227326 SAT  expenses:unknown
        -5582971 SAT  income:unknown
--------------------
                   0  
➜ hl-test print bc1qfp32zz2wenptc9nvu7v9qedhf8vdkufljq8qzx
2026-05-02 * Outgoing BTC  ; txid:9f3e90d36c37cc5025dce7a3fedabcace7e6391470642e148a4927ba268b47>
    assets:bitcoin:testwallet:bc1qfp32zz2wenptc9nvu7v9qedhf8vdkufljq8qzx       -4000 SAT
    expenses:fees:onchain                                                       3960 SAT
    expenses:unknown
 
2026-05-02 * Incoming BTC  ; txid:8cae3bef307ca4b3bf7a6461d94352e98b38a39a6a39205ad5528fddcf49fa>
    assets:bitcoin:testwallet:bc1qfp32zz2wenptc9nvu7v9qedhf8vdkufljq8qzx        4000 SAT
    income:unknown
```

## Design

### hledger as source of truth

`hledger-btc` does not maintain its own UTXO set or transaction database. The
journal file is the store. On each sync the tool scans the blockchain via
Electrum, builds journal entries from confirmed transactions, and writes them to
the configured file.

### Per-address sub-accounts

Each Bitcoin address becomes a sub-account under the wallet account (e.g.
`assets:bitcoin:mywallet:bc1q...`). This makes it possible to track which
address holds or spent funds, audit individual UTXOs, and produce accurate
per-address balance reports in hledger.

### SAT accounting

All amounts are recorded in satoshis to avoid floating-point imprecision.

### BIP329 *(planned)*

[BIP329](https://github.com/bitcoin/bips/blob/master/bip-0329.mediawiki) is a
standard JSONL format for wallet labels. `import` and `export` subcommands are
planned for a future release.

## Project status

| Phase | Status | Description |
|---|---|---|
| 1 — Scaffold | ✅ | Workspace, CLI, config, logging |
| 2 — Scan | ✅ | Electrum scan, per-address postings, fee extraction |
| 3 — Receive | ✅ | Address derivation, receivable journal entries |
| 4 — Trace | 🔲 | Per-address transaction history |
| 5 — Import | 🔲 | BIP329 → hledger |
| 6 — Export | 🔲 | hledger → BIP329 |
| 7 — Tests | 🔲 | Unit tests for journal formatting and receive entry construction; integration tests for scan against regtest |
| 8 — Polish | 🔲 | CI, crates.io publish |

## Dependencies

| Crate | Purpose |
|---|---|
| `bdk_wallet` | Descriptor parsing, address derivation, fee calculation |
| `bdk_electrum` | Electrum blockchain backend |
| `bdk_file_store` | Persistent wallet state (keychain index, UTXO graph) |
| `clap` | CLI argument parsing |
| `serde` + `toml` | Config serialization |
| `chrono` | Date formatting |
| `keyring` | OS keychain integration |
| `dirs` | Platform config directory |
| `anyhow` + `thiserror` | Error handling |
| `tracing` + `tracing-subscriber` | Structured logging |

## Troubleshooting

**`Descriptor mismatch for External keychain`** — the descriptor in `wallets.toml` doesn't match the one stored in the wallet state file. This happens if you change or correct the descriptor after the wallet was first initialized. Delete the state file and rescan:

```bash
rm ~/.local/share/hledger-btc/<walletname>.db
hledger-btc -v scan
```

## License

MIT OR Apache-2.0
