use serde::Deserialize;

#[derive(Deserialize, Debug, Default, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct RpcSendTransactionConfig {
    #[serde(default)]
    pub skip_preflight: bool,
    // TODO: preflight_commitment, encoding, max_retries
}
