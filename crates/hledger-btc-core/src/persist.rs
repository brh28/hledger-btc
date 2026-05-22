use std::fmt;
use bdk_file_store::Store;
use bdk_wallet::{ChangeSet, WalletPersister};

const DB_MAGIC: &[u8] = b"hledger-btc";

#[derive(Debug)]
pub struct StoreError(String);

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for StoreError {}

/// Newtype wrapping [`bdk_file_store::Store`] that implements [`WalletPersister`].
pub struct WalletStore(Store<ChangeSet>);

impl WalletStore {
    pub fn load_or_create(path: &std::path::Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")))?;
        let (store, _) = Store::<ChangeSet>::load_or_create(DB_MAGIC, path)?;
        Ok(WalletStore(store))
    }
}

impl WalletPersister for WalletStore {
    type Error = StoreError;

    fn initialize(persister: &mut Self) -> Result<ChangeSet, Self::Error> {
        persister.0.dump()
            .map(|opt| opt.unwrap_or_default())
            .map_err(|e| StoreError(e.to_string()))
    }

    fn persist(persister: &mut Self, changeset: &ChangeSet) -> Result<(), Self::Error> {
        persister.0.append(changeset)
            .map_err(|e| StoreError(e.to_string()))
    }
}
