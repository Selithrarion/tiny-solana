use crate::runtime::AccountInfo;
use crate::{
    accounts::{Account, AccountError, AccountStore, OverlayStore as OverlayStoreGeneric},
    runtime::{ChainedProgramExecutor, InvokeContext, ProgramError, ProgramExecutor},
    sync::TrackedRwLock,
    transactions::{Instruction, Transaction, TransactionError},
    types::{Hash, Pubkey},
};
use anyhow::Result;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::atomic::AtomicUsize;
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub type OverlayStore = OverlayStoreGeneric<dyn AccountStore>;

pub const MAX_BLOCKHASH_QUEUE_DEPTH: usize = 150;

#[derive(Debug, Clone)]
pub struct BlockhashInfo {
    pub hash: Hash,
    pub created_at_slot: u64,
}

pub struct Bank {
    pub slot: u64,
    pub accounts: Arc<dyn AccountStore>,
    pub blockhash_queue: VecDeque<BlockhashInfo>,
    pub parent: Option<Arc<Bank>>,
    pub parent_hash: Hash,
    is_frozen: AtomicBool,
    freeze_lock_count: AtomicUsize,
    finalized_hash: TrackedRwLock<Option<Hash>>,
    program_executor: Arc<dyn ProgramExecutor>,
    processed_transactions: AtomicUsize,
    failed_transactions: AtomicUsize,
}
// TODO: QoS Service
// TODO: bank lifecycle management
//  - a bank is created for a slot
//  - it processes transactions
//  - it gets frozen
//  - a new bank is created from it for the next slot
//  - this process updates parent_hash, blockhash_queue, etc.
// TODO: PoH integration with bank lifecycle management
//   - slot boundaries, grace ticks, and transaction recording with height checks.

impl Bank {
    pub fn new(accounts: Arc<dyn AccountStore>) -> Self {
        let mut blockhash_queue = VecDeque::with_capacity(MAX_BLOCKHASH_QUEUE_DEPTH);
        blockhash_queue.push_back(BlockhashInfo {
            hash: Hash::default(),
            created_at_slot: 0,
        });
        Self {
            slot: 0,
            accounts,
            blockhash_queue,
            parent: None,
            parent_hash: Hash::default(),
            is_frozen: AtomicBool::new(false),
            freeze_lock_count: AtomicUsize::new(0),
            finalized_hash: TrackedRwLock::new(None, "finalized_hash"),
            program_executor: Arc::new(ChainedProgramExecutor::new()),
            processed_transactions: AtomicUsize::new(0),
            failed_transactions: AtomicUsize::new(0),
        }
    }

    pub fn new_from_parent(parent: Arc<Bank>) -> Self {
        parent.freeze();
        // TODO: implement cow for AccountStore, now we are using the same account db

        let mut blockhash_queue = parent.blockhash_queue.clone();
        blockhash_queue.push_back(BlockhashInfo {
            hash: parent.hash(),
            created_at_slot: parent.slot,
        });
        if blockhash_queue.len() > MAX_BLOCKHASH_QUEUE_DEPTH {
            blockhash_queue.pop_front();
        }

        Self {
            slot: parent.slot + 1,
            accounts: parent.accounts.clone(),
            blockhash_queue,
            parent_hash: parent.hash(),
            is_frozen: AtomicBool::new(false),
            freeze_lock_count: AtomicUsize::new(0),
            finalized_hash: TrackedRwLock::new(None, "finalized_hash"),
            program_executor: parent.program_executor.clone(),
            processed_transactions: AtomicUsize::new(0),
            failed_transactions: AtomicUsize::new(0),
            parent: Some(parent),
        }
    }

    pub fn freeze(&self) {
        while self.freeze_lock_count.load(Ordering::Acquire) > 0 {
            std::thread::yield_now();
        }

        if self.is_frozen.swap(true, Ordering::AcqRel) {
            return;
        }

        let mut final_hash = self.finalized_hash.write();
        if final_hash.is_none() {
            let hash = self
                .accounts
                .get_root_hash()
                .expect("failed to compute root hash during freeze");
            *final_hash = Some(hash);
        }
    }

    pub fn hash(&self) -> Hash {
        *self
            .finalized_hash
            .read()
            .as_ref()
            .unwrap_or(&self.parent_hash)
    }

    pub fn is_frozen(&self) -> bool {
        self.is_frozen.load(Ordering::Relaxed)
    }

    pub fn acquire_freeze_lock(&'_ self) -> FreezeLockGuard<'_> {
        self.freeze_lock_count.fetch_add(1, Ordering::Acquire);
        FreezeLockGuard { bank: self }
    }

    pub fn simulate_transaction(&self, tx: &Transaction) -> Result<(), TransactionError> {
        let temp_overlay = OverlayStoreGeneric::new(self.accounts.clone());
        let mut loaded_accounts_rc = self.load_and_verify_accounts(tx, &temp_overlay)?;
        self.execute_instructions(tx, &mut loaded_accounts_rc, 200_000)?; // TODO: compute budget
        Ok(())
    }

    pub fn process_single_transaction(
        &self,
        tx: &Transaction,
        compute_budget: u64,
    ) -> Result<OverlayStore, TransactionError> {
        // TODO: check_account_privileges fn
        // TODO: per-instruction checks
        // TODO: AccountLoader ?
        // TODO: check rent

        self.check_preflight_rules(&tx)?;

        let temp_overlay = OverlayStore::new(self.accounts.clone());

        let mut loaded_accounts_rc = self.load_and_verify_accounts(tx, &temp_overlay)?;

        self.execute_instructions(&tx, &mut loaded_accounts_rc, compute_budget)?;

        for account_rc in loaded_accounts_rc {
            let account = account_rc.borrow();
            temp_overlay
                .store(&account.pubkey, account.clone())
                .map_err(|e| {
                    TransactionError::ProgramError(ProgramError::InternalError(e.to_string()))
                })?;
        }

        Ok(temp_overlay)
    }

    pub fn check_preflight_rules(&self, tx: &Transaction) -> Result<(), TransactionError> {
        if self.is_frozen() {
            return Err(TransactionError::from(ProgramError::InternalError(
                "bank is frozen".to_string(),
            )));
        }

        match self
            .blockhash_queue
            .iter()
            .find(|info| info.hash == tx.message.recent_blockhash)
        {
            Some(info) => {
                let last_valid_slot = info
                    .created_at_slot
                    .saturating_add(MAX_BLOCKHASH_QUEUE_DEPTH as u64);
                if self.slot > last_valid_slot {
                    return Err(TransactionError::BlockhashExpired);
                }
            }
            None => {
                return Err(TransactionError::BlockhashNotFound);
            }
        }

        Ok(())
    }

    pub fn load_and_verify_accounts<'a, S: AccountStore>(
        &self,
        tx: &'a Transaction,
        store: &'a S,
    ) -> Result<Vec<Rc<RefCell<Account>>>, TransactionError> {
        let mut loaded_accounts_rc = Vec::new();

        for key in &tx.message.account_keys {
            let account = store.load(key).unwrap_or_else(|| Account {
                pubkey: *key,
                owner: crate::programs::ids::SYSTEM_PROGRAM_ID,
                ..Account::default()
            });
            loaded_accounts_rc.push(Rc::new(RefCell::new(account)));
        }

        Ok(loaded_accounts_rc)
    }

    pub fn execute_instructions(
        &self,
        tx: &Transaction,
        loaded_accounts: &mut [Rc<RefCell<Account>>],
        compute_budget: u64,
    ) -> Result<(), TransactionError> {
        for instruction in &tx.message.instructions {
            let mut account_infos =
                self.prepare_account_infos(instruction, &tx.message.account_keys, loaded_accounts)?;

            let log_collector = Rc::new(RefCell::new(Vec::new()));
            let mut invoke_context = InvokeContext::new(
                instruction.program_id,
                &instruction.data,
                &*self.program_executor,
                log_collector.clone(),
                compute_budget,
            );
            self.program_executor
                .execute(instruction, &mut invoke_context, &mut account_infos)?;
        }

        Ok(())
    }

    fn prepare_account_infos<'a>(
        &self,
        instruction: &'a Instruction,
        tx_account_keys: &[Pubkey],
        accounts: &'a [Rc<RefCell<Account>>],
    ) -> Result<Vec<AccountInfo>, TransactionError> {
        // TODO: finish
        // let tx_account_keys_map: HashMap<Pubkey, usize> = tx_account_keys
        //     .iter()
        //     .enumerate()
        //     .map(|(i, &pubkey)| (pubkey, i))
        //     .collect();

        let mut account_infos = Vec::with_capacity(instruction.accounts.len());
        let mut borrowed_mutably = BTreeSet::new();

        for account_meta in &instruction.accounts {
            let account_index = tx_account_keys
                .iter()
                .position(|key| key == &account_meta.pubkey)
                .ok_or_else(|| {
                    TransactionError::ProgramError(ProgramError::InternalError(
                        "instruction account not found in transaction".to_string(),
                    ))
                })?;

            if account_meta.is_writable {
                if !borrowed_mutably.insert(account_index) {
                    return Err(TransactionError::ProgramError(ProgramError::InternalError(
                        "account borrowed mutably more than once".to_string(),
                    )));
                }
            }

            let account_rc = accounts.get(account_index).unwrap();

            account_infos.push(AccountInfo {
                pubkey: account_meta.pubkey,
                is_signer: account_meta.is_signer,
                is_writable: account_meta.is_writable,
                account: account_rc.clone(),
                executable: account_rc.borrow().executable,
                rent_epoch: account_rc.borrow().rent_epoch,
            });
        }

        Ok(account_infos)
    }

    pub fn record_transaction_metrics(&self, result: &Result<(), TransactionError>) {
        if result.is_ok() {
            self.processed_transactions.fetch_add(1, Ordering::Relaxed);
        } else {
            self.failed_transactions.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub struct FreezeLockGuard<'a> {
    bank: &'a Bank,
}

impl<'a> Drop for FreezeLockGuard<'a> {
    fn drop(&mut self) {
        self.bank.freeze_lock_count.fetch_sub(1, Ordering::Release);
    }
}
