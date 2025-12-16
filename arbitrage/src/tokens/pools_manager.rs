use base64::{engine::general_purpose, Engine as _};
use log::debug;
use serde::{Deserialize, Serialize};
use serde_json::json;
use solana_client::rpc_client::RpcClient;
use solana_client::nonblocking::rpc_client::RpcClient as AsyncRpcClient;
use crossbeam_channel::{unbounded, Receiver};
use solana_sdk::{
    address_lookup_table::{state::AddressLookupTable, AddressLookupTableAccount},
    commitment_config::{CommitmentConfig, CommitmentLevel},
    hash::Hash,
    message::Message,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_instruction,
    transaction::Transaction,
};
use std::sync::atomic::{AtomicBool, Ordering};
use crossbeam_channel::Sender;
use bimap::BiMap;
use meteora::swap::get_bin_arrays_for_swap;
use crate::{
    wallets::{WalletsPool, TokenAccount},
    arbitrage_client::TrackedPubkeys,
    pools::{
        PoolInitialize,
        PoolUpdateType,
        meteora_dlmm::MeteoraDlmmPool,
        raydium_clmm::clmm::RaydiumClmmPool, 
        raydium_v4::RaydiumV4Pool, Pool,
        raydium_cpmm::RaydiumCpmmPool,
        pumpfun_amm::PumpfunAmmPool,
    },
    simulation::simulator_engine::SimulatorEngine,
    tokens::pools_searcher::{
        InitializingTokensMap,
        FarmableTokens,
        create_tracked_pubkeys_for_pairs, fetch_dlmm_pools_page, fetch_raydium_pools_page, merge_new_token_pairs_hashmap,
        fetch_pumpfun_pools_page, OptionalData
    },
    simulation::pool_impact::AmmType,
};
use spl_associated_token_account::{
    get_associated_token_address, instruction::create_associated_token_account,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;
use ureq;

pub const TOKEN_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey = spl_associated_token_account::id();
pub const MAX_RECENT_BLOCKHASHES: usize = 151;
const SOL_TOKEN_ID: &str = "solana_So11111111111111111111111111111111111111112";
const SOL_ADDRESS: &str = "So11111111111111111111111111111111111111112";
const WSOL_TOKEN_ACCOUNT: Pubkey =
    Pubkey::from_str_const("6dKxJwmc2dFSzru3DSBQk6cbMZESQ5DXUJYY39NwMk2t");

#[cfg(not(test))]
const MAX_PAGES: u32 = 10;
#[cfg(test)]
const MAX_PAGES: u32 = 1;

const BATCH_SIZE: usize = 8;


#[cfg(test)]
const RPC_URL: &str = "https://mainnet.helius-rpc.com/?api-key=";

#[cfg(not(test))]
const RPC_URL: &str = "http://0.0.0.0:8899";

/*const MAINNET_RPC_URL: &str =
    "https://solana-mainnet.core.chainstack.com/e8122c48af8083c0eee6531e86d7fbaf";*/
const MAINNET_RPC_URL: &str =
    "https://mainnet.helius-rpc.com/?api-key=";
pub const JITO_BUNDLES_URL_MAIN: &str = "https://mainnet.block-engine.jito.wtf/api/v1/bundles";
pub const JITO_BUNDLES_URL_AMS: &str = "https://amsterdam.mainnet.block-engine.jito.wtf/api/v1/bundles";
pub const JITO_BUNDLES_URL_FRA: &str = "https://frankfurt.mainnet.block-engine.jito.wtf/api/v1/bundles";
pub const JITO_BUNDLES_URL_NY: &str = "https://ny.mainnet.block-engine.jito.wtf/api/v1/bundles";
pub const JITO_BUNDLES_URL_TKY: &str = "https://tokyo.mainnet.block-engine.jito.wtf/api/v1/bundles";
pub const JITO_BUNDLES_URL_SLC: &str = "https://slc.mainnet.block-engine.jito.wtf/api/v1/bundles";

const JITO_TIP_ACCOUNT: Pubkey =
    Pubkey::from_str_const("ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49");
const JITO_TIP_AMOUNT: u64 = 50_000;

#[derive(Clone)]
pub struct RecentBlockhash {
    pub confirmed: Option<Hash>,
    pub processed: Option<Hash>,
    pub finalized: Option<Hash>,
}

#[derive(Clone)]
pub struct ConnectionManager {
    confirmed: Arc<RpcClient>,
    processed: Arc<RpcClient>,
    finalized: Arc<RpcClient>,
    pub recent_blockhash: Arc<RwLock<RecentBlockhash>>,
}
impl ConnectionManager {
    pub fn new() -> Self {
        let confirmed_client = Arc::new(RpcClient::new_with_commitment(RPC_URL.to_string(), CommitmentConfig::confirmed()));
        Self {
            confirmed: confirmed_client,
            processed: Arc::new(RpcClient::new_with_commitment(RPC_URL.to_string(), CommitmentConfig::processed())),
            finalized: Arc::new(RpcClient::new_with_commitment(RPC_URL.to_string(), CommitmentConfig::finalized())),
            recent_blockhash: Arc::new(RwLock::new(RecentBlockhash {
                confirmed: None, 
                processed: None,
                finalized: None,
            })),
        }
    }

    pub fn get_connection(&self, commitment: CommitmentLevel) -> Arc<RpcClient> {
        match commitment {
            CommitmentLevel::Confirmed => self.confirmed.clone(),
            CommitmentLevel::Processed => self.processed.clone(),
            CommitmentLevel::Finalized => self.finalized.clone(),
        }
    }

    pub fn get_recent_blockhash(&self, commitment: CommitmentLevel) -> Option<Hash> {
        match commitment {
            CommitmentLevel::Confirmed => self.recent_blockhash.read().unwrap().confirmed,
            CommitmentLevel::Processed => self.recent_blockhash.read().unwrap().processed,
            CommitmentLevel::Finalized => self.recent_blockhash.read().unwrap().finalized,
        }
    }

    pub fn update(&self) -> Result<(), Box<dyn std::error::Error>> {
        for commitment in [
            CommitmentLevel::Confirmed,
            CommitmentLevel::Processed,
            CommitmentLevel::Finalized,
        ] {
            self.fetch_recent_blockhash(commitment)?;
        }
        Ok(())
    }

    pub fn fetch_recent_blockhash(
        &self,
        commitment: CommitmentLevel,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let recent_blockhash = self.get_connection(commitment).get_latest_blockhash()?;
        match commitment {
            CommitmentLevel::Confirmed => {
                self.recent_blockhash.write().unwrap().confirmed = Some(recent_blockhash)
            }
            CommitmentLevel::Processed => {
                self.recent_blockhash.write().unwrap().processed = Some(recent_blockhash)
            }
            CommitmentLevel::Finalized => {
                self.recent_blockhash.write().unwrap().finalized = Some(recent_blockhash)
            }
        }
        Ok(())
    }

}

#[derive(Debug, Serialize, Deserialize)]
struct Token {
    address: String,
    name: String,
    symbol: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DexPool {
    dex_id: String,
    url: String,
    pair_address: String,
    base_token: Token,
    quote_token: Token,
    price_native: String,
    liquidity: Option<Liquidity>,
    labels: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct DexResponse {
    pairs: Vec<DexPool>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Liquidity {
    usd: f64,
    base: f64,
    quote: f64,
}

#[derive(Debug, Deserialize)]
struct GeckoPool {
    attributes: PoolAttributes,
    relationships: PoolRelationships,
}

#[derive(Debug, Deserialize)]
struct PoolAttributes {
    market_cap_usd: Option<String>,
    fdv_usd: String,
    reserve_in_usd: String,
}

#[derive(Debug, Deserialize)]
struct PoolRelationships {
    base_token: TokenData,
    quote_token: TokenData,
}

#[derive(Debug, Deserialize)]
struct TokenData {
    data: TokenInfo,
}

#[derive(Debug, Deserialize)]
struct TokenInfo {
    id: String,
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    data: Vec<GeckoPool>,
}

pub struct PoolCluster {
    pub ids: HashSet<u16>,
    pub is_root: bool,
    //pub amm_type: AmmType,
    //pub related_address: Arc<RwLock<Option<String>>>,
    pub meteora_dlmm_arbitrage_pool: Option<Arc<RwLock<MeteoraDlmmPool>>>,
    pub raydium_v4_arbitrage_pool: Option<Arc<RwLock<RaydiumV4Pool>>>,
    pub reserve_wsol_account: String,
    pub reserve_token_account: String,
}

#[derive(Clone)]
pub struct PoolsManager {
    pub client: reqwest::Client,
    pub connection_manager: ConnectionManager,
    pub simulator_engine: SimulatorEngine,
    pub pools_map: Arc<RwLock<HashMap<String, Arc<dyn Pool>>>>,
    pub update_pubkeys_receiver: Receiver<(String, PoolUpdateType)>,
    pub initialize_receiver: Receiver<PoolInitialize>,
    pub clear_pubkeys_receiver: Receiver<Vec<String>>,
    pub tracked_pubkeys_sender: Sender<TrackedPubkeys>,
    pub farmable_tokens: FarmableTokens,
}

impl PoolsManager {
    pub fn new(
        clear_pubkeys_receiver: Receiver<Vec<String>>,
        tracked_pubkeys_sender: Sender<TrackedPubkeys>,
        update_pubkeys_receiver: Receiver<(String, PoolUpdateType)>,
        initialize_receiver: Receiver<PoolInitialize>,
        farmable_tokens: FarmableTokens
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            connection_manager: ConnectionManager::new(),
            simulator_engine: SimulatorEngine::new(),
            pools_map: Arc::new(RwLock::new(HashMap::new())),
            initialize_receiver,
            clear_pubkeys_receiver,
            tracked_pubkeys_sender,
            update_pubkeys_receiver,
            farmable_tokens,
        }
    }

    /*
    fn does_pair_exist(&self, pool_a: &str, pool_b: &str, old_arbitrage_pair_list: &[ArbitragePair]) -> Option<ArbitragePair> {
        old_arbitrage_pair_list.iter()
            .find(|pair| {
                (pair.pool_a.to_string() == pool_a && pair.pool_b.to_string() == pool_b) ||
                (pair.pool_a.to_string() == pool_b && pair.pool_b.to_string() == pool_a)
            })
            .cloned()
    }
    */

    pub async fn clear_tracked_pubkeys(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut pubkeys_str = match self.clear_pubkeys_receiver.try_recv() {
            Ok(str) => str,
            Err(_) => return Ok(()),
        };

        
        
        for pubkey_vec in self.clear_pubkeys_receiver.try_iter() {
            pubkeys_str.extend(pubkey_vec);
        }

        let mut pools_map = self.pools_map.write().unwrap();
        for pubkey in &pubkeys_str {
            pools_map.remove(pubkey);
        }

        let tracked_pubkeys_to_send = TrackedPubkeys::new_remove(pubkeys_str);
        let _ = self.tracked_pubkeys_sender.send(tracked_pubkeys_to_send);
        
        
        Ok(())
    }

    pub async fn update_pools(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut pubkeys_to_update = match self.update_pubkeys_receiver.try_recv() {
            Ok(str) => vec![str],
            Err(_) => return Ok(()),
        };

        pubkeys_to_update.extend(self.update_pubkeys_receiver.try_iter());
        let connection = Arc::new(AsyncRpcClient::new(RPC_URL.to_string()));

        
        for (pubkey, pool_update_type) in pubkeys_to_update {
            let pool = {
                let pools_map_read = self.pools_map.read().unwrap();
                pools_map_read.get(&pubkey).cloned()
            };
            if let Some(pool) = pool {
                match pool_update_type {
                    PoolUpdateType::NeedLeftBins => {
                        
                        let bin_arrays_for_swap = get_bin_arrays_for_swap(
                            connection.clone(),
                            &Pubkey::from_str_const(&pubkey),
                            true
                        ).await;
                        if let Ok(bin_arrays_for_swap) = bin_arrays_for_swap {
                            
                            let meteora_pool = pool.as_meteora_dlmm().unwrap();
                            let (pubkeys_to_remove, pubkeys_to_add) = 
                                meteora_pool.set_left_bins(bin_arrays_for_swap.clone());
                            
                            
                            let mut pools_map_write = self.pools_map.write().unwrap();
                            
                            if !pubkeys_to_remove.is_empty() {
                                for pubkey in &pubkeys_to_remove {
                                    pools_map_write.remove(pubkey);
                                }
                                let tracked_pubkeys_to_remove = TrackedPubkeys::new_remove(pubkeys_to_remove);
                                let _ = self.tracked_pubkeys_sender.send(tracked_pubkeys_to_remove);
                            }

                            if !bin_arrays_for_swap.is_empty() {
                                for pubkey in &pubkeys_to_add {
                                    pools_map_write.insert(pubkey.to_string(), pool.clone());
                                }
                                let tracked_pubkeys_to_send = TrackedPubkeys::new_add(pubkeys_to_add);
                                let _ = self.tracked_pubkeys_sender.send(tracked_pubkeys_to_send);
                            }
                            
                        }
                    }
                    PoolUpdateType::NeedRightBins => {
                        let bin_arrays_for_swap = get_bin_arrays_for_swap(
                            connection.clone(),
                            &Pubkey::from_str_const(&pubkey),
                            false
                        ).await;
                        if let Ok(bin_arrays_for_swap) = bin_arrays_for_swap {        
                            let meteora_pool = pool.as_meteora_dlmm().unwrap();
                            let (pubkeys_to_remove, pubkeys_to_add) = meteora_pool.set_right_bins(bin_arrays_for_swap.clone());
                            
                            let mut pools_map_write = self.pools_map.write().unwrap();

                            if !pubkeys_to_remove.is_empty() {
                                for pubkey in &pubkeys_to_remove {
                                    pools_map_write.remove(pubkey);
                                }
                                let tracked_pubkeys_to_remove = TrackedPubkeys::new_remove(pubkeys_to_remove);
                                let _ = self.tracked_pubkeys_sender.send(tracked_pubkeys_to_remove);
                            }

                            if !bin_arrays_for_swap.is_empty() {
                                for pubkey in &pubkeys_to_add {
                                    pools_map_write.insert(pubkey.to_string(), pool.clone());
                                }
                                let tracked_pubkeys_to_send = TrackedPubkeys::new_add(pubkeys_to_add);
                                let _ = self.tracked_pubkeys_sender.send(tracked_pubkeys_to_send);
                            }
                            
                        }
                    }
                    PoolUpdateType::NeedLeftTickArrayState => {
                        if let Ok((pubkeys_to_remove, pubkeys_to_add)) = 
                            pool.as_raydium_clmm().unwrap().extend_left_tick_array_states(&connection).await {
                        
                            let mut pools_map_write = self.pools_map.write().unwrap();

                            if !pubkeys_to_remove.is_empty() {
                                for pubkey in &pubkeys_to_remove {
                                    pools_map_write.remove(pubkey);
                                }
                                let tracked_pubkeys_to_remove = TrackedPubkeys::new_remove(pubkeys_to_remove);
                                let _ = self.tracked_pubkeys_sender.send(tracked_pubkeys_to_remove);
                            }

                            if !pubkeys_to_add.is_empty() {
                                for pubkey in &pubkeys_to_add {
                                    pools_map_write.insert(pubkey.to_string(), pool.clone());
                                }
                                let tracked_pubkeys_to_send = TrackedPubkeys::new_add(pubkeys_to_add);
                                let _ = self.tracked_pubkeys_sender.send(tracked_pubkeys_to_send);
                            }
                        }
                    }
                    PoolUpdateType::NeedRightTickArrayState => {
                        if let Ok((pubkeys_to_remove, pubkeys_to_add)) = 
                            pool.as_raydium_clmm().unwrap().extend_right_tick_array_states(&connection).await {

                                let mut pools_map_write = self.pools_map.write().unwrap();

                                if !pubkeys_to_remove.is_empty() {
                                    for pubkey in &pubkeys_to_remove {
                                        pools_map_write.remove(pubkey);
                                    }
                                    let tracked_pubkeys_to_remove = TrackedPubkeys::new_remove(pubkeys_to_remove);
                                    let _ = self.tracked_pubkeys_sender.send(tracked_pubkeys_to_remove);
                                }

                                if !pubkeys_to_add.is_empty() {
                                    for pubkey in &pubkeys_to_add {
                                        pools_map_write.insert(pubkey.to_string(), pool.clone());
                                    }
                                    let tracked_pubkeys_to_send = TrackedPubkeys::new_add(pubkeys_to_add);
                                    let _ = self.tracked_pubkeys_sender.send(tracked_pubkeys_to_send);
                                }
                            }
                    }
                }
            }
        }
        
        
        Ok(())
    }

    pub async fn initialize_pools(
        &self,
    ) -> Result<(u64, u64), Box<dyn std::error::Error + Send + Sync>> {
        let connection = AsyncRpcClient::new_with_commitment("http://0.0.0.0:8899".to_string(), CommitmentConfig::confirmed());
        
        let mut pool_initializes_with_accounts = Vec::new();
        let mut to_initialize_count = 0;
        let mut initialized_count = 0;
        
        for pool_initialize in self.initialize_receiver.try_iter() {
            to_initialize_count += pool_initialize.accounts_to_init.len() as u64;
            let pubkeys: Vec<Pubkey> = pool_initialize.accounts_to_init.iter()
                .map(|(pubkey_str, _)| Pubkey::from_str_const(pubkey_str))
                .collect();
            
            if !pubkeys.is_empty() {
                let accounts = connection.get_multiple_accounts(&pubkeys).await?;
                pool_initializes_with_accounts.push((pool_initialize, accounts));
                
                // Wait 20ms before the next batch call
                tokio::time::sleep(Duration::from_millis(20)).await;
            } else {
                pool_initializes_with_accounts.push((pool_initialize, vec![]));
            }
        }
        
        // Process the pool initializations with their accounts
        let pools_map = self.pools_map.read().unwrap();
        for (pool_initialize, accounts) in pool_initializes_with_accounts.into_iter() {
            
            for (i, (pubkey_str, amm_type)) in pool_initialize.accounts_to_init.into_iter().enumerate() {
                if let Some(Some(account)) = accounts.get(i) {
                    initialized_count += 1;
                    let pool = pools_map.get(&pool_initialize.pool).unwrap();
                    match amm_type {
                        AmmType::MeteoraDlmmPool => {
                            let meteora_pool = pool.as_meteora_dlmm().unwrap();
                            meteora_pool.update_liquidity_state_from_data(&account.data, pool_initialize.account_write_number);
                        }
                        AmmType::MeteoraDlmmBinArray => {
                            let meteora_pool = pool.as_meteora_dlmm().unwrap();
                            meteora_pool.update_bin_container_from_data(pubkey_str.clone(), &account.data, pool_initialize.account_write_number);
                        }
                        AmmType::RaydiumClmmPool => {
                            let raydium_pool = pool.as_raydium_clmm().unwrap();
                            raydium_pool.update_liquidity_state_from_data(&account.data, pool_initialize.account_write_number);
                        }
                        AmmType::RaydiumClmmTickArray => {
                            let raydium_pool = pool.as_raydium_clmm().unwrap();
                            raydium_pool.update_tick_array_from_data(pubkey_str.clone(), &account.data, pool_initialize.account_write_number);
                        }
                        AmmType::RaydiumClmmBitmapExt => {
                            let raydium_pool = pool.as_raydium_clmm().unwrap();
                            raydium_pool.update_tick_array_bitmap_extension_from_data(&account.data, pool_initialize.account_write_number);
                        }
                        _ => continue,
                    }

                }
                
            }
            
            
        }

        Ok((to_initialize_count, initialized_count))
    }

    pub async fn search_new_arbitrage_pairs(
        &self, 
        wallets: Arc<WalletsPool>,
        tracked_pubkeys_sender: Arc<Sender<TrackedPubkeys>>
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        println!("Searching for new arbitrage pairs");
        let connection = Arc::new(AsyncRpcClient::new(RPC_URL.to_string()));
        let helius_connection = Arc::new(AsyncRpcClient::new_with_timeout(MAINNET_RPC_URL.to_string(), Duration::from_secs(30)));
        let time = std::time::Instant::now();
        let filtered_pools = self.find_new_pairs(helius_connection).await?;
        println!("Find new pairs in {:?}", time.elapsed());
        let time = std::time::Instant::now();
        let tracked_pubkeys_to_send = self.initialize_new_pairs(
            connection,
            &wallets,
            &filtered_pools
        ).await?;
        println!("Initialize new pairs in {:?}", time.elapsed());
        let _ = tracked_pubkeys_sender.send(tracked_pubkeys_to_send);
        Ok(())
    }

    // Helper function to collect mints that need to be initialized
    fn collect_mints_to_initialize(
        mints_pool: &mut HashMap<String, Vec<Arc<dyn Pool>>>,
        pools: &Vec<Arc<dyn Pool>>,
        token_accounts: &HashMap<String, Arc<TokenAccount>>,
        mints_to_initialize: &mut HashSet<String>
    ) {
        // Get mints that need to be initialized from the pool
        let pool_mints: Vec<(Arc<dyn Pool>, Vec<String>)> = pools.iter().map(|pool| (pool.clone(), pool.get_mints_to_initialize())).collect();
        
        // Add each mint to the hashset if it's not already in token_accounts
        for (pool, mints) in pool_mints {
            for mint in mints {
                if !token_accounts.contains_key(&mint) {
                    mints_to_initialize.insert(mint.clone());
                }
                mints_pool.entry(mint.clone()).or_insert(vec![]).push(pool.clone());
            }
        }
    }

    pub async fn initialize_new_pairs(
        &self,
        connection: Arc<AsyncRpcClient>,
        wallets: &Arc<WalletsPool>,
        initializing_tokens_map: &InitializingTokensMap,
    ) -> Result<TrackedPubkeys, Box<dyn std::error::Error + Send + Sync>> {
        let mut mint_to_pools: HashMap<String, Vec<Arc<dyn Pool>>> = HashMap::new();
        
        // Initialize a hashset to collect all mints that need to be initialized
        let mut mints_to_initialize: HashSet<String> = HashSet::new();
        
        
        for (_token_mint, pools) in &initializing_tokens_map.tokens {
            let mut initalized_pools: Vec<Arc<dyn Pool>> = Vec::new();

            for non_initialized_pool in &pools.0 {
                let pubkey = Pubkey::from_str_const(&non_initialized_pool.address);
                match non_initialized_pool.amm_type {
                    AmmType::MeteoraDlmmPool => {
                        if let Some(pool) = MeteoraDlmmPool::new(
                            pubkey,
                            connection.clone(),
                            non_initialized_pool.farm_token_label.clone(),
                        ).await {
                            initalized_pools.push(Arc::new(pool)) ;
                        }
                    },
                    AmmType::RaydiumV4Pool => {
                        if let Some(pool) = RaydiumV4Pool::new(
                            pubkey,
                            connection.clone(),
                            non_initialized_pool.farm_token_label.clone(),
                        ).await {
                            initalized_pools.push(Arc::new(pool));
                        }
                    },
                    AmmType::RaydiumCpmmPool => {
                        if let Some(pool) = RaydiumCpmmPool::new(
                            pubkey,
                            connection.clone(),
                            non_initialized_pool.farm_token_label.clone(),
                        ).await {
                            initalized_pools.push(Arc::new(pool));
                        }
                    },
                    AmmType::PumpfunAmmPool => {
                        if let Some(OptionalData::Pumpfun(pumpfun_data)) = non_initialized_pool.optional_data.as_ref() {
                            if let Some(pool) = PumpfunAmmPool::new(
                                    pubkey,
                                    pumpfun_data.clone(),
                                    non_initialized_pool.farm_token_label.clone(),
                            ) {
                                initalized_pools.push(Arc::new(pool));
                                continue;
                            }
                        }
                    },
                    AmmType::RaydiumClmmPool => {
                        if let Some(pool) = RaydiumClmmPool::new(
                            pubkey,
                            connection.clone(),
                            non_initialized_pool.farm_token_label.clone(),
                        ).await {
                            initalized_pools.push(Arc::new(pool));
                        }
                    },
                    _ => continue,
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            
            let base_wallet = wallets.get_wallet(0);
            let base_wallet_token_accounts = &base_wallet.token_accounts.read().unwrap().token_accounts;

            if !initalized_pools.is_empty() && pools.1 + initalized_pools.len() >= 2{
                PoolsManager::collect_mints_to_initialize(
                    &mut mint_to_pools, 
                    &initalized_pools, 
                    &base_wallet_token_accounts, 
                    &mut mints_to_initialize
                );
            }
        }

        println!("Pairs initialized !");
        
        let _ = wallets.initalize_all_with_mints(connection, &mints_to_initialize.iter().cloned().collect()).await;

        println!("Ata Initialized !");
        let new_pools = self.simulator_engine.update_with_new_pairs(
            &mint_to_pools
        ).await?;

        let mut pools_map = self.pools_map.write().unwrap();
        let new_tracked_pubkeys = create_tracked_pubkeys_for_pairs(&new_pools, &mut pools_map);
        let tracked_pubkeys_to_send= TrackedPubkeys::new_add(new_tracked_pubkeys);
        println!("Tracked pubkeys sent !");
        Ok(tracked_pubkeys_to_send)
    }

    pub async fn find_new_pairs(
        &self,
        connection: Arc<AsyncRpcClient>,
    ) -> Result<InitializingTokensMap, Box<dyn std::error::Error + Send + Sync>> {
        
        // Fetch both pools simultaneously
        let farmable_tokens = &self.farmable_tokens;
        let (meteora_pools, raydium_pools, pumpfun_pools) = tokio::try_join!(
            fetch_dlmm_pools_page(0, farmable_tokens),
            fetch_raydium_pools_page(1, farmable_tokens),
            fetch_pumpfun_pools_page(connection.clone(), farmable_tokens)
        )?;

        let current_pairs = self.pools_map.read().unwrap();

        // Filter Meteora pools
        let filtered_map = merge_new_token_pairs_hashmap(
            vec![
                meteora_pools,
                raydium_pools.0,
                pumpfun_pools
            ], 
            &current_pairs
        );

        Ok(filtered_map)
    }
}

#[derive(Clone)]
pub struct CustomBlockHashQueue {
    block_hash_queue: Arc<RwLock<VecDeque<(u64, Hash)>>>,
    block_hash_to_use: Arc<RwLock<Option<Hash>>>,
}

impl CustomBlockHashQueue {
    pub fn new() -> Self {
        Self {
            block_hash_queue: Arc::new(RwLock::new(VecDeque::with_capacity(MAX_RECENT_BLOCKHASHES))),
            block_hash_to_use: Arc::new(RwLock::new(None)),
        }
    }

    pub fn push(&self, last_valid_block_height: u64, hash: Hash) {
        if let Ok(mut queue) = self.block_hash_queue.write() {
            // Add new element to the back
            queue.push_back((last_valid_block_height, hash));
            
            // Remove from front if queue exceeds MAX_RECENT_BLOCKHASHES
            if queue.len() > MAX_RECENT_BLOCKHASHES {
                queue.pop_front();
            }
        }
    }

    pub fn push_and_get_blockhash(&self, last_valid_block_height: u64, hash: Hash, current_block_height: u64) -> Option<Hash> {
        if let Ok(mut queue) = self.block_hash_queue.write() {
            // Add new element to the back
            if let Some(last_elem) = queue.back() {
                if last_elem.0 < last_valid_block_height { 
                    queue.push_back((last_valid_block_height, hash));
                }
            }
            else {
                queue.push_back((last_valid_block_height, hash));
            }
            
            
            // Remove from front if queue exceeds MAX_RECENT_BLOCKHASHES
            if queue.len() > MAX_RECENT_BLOCKHASHES {
                queue.pop_front();
            }

            let target_block_height = current_block_height + 10;
            for (block_height, blockhash) in queue.iter() {
                if *block_height == target_block_height {
                    return Some(*blockhash);
                }
            }
            return Some(queue.back().unwrap().1);
        }
        None
    }
    pub async fn update(&self, rpc_client: Arc<AsyncRpcClient>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        
        {
            let (blockhash_result, block_height_result) = tokio::join!(
                rpc_client.get_latest_blockhash_with_commitment(CommitmentConfig::finalized()),
                rpc_client.get_block_height_with_commitment(CommitmentConfig::processed())
            );
            
            let (fetched_blockhash, last_valid_block_height) = blockhash_result.unwrap();
            let block_height = block_height_result.unwrap();
            
            // Create a new scope for the write lock
            let mut block_hash_to_use_guard = self.block_hash_to_use.write().unwrap();
            *block_hash_to_use_guard = self.push_and_get_blockhash(last_valid_block_height, fetched_blockhash, block_height);     
        } // Lock is dropped here



        Ok(())
    }

    pub fn get_blockhash(&self) -> Option<Hash> {
        self.block_hash_to_use.read().unwrap().clone()
    }

    pub fn len(&self) -> usize {
        self.block_hash_queue.read().map(|queue| queue.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.block_hash_queue.read().map(|queue| queue.is_empty()).unwrap_or(true)
    }

    pub fn get(&self, index: usize) -> Option<(u64, Hash)> {
        self.block_hash_queue
            .read()
            .ok()
            .and_then(|queue| queue.get(index).cloned())
    }
}