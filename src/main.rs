use anyhow::Result;
use crossbeam_channel::unbounded;
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tiny_solana::cost_model::CostModel;
use tiny_solana::native_token::LAMPORTS_PER_SOL;
use tiny_solana::poh::{PohConfig, PohRecorder, PohService};
use tiny_solana::programs::ids::SYSTEM_PROGRAM_ID;
use tiny_solana::types::load_keypair;
use tiny_solana::{
    accounts::{Account, InMemoryAccountStore},
    bank::Bank,
    banking_stage::BankingStage,
    rpc::server::{RpcServerConfig, start_rpc_server},
    types::{Hash, Pubkey},
};
use tracing_subscriber::EnvFilter;

const BATCH_SIZE_LIMIT: usize = 128;
const FAUCET_FILE: &str = "faucet.json";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let faucet_keypair = load_keypair(FAUCET_FILE)?;
    let faucet_pubkey = faucet_keypair.pubkey();
    tracing::info!("loaded faucet pubkey: {}", faucet_pubkey);

    let account_store = Arc::new(InMemoryAccountStore::default());

    let root_bank = Arc::new(Bank::new(account_store));

    init_faucet(&root_bank, &faucet_pubkey);
    let (tx_sender, tx_receiver) = crossbeam_channel::unbounded();
    let cost_model = Arc::new(CostModel::new());

    let poh_config = PohConfig {
        hashes_per_tick: 8,
        ticks_per_slot: 16,
        tick_rate: Duration::from_millis(50),
    };
    let poh_recorder = PohRecorder::new(
        Hash::default(),
        poh_config.ticks_per_slot,
        poh_config.hashes_per_tick,
    );
    let (poh_record_sender, poh_record_receiver) = unbounded();
    let exit = Arc::new(AtomicBool::new(false));

    let banking_stage = BankingStage::new(poh_record_sender, tx_sender.clone());
    let banking_handle = {
        let bank = root_bank.clone();
        let cost_model = cost_model.clone();
        let exit_clone = exit.clone();
        tokio::spawn(async move {
            // TODO: bankforks
            loop {
                if exit_clone.load(Ordering::Relaxed) {
                    break;
                }

                let mut batch = Vec::new();
                while let Ok(tx) = tx_receiver.try_recv() {
                    batch.push(tx);
                    if batch.len() >= BATCH_SIZE_LIMIT {
                        break;
                    }
                }

                if batch.is_empty() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                }

                tracing::info!("processing batch of {} transactions", batch.len());
                match banking_stage.process_batch(&bank, batch, &cost_model).await {
                    Ok(results) => {
                        let errors: Vec<_> = results
                            .iter()
                            .enumerate()
                            .filter_map(|(i, r)| r.as_ref().map(|e| (i, e)))
                            .collect();
                        if !errors.is_empty() {
                            tracing::warn!("transaction processing errors: {:?}", errors);
                        }
                    }
                    Err(e) => {
                        tracing::error!("critical error in batch processing: {:?}", e);
                    }
                }
            }
        })
    };

    let rpc_config = RpcServerConfig {
        addr: SocketAddr::from(([127, 0, 0, 1], 8899)),
    };
    tracing::info!("starting rpc server on {}", rpc_config.addr);
    let rpc_handle = tokio::spawn(start_rpc_server(rpc_config, root_bank, tx_sender));

    let poh_service = PohService::new(poh_recorder, &poh_config, poh_record_receiver, exit.clone());

    tokio::select! {
        res = rpc_handle => {
            tracing::error!("rpc server exited: {:?}", res);
        },
        res = banking_handle => {
            tracing::error!("banking stage exited: {:?}", res);
        },
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("ctrl-c received");
        }
    }

    exit.store(true, Ordering::Relaxed);
    if let Err(e) = poh_service.join() {
        tracing::error!("poh service panicked: {:?}", e);
    }

    Ok(())
}

fn init_faucet(bank: &Bank, faucet_pubkey: &Pubkey) {
    let lamports = 1_000 * LAMPORTS_PER_SOL;
    let faucet_account = Account {
        lamports,
        owner: SYSTEM_PROGRAM_ID,
        executable: false,
        rent_epoch: 0,
        data: vec![],
        pubkey: *faucet_pubkey,
    };
    bank.accounts
        .store(faucet_pubkey, faucet_account)
        .expect("failed to store faucet account");

    tracing::info!(
        "initialized faucet account {} with {} lamports",
        faucet_pubkey,
        lamports
    );
}
