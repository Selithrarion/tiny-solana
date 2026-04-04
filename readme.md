### tiny-solana

solana deep dive (took ~1.5 week)  
validator / consensus client implementation. was studying agave so not from scratch  

thought about implementing both tokio ingress and af_xdp ingress and sending it to our internal channel (ingress layer -> channel layer -> processing layer)  
but we have to copy data - losing af_xdp advantages  
or maybe can use raw pointers but have to implement arena allocator with free_list or smth... so decided to take simpler approach and use tokio without any abstractions  

using parking_lot cuz lazy to handle poison errs but in 2026 there is no huge difference with std rwlock in benches  

block-stm in sui looks fun, wanna study it later  

## features
- creating a tx queue in `main.rs`, handle batches in `BankingStage`
- using bincode with `with_fixint_encoding` to achieve determinism
- an account store trait with memory/overlay(cow)/rocksdb implementations, but no support for blockstore yet (only current accounts state)
- proof of history ticking
- transaction retrying if possible
- a simple cost model
- a runtime with a native loader - system program (`Transfer` `Assign` `CreateAccount`  ), token program (`Mint` `Transfer` `SetAuthority` `InitializeMint` `InitializeAccount`)
- solana-rbpf to create bpf loader (`InitializeBuffer`, `Write`, `DeployWithMaxDataLen`, `Upgrade`, `SetAuthority`, `Close`) but don't have enough registered syscalls yet
- `ChainedProgramExecutor` combining `NativeProgramExecutor` and `RbpfProgramExecutor`
- making own vm is out of scope
- simple rpc with axum
- custom `TrackedRwLock` wrapper 
- parallel transactions executing (`sealevel`) - tx grouping, account locking, bank freeze counter
- blockhash queue
- cpi (Cross-Program Invocation) with `invoke_context`
- tx custom manual serialization and deserialization (bincode and repr c not enough) 

## not implemented
- bank forks (basically a `DashMap<Slot, Arc<Bank>>` but have to refactor)
- only leader-only mode, no validator mode
- metrics
- Program Derived Addresses (PDAs)
- no gossip, no turbine, no quic
- snapshots
- rent collection
- priority fees

## architecture

high-level
```mermaid
---
config:
  theme: redux-color
---
sequenceDiagram
    participant Client
    participant Node (RPC Server)
    participant Transaction Processor
    participant State (Bank/Accounts)
    participant Ledger (PoH/Storage)

    Client->>Node (RPC Server): sendTransaction(tx)
    Note over Node (RPC Server): 1. Basic Validation

    Node (RPC Server)->>Transaction Processor: Enqueue for processing
    Note over Transaction Processor: 2. Scheduling & Runtime Execution

    Transaction Processor->>State (Bank/Accounts): Execute transaction
    Note over State (Bank/Accounts): 3. Update Balances / Modify Data
    State (Bank/Accounts)-->>Transaction Processor: Success / Failure

    alt Execution Successful
        Transaction Processor->>Ledger (PoH/Storage): Record transaction hash
        Note over Ledger (PoH/Storage): 4. Sealing to History (Immutable)
    end

    Node (RPC Server)-->>Client: Signature (Acknowledgement)
```

detailed execution flow
```mermaid
---
config:
  theme: redux-color
---
sequenceDiagram
    participant Client
    participant RPC
    participant TxQueue
    participant BankingStage
    participant Scheduler
    participant CostModel
    participant AccountLocks
    participant Bank
    participant Runtime
    participant TxRecorder
    participant PoH

    Client->>RPC: sendTransaction(tx_bytes)
    RPC-->>Client: Signature (Acknowledgement)
    RPC->>TxQueue: push(tx)

    Note over BankingStage: === BATCH PROCESSING LOOP ===

    BankingStage->>TxQueue: pull_batch()
    BankingStage->>BankingStage: Verify Signatures (parallel, rayon)

    alt Invalid Signature
        BankingStage-->>Client: Dropped (InvalidSignature)
    end

    BankingStage->>Scheduler: schedule(valid_txs)
    Scheduler-->>BankingStage: Vec<Vec<(index, &tx)>> (parallel groups)

    Note over BankingStage: === PER GROUP EXECUTION ===

    loop For each parallel group

        loop For each tx in group
            BankingStage->>CostModel: calculate_cost(tx)
            CostModel-->>BankingStage: cost (u64)
            BankingStage->>CostModel: try_add_cost(cost)

            alt WouldExceedMaxBlockCostLimit
                CostModel-->>BankingStage: Err
                BankingStage->>TxQueue: re-queue(tx)
            end
        end

        BankingStage->>Bank: acquire_freeze_lock()
        Note over Bank: Prevents Bank.freeze() during execution

        BankingStage->>AccountLocks: try_lock_accounts(group)

        alt AccountInUse
            AccountLocks-->>BankingStage: Err
            BankingStage->>TxQueue: re-queue(tx)
        end

        AccountLocks-->>BankingStage: LockGuard

        Note over BankingStage: === PARALLEL EXECUTION (rayon::par_iter) ===

        BankingStage->>Bank: process_single_transaction(tx)
        Bank->>Bank: check_preflight_rules()
        Bank->>Bank: load_and_verify_accounts()
        Bank->>Runtime: execute(instruction)
        Runtime-->>Bank: Ok / ProgramError
        Bank->>Bank: commit_transaction()
        Bank-->>BankingStage: Result

        alt ProgramError
            Bank-->>BankingStage: Dropped
        end

        BankingStage->>TxRecorder: record_transactions(batch_hash)
        TxRecorder->>PoH: record(batch_hash)
        PoH-->>TxRecorder: PoHEntry
        TxRecorder-->>BankingStage: Result

        BankingStage->>Committer: commit_overlays(results)
        Committer->>AccountStore: overlay.flush()
        Committer->>Committer: update_fee_cache()
        Committer->>Committer: finalize_accounts()
        Committer-->>BankingStage: CommitResult

        Note over BankingStage: drop(lock_guard) → AccountLocks::unlock
        Note over BankingStage: drop(freeze_lock) → freeze_lock_count -= 1

        BankingStage->>Bank: record_transaction_metrics(results)
    end
```

## run
`cargo run --bin keygen`  
`$env:RUST_LOG="debug"; cargo run --bin tiny-solana`  
`cargo run --bin client`  

```
faucet: 5nbVsRG8VCpcoaod1nmnyWgV4Bv47RQChLLnFRui62gB
recipient: 7maCHzw4GLxhuBRpNUaYDDYrfELozgmF8tXFRnNSTX7D
latest blockhash: 11111111111111111111111111111111
sending transaction
response: Object {"id": Number(1), "jsonrpc": String("2.0"), "result": String("8uzuqRvkjsnmJoJqQDoSW2oeHDHsFkm4Es11Mou4GZmBAX4Z9akPL8zW5MnLRFhWS3NDk9Cd71B9wUDUQFqtk5N")}
transaction sent, signature: 8uzuqRvkjsnmJoJqQDoSW2oeHDHsFkm4Es11Mou4GZmBAX4Z9akPL8zW5MnLRFhWS3NDk9Cd71B9wUDUQFqtk5N
waiting for confirmation
faucet balance: 999999000000 lamports
recipient balance: 1000000 lamports
```