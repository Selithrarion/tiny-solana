use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};
use std::fmt;
use std::fmt::{Debug, Display};
use std::fs;
use std::str::FromStr;
use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum ParsePubkeyError {
    #[error("invalid base58 string")]
    InvalidBase58,
    #[error("invalid length: expected 32, got {0}")]
    InvalidLength(usize),
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pubkey(pub [u8; 32]);

impl Default for Pubkey {
    fn default() -> Self {
        Self([0u8; 32])
    }
}

impl Debug for Pubkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", bs58::encode(self.0).into_string())
    }
}

impl Display for Pubkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", bs58::encode(self.0).into_string())
    }
}

impl FromStr for Pubkey {
    type Err = ParsePubkeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = bs58::decode(s)
            .into_vec()
            .map_err(|_| ParsePubkeyError::InvalidBase58)?;
        if bytes.len() != 32 {
            return Err(ParsePubkeyError::InvalidLength(bytes.len()));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(Pubkey(arr))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Signature(pub [u8; 64]);

impl Default for Signature {
    fn default() -> Self {
        Self([0u8; 64])
    }
}

impl Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Signature({}..{})",
            hex::encode(&self.0[..4]),
            hex::encode(&self.0[60..])
        )
    }
}

impl Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", bs58::encode(self.0).into_string())
    }
}

impl Serialize for Signature {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for Signature {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes: &[u8] = serde_bytes::Deserialize::deserialize(deserializer)?;
        let array: [u8; 64] = bytes
            .try_into()
            .map_err(|_| D::Error::invalid_length(bytes.len(), &"an array of length 64"))?;
        Ok(Signature(array))
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Hash(pub [u8; 32]);

impl Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", bs58::encode(self.0).into_string())
    }
}

impl Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", bs58::encode(self.0).into_string())
    }
}

#[derive(Clone)]
pub struct Keypair {
    signing_key: SigningKey,
}

impl Keypair {
    pub fn new() -> Self {
        use ed25519_dalek::ed25519::signature::rand_core::OsRng;
        let signing_key = SigningKey::generate(&mut OsRng);
        Self { signing_key }
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ed25519_dalek::SignatureError> {
        let secret_bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| ed25519_dalek::SignatureError::new())?;
        let signing_key = SigningKey::from_bytes(&secret_bytes);
        Ok(Self { signing_key })
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }

    pub fn pubkey(&self) -> Pubkey {
        Pubkey(self.signing_key.verifying_key().to_bytes())
    }

    pub fn sign_message(&self, message: &[u8]) -> Signature {
        Signature(self.signing_key.sign(message).to_bytes())
    }
}

impl Default for Keypair {
    fn default() -> Self {
        Self::new()
    }
}

impl Debug for Keypair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Keypair {{ pubkey: {} }}", self.pubkey())
    }
}

pub fn verify_signature(signature: &Signature, message: &[u8], pubkey: &Pubkey) -> bool {
    let verifying_key = match VerifyingKey::from_bytes(&pubkey.0) {
        Ok(key) => key,
        Err(_) => return false,
    };
    let dalek_signature = ed25519_dalek::Signature::from_bytes(&signature.0);
    verifying_key.verify(message, &dalek_signature).is_ok()
}

pub fn load_keypair(path: &str) -> anyhow::Result<Keypair> {
    let json_content =
        fs::read_to_string(path).map_err(|e| anyhow::anyhow!("failed to read {}: {}", path, e))?;

    let bytes: Vec<u8> = serde_json::from_str(&json_content)
        .map_err(|e| anyhow::anyhow!("failed to parse keypair json: {}", e))?;

    Keypair::from_bytes(&bytes).map_err(|e| anyhow::anyhow!("invalid keypair bytes: {:?}", e))
}
