use {
    crate::{
        sender::{
            transaction_sender::{JitoSender, SimulationSender, JitoClientError, NativeSender, LeaderTiming},
        },
        analyzer::ranking::ArbitrageRankingTracker, 
        pools::spam_pools::{
            dlmm_pamm::DlmmPammGlobalAccounts, 
            PoolPair
        },
        wallets::{
            auto_lookup_table::{AutoLookupTable, LookupTableChange, get_lookup_table_account}, 
            create_tx_with_address_table_lookup, 
            Wallet,
            TokenAccounts,
            get_pre_transfer_tip_instruction,
            get_fee_instruction_with_profit,
        },
        tokens::pools_manager::RecentBlockhash
    }, 
    std::time::{
        Instant,
    },
    rand::{
        thread_rng, Rng
    }, solana_client::nonblocking::rpc_client::RpcClient as AsyncRpcClient, 
    solana_sdk::{
        address_lookup_table::{
            state::AddressLookupTable, 
            AddressLookupTableAccount
        },
        transaction::VersionedTransaction,
        hash::Hash as BlockHash,
        commitment_config::CommitmentConfig,
        instruction::{AccountMeta, Instruction}, 
        pubkey::Pubkey, 
        signature::Signer,
        signer::keypair::Keypair,
        compute_budget::ComputeBudgetInstruction
    }, 
    solana_rpc_client_api::config::RpcTransactionConfig,
    spl_associated_token_account::instruction::create_associated_token_account, std::{
        collections::{
            HashMap, HashSet
        },
        error::Error,
        sync::{
            atomic::{AtomicU64, Ordering}, Arc, RwLock
        },
        time::Duration,
    }, 
    tokio::{
        fs, sync::{
            RwLock as TokioRwLock,
            mpsc,
        }, 
        time::sleep, 
        task::JoinHandle as TokioJoinHandle,
        runtime::Handle,
    }
};

pub const MAX_SPAM_ACCOUNTS: usize = 4;
pub const MIN_DUMMY_FEE_AMOUNT: u64 = 30_000;
pub const MAX_DUMMY_FEE_AMOUNT: u64 = 75_000;
pub const FIXED_JITO_TIP_AMOUNT: u64 = 700_000;
pub const CURRENT_ALT_PATH: &str = "/home/thsmg/solana-arb-client/arbitrage/lookup_table.txt";
pub const HELIUS_RPC_URL: &str = "https://mainnet.helius-rpc.com/?api-key=";


pub const EXTEND_ATL_BASE_BUDGET: u32 = 15_000;

pub const CREATE_ATL_BUDGET: u32 = 15_000;
pub const CREATE_ATA_BUDGET: u32 = 30_000;
pub const JITO_TIP_AMOUNT: u64 = 70_000;
pub const NATIVE_SPAM_FEE: u64 = 15_000;

pub const SPAM_ARB_BUDGET: u32 = 175_000;


pub const SPAM_ARBITRAGE_PROGRAM: Pubkey =
        Pubkey::from_str_const("EGGUbFQc5se1CJSsjMm5B6v81ETQpDnZGFzv4JUQJaXd");

pub const TOKEN_PROGRAM_ID: Pubkey =
        Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

#[derive(Debug)]
pub struct SpamSetup{
    pub auto_lookup_table: Arc<TokioRwLock<AutoLookupTable>>,
    pub pool_pairs: Vec<PoolPair>,
}

pub struct SpamService{
    search_spam_hdl: TokioJoinHandle<()>,
    send_spam_hdl: TokioJoinHandle<()>,
}

impl SpamService{
    pub fn new(
        ranking_tracker: Arc<RwLock<ArbitrageRankingTracker>>,
        main_wallet: Arc<Wallet>,
        tip_wallet: Arc<Wallet>,
        highest_slot: Arc<AtomicU64>,
        recent_blockhash: Arc<RwLock<RecentBlockhash>>,
        jito_sender: Arc<JitoSender>,
        native_sender: Arc<NativeSender>,
        runtime_handle: Handle,
        farm_token: String,
        mut receiver_channel: mpsc::Receiver<TokenAccounts>,
    ) -> Result<Self, Box<dyn Error>> {
        let (spam_setup_sender, spam_setup_receiver) = mpsc::channel(5);
        let main_wallet_clone = main_wallet.clone();
        let highest_slot_clone = highest_slot.clone();
        let recent_blockhash_clone = recent_blockhash.clone();
        let jito_sender_clone = jito_sender.clone();
        let farm_token_clone = farm_token.clone();

        let token_accounts = Arc::new(TokioRwLock::new(
            TokenAccounts {
                token_accounts: HashMap::new(),
            }
        ));

        let token_accounts_clone = token_accounts.clone();

        let search_spam_hdl = runtime_handle.spawn(async move {
            start_spam_loop(
                ranking_tracker.clone(),
                main_wallet_clone.clone(),
                highest_slot_clone.clone(),
                recent_blockhash_clone.clone(),
                jito_sender_clone.clone(),
                spam_setup_sender.clone(),
                farm_token_clone.clone(),
                token_accounts_clone,
                receiver_channel
            ).await
        });
        let send_spam_hdl = runtime_handle.spawn(async move {
            start_spam_sending_loop(
                main_wallet,
                tip_wallet,
                highest_slot,
                recent_blockhash,
                jito_sender,
                native_sender,
                spam_setup_receiver,
                farm_token,
                token_accounts,
            ).await
        });
        Ok(SpamService {
            search_spam_hdl,
            send_spam_hdl,
        })
    }
}

pub async fn start_spam_loop(
    ranking_tracker: Arc<RwLock<ArbitrageRankingTracker>>,
    main_wallet: Arc<Wallet>,
    highest_slot: Arc<AtomicU64>,
    recent_blockhash: Arc<RwLock<RecentBlockhash>>,
    jito_sender: Arc<JitoSender>,
    spam_setup_sender: mpsc::Sender<SpamSetup>,
    farm_token: String,
    shared_token_accounts: Arc<TokioRwLock<TokenAccounts>>,
    mut receiver_channel: mpsc::Receiver<TokenAccounts>,
) {
    let warm_up_time = Duration::from_secs(15000000);

    sleep(warm_up_time).await;
    println!("Ranking Warmup Complete");

    let confirmed_client = 
        Arc::new(AsyncRpcClient::new_with_commitment(HELIUS_RPC_URL.to_string(), CommitmentConfig::confirmed()));

    let raw_account_string = fs::read_to_string(CURRENT_ALT_PATH).await.unwrap();
    let mut address_lookup_table_key = Pubkey::from_str_const(&raw_account_string.trim());

    let address_lookup_table_account =
        get_lookup_table_account(confirmed_client.clone(), address_lookup_table_key).await;

    let auto_lookup_table =
        Arc::new(TokioRwLock::new(AutoLookupTable::new(address_lookup_table_account)));
    
    let spam_sender_ref = &spam_setup_sender;

    let token_accounts = receiver_channel.recv().await.unwrap();
    shared_token_accounts.write().await.token_accounts = token_accounts.token_accounts;

    loop {
        if let Ok(new_token_accounts) = receiver_channel.try_recv() {
            shared_token_accounts.write().await.token_accounts = new_token_accounts.token_accounts;
        }

        let top_pairs = {
            let ranking_tracker_r = ranking_tracker.read().unwrap();
            let pairs = ranking_tracker_r.get_top_pairs(MAX_SPAM_ACCOUNTS);
            drop(ranking_tracker_r);
            pairs
        };
        println!("Top 10 Pairs: {:?}", top_pairs);

        let current_slot = highest_slot.load(Ordering::Relaxed);
        let blockhash = {
            let recent_blockhash_r = recent_blockhash.read().unwrap();
            recent_blockhash_r.confirmed.unwrap()
        };

        

        let mut unique_token_accounts = HashMap::new();
        let mut unique_addresses = HashSet::new();

        for pool_pair in &top_pairs {
            let (accounts_meta, _inverse_dlmm) =
                pool_pair.get_dlmm_pamm_spam_arb_ix_data(&shared_token_accounts, true).await;
            let mint = accounts_meta[0].pubkey.to_string();
            let is_created = shared_token_accounts.read().await.get_token_account_and_state(&mint).unwrap().1;
            if !is_created {
                unique_token_accounts.insert(mint, accounts_meta[2].pubkey);
            }

            for account in accounts_meta {
                unique_addresses.insert(account.pubkey);
            }
        }
        

        let lookup_table_change;
        let alt_instructions;
        let pubkeys;
        {
            let auto_lookup_table_r = auto_lookup_table.read().await;
            let (change, instr, pubs, lookup_table_address) = auto_lookup_table_r.extend_or_create_ix(unique_addresses, current_slot, &main_wallet.keypair);
            lookup_table_change = change;
            alt_instructions = instr;
            pubkeys = pubs;
            address_lookup_table_key = lookup_table_address;
        }
            
        let mut create_tokens_ixs = Vec::new();
        for (mint, _user_ata) in &unique_token_accounts {
            let create_tokens_ix = create_associated_token_account(
                &main_wallet.keypair.pubkey(),
                &main_wallet.keypair.pubkey(),
                &Pubkey::from_str_const(mint),
                &TOKEN_PROGRAM_ID,
            );
            create_tokens_ixs.push(create_tokens_ix);
        };

        let transactions = create_txs_with_alt_change(
            lookup_table_change.clone(),
            create_tokens_ixs,
            alt_instructions,
            &main_wallet.keypair,
            current_slot,
            blockhash,
        );

        let mut tx_config = RpcTransactionConfig::default();
        tx_config.commitment = Some(CommitmentConfig::confirmed());
        tx_config.max_supported_transaction_version = Some(0);

        let mut success: bool = true;
        if transactions.len() > 0 {
            let signature = transactions[0].signatures[0];
            println!("sig : {}", signature);
            loop {
                let result = jito_sender.send_jito_bundle(transactions.clone()).await;
                if result.is_ok() {
                    let start_time = Instant::now();
                    loop {
                        let confirmed = 
                            confirmed_client.get_transaction_with_config(&signature, tx_config.clone()).await;
                        if confirmed.is_ok() {
                            success = true;
                            break;
                        }
                        if start_time.elapsed() > Duration::from_secs(60) {
                            success = false;
                            break;
                        }
                        sleep(Duration::from_secs(1)).await;
                        
                    }

                    if success {
                        break;
                    } else {
                        sleep(Duration::from_millis(50)).await;
                    }
                }
                
            }
        }
        if success {
            println!("Success ALT txs confirmed");
            shared_token_accounts.read().await.set_true_token_accounts_state(&unique_token_accounts.keys().cloned().collect());
            let alt_updated = 
                auto_lookup_table.write().await.update_and_check_lookup_table(
                    confirmed_client.clone(), 
                    pubkeys, 
                    address_lookup_table_key
                ).await;

            if !alt_updated {
                println!("Failed ALT update");
            }
            println!("Success ALT update");
            if lookup_table_change == LookupTableChange::Create {
                fs::write(CURRENT_ALT_PATH, address_lookup_table_key.to_string()).await.unwrap();
            }

            let spam_setup = SpamSetup {
                auto_lookup_table: auto_lookup_table.clone(),
                pool_pairs: top_pairs,
            };
            println!("spam setup : {:?}", spam_setup);

            let _ = spam_sender_ref.send(spam_setup).await;


        } else {
            println!("Failed ALT txs");
        }
        
        sleep(Duration::from_secs(300)).await;
            
    }
}

pub async fn start_spam_sending_loop(
    main_wallet: Arc<Wallet>,
    tip_wallet: Arc<Wallet>,
    highest_slot: Arc<AtomicU64>,
    recent_blockhash: Arc<RwLock<RecentBlockhash>>,
    jito_sender: Arc<JitoSender>,
    native_sender: Arc<NativeSender>,
    mut spam_setup_receiver: mpsc::Receiver<SpamSetup>,
    farm_token: String,
    shared_token_accounts: Arc<TokioRwLock<TokenAccounts>>,
) {
    // Wait for the first spam setup before starting the loop
    println!("Waiting for initial spam setup...");
    let mut current_setup = match spam_setup_receiver.recv().await {
        Some(setup) => {
            println!("Received initial spam setup with {} pool pairs", setup.pool_pairs.len());
            setup
        },
        None => {
            println!("Spam setup channel closed before receiving initial setup");
            return;
        }
    };
    
    
    let farm_token_account = shared_token_accounts.read().await.get_token_account_and_state(&farm_token).unwrap().0;
    
    

    let dlmm_pamm_global_accounts = DlmmPammGlobalAccounts::new(
        AccountMeta::new(main_wallet.keypair.pubkey(), true),
        AccountMeta::new(Pubkey::from_str_const(&farm_token_account), false),
        AccountMeta::new_readonly(Pubkey::from_str_const(&farm_token), false),
    );
    let mut accounts_meta = Vec::with_capacity(12*MAX_SPAM_ACCOUNTS + 13);
    accounts_meta.extend(dlmm_pamm_global_accounts.get_accounts_meta());

    let simulation_sender = SimulationSender::new();

    let auto_lookup_table_account = current_setup.auto_lookup_table.read().await.lookup_table[0].clone();
    let general_lookup_table_account = main_wallet.address_lookup_table_account[0].clone();

    let mut accounts_lookup_table_accounts = [
        general_lookup_table_account,
        auto_lookup_table_account,
    ];

    println!("Starting spam sending loop");
    loop {
        // Check if there's a new setup available
        current_setup = match spam_setup_receiver.try_recv() {
            Ok(new_setup) => {
                println!("Received new spam setup with {} pool pairs", new_setup.pool_pairs.len());
                let new_setup_accounts_lookup_table_accounts = new_setup.auto_lookup_table.read().await.lookup_table[0].clone();
                accounts_lookup_table_accounts[1] = new_setup_accounts_lookup_table_accounts;
                new_setup
            },
            Err(e) => {current_setup}
        };

        let current_slot = highest_slot.load(Ordering::Relaxed);
        let blockhash = recent_blockhash.read().unwrap().confirmed.unwrap();

        let mut instruction_data = Vec::with_capacity(1 + 8 + 1 + MAX_SPAM_ACCOUNTS);
        let dummy_fee_amount = chose_dummy_fee_amount(MIN_DUMMY_FEE_AMOUNT, MAX_DUMMY_FEE_AMOUNT);
        //let fee_amount = chose_dummy_fee_amount(NATIVE_SPAM_FEE, NATIVE_SPAM_FEE + 1_000);
        let jito_tip_amount = chose_dummy_fee_amount(FIXED_JITO_TIP_AMOUNT, FIXED_JITO_TIP_AMOUNT + 300_000);
        let total_fee = dummy_fee_amount + jito_tip_amount;

        instruction_data.push(current_setup.pool_pairs.len() as u8);
        instruction_data.extend_from_slice(&total_fee.to_le_bytes());
        
        instruction_data.push(0_u8); // dlmm pamm id 

        let mut current_account_metas = accounts_meta.clone();

        for pool_pair in &current_setup.pool_pairs {
            let (new_accounts_meta, inverse_dlmm) = pool_pair.get_dlmm_pamm_spam_arb_ix_data(
                &shared_token_accounts, 
                false
            ).await;
            current_account_metas.extend(new_accounts_meta);
            instruction_data.push(inverse_dlmm as u8);    
        }
        let mut ixs = Vec::new();
        let budget = SPAM_ARB_BUDGET;
        ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(budget));
        ixs.push(get_fee_instruction_with_profit(dummy_fee_amount, budget));

        ixs.push(Instruction {
            program_id: SPAM_ARBITRAGE_PROGRAM,
            accounts: current_account_metas,
            data: instruction_data,
        });
        //let tip_tx = tip_wallet.create_and_sign_jito_tip_tx(JITO_TIP_AMOUNT, (current_slot % 8) as u8, blockhash).unwrap();

        // Then use it without the guard
        let tx = main_wallet.sign_tx_with_alt(
            &ixs, 
            &accounts_lookup_table_accounts,
            blockhash
        ).unwrap();

        let start_time = Instant::now();
        let is_simulation = false;
        if !is_simulation {
            //let _ = native_sender.send_native_transaction(&tx, &LeaderTiming::Current, current_slot).await;
            let _ = jito_sender.send_duplicate_jito_tx(
                tx, 
                jito_tip_amount, 
                (current_slot % 8) as u8, 
                tip_wallet.clone(), 
                blockhash
            ).await;
        } else {
            let _ = tokio::time::timeout(
                Duration::from_millis(1_000),
                simulation_sender.send_simulation_transaction(&tx)
            ).await;
        }
        // Calculate elapsed time and sleep for the remainder if needed
        let elapsed = start_time.elapsed();
        if elapsed < Duration::from_millis(200) {
            let remaining = Duration::from_millis(200) - elapsed;
            sleep(remaining).await;
        } else {
            sleep(Duration::from_millis(200)).await;
        }
    }
}


fn create_txs_with_alt_change(
    lookup_table_change: LookupTableChange,
    create_tokens_ixs: Vec<Instruction>,
    alt_instructions: Vec<Instruction>,
    main_wallet: &Keypair,
    current_slot: u64,
    blockhash: BlockHash,
) -> Vec<VersionedTransaction> {
    let mut transactions = Vec::new();
    let jito_tip_ix = 
            get_pre_transfer_tip_instruction(&main_wallet, JITO_TIP_AMOUNT, (current_slot % 8) as u8);

    let atl_budget = match lookup_table_change {
        LookupTableChange::Create => CREATE_ATL_BUDGET,
        LookupTableChange::Extend => EXTEND_ATL_BASE_BUDGET,
        LookupTableChange::NoChange => 0,
    };
    
    if lookup_table_change != LookupTableChange::NoChange {
        let budget = CREATE_ATA_BUDGET*create_tokens_ixs.len() as u32 + atl_budget;
        let create_compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(budget);
        let mut ixs = create_tokens_ixs;
        ixs.push(jito_tip_ix);
        ixs.push(alt_instructions[0].clone());
        ixs.push(create_compute_budget_ix);
        
        transactions.push(
            create_tx_with_address_table_lookup(
                &ixs,
                &[], 
                &main_wallet, 
                &main_wallet,
                blockhash
            ).unwrap()
        );
        for ix in alt_instructions.iter().skip(1) {
            let compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(EXTEND_ATL_BASE_BUDGET);
            let ixs = vec![ix.clone(), compute_budget_ix];
            transactions.push(
                create_tx_with_address_table_lookup(
                    &ixs,
                    &[], 
                    &main_wallet, 
                    &main_wallet,
                    blockhash
                ).unwrap()
            );
        }
    } else {
        if create_tokens_ixs.len() > 0 {
            let budget = CREATE_ATA_BUDGET*create_tokens_ixs.len() as u32;
            let create_compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(budget);
            let mut ixs = create_tokens_ixs;
            ixs.push(jito_tip_ix);
            ixs.push(create_compute_budget_ix);
            transactions.push(
                create_tx_with_address_table_lookup(
                    &ixs,
                    &[], 
                    &main_wallet, 
                    &main_wallet, 
                    blockhash
                ).unwrap()
            );
        }
    }
    transactions
    
}

pub fn chose_dummy_fee_amount(min_fee: u64, max_fee: u64) -> u64 {
    let mut rng = thread_rng();
    rng.gen_range(min_fee..max_fee)
}

#[cfg(test)]
mod tests {
    use solana_client::nonblocking::rpc_client::RpcClient as AsyncRpcClient;
    use solana_client::rpc_config::RpcTransactionConfig;
    use solana_sdk::commitment_config::CommitmentConfig;
    use solana_sdk::signature::Signature;
    use std::str::FromStr;
    use crate::spam::spammer::HELIUS_RPC_URL;
    #[tokio::test]
    async fn test_get_transaction_with_config() {
        // Create an RPC client connected to a public Solana endpoint
        let rpc_client = AsyncRpcClient::new(HELIUS_RPC_URL.to_string());
        
        // Create the signature from the provided string
        let signature = Signature::from_str(
            "62LjxEJW8GjxPjUMdpfmfjgw727Euzc6eut1YqVCyHnHXW4wrnwShsjJmccX7ykodZ5co9yq5AgQqEsJdFn6EVYR"
        ).expect("Failed to parse signature");
        
        // Set up the transaction config with confirmed commitment
        let mut tx_config = RpcTransactionConfig::default();
        tx_config.commitment = Some(CommitmentConfig::confirmed());
        tx_config.max_supported_transaction_version = Some(0);
        
        // Try to get the transaction
        match rpc_client.get_transaction_with_config(&signature, tx_config).await {
            Ok(transaction) => {
                println!("Successfully retrieved transaction: {:?}", transaction);
            },
            Err(err) => {
                println!("Failed to retrieve transaction: {}", err);
            }
        }
    }
}
