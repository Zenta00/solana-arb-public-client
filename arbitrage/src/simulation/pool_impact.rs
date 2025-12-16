use {
    std::collections::VecDeque,
    crate::{

        wallets::wallet::Wallet,
        tracker::{
            order::{Order, OrderQueues},
            copytrade::{CopyTrader, TradeDirection},
            chart::{Charts, WhaleSwap},
            tracker_positions::{
                TrackerWalletsPositions,
                PositionResult,
            },
            bonding_pool::{BondingPool, PumpBondingPool, RaydiumBondingPool},
        },
        
        pools::{
            TokenLabel,
            raydium_cpmm::RaydiumCpmmPool,
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
            arbitrage_simulator::PoolNotification,
            router::{Router, ArbitrageRoute},
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
            
        }, fee::get_random_jito_tip_account, 
        pools::{
            PoolError,
            meteora_dlmm::{
                bin_id_to_bin_array_index,
                Bin, 
                MeteoraDlmmPool
            },
            raydium_v4::RaydiumV4Pool, Pool, PoolConstants}, 
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
    solana_sdk::instruction::Instruction,
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

const COPYTRADER_WALLET: &str = "CtP5fsUHjVz2xVRakdso5GPJ1556cH81oTg3GpzCZE2X";
#[derive(Debug, PartialEq, Clone)]
pub enum AmmType {
    MeteoraDlmmPool,
    MeteoraDlmmBinArray,
    RaydiumV4Pool,
    RaydiumCpmmPool,
    OrcaWpPool,
    RaydiumBondingPool,
    RaydiumClmmPool,
    RaydiumClmmBitmapExt,
    RaydiumClmmTickArray,
    PumpfunAmmPool,
    PumpfunBondingPool,
    PoolReserve,
    NotSupported,
    MaybeSpam,
}

impl AmmType {
    pub fn new_from_len(len: usize) -> (Self, bool, bool) { // is_init , is_bonding
        match len {
            904 => (Self::MeteoraDlmmPool, false, false),
            752 => (Self::RaydiumV4Pool, true, false),
            10_136 => (Self::MeteoraDlmmBinArray, false, false),
            637 => (Self::RaydiumCpmmPool, true, false),
            211 | 300  => (Self::PumpfunAmmPool, true, false),
            256 | 49 | 150 => (Self::PumpfunBondingPool, true, true),
            1832 => (Self::RaydiumClmmBitmapExt, false, false),
            10_240 => (Self::RaydiumClmmTickArray, false, false),
            1544 => (Self::RaydiumClmmPool, false, false),
            429 => (Self::RaydiumBondingPool, true, true),
            165 => (Self::PoolReserve, true, false),
            0 => (Self::MaybeSpam, false, false),
            _ => (Self::NotSupported, false, false),
        }
    }

    pub fn get_pool(
        pools_map: &Arc<RwLock<HashMap<String, Arc<dyn Pool>>>>,
        tracked_bonding_pools: &Arc<RwLock<HashMap<String, Arc<dyn BondingPool>>>>,
        pool_address: &String,
        is_bonding: bool,
    ) -> Option<BondingPoolType> {
        let pools_map_guard = pools_map.read().unwrap();
        let bonding_map_guard = tracked_bonding_pools.read().unwrap();
        if is_bonding {
            bonding_map_guard.get(pool_address).map(|pool| BondingPoolType::BondingPool(pool.clone()))
        } else {
            pools_map_guard.get(pool_address).map(|pool| BondingPoolType::NormalPool(pool.clone()))
        }
    }
}

#[derive(Clone)]
pub enum BondingPoolType {
    NormalPool(Arc<dyn Pool>),
    BondingPool(Arc<dyn BondingPool>),
}

impl BondingPoolType {


    pub fn buy(
        &self,
        amount_in: u128,
    ) -> u128 {
        match self {
            BondingPoolType::NormalPool(pool) => {
                let direction = if pool.get_farm_token_label() == TokenLabel::Y {
                    SwapDirection::YtoX
                } else {
                    SwapDirection::XtoY
                };
                match pool.get_amount_out_with_cu(amount_in as f64, direction) {
                    Ok((amount_out, _)) => amount_out as u128,
                    Err(_) => 0,
                }
            },
            BondingPoolType::BondingPool(pool) => {
                pool.get_amount_out(amount_in, SwapDirection::YtoX)
            }
        }
    }

    pub fn sell(
        &self,
        amount_in: u128,
    ) -> u128 {
        match self {
            BondingPoolType::NormalPool(pool) => {
                let direction = if pool.get_farm_token_label() == TokenLabel::Y {
                    SwapDirection::XtoY
                } else {
                    SwapDirection::YtoX
                };
                match pool.get_amount_out_with_cu(amount_in as f64, direction) {
                    Ok((amount_out, _)) => amount_out as u128,
                    Err(_) => 0,
                }
            },
            BondingPoolType::BondingPool(pool) => {
                pool.get_amount_out(amount_in, SwapDirection::XtoY)
            }
        }
    }

    pub fn get_token_mint(&self) -> String {
        match self {
            BondingPoolType::NormalPool(pool) => pool.get_non_farm_token_mint().unwrap(),
            BondingPoolType::BondingPool(pool) => pool.get_token_mint(),
        }
    }

    pub fn get_swap_instructions(
        &self,
        amount_in: u64,
        slippage: u64,
        is_buy: bool,
        user_token_account: Pubkey,
        user_wsol_account: Pubkey,
        signer_pubkey: Pubkey,
        reserve_base_amount: u128,
        reserve_quote_amount: u128,
        full_out: bool,
        tip_amount: u64,
    ) -> (Vec<Instruction>, u32) {
        match self {
            BondingPoolType::NormalPool(pool) => {
                pool.get_swap_instructions(
                    amount_in, 
                    slippage, 
                    is_buy, 
                    user_token_account, 
                    user_wsol_account, 
                    signer_pubkey, 
                    reserve_base_amount, 
                    reserve_quote_amount, 
                    full_out, 
                    tip_amount
                )
            }
            BondingPoolType::BondingPool(pool) => {
                pool.get_swap_instructions(
                    amount_in, 
                    slippage, 
                    is_buy, 
                    user_token_account, 
                    user_wsol_account, 
                    signer_pubkey, 
                    reserve_base_amount, 
                    reserve_quote_amount, 
                    full_out, 
                    tip_amount
                )
            }
        }
    }
}
pub fn handle_amm_impact(
    pool_notification: PoolNotification,
    pools_map: &Arc<RwLock<HashMap<String, Arc<dyn Pool>>>>,
    pools_updated_map: &mut HashMap<Signature, PoolImpacted>,
    tracked_bonding_pools: &Arc<RwLock<HashMap<String, Arc<dyn BondingPool>>>>,
) {

    let (pool_type, is_init, is_bonding) = AmmType::new_from_len(pool_notification.data.len());

    let pool = AmmType::get_pool(
        pools_map,
        tracked_bonding_pools,
        &pool_notification.pubkey,
        is_bonding,
    );

    if is_bonding{
        let write_number = pool_notification.write_version;
        let pool_impacted = pools_updated_map.entry(pool_notification.txn_sig).or_insert_with(
            || PoolImpacted::new_empty(pool_notification.slot, write_number != 0)
        );
        
        if let Some(BondingPoolType::BondingPool(bonding_pool)) = pool {
            pool_impacted.bonding_pools_changed.push(
                BondingImpact {
                    pool: Some(bonding_pool.clone()),
                    pool_notification,
                    pool_type: pool_type,
                }
            );
        } else {
            pool_impacted.bonding_pools_changed.push(
                BondingImpact {
                    pool: None,
                    pool_notification,
                    pool_type: pool_type,
                }
            );
        }
        pool_impacted.write_number = write_number;
    } else {
        
        match pool_type {
            AmmType::MeteoraDlmmBinArray => {
                if let Some(BondingPoolType::NormalPool(pool)) = pool {
                    let write_number = pool_notification.write_version;
                    let pool_impacted = pools_updated_map.entry(pool_notification.txn_sig).or_insert_with(
                        || PoolImpacted::new_empty(pool_notification.slot, write_number != 0)
                    );
                    let bin_address = pool_notification.pubkey.to_string();
                    pool_impacted.dynamic_pools_changed.push(
                        NormalPoolImpact {
                            pool: Some(pool.clone()),
                            pool_notification: Some(pool_notification),
                            optional_impact: Some(
                                OptionalImpact::DlmmImpact(DlmmImpact::BinArrayImpact(DlmmBinsImpact {
                                    bin_address,
                                })),
                            ),
                            pool_type: AmmType::MeteoraDlmmBinArray,
                        }
                    );
                    pool_impacted.write_number = write_number;
                }
            }
            AmmType::MeteoraDlmmPool => {
                debug!("MeteoraDlmmPool impact {}", pool_notification.pubkey);
                
                if let Some(BondingPoolType::NormalPool(pool)) = pool {
                    let write_number = pool_notification.write_version;
                    let pool_impacted = pools_updated_map.entry(pool_notification.txn_sig).or_insert_with(
                        || PoolImpacted::new_empty(pool_notification.slot, write_number != 0)
                    );
                    pool_impacted.dynamic_pools_changed.push(
                        NormalPoolImpact {
                            pool: Some(pool.clone()),
                            pool_notification: Some(pool_notification),
                            optional_impact: Some(
                                OptionalImpact::DlmmImpact(DlmmImpact::LiquidityImpact),
                            ),
                            pool_type: pool_type,
                        }
                    );
                    pool_impacted.write_number = write_number;
                }
            }
            
            AmmType::RaydiumClmmPool => {
                let pools_map_guard = pools_map.read().unwrap();
                
                if let Some(BondingPoolType::NormalPool(pool)) = pool {
                    let write_number = pool_notification.write_version;
                    let pool_impacted = pools_updated_map.entry(pool_notification.txn_sig).or_insert_with(
                        || PoolImpacted::new_empty(pool_notification.slot, write_number != 0)
                    );
                    pool_impacted.dynamic_pools_changed.push(
                        NormalPoolImpact {
                            pool: Some(pool.clone()),
                            pool_notification: Some(pool_notification),
                            optional_impact: Some(
                                OptionalImpact::RaydiumClmmImpact(RaydiumClmmImpact::LiquidityImpact),
                            ),
                            pool_type: pool_type,
                        }
                    );
                    pool_impacted.write_number = write_number;
                }
            }
            AmmType::RaydiumClmmTickArray => {
                if let Some(BondingPoolType::NormalPool(pool)) = pool {
                    let write_number = pool_notification.write_version;
                    let pool_impacted = pools_updated_map.entry(pool_notification.txn_sig).or_insert_with(
                        || PoolImpacted::new_empty(pool_notification.slot, write_number != 0)
                    );
                    let tick_array_address = pool_notification.pubkey.to_string();
                    pool_impacted.dynamic_pools_changed.push(
                        NormalPoolImpact {
                            pool: Some(pool.clone()),
                            pool_notification: Some(pool_notification),
                            optional_impact: Some(
                                OptionalImpact::RaydiumClmmImpact(RaydiumClmmImpact::TickArrayImpact(RaydiumClmmTickArrayImpact {
                                    tick_array_address,
                                })),
                            ),
                            pool_type: AmmType::RaydiumClmmPool,
                        }
                    );
                    pool_impacted.write_number = write_number;
                }
            }
            AmmType::RaydiumClmmBitmapExt => {
                if let Some(BondingPoolType::NormalPool(pool)) = pool {
                    let write_number = pool_notification.write_version;
                    let pool_impacted = pools_updated_map.entry(pool_notification.txn_sig).or_insert_with(
                        || PoolImpacted::new_empty(pool_notification.slot, write_number != 0)
                    );
                    let bitmap_address = pool_notification.pubkey.to_string();
                    pool_impacted.dynamic_pools_changed.push(
                        NormalPoolImpact {
                            pool: Some(pool.clone()),
                            pool_notification: Some(pool_notification),
                            optional_impact: Some(
                                OptionalImpact::RaydiumClmmImpact(RaydiumClmmImpact::BitmapImpact(RaydiumClmmBitmapImpact {
                                    bitmap_address,
                                })),
                            ),
                            pool_type: AmmType::RaydiumClmmPool,
                        }
                    );
                    pool_impacted.write_number = write_number;
                }
            }
            AmmType::PumpfunAmmPool | AmmType::PoolReserve => {

                if let Some(BondingPoolType::NormalPool(pool)) = pool {

                    let write_number = pool_notification.write_version;
                    let pool_impacted = pools_updated_map.entry(pool_notification.txn_sig).or_insert_with(
                        || PoolImpacted::new_empty(pool_notification.slot, write_number != 0)
                    );
                    // Only add if pool is not already in constant_product_changed
                    if !pool_impacted.constant_product_pools_changed.iter().any(|impact| 
                        if let Some(pool) = impact.pool.as_ref() {
                            pool.get_address() == pool.get_address()
                        } else {
                            false
                        }
                    ) 
                    {
                        pool_impacted.constant_product_pools_changed.push(
                            NormalPoolImpact {
                                pool: Some(pool.clone()),
                                pool_notification: None,
                                optional_impact: None,
                                pool_type,
                            }
                        );
                    }
                    pool_impacted.write_number = write_number;
                } else {
                    if is_init {
                        let write_number = pool_notification.write_version;
                        let pool_impacted = pools_updated_map.entry(pool_notification.txn_sig).or_insert_with(
                            || PoolImpacted::new_empty(pool_notification.slot, write_number != 0)
                        );
                        let pool_address = pool_notification.pubkey.to_string();

                        if !pool_impacted.constant_product_pools_changed.iter().any(|impact| 
                            if let Some(pool) = impact.pool.as_ref() {
                                pool.get_address() == pool_address
                            } else if let Some(pool) = impact.pool_notification.as_ref() {
                                pool.pubkey.to_string() == pool_address
                            } else {
                                false
                            }
                        ) {
                            pool_impacted.constant_product_pools_changed.push(
                                NormalPoolImpact {
                                    pool: None,
                                    pool_notification: Some(pool_notification),
                                    optional_impact: None,
                                    pool_type,
                                }
                            );
                        }
                    }
                }
            }
            AmmType::RaydiumV4Pool | AmmType::RaydiumCpmmPool => {
                if let Some(BondingPoolType::NormalPool(pool)) = pool  {
                    let write_number = pool_notification.write_version;
                    let pool_impacted = pools_updated_map.entry(pool_notification.txn_sig).or_insert_with(
                        || PoolImpacted::new_empty(pool_notification.slot, write_number != 0)
                    );
                    let pool_notification = if pool_type == AmmType::RaydiumCpmmPool {  
                        Some(pool_notification)
                    } else {
                        None
                    };
                    pool_impacted.constant_product_pools_changed.push(
                        NormalPoolImpact {
                            pool: Some(pool.clone()),
                            pool_notification,
                            optional_impact: None,
                            pool_type,
                        }
                    );
                    pool_impacted.write_number = write_number;
                } else {
                    if is_init {
                        let write_number = pool_notification.write_version;
                        let pool_impacted = pools_updated_map.entry(pool_notification.txn_sig).or_insert_with(
                            || PoolImpacted::new_empty(pool_notification.slot, write_number != 0)
                        );
                        pool_impacted.constant_product_pools_changed.push(
                            NormalPoolImpact {
                                pool: None,
                                pool_notification: Some(pool_notification),
                                optional_impact: None,
                                pool_type,
                            }
                        );
                        
                    }
                }
            }
    
            AmmType::MaybeSpam => {
                let pools_map_guard = pools_map.read().unwrap();
                if let Some(BondingPoolType::NormalPool(pool)) = pool {
                    let write_number = pool_notification.write_version;
                    let pool_impacted = pools_updated_map.entry(pool_notification.txn_sig).or_insert_with(
                        || PoolImpacted::new_empty(pool_notification.slot, write_number != 0)
                    );
    
                    // Add pool to seen_pools if not already present
                    if !pool_impacted.seen_pools.iter().any(|(p, _)| 
                        p.get_address() == pool.get_address()) {
                        pool_impacted.seen_pools.push((pool.clone(), false));
                    }
                    pool_impacted.write_number = write_number;
                } else if pool_notification.pubkey == COPYTRADER_WALLET {
                    let _pool_impacted = pools_updated_map.entry(pool_notification.txn_sig).or_insert_with(
                        || PoolImpacted::new_empty(pool_notification.slot, false)
                    );
                }
            }
            _ => {}
        }
    } 
    
}

pub enum RaydiumClmmImpact {
    TickArrayImpact(RaydiumClmmTickArrayImpact),
    LiquidityImpact,
    BitmapImpact(RaydiumClmmBitmapImpact),
}

pub struct RaydiumClmmTickArrayImpact {
    pub tick_array_address: String,
}


pub struct RaydiumClmmBitmapImpact {
    pub bitmap_address: String,

}

pub enum OptionalImpact{
    DlmmImpact(DlmmImpact),
    RaydiumClmmImpact(RaydiumClmmImpact),
}


pub enum DlmmImpact {
    BinArrayImpact(DlmmBinsImpact),
    LiquidityImpact,
}


pub struct DlmmBinsImpact {
    pub bin_address: String,
}


pub struct NormalPoolImpact {
    pub pool: Option<Arc<dyn Pool>>,
    pub pool_notification: Option<PoolNotification>,
    pub optional_impact: Option<OptionalImpact>,
    pub pool_type: AmmType,
}
impl NormalPoolImpact {
    pub fn get_pool(&self) -> Option<Arc<dyn Pool>> {
        self.pool.clone()
    }
}

pub struct BondingImpact {
    pub pool: Option<Arc<dyn BondingPool>>,
    pub pool_notification: PoolNotification,
    pub pool_type: AmmType,
}
impl BondingImpact {
    pub fn get_pool(&self) -> Option<Arc<dyn BondingPool>> {
        self.pool.clone()
    }
}

pub struct PoolImpacted {
    pub bonding_pools_changed: Vec<BondingImpact>,
    pub dynamic_pools_changed: Vec<NormalPoolImpact>,
    pub constant_product_pools_changed: Vec<NormalPoolImpact>,
    pub seen_pools: Vec<(Arc<dyn Pool>, bool)>,
    pub slot: u64,
    pub sent_at: Instant,
    pub write_number: u64,
    pub has_impact: bool,
    pub committed: bool,
}

impl PoolImpacted {

    pub fn new_empty(slot: u64, committed: bool) -> Self {
        Self {
            bonding_pools_changed: vec![],
            dynamic_pools_changed: vec![],
            constant_product_pools_changed: vec![],
            seen_pools: vec![],
            slot,
            sent_at: Instant::now(),
            write_number: 0,
            has_impact: false,
            committed,
        }
    }
    pub fn prepare_for_simulation(&mut self, 
        transaction_info: &TransactionInfo, 
        delete_pool_sender: &Sender<Vec<String>>,
        update_pool_sender: &Sender<(String, PoolUpdateType)>,
        tracked_bonding_pools: &Arc<RwLock<HashMap<String, Arc<dyn BondingPool>>>>,
        tracker_wallets: &mut TrackerWalletsPositions,
        farmable_tokens: &FarmableTokens,
        pools_map: &Arc<RwLock<HashMap<String, Arc<dyn Pool>>>>,
        charts: &mut Charts,
        copytrader: &mut CopyTrader,
        order_queues: &mut OrderQueues,
    ) {
        if !self.committed {

            return;
        }
        if !self.dynamic_pools_changed.is_empty() {
            PoolImpacted::prepare_dynamic_pools(
                &mut self.dynamic_pools_changed,
                &mut self.seen_pools,
            );
        }

        if !self.constant_product_pools_changed.is_empty() {
            self.prepare_constant_product_pools(
                delete_pool_sender,
                transaction_info,
                update_pool_sender,
                tracker_wallets,
                farmable_tokens,
                pools_map,
                charts,
                copytrader,
                order_queues,
            );
        }

        if !self.bonding_pools_changed.is_empty() {
            PoolImpacted::prepare_bonding_pools(
                transaction_info,
                tracked_bonding_pools,
                tracker_wallets,
                &self.bonding_pools_changed,
                &mut self.seen_pools,
                charts,
                copytrader,
                order_queues
            );
        }

        self.seen_pools.iter_mut().for_each(|(pool, price_changed)| {
            if pool.get_liquidity_type() == LiquidityType::ConstantProduct {
                return;
            }

            match pool.set_price(update_pool_sender) {
                Ok(is_price_diff) => {
                    *price_changed = is_price_diff;
                    self.has_impact = true;
                },
                Err(_e) => {
                    let _ = delete_pool_sender.send(pool.get_all_associated_tracked_pubkeys());
                },
            };
        });
    }

    fn prepare_dynamic_pools(
        normal_pools_changed: &mut Vec<NormalPoolImpact>,
        seen_pools: &mut Vec<(Arc<dyn Pool>, bool)>,
    ) {
        for normal_pool_impact in normal_pools_changed.iter_mut() {
            match normal_pool_impact.pool_type {
                AmmType::MeteoraDlmmPool => {
                    let dlmm_impact = normal_pool_impact.optional_impact.as_ref().unwrap();
                    let pool = normal_pool_impact.pool.as_ref().unwrap();
                    match dlmm_impact {
                        OptionalImpact::DlmmImpact(dlmm_impact) => {
                            PoolImpacted::prepare_meteora_dlmm(
                                pool, dlmm_impact, 
                                normal_pool_impact.pool_notification.as_ref().unwrap(), 
                                seen_pools
                            );
                        }
                        _ => {}
                    }
                }
                AmmType::RaydiumClmmPool => {
                    let raydium_clmm_impact = normal_pool_impact.optional_impact.as_ref().unwrap();
                    let pool = normal_pool_impact.pool.as_ref().unwrap();
                    match raydium_clmm_impact {
                        OptionalImpact::RaydiumClmmImpact(raydium_clmm_impact) => {
                            PoolImpacted::prepare_raydium_clmm(
                                pool, raydium_clmm_impact, 
                                normal_pool_impact.pool_notification.as_ref().unwrap(), 
                                seen_pools
                            );
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    pub fn prepare_meteora_dlmm(
        pool: &Arc<dyn Pool>,
        dlmm_impact: &DlmmImpact,
        pool_notification: &PoolNotification,
        seen_pools: &mut Vec<(Arc<dyn Pool>, bool)>,
    ) {
        match dlmm_impact { 
            DlmmImpact::BinArrayImpact(dlmm_bins_impact) => {
                if let Some(meteora_dlmm_pool) = pool.as_meteora_dlmm() {
                    meteora_dlmm_pool.update_bin_container_from_data(
                        dlmm_bins_impact.bin_address.clone(), 
                        &pool_notification.data,
                        pool_notification.write_version
                    );
                }
            },
            DlmmImpact::LiquidityImpact => {
                if let Some(meteora_dlmm_pool) = pool.as_meteora_dlmm() {
                    meteora_dlmm_pool.update_liquidity_state_from_data(&pool_notification.data, pool_notification.write_version);
                    if !seen_pools.iter().any(|(p, _)| p.get_address() == pool.get_address()) {
                        seen_pools.push((pool.clone(), false));
                    }
                }
            }
        }
    }
    

    pub fn prepare_raydium_clmm(
        pool: &Arc<dyn Pool>,
        raydium_clmm_impact: &RaydiumClmmImpact,
        pool_notification: &PoolNotification,
        seen_pools: &mut Vec<(Arc<dyn Pool>, bool)>,
    ) {
        match raydium_clmm_impact { 
            RaydiumClmmImpact::TickArrayImpact(tick_array_impact) => {
                if let Some(raydium_clmm_pool) = pool.as_raydium_clmm() {
                    raydium_clmm_pool.update_tick_array_from_data(
                        tick_array_impact.tick_array_address.clone(), 
                        &pool_notification.data,
                        pool_notification.write_version
                    );
                    
                }
            },
            RaydiumClmmImpact::LiquidityImpact => {
                if let Some(raydium_clmm_pool) = pool.as_raydium_clmm() {
                    raydium_clmm_pool.update_liquidity_state_from_data(&pool_notification.data, pool_notification.write_version);
                    if !seen_pools.iter().any(|(p, _)| p.get_address() == pool.get_address()) {
                        seen_pools.push((pool.clone(), false));
                    }
                }
            },
            RaydiumClmmImpact::BitmapImpact(bitmap_impact) => {
                if let Some(raydium_clmm_pool) = pool.as_raydium_clmm() {
                    raydium_clmm_pool.update_tick_array_bitmap_extension_from_data(&pool_notification.data, pool_notification.write_version);
                    if !seen_pools.iter().any(|(p, _)| p.get_address() == pool.get_address()) {
                        seen_pools.push((pool.clone(), false));
                    }
                }
            }
        }
    }

    pub fn prepare_bonding_pools(
            transaction_info: &TransactionInfo,
            tracked_bonding_pools: &Arc<RwLock<HashMap<String, Arc<dyn BondingPool>>>>,
            tracker_wallets: &mut TrackerWalletsPositions,
            bonding_pools_changed: &Vec<BondingImpact>,
            seen_pools: &mut Vec<(Arc<dyn Pool>, bool)>,
            charts: &mut Charts,
            copytrader: &mut CopyTrader,
            order_queues: &mut OrderQueues,
        ) {
            for bonding_pool_impact in bonding_pools_changed.iter() {
                PoolImpacted::prepare_bonding_pool(
                    transaction_info,
                    tracked_bonding_pools,
                    bonding_pool_impact,
                    tracker_wallets,
                    charts,
                    copytrader,
                    order_queues,
                );
                
            }
        }

    pub fn prepare_bonding_pool(
        transaction_info: &TransactionInfo,
        tracked_bonding_pools: &Arc<RwLock<HashMap<String, Arc<dyn BondingPool>>>>,
        bonding_impact: &BondingImpact,
        tracker_wallets: &mut TrackerWalletsPositions,
        charts: &mut Charts,
        copytrader: &mut CopyTrader,
        order_queues: &mut OrderQueues,
    ) {

        let pool = bonding_impact.pool.as_ref();
        let pool_notification = &bonding_impact.pool_notification;

        let maker = transaction_info.account_keys[0].to_string();
        if tracker_wallets.positions.contains_key(&maker) {
            println!("maker : {} tx_sig : {}", maker, transaction_info.signature);
        } 
        let (swap_result, mint_address, pool) = if let Some(pool) = pool {
            ((pool.update_with_data(&pool_notification.data)), pool.get_token_mint(), pool)
        } else {
            match bonding_impact.pool_type {
                AmmType::PumpfunBondingPool => {
                    let mint = transaction_info.find_mint(&pool_notification.pubkey);
                    if let Some(mint) = mint {
                        let pump_pool = 
                            PumpBondingPool::new(pool_notification.pubkey.clone(), &pool_notification.data, mint);
                        tracked_bonding_pools.write().unwrap().insert(pool_notification.pubkey.clone(), Arc::new(pump_pool));

                    }
                }
                AmmType::RaydiumBondingPool => {
                    let raydium_pool = RaydiumBondingPool::new(pool_notification.pubkey.clone(), &pool_notification.data);
                    tracked_bonding_pools.write().unwrap().insert(pool_notification.pubkey.clone(), Arc::new(raydium_pool));
                }
                _ => { return;}
            }
            
            return;
        };
        if let Some(swap_result) = swap_result {
            PoolImpacted::handle_swap_result(
                maker,
                swap_result,
                BondingPoolType::BondingPool(pool.clone()),
                tracker_wallets,
                copytrader,
                order_queues,
                charts,
                transaction_info.slot,
            );
        }
        
    }

    pub fn prepare_constant_product_pools(
        &mut self, 
        delete_pool_sender: &Sender<Vec<String>>,
        transaction_info: &TransactionInfo,
        _update_pool_sender: &Sender<(String, PoolUpdateType)>,
        tracker_wallets: &mut TrackerWalletsPositions,
        farmable_tokens: &FarmableTokens,
        pools_map: &Arc<RwLock<HashMap<String, Arc<dyn Pool>>>>,
        charts: &mut Charts,
        copytrader: &mut CopyTrader,
        order_queues: &mut OrderQueues,
    ) {
        let kp_changed = &mut self.constant_product_pools_changed;
        let len = kp_changed.len();
        let mut pool_data_vec = Vec::with_capacity(len);
        let mut owners = HashSet::new();
        
        for kp_impact in kp_changed.iter_mut() {
            if let Some(pool) = &kp_impact.pool {
                owners.insert(pool.get_owner());
                pool_data_vec.push(
                    (
                        pool.get_reserve_x_account(),
                        pool.get_reserve_y_account(),
                        pool.clone(),
                    )
                )
            } else {

                let pool_notification = kp_impact.pool_notification.as_ref().unwrap();
                let pool_key = Pubkey::from_str_const(&pool_notification.pubkey);

                let pool: Arc<dyn Pool> = match kp_impact.pool_type {
                    AmmType::PumpfunAmmPool => {
                        
                        let pool_info = 
                        PumpfunAmmPoolInfo::new(&pool_key, &pool_notification.data);

                        let pool_info = if let Some(pool_info) = pool_info {
                            pool_info
                        } else {
                            continue;
                        };

                        let farm_token_label = farmable_tokens.choose_farmable_tokens(
                            &pool_info.keys.vec[4].pubkey.to_string(),  
                            &pool_info.keys.vec[5].pubkey.to_string()
                        );
    
                        let pool = Arc::new(PumpfunAmmPool::new(
                            pool_key, 
                                pool_info, 
                                farm_token_label
                            ).unwrap()
                        );

                        let mut pools_map_write = pools_map.write().unwrap();
                        pools_map_write.insert(pool.get_reserve_y_account().clone(), pool.clone());
                    
                        pool
                    }
                    AmmType::RaydiumV4Pool => {
                        let pool = RaydiumV4Pool::new_from_data(
                            pool_key,
                            farmable_tokens,
                            &pool_notification.data,
                        );
                        if let Some(pool) = pool {
                            Arc::new(pool)
                        } else {
                            continue;
                        }
                    }
                    AmmType::RaydiumCpmmPool => {
                        let pool = RaydiumCpmmPool::new_from_data(
                            pool_key,
                            farmable_tokens,
                            &pool_notification.data,
                        );
                        if let Some(pool) = pool {
                            Arc::new(pool)
                        } else {
                            continue;
                        }
                    }
                    _ => {
                        continue;
                    }
                };
                    
                pools_map.write().unwrap().insert(pool_notification.pubkey.clone(), pool.clone());
                owners.insert(pool.get_owner());
                pool_data_vec.push(
                    (
                        pool.get_reserve_x_account(),
                        pool.get_reserve_y_account(),
                        pool.clone(),
                    )
                );
                kp_impact.pool = Some(pool);
            }
        }
    
        // Split the collected data into separate vectors
        
        let reserves_amounts = extract_indexes_for_reserves_accounts(
            &transaction_info.account_keys,
            &pool_data_vec,
            &transaction_info.post_token_balances,
            &owners,
            &transaction_info.signature.to_string(),
        );

        /*
        let signer = transaction_info.account_keys[0].to_string();
            // Read the bool and amount from test.txt file
        let pool = kp_changed[0].get_pool();
        if signer != "4VztozdDL8yFPLdpUpMNT3Hf7xMZ2qPGAMretExTvZ78".to_string() && signer != "AEK3Z5CGNgmRQHxK9sRHbbn6MJ5oCg5M96qsXjQJE123".to_string() 
             && signer != "Cr6v6hzmEGhxEHUiVtQRuVTj65cGRzhmKZ6mru8ARY1r".to_string()
             && signer != "A7FMMgue4aZmPLLoutVtbC7gJcyqkHybUieiaDg9aaVE".to_string() 
             && signer != "HxkTYMtx4FsMCBRtJ4LTwZnGAMZE4vMbh6sPUBgRCeY7".to_string()
            && pool.get_pool_type() == "Raydium-Cpmm" {

            println!("sig : {}", transaction_info.signature);
            println!("pool : {}", pool.get_address());
            
            let (x_for_y, amount) = match std::fs::read_to_string("/home/thsmg/solana-arb-client/arbitrage/test.txt") {
                Ok(content) => {
                    let parts: Vec<&str> = content.trim().split(';').collect();
                    if parts.len() == 2 {
                        let direction = match parts[0].parse::<u8>() {
                            Ok(val) => val == 1,
                            Err(e) => {
                                println!("Error parsing direction from test.txt: {}", e);
                                false
                            }
                        };
                        
                        let amount_value = match parts[1].parse::<f64>() {
                            Ok(val) => {
                                println!("Read amount from test.txt: {}", val);
                                val
                            },
                            Err(e) => {
                                println!("Error parsing amount from test.txt: {}", e);
                                0.0
                            }
                        };
                        
                        (direction, amount_value)
                    } else {
                        println!("Invalid format in test.txt. Expected 'bool;amount'");
                        (false, 0.0)
                    }
                },
                Err(e) => {
                    println!("Error reading test.txt: {}", e);
                    (false, 0.0)
                }
            };

            // If we have a valid amount, test get_amount_out_with_cu for each pool
            
            if amount > 0.0 { 
                match pool.get_amount_out_with_cu(amount, if x_for_y { SwapDirection::XtoY } else { SwapDirection::YtoX }) {
                    Ok((amount_out, cu)) => {
                            println!("Pool {}: amount_out={}, cu={}", 
                                pool.get_address(), amount_out, cu);
                    },
                        Err(e) => {
                            println!("Pool {}: get_amount_out_with_cu returned error: {:?}", 
                                pool.get_address(), e);
                    }
                }
            }
        }*/
        for (kp_impact, reserves) in kp_changed.iter().zip(reserves_amounts) {
            if !reserves.2 {
                continue;
            } 
            let pool = if let Some(pool) = kp_impact.pool.as_ref() {
                pool
            } else {
                continue;
            };
            let optional_data = if let Some(pool_notification) = kp_impact.pool_notification.as_ref() {
                &pool_notification.data
            } else {
                &vec![]
            };


            match pool.update_with_balances(reserves.0 as u128, reserves.1 as u128, optional_data) {
                Ok((is_price_diff, swap_result)) => {
                    if !self.seen_pools.iter().any(|(p, _)| p.get_address() == pool.get_address()) {
                        self.seen_pools.push((pool.clone(), is_price_diff));
                    }

                    let maker = transaction_info.account_keys[0].to_string();
                    
                    if let Some(swap_result) = swap_result {
                        PoolImpacted::handle_swap_result(
                            maker,
                            swap_result,
                            BondingPoolType::NormalPool(pool.clone()),
                            tracker_wallets,
                            copytrader,
                            order_queues,
                            charts,
                            transaction_info.slot,
                        );
                    }
                    
                    if is_price_diff {
                        self.has_impact = true;
                    }
                },
                Err(_e) => {
                    let _ = delete_pool_sender.send(pool.get_all_associated_tracked_pubkeys());
                },
            };
        }
    }

    pub fn handle_swap_result(
        maker: String,
        swap_result: (u128, u128, bool, u128, u128),
        pool: BondingPoolType,
        tracker_wallets: &mut TrackerWalletsPositions,
        copytrader: &mut CopyTrader,
        order_queues: &mut OrderQueues,
        charts: &mut Charts,
        slot: u64,
    ) {
        let (sol_amount, token_amount, is_buy, post_sol_reserve, post_token_reserve) = swap_result;
        if sol_amount != 0 && token_amount != 0 {
            
        
            let token_mint = pool.get_token_mint();
            let (maker_is_whale, pre_token_balance) = if tracker_wallets.positions.contains_key(&maker) {
                
                let (_, pre_token_balance) = tracker_wallets.update_with_swap(
                    maker.clone(), 
                    token_mint.clone(), 
                    is_buy, 
                    sol_amount, 
                    token_amount
                );
                (true, pre_token_balance)
            } else if copytrader.wallet.wallet_str == maker {
                let order = order_queues.confirm_order(&token_mint);
                let direction = if is_buy { TradeDirection::Buy } else { TradeDirection::Sell };
                if let Some(order) = order {

                    match &order {
                        Order::Buy(_buy_order) => {
                            if !is_buy {
                                println!("ORDER BUY DESYNC");
                            }
                        }
                        Order::Sell(_sell_order) => {
                            if is_buy {
                                println!("ORDER SELL DESYNC");
                            }
                        }
                    }
                    copytrader.update_position(
                        &token_mint, 
                        sol_amount, 
                        token_amount,
                        direction,
                        &order.get_whale_address()
                    );
                    (false,0)
                } else {
                    (false,0)
                }
            } else {
                (false,0)
            };
            let whale_swap = WhaleSwap::new(
                sol_amount, 
                token_amount, 
                post_sol_reserve, 
                post_token_reserve, 
                is_buy,
                slot,
                maker,
            );
            
            if maker_is_whale && copytrader.is_ready(){
                let order = 
                    copytrader.make_trade_decision(
                        &whale_swap, 
                        pre_token_balance, 
                        pool
                    );
                if let Some(order) = order {
                    order_queues.add_order(&token_mint, order);
                }
            }
            charts.add_swap(token_mint, whale_swap, maker_is_whale);
        }
    }

    
    pub fn iter_pools<'a>(&'a self) -> impl Iterator<Item = Arc<dyn Pool>> + 'a {
        self.seen_pools.iter()
            .filter(|(_, is_price_diff)| *is_price_diff)
            .map(|(pool, _)| pool.clone())
    }
    /* 
    pub fn set_seen_pools(&mut self) {
        // Process meteoras_dlmm_changed pools
        for dlmm_impact in &self.meteoras_dlmm_changed {
            let pool = dlmm_impact.get_pool();
            if !self.seen_pools.iter().any(|(p, _)| p.get_address() == pool.get_address()) {
                self.seen_pools.push((pool, false));
            }
        }
        
        // Process raydium_clmm_changed pools
        for raydium_impact in &self.raydium_clmm_changed {
            let pool = raydium_impact.get_pool();
            if !self.seen_pools.iter().any(|(p, _)| p.get_address() == pool.get_address()) {
                self.seen_pools.push((pool, false));
            }
        }
        
        // Process constant_product_changed pools
        for cp_impact in &self.constant_product_changed {
            let pool = cp_impact.get_pool();
            if !self.seen_pools.iter().any(|(p, _)| p.get_address() == pool.get_address()) {
                self.seen_pools.push((pool, false));
            }
        }
    }*/
}

pub fn extract_indexes_for_reserves_accounts(
    account_keys: &Vec<String>,
    pool_data_vec: &Vec<(String, String, Arc<dyn Pool>)>,
    post_token_balances: &Vec<PostTokenBalance>,
    owners: &HashSet<String>,
    signature: &String,
) -> Vec<(u64, u64, bool)> {
    // Initialize result vector with None values
    let mut result = vec![(0, 0, false); pool_data_vec.len()];


    for balance in post_token_balances.iter().filter(
        |b| owners.contains(&b.owner)
    ) {
        let account_index = balance.account_index as usize;
        let account_key = account_keys.get(account_index)
            .map(|key| key.to_string());
        if let Some(key) = account_key {
            // Check if this account matches any wsol or token reserve accounts
            for (idx, (x_account, y_account, _)) in pool_data_vec.iter().enumerate() {     
                if &key == x_account{
                    if balance.ui_token_amount == 0 {
                        println!("signature : {}", signature);
                        println!("balance == 0");
                    }
                    result[idx].0 = balance.ui_token_amount;
                    result[idx].2 = true;
                } else if &key == y_account {
                    if balance.ui_token_amount == 0 {
                        println!("signature : {}", signature);
                        println!("balance == 0");
                    }
                    result[idx].1 = balance.ui_token_amount;
                    result[idx].2 = true;
                }
            }
        }
    }

    result
}