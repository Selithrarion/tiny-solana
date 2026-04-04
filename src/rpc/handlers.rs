use crate::bank::MAX_BLOCKHASH_QUEUE_DEPTH;
use crate::rpc::config::RpcSendTransactionConfig;
use crate::rpc::error::RpcError;
use crate::{
    accounts::Account,
    bank::Bank,
    transactions::Transaction,
    types::{Hash, Pubkey},
};
use axum::{Json, extract::State};
use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use bincode::Options;
use crossbeam_channel::Sender;
use serde::{Deserialize, Serialize};
use std::{str::FromStr, sync::Arc};

#[derive(Clone)]
pub struct RpcState {
    pub bank: Arc<Bank>,
    pub tx_sender: Sender<Transaction>,
}

#[derive(Deserialize, Debug)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
    pub id: serde_json::Value,
}

#[derive(Serialize, Debug)]
pub struct JsonRpcResponse {
    jsonrpc: String,
    result: serde_json::Value,
    id: serde_json::Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RpcAccount {
    lamports: u64,
    data: String, // base64
    owner: String,
    executable: bool,
    rent_epoch: u64,
}

impl From<Account> for RpcAccount {
    fn from(account: Account) -> Self {
        Self {
            lamports: account.lamports,
            data: BASE64_STANDARD.encode(&account.data),
            owner: account.owner.to_string(),
            executable: account.executable,
            rent_epoch: account.rent_epoch,
        }
    }
}

pub async fn handle_rpc_request(
    State(state): State<Arc<RpcState>>,
    Json(request): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    let result = match request.method.as_str() {
        "sendTransaction" => handle_send_transaction(&state, request.params).await,
        "getSignatureStatuses" => handle_get_signature_statuses(&state, &request.params).await,

        "getBalance" => handle_get_balance(&state, &request.params).await,
        "getAccountInfo" => handle_get_account_info(&state, &request.params).await,
        "getMultipleAccounts" => handle_get_multiple_accounts(&state, &request.params).await,

        "getLatestBlockhash" => handle_get_latest_blockhash(&state).await,
        "getSlot" => handle_get_slot(&state).await,
        "getBlockHeight" => handle_get_block_height(&state).await,

        "getHealth" => handle_get_health().await,
        "getVersion" => handle_get_version().await,
        "getIdentity" => handle_get_identity(&state).await,

        "getMinimumBalanceForRentExemption" => {
            handle_get_minimum_balance_for_rent_exemption(&request.params).await
        }
        "getEpochInfo" => handle_get_epoch_info(&state).await,

        _ => Ok(serde_json::json!({"error": format!("method '{}' not found", request.method)})),
    };

    let response_result = result.unwrap_or_else(
        |e| serde_json::json!({"error": {"code": -32602, "message": e.to_string()}}),
    );

    Json(JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        result: response_result,
        id: request.id,
    })
}

async fn handle_send_transaction(
    state: &Arc<RpcState>,
    params: serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let params_array: Vec<serde_json::Value> = serde_json::from_value(params)
        .map_err(|e| RpcError::InvalidParams(format!("params must be an array: {}", e)))?;
    if params_array.is_empty() {
        return Err(RpcError::InvalidParams("params array is empty".to_string()));
    }

    let tx_data_str: String = serde_json::from_value(params_array[0].clone()).map_err(|e| {
        RpcError::InvalidParams(format!("param 0 (transaction) must be a string: {}", e))
    })?;

    let config: RpcSendTransactionConfig = params_array
        .get(1)
        .map_or(Ok(RpcSendTransactionConfig::default()), |v| {
            serde_json::from_value(v.clone())
        })
        .map_err(|e| RpcError::InvalidParams(format!("param 1 (config) is invalid: {}", e)))?;

    let tx_bytes = BASE64_STANDARD
        .decode(tx_data_str)
        .map_err(|e| RpcError::InvalidParams(format!("invalid base64: {}", e)))?;
    let tx: Transaction = bincode::options()
        .with_fixint_encoding()
        .deserialize(&tx_bytes)
        .map_err(|e| {
            RpcError::InvalidParams(format!("failed to deserialize transaction: {}", e))
        })?;

    tracing::info!(
        "received transaction, signature: {:?}",
        tx.signatures.get(0)
    );

    if !config.skip_preflight {
        // TODO: use a clean bank from bankforks for simulation
        let preflight_bank = &state.bank;
        let simulation_result = preflight_bank.simulate_transaction(&tx);

        if let Err(e) = simulation_result {
            return Err(RpcError::SendTransactionPreflightFailure {
                message: e.to_string(),
            });
        }
    }

    let signature = tx
        .signatures
        .get(0)
        .map(|s| s.to_string())
        .unwrap_or_default();

    state
        .tx_sender
        .send(tx)
        .map_err(|e| RpcError::InternalError(format!("failed to send to banking stage: {}", e)))?;

    Ok(serde_json::json!(signature))
}

async fn handle_get_balance(
    state: &Arc<RpcState>,
    params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    // TODO: implement bankforks
    let pubkey_str = params[0]
        .as_str()
        .ok_or_else(|| RpcError::InvalidParams("param 0 must be a string".to_string()))?;
    let pubkey =
        Pubkey::from_str(pubkey_str).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
    let balance = state
        .bank
        .accounts
        .load(&pubkey)
        .map(|acc| acc.lamports)
        .unwrap_or(0);
    Ok(serde_json::json!(balance))
}

async fn handle_get_account_info(
    state: &Arc<RpcState>,
    params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    // TODO: implement bankforks
    let pubkey_str = params[0]
        .as_str()
        .ok_or_else(|| RpcError::InvalidParams("param 0 must be a string".to_string()))?;
    let pubkey =
        Pubkey::from_str(pubkey_str).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
    let account = state.bank.accounts.load(&pubkey);
    let rpc_account: Option<RpcAccount> = account.map(Into::into);
    Ok(serde_json::to_value(rpc_account).map_err(|e| RpcError::InternalError(e.to_string()))?)
}

async fn handle_get_signature_statuses(
    _state: &Arc<RpcState>,
    _params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    // TODO: implement a transaction status cache
    Ok(serde_json::json!([]))
}

async fn handle_get_multiple_accounts(
    _state: &Arc<RpcState>,
    _params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    // TODO: implement batch account loading
    Ok(serde_json::json!([]))
}

async fn handle_get_latest_blockhash(state: &Arc<RpcState>) -> Result<serde_json::Value, RpcError> {
    // TODO: implement bankforks
    let blockhash_info = state.bank.blockhash_queue.back().cloned();
    let result = match blockhash_info {
        Some(info) => {
            serde_json::json!({ "blockhash": info.hash.to_string(), "lastValidBlockHeight": info.created_at_slot + MAX_BLOCKHASH_QUEUE_DEPTH as u64 })
        }
        None => {
            serde_json::json!({ "blockhash": Hash::default().to_string(), "lastValidBlockHeight": 0 })
        }
    };
    Ok(result)
}

async fn handle_get_slot(state: &Arc<RpcState>) -> Result<serde_json::Value, RpcError> {
    // TODO: implement bankforks
    Ok(serde_json::json!(state.bank.slot))
}

async fn handle_get_block_height(state: &Arc<RpcState>) -> Result<serde_json::Value, RpcError> {
    // TODO: block height
    Ok(serde_json::json!(state.bank.slot))
}

async fn handle_get_health() -> Result<serde_json::Value, RpcError> {
    Ok(serde_json::json!("ok"))
}

async fn handle_get_version() -> Result<serde_json::Value, RpcError> {
    Ok(serde_json::json!({ "tiny-solana-version": "0.1.0" }))
}

async fn handle_get_identity(_state: &Arc<RpcState>) -> Result<serde_json::Value, RpcError> {
    // TODO: load node identity keypair
    Ok(serde_json::json!(Pubkey::default().to_string()))
}

async fn handle_get_minimum_balance_for_rent_exemption(
    _params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    // TODO: implement rent calculation logic
    Ok(serde_json::json!(2_039_280)) // placeholder value from solana docs
}

async fn handle_get_epoch_info(_state: &Arc<RpcState>) -> Result<serde_json::Value, RpcError> {
    // TODO: implement epoch tracking
    Ok(serde_json::json!({ "epoch": 0 }))
}
