use anyhow::Result;
use std::fs;
use tiny_solana::types::Keypair;

const FAUCET_FILE: &str = "faucet.json";

fn main() -> Result<()> {
    let keypair = Keypair::new();

    let bytes = keypair.to_bytes();
    let json = serde_json::to_string(&bytes.to_vec())?;
    fs::write(FAUCET_FILE, &json)?;

    println!("faucet keypair generated and saved to {}", FAUCET_FILE);
    println!("pubkey: {}", keypair.pubkey());

    Ok(())
}
