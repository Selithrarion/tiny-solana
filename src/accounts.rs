use crate::types::{Hash, Pubkey};
use anyhow::{Context, Result, anyhow};
use bincode::Options;
use dashmap::DashMap;
use rayon::prelude::*;
use rocksdb::{DB, Options as RocksDbOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq, Clone)]
pub enum AccountError {
    #[error("account not found: {0:?}")]
    NotFound(Pubkey),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Account {
    pub pubkey: Pubkey,
    pub lamports: u64,
    pub data: Vec<u8>,
    pub owner: Pubkey,
    pub executable: bool,
    pub rent_epoch: u64,
}

pub trait AccountStore: Send + Sync {
    fn load(&self, pubkey: &Pubkey) -> Option<Account>;
    fn store(&self, pubkey: &Pubkey, account: Account) -> Result<()>;
    fn all_accounts(&self) -> Vec<Account>;
    fn get_root_hash(&self) -> Result<Hash>;
}

// TODO: snapshots and efficient indexing
// TODO: handle rent exemption logic
#[derive(Clone)]
pub struct InMemoryAccountStore {
    accounts: DashMap<Pubkey, Account>,
}

impl Default for InMemoryAccountStore {
    fn default() -> Self {
        Self {
            accounts: DashMap::new(),
        }
    }
}

impl AccountStore for InMemoryAccountStore {
    fn load(&self, pubkey: &Pubkey) -> Option<Account> {
        self.accounts.get(pubkey).map(|acc| acc.clone())
    }

    fn store(&self, pubkey: &Pubkey, account: Account) -> Result<()> {
        self.accounts.insert(*pubkey, account);
        Ok(())
    }

    fn all_accounts(&self) -> Vec<Account> {
        self.accounts
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    fn get_root_hash(&self) -> Result<Hash> {
        hash_accounts(&self.all_accounts())
    }
}

pub struct RocksDbAccountStore {
    db: Arc<DB>,
}

impl RocksDbAccountStore {
    pub fn new(path: &str) -> Result<Self> {
        let mut opts = RocksDbOptions::default();
        opts.create_if_missing(true);
        let db = DB::open(&opts, path).context("failed to open rocksdb")?;
        Ok(Self { db: Arc::new(db) })
    }
}

impl AccountStore for RocksDbAccountStore {
    fn load(&self, pubkey: &Pubkey) -> Option<Account> {
        match self.db.get(&pubkey.0) {
            Ok(Some(bytes)) => bincode::options()
                .with_fixint_encoding()
                .deserialize(&bytes)
                .ok(),
            _ => None,
        }
    }

    fn store(&self, pubkey: &Pubkey, account: Account) -> Result<()> {
        let serialized = bincode::options()
            .with_fixint_encoding()
            .serialize(&account)
            .context("failed to serialize account for rocksdb")?;
        self.db
            .put(&pubkey.0, serialized)
            .context("failed to put account into rocksdb")?;
        Ok(())
    }

    fn all_accounts(&self) -> Vec<Account> {
        self.db
            .iterator(rocksdb::IteratorMode::Start)
            .filter_map(|res| res.ok())
            .filter_map(|(_, value)| {
                bincode::options()
                    .with_fixint_encoding()
                    .deserialize(&value)
                    .ok()
            })
            .collect()
    }

    fn get_root_hash(&self) -> Result<Hash> {
        // TODO: replace
        hash_accounts(&self.all_accounts())
    }
}

pub struct OverlayStore<S: AccountStore + ?Sized> {
    dirty: DashMap<Pubkey, Account>,
    parent: Arc<S>,
}

impl<S: AccountStore + ?Sized> OverlayStore<S> {
    pub fn new(parent: Arc<S>) -> Self {
        Self {
            dirty: DashMap::new(),
            parent,
        }
    }

    pub fn flush(&self) {
        for entry in self.dirty.iter() {
            self.parent
                .store(entry.key(), entry.value().clone())
                .unwrap(); // TODO
        }
        self.dirty.clear();
    }

    pub fn dirty_count(&self) -> usize {
        self.dirty.len()
    }
}

impl<S: AccountStore + ?Sized> AccountStore for OverlayStore<S> {
    fn load(&self, pubkey: &Pubkey) -> Option<Account> {
        if let Some(account) = self.dirty.get(pubkey) {
            return Some(account.clone());
        }
        self.parent.load(pubkey)
    }

    fn store(&self, pubkey: &Pubkey, account: Account) -> Result<()> {
        self.dirty.insert(*pubkey, account);
        Ok(())
    }

    fn all_accounts(&self) -> Vec<Account> {
        let all: DashMap<Pubkey, Account> = DashMap::new();
        for account in self.parent.all_accounts() {
            all.insert(account.pubkey, account);
        }
        for entry in self.dirty.iter() {
            all.insert(*entry.key(), entry.value().clone());
        }
        all.into_iter().map(|(_, v)| v).collect()
    }

    fn get_root_hash(&self) -> Result<Hash> {
        // TODO: incremental hashing
        let all = self.all_accounts();
        if all.is_empty() {
            return Ok(Hash::default());
        }
        hash_accounts(&all)
    }
}

fn hash_accounts(accounts: &[Account]) -> Result<Hash> {
    let mut leaf_hashes: Vec<[u8; 32]> = accounts
        .par_iter()
        .map(|account| {
            let serialized = bincode::options()
                .with_fixint_encoding()
                .serialize(&(account.pubkey, account))
                .context("failed to serialize account")?;
            Ok(Sha256::digest(serialized).into())
        })
        .collect::<Result<Vec<_>>>()?;

    leaf_hashes.par_sort_unstable();

    let mut level = leaf_hashes;
    while level.len() > 1 {
        level = level
            .par_chunks(2)
            .map(|chunk| {
                let mut hasher = Sha256::new();
                hasher.update(chunk[0]);
                hasher.update(chunk.get(1).unwrap_or(&chunk[0]));
                hasher.finalize().into()
            })
            .collect();
    }

    level
        .first()
        .map(|h| Hash(*h))
        .ok_or_else(|| anyhow!("empty level"))
}
