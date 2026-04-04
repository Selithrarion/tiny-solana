use crate::{sync::TrackedRwLock, transactions::Transaction, types::Pubkey};
use ahash::{AHashMap, AHashSet};
use std::collections::BTreeSet;
use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum LockError {
    #[error("account is in use")]
    AccountInUse,
}

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    derive_more::From,
    derive_more::Into,
    derive_more::Add,
    derive_more::Sub,
    derive_more::AddAssign,
    derive_more::SubAssign,
    PartialEq,
    Eq,
)]
struct ReadCount(usize);

// TODO: ThreadAwareAccountLocks

pub struct AccountLocks {
    locks: TrackedRwLock<AccountLockState>,
}

struct AccountLockState {
    write_locks: AHashSet<Pubkey>,
    read_locks: AHashMap<Pubkey, ReadCount>,
}

pub struct LockGuard<'a> {
    account_locks: &'a AccountLocks,
    write_locks: Vec<Pubkey>,
    read_locks: Vec<Pubkey>,
}

impl AccountLocks {
    pub fn new() -> Self {
        Self {
            locks: TrackedRwLock::new(
                AccountLockState {
                    write_locks: AHashSet::new(),
                    read_locks: AHashMap::new(),
                },
                "account_locks",
            ),
        }
    }

    pub fn try_lock_accounts_for_group<'a>(
        &'a self,
        group: &[(usize, &Transaction)],
    ) -> Result<LockGuard<'a>, LockError> {
        let mut write_keys_to_lock = BTreeSet::new();
        let mut read_keys_to_lock = BTreeSet::new();

        for (_, tx) in group {
            for instruction in &tx.message.instructions {
                for acc_meta in &instruction.accounts {
                    if acc_meta.is_writable {
                        write_keys_to_lock.insert(acc_meta.pubkey);
                    } else {
                        read_keys_to_lock.insert(acc_meta.pubkey);
                    }
                }
            }
        }

        let mut state = self.locks.write();

        // check for write-write and read-write conflicts
        for key in &write_keys_to_lock {
            if state.write_locks.contains(key) || state.read_locks.contains_key(key) {
                return Err(LockError::AccountInUse);
            }
        }

        // check for write-read conflicts
        for key in &read_keys_to_lock {
            if state.write_locks.contains(key) {
                return Err(LockError::AccountInUse);
            }
        }

        // no conflicts, acquire locks
        let mut acquired_write_locks = Vec::new();
        for key in write_keys_to_lock {
            state.write_locks.insert(key);
            acquired_write_locks.push(key);
        }

        let mut acquired_read_locks = Vec::new();
        for key in read_keys_to_lock {
            *state.read_locks.entry(key).or_insert(ReadCount(0)) += ReadCount(1);
            acquired_read_locks.push(key);
        }

        Ok(LockGuard {
            account_locks: self,
            write_locks: acquired_write_locks,
            read_locks: acquired_read_locks,
        })
    }
}

impl<'a> Drop for LockGuard<'a> {
    fn drop(&mut self) {
        if self.write_locks.is_empty() && self.read_locks.is_empty() {
            return;
        }

        let mut state = self.account_locks.locks.write();

        for key in &self.write_locks {
            state.write_locks.remove(key);
        }

        for key in &self.read_locks {
            if let Some(count) = state.read_locks.get_mut(key) {
                *count -= ReadCount(1);
                if *count == ReadCount(0) {
                    state.read_locks.remove(key);
                }
            }
        }
    }
}
