use crate::runtime::InvokeContext;
use crate::{
    programs::system_program,
    runtime::{AccountInfo, ProgramError},
    types::Pubkey,
};
use anyhow::Context;
use bincode::Options;
use serde::{Deserialize, Serialize};
use solana_program::{
    program_pack::{Pack, Sealed},
    sysvar::rent::Rent,
};
use thiserror::Error;
// TODO: token 2022 extensions (fees, zk-proofs, soulbound, interest)
// TODO: add AuthorityType enum

#[derive(Error, Debug, Copy, Clone, PartialEq, Eq)]
pub enum TokenError {
    #[error("not enough account keys")]
    NotEnoughAccountKeys,
    #[error("missing required signature")]
    MissingRequiredSignature,
    #[error("authority mismatch")]
    AuthorityMismatch,
    #[error("token account mint mismatch")]
    TokenAccountMintMismatch,
    #[error("source account not owned by signer")]
    SourceNotOwnedBySigner,
    #[error("token mint mismatch")]
    MintMismatch,
    #[error("account already initialized")]
    AlreadyInitialized,
    #[error("mint not initialized")]
    MintNotInitialized,
    #[error("account not rent-exempt")]
    NotRentExempt,
    #[error("insufficient funds")]
    InsufficientFunds,
}

impl From<TokenError> for ProgramError {
    fn from(e: TokenError) -> Self {
        match e {
            TokenError::InsufficientFunds => ProgramError::InsufficientFunds,
            TokenError::NotEnoughAccountKeys => ProgramError::NotEnoughAccountKeys,
            TokenError::MissingRequiredSignature => ProgramError::MissingRequiredSignature,
            // map custom errors to u32 codes
            _ => ProgramError::Custom(e as u32),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Default)]
pub struct Mint {
    pub mint_authority: Pubkey,
    pub supply: u64,
    pub decimals: u8,
    pub is_initialized: bool,
}

impl Sealed for Mint {}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Default)]
pub struct TokenAccount {
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub amount: u64,
    pub is_initialized: bool,
}

impl Sealed for TokenAccount {}

impl Pack for Mint {
    const LEN: usize = 32 + 8 + 1 + 1;

    fn unpack_from_slice(src: &[u8]) -> Result<Self, solana_program::program_error::ProgramError> {
        bincode::options()
            .with_fixint_encoding()
            .deserialize(src)
            .map_err(|_| solana_program::program_error::ProgramError::InvalidAccountData)
    }

    fn pack_into_slice(&self, dst: &mut [u8]) {
        let packed = bincode::options()
            .with_fixint_encoding()
            .serialize(self)
            .unwrap();
        dst[..packed.len()].copy_from_slice(&packed);
    }
}

impl Pack for TokenAccount {
    const LEN: usize = 32 + 32 + 8 + 1;
    fn unpack_from_slice(src: &[u8]) -> Result<Self, solana_program::program_error::ProgramError> {
        bincode::options()
            .with_fixint_encoding()
            .deserialize(src)
            .map_err(|_| solana_program::program_error::ProgramError::InvalidAccountData)
    }

    fn pack_into_slice(&self, dst: &mut [u8]) {
        let packed = bincode::options()
            .with_fixint_encoding()
            .serialize(self)
            .unwrap();
        dst[..packed.len()].copy_from_slice(&packed);
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum TokenInstruction {
    /// Accounts expected:
    /// 0. `[signer, writable]` The mint authority account.
    /// 1. `[writable]` The token mint account.
    /// 2. `[writable]` The destination token account.
    Mint { amount: u64 },

    /// Accounts expected:
    /// 0. `[signer]` The source account owner.
    /// 1. `[writable]` The source token account.
    /// 2. `[writable]` The destination token account.
    Transfer { amount: u64 },

    /// Accounts expected:
    /// 0. `[writable]` The mint or token account.
    /// 1. `[signer]` The current authority.
    /// 2. `[]` The new authority.
    SetAuthority { new_authority: Pubkey },

    /// Accounts expected:
    /// 0. `[writable]` The mint account to initialize.
    /// 1. `[]` The rent sysvar.
    InitializeMint {
        decimals: u8,
        mint_authority: Pubkey,
    },

    /// Accounts expected:
    /// 0. `[writable]` The token account to initialize.
    /// 1. `[]` The mint account.
    /// 2. `[]` The owner of the new token account.
    /// 3. `[]` The rent sysvar.
    InitializeAccount,
    // TODO: burn, approve
}

pub fn entrypoint(
    invoke_context: &mut InvokeContext,
    accounts: &mut [AccountInfo],
) -> Result<(), ProgramError> {
    let instruction: TokenInstruction = bincode::options()
        .with_fixint_encoding()
        .deserialize(invoke_context.instruction_data)
        .context("failed to deserialize token instruction data")
        .map_err(|_| ProgramError::InvalidInstructionData)?;

    match instruction {
        TokenInstruction::Mint { amount } => {
            if accounts.len() < 3 {
                return Err(TokenError::NotEnoughAccountKeys.into());
            }

            let (authority_slice, rest) = accounts.split_at_mut(1);
            let (mint_slice, dest_slice) = rest.split_at_mut(1);
            let (mint_authority_info, mint_info, destination_info) =
                (&authority_slice[0], &mut mint_slice[0], &mut dest_slice[0]);

            if !mint_authority_info.is_signer {
                return Err(TokenError::MissingRequiredSignature.into());
            }

            let mut mint = Mint::unpack_from_slice(&mint_info.try_borrow_data()?)?;
            if mint.mint_authority != mint_authority_info.pubkey {
                return Err(TokenError::AuthorityMismatch.into());
            }

            let mut destination_account =
                TokenAccount::unpack_from_slice(&destination_info.try_borrow_data()?)?;
            if destination_account.mint != mint_info.pubkey {
                return Err(TokenError::TokenAccountMintMismatch.into());
            }

            mint.supply = mint.supply.saturating_add(amount);
            destination_account.amount = destination_account.amount.saturating_add(amount);
            mint.pack_into_slice(&mut mint_info.try_borrow_mut_data()?);
            destination_account.pack_into_slice(&mut destination_info.try_borrow_mut_data()?);
            Ok(())
        }
        TokenInstruction::Transfer { amount } => {
            if accounts.len() < 3 {
                return Err(TokenError::NotEnoughAccountKeys.into());
            }

            let (owner_slice, rest) = accounts.split_at_mut(1);
            let (source_slice, dest_slice) = rest.split_at_mut(1);
            let (owner_info, source_info, destination_info) =
                (&owner_slice[0], &mut source_slice[0], &mut dest_slice[0]);

            if !owner_info.is_signer {
                return Err(TokenError::MissingRequiredSignature.into());
            }

            let mut source_account =
                TokenAccount::unpack_from_slice(&source_info.try_borrow_data()?)?;
            if source_account.owner != owner_info.pubkey {
                return Err(TokenError::SourceNotOwnedBySigner.into());
            }
            let mut destination_account =
                TokenAccount::unpack_from_slice(&destination_info.try_borrow_data()?)?;
            if source_account.mint != destination_account.mint {
                return Err(TokenError::MintMismatch.into());
            }
            if source_account.amount < amount {
                return Err(TokenError::InsufficientFunds.into());
            }

            source_account.amount = source_account.amount.saturating_sub(amount);
            destination_account.amount = destination_account.amount.saturating_add(amount);

            source_account.pack_into_slice(&mut source_info.try_borrow_mut_data()?);
            destination_account.pack_into_slice(&mut destination_info.try_borrow_mut_data()?);
            Ok(())
        }
        TokenInstruction::SetAuthority { new_authority } => {
            if accounts.len() < 2 {
                return Err(TokenError::NotEnoughAccountKeys.into());
            }

            let (target_account_slice, rest) = accounts.split_at_mut(1);
            let current_authority_info = &rest[0];
            let target_account_info = &mut target_account_slice[0];

            // TODO: check is signer matches mint_authority
            if !current_authority_info.is_signer {
                return Err(TokenError::MissingRequiredSignature.into());
            }

            let assign_instruction = system_program::create_assign_instruction(
                target_account_info.pubkey,
                &new_authority,
            )?;

            invoke_context.invoke(
                &assign_instruction,
                std::slice::from_mut(target_account_info),
            )?;
            Ok(())
        }
        TokenInstruction::InitializeMint {
            decimals,
            mint_authority,
        } => {
            if accounts.len() < 2 {
                return Err(TokenError::NotEnoughAccountKeys.into());
            }

            let (mint_slice, rent_slice) = accounts.split_at_mut(1);
            let mint_info = &mut mint_slice[0];
            let rent_info = &rent_slice[0];

            let rent: Rent = bincode::options()
                .with_fixint_encoding()
                .deserialize(&rent_info.try_borrow_data()?)
                .map_err(|_| {
                    ProgramError::InternalError("failed to deserialize rent sysvar".to_string())
                })?;

            let mut mint = Mint::unpack_from_slice(&mint_info.try_borrow_data()?)
                .map_err(|_| ProgramError::InvalidAccountData)?;
            if mint.is_initialized {
                return Err(TokenError::AlreadyInitialized.into());
            }

            if !rent.is_exempt(mint_info.lamports()?, mint_info.try_borrow_data()?.len()) {
                return Err(TokenError::NotRentExempt.into());
            }

            mint.mint_authority = mint_authority;
            mint.decimals = decimals;
            mint.is_initialized = true;
            mint.supply = 0;
            mint.pack_into_slice(&mut mint_info.try_borrow_mut_data()?);
            Ok(())
        }
        TokenInstruction::InitializeAccount => {
            if accounts.len() < 4 {
                return Err(TokenError::NotEnoughAccountKeys.into());
            }

            let (token_account_slice, rest) = accounts.split_at_mut(1);
            let (mint_slice, rest) = rest.split_at_mut(1);
            let (owner_slice, rent_slice) = rest.split_at_mut(1);
            let (token_account_info, mint_info, owner_info, rent_info) = (
                &mut token_account_slice[0],
                &mint_slice[0],
                &owner_slice[0],
                &rent_slice[0],
            );

            let rent: Rent = bincode::options()
                .with_fixint_encoding()
                .deserialize(&rent_info.try_borrow_data()?)
                .map_err(|_| {
                    ProgramError::InternalError("failed to deserialize rent sysvar".to_string())
                })?;

            let mut token_account =
                TokenAccount::unpack_from_slice(&token_account_info.try_borrow_data()?)
                    .map_err(|_| ProgramError::InvalidAccountData)?;
            if token_account.is_initialized {
                return Err(TokenError::AlreadyInitialized.into());
            }

            if !rent.is_exempt(
                token_account_info.lamports()?,
                token_account_info.try_borrow_data()?.len(),
            ) {
                return Err(TokenError::NotRentExempt.into());
            }

            let mint = Mint::unpack_from_slice(&mint_info.try_borrow_data()?)
                .map_err(|_| ProgramError::InvalidAccountData)?;
            if !mint.is_initialized {
                return Err(TokenError::MintNotInitialized.into());
            }

            token_account.mint = mint_info.pubkey;
            token_account.owner = owner_info.pubkey;
            token_account.is_initialized = true;
            token_account.amount = 0;
            token_account.pack_into_slice(&mut token_account_info.try_borrow_mut_data()?);
            Ok(())
        }
    }
}
