use {
    serde::Deserialize,
    std::collections::{
        HashSet,
        HashMap
    },
    futures::future::join_all,
    std::sync::{
        Arc,
        RwLock,
        atomic::AtomicBool,
    },
    crossbeam_channel::Sender,
    solana_sdk::{
        pubkey::Pubkey,
        transaction::Transaction,
        signature::{Keypair, Signer},

    },
    spl_associated_token_account::{
        get_associated_token_address,
        instruction::create_associated_token_account,
    },
    solana_client::nonblocking::rpc_client::RpcClient as AsyncRpcClient,
    crate::{pools::{
        Pool,
        TokenLabel,
        pumpfun_amm::PumpfunAmmPoolInfo,
    }, simulation::pool_impact::AmmType, pools::pumpfun_amm::PUMPFUN_AMM_ID},
};

const BIRDEYE_BASE_URL: &str ="https://public-api.birdeye.so/defi/tokenlist";
const MIN_LIQUIDITY: f64 = 350.0;
const MIN_MC: f64 = 100_000.0;
const MAX_MC: f64 = 200_000_000_000.0;
const METEORA_BASE_URL: &str = "https://dlmm-api.meteora.ag/pair/all_by_groups";
const WSOL_ADDRESS: &str = "So11111111111111111111111111111111111111112";
const RAYDIUM_BASE_URL: &str = "https://api-v3.raydium.io/pools/info/list";
const MAX_BIRDEYE_PAGES: u16 = 250;
const RAYDIUM_API_BASE_URL: &str = "https://api-v3.raydium.io/pools/info/list";
const TOKEN_PROGRAM_STR: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const MIN_DAY_VOLUME: f64 = 50.0;
#[derive(Debug, Deserialize)]
struct BirdeyeResponse {
    success: bool,
    data: TokenData,
}

#[derive(Debug, Deserialize)]
struct TokenData {
    #[serde(rename = "updateUnixTime")]
    update_unix_time: u64,
    #[serde(rename = "updateTime")]
    update_time: String,
    tokens: Vec<TokenInfo>,
    total: u32,
}

#[derive(Debug, Deserialize)]
struct TokenInfo {
    address: String,
    decimals: u8,
    #[serde(rename = "lastTradeUnixTime")]
    last_trade_unix_time: u64,
    liquidity: f64,
    #[serde(rename = "logoURI")]
    logo_uri: Option<String>,
    mc: Option<f64>,
    name: String,
    symbol: String,
    #[serde(rename = "v24hChangePercent")]
    v24h_change_percent: Option<f64>,
    #[serde(rename = "v24hUSD")]
    v24h_usd: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct MeteoraResponse {
    groups: Vec<PoolGroup>,
    total: u64,
}

#[derive(Debug, Deserialize)]
struct PoolGroup {
    name: String,
    pairs: Vec<DlmmPoolFetch>,
}

impl PoolGroup {
    pub fn get_highest_volume(&self) -> f64 {
        if let Some(first_pair) = self.pairs.first() {
            return first_pair.trade_volume_24h;
        }
        return 0.0;
    }
}

#[derive(Debug, Deserialize)]
struct DlmmPoolFetch {
    address: String,
    name: String,
    mint_x: String,
    mint_y: String,
    liquidity: String,
    trade_volume_24h: f64,
}

#[derive(Debug, Deserialize)]
struct RaydiumResponse {
    success: bool,
    data: RaydiumData,
}

#[derive(Debug, Deserialize)]
struct RaydiumData {
    count: u32,
    data: Vec<PoolInfo>,
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
}

#[derive(Debug, Deserialize)]
struct PoolInfo {
    #[serde(rename = "programId")]
    program_id: String,
    id: String,
    #[serde(rename = "mintA")]
    mint_a: MintInfo,
    #[serde(rename = "mintB")]
    mint_b: MintInfo,
    #[serde(rename = "mintAmountA")]
    mint_amount_a: f64,
    #[serde(rename = "mintAmountB")]
    mint_amount_b: f64,
    #[serde(rename = "type")]
    pool_type: String,
    day: RaydiumApiDayStats,
}
impl PoolInfo {
    pub fn get_type(&self) -> AmmType {
        match self.pool_type.as_str() {
            "Standard" => {
                if self.program_id == "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C" {
                    AmmType::RaydiumCpmmPool
                }
                else if self.program_id == "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8" {
                    AmmType::RaydiumV4Pool
                }
                else {
                    AmmType::NotSupported
                }
            }
            "Concentrated" => AmmType::RaydiumClmmPool,
            _ => AmmType::NotSupported,
        }
    }
}

#[derive(Debug, Deserialize)]
struct MintInfo {
    address: String,
    #[serde(rename = "programId")]
    program_id: String,
}

#[derive(Debug, Deserialize)]
struct RaydiumApiResponse {
    success: bool,
    data: RaydiumApiData,
}

#[derive(Debug, Deserialize)]
struct RaydiumApiData {
    count: u32,
    data: Vec<RaydiumApiPool>,
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
}

#[derive(Debug, Deserialize)]
struct RaydiumApiPool {
    #[serde(rename = "type")]
    pool_type: String,
    id: String,
    #[serde(rename = "mintA")]
    mint_a: RaydiumApiMint,
    #[serde(rename = "mintB")]
    mint_b: RaydiumApiMint,
    day: RaydiumApiDayStats,
}



#[derive(Debug, Deserialize)]
struct RaydiumApiMint {
    address: String,
    #[serde(rename = "programId")]
    program_id: String,
}

#[derive(Debug, Deserialize)]
struct RaydiumApiDayStats {
    volume: f64,
}

pub async fn fetch_tokens_by_page(current_tokens: &HashSet<String>, page: u16, min_liquidity: u64) -> Result<HashSet<String>, Box<dyn std::error::Error>> {
    let url = format!("{}?sort_by=v24hUSD&sort_type=desc&offset={}&min_liquidity={}", BIRDEYE_BASE_URL, page, min_liquidity);
    let client = reqwest::Client::new();
    let response = client.get(url)
        .header("X-API-KEY", "567db4ce3a9d4858a8387a1fa915504e")
        .header("accept", "application/json")
        .send()
        .await?;
    
    let birdeye_response: BirdeyeResponse = response.json().await?;
    if !birdeye_response.success {
        return Err("Birdeye API request was not successful".into());
    }
    
    Ok(birdeye_response.data.tokens.into_iter()
        .filter(|token| {
            token.liquidity >= MIN_LIQUIDITY && 
            token.mc.map_or(false, |mc| mc >= MIN_MC && mc <= MAX_MC) &&
            !current_tokens.contains(&token.address)
        })
        .map(|token| token.address)
        .collect())
}

pub async fn fetch_dlmm_pools_page(
    page: u32,
    farmable_tokens: &FarmableTokens,
) -> Result<InitializingTokensMap, Box<dyn std::error::Error + Send + Sync>> {
    let url = format!(
        "{}?limit=200&sort_key=volume&page={}",
        METEORA_BASE_URL, page
    );

    let client = reqwest::Client::new();
    let response = client.get(&url)
        .send()
        .await?;

    let meteora_response: MeteoraResponse = response.json().await?;

    let mut initializing_tokens_map = InitializingTokensMap::new();

    let mut min_volume = 1_000_000.0;
    let mut stop = false;
    
    for group in meteora_response.groups {
        min_volume = group.get_highest_volume();
        if min_volume == 0.0 {
            return Ok(initializing_tokens_map);
        }
        for pair in group.pairs {

            /*if !pair.address.contains("HZErmEhFdPtEv8miyRNJ6YYDCJVTUDQ8vb6b9gYV1pAY") {
                continue;
            }
            else {
                println!("pair: {}", pair.address);
                stop = true;
            }*/

            if pair.trade_volume_24h == 0.0 {
                break;
            }

            if pair.liquidity.parse::<f64>().unwrap_or(0.0) < MIN_LIQUIDITY {
                continue;
            }

            let farm_token_label = farmable_tokens.choose_farmable_tokens(&pair.mint_x, &pair.mint_y);
            
            /*if pair.mint_x != "2wzVMXhLypmP92mXNCq4fuFcd9TCC972AbMfuiH3pump"&& pair.mint_y != "2wzVMXhLypmP92mXNCq4fuFcd9TCC972AbMfuiH3pump" {
                continue;  
            }*/


            initializing_tokens_map.add_with_farm_token_label(
                &pair.mint_x, 
                &pair.mint_y, 
                pair.address.clone(),
                AmmType::MeteoraDlmmPool, 
                farm_token_label, 
                None
            );
        }
    }
    // If we haven't reached the total, recursively fetch next pages
    if min_volume > 0.0 && !stop{
        let next_page_future = Box::pin(fetch_dlmm_pools_page(
            page + 1, 
            farmable_tokens
        ));
        let next_page_response = next_page_future.await?;
        
        // Merge the responses, appending vectors for duplicate keys
        for (key, value) in next_page_response.tokens {
            initializing_tokens_map.tokens.entry(key)
                .and_modify(|(e, _count)| e.extend(value.0.clone()))
                .or_insert(value);
        }
    }

    Ok(initializing_tokens_map)
}

pub async fn fetch_raydium_pools_page(
    page: u32, farmable_tokens: &FarmableTokens
) -> Result<(InitializingTokensMap, f64), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!(
        "{}?poolType=all&poolSortField=volume24h&sortType=desc&pageSize=1000&page={}",
        RAYDIUM_BASE_URL, page
    );

    let client = reqwest::Client::new();
    let response_to_json = client.get(&url)
        .send()
        .await?;
    
    // Clone the response body before consuming it
    let text = response_to_json.text().await?;
    // Parse the cloned text
    let response: RaydiumResponse = match serde_json::from_str(&text) {
        Ok(response) => response,
        Err(e) => {
            println!("Error parsing Raydium response: {}", e);
            return Err("Raydium API request was not successful".into());
        }
    };

    if !response.success {
        return Err("Raydium API request was not successful".into());
    }

    let mut initializing_tokens_map = InitializingTokensMap::new();
    let mut min_volume = 1_000_000.0;
    // Process current page
    let mut stop= false;
    for pool in response.data.data {
        /*if !pool.id.contains("9fmdkQipJK2teeUv53BMDXi52uRLbrEvV38K8GBNkiM7") {
            continue;
        }
        else {
            println!("pool: {}", pool.id);
            stop = true;
        }*/
        let mint_x = pool.mint_a.address.clone();
        let mint_y = pool.mint_b.address.clone();
        let (program_id_x, program_id_y) = (pool.mint_a.program_id.clone(), pool.mint_b.program_id.clone());

        min_volume = pool.day.volume;
        if min_volume < MIN_DAY_VOLUME {return Ok((initializing_tokens_map, min_volume));}

        if program_id_x == TOKEN_PROGRAM_STR && program_id_y == TOKEN_PROGRAM_STR {
            let pool_type = pool.get_type();
            if pool_type == AmmType::NotSupported || pool_type == AmmType::RaydiumClmmPool{
                continue;
            }
            let farm_token_label = farmable_tokens.choose_farmable_tokens(&mint_x, &mint_y);

            /*if mint_x != "2wzVMXhLypmP92mXNCq4fuFcd9TCC972AbMfuiH3pump"&& mint_y != "2wzVMXhLypmP92mXNCq4fuFcd9TCC972AbMfuiH3pump" {
                continue;  
            }*/

            initializing_tokens_map.add_with_farm_token_label(
                &mint_x, 
                &mint_y, 
                pool.id.clone(), 
                pool_type, 
                farm_token_label, 
                None
            );
        }
    }

    // Handle next page if it exists
    if response.data.has_next_page && !stop{
        let next_page_future = Box::pin(
            fetch_raydium_pools_page(page + 1, farmable_tokens)
        );
        let next_page_pools = next_page_future.await?;
        for (key, value) in next_page_pools.0.tokens {
            initializing_tokens_map.tokens.entry(key)
                .and_modify(|(e, count)| {
                    e.extend(value.0.clone());
                    *count = value.1;
                })
                .or_insert(value);
        }
    }

    Ok((initializing_tokens_map, min_volume))
}

pub async fn fetch_pumpfun_pools_page(
    connection: Arc<AsyncRpcClient>,
    farmable_tokens: &FarmableTokens
) -> Result<(InitializingTokensMap), Box<dyn std::error::Error + Send + Sync>> {
    let program_accounts = connection.get_program_accounts(&PUMPFUN_AMM_ID).await;
    if let Err(e) = &program_accounts {
        println!("Error fetching pumpfun pools: {:?}", e);
    }
    let program_accounts = program_accounts.unwrap();
    let mut initializing_tokens_map = InitializingTokensMap::new();
    for (pubkey, account) in program_accounts {
        if account.data.len() != 211 && account.data.len() != 300 {
            continue;
        }
        let pool_info = if let Some(pool_info) = PumpfunAmmPoolInfo::new(&pubkey, &account.data) {
            pool_info
        } else {
            continue;
        };
        let base_mint = pool_info.keys.vec[4].pubkey.to_string();
        let quote_mint = pool_info.keys.vec[5].pubkey.to_string();
        let farm_token_label = farmable_tokens.choose_farmable_tokens(
            &base_mint,  
            &quote_mint
        );

        /*if base_mint != "2wzVMXhLypmP92mXNCq4fuFcd9TCC972AbMfuiH3pump" && quote_mint != "2wzVMXhLypmP92mXNCq4fuFcd9TCC972AbMfuiH3pump" {
            continue;  
        }*/
        initializing_tokens_map.add_with_farm_token_label(
            &base_mint, 
            &quote_mint, 
            pubkey.to_string(), 
            AmmType::PumpfunAmmPool, 
            farm_token_label, 
            Some(OptionalData::Pumpfun(pool_info))
        );
    }
    Ok(initializing_tokens_map)    
}


pub fn create_tracked_pubkeys_for_pairs(
    pools: &Vec<Arc<dyn Pool>>,
    pools_map: &mut HashMap<String, Arc<dyn Pool>>,
) -> Vec<String> {
    let mut tracked_pubkeys = Vec::new();
    for pool in pools {
        let pool_address = pool.get_address();
        let pool_associated_pubkeys = pool.get_associated_tracked_pubkeys();
        tracked_pubkeys.push(pool_address.clone());
        pools_map.insert(pool_address, pool.clone());
        for pubkey in pool_associated_pubkeys {
            tracked_pubkeys.push(pubkey.clone());
            pools_map.insert(pubkey.clone(), pool.clone());
        }
    }
    tracked_pubkeys
}

pub fn merge_new_token_pairs_hashmap(
    hashmaps: Vec<InitializingTokensMap>,
    current_pairs: &HashMap<String, Arc<dyn Pool>>,
) -> InitializingTokensMap {
    // First, merge all hashmaps without filtering
    let mut initializing_tokens_map = InitializingTokensMap::new();
    let merged_hashmap = &mut initializing_tokens_map.tokens;
    for hashmap in hashmaps {
        for (key, value) in hashmap.tokens {
            merged_hashmap.entry(key)
                .and_modify(|(e, _): &mut (Vec<NonInitializedPair>, usize)| {
                    e.extend(value.0.clone());
                })
                .or_insert(value);
        }
    }
    
    // Update count and filter pairs
    merged_hashmap.retain(|_, (pairs, count)| {
        // Count how many pairs exist in current_pairs
        *count = pairs.iter()
            .filter(|pair| current_pairs.contains_key(&pair.address))
            .count();
        let should_keep = pairs.len() >= 2;
        
        // Remove pairs that exist in current_pairs
        pairs.retain(|pair| !current_pairs.contains_key(&pair.address));
        
        // Keep only entries that have 2 or more pairs remaining
        should_keep
    });

    initializing_tokens_map
}

#[derive(Clone)]
pub struct FarmableTokens {
    pub tokens: Vec<String>,
}

impl FarmableTokens {
    pub fn new(tokens: Vec<String>) -> Self {
        Self { tokens }
    }

    pub fn choose_farmable_tokens(&self, mint_x: &String, mint_y: &String) -> TokenLabel {
        let contains_mint_x = self.tokens.contains(mint_x);
        let contains_mint_y = self.tokens.contains(mint_y);
        
        if contains_mint_x && contains_mint_y {
            // Choose the first one in the vec
            if self.tokens.iter().position(|t| t == mint_x).unwrap_or(usize::MAX) < 
               self.tokens.iter().position(|t| t == mint_y).unwrap_or(usize::MAX) {
                TokenLabel::X
            } else {
                TokenLabel::Y
            }
        } else if contains_mint_x {
            TokenLabel::X
        } else if contains_mint_y {
            TokenLabel::Y
        } else {
            TokenLabel::None
        }
    }
    
}

#[derive(Debug, Clone)]
pub struct NonInitializedPair{
    pub address: String,
    pub amm_type: AmmType,
    pub farm_token_label: TokenLabel,
    pub optional_data : Option<OptionalData>,
}

#[derive(Debug, Clone)]
pub struct InitializingTokensMap{
    pub tokens: HashMap<String, (Vec<NonInitializedPair>, usize)>,
}

impl InitializingTokensMap {
    pub fn new() -> Self {
        Self { tokens: HashMap::new() }
    }

    pub fn add_with_farm_token_label(
        &mut self, 
        mint_x: &String, 
        mint_y: &String, 
        pair_address: String, 
        amm_type: AmmType, 
        farm_token_label: TokenLabel,
        optional_data: Option<OptionalData>
    ) {
        match farm_token_label {
            TokenLabel::X => {
                self.add_pair(
                    mint_y, 
                    pair_address.clone(), 
                    amm_type, 
                    farm_token_label,
                    optional_data
                );
            }
            TokenLabel::Y => {
                self.add_pair(
                    mint_x, 
                    pair_address.clone(), 
                    amm_type, 
                    farm_token_label,
                    optional_data
                );
            }
            TokenLabel::None => {
                self.add_pair(
                    mint_x, 
                    pair_address.clone(), 
                    amm_type.clone(), 
                    farm_token_label.clone(),
                    optional_data.clone()
                );
                self.add_pair(
                    mint_y, 
                    pair_address.clone(), 
                    amm_type, 
                    farm_token_label,
                    optional_data
                );
            }
        }
    }

    pub fn add_pair(
        &mut self, 
        key_mint: &String, 
        pair_address: String, 
        amm_type: AmmType, 
        farm_token_label: TokenLabel,
        optional_data: Option<OptionalData>
    ) {
        self.tokens.entry(key_mint.clone())
            .and_modify(|(pairs, _count)| {
                pairs.push(NonInitializedPair{
                    address: pair_address.clone(),
                    amm_type: amm_type.clone(),
                    farm_token_label: farm_token_label.clone(),
                    optional_data: optional_data.clone(),
                });
            })
            .or_insert_with(|| {
                let pair = NonInitializedPair{
                    address: pair_address,
                    amm_type,
                    farm_token_label,
                    optional_data,
                };
                (vec![pair], 0)
            });
    }
}

#[derive(Debug, Clone)]
pub enum OptionalData {
    Pumpfun(PumpfunAmmPoolInfo),
}
