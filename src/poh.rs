use crate::types::Hash;
use crossbeam_channel::Receiver;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    sync::Arc,
    thread::{self, JoinHandle},
    time::Duration,
    time::Instant,
};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoHEntry {
    pub hash: Hash,
    pub num_hashes: u64,
    pub transactions_hash: Hash,
}

#[derive(Debug, Clone)]
pub struct PohRecord {
    pub batch_hash: Hash,
}

#[derive(Debug, Clone, Copy)]
pub struct PohConfig {
    pub hashes_per_tick: u64,
    pub ticks_per_slot: u64,
    pub tick_rate: Duration,
}

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum PohError {
    #[error("max tick height reached for this slot")]
    MaxHeightReached,
}

// TODO: separate poh module
// TODO: must be aware of the current working_bank and slot
// TODO: tick_cache, grace_ticks?

/// Verifiable Delay Function
pub struct PohRecorder {
    current_hash: Hash,
    num_hashes: u64,
    ticks_per_slot: u64,
    hashes_per_tick: u64,
    max_tick_height: u64,
    hashes_since_last_tick: u64,
    tick_count: u64,
    slot: u64,
    // working_bank: Option<WorkingBank>,
    // tick_cache: Vec<(Entry, u64)>,
}

impl PohRecorder {
    pub fn new(initial_hash: Hash, ticks_per_slot: u64, hashes_per_tick: u64) -> Self {
        let max_tick_height = ticks_per_slot; // TODO: max_tick_height = (slot + 1) * ticks_per_slot
        Self {
            current_hash: initial_hash,
            num_hashes: 0,
            ticks_per_slot,
            hashes_per_tick,
            max_tick_height,
            hashes_since_last_tick: 0,
            tick_count: 0,
            slot: 0,
        }
    }

    fn hash(&mut self, num_hashes: u64) {
        for _ in 0..num_hashes {
            self.current_hash = Hash(Sha256::digest(self.current_hash.0).into());
            self.num_hashes += 1;
            self.hashes_since_last_tick += 1;
        }
    }

    pub fn tick(&mut self) {
        let remaining_hashes = self
            .hashes_per_tick
            .saturating_sub(self.hashes_since_last_tick);
        self.hash(remaining_hashes);
        self.hashes_since_last_tick = 0;
        self.tick_count += 1;

        if self.tick_count % self.ticks_per_slot == 0 {
            self.slot += 1;
            self.max_tick_height = (self.slot + 1) * self.ticks_per_slot;
            // TODO: bankforks
            tracing::info!(
                "slot boundary reached, new slot: {}, new max_tick_height: {}",
                self.slot,
                self.max_tick_height
            );
        }
    }

    pub fn should_tick(&self) -> bool {
        self.hashes_since_last_tick >= self.hashes_per_tick
    }

    // TODO: using atomic model instead of slots for simplicity
    pub fn record(&mut self, transactions_hash: Hash) -> Result<PoHEntry, PohError> {
        if self.tick_count >= self.max_tick_height {
            return Err(PohError::MaxHeightReached);
        }

        let mut hasher = Sha256::new();
        hasher.update(&self.current_hash.0);
        hasher.update(&transactions_hash.0);

        self.current_hash = Hash(hasher.finalize().into());
        self.num_hashes += 1;
        self.hashes_since_last_tick += 1;

        Ok(PoHEntry {
            hash: self.current_hash,
            num_hashes: self.num_hashes,
            transactions_hash,
        })
    }
}

pub struct PohService {
    tick_producer: JoinHandle<()>,
}

impl PohService {
    pub fn new(
        mut poh_recorder: PohRecorder,
        config: &PohConfig,
        poh_record_receiver: Receiver<PohRecord>,
        exit: Arc<AtomicBool>,
    ) -> Self {
        let hashes_per_batch = 128;
        let tick_rate_ns = config.tick_rate.as_nanos() as u64;
        let ideal_ns_per_batch = (tick_rate_ns * hashes_per_batch) / config.hashes_per_tick;

        let tick_producer = thread::spawn(move || {
            while !exit.load(Ordering::Relaxed) {
                let start = Instant::now();

                match poh_record_receiver.try_recv() {
                    Ok(record) => {
                        if let Err(e) = poh_recorder.record(record.batch_hash) {
                            tracing::error!("failed to record transaction batch hash: {}", e);
                        }
                        continue;
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => {
                        poh_recorder.hash(hashes_per_batch);
                    }
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        tracing::error!("poh record channel disconnected, exiting poh service");
                        break;
                    }
                }

                if poh_recorder.should_tick() {
                    poh_recorder.tick();
                }

                let elapsed_ns = start.elapsed().as_nanos() as u64;
                match elapsed_ns.cmp(&ideal_ns_per_batch) {
                    std::cmp::Ordering::Less => {
                        let sleep_ns = ideal_ns_per_batch - elapsed_ns;
                        if sleep_ns > 1000 {
                            thread::sleep(Duration::from_nanos(sleep_ns));
                        }
                    }
                    std::cmp::Ordering::Greater => {
                        let lag_ns = elapsed_ns - ideal_ns_per_batch;
                        tracing::warn!(
                            "poh service is lagging by {}ms per batch",
                            lag_ns as f64 / 1_000_000.0
                        );
                    }
                    std::cmp::Ordering::Equal => {}
                }
            }
        });

        Self { tick_producer }
    }

    pub fn join(self) -> thread::Result<()> {
        self.tick_producer.join()
    }
}
