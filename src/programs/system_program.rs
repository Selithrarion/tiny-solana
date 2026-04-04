use crate::{
    runtime::{AccountInfo, InvokeContext, ProgramError},
    transactions::Instruction,
    types::Pubkey,
};

use crate::programs::ids::SYSTEM_PROGRAM_ID;
use crate::transactions::AccountMeta;
use anyhow::Context;
use bincode::Options;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum SystemInstruction {
    /// `[signer, writable]` the source account
    /// `[writable]` the destination account
    Transfer { lamports: u64 },

    /// `[writable, signer]` account to be assigned
    Assign { owner: Pubkey },

    /// `[signer, writable]` fee payer
    /// `[signer, writable]` new account
    CreateAccount {
        lamports: u64,
        space: u64,
        owner: Pubkey,
    },
}

pub fn create_assign_instruction(
    target_pubkey: Pubkey,
    new_owner_pubkey: &Pubkey,
) -> Result<Instruction, ProgramError> {
    Ok(Instruction {
        program_id: SYSTEM_PROGRAM_ID,
        accounts: vec![AccountMeta {
            pubkey: target_pubkey,
            is_signer: true,
            is_writable: true,
        }],
        data: bincode::options()
            .with_fixint_encoding()
            .serialize(&SystemInstruction::Assign {
                owner: *new_owner_pubkey,
            })
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    })
}

pub fn create_account_instruction(
    from_pubkey: Pubkey,
    to_pubkey: Pubkey,
    lamports: u64,
    space: u64,
    owner: Pubkey,
) -> Result<Instruction, ProgramError> {
    Ok(Instruction {
        program_id: SYSTEM_PROGRAM_ID,
        accounts: vec![
            AccountMeta {
                pubkey: from_pubkey,
                is_signer: true,
                is_writable: true,
            },
            AccountMeta {
                pubkey: to_pubkey,
                is_signer: true,
                is_writable: true,
            },
        ],
        data: bincode::options()
            .with_fixint_encoding()
            .serialize(&SystemInstruction::CreateAccount {
                lamports,
                space,
                owner,
            })
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    })
}

pub fn entrypoint(
    invoke_context: &mut InvokeContext,
    accounts: &mut [AccountInfo],
) -> Result<(), ProgramError> {
    let instruction: SystemInstruction = bincode::options()
        .with_fixint_encoding()
        .deserialize(invoke_context.instruction_data)
        .context("failed to deserialize system instruction")
        .map_err(|_| ProgramError::InvalidInstructionData)?;

    match instruction {
        SystemInstruction::Transfer { lamports } => {
            if accounts.len() < 2 {
                return Err(ProgramError::InternalError(
                    "transfer requires 2 accounts".to_string(),
                ));
            }

            let (from_slice, to_slice) = accounts.split_at_mut(1);
            let from = &mut from_slice[0];
            let to = &mut to_slice[0];

            if !from.is_signer {
                return Err(ProgramError::MissingRequiredSignature);
            }

            if from.lamports()? < lamports {
                return Err(ProgramError::InsufficientFunds);
            }

            from.set_lamports(from.lamports()?.saturating_sub(lamports))?;
            to.set_lamports(to.lamports()?.saturating_add(lamports))?;

            Ok(())
        }
        SystemInstruction::Assign { owner } => {
            if accounts.len() < 1 {
                return Err(ProgramError::InternalError(
                    "assign requires 1 account".to_string(),
                ));
            }

            let target_account = &mut accounts[0];
            if !target_account.is_signer {
                return Err(ProgramError::MissingRequiredSignature);
            }

            target_account.set_owner(owner)?;
            Ok(())
        }
        SystemInstruction::CreateAccount {
            lamports,
            space,
            owner,
        } => {
            if accounts.len() < 2 {
                return Err(ProgramError::NotEnoughAccountKeys);
            }
            let (from_slice, to_slice) = accounts.split_at_mut(1);
            let from = &mut from_slice[0];
            let to = &mut to_slice[0];

            if !from.is_signer || !to.is_signer {
                return Err(ProgramError::MissingRequiredSignature);
            }

            if from.lamports()? < lamports {
                return Err(ProgramError::InsufficientFunds);
            }

            from.set_lamports(from.lamports()?.saturating_sub(lamports))?;
            to.set_lamports(to.lamports()?.saturating_add(lamports))?;
            *to.try_borrow_mut_data()? = vec![0; space as usize];
            to.set_owner(owner)?;

            Ok(())
        }
    }
}
