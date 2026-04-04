use crate::{transactions::Transaction, types::Pubkey};
use std::collections::BTreeSet;

struct TransactionSets {
    read_set: BTreeSet<Pubkey>,
    write_set: BTreeSet<Pubkey>,
}

pub struct TransactionScheduler {}

impl TransactionScheduler {
    pub fn new() -> Self {
        Self {}
    }

    // TODO: add per-address FIFO queues
    // TODO: different scheduling modes (BlockProduction,  BlockVerification)
    // TODO: integrate with a Cost Model to prioritize transactions
    pub fn schedule<'a>(
        &self,
        txs: &'a Vec<(usize, Transaction)>,
    ) -> Vec<Vec<(usize, &'a Transaction)>> {
        if txs.is_empty() {
            return Vec::new();
        }

        let tx_sets: Vec<TransactionSets> = txs.iter().map(|tx| Self::extract_sets(tx)).collect();

        let mut scheduled_groups = Vec::new();
        let mut remaining_tx_indices: BTreeSet<usize> = (0..txs.len()).collect();

        while !remaining_tx_indices.is_empty() {
            let mut current_group_indices = Vec::new();
            let mut current_group_write_set = BTreeSet::new();

            remaining_tx_indices.retain(|&tx_index| {
                let sets = &tx_sets[tx_index];

                let read_conflict = sets
                    .read_set
                    .iter()
                    .any(|key| current_group_write_set.contains(key));
                let write_conflict = sets
                    .write_set
                    .iter()
                    .any(|key| current_group_write_set.contains(key));

                if !read_conflict && !write_conflict {
                    current_group_indices.push(tx_index);
                    for key in &sets.write_set {
                        current_group_write_set.insert(*key);
                    }
                    false
                } else {
                    true
                }
            });

            if !current_group_indices.is_empty() {
                let group: Vec<(usize, &'a Transaction)> = current_group_indices
                    .iter()
                    .map(|&i| (txs[i].0, &txs[i].1))
                    .collect();
                scheduled_groups.push(group);
            }
        }

        scheduled_groups
    }

    fn extract_sets(tx: &(usize, Transaction)) -> TransactionSets {
        let mut read_set = BTreeSet::new();
        let mut write_set = BTreeSet::new();

        for instruction in &tx.1.message.instructions {
            for account_meta in &instruction.accounts {
                if account_meta.is_writable {
                    write_set.insert(account_meta.pubkey);
                } else {
                    read_set.insert(account_meta.pubkey);
                }
            }
        }
        TransactionSets {
            read_set,
            write_set,
        }
    }
}
