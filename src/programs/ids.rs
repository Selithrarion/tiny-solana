use crate::types::Pubkey;

macro_rules! declare_id {
    ($name:ident, $bs58_str:expr) => {
        pub const $name: Pubkey = Pubkey(const_crypto::bs58::decode_pubkey($bs58_str));
    };
}

declare_id!(SYSTEM_PROGRAM_ID, "11111111111111111111111111111111");
declare_id!(
    TOKEN_PROGRAM_ID,
    "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
);
declare_id!(
    BPF_LOADER_UPGRADEABLE_PROGRAM_ID,
    "BPFLoaderUpgradeab1e111111111111111111111111"
);
