#![cfg(feature = "integration")]

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use bdk_wallet::bitcoin::bip32::{Xpriv, Xpub};
use bdk_wallet::bitcoin::secp256k1::Secp256k1;
use bdk_wallet::bitcoin::{Amount, Network};
use bdk_wallet::{KeychainKind, PersistedWallet, SignOptions, Wallet};

use hledger_btc_core::config::{ClientType, Config, Network as CfgNetwork, ScanConfig, WalletConfig};
use hledger_btc_core::journal::{self, Account, JournalEntry};
use hledger_btc_core::persist::WalletStore;
use hledger_btc_core::{scan, source};

// ── Seed generation ───────────────────────────────────────────────────────────

static SEED_CTR: AtomicU64 = AtomicU64::new(0);

// Unique 64-byte seed per call: mixes wall-clock seconds with a monotonic
// counter so addresses never collide across test runs on the same Docker chain.
fn fresh_seed() -> [u8; 64] {
    let n = SEED_CTR.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let base = t.wrapping_mul(0x517cc1b727220a95) ^ n.wrapping_mul(0x9e3779b97f4a7c15);
    let mut seed = [0u8; 64];
    for i in 0..8usize {
        seed[i * 8..(i + 1) * 8].copy_from_slice(&base.wrapping_add(i as u64).to_le_bytes());
    }
    seed
}

// ── Harness ───────────────────────────────────────────────────────────────────

struct Harness {
    rpc_url: String,
    electrum_url: String,
}

impl Harness {
    fn new() -> Self {
        let rpc_url = std::env::var("BITCOIND_RPC_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:18443".to_string());
        let electrum_url = std::env::var("ELECTRS_URL")
            .unwrap_or_else(|_| "tcp://127.0.0.1:60401".to_string());
        Self { rpc_url, electrum_url }
    }

    fn electrum_url(&self) -> String {
        self.electrum_url.clone()
    }

    fn rpc(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let body = serde_json::json!({
            "jsonrpc": "1.0",
            "id": "test",
            "method": method,
            "params": params
        });
        let resp = ureq::post(&self.rpc_url)
            .set("Authorization", "Basic dGVzdDp0ZXN0")
            .send_json(body);
        let json: serde_json::Value = match resp {
            Ok(r) => r.into_json().unwrap(),
            Err(ureq::Error::Status(_, r)) => r.into_json().unwrap(),
            Err(e) => panic!("bitcoind unreachable ({method}): {e}"),
        };
        if !json["error"].is_null() {
            panic!("RPC error calling {method}: {}", json["error"]);
        }
        json["result"].clone()
    }

    fn mine_blocks(&self, n: u64, coinbase_to: &bdk_wallet::bitcoin::Address) {
        self.rpc("generatetoaddress", serde_json::json!([n, coinbase_to.to_string()]));
        let height = self.rpc("getblockcount", serde_json::json!([])).as_u64().unwrap() as usize;
        self.wait_for_height(height);
    }

    fn wait_for_height(&self, target: usize) {
        use bdk_electrum::electrum_client::{ConfigBuilder, ElectrumApi};
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "electrs stuck below height {target} after 120s — check `docker compose logs electrs`"
            );
            let config = ConfigBuilder::new().timeout(Some(Duration::from_secs(3))).build();
            let client = match bdk_electrum::electrum_client::Client::from_config(
                &self.electrum_url,
                config,
            ) {
                Ok(c) => c,
                Err(_) => { std::thread::sleep(Duration::from_millis(500)); continue; }
            };
            match client.block_headers_subscribe() {
                Ok(hdr) if hdr.height >= target => break,
                _ => std::thread::sleep(Duration::from_millis(500)),
            }
        }
    }
}

// ── SharedFunder ──────────────────────────────────────────────────────────────
//
// Initialized once per test binary run via LazyLock. Mines 101 coinbase blocks
// on first access so tests can immediately spend. Uses a time-based seed so the
// funder address is fresh each run, preventing "too many history entries" from
// accumulating across runs on the same Docker chain.

struct SharedFunder {
    wallet: Mutex<PersistedWallet<WalletStore>>,
    db: Mutex<WalletStore>,
    harness: Harness,
    _dir: tempfile::TempDir,
}

static SHARED_FUNDER: LazyLock<SharedFunder> = LazyLock::new(|| {
    let harness = Harness::new();
    let dir = tempfile::tempdir().unwrap();

    let seed = fresh_seed();
    let secp = Secp256k1::new();
    let master = Xpriv::new_master(Network::Regtest, &seed).unwrap();

    let mut db = WalletStore::load_or_create(&dir.path().join("funder-signing.db")).unwrap();
    let wallet = Wallet::create(
        format!("wpkh({}/0/*)", master),
        format!("wpkh({}/1/*)", master),
    )
    .network(Network::Regtest)
    .create_wallet(&mut db)
    .unwrap();

    let coinbase_addr = wallet.peek_address(KeychainKind::External, 0).address;
    harness.mine_blocks(101, &coinbase_addr);

    SharedFunder {
        wallet: Mutex::new(wallet),
        db: Mutex::new(db),
        harness,
        _dir: dir,
    }
});

impl SharedFunder {
    fn harness(&self) -> &Harness {
        &self.harness
    }

    fn mine(&self, n: u64) {
        let coinbase_addr = self.wallet.lock().unwrap()
            .peek_address(KeychainKind::External, 0)
            .address;
        self.harness.mine_blocks(n, &coinbase_addr);
    }

    fn send_to(&self, to: &bdk_wallet::bitcoin::Address, amount: Amount) {
        let mut wallet = self.wallet.lock().unwrap();
        let mut db = self.db.lock().unwrap();

        let client = bdk_electrum::BdkElectrumClient::new(
            bdk_electrum::electrum_client::Client::new(&self.harness.electrum_url()).unwrap(),
        );
        let update = client.full_scan(wallet.start_full_scan(), 20, 5, true).unwrap();
        wallet.apply_update(update).unwrap();
        wallet.persist(&mut db).unwrap();

        let mut psbt = {
            let mut builder = wallet.build_tx();
            builder.add_recipient(to.script_pubkey(), amount);
            builder.finish().unwrap()
        };
        wallet.sign(&mut psbt, SignOptions::default()).unwrap();
        let tx = psbt.extract_tx().unwrap();

        use bdk_electrum::electrum_client::ElectrumApi;
        let electrum =
            bdk_electrum::electrum_client::Client::new(&self.harness.electrum_url()).unwrap();
        electrum.transaction_broadcast(&tx).unwrap();
    }
}

// ── TestWallet ────────────────────────────────────────────────────────────────

struct TestWallet {
    wallet: PersistedWallet<WalletStore>,
    db: WalletStore,
    pub cfg: Config,
    pub wallet_cfg: WalletConfig,
}

impl TestWallet {
    fn new(harness: &Harness, name: &str, state_dir: &Path) -> Self {
        let seed = fresh_seed();
        let master = Xpriv::new_master(Network::Regtest, &seed).unwrap();
        let xpub = Xpub::from_priv(&Secp256k1::new(), &master);

        let cfg = Config {
            base_account: Account::new("assets"),
            scan: ScanConfig {
                network: CfgNetwork::Regtest,
                server_url: harness.electrum_url(),
                client_type: ClientType::Electrum,
            },
            wallets: vec![],
        };
        let wallet_cfg = WalletConfig {
            name: name.to_string(),
            ext_descriptor: format!("wpkh({}/0/*)", xpub),
            int_descriptor: Some(format!("wpkh({}/1/*)", xpub)),
            state_file: Some(state_dir.join(format!("{name}.db"))),
            archived: false,
        };

        let mut db = WalletStore::load_or_create(
            &state_dir.join(format!("{name}-signing.db")),
        ).unwrap();
        let wallet = Wallet::create(
            format!("wpkh({}/0/*)", master),
            format!("wpkh({}/1/*)", master),
        )
        .network(Network::Regtest)
        .create_wallet(&mut db)
        .unwrap();

        TestWallet { wallet, db, cfg, wallet_cfg }
    }

    fn address(&self, index: u32) -> bdk_wallet::bitcoin::Address {
        self.wallet.peek_address(KeychainKind::External, index).address
    }

    fn send_to_address(&mut self, to: &bdk_wallet::bitcoin::Address, amount: Amount) {
        let client = bdk_electrum::BdkElectrumClient::new(
            bdk_electrum::electrum_client::Client::new(&self.cfg.scan.server_url).unwrap(),
        );
        let update = client.full_scan(self.wallet.start_full_scan(), 20, 5, true).unwrap();
        self.wallet.apply_update(update).unwrap();
        self.wallet.persist(&mut self.db).unwrap();

        let mut psbt = {
            let mut builder = self.wallet.build_tx();
            builder.add_recipient(to.script_pubkey(), amount);
            builder.finish().unwrap()
        };
        self.wallet.sign(&mut psbt, SignOptions::default()).unwrap();
        let tx = psbt.extract_tx().unwrap();

        use bdk_electrum::electrum_client::ElectrumApi;
        let electrum =
            bdk_electrum::electrum_client::Client::new(&self.cfg.scan.server_url).unwrap();
        electrum.transaction_broadcast(&tx).unwrap();
    }

    fn as_source(&self) -> scan::WalletSource<'_> {
        scan::WalletSource { cfg: &self.cfg, wallet: &self.wallet_cfg }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn scan_and_merge(wallets: &[&TestWallet]) -> Vec<JournalEntry> {
    let sources: Vec<Box<dyn source::Source + '_>> = wallets
        .iter()
        .map(|w| -> Box<dyn source::Source + '_> { Box::new(w.as_source()) })
        .collect();
    let collected = source::collect(&sources);
    journal::merge_entries(collected.entries)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn scan_detects_incoming_transaction() {
    let funder = &*SHARED_FUNDER;
    let harness = funder.harness();
    let tmp = tempfile::tempdir().unwrap();
    let wallet1 = TestWallet::new(harness, "wallet-1", tmp.path());

    funder.send_to(&wallet1.address(0), Amount::from_sat(100_000));
    funder.mine(1);

    let entries = scan::scan(&wallet1.cfg, &wallet1.wallet_cfg).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].journal.description, "Incoming BTC");
}

#[test]
fn scan_is_idempotent() {
    let funder = &*SHARED_FUNDER;
    let harness = funder.harness();
    let tmp = tempfile::tempdir().unwrap();
    let wallet1 = TestWallet::new(harness, "wallet-1", tmp.path());

    funder.send_to(&wallet1.address(0), Amount::from_sat(50_000));
    funder.mine(1);

    let first = scan::scan(&wallet1.cfg, &wallet1.wallet_cfg).unwrap();
    let second = scan::scan(&wallet1.cfg, &wallet1.wallet_cfg).unwrap();

    assert_eq!(first.len(), second.len());
    for (a, b) in first.iter().zip(second.iter()) {
        assert_eq!(a.journal.description, b.journal.description);
        assert_eq!(a.journal.postings.len(), b.journal.postings.len());
    }
}

#[test]
fn scan_detects_multiple_receives() {
    let funder = &*SHARED_FUNDER;
    let harness = funder.harness();
    let tmp = tempfile::tempdir().unwrap();
    let wallet1 = TestWallet::new(harness, "wallet-1", tmp.path());

    funder.send_to(&wallet1.address(0), Amount::from_sat(100_000));
    funder.mine(1);
    funder.send_to(&wallet1.address(1), Amount::from_sat(200_000));
    funder.mine(1);

    let entries = scan::scan(&wallet1.cfg, &wallet1.wallet_cfg).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|e| e.journal.description == "Incoming BTC"));
}

#[test]
fn scan_merges_inter_wallet_transfer() {
    let funder = &*SHARED_FUNDER;
    let harness = funder.harness();
    let tmp = tempfile::tempdir().unwrap();
    let mut wallet1 = TestWallet::new(harness, "wallet-1", tmp.path());
    let wallet2 = TestWallet::new(harness, "wallet-2", tmp.path());

    funder.send_to(&wallet1.address(0), Amount::from_sat(500_000));
    funder.mine(1);
    wallet1.send_to_address(&wallet2.address(0), Amount::from_sat(200_000));
    funder.mine(1);

    let merged = scan_and_merge(&[&wallet1, &wallet2]);

    assert_eq!(merged.len(), 2, "expected funding + transfer, got {}", merged.len());

    let transfer = merged
        .iter()
        .find(|e| e.postings.iter().any(|p| p.account.contains("wallet-2")))
        .expect("no entry with wallet-2 postings after merge");

    assert!(
        transfer.postings.iter().any(|p| p.account.contains("wallet-1")),
        "transfer entry missing wallet-1 posting"
    );
    assert!(
        !transfer.postings.iter().any(|p| p.account.contains("income:unknown")),
        "unexpected income:unknown in merged transfer"
    );
    assert!(
        !transfer.postings.iter().any(|p| p.account.contains("expenses:unknown")),
        "unexpected expenses:unknown in merged transfer"
    );
}
