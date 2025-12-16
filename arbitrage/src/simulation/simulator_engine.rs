use {
    rand::{thread_rng, Rng},
    crate::{
        simulation::pool_impact::AmmType,
        
        pools::{
            price_maths::price_differ,
            meteora_dlmm::{
                bin_id_to_bin_array_index, Bin, MeteoraDlmmPool
            }, 
            raydium_v4::{
                RaydiumV4Pool,
            },
            ArbitrageType,
            Pool,
        }, 
        simulation::simulator_safe_maths::*, 
        tokens::pools_manager::{
            CustomBlockHashQueue,
            ConnectionManager
        }
    }, solana_client::{
        nonblocking::rpc_client::RpcClient as AsyncRpcClient, rpc_client::RpcClient
    }, solana_sdk::{
        commitment_config::{CommitmentConfig, CommitmentLevel},
        hash::Hash as BlockHash,
        address_lookup_table::AddressLookupTableAccount,
        clock::Slot, pubkey::Pubkey, signature::{
            Keypair, Signature, Signer
        }, system_instruction, transaction::{Transaction, VersionedTransaction}
    }, std::{collections::{HashMap, HashSet}, sync::{atomic::{AtomicBool, Ordering}, Arc, RwLock}}
};


pub const MAX_SHIFT: u128 = 1 << 64;
const MAX_SHIFT_CONST: u128 = 64;
const SQRT_MAX_SHIFT: u64 = 1 << 32;
const WALLET_PUBLIC_KEY_STR: &str = "2S4SJ9Ffyuvu246xc54buoJg6gZ7wQePoKFwSA3X7Vt3";


#[derive(Debug)]
pub struct PoolEntry {
    pub pool: Arc<dyn Pool>,
    pub quote_is_mint: bool,
}
#[derive(Debug)]
pub struct PoolStore {
    pub pools: Vec<PoolEntry>,
}

impl PoolStore {
    pub fn new() -> Self {
        Self {
            pools: Vec::new(),
        }
    }

    pub fn new_with_pools(pools: Vec<Arc<dyn Pool>>, mint: &String) -> Self {
        let mut pool_store = Self::new();
        for pool in pools {
            pool_store.pools.push(PoolEntry {
                pool: pool.clone(),
                quote_is_mint: pool.get_mint_y() == *mint,
            });
        }
        pool_store
    }

    pub fn add_pool(&mut self, pool: Arc<dyn Pool>, mint: &String) {
        self.pools.push(PoolEntry {
            pool: pool.clone(),
            quote_is_mint: pool.get_mint_y() == *mint,
        });
    }

    pub fn add_pools(&mut self, pools: Vec<Arc<dyn Pool>>, mint: &String) {
        self.pools.reserve(pools.len());
        for pool in pools {
            self.add_pool(pool, mint);
        }
    }
}


#[derive(Debug)]
pub struct MintPools {
    pub mint: String,
    pub pools: PoolStore,
}


#[derive(Clone)]
pub struct SimulatorEngine {
    pub mint_pools: Arc<RwLock<HashMap<String, Arc<RwLock<MintPools>>>>>,
}




impl SimulatorEngine {
    pub fn new() -> Self {
        Self {
            mint_pools: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    pub fn get_related_mint_pools(&self, mint: &String) -> Option<Arc<RwLock<MintPools>>> {
        Some(self.mint_pools.read().unwrap().get(mint)?.clone())

    }

    pub async fn update_with_new_pairs(
        &self, 
        new_pairs: &HashMap<String, Vec<Arc<dyn Pool>>>
    ) -> Result<Vec<Arc<dyn Pool>>, Box<dyn std::error::Error + Send + Sync>> {

        let mut new_pools = Vec::with_capacity(new_pairs.len());
        let mut mint_pools = self.mint_pools.write().unwrap();
        
        
        for (mint, pools) in new_pairs {
            mint_pools.entry(mint.clone())
                .and_modify(|mint_pool| {
                    let mut mp_guard = mint_pool.write().unwrap();
                    mp_guard.pools.add_pools(pools.clone(), mint);
                })
                .or_insert_with(|| {
                    Arc::new(RwLock::new(MintPools {
                        mint: mint.clone(),
                        pools: PoolStore::new_with_pools(pools.clone(), mint),
                }))
            });
            for pool in pools {
                new_pools.push(pool.clone());
            }
        }
        Ok(new_pools)
    }

    pub fn get_current_tokens(&self) -> HashSet<String> {
        self.mint_pools.read().unwrap().keys().cloned().collect()
    }
}

