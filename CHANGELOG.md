# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-06-30

### Added

- **`scan` command** — syncs descriptor wallets against an Electrum server; produces
  hledger journal entries with per-address sub-accounts (`assets:bitcoin:wallets:<name>:<addr>`)
  and sat-denominated amounts
- **Fee extraction** — on-chain transaction fees broken out to `expenses:fees:onchain`
- **Transfer merge** — inter-wallet sends (wallet A → wallet B) are detected by shared `txid`
  and merged into a single balanced entry; eliminates spurious `income:unknown` /
  `expenses:unknown` legs for internal transfers
- **`receive` command** — writes a pending (`!`) journal entry for a Bitcoin address with
  optional expected amount, credit account, and carry-forward tags; supports
  `--unit-price` / `--total-cost` for price annotation
- **Receivable settlement** — `scan` matches incoming transactions against open receivables
  by address; carries forward description and tags, replaces `income:unknown` with the
  configured credit account, and marks the receivable settled in the journal
- **Amount mismatch notices** — printed when an incoming amount differs from the
  `expected:` sat value on a receivable
- **`label` command** — sets the hledger description on a transaction via BIP329 `tx` label
- **`tag` command** — adds arbitrary key:value tags to transactions; written as hledger
  inline comments (`; key:value`)
- **`trace` command** — displays all journal entries associated with a Bitcoin address
- **`reconcile` command** — when a feed entry (e.g. Coinbase withdrawal) shares a `txid`
  with an existing wallet-scan entry, replaces the `income:unknown` or `expenses:unknown`
  placeholder with explicit postings from the feed; stamps novel `source:` tags on the
  header; preserves residual auto-balance for fee differences
- **BIP329 import** — `import bip329` ingests a BIP329 JSON label file; `tx` labels become
  hledger descriptions, `addr` labels become posting comments, `xpub` / `output` labels
  written as tags
- **BIP329 export** — `export bip329` produces a BIP329 JSON file from the journal;
  descriptions round-trip as `tx` labels
- **Coinbase feed** — `import coinbase` ingests Coinbase Advanced Trade transaction history
  CSV; handles buys, sells, sends, receives, and rewards; deduplicates against wallet-scan
  entries via shared `txid` / `payment_hash`
- **Phoenix feed** — `import phoenix` ingests Phoenix wallet CSV exports; handles Lightning
  sends, Lightning receives, swap-ins (Lightning → on-chain), and swap-outs (on-chain →
  Lightning); links to wallet-scan entries via `txid` and `payment_hash`
- **CashApp feed** — `import cashapp` ingests Cash App transaction history CSV; handles
  Bitcoin sends, receives, and purchases
- **River feed** — `import river` ingests River "Account Activity" CSV exports; handles
  on-chain deposits/withdrawals, Lightning sends/receives, buys, sells, and interest
- **Lightning support** — `payment_hash` dedup key links Phoenix CSV entries to on-chain
  swap transactions; BOLT11 invoice parsing extracts payment hash from raw invoice strings
- **`source:` stamping** — on-chain and Lightning entries are stamped with
  `source:<name>` so cross-source reconciliation can detect novel contributors to a
  known `txid`; provider-keyed entries (trades, exchange-internal transfers) use their
  own key (`coinbase_id:`, etc.) without a `source:` tag
- **Per-wallet descriptor config** — `wallets.toml` (or `config.toml` `[[wallets]]` table)
  supports `name`, `ext_descriptor`, optional `int_descriptor` (derived from external if
  omitted), and optional `state_file`

[Unreleased]: https://github.com/brh28/hledger-btc/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/brh28/hledger-btc/releases/tag/v0.1.0
