use anyhow::{Context, Result};
use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use bincode::Options;
use reqwest::Client;
use serde_json::json;
use std::time::Duration;
use tiny_solana::transactions::{AccountMeta, Instruction, Message, Transaction};
use tiny_solana::{
    programs::{ids::SYSTEM_PROGRAM_ID, system_program::SystemInstruction},
    types::{Hash, Keypair, Pubkey, load_keypair},
};

const RPC_URL: &str = "http://127.0.0.1:8899/";

async fn get_latest_blockhash(client: &Client) -> Result<Hash> {
    let response = client
        .post(RPC_URL)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getLatestBlockhash",
        }))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    let blockhash_str = response["result"]["blockhash"]
        .as_str()
        .context("failed to parse blockhash from rpc response")?;

    let bytes = bs58::decode(blockhash_str)
        .into_vec()
        .context("invalid base58 blockhash")?;
    let mut hash_bytes = [0u8; 32];
    hash_bytes.copy_from_slice(&bytes);
    Ok(Hash(hash_bytes))
}

async fn get_balance(client: &Client, pubkey: &Pubkey) -> Result<u64> {
    let response = client
        .post(RPC_URL)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getBalance",
            "params": [pubkey.to_string()]
        }))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    let balance = response["result"]
        .as_u64()
        .context("failed to parse balance from rpc response")?;
    Ok(balance)
}

async fn send_transaction(client: &Client, transaction: &Transaction) -> Result<String> {
    let serialized_tx = bincode::options()
        .with_fixint_encoding()
        .serialize(transaction)
        .context("failed to serialize transaction")?;
    let base64_tx = BASE64_STANDARD.encode(serialized_tx);

    let response = client
        .post(RPC_URL)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendTransaction",
            "params": [base64_tx]
        }))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;
    println!("response: {:?}", response);

    let signature = response["result"]
        .as_str()
        .context("failed to parse signature from rpc response")?
        .to_string();
    Ok(signature)
}

fn create_transfer_transaction(
    from_keypair: &Keypair,
    to_pubkey: &Pubkey,
    lamports: u64,
    recent_blockhash: Hash,
) -> Result<Transaction> {
    let from_pubkey = from_keypair.pubkey();

    let account_keys = vec![from_pubkey, *to_pubkey, SYSTEM_PROGRAM_ID];

    let transfer_data = SystemInstruction::Transfer { lamports };
    let instruction = Instruction {
        program_id: SYSTEM_PROGRAM_ID,
        accounts: vec![
            AccountMeta {
                pubkey: from_pubkey,
                is_signer: true,
                is_writable: true,
            },
            AccountMeta {
                pubkey: *to_pubkey,
                is_signer: false,
                is_writable: true,
            },
        ],
        data: bincode::options()
            .with_fixint_encoding()
            .serialize(&transfer_data)
            .context("failed to serialize transfer instruction data")?,
    };

    let message = Message {
        account_keys,
        recent_blockhash,
        instructions: vec![instruction],
    };

    let message_bytes = message.serialize_for_signing()?;
    let signature = from_keypair.sign_message(&message_bytes);
    let signatures = vec![signature];

    Ok(Transaction {
        signatures,
        message,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let client = Client::new();

    let faucet_path = "faucet.json";
    let faucet = load_keypair(faucet_path).context("load_keypair err")?;
    let recipient = Keypair::new();

    println!("faucet: {}", faucet.pubkey());
    println!("recipient: {}", recipient.pubkey());

    let blockhash = get_latest_blockhash(&client).await?;
    println!("latest blockhash: {}", blockhash);

    let tx = create_transfer_transaction(&faucet, &recipient.pubkey(), 1_000_000, blockhash)?;

    println!("sending transaction");
    let signature = send_transaction(&client, &tx).await?;
    println!("transaction sent, signature: {}", signature);

    println!("waiting for confirmation");
    tokio::time::sleep(Duration::from_secs(2)).await;

    let faucet_balance = get_balance(&client, &faucet.pubkey()).await?;
    println!("faucet balance: {} lamports", faucet_balance);

    let recipient_balance = get_balance(&client, &recipient.pubkey()).await?;
    println!("recipient balance: {} lamports", recipient_balance);

    Ok(())
}
