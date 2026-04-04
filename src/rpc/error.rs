use thiserror::Error;

#[derive(Error, Debug)]
pub enum RpcError {
    #[error("invalid params: {0}")]
    InvalidParams(String),
    #[error("internal error: {0}")]
    InternalError(String),
    #[error("transaction simulation failed: {message}")]
    SendTransactionPreflightFailure {
        message: String,
        // TODO: add full RpcSimulateTransactionResult
    },
}
