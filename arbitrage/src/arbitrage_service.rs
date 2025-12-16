use crossbeam_channel::unbounded;

use crate::connexion_routine_service::ConnexionRoutineService;
use crate::tokens::pools_manager::PoolsManager;
use crate::tokens::pools_searcher::FarmableTokens;
use {
    std::future::Future,
    std::pin::Pin,
    bimap::BiMap,
    crate::simulation::arbitrage_simulator::{
        ArbitrageSimulatorService,
        PoolNotification
    },
    crate::connexion_routine_service::{
        ConnectionHandler, ConnexionHandlerWrapper, JitoLeaderCache,
    },
    crate::arbitrage_client::{CustomLeaderScheduleCache, CustomTpuInfo},
    crate::sender::transaction_sender::TransactionSender,
    crate::analyzer::Analyzer,
    crate::wallets::TokenAccounts,
    crate::analyzer::ranking::ArbitrageRankingTracker,
    crate::arbitrage_client::ArbitrageClient,
    crate::tokens::pools_manager::ConnectionManager,
    crate::tokens::pools_manager::CustomBlockHashQueue,
    crate::arbitrage_client::TrackedPubkeys,
    crate::wallets::wallets_pool::WalletsPool,
    crate::wallets::wallet::Wallet,
    crate::spam::spammer::SpamService,
    crate::sender::decision_maker::{
        LeaderBlacklist,
        DecisionMaker
    },
    base64::{engine::general_purpose, Engine as _},
    solana_ledger::leader_schedule_cache::LeaderScheduleCache,
    bincode::serialize,
    crossbeam_channel::{Receiver, RecvTimeoutError, Sender},
    itertools::izip,
    tracing::{info,debug, error},
    serde_json::json,
    solana_client::{
        nonblocking::rpc_client::RpcClient as AsyncRpcClient,
        rpc_client::RpcClient,
    },
    solana_ledger::{
        blockstore::BlockstoreError,
        blockstore_processor::{TransactionStatusBatch, TransactionStatusMessage},
    },
    solana_rpc::rpc::JsonRpcRequestProcessor,
    solana_rpc_client_api::config::*,
    solana_sdk::{
        clock::Slot,
        commitment_config::CommitmentConfig,
        account::AccountSharedData,
        signer::keypair::Keypair,
    },
    
    solana_runtime::bank_forks::BankForks,
    solana_send_transaction_service::tpu_info::TpuInfo,
    solana_transaction_status_client_types::UiTransactionEncoding,
    std::collections::HashSet,
    std::time::Duration,
    std::time::Instant,
    std::{
        collections::HashMap,
        error::Error,
        result::Result,
        sync::{
            atomic::{AtomicBool, AtomicU64, Ordering},
            Arc, RwLock,
        },
        thread::{Builder, JoinHandle},
    },
    tokio::runtime::Handle,
    tokio::runtime::Runtime,
    tokio::sync::broadcast,
    tokio::sync::RwLock as TokioRwLock,
    tokio::sync::{mpsc, Mutex},
    tokio::task,
    tokio::task::JoinHandle as TokioJoinHandle,
    tokio::time::interval,
    tokio::time::{self},
};

/*pub const HELIUS_MAINNET_URL: &str =
    "https://solana-mainnet.core.chainstack.com";*/
pub const HELIUS_MAINNET_URL: &str = "https://mainnet.helius-rpc.com/?api-key=";
pub const TESTNET_URL: &str = "https://api.testnet.solana.com";

pub const REFRESH_PAIR_INTERVAL: u64 = 30;

#[derive(Debug, Clone)]
struct TrackedPair {
    tx_hashes: Vec<(String, Instant)>,
    jito_tx_hashes: Vec<(String, Instant)>,
    should_discard: bool,
}


pub struct ArbitrageService {
    //arbitrage_slot_status_observer: ArbitrageSlotStatusObserver,
    arbitrage_simulator_service: ArbitrageSimulatorService,
    //transaction_sender: TransactionSender,
    arbitrage_client: ArbitrageClient,
    //transaction_tracker_service: TransactionTrackerService,
    //arbitrage_transaction_status_service: ArbitrageTransactionStatusService,
    connexion_routine_service: ConnexionRoutineService,
    periodic_tasks: Vec<TokioJoinHandle<()>>,
    runtime: Arc<Runtime>,
    spam_service: SpamService,
}

impl ArbitrageService {
    pub fn new(
        exit: Arc<AtomicBool>,
    ) -> Result<Self, Box<dyn std::error::Error>> {

        let (tracked_pubkeys_sender, tracked_pubkeys_receiver) = unbounded();
        let (transaction_status_sender, transaction_status_receiver) = unbounded();
        let (pool_notifications_sender, pool_notifications_receiver) = unbounded();
        let (clear_pubkeys_sender, clear_pubkeys_receiver) = unbounded();
        let (update_pubkeys_sender, update_pubkeys_receiver) = unbounded();
        let (initialize_sender, initialize_receiver) = unbounded();
        let (fee_tracker_sender, fee_tracker_receiver) = unbounded();


        let farmable_tokens = FarmableTokens::new(
            vec![
                "So11111111111111111111111111111111111111112".to_string(),
            ]
        );

        
        let wallets_pool = WalletsPool::new_from_private_keys(
            vec![
                "".to_string(),
            ],
            &farmable_tokens.tokens
        );
        let tip_wallet = Arc::new(Wallet::new_empty(
            "".to_string(),
        ));

        let pools_manager = PoolsManager::new(
            clear_pubkeys_receiver.clone(),
            tracked_pubkeys_sender.clone(),
            update_pubkeys_receiver,
            initialize_receiver,
            farmable_tokens.clone(),
        );

        let highest_slot = Arc::new(AtomicU64::new(0));

        
        
        let tracked_pubkeys_sender = Arc::new(tracked_pubkeys_sender);

        let jito_leader_cache = JitoLeaderCache {
            leaders: Arc::new(TokioRwLock::new(HashSet::new())),
        };



        let connexion = RpcClient::new_with_commitment("http://0.0.0.0:8899", CommitmentConfig::processed());

        let custom_leader_schedule_cache = Arc::new(
            RwLock::new(
                CustomLeaderScheduleCache::new(
                    &connexion,
                    100,
                ).unwrap()
            )
        );

        let custom_cluster_tpu_info = Arc::new(RwLock::new(CustomTpuInfo::new(&connexion)));

        let connexion_routine_service = ConnexionRoutineService::new(
            highest_slot.clone(),
            exit.clone(),
            custom_leader_schedule_cache.clone(),
            custom_cluster_tpu_info.clone(),
        )?;
        debug!("Created connexion_routine_service");

        // Create a new runtime
        let rt: Runtime = tokio::runtime::Builder::new_multi_thread()
            .thread_name("ArbitrageService".to_string())
            .enable_all()
            .build()?;

        let runtime = Arc::new(rt);

        // Let's add some debug logs to see where exactly we're panicking
        debug!("Created runtime");

        let runtime_handle = runtime.handle();

        let _guard = runtime.enter();


        let arbitrage_client = ArbitrageClient::new(
            transaction_status_sender,
            highest_slot.clone(),
            pool_notifications_sender,
            tracked_pubkeys_receiver,
            runtime.clone(),
            exit.clone(),
        )?;

        let analyzer = Arc::new(Analyzer::new(runtime_handle.clone()));
        let connexion_manager = pools_manager.connection_manager.clone();
        let recent_blockhash= connexion_manager.recent_blockhash.clone();
        let transaction_sender = Arc::new(TransactionSender::new(
            connexion_routine_service.leader_connexion_cache.clone(),
            runtime_handle.clone(),
            analyzer.clone(),
            fee_tracker_sender,
        ));
        debug!("Created transaction_sender");


        let is_jito_slots_map = Arc::new(RwLock::new(Vec::with_capacity(100)));
        let custom_block_hash_queue = CustomBlockHashQueue::new();

        let blacklist = Arc::new(RwLock::new(LeaderBlacklist::new()));
        let decision_maker = DecisionMaker::new(blacklist.clone());

        let ranking_tracker = Arc::new(RwLock::new(ArbitrageRankingTracker::new(750)));
        let (token_accounts_sender, token_accounts_receiver) = mpsc::channel(1);
        let spam_service = SpamService::new(
            ranking_tracker.clone(),
            wallets_pool.get_wallet(0).clone(),
            tip_wallet.clone(),
            highest_slot.clone(),
            recent_blockhash.clone(),
            transaction_sender.jito_sender.clone(),
            transaction_sender.native_sender.clone(),
            runtime_handle.clone(),
            farmable_tokens.tokens[0].clone(),
            token_accounts_receiver,
        )?;

        let arbitrage_simulator_service = ArbitrageSimulatorService::new(
            transaction_sender,
            pool_notifications_receiver,
            transaction_status_receiver,
            pools_manager.clone(),
            is_jito_slots_map.clone(),
            custom_block_hash_queue,
            custom_leader_schedule_cache.clone(),
            connexion_routine_service.leader_connexion_cache.clone(),
            highest_slot.clone(),
            runtime_handle.clone(),
            analyzer,
            fee_tracker_receiver,
            clear_pubkeys_sender,
            update_pubkeys_sender,
            initialize_sender,
            wallets_pool.clone(),
            decision_maker,
            ranking_tracker,
            tip_wallet, 
            farmable_tokens.clone(),
            exit.clone(),
        )?;
        debug!("Created arbitrage_simulator_service");


        let periodic_tasks = vec![
            Self::cluster_tpu_info_routine_task(
                connexion_manager,
                custom_cluster_tpu_info.clone(),
                jito_leader_cache.clone(),
                custom_leader_schedule_cache.clone(),
                is_jito_slots_map.clone(),
                highest_slot,
                exit,
                500,
            ),
            Self::refresh_jito_leader_cache_routine_task(
                jito_leader_cache,
            ),
            Self::refresh_arbitrage_pairs_routine_task(
                pools_manager,
                tracked_pubkeys_sender.clone(),
                Arc::new(wallets_pool),
                farmable_tokens,
                token_accounts_sender,
            )
        ];
        debug!("ArbitrageService has started");

        Ok(ArbitrageService {
            //arbitrage_slot_status_observer,
            arbitrage_simulator_service,
            //transaction_sender,
            arbitrage_client,
            //transaction_tracker_service,
            //arbitrage_transaction_status_service,
            connexion_routine_service,
            periodic_tasks,
            spam_service,
            runtime,
        })
    }

    fn cluster_tpu_info_routine_task(
        connexion_manager: ConnectionManager,
        cluster_tpu_info: Arc<RwLock<CustomTpuInfo>>,
        jito_leader_cache: JitoLeaderCache,
        leader_schedule_cache: Arc<RwLock<CustomLeaderScheduleCache>>,
        is_jito_slots_map: Arc<RwLock<Vec<(u64, bool)>>>,
        highest_slot: Arc<AtomicU64>,
        exit: Arc<AtomicBool>,
        interval_ms: u64,
    ) -> TokioJoinHandle<()> {
        debug!("Starting cluster tpu info routine task");
        let leader_schedule_cache_clone = leader_schedule_cache.clone();
        tokio::spawn(async move {
            let interval = Duration::from_millis(interval_ms);
            let mut interval_timer = time::interval(interval);
            while highest_slot.load(Ordering::Relaxed) == 0 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let mut last_slot_updated: u64 = highest_slot.load(Ordering::Relaxed) -1;
            let mut tpu_last_refresh = Instant::now();
            loop {
                interval_timer.tick().await;

                // Check if we should exit
                if exit.load(Ordering::Relaxed) {
                    break;
                }
                let connexion_manager_clone = connexion_manager.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = connexion_manager_clone.update();
                })
                .await
                .unwrap_or_else(|e| {
                    debug!("Failed to update connexion manager: {:?}", e);
                });
                let confirmed_connexion = connexion_manager.clone().get_connection(CommitmentConfig::confirmed().commitment);

                if tpu_last_refresh.elapsed().as_secs() > 300 {
                    let cluster_tpu_info = cluster_tpu_info.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Ok(mut cluster_info) = cluster_tpu_info.write() {
                            cluster_info.refresh_recent_peers(&confirmed_connexion);
                        }
                    })
                    .await
                    .unwrap_or_else(|e| {
                        debug!("Failed to refresh peers: {:?}", e);
                    });
                    tpu_last_refresh = Instant::now();
                } 

                let cluster_slot = highest_slot.load(Ordering::Relaxed);

                if last_slot_updated < cluster_slot + 50 {
                    let cutoff = cluster_slot - 5;
                    debug!("Updating jito slots map at slot {}", last_slot_updated);
                    {
                        let mut leader_schedule = leader_schedule_cache_clone.write().unwrap();
                        let _ = leader_schedule.advance_n_slots(
                            connexion_manager.get_connection(CommitmentConfig::confirmed().commitment),
                            50,
                            cutoff
                        );
                    }
                    let jito_leader_cache_clone = jito_leader_cache.leaders.read().await;
                    let mut is_jito_slots_map_clone = is_jito_slots_map.write().unwrap();
                    
                    while !is_jito_slots_map_clone.is_empty() && is_jito_slots_map_clone[0].0 < cutoff {
                        is_jito_slots_map_clone.remove(0);
                    }
                    let leader_schedule = leader_schedule_cache_clone.read().unwrap();
                    for slot in (last_slot_updated + 1)..(last_slot_updated + 51) {
                        let leader: Option<String> = leader_schedule.slot_leader_at(slot);
                        if jito_leader_cache_clone.len() == 0 {
                            is_jito_slots_map_clone.push((slot, true));
                            continue;
                        }
                        if leader.is_some() {
                            if jito_leader_cache_clone.contains(&leader.unwrap()) {
                                is_jito_slots_map_clone.push((slot, true));
                            } else {
                                is_jito_slots_map_clone.push((slot, false));
                            }
                        }
                        else {
                            debug!("No leader found for slot {}", slot);
                            is_jito_slots_map_clone.push((slot, true));
                        }
                    }
                    last_slot_updated = last_slot_updated + 50;
                }

                debug!("update routine task finished");
            }
        })
    }
    pub fn refresh_jito_leader_cache_routine_task(
        jito_leader_cache: JitoLeaderCache,
    ) -> TokioJoinHandle<()> {
        let interval = Duration::from_secs(3600);
        tokio::spawn(async move {
            let mut interval_timer = time::interval(interval);
            loop {
                interval_timer.tick().await;

                debug!("Fetching Jito validators...");

                let client = reqwest::Client::new();
                let mut node_keys = HashSet::new();
                match client
                    .get("https://kobe.mainnet.jito.network/api/v1/validators")
                    .send()
                    .await
                {
                    Ok(response) => {
                        if let Ok(json) = response.json::<serde_json::Value>().await {
                            if let Some(validators) = json["validators"].as_array() {
                                let vote_accounts: Vec<String> = validators
                                    .iter()
                                    .filter_map(|v| v["vote_account"].as_str().map(String::from))
                                    .collect();

                                debug!("Found {} Jito validators", vote_accounts.len());

                                // Process in batches of 50
                                for chunk in vote_accounts.chunks(20) {
                                    let mut batch_requests = Vec::new();
                                    for vote_account in chunk {
                                        let request = json!({
                                            "jsonrpc": "2.0",
                                            "id": "1",
                                            "method": "getVoteAccounts",
                                            "params": [{
                                                "votePubkey": vote_account
                                            }]
                                        });
                                        batch_requests.push(request);
                                    }

                                    let client = reqwest::Client::new();
                                    if let Ok(response) = client
                                        .post("http://127.0.0.1:8899")
                                        .json(&batch_requests)
                                        .send()
                                        .await
                                    {
                                        if let Ok(results) =
                                            response.json::<Vec<serde_json::Value>>().await
                                        {
                                            for result in results {
                                                if let Some(current) =
                                                    result["result"]["current"].as_array()
                                                {
                                                    for account in current {
                                                        if let Some(node_key) =
                                                            account["nodePubkey"].as_str()
                                                        {
                                                            node_keys.insert(node_key.to_string());
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    tokio::time::sleep(Duration::from_millis(200)).await;
                                }
                            }
                        }
                    }
                    Err(e) => error!("Failed to fetch Jito validators: {:?}", e),
                }
                let mut jito_cache_guard = jito_leader_cache.leaders.write().await;
                debug!("Updated Jito leader cache with {} leaders", node_keys.len());
                *jito_cache_guard = node_keys;
            }
        })
    }

    fn refresh_arbitrage_pairs_routine_task(
        pools_manager: PoolsManager,
        tracked_pubkeys_sender: Arc<Sender<TrackedPubkeys>>,
        wallets_pool: Arc<WalletsPool>,
        farmable_tokens: FarmableTokens,
        token_accounts_sender: mpsc::Sender<TokenAccounts>,
    ) -> TokioJoinHandle<()> {
        tokio::spawn(async move {
            let interval = Duration::from_secs(REFRESH_PAIR_INTERVAL);
            let mut interval_timer = time::interval(interval);
            let tracked_pubkeys_sender_clone = tracked_pubkeys_sender.clone();
            let mut last_updated_wallets = Instant::now() - Duration::from_secs(3000);
            let mut last_updated_new_pairs = Instant::now() - Duration::from_secs(3000);
            let mut last_updated_refresh_pools = Instant::now() - Duration::from_secs(3000);
            loop {
                if last_updated_wallets.elapsed().as_secs() > 1500 {
                    let wallets_pool_clone = wallets_pool.clone();
                    let async_connexion = AsyncRpcClient::new_with_commitment("http://0.0.0.0:8899".to_string(), CommitmentConfig::confirmed());
                    match wallets_pool_clone.refresh_wallets(&async_connexion, &farmable_tokens.tokens.clone()).await {
                        Ok(_) => {
                            last_updated_wallets = Instant::now();
                            
                        },
                        Err(e) => {
                            error!("Error refreshing wallets: {:?}", e);
                        }
                    }
                    
                }
                
                // Clone these values to avoid holding references across await points
                let wallets_clone = wallets_pool.clone();
                let sender_clone = tracked_pubkeys_sender_clone.clone();
                
                if last_updated_new_pairs.elapsed().as_secs() > 300 && false{
                    match pools_manager.search_new_arbitrage_pairs(wallets_clone, sender_clone).await {
                        Ok(_) => {
                            info!("Successfully searched for new arbitrage pairs");
                            last_updated_new_pairs = Instant::now();
                            let token_accounts = 
                            wallets_pool.get_wallet(0).token_accounts.read().unwrap().clone();
                            
                            token_accounts_sender.send(token_accounts).await.unwrap();
                        },
                        Err(e) => {
                            error!("Error searching for new arbitrage pairs: {:?}", e);
                            last_updated_new_pairs = Instant::now();
                        }
                    }
                }

                

                
                if last_updated_refresh_pools.elapsed().as_secs() > 30 && false {
                    match pools_manager.clear_tracked_pubkeys().await {
                        Ok(_) => {},
                        Err(e) => {
                            error!("Error clearing tracked pubkeys: {:?}", e);
                        }
                    }
                    match pools_manager.update_pools().await {
                        Ok(_) => {},
                        Err(e) => {
                            error!("Error updating pools: {:?}", e);
                        }
                    }
                    let start_time = Instant::now();
                    match pools_manager.initialize_pools().await {
                        Ok((to_initialize_count, initialized_count)) => {

                            println!(
                                "Initialized {} pools out of {} in {}ms", initialized_count, to_initialize_count, start_time.elapsed().as_millis()
                            );
                        },
                        Err(e) => {
                            error!("Error initializing pools: {:?}", e);
                        }
                    }
                    last_updated_refresh_pools = Instant::now();
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                
            }
        })
    }

    pub fn join(self) -> Result<(), Box<dyn std::error::Error>> {
        self.arbitrage_simulator_service.thread_hdl.join().unwrap();
        for task in self.periodic_tasks {
            task.abort();
        }
        drop(self.connexion_routine_service);

        //self.transaction_tracker_service
        //    .shutdown_sender
        //    .send(())
        //    .unwrap();

        drop(self.runtime);
        //drop(self.transaction_tracker_service.task_handle);
        Ok(())
    }
}

pub struct TrackedPoolNotification {
    slot: Slot,
    pubkeys: Vec<String>,
}

// Add a helper function for getting slot with retries
fn get_slot_with_retries(
    client: &RpcClient,
    max_retries: u32,
    retry_delay_ms: u64,
) -> Result<u64, Box<dyn std::error::Error>> {
    let mut attempts = 0;
    loop {
        match client.get_slot_with_commitment(CommitmentConfig::processed()) {
            Ok(slot) => return Ok(slot),
            Err(err) => {
                attempts += 1;
                if attempts >= max_retries {
                    return Err(Box::new(err));
                }
                println!("Failed to get slot (attempt {}/{}): {:?}", attempts, max_retries, err);
                std::thread::sleep(Duration::from_millis(retry_delay_ms));
            }
        }
    }
}


pub async fn is_node_synced(client: &AsyncRpcClient, highest_slot: u64) -> bool {
    let slot = client.get_slot_with_commitment(CommitmentConfig::processed()).await.unwrap();
    if slot <= highest_slot {
        return true;
    }
    false
}