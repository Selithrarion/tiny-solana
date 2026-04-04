use crate::accounts::{AccountStore, OverlayStore};
use crate::cost_model::TransactionCost;
use crate::poh::PohRecord;
use crate::{
    account_locks::{AccountLocks, LockError},
    bank::Bank,
    committer::Committer,
    cost_model::{CostModel, CostTracker, MAX_BLOCK_UNITS, MAX_WRITABLE_ACCOUNT_UNITS},
    scheduler::TransactionScheduler,
    transactions::{Transaction, TransactionError},
    tx_recorder::TransactionRecorder,
};
use anyhow::Result;
use crossbeam_channel::Sender;
use rayon::prelude::*;
use std::sync::Arc;

struct TransactionProcessResult {
    original_index: usize,
    processing_result: Result<OverlayStore<dyn AccountStore>, TransactionError>,
    transaction: Transaction,
    cost: TransactionCost,
}
pub struct BankingStage {
    scheduler: TransactionScheduler,
    account_locks: AccountLocks,
    cost_tracker: CostTracker,
    tx_recorder: TransactionRecorder,
    committer: Committer,
    tx_sender: Sender<Transaction>,
    poh_record_sender: Sender<PohRecord>,
}

impl BankingStage {
    pub fn new(poh_record_sender: Sender<PohRecord>, tx_sender: Sender<Transaction>) -> Self {
        Self {
            scheduler: TransactionScheduler::new(),
            account_locks: AccountLocks::new(),
            cost_tracker: CostTracker::new(MAX_BLOCK_UNITS, MAX_WRITABLE_ACCOUNT_UNITS),
            tx_recorder: TransactionRecorder {},
            committer: Committer {},
            tx_sender,
            poh_record_sender,
        }
    }

    pub async fn process_batch(
        &self,
        bank: &Arc<Bank>,
        transactions: Vec<Transaction>,
        cost_model: &CostModel,
    ) -> Result<Vec<Option<TransactionError>>> {
        tracing::debug!(
            "process_batch started for {} transactions",
            transactions.len()
        );

        let tx_count = transactions.len();
        let mut final_results: Vec<Option<TransactionError>> = vec![None; tx_count];
        let tx_queue = transactions;

        let txs_to_verify = tx_queue.clone();
        let verification_results = tokio::task::spawn_blocking(move || {
            txs_to_verify
                .par_iter()
                .map(|tx| tx.verify_signatures_sync())
                .collect::<Vec<_>>()
        })
        .await?;

        let txs_to_schedule: Vec<(usize, Transaction)> = tx_queue
            .into_iter()
            .enumerate()
            .filter_map(|(i, tx)| {
                let sig_res = &verification_results[i];
                if let Err(e) = sig_res {
                    final_results[i] = Some(e.clone()); // DROPPED
                    None
                } else {
                    Some((i, tx))
                }
            })
            .collect();

        tracing::debug!(
            "{} transactions passed signature verification",
            txs_to_schedule.len()
        );

        let scheduled_groups = self.scheduler.schedule(&txs_to_schedule);
        tracing::debug!("scheduled into {} parallel groups", scheduled_groups.len());

        for group in scheduled_groups {
            // TODO: do we still have group race
            tracing::debug!("processing group with {} transactions", group.len());
            let mut group_to_process_with_cost: Vec<(usize, &Transaction, TransactionCost)> =
                Vec::new();

            for (original_index, tx) in group {
                let cost = cost_model.calculate_cost(tx);
                if self.cost_tracker.would_fit(&cost).is_ok() {
                    self.cost_tracker.add_cost(&cost);
                    group_to_process_with_cost.push((original_index, tx, cost));
                } else {
                    final_results[original_index] =
                        Some(TransactionError::WouldExceedMaxBlockCostLimit);
                }
            }

            if group_to_process_with_cost.is_empty() {
                continue;
            }

            let group_txs_for_locking: Vec<_> = group_to_process_with_cost
                .iter()
                .map(|(idx, tx, _)| (*idx, *tx))
                .collect();

            let lock_guard = match self
                .account_locks
                .try_lock_accounts_for_group(&group_txs_for_locking)
            {
                Ok(guard) => guard,
                Err(LockError::AccountInUse) => {
                    for (original_index, _tx, cost) in &group_to_process_with_cost {
                        self.cost_tracker.subtract_cost(&cost);
                        final_results[*original_index] = Some(TransactionError::AccountInUse);
                        let tx_to_requeue = txs_to_schedule
                            .iter()
                            .find(|(idx, _)| *idx == *original_index)
                            .unwrap()
                            .1
                            .clone();
                        self.requeue_transaction(tx_to_requeue);
                    }
                    tracing::warn!(
                        "failed to lock accounts for group, rolling back costs and re-queueing {} transactions",
                        group_to_process_with_cost.len()
                    );
                    continue;
                }
            };

            let freeze_lock = bank.acquire_freeze_lock();

            let group_results: Vec<TransactionProcessResult> = group_to_process_with_cost
                .par_iter()
                .map(|(original_index, tx, cost)| {
                    let processing_result = bank.process_single_transaction(tx, cost.sum());
                    TransactionProcessResult {
                        original_index: *original_index,
                        processing_result,
                        transaction: (*tx).clone(),
                        cost: cost.clone(),
                    }
                })
                .collect();

            self.tx_recorder
                .record_transactions(&self.poh_record_sender, &group_txs_for_locking)?;

            let mut successful_overlays: Vec<OverlayStore<dyn AccountStore>> = Vec::new();
            for result in group_results {
                let metrics_result = result
                    .processing_result
                    .as_ref()
                    .map(|_| ())
                    .map_err(|e| e.clone());
                tracing::debug!(
                    tx_index = result.original_index,
                    "transaction processing finished with result: {:?}",
                    &metrics_result
                );
                bank.record_transaction_metrics(&metrics_result);

                match result.processing_result {
                    Ok(overlay) => {
                        successful_overlays.push(overlay);
                    }
                    Err(e) => {
                        self.cost_tracker.subtract_cost(&result.cost);
                        if e.is_retryable() {
                            self.requeue_transaction(result.transaction.clone());
                        }
                        final_results[result.original_index] = Some(e);
                    }
                }
            }

            tracing::debug!(
                "committing {} successful transaction(s)",
                successful_overlays.len()
            );
            self.committer.commit_overlays(successful_overlays);

            drop(lock_guard);
            drop(freeze_lock);
        }

        self.cost_tracker.reset();

        Ok(final_results)
    }

    fn requeue_transaction(&self, tx: Transaction) {
        if let Err(e) = self.tx_sender.send(tx) {
            tracing::error!("failed to re-queue transaction: {}", e);
        }
    }
}
