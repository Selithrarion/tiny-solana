use crate::poh::{PohError, PohRecord};
use crate::{transactions::Transaction, types::Hash};
use anyhow::Result;
use crossbeam_channel::Sender;
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum RecorderError {
    #[error("poh error: {0}")]
    Poh(#[from] PohError),
    #[error("failed to send record to poh service")]
    SendError,
}

pub struct TransactionRecorder {}

impl TransactionRecorder {
    pub fn new() -> Self {
        Self {}
    }

    pub fn record_transactions(
        &self,
        poh_record_sender: &Sender<PohRecord>,
        txs: &[(usize, &Transaction)],
    ) -> Result<(), RecorderError> {
        if txs.is_empty() {
            return Ok(());
        }

        let mut hasher = Sha256::new();
        for (_, tx) in txs {
            hasher.update(&tx.signatures[0].0);
        }
        let batch_hash = Hash(hasher.finalize().into());

        let record = PohRecord { batch_hash };
        poh_record_sender
            .send(record)
            .map_err(|_| RecorderError::SendError)
    }
}
