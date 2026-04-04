use crate::{
    bank::Bank,
    rpc::handlers::{RpcState, handle_rpc_request},
    transactions::Transaction,
};
use axum::{Router, routing::post};
use crossbeam_channel::Sender;
use std::{net::SocketAddr, sync::Arc};

pub struct RpcServerConfig {
    pub addr: SocketAddr,
}

pub async fn start_rpc_server(
    config: RpcServerConfig,
    bank: Arc<Bank>,
    tx_sender: Sender<Transaction>,
) -> anyhow::Result<()> {
    let state = Arc::new(RpcState { bank, tx_sender });

    let app = Router::new()
        .route("/", post(handle_rpc_request))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(config.addr).await?;
    tracing::info!("rpc server listening on {}", config.addr);

    axum::serve(listener, app).await?;

    Ok(())
}
