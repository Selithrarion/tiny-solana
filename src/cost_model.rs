use crate::{programs, transactions::Transaction, types::Pubkey};
use std::{
    collections::{BTreeSet, HashSet},
    sync::atomic::{AtomicU64, Ordering},
};
use thiserror::Error;

// (from agave's block_cost_limits.rs)
/// Cluster averaged compute unit to micro-sec conversion rate
pub const COMPUTE_UNIT_TO_US_RATIO: u64 = 30;
/// Number of compute units for one signature verification.
pub const SIGNATURE_COST: u64 = COMPUTE_UNIT_TO_US_RATIO * 24;
/// Number of compute units for one secp256k1 signature verification.
pub const SECP256K1_VERIFY_COST: u64 = COMPUTE_UNIT_TO_US_RATIO * 223;
/// Number of compute units for one ed25519 strict signature verification.
pub const ED25519_VERIFY_STRICT_COST: u64 = COMPUTE_UNIT_TO_US_RATIO * 80;
/// Number of compute units for one secp256r1 signature verification.
pub const SECP256R1_VERIFY_COST: u64 = COMPUTE_UNIT_TO_US_RATIO * 160;
/// Number of compute units for one write lock
pub const WRITE_LOCK_UNITS: u64 = COMPUTE_UNIT_TO_US_RATIO * 10;
/// Number of data bytes per compute units
pub const INSTRUCTION_DATA_BYTES_COST: u64 = 140 / COMPUTE_UNIT_TO_US_RATIO;

/// Number of compute units that a block is allowed. A block's compute units are
/// accumulated by Transactions added to it; A transaction's compute units are
/// calculated by cost_model, based on transaction's signatures, write locks,
/// data size and built-in and SBF instructions.
pub const MAX_BLOCK_UNITS: u64 = MAX_BLOCK_UNITS_SIMD_0256;
pub const MAX_BLOCK_UNITS_SIMD_0256: u64 = 60_000_000;
pub const MAX_BLOCK_UNITS_SIMD_0286: u64 = 100_000_000;

/// Number of compute units that a writable account in a block is allowed. The
/// limit is to prevent too many transactions write to same account, therefore
/// reduce block's parallelism.
pub const MAX_WRITABLE_ACCOUNT_UNITS: u64 = 24_000_000;

/// Number of compute units that a block can have for vote transactions,
/// set to less than MAX_BLOCK_UNITS to leave room for non-vote transactions
pub const MAX_VOTE_UNITS: u64 = 36_000_000;

/// The maximum allowed size, in bytes, that accounts data can grow, per block.
/// This can also be thought of as the maximum size of new allocations per block.
pub const MAX_BLOCK_ACCOUNTS_DATA_SIZE_DELTA: u64 = 100_000_000;

/// Return the block limits that will be used upon activation of SIMD-0286.
pub const fn simd_0286_block_limit() -> u64 {
    MAX_BLOCK_UNITS_SIMD_0286
}

// transaction_cost.rs
const SIMPLE_VOTE_USAGE_COST: u64 = 3428;

// ours
pub const BUILTIN_PROGRAM_COST: u64 = COMPUTE_UNIT_TO_US_RATIO * 10;
pub const BPF_PROGRAM_COST: u64 = 200_000;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum CostModelError {
    #[error("would exceed block max limit")]
    WouldExceedBlockMaxLimit,
    #[error("would exceed account max limit")]
    WouldExceedAccountMaxLimit,
}

pub struct CostModel {
    builtin_programs: HashSet<Pubkey>,
}

// TODO: priority fee support
// TODO: vote transactions

#[derive(Debug, Clone)]
pub struct TransactionCost {
    pub signature_cost: u64,
    pub write_lock_cost: u64,
    pub data_cost: u64,
    pub execution_cost: u64,
    pub writable_accounts: BTreeSet<Pubkey>,
}

impl CostModel {
    pub fn new() -> Self {
        let mut builtin_programs = HashSet::new();

        builtin_programs.insert(programs::ids::SYSTEM_PROGRAM_ID);
        builtin_programs.insert(programs::ids::TOKEN_PROGRAM_ID);

        Self { builtin_programs }
    }

    pub fn calculate_cost(&self, tx: &Transaction) -> TransactionCost {
        let signature_cost = tx.signatures.len() as u64 * SIGNATURE_COST;

        let mut write_set = BTreeSet::new();
        let mut instruction_data_len: u64 = 0;
        let mut execution_cost: u64 = 0;

        for instruction in &tx.message.instructions {
            // TODO: add compute budget instruction parsing
            for acc_meta in &instruction.accounts {
                if acc_meta.is_writable {
                    write_set.insert(acc_meta.pubkey);
                }
            }
            instruction_data_len += instruction.data.len() as u64;

            if self.builtin_programs.contains(&instruction.program_id) {
                execution_cost += BUILTIN_PROGRAM_COST;
            } else {
                execution_cost += BPF_PROGRAM_COST;
            }
        }

        let write_lock_cost = write_set.len() as u64 * WRITE_LOCK_UNITS;
        let data_cost = instruction_data_len * INSTRUCTION_DATA_BYTES_COST;

        TransactionCost {
            signature_cost,
            write_lock_cost,
            data_cost,
            execution_cost,
            writable_accounts: write_set,
        }
    }
}

impl TransactionCost {
    pub fn sum(&self) -> u64 {
        self.signature_cost
            .saturating_add(self.write_lock_cost)
            .saturating_add(self.data_cost)
            .saturating_add(self.execution_cost)
    }
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
pub struct AccountCost(u64);

pub struct CostTracker {
    block_cost_limit: u64,
    account_cost_limit: u64,
    block_cost: AtomicU64,
    cost_by_writable_accounts: dashmap::DashMap<Pubkey, AccountCost>,
}

impl CostTracker {
    pub fn new(block_cost_limit: u64, account_cost_limit: u64) -> Self {
        Self {
            block_cost_limit,
            account_cost_limit,
            block_cost: AtomicU64::new(0),
            cost_by_writable_accounts: dashmap::DashMap::new(),
        }
    }

    pub fn would_fit(&self, tx_cost: &TransactionCost) -> Result<(), CostModelError> {
        let cost = tx_cost.sum();

        if self.block_cost.load(Ordering::Relaxed).saturating_add(cost) > self.block_cost_limit {
            return Err(CostModelError::WouldExceedBlockMaxLimit);
        }

        for account_key in &tx_cost.writable_accounts {
            if let Some(chained_cost) = self.cost_by_writable_accounts.get(account_key) {
                if chained_cost.value().0.saturating_add(cost) > self.account_cost_limit {
                    return Err(CostModelError::WouldExceedAccountMaxLimit);
                }
            }
        }

        Ok(())
    }

    pub fn add_cost(&self, tx_cost: &TransactionCost) {
        let cost = tx_cost.sum();
        self.block_cost.fetch_add(cost, Ordering::Relaxed);

        for account_key in &tx_cost.writable_accounts {
            let mut account_cost = self
                .cost_by_writable_accounts
                .entry(*account_key)
                .or_insert(AccountCost(0));
            *account_cost += AccountCost(cost);
        }
    }

    pub fn subtract_cost(&self, tx_cost: &TransactionCost) {
        let cost = tx_cost.sum();
        self.block_cost.fetch_sub(cost, Ordering::Relaxed);

        for account_key in &tx_cost.writable_accounts {
            if let Some(mut entry) = self.cost_by_writable_accounts.get_mut(account_key) {
                entry.0 -= cost;
            }
        }
    }

    pub fn reset(&self) {
        self.block_cost.store(0, Ordering::Relaxed);
        self.cost_by_writable_accounts.clear();
    }
}
