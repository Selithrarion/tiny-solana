use crate::syscalls;
use crate::{accounts::Account, programs, transactions::Instruction, types::Pubkey};
use dashmap::DashMap;
use solana_rbpf::elf::Executable;
use solana_rbpf::error::ProgramResult;
use solana_rbpf::memory_region::{MemoryMapping, MemoryRegion};
use solana_rbpf::program::SBPFVersion;
use solana_rbpf::vm::{ContextObject, EbpfVm};
use solana_rbpf::{ebpf, verifier::RequisiteVerifier};
use std::cell::{Ref, RefCell, RefMut};
use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq, Clone)]
pub enum ProgramError {
    #[error("account not found: {0:?}")]
    AccountNotFound(Pubkey),
    #[error("invalid instruction data")]
    InvalidInstructionData,
    #[error("insufficient funds")]
    InsufficientFunds,
    #[error("not enough account keys")]
    NotEnoughAccountKeys,
    #[error("missing required signature")]
    MissingRequiredSignature,
    #[error("read-only account was written")]
    ReadonlyAccountWritten,
    #[error("account is not executable")]
    AccountNotExecutable,
    #[error("account owner mismatch")]
    AccountOwnerMismatch,
    #[error("invalid account data")]
    InvalidAccountData,
    #[error("account borrow failed")]
    AccountBorrowFailed,
    #[error("invalid arg")]
    InvalidArgument,
    #[error("program failed: {0}")]
    Custom(u32),
    #[error("internal error: {0}")]
    InternalError(String),
}

impl From<solana_rbpf::error::EbpfError> for ProgramError {
    fn from(e: solana_rbpf::error::EbpfError) -> Self {
        ProgramError::InternalError(format!("ebpf error: {}", e))
    }
}

impl From<solana_program::program_error::ProgramError> for ProgramError {
    fn from(e: solana_program::program_error::ProgramError) -> Self {
        match e {
            solana_program::program_error::ProgramError::InvalidAccountData => {
                ProgramError::InvalidAccountData
            }
            _ => ProgramError::InternalError(format!("external program error: {:?}", e)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AccountInfo {
    pub pubkey: Pubkey,
    pub is_signer: bool,
    pub is_writable: bool,
    pub account: Rc<RefCell<Account>>,
    pub executable: bool,
    pub rent_epoch: u64,
}

impl AccountInfo {
    pub fn lamports(&self) -> Result<u64, ProgramError> {
        Ok(self
            .account
            .try_borrow()
            .map_err(|_| ProgramError::AccountBorrowFailed)?
            .lamports)
    }

    pub fn try_borrow_data(&'_ self) -> Result<Ref<'_, [u8]>, ProgramError> {
        self.account
            .try_borrow()
            .map(|account| Ref::map(account, |acc| acc.data.as_slice()))
            .map_err(|_| ProgramError::AccountBorrowFailed)
    }

    pub fn try_borrow_mut_data(&'_ mut self) -> Result<RefMut<'_, Vec<u8>>, ProgramError> {
        self.account
            .try_borrow_mut()
            .map(|account| RefMut::map(account, |acc| &mut acc.data))
            .map_err(|_| ProgramError::AccountBorrowFailed)
    }

    pub fn owner(&self) -> Result<Pubkey, ProgramError> {
        Ok(self
            .account
            .try_borrow()
            .map_err(|_| ProgramError::AccountBorrowFailed)?
            .owner)
    }

    pub fn set_lamports(&self, lamports: u64) -> Result<(), ProgramError> {
        self.account
            .try_borrow_mut()
            .map_err(|_| ProgramError::AccountBorrowFailed)?
            .lamports = lamports;
        Ok(())
    }

    pub fn set_owner(&self, owner: Pubkey) -> Result<(), ProgramError> {
        self.account
            .try_borrow_mut()
            .map_err(|_| ProgramError::AccountBorrowFailed)?
            .owner = owner;
        Ok(())
    }
}

pub struct InvokeContext<'a> {
    program_executor: &'a dyn ProgramExecutor, // TODO: executor should be part of Bank i guess to get rid of lifetimes?
    pub program_id: Pubkey,
    pub instruction_data: &'a [u8],
    call_stack: Vec<Pubkey>,
    log_collector: Rc<RefCell<Vec<String>>>,
    compute_meter: Rc<RefCell<u64>>,
    compute_budget: u64,
    // TODO: from agave
    // pub transaction_context: &'a mut TransactionContext<'ix_data>,
    // pub program_cache_for_tx_batch: &'a mut ProgramCacheForTxBatch,
    // pub environment_config: EnvironmentConfig<'a>,
    // pub timings: ExecuteDetailsTimings,
    // pub syscall_context: Vec<Option<SyscallContext>>,
    // pub register_traces: Vec<(usize, Vec<[u64; 12]>)>,
}

impl<'a> InvokeContext<'a> {
    pub fn new(
        program_id: Pubkey,
        instruction_data: &'a [u8],
        program_executor: &'a dyn ProgramExecutor,
        log_collector: Rc<RefCell<Vec<String>>>,
        compute_budget: u64,
    ) -> Self {
        Self {
            program_executor,
            program_id,
            instruction_data,
            call_stack: Vec::with_capacity(5),
            log_collector,
            compute_meter: Rc::new(RefCell::new(0)),
            compute_budget,
        }
    }

    pub fn get_log_collector(&self) -> Rc<RefCell<Vec<String>>> {
        self.log_collector.clone()
    }

    pub fn invoke(
        &mut self,
        instruction: &Instruction,
        accounts: &mut [AccountInfo],
    ) -> Result<(), ProgramError> {
        // TODO: check call depth, signer privilege escalation
        self.call_stack.push(instruction.program_id);
        let result = self.program_executor.execute(instruction, self, accounts);
        self.call_stack.pop();
        result
    }
}

impl<'a> ContextObject for InvokeContext<'a> {
    fn trace(&mut self, _state: [u64; 12]) {
        todo!()
    }

    fn consume(&mut self, amount: u64) {
        *self.compute_meter.borrow_mut() = self.compute_meter.borrow().saturating_add(amount);
    }

    fn get_remaining(&self) -> u64 {
        self.compute_budget
            .saturating_sub(*self.compute_meter.borrow())
    }
}

pub type ProgramEntrypoint = fn(&mut InvokeContext, &mut [AccountInfo]) -> Result<(), ProgramError>;

pub trait ProgramExecutor: Send + Sync {
    fn execute(
        &self,
        instruction: &Instruction,
        invoke_context: &mut InvokeContext,
        accounts: &mut [AccountInfo], // TODO: should be AccountInfo<'a>
    ) -> Result<(), ProgramError>;
}

#[derive(Clone)]
pub struct NativeProgramExecutor {
    programs: HashMap<Pubkey, ProgramEntrypoint>,
}

impl NativeProgramExecutor {
    pub fn new() -> Self {
        let mut programs: HashMap<Pubkey, ProgramEntrypoint> = HashMap::new();

        programs.insert(
            programs::ids::SYSTEM_PROGRAM_ID,
            programs::system_program::entrypoint,
        );
        programs.insert(
            programs::ids::TOKEN_PROGRAM_ID,
            programs::token_program::entrypoint,
        );

        programs.insert(
            programs::ids::BPF_LOADER_UPGRADEABLE_PROGRAM_ID,
            programs::bpf_loader_program::entrypoint,
        );

        Self { programs }
    }
}

impl ProgramExecutor for NativeProgramExecutor {
    fn execute(
        &self,
        instruction: &Instruction,
        invoke_context: &mut InvokeContext,
        accounts: &mut [AccountInfo],
    ) -> Result<(), ProgramError> {
        if let Some(program_entrypoint) = self.programs.get(&instruction.program_id) {
            program_entrypoint(invoke_context, accounts)
        } else {
            Err(ProgramError::AccountNotFound(instruction.program_id))
        }
    }
}

#[derive(Clone)]
pub struct RbpfProgramExecutor {
    program_cache: DashMap<Pubkey, Vec<u8>>,
}

impl RbpfProgramExecutor {
    pub fn new() -> Self {
        Self {
            program_cache: DashMap::new(),
        }
    }
}

impl ProgramExecutor for RbpfProgramExecutor {
    fn execute(
        &self,
        instruction: &Instruction,
        invoke_context: &mut InvokeContext,
        accounts: &mut [AccountInfo],
    ) -> Result<(), ProgramError> {
        let program_id = instruction.program_id;

        if !self.program_cache.contains_key(&program_id) {
            let program_account_info = accounts
                .iter()
                .find(|a| a.pubkey == program_id)
                .ok_or_else(|| {
                    ProgramError::InternalError(
                        "program account not found in accounts list".to_string(),
                    )
                })?; // this should not happen

            if !program_account_info.executable {
                return Err(ProgramError::AccountNotExecutable);
            }

            let bytecode = program_account_info.try_borrow_data()?.to_vec();
            self.program_cache.insert(program_id, bytecode);
        }

        let bytecode = self.program_cache.get(&program_id).unwrap();

        let loader = Arc::new(syscalls::create_program_runtime());
        let sbpf_version = SBPFVersion::V2;
        let config = loader.get_config();

        let executable = Executable::<InvokeContext>::from_elf(&bytecode, loader.clone())
            .map_err(|e| ProgramError::InternalError(format!("failed to load elf: {}", e)))?;
        executable
            .verify::<RequisiteVerifier>()
            .map_err(|e| ProgramError::InternalError(format!("elf verification failed: {}", e)))?;

        let mut input_mem = serialize_parameters(accounts, &instruction.data)?;
        let regions = vec![MemoryRegion::new_writable(
            &mut input_mem,
            ebpf::MM_INPUT_START,
        )];

        let memory_mapping = MemoryMapping::new(regions, config, &sbpf_version)
            .map_err(|e| ProgramError::InternalError(format!("memory mapping: {}", e)))?;

        let mut vm = EbpfVm::new(
            loader.clone(),
            &sbpf_version,
            invoke_context,
            memory_mapping,
            config.stack_size(),
        );

        let (instruction_count, result) = vm.execute_program(&executable, true);

        tracing::debug!(
            "bpf executed: {} instructions, result: {:?}",
            instruction_count,
            result
        );

        let result_code = match result {
            ProgramResult::Ok(val) => val,
            ProgramResult::Err(e) => return Err(e.into()),
        };

        if result_code != 0 {
            // TODO: map to solana program errors
            return Err(ProgramError::Custom(result_code as u32));
        }

        invoke_context.consume(instruction_count);
        if invoke_context.get_remaining() == 0 {
            return Err(ProgramError::InternalError(
                "computational budget exceeded".to_string(),
            ));
        }

        deserialize_parameters(accounts, &input_mem)?;

        Ok(())
    }
}

const NON_DUP_MARKER: u8 = u8::MAX;

enum SerializeAccount<'a> {
    Original(Ref<'a, Account>),
    Duplicate(u8), // index of the original account
}

fn serialize_parameters(
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> Result<Vec<u8>, ProgramError> {
    let mut first_indices = HashMap::new();
    let mut serialized_accounts = Vec::with_capacity(accounts.len());
    for (i, account_info) in accounts.iter().enumerate() {
        if let Some(first_index) = first_indices.get(&account_info.pubkey) {
            serialized_accounts.push(SerializeAccount::Duplicate(*first_index as u8));
        } else {
            first_indices.insert(account_info.pubkey, i);
            let account = account_info
                .account
                .try_borrow()
                .map_err(|_| ProgramError::AccountBorrowFailed)?;
            serialized_accounts.push(SerializeAccount::Original(account));
        }
    }

    let mut size = 8 + instruction_data.len() + 8; // num_accounts + instruction_data + instruction_data_len
    for ser_account in &serialized_accounts {
        size += 1; // dup_marker
        if let SerializeAccount::Original(account) = ser_account {
            size += 1 + 1 + 32 + 32 + 8 + 8; // is_signer, is_writable, pubkey, owner, lamports, data_len
            size += account.data.len();
            size += (8 - (account.data.len() % 8)) % 8; // padding
        }
    }

    let mut buf = Vec::with_capacity(size);
    buf.write_all(&(accounts.len() as u64).to_le_bytes())
        .map_err(|e| ProgramError::InternalError(format!("failed to serialize params: {}", e)))?;

    for (i, ser_account) in serialized_accounts.into_iter().enumerate() {
        match ser_account {
            SerializeAccount::Original(account) => {
                buf.push(NON_DUP_MARKER);
                let account_info = &accounts[i];
                buf.push(account_info.is_signer as u8);
                buf.push(account_info.is_writable as u8);
                buf.write_all(&account_info.pubkey.0).map_err(|e| {
                    ProgramError::InternalError(format!("failed to serialize params: {}", e))
                })?;
                buf.write_all(&account.owner.0).map_err(|e| {
                    ProgramError::InternalError(format!("failed to serialize params: {}", e))
                })?;
                buf.write_all(&account.lamports.to_le_bytes())
                    .map_err(|e| {
                        ProgramError::InternalError(format!("failed to serialize params: {}", e))
                    })?;
                buf.write_all(&(account.data.len() as u64).to_le_bytes())
                    .map_err(|e| {
                        ProgramError::InternalError(format!("failed to serialize params: {}", e))
                    })?;
                buf.write_all(&account.data).map_err(|e| {
                    ProgramError::InternalError(format!("failed to serialize params: {}", e))
                })?;

                let padding = (8 - (account.data.len() % 8)) % 8;
                buf.extend(std::iter::repeat(0u8).take(padding));
            }
            SerializeAccount::Duplicate(first_index) => {
                buf.push(first_index);
            }
        }
    }

    buf.write_all(&(instruction_data.len() as u64).to_le_bytes())
        .map_err(|e| ProgramError::InternalError(format!("failed to serialize params: {}", e)))?;
    buf.write_all(instruction_data)
        .map_err(|e| ProgramError::InternalError(format!("failed to serialize params: {}", e)))?;

    Ok(buf)
}

fn deserialize_parameters(accounts: &mut [AccountInfo], input: &[u8]) -> Result<(), ProgramError> {
    let mut offset = 8; // skip account count
    let input_len = input.len();

    for account_info in accounts.iter_mut() {
        if offset >= input_len {
            return Err(ProgramError::InvalidAccountData);
        }
        let is_duplicate = input[offset] != NON_DUP_MARKER;
        offset += 1; // skip dup_marker

        if is_duplicate {
            continue; // update only original one
        }

        let is_writable = input[offset + 1] != 0;
        offset += 1 + 1 + 32; // is_signer, is_writable, pubkey

        if is_writable {
            let mut account = account_info
                .account
                .try_borrow_mut()
                .map_err(|_| ProgramError::AccountBorrowFailed)?;

            if offset + 32 > input_len {
                return Err(ProgramError::InvalidAccountData);
            }
            account.owner = Pubkey(
                input[offset..offset + 32]
                    .try_into()
                    .map_err(|_| ProgramError::InvalidAccountData)?,
            );
            offset += 32;

            if offset + 8 > input_len {
                return Err(ProgramError::InvalidAccountData);
            }
            account.lamports = u64::from_le_bytes(
                input[offset..offset + 8]
                    .try_into()
                    .map_err(|_| ProgramError::InvalidAccountData)?,
            );
            offset += 8;

            if offset + 8 > input_len {
                return Err(ProgramError::InvalidAccountData);
            }
            let data_len = u64::from_le_bytes(
                input[offset..offset + 8]
                    .try_into()
                    .map_err(|_| ProgramError::InvalidAccountData)?,
            ) as usize;
            offset += 8;

            if offset + data_len > input_len {
                return Err(ProgramError::InvalidAccountData);
            }
            let data = &input[offset..offset + data_len];

            // TODO: reallocate if new data length is larger than capacity?
            account.data.clear();
            account.data.extend_from_slice(data);

            let padding = (8 - (data_len % 8)) % 8;
            if offset + data_len + padding > input_len {
                return Err(ProgramError::InvalidAccountData);
            }
            offset += data_len + padding;
        } else {
            // owner(32) + lamports(8) + data_len(8)
            let data_len_offset = offset + 32 + 8;
            if data_len_offset + 8 > input_len {
                return Err(ProgramError::InvalidAccountData);
            }
            let data_len = u64::from_le_bytes(
                input[data_len_offset..data_len_offset + 8]
                    .try_into()
                    .map_err(|_| ProgramError::InvalidAccountData)?,
            ) as usize;
            let padding = (8 - (data_len % 8)) % 8;
            offset += 32 + 8 + 8 + data_len + padding; // owner, lamports, data_len, data, padding
            if offset > input_len {
                return Err(ProgramError::InvalidAccountData);
            }
        }
    }

    Ok(())
}

#[derive(Clone)]
pub struct ChainedProgramExecutor {
    native_executor: NativeProgramExecutor,
    rbpf_executor: RbpfProgramExecutor,
}

impl ChainedProgramExecutor {
    pub fn new() -> Self {
        Self {
            native_executor: NativeProgramExecutor::new(),
            rbpf_executor: RbpfProgramExecutor::new(),
        }
    }
}

impl ProgramExecutor for ChainedProgramExecutor {
    fn execute(
        &self,
        instruction: &Instruction,
        invoke_context: &mut InvokeContext,
        accounts: &mut [AccountInfo],
    ) -> Result<(), ProgramError> {
        let program_id = &instruction.program_id;

        // check if it's a pre-compiled native program
        if self.native_executor.programs.contains_key(program_id) {
            return self
                .native_executor
                .execute(instruction, invoke_context, accounts);
        }

        // if not, find the program's account to see if it's an on-chain bpf program
        let program_account_info = accounts
            .iter()
            .find(|acc| acc.pubkey == *program_id)
            .ok_or(ProgramError::AccountNotFound(*program_id))?;

        // must be marked as executable
        if !program_account_info.executable {
            return Err(ProgramError::AccountNotExecutable);
        }

        // owner must be the bpf loader program
        if program_account_info.owner()? != programs::ids::BPF_LOADER_UPGRADEABLE_PROGRAM_ID {
            return Err(ProgramError::AccountOwnerMismatch);
        }

        // execute as a bpf program
        self.rbpf_executor
            .execute(instruction, invoke_context, accounts)
    }
}
