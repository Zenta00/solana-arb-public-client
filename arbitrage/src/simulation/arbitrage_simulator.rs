use {
    std::collections::VecDeque,
    solana_sdk::{
        signature::Keypair,
    },
    crate::{
        wallets::wallet::Wallet,
        tracker::{
            strategy::{BuyStrategy, SellStrategy},
            chart::Charts,
            copytrade::CopyTrader,
            order::OrderQueues,
            tracker_positions::{
                TrackerWalletsPositions,
                read_whale_file,
            },
            bonding_pool::{BondingPool, PumpBondingPool},
        },
        
        pools::{
            pumpfun_amm::{PumpfunAmmPool, PumpfunAmmPoolInfo},
            PoolUpdateType,
            LiquidityType,
            PoolInitialize,
            spam_pools::{
                PoolPair,
                ArbSpamPoolType,
            },
        },
        tokens::{
            pools_manager::RecentBlockhash,
            pools_searcher::FarmableTokens,
        },
        wallets::wallets_pool::WalletsPool,
        simulation::{
            router::{Router, ArbitrageRoute},
            pool_impact::{
                PoolImpacted,
                handle_amm_impact,
                AmmType,
            },
        },
        analyzer::{
            Analyzer,
            ranking::ArbitrageRankingTracker,
        },
        sender::{
            decision_maker::{
                EndpointDecision,
                DecisionMaker
            },
        },
        arbitrage_service::is_node_synced,
        fees::fee_tracker::FeeTracker,
        arbitrage_client::{
            CustomTransactionStatusBatch,
            PostTokenBalance,
            TransactionInfo,
            CustomLeaderScheduleCache
        }, connexion_routine_service::{
            JitoLeaderCache,
            LeaderConnexionCache,
            
        }, fee::get_random_jito_tip_account, pools::{meteora_dlmm::{
            bin_id_to_bin_array_index,
            Bin, 
            MeteoraDlmmPool
            }, raydium_v4::RaydiumV4Pool, Pool, PoolConstants}, 
            sender::{transaction_sender::{
                TransactionSender,
                Opportunity
            }}, 
            simulation::simulator_engine::SimulatorEngine,
            simulation::router::{SwapDirection, ArbitrageType},
            simulation::simulator_safe_maths::*, tokens::pools_manager::{
            ConnectionManager, CustomBlockHashQueue, PoolsManager
        }
    },
    bincode::serialize, 
    crossbeam_channel::{
        Receiver,
        RecvTimeoutError, 
        Sender
    }, 
    itertools::izip, 
    tracing::{
        info,
        debug,
        error
    },
    solana_sdk::hash::Hash as BlockHash,
    serde::{
        Deserialize, 
        Serialize
    }, 
    solana_client::nonblocking::rpc_client::RpcClient as AsyncRpcClient, 
    solana_ledger::{
        blockstore::BlockstoreError, 
        blockstore_processor::{
            TransactionStatusBatch, 
            TransactionStatusMessage
        }, 
        leader_schedule_cache::LeaderScheduleCache
    }, solana_rpc::rpc::JsonRpcRequestProcessor, solana_sdk::{
        account::AccountSharedData, address_lookup_table::AddressLookupTableAccount, clock::Slot, commitment_config::CommitmentLevel, pubkey::Pubkey, signature::{
            Signature,
            Signer,
        }, system_instruction, transaction::{SanitizedTransaction, Transaction, VersionedTransaction}
    },
    solana_transaction_status::token_balances::TransactionTokenBalances,
    std::{
        borrow::Borrow, 
        cmp::{max, min}, 
        collections::{
            HashMap, 
            HashSet
        }, 
        error::Error, 
        hash::{Hash, Hasher}, 
        sync::{
        atomic::{
            AtomicBool, 
            AtomicU32,
            AtomicU64, 
            Ordering
        },
        Arc, RwLock,
    }, thread::{Builder, JoinHandle}, time::{Duration, Instant, SystemTime, UNIX_EPOCH}}, tokio::runtime::Handle
};

pub const RAYDIUM_AUTHORITY_STR: &str = "5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1";
pub const CPMM_AUTHORITY_STR: &str = "GpMZbSM2GgvTKHJirzeGfMFoaZ8UR2X7F4v8vHTvxFbL";
pub const SIMULATOR_THREADS_COUNT: usize = 1;

pub const BASIS_POINT_MAX: u16 = 10000;
pub const BASIS_POINT_MAX_U32: u32 = BASIS_POINT_MAX as u32;
pub const MAX_FEE_RATE: u128 = 100_000_000;

pub const MAX_SHIFT: u128 = 1 << 64;
const MAX_SHIFT_CONST: u128 = 64;
const SQRT_MAX_SHIFT: u64 = 1 << 32;
const WALLET_PUBLIC_KEY_STR: &str = "2S4SJ9Ffyuvu246xc54buoJg6gZ7wQePoKFwSA3X7Vt3";
//const WALLET_PUBLIC_KEY_STR: &str = "CnxVMDLYEihUk3vm5mNpZUMhiMsR6tScVntWTfsUMm3C";

const MAINNET_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

const RPC_URL: &str = "http://localhost:8899";

const CT_WSOL_ACCOUNT: Pubkey = Pubkey::from_str_const("ACabTJNzKEaxNPo4HjEFaLFMYJRa7grSmTMUe6YnHw4B");

const NUM_RETRY_TX: u64 = 15;




pub fn safe_sub(a: u128, b: u128) -> u128 {
    if b > a {
        0
    } else {
        a - b
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PoolNotification {
    pub pubkey: String,
    pub txn_sig: Signature,
    pub slot: Slot,
    pub data: Vec<u8>,
    pub write_version: u64,
}

impl PoolNotification {
    pub fn len(&self) -> usize {
        let len = 112 + self.data.len();
        len
    }
}




pub struct ArbitrageSimulatorService {
    pub thread_hdl: JoinHandle<()>,
    pub simulator_threads: Vec<SimulatorThread>,
}

impl ArbitrageSimulatorService {
    pub fn new(
        transaction_sender: Arc<TransactionSender>,
        tracked_pubkey_receiver: Receiver<Vec<PoolNotification>>,
        transaction_status_receiver: Receiver<Vec<CustomTransactionStatusBatch>>,
        pool_manager: PoolsManager,
        is_jito_slots_map: Arc<RwLock<Vec<(u64, bool)>>>,
        custom_block_hash_queue: CustomBlockHashQueue,
        leader_schedule_cache: Arc<RwLock<CustomLeaderScheduleCache>>,
        leader_connexion_cache: Arc<LeaderConnexionCache>,
        highest_slot: Arc<AtomicU64>,
        runtime_handler: Handle,
        analyzer: Arc<Analyzer>,
        fee_tracker_receiver: Receiver<FeeTracker>,
        delete_pool_sender: Sender<Vec<String>>,
        update_pool_sender: Sender<(String, PoolUpdateType)>,
        initialize_sender: Sender<PoolInitialize>,
        wallets_pool: WalletsPool,
        decision_maker: DecisionMaker,
        ranking_tracker: Arc<RwLock<ArbitrageRankingTracker>>,
        tip_wallet: Arc<Wallet>,
        farmable_tokens: FarmableTokens,
        exit: Arc<AtomicBool>,
    ) -> Result<Self, Box<dyn Error>> {

        let mut simulator_threads = Vec::new();
        let mut pool_impacts_senders = Vec::new();
        let mut sender_index = 0;
        let slot_infos = Arc::new(RwLock::new(SlotInfos::new()));
        let recent_blockhash = &pool_manager.connection_manager.recent_blockhash;
        let pools_map = pool_manager.pools_map.clone();

        let mainnet_client = Arc::new(AsyncRpcClient::new(MAINNET_RPC_URL.to_string()));
        let blacklisted_accounts = vec![
            "6kkoqcKfnpJ1E8sSZPQw7hnaW4ovGqntdu5PbDDFsewg".to_string(),
            "6J3qfgsquC145TMJnQ8Q1JQcQSBtpYgds7qdsXqfK3W1".to_string(),
        ];

        let mut tracked_bonding_pools: Arc<RwLock<HashMap<String, Arc<dyn BondingPool>>>> = Arc::new(RwLock::new(HashMap::new()));

        for i in 0..SIMULATOR_THREADS_COUNT {
            let (pool_impacts_sender, pool_impacts_receiver) = crossbeam_channel::unbounded();
            simulator_threads.push(SimulatorThread::new(
                pool_manager.simulator_engine.clone(),
                i as u16, 
                pool_impacts_receiver, 
                transaction_sender.clone(),
                initialize_sender.clone(),
                delete_pool_sender.clone(),
                update_pool_sender.clone(),
                recent_blockhash.clone(),
                wallets_pool.clone(),
                runtime_handler.clone(),
                slot_infos.clone(),
                decision_maker.clone(),
                pools_map.clone(),
                blacklisted_accounts.clone(),
                ranking_tracker.clone(),
                tip_wallet.clone(),
                tracked_bonding_pools.clone(),
                farmable_tokens.clone(),
                exit.clone(),
            ).unwrap());
            pool_impacts_senders.push(pool_impacts_sender);
        }

        let thread_hdl = Builder::new()
            .name("solTxSimulator".to_string())
            .spawn(move || {
                debug!("ArbitrageSimulator has started");

                let mut pools_updated_map: HashMap<Signature, PoolImpacted> = HashMap::new();
                let (
                    connection_manager,
                    pools_map,
                    simulator_engine
                ) = {
                    (
                        pool_manager.connection_manager.clone(),
                        pool_manager.pools_map.clone(),
                        pool_manager.simulator_engine.clone()
                    )
                };
                
                let mut tracked_signatures: HashMap<Signature,FeeTracker> = HashMap::new();
                let mut loop_counter = 0;
                let mut total_time = 0;
                let mut total_batch_tx_received = 0;
                let mut total_notification_received = 0;
                let mut total_slots_processed = 0;

                let mut failed_tx_count = 0;
                let mut win_tx_count = 0;
                let mut last_stats_log_time = Instant::now();
                

                let async_client = Arc::new(AsyncRpcClient::new(RPC_URL.to_string()));


                debug!("ArbitrageSimulator initialized");

                let mut last_jito_slot_update = 0;
                let mut is_jito_slot_bool = true;

                while !exit.load(Ordering::Relaxed) {
                    let start_time = Instant::now();

                    let mut pool_notifications = match tracked_pubkey_receiver.try_recv() {
                        Ok(pool_notification) => {
                            vec![pool_notification]
                        }
                        Err(_) => vec![],
                    };
                    pool_notifications.extend(tracked_pubkey_receiver.try_iter());

                    let mut transaction_status_message = match transaction_status_receiver.try_recv() {
                        Ok(transaction_status_message) => vec![transaction_status_message],
                        Err(_) => vec![],
                    };

                    transaction_status_message.extend(transaction_status_receiver.try_iter());

                    total_batch_tx_received += transaction_status_message.len();
                    total_notification_received += pool_notifications.len();

                    pool_notifications.into_iter().for_each(|vec_pool_notification| {
                        vec_pool_notification.into_iter().for_each(|pool_notification| {
                            handle_amm_impact(
                                pool_notification,
                                &pools_map,
                                &mut pools_updated_map,
                                &tracked_bonding_pools
                            );
                        });
                    });

                    transaction_status_message.into_iter().for_each(|transaction_status_message| {
                        transaction_status_message.into_iter().for_each(|transaction_info| {
                            match transaction_info {
                                CustomTransactionStatusBatch::Transaction(transaction_info) => {   
                                    let signature = transaction_info.signature;
                                    if let Some(pool_impacted) = pools_updated_map.remove(&signature) {
                                        if let Some(key) = transaction_info.account_keys.get(0) {
                                            if key.to_string() == WALLET_PUBLIC_KEY_STR && transaction_info.post_token_balances.len() > 0 {
                                                if let Some(fee_tracker) = tracked_signatures.remove(&signature) {
                                                    info!("Success tx {}", &signature);
                                                    fee_tracker.ata_states.iter().for_each(|ata_state| {
                                                        ata_state.is_created.store(true, Ordering::Relaxed);
                                                    });
                                                    let _ = analyzer.report_transaction_success(
                                                        signature.to_string(), 
                                                        transaction_info.slot
                                                    );
                                                }
                                                else {
                                                    error!("Fee tracker not found for signature: {}", signature);
                                                }
                                            
                                            } 
                                        }
                                        let slot = transaction_info.slot;

                                        if slot > last_jito_slot_update {
                                            is_jito_slot_bool = is_jito_slot(slot, &is_jito_slots_map);
                                            let next_is_jito_slot_bool = is_jito_slot(slot + 1, &is_jito_slots_map);
                                            last_jito_slot_update = slot;

                                            let mut slot_infos_guard = slot_infos.write().unwrap();
                                            let leader_schedule_cach_r = leader_schedule_cache.read().unwrap();
                                            let leader_id = leader_schedule_cach_r.slot_leader_at(slot);
                                            let next_leader_id = leader_schedule_cach_r.slot_leader_at(slot + 1);

                                            slot_infos_guard.highest_slot = slot;
                                            slot_infos_guard.highest_slot_is_jito = is_jito_slot_bool;
                                            slot_infos_guard.next_slot_is_jito = next_is_jito_slot_bool;
                                            slot_infos_guard.highest_slot_leader_id = leader_id;
                                            slot_infos_guard.next_slot_leader_id = next_leader_id;
                                            slot_infos_guard.highest_slot_time = Instant::now();

                                            let leader_connexion_cache_clone = leader_connexion_cache.clone();

                                            let (async_current_connection_rtt, async_next_connection_rtt) = 
                                                runtime_handler.block_on(async move {
                                                    leader_connexion_cache_clone.get_current_and_next_connexions_rtt(slot).await
                                                });

                                            slot_infos_guard.current_connection_rtt = async_current_connection_rtt;
                                            slot_infos_guard.next_connection_rtt = async_next_connection_rtt;

                                            /*if slot % 750 == 0 {
                                                ranking_tracker.write().unwrap().cleanup();
                                            }*/
                                        }
                                        sender_index = (sender_index + 1) % SIMULATOR_THREADS_COUNT;
                                        let _ = pool_impacts_senders[sender_index].send((pool_impacted, transaction_info));
                                    }
                                }
                                
                                CustomTransactionStatusBatch::FreezeSlot(slot) => {
                                    debug!("Freezing slot {}", slot);
                                    total_slots_processed += 1;
                                    /*let custom_block_hash_queue_clone = custom_block_hash_queue.clone();
                                    let async_client_clone = async_client.clone();
                                    runtime_handler.spawn(async move {
                                        let _ =custom_block_hash_queue_clone.update(async_client_clone).await;
                                    });*/
                                    let slot_infos_guard = slot_infos.clone();
                                    let mainnet_client_clone = mainnet_client.clone();
                                    runtime_handler.spawn(async move {
                                        let is_node_synced = is_node_synced(&mainnet_client_clone, slot).await;
                                        slot_infos_guard.write().unwrap().is_node_synced = is_node_synced;
                                    });

                                    

                                    tracked_signatures.retain(|signature: &Signature, fee_tracker: &mut FeeTracker| {
                                        if slot > fee_tracker.last_valid_slot {
                                            false
                                        } else {
                                            true
                                        }
                                    });
                                    
                                            
                                }
                            }
                        })
                    });

                    fee_tracker_receiver.try_iter().for_each(|fee_tracker| {
                        tracked_signatures.entry(fee_tracker.signature).or_insert_with(|| fee_tracker);
                    });
                }
            })?;

        Ok(Self {
            thread_hdl,
            simulator_threads,
        })
    }

}




pub fn is_jito_slot(slot: u64, is_jito_slots_map: &Arc<RwLock<Vec<(u64, bool)>>>) -> bool {
    let is_jito_slots_map_clone = is_jito_slots_map.read().unwrap();
    let first_slot = is_jito_slots_map_clone.first().unwrap_or(&(0,true)).0;
    if first_slot > slot {
        //println!("First slot {} > slot {}", first_slot, slot);
        return true;
    }
    let slot_index = slot - first_slot;
    let (slot_found, is_jito) = match is_jito_slots_map_clone.get(slot_index as usize) {
        Some(slot_info) => slot_info,
        None => {
            error!("Slot {} not found in is_jito_slots_map", slot);
            &(slot,true)
        }
    };
    if *slot_found != slot {
        error!("Error slot found {} != {}", slot_found, slot);
    }
    debug!("Slot {} is jito: {}", slot, *is_jito);
    *is_jito
}





pub struct SimulatorThread {
    pub thread_hdl: JoinHandle<()>,
    pub thread_id: u16,
}

impl SimulatorThread {
    pub fn new(
        simulator_engine: SimulatorEngine,
        thread_id: u16, 
        pools_impact_receiver: Receiver<(PoolImpacted, TransactionInfo)>,
        transaction_sender: Arc<TransactionSender>,
        initialize_sender: Sender<PoolInitialize>,
        delete_pool_sender: Sender<Vec<String>>,
        update_pool_sender: Sender<(String, PoolUpdateType)>,
        recent_blockhash: Arc<RwLock<RecentBlockhash>>,
        wallets_pool: WalletsPool,
        runtime_handler: Handle,
        slot_infos: Arc<RwLock<SlotInfos>>,
        decision_maker: DecisionMaker, 
        pools_map: Arc<RwLock<HashMap<String, Arc<dyn Pool>>>>,
        blacklisted_accounts: Vec<String>,
        ranking_tracker: Arc<RwLock<ArbitrageRankingTracker>>,
        tip_wallet: Arc<Wallet>,
        tracked_bonding_pools: Arc<RwLock<HashMap<String, Arc<dyn BondingPool>>>>,
        farmable_tokens: FarmableTokens,
        exit: Arc<AtomicBool>,
    ) -> Result<Self, Box<dyn Error>> {
        let mut jito_opportunity_queue = JitoQueue::new();
        
        let mut previous_highest_slot = 0;
        let mut unwrapped_current_connection_rtt = Duration::from_millis(500);
        let mut unwrapped_next_connection_rtt = Duration::from_millis(500);
        let mut unwrapped_leader_id = "".to_string();
        let mut unwrapped_next_leader_id = "".to_string();
        let mut blockhash = BlockHash::default();
        let mut skip_tx = false;
        std::thread::sleep(Duration::from_secs(10));
        let whales: Vec<String> = read_whale_file("/home/thsmg/solana-arb-client/arbitrage/tracker/whales.txt");
        let copytraded_whales: HashSet<String> = read_whale_file("/home/thsmg/solana-arb-client/arbitrage/tracker/copytraded_whales.txt")
            .into_iter()
            .collect();
        
        let mut tracker_wallets = TrackerWalletsPositions::new_from_wallets(&whales);

        let keypair = Keypair::from_base58_string("");
        let address_lookup_table_account = wallets_pool.get_wallet(0).address_lookup_table_account.clone();
        let mut order_queues = OrderQueues::new();
        let mut copytrader = CopyTrader::new(
            BuyStrategy::FixedAmount(30_000_000),
            SellStrategy::FirstLead,
            10,
            copytraded_whales,
            keypair,
            address_lookup_table_account,
            CT_WSOL_ACCOUNT,
        );

        let mut charts = Charts::new();
        println!("SimulatorThread::new");
        let mut last_chart_save_time = Instant::now();

        let thread_hdl = Builder::new()
            .name(format!("SimulatorThread-{}", thread_id))
            .spawn(move || {
                let mut counter = 0_u8;
                while !exit.load(Ordering::Relaxed) {
                    let start_time = Instant::now();
                    

                    let mut pools_impacts = match pools_impact_receiver.try_recv() {
                        Ok(pools_impacts) => {
                            vec![pools_impacts]
                        }
                        Err(_) => {
                            vec![]
                        }
                    };

                    pools_impacts.extend(pools_impact_receiver.try_iter());

                    let SlotInfos {
                        highest_slot,
                        highest_slot_time,
                        highest_slot_is_jito,
                        next_slot_is_jito,
                        highest_slot_leader_id,
                        next_slot_leader_id,
                        current_connection_rtt,
                        next_connection_rtt,
                        is_node_synced,
                    } = slot_infos.read().unwrap().clone();

                    if previous_highest_slot != highest_slot {

                        let opportunities = jito_opportunity_queue.take_from_slot(highest_slot);
                        blockhash = recent_blockhash.read().unwrap().confirmed.unwrap().clone();
                        /*if !opportunities.is_empty() {
                            let transaction_sender_clone = transaction_sender.clone();
                            
                            let leader_id_clone = unwrapped_leader_id.clone();
                            let tip_wallet_clone = tip_wallet.clone();
                            runtime_handler.spawn(async move {
                                let mut futures = Vec::new();
                                
                                for opportunity in opportunities {
                                    
                                    futures.push(transaction_sender_clone.simulate_budget_and_send(
                                        opportunity,
                                        counter,
                                        blockhash,
                                        highest_slot,
                                        leader_id_clone.clone(),
                                        true,
                                        tip_wallet_clone.clone(),
                                    ));
                                }
                                
                                futures::future::join_all(futures).await;
                            });
                        }*/
                       
                        previous_highest_slot = highest_slot;
                        (unwrapped_current_connection_rtt, unwrapped_next_connection_rtt) = if current_connection_rtt.is_none() {
                            (Duration::from_millis(500), Duration::from_millis(500))
                        }
                        else {
                            (current_connection_rtt.unwrap(), next_connection_rtt.unwrap_or(Duration::from_millis(500)))
                        };

                        (unwrapped_leader_id, unwrapped_next_leader_id) = if highest_slot_leader_id.is_none() {
                            ("".to_string(), "".to_string())
                        }
                        else {
                            (highest_slot_leader_id.unwrap(), next_slot_leader_id.unwrap_or("".to_string()))
                        };
                    }

                    if pools_impacts.is_empty() {
                        std::thread::sleep(Duration::from_micros(20));
                        continue;
                    }

                    if last_chart_save_time.elapsed() > Duration::from_secs(300) {
                        let start_save = Instant::now();
                        charts.save_and_clear();
                        let end_save = Instant::now();
                        let duration_save = end_save.duration_since(start_save);
                        println!("Charts saved in {:?}s", duration_save.as_secs());
                        last_chart_save_time = Instant::now();
                    }

                    for (pool_impacted, transaction_info) in pools_impacts.iter_mut() {
                        let slot = transaction_info.slot;


                        // Start measuring time for simulation
                        let simulation_start = std::time::Instant::now();

                        
                        pool_impacted.prepare_for_simulation(
                            transaction_info, 
                            &delete_pool_sender,
                            &update_pool_sender,
                            &tracked_bonding_pools,
                            &mut tracker_wallets,
                            &farmable_tokens,
                            &pools_map,
                            &mut charts,
                            &mut copytrader,
                            &mut order_queues,
                        );
                        
                        if order_queues.state_changed || order_queues.need_check_orders() {
                            let mut awaiting_orders = Vec::new();
                            order_queues.advance_all_order_queues(&mut awaiting_orders, &mut copytrader);
                            if awaiting_orders.len() > 0 {
                                let mut txs = Vec::new();
                                for (token_address, order, amount, is_full_sell) in awaiting_orders {

                                    txs.push(copytrader.get_duplicate_txs_from_order(
                                        &order,
                                        amount,
                                        &token_address,
                                        is_full_sell,
                                        blockhash,
                                        slot,
                                    ));

                                    println!("sending swap: is buy: {} amount: {}, token: {}, slot: {}", order.is_buy(), amount, token_address, slot);
                                }
                                if txs.len() > 0 {
                                    //transaction_sender.send_swap_transaction_native(txs, 20, slot);
                                }
                            }
                        }


                        if pool_impacted.has_impact{
                            if transaction_info.account_keys.iter().any(|key| blacklisted_accounts.contains(key)) {
                                skip_tx = true;
                                println!("Skipping tx {}", transaction_info.signature);
                                continue;
                            }
    
                            if skip_tx {
                                skip_tx = false;
                                continue;
                            }
                        } else {
                            if pool_impacted.seen_pools.len() < 2 {
                                continue;
                            }
                            pool_impacted.seen_pools.iter().for_each(|(pool, _)| {
                                pool.add_congestion(slot);
                            });
                        }

                        

                        let routes = match Router::new_routes_from_price_movement(
                            pool_impacted,
                            &simulator_engine,
                            &delete_pool_sender,
                            &initialize_sender,
                        ) {
                            Ok(routes) => routes,
                            Err(_e) => {
                                continue;
                            }
                        };

                        for route in routes {
                            let (
                                min_congestion_rate, 
                                min_congestion_slot,
                                min_num_congestion_last_slot
                            ) = route.get_min_congestion_rates();

                            let (arbitrage_type, revert_types) = route.get_arbitrage_type();

                            if arbitrage_type == ArbSpamPoolType::DlmmPamm {
                                let (pool_a, pool_b) = if revert_types {
                                    (route.route[1].0.clone(), route.route[0].0.clone())
                                } else {
                                    (route.route[0].0.clone(), route.route[1].0.clone())
                                };

                                ranking_tracker.write().unwrap().record_arbitrage(
                                    PoolPair::new(
                                        pool_a, 
                                        pool_b,
                                        ArbSpamPoolType::DlmmPamm
                                    ),
                                    slot,
                                    route.simulation_result.profit as u64
                                );
                            }

                            let wallet = wallets_pool.get_wallet(0);
                            let (
                                instruction, 
                                token_accounts_to_create, 
                                versioned_transaction, 
                                token_accounts_to_update
                            ) = 
                                match wallet.create_unsigned_arb_transaction(
                                    &route,
                                    recent_blockhash.read().unwrap().confirmed.unwrap(),
                                    slot
                            ) {
                                Ok(result) => result,
                                Err(_e) => {
                                    continue;
                                }
                            };

                            let (
                                    endpoint_decision, 
                                    estimated_detection_time, 
                                    tipping_strategy
                                ) = decision_maker.make_decision(
                                    &unwrapped_leader_id,
                                    route.simulation_result.profit as u64,
                                    highest_slot_is_jito,
                                    unwrapped_current_connection_rtt,
                                    is_node_synced,
                                    slot.saturating_sub(min_congestion_slot),
                                    min_congestion_rate
                            );

                            let opportunity = Opportunity::new_from_decision(
                                slot + 1,
                                &mut jito_opportunity_queue,
                                endpoint_decision,
                                route,
                                token_accounts_to_update,
                                instruction,
                                token_accounts_to_create,
                                versioned_transaction,
                                tipping_strategy,
                                estimated_detection_time,
                                unwrapped_current_connection_rtt,
                                wallet,
                            );

                            if let Some(opportunity) = opportunity {

                                let simulation_duration = pool_impacted.sent_at.elapsed();
                                println!("\n\nProcessing time: {:?}", simulation_duration);
                                println!("{}", opportunity);
                                println!("Timestamp: {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"));
                                println!("Slot: {}", slot);
                                println!("rtt: {:?}", opportunity.current_connection_rtt);
                                println!("last congested was {} slots before", slot - min_congestion_slot);
                                println!("min_congestion_rate: {}", min_congestion_rate);
                                println!("min_num_congestion_last_slot: {}", min_num_congestion_last_slot);
                                
                                let transaction_sender_clone = transaction_sender.clone();
                                let leader_id_clone = unwrapped_leader_id.clone();
                                let tip_wallet_clone = tip_wallet.clone();
                                runtime_handler.spawn(async move {
                                    let _ = transaction_sender_clone.simulate_budget_and_send(
                                        opportunity,
                                        counter,
                                        blockhash,
                                        highest_slot,
                                        leader_id_clone.clone(),
                                        true,
                                        tip_wallet_clone,
                                    ).await;
                                });

                            }
                        }

                    }

                    counter = counter.wrapping_add(1);
                    let end_time = Instant::now();
                    let duration = end_time.duration_since(start_time);
                    // If counter overflows, it will automatically start at 0 due to wrapping_add
                    
                }
            })?;
        Ok(Self { thread_hdl, thread_id })
    }
}

#[derive(Clone)]
pub struct SlotInfos {
    pub highest_slot: u64,
    pub highest_slot_time: Instant,
    pub highest_slot_is_jito: bool,
    pub next_slot_is_jito: bool,
    pub highest_slot_leader_id: Option<String>,
    pub next_slot_leader_id: Option<String>,
    pub current_connection_rtt: Option<Duration>,
    pub next_connection_rtt: Option<Duration>,
    pub is_node_synced: bool,
}

impl SlotInfos {
    pub fn new() -> Self {
        Self {
            highest_slot: 0,
            highest_slot_time: Instant::now(),
            highest_slot_is_jito: true,
            next_slot_is_jito: true,
            highest_slot_leader_id: None,
            next_slot_leader_id: None,
            current_connection_rtt: None,
            next_connection_rtt: None,
            is_node_synced: false,
        }
    }

    pub fn update(&mut self, slot_infos: SlotInfos) {
        self.highest_slot = slot_infos.highest_slot;
        self.highest_slot_time = slot_infos.highest_slot_time;
        self.highest_slot_is_jito = slot_infos.highest_slot_is_jito;
        self.next_slot_is_jito = slot_infos.next_slot_is_jito;
        self.highest_slot_leader_id = slot_infos.highest_slot_leader_id;
        self.next_slot_leader_id = slot_infos.next_slot_leader_id;
        self.current_connection_rtt = slot_infos.current_connection_rtt;
        self.next_connection_rtt = slot_infos.next_connection_rtt;
        self.is_node_synced = slot_infos.is_node_synced;
    }
}

pub struct JitoQueue {
    opportunites: VecDeque<(u64, Opportunity)>,
}

impl JitoQueue {
    pub fn new() -> Self {
        Self { opportunites: VecDeque::new() }
    }

    pub fn add(&mut self, target_slot: u64, opportunity: Opportunity) {
        self.opportunites.push_back((target_slot, opportunity));
    }

    pub fn pop_front(&mut self) -> Option<(u64, Opportunity)> {
        self.opportunites.pop_front()
    }

    pub fn take_from_slot(&mut self, current_slot: u64) -> Vec<Opportunity> {
        let mut result = Vec::new();
        
        while let Some((slot, _)) = self.opportunites.front() {
            if *slot <= current_slot {
                if let Some((_, opportunity)) = self.opportunites.pop_front() {
                    result.push(opportunity);
                }
            } else {
                break;
            }
        }
        
        result
    }
}
