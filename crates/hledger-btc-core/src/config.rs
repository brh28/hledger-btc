use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::journal::Account;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(default = "default_base_account")]
    pub base_account: Account,
    pub scan: ScanConfig,
    #[serde(default)]
    pub wallets: Vec<WalletConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScanConfig {
    pub network: Network,
    pub server_url: String,
    #[serde(default = "default_client_type")]
    pub client_type: ClientType,
}

fn default_client_type() -> ClientType {
    ClientType::Electrum
}

fn default_base_account() -> Account {
    Account::new("assets")
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WalletConfig {
    pub name: String,
    pub ext_descriptor: String,
    pub int_descriptor: Option<String>,
    pub state_file: Option<PathBuf>,
    #[serde(default)]
    pub archived: bool,
}

impl WalletConfig {
    pub fn int_descriptor(&self) -> String {
        self.int_descriptor
            .clone()
            .unwrap_or_else(|| derive_change_descriptor(&self.ext_descriptor))
    }

    pub fn state_path(&self) -> PathBuf {
        self.state_file.clone().unwrap_or_else(|| {
            dirs::data_local_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("hledger-btc")
                .join(format!("{}.db", self.name))
        })
    }

    pub fn account_name(&self, base_account: &Account) -> Account {
        base_account.append(&self.name)
    }
}

/// Derives a change (internal) descriptor by replacing the last /0/* with /1/*.
fn derive_change_descriptor(ext: &str) -> String {
    match ext.rfind("/0/*") {
        Some(pos) => format!("{}/1/*{}", &ext[..pos], &ext[pos + 4..]),
        None => ext.to_string(),
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    Bitcoin,
    Testnet,
    Signet,
    Regtest,
}

impl From<Network> for bdk_wallet::bitcoin::Network {
    fn from(n: Network) -> Self {
        match n {
            Network::Bitcoin => bdk_wallet::bitcoin::Network::Bitcoin,
            Network::Testnet => bdk_wallet::bitcoin::Network::Testnet,
            Network::Signet => bdk_wallet::bitcoin::Network::Signet,
            Network::Regtest => bdk_wallet::bitcoin::Network::Regtest,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientType {
    Electrum,
}

