use crate::{
    programs::system_program,
    runtime::{AccountInfo, InvokeContext, ProgramError},
    types::Pubkey,
};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use solana_program::sysvar::rent::Rent;
use thiserror::Error;

// TODO: thats a v3 version, wanna do v4 too

#[derive(Error, Debug, Copy, Clone, PartialEq, Eq)]
pub enum BpfLoaderError {
    #[error("not enough account keys")]
    NotEnoughAccountKeys,
    #[error("invalid account data")]
    InvalidAccountData,
    #[error("invalid buffer account")]
    InvalidBufferAccount,
    #[error("invalid program account")]
    InvalidProgramAccount,
    #[error("buffer is immutable")]
    ImmutableBuffer,
    #[error("incorrect authority provided")]
    IncorrectAuthority,
    #[error("missing required signature")]
    MissingRequiredSignature,
    #[error("program account already initialized")]
    AccountAlreadyInitialized,
    #[error("program account too small")]
    AccountDataTooSmall,
    #[error("program account not rent-exempt")]
    AccountNotRentExempt,
    #[error("max data length is too small to hold buffer data")]
    MaxDataLengthTooSmall,
    #[error("invalid argument")]
    InvalidArgument,
    #[error("buffer account too small")]
    BufferAccountTooSmall,
}

impl From<BpfLoaderError> for ProgramError {
    fn from(e: BpfLoaderError) -> Self {
        match e {
            BpfLoaderError::InvalidAccountData => ProgramError::InvalidAccountData,
            BpfLoaderError::MissingRequiredSignature => ProgramError::MissingRequiredSignature,
            BpfLoaderError::NotEnoughAccountKeys => ProgramError::NotEnoughAccountKeys,
            _ => ProgramError::Custom(e as u32),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum UpgradeableLoaderState {
    Uninitialized,
    Buffer {
        authority_address: Option<Pubkey>,
    },
    Program {
        programdata_address: Pubkey,
    },
    ProgramData {
        slot: u64,
        authority_address: Option<Pubkey>,
    },
}

impl Default for UpgradeableLoaderState {
    fn default() -> Self {
        Self::Uninitialized
    }
}

impl UpgradeableLoaderState {
    pub fn size_of_uninitialized() -> usize {
        bincode::serialized_size(&Self::Uninitialized).unwrap() as usize
    }

    pub fn size_of_buffer_metadata() -> usize {
        bincode::serialized_size(&Self::Buffer {
            authority_address: Some(Pubkey::default()),
        })
        .unwrap() as usize
    }

    pub fn size_of_buffer(len: usize) -> usize {
        len.saturating_add(Self::size_of_buffer_metadata())
    }

    pub fn size_of_program() -> usize {
        bincode::serialized_size(&Self::Program {
            programdata_address: Pubkey::default(),
        })
        .unwrap() as usize
    }

    pub fn size_of_programdata_metadata() -> usize {
        bincode::serialized_size(&Self::ProgramData {
            slot: 0,
            authority_address: Some(Pubkey::default()),
        })
        .unwrap() as usize
    }

    pub fn size_of_programdata(len: usize) -> usize {
        len.saturating_add(Self::size_of_programdata_metadata())
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum LoaderInstruction {
    /// Accounts expected:
    /// 0. `[writable]` The buffer account.
    /// 1. `[signer]` The buffer's authority.
    InitializeBuffer,

    /// Accounts expected:
    /// 0. `[writable]` The buffer account.
    /// 1. `[signer]` The buffer's authority.
    Write { offset: u32, bytes: Vec<u8> },

    /// Accounts expected:
    /// 0. `[signer, writable]` The payer account.
    /// 1. `[writable]` The programdata account.
    /// 2. `[writable]` The program account.
    /// 3. `[writable]` The buffer account.
    /// 4. `[]` Rent sysvar.
    /// 5. `[]` Clock sysvar.
    /// 6. `[]` System program.
    /// 7. `[signer]` The upgrade authority.
    DeployWithMaxDataLen { max_data_len: u64 },

    /// Accounts expected:
    /// 0. `[writable]` The programdata account.
    /// 1. `[writable]` The program account.
    /// 2. `[writable]` The buffer account.
    /// 3. `[writable]` The spill account, receiving rent.
    /// 4. `[]` Rent sysvar.
    /// 5. `[]` Clock sysvar.
    /// 6. `[signer]` The upgrade authority.
    Upgrade,

    /// Accounts expected:
    /// 0. `[writable]` The buffer or programdata account.
    /// 1. `[signer]` The current authority.
    /// 2. `[]` The new authority.
    SetAuthority { new_authority: Option<Pubkey> },

    /// Accounts expected:
    /// 0. `[writable]` The account to close.
    /// 1. `[writable]` The account to receive the lamports.
    /// 2. `[signer]` The authority of the account to close.
    Close,
}

pub fn entrypoint(
    invoke_context: &mut InvokeContext,
    accounts: &mut [AccountInfo],
) -> Result<(), ProgramError> {
    let program_id = invoke_context.program_id;
    let instruction: LoaderInstruction = bincode::deserialize(invoke_context.instruction_data)
        .context("failed to deserialize loader instruction")
        .map_err(|_| ProgramError::InvalidInstructionData)?;

    match instruction {
        LoaderInstruction::InitializeBuffer => {
            if accounts.len() < 1 {
                return Err(BpfLoaderError::NotEnoughAccountKeys.into());
            }
            let (buffer_info_slice, authority_slice) = accounts.split_at_mut(1);
            let buffer_info = &mut buffer_info_slice[0];
            let authority_info = authority_slice.get(0);

            let state: UpgradeableLoaderState =
                bincode::deserialize(&buffer_info.try_borrow_data()?).unwrap_or_default();

            if state != UpgradeableLoaderState::Uninitialized {
                return Err(BpfLoaderError::AccountAlreadyInitialized.into());
            }

            let new_state = UpgradeableLoaderState::Buffer {
                authority_address: authority_info.map(|info| info.pubkey),
            };

            new_state.pack_into_account(buffer_info)?;

            Ok(())
        }
        LoaderInstruction::Write { offset, bytes } => {
            if accounts.len() < 2 {
                return Err(BpfLoaderError::NotEnoughAccountKeys.into());
            }
            let (buffer_info_slice, authority_info_slice) = accounts.split_at_mut(1);
            let buffer_info = &mut buffer_info_slice[0];
            let authority_info = &authority_info_slice[0];

            let mut buffer_state: UpgradeableLoaderState =
                bincode::deserialize(&buffer_info.try_borrow_data()?)
                    .map_err(|_| BpfLoaderError::InvalidAccountData)?;

            match &mut buffer_state {
                UpgradeableLoaderState::Buffer { authority_address } => {
                    if authority_address.is_none() {
                        return Err(BpfLoaderError::ImmutableBuffer.into());
                    }
                    if authority_address.unwrap() != authority_info.pubkey {
                        return Err(BpfLoaderError::IncorrectAuthority.into());
                    }
                    if !authority_info.is_signer {
                        return Err(BpfLoaderError::MissingRequiredSignature.into());
                    }

                    let start_index =
                        UpgradeableLoaderState::size_of_buffer_metadata() + offset as usize;
                    let mut buffer_data = buffer_info.try_borrow_mut_data()?;
                    let end_index = start_index + bytes.len();
                    if end_index > buffer_data.len() {
                        return Err(BpfLoaderError::AccountDataTooSmall.into());
                    }
                    buffer_data[start_index..end_index].copy_from_slice(&bytes);
                }
                _ => return Err(BpfLoaderError::InvalidBufferAccount.into()),
            }

            buffer_state.pack_into_account(buffer_info)?;

            Ok(())
        }
        LoaderInstruction::DeployWithMaxDataLen { max_data_len } => {
            if accounts.len() < 8 {
                return Err(BpfLoaderError::NotEnoughAccountKeys.into());
            }

            let (
                payer_info,
                programdata_info,
                program_info,
                buffer_info,
                rent_info,
                clock_info,
                system_program_info,
                authority_info,
            ) = parse_deploy_accounts(accounts)?;

            if !authority_info.is_signer {
                return Err(BpfLoaderError::MissingRequiredSignature.into());
            }

            let rent: Rent = bincode::deserialize(&rent_info.try_borrow_data()?)
                .map_err(|_| ProgramError::InvalidArgument)?;

            let buffer_data_len = {
                let buffer_data = buffer_info.try_borrow_data()?;
                let buffer_data_offset = UpgradeableLoaderState::size_of_buffer_metadata();
                if buffer_data.len() < buffer_data_offset {
                    return Err(BpfLoaderError::BufferAccountTooSmall.into());
                }
                let len = buffer_data.len().saturating_sub(buffer_data_offset);
                if max_data_len < len as u64 {
                    return Err(BpfLoaderError::MaxDataLengthTooSmall.into());
                }
                len
            };

            let programdata_len =
                UpgradeableLoaderState::size_of_programdata(max_data_len as usize);
            let required_payment = rent
                .minimum_balance(programdata_len)
                .saturating_sub(programdata_info.lamports()?);

            let create_account_ix = system_program::create_account_instruction(
                payer_info.pubkey,
                programdata_info.pubkey,
                required_payment,
                programdata_len as u64,
                program_id,
            )?;

            invoke_context.invoke(
                &create_account_ix,
                &mut [
                    payer_info.clone(),
                    programdata_info.clone(),
                    system_program_info.clone(),
                ],
            )?;

            let programdata_state = UpgradeableLoaderState::ProgramData {
                slot: bincode::deserialize::<solana_program::clock::Clock>(
                    &clock_info.try_borrow_data()?,
                )
                .map_err(|_| ProgramError::InvalidArgument)?
                .slot,
                authority_address: Some(authority_info.pubkey),
            };
            programdata_state.pack_into_account(programdata_info)?;

            // copy code from buffer to programdata
            let metadata_len = UpgradeableLoaderState::size_of_programdata_metadata();
            {
                let buffer_data_offset = UpgradeableLoaderState::size_of_buffer_metadata();
                let buffer_data = buffer_info.try_borrow_data()?;
                let mut programdata_data = programdata_info.try_borrow_mut_data()?;

                let src_slice =
                    &buffer_data[buffer_data_offset..buffer_data_offset + buffer_data_len];
                let dst_slice = &mut programdata_data[metadata_len..metadata_len + buffer_data_len];
                dst_slice.copy_from_slice(src_slice);
            }

            // initialize program account
            let program_state = UpgradeableLoaderState::Program {
                programdata_address: programdata_info.pubkey,
            };
            program_state.pack_into_account(program_info)?;

            {
                let mut program_account = program_info.account.borrow_mut();
                program_account.owner = program_id;
                program_account.executable = true;
            }

            // close buffer account and send lamports to payer
            payer_info.set_lamports(
                payer_info
                    .lamports()?
                    .saturating_add(buffer_info.lamports()?),
            )?;
            buffer_info.set_lamports(0)?;
            *buffer_info.try_borrow_mut_data()? = Vec::new();

            Ok(())
        }
        LoaderInstruction::Upgrade => Err(ProgramError::InternalError(
            "upgrade not implemented".to_string(),
        )),
        LoaderInstruction::SetAuthority { new_authority } => {
            if accounts.len() < 2 {
                return Err(BpfLoaderError::NotEnoughAccountKeys.into());
            }
            let (target_info_slice, authority_info_slice) = accounts.split_at_mut(1);
            let target_info = &mut target_info_slice[0];
            let authority_info = &authority_info_slice[0];

            if !authority_info.is_signer {
                return Err(BpfLoaderError::MissingRequiredSignature.into());
            }

            let mut state: UpgradeableLoaderState =
                bincode::deserialize(&target_info.try_borrow_data()?)
                    .map_err(|_| BpfLoaderError::InvalidAccountData)?;

            match &mut state {
                UpgradeableLoaderState::Buffer { authority_address }
                | UpgradeableLoaderState::ProgramData {
                    authority_address, ..
                } => {
                    if *authority_address != Some(authority_info.pubkey) {
                        return Err(BpfLoaderError::IncorrectAuthority.into());
                    }
                    *authority_address = new_authority;
                }
                _ => return Err(BpfLoaderError::InvalidArgument.into()),
            }

            state.pack_into_account(target_info)?;

            Ok(())
        }
        LoaderInstruction::Close => {
            if accounts.len() < 3 {
                return Err(BpfLoaderError::NotEnoughAccountKeys.into());
            }
            let (account_to_close_slice, rest) = accounts.split_at_mut(1);
            let (recipient_slice, authority_slice) = rest.split_at_mut(1);
            let account_to_close = &mut account_to_close_slice[0];
            let recipient = &mut recipient_slice[0];
            let authority_info = &authority_slice[0];

            // TODO: check authority
            if !authority_info.is_signer {
                return Err(BpfLoaderError::MissingRequiredSignature.into());
            }

            recipient.set_lamports(
                recipient
                    .lamports()?
                    .saturating_add(account_to_close.lamports()?),
            )?;
            account_to_close.set_lamports(0)?;
            *account_to_close.try_borrow_mut_data()? = Vec::new();

            Ok(())
        }
    }
}

trait PackableState: Serialize {
    fn pack_into_account(&self, account_info: &mut AccountInfo) -> Result<(), ProgramError> {
        let metadata_len = bincode::serialized_size(self)
            .map_err(|_| ProgramError::InternalError("failed to get serialized size".to_string()))?
            as usize;

        let mut data = account_info.try_borrow_mut_data()?;
        if data.len() < metadata_len {
            return Err(BpfLoaderError::AccountDataTooSmall.into());
        }

        bincode::serialize_into(&mut data[..metadata_len], self).map_err(|_| {
            ProgramError::InternalError("failed to serialize state into account".to_string())
        })
    }
}

impl PackableState for UpgradeableLoaderState {}

fn parse_deploy_accounts(
    accounts: &mut [AccountInfo],
) -> Result<
    (
        &mut AccountInfo,
        &mut AccountInfo,
        &mut AccountInfo,
        &mut AccountInfo,
        &AccountInfo,
        &AccountInfo,
        &AccountInfo,
        &AccountInfo,
    ),
    ProgramError,
> {
    if accounts.len() < 8 {
        return Err(BpfLoaderError::NotEnoughAccountKeys.into());
    }
    let (payer_slice, rest) = accounts.split_at_mut(1);
    let (programdata_slice, rest) = rest.split_at_mut(1);
    let (program_slice, rest) = rest.split_at_mut(1);
    let (buffer_slice, rest) = rest.split_at_mut(1);
    let (rent_slice, rest) = rest.split_at_mut(1);
    let (clock_slice, rest) = rest.split_at_mut(1);
    let (system_program_slice, authority_slice) = rest.split_at_mut(1);

    Ok((
        &mut payer_slice[0],
        &mut programdata_slice[0],
        &mut program_slice[0],
        &mut buffer_slice[0],
        &rent_slice[0],
        &clock_slice[0],
        &system_program_slice[0],
        &authority_slice[0],
    ))
}
