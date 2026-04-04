use crate::accounts::AccountError;
use crate::runtime::ProgramError;
use crate::types::{Hash, Pubkey, Signature};
use anyhow::{Context, Result};
use bincode::Options;
use ed25519_dalek::{Signature as DalekSignature, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::task;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AccountMeta {
    pub pubkey: Pubkey,
    pub is_signer: bool,
    pub is_writable: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    pub program_id: Pubkey,
    pub accounts: Vec<AccountMeta>,
    pub data: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub account_keys: Vec<Pubkey>,
    pub recent_blockhash: Hash,
    pub instructions: Vec<Instruction>,
}

#[derive(Error, Debug, PartialEq, Eq, Clone)]
pub enum TransactionError {
    #[error("invalid signature")]
    InvalidSignature, // dropped
    #[error("account error: {0:?}")]
    AccountError(#[from] AccountError), // depends on the specific account error
    #[error("program error: {0:?}")]
    ProgramError(#[from] ProgramError), // dropped
    #[error("blockhash not found")]
    BlockhashNotFound, // dropped
    #[error("blockhash has expired")]
    BlockhashExpired, // dropped

    // retryable
    #[error("account is in use")]
    AccountInUse,
    #[error("transaction would exceed max block cost limit")]
    WouldExceedMaxBlockCostLimit,
}

impl TransactionError {
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::AccountInUse | Self::WouldExceedMaxBlockCostLimit
        )
    }
}

impl Message {
    pub fn serialize_for_signing(&self) -> Result<Vec<u8>> {
        bincode::options()
            .with_fixint_encoding()
            .serialize(self)
            .context("failed to serialize message for signing")
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    pub signatures: Vec<Signature>,
    pub message: Message,
}

impl Transaction {
    pub fn verify_signatures_sync(&self) -> Result<(), TransactionError> {
        let message_bytes = self
            .message
            .serialize_for_signing()
            .map_err(|_| TransactionError::InvalidSignature)?;

        let signer_pubkeys = &self.message.account_keys[..self.signatures.len()];

        let all_valid =
            self.signatures
                .iter()
                .zip(signer_pubkeys.iter())
                .all(|(signature, pubkey)| {
                    if let (Ok(dalek_pubkey), dalek_signature) = (
                        VerifyingKey::from_bytes(&pubkey.0),
                        DalekSignature::from_bytes(&signature.0),
                    ) {
                        dalek_pubkey
                            .verify_strict(&message_bytes, &dalek_signature)
                            .is_ok()
                    } else {
                        false
                    }
                });

        if all_valid {
            Ok(())
        } else {
            Err(TransactionError::InvalidSignature)
        }
    }

    pub async fn verify_signatures(&self) -> Result<(), TransactionError> {
        let tx = self.clone();
        task::spawn_blocking(move || tx.verify_signatures_sync())
            .await
            .map_err(|join_error| {
                eprintln!("panic in signature verification task: {:?}", join_error);
                TransactionError::ProgramError(ProgramError::InternalError(
                    "signature verification panicked".to_string(),
                ))
            })?
    }
}
