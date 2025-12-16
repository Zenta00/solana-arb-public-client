use borsh::{BorshDeserialize, BorshSerialize};
use lazy_static::lazy_static;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{instruction::AccountMeta, pubkey::Pubkey};
use std::error::Error;
use std::sync::Arc;

lazy_static! {
    static ref RAYDIUM_ID: Pubkey =
        Pubkey::from_str_const("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");
    static ref RAYDIUM_AUTHORITY_ID: Pubkey =
        Pubkey::from_str_const("5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1");
    static ref OPENBOOK_MARKET_ID: Pubkey =
        Pubkey::from_str_const("srmqPvymJeFKQ4zGQed1GFppgkRHL9kaELCbyksJtPX");
}
const WSOL_MINT: Pubkey = Pubkey::from_str_const("So11111111111111111111111111111111111111112");

pub const TOKEN_PROGRAM_ACCOUNT: Pubkey = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct LiquidityStateV4 {
    pub status: u64,
    pub nonce: u64,
    pub max_order: u64,
    pub depth: u64,
    pub base_decimal: u64,
    pub quote_decimal: u64,
    pub state: u64,
    pub reset_flag: u64,
    pub min_size: u64,
    pub vol_max_cut_ratio: u64,
    pub amount_wave_ratio: u64,
    pub base_lot_size: u64,
    pub quote_lot_size: u64,
    pub min_price_multiplier: u64,
    pub max_price_multiplier: u64,
    pub system_decimal_value: u64,
    pub min_separate_numerator: u64,
    pub min_separate_denominator: u64,
    pub trade_fee_numerator: u64,
    pub trade_fee_denominator: u64,
    pub pnl_numerator: u64,
    pub pnl_denominator: u64,
    pub swap_fee_numerator: u64,
    pub swap_fee_denominator: u64,
    pub base_need_take_pnl: u64,
    pub quote_need_take_pnl: u64,
    pub quote_total_pnl: u64,
    pub base_total_pnl: u64,
    pub pool_open_time: u64,
    pub punish_pc_amount: u64,
    pub punish_coin_amount: u64,
    pub orderbook_to_init_time: u64,
    pub swap_base_in_amount: u128,
    pub swap_quote_out_amount: u128,
    pub swap_base2_quote_fee: u64,
    pub swap_quote_in_amount: u128,
    pub swap_base_out_amount: u128,
    pub swap_quote2_base_fee: u64,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub lp_mint: Pubkey,
    pub open_orders: Pubkey,
    pub market_id: Pubkey,
    pub market_program_id: Pubkey,
    pub target_orders: Pubkey,
    pub withdraw_queue: Pubkey,
    pub lp_vault: Pubkey,
    pub owner: Pubkey,
    pub lp_reserve: u64,
    pub padding: [u64; 3],
}

impl LiquidityStateV4 {
    pub fn try_deserialize(data: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(LiquidityStateV4::try_from_slice(data)?)
    }
}

#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct MarketStateV3 {
    pub padding1: [u8; 5],             // blob(5)
    pub account_flags: [u8; 8],        // blob(8)
    pub own_address: Pubkey,           // publicKey
    pub vault_signer_nonce: u64,       // u64
    pub base_mint: Pubkey,             // publicKey
    pub quote_mint: Pubkey,            // publicKey
    pub base_vault: Pubkey,            // publicKey
    pub base_deposits_total: u64,      // u64
    pub base_fees_accrued: u64,        // u64
    pub quote_vault: Pubkey,           // publicKey
    pub quote_deposits_total: u64,     // u64
    pub quote_fees_accrued: u64,       // u64
    pub quote_dust_threshold: u64,     // u64
    pub request_queue: Pubkey,         // publicKey
    pub event_queue: Pubkey,           // publicKey
    pub bids: Pubkey,                  // publicKey
    pub asks: Pubkey,                  // publicKey
    pub base_lot_size: u64,            // u64
    pub quote_lot_size: u64,           // u64
    pub fee_rate_bps: u64,             // u64
    pub referrer_rebates_accrued: u64, // u64
    pub padding2: [u8; 7],             // blob(7)
}

impl MarketStateV3 {
    pub fn try_deserialize(data: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(MarketStateV3::try_from_slice(data)?)
    }
}

#[derive(Debug, Clone)]
pub struct SwapAccountMetas {
    pub vec: Vec<AccountMeta>,
}

impl SwapAccountMetas {
    pub fn get_with_user_and_tokens_pubkeys(
        &self, 
        in_mint: Pubkey, 
        out_mint: Pubkey, 
        user_pubkey: Pubkey
    ) -> Vec<AccountMeta> {
        let mut vec = self.vec.clone();
        vec[7] = AccountMeta::new(in_mint, false);
        vec[8] = AccountMeta::new(out_mint, false);
        vec[9] = AccountMeta::new(user_pubkey, true);
        let market_account = vec[2].clone();
        let swap_vec = vec![
            vec[1].clone(),
            market_account.clone(),
            vec[3].clone(),
            market_account.clone(),
            vec[5].clone(),
            vec[6].clone(),
            market_account.clone(),
            market_account.clone(),
            market_account.clone(),
            market_account.clone(),
            market_account.clone(),
            market_account.clone(),
            market_account.clone(),
            market_account.clone(),
            vec[7].clone(),
            vec[8].clone(),
            vec[9].clone(),
        ];
        swap_vec
    }  

    pub fn get_with_user_and_tokens(
        &self, 
        in_mint: String, 
        out_mint: String, 
        user_pubkey: Pubkey
    ) -> Vec<AccountMeta> {
        let mut vec = self.vec.clone();
        vec[7] = AccountMeta::new(Pubkey::from_str_const(&in_mint), false);
        vec[8] = AccountMeta::new(Pubkey::from_str_const(&out_mint), false);
        vec[9] = AccountMeta::new(user_pubkey, true);
        vec
        
    }   
}
#[derive(Debug, Clone)]
pub struct RaydiumV4PoolInfo {
    pub keys: SwapAccountMetas,
    pub amm_flag: u8,
    pub owner: String,
}

// Constants for market versions
pub const MARKET_VERSION_TO_STATE_LAYOUT: u8 = 3;

pub fn raydium_pool_info_from_data(
    pool_id: &Pubkey,
    data: &[u8],
) -> Result<(RaydiumV4PoolInfo, String, String, String, String), Box<dyn Error>> {
    let pool_state = LiquidityStateV4::try_deserialize(data)?;
    let reserve_x = AccountMeta::new(pool_state.base_vault, false);
    let reserve_y = AccountMeta::new(pool_state.quote_vault, false);
    let reserve_x_pubkey = pool_state.base_vault.to_string();
    let reserve_y_pubkey = pool_state.quote_vault.to_string();

    let pair_account = AccountMeta::new(*pool_id, false);

    let keys= SwapAccountMetas{
        vec: vec![
            AccountMeta::new_readonly(RAYDIUM_ID.clone(), false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ACCOUNT.clone(), false),
            pair_account,
            AccountMeta::new_readonly(RAYDIUM_AUTHORITY_ID.clone(), false),
            AccountMeta::new(pool_state.open_orders, false),
            reserve_x,
            reserve_y,
            AccountMeta::default(),
            AccountMeta::default(),
            AccountMeta::default(),
        ]
    };



    Ok((
        RaydiumV4PoolInfo {
            keys,
            amm_flag: 0,
            owner: RAYDIUM_AUTHORITY_ID.to_string(),
        },
        pool_state.base_mint.to_string(),
        pool_state.quote_mint.to_string(),
        reserve_x_pubkey,
        reserve_y_pubkey
    ))
}

pub async fn raydium_pool_info(
    connection: Arc<RpcClient>,
    pool_id: &Pubkey,
) -> Result<(RaydiumV4PoolInfo, String, String, String, String), Box<dyn Error>> {
    let amm_account_info = connection.get_account(pool_id).await?;

    let pool_state = LiquidityStateV4::try_deserialize(&amm_account_info.data)?;

    let reserve_x = AccountMeta::new(pool_state.base_vault, false);
    let reserve_y = AccountMeta::new(pool_state.quote_vault, false);
    let reserve_x_pubkey = pool_state.base_vault.to_string();
    let reserve_y_pubkey = pool_state.quote_vault.to_string();

    let pair_account = AccountMeta::new(*pool_id, false);

    let keys= SwapAccountMetas{
        vec: vec![
            AccountMeta::new_readonly(RAYDIUM_ID.clone(), false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ACCOUNT.clone(), false),
            pair_account,
            AccountMeta::new_readonly(RAYDIUM_AUTHORITY_ID.clone(), false),
            AccountMeta::new(pool_state.open_orders, false),
            reserve_x,
            reserve_y,
            AccountMeta::default(),
            AccountMeta::default(),
            AccountMeta::default(),
        ]
    };



    Ok((
        RaydiumV4PoolInfo {
            keys,
            amm_flag: 0,
            owner: RAYDIUM_AUTHORITY_ID.to_string(),
        },
        pool_state.base_mint.to_string(),
        pool_state.quote_mint.to_string(),
        reserve_x_pubkey,
        reserve_y_pubkey
    ))
}