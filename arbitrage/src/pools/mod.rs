pub mod pools_utils;
pub mod meteora_dlmm;
pub mod raydium_v4;
pub mod pumpfun_amm;
pub mod raydium_clmm;
pub mod price_maths;
pub mod raydium_cpmm;
pub use pools_utils::*;
pub mod spam_pools;
use {
    crossbeam_channel::Sender,
    std::any::Any,
    std::hash::Hash,
    std::sync::{Arc, RwLock},
    crate::{
        tracker::copytrade::{CopyTradePosition},
        simulation::router::{
            SwapDirection,
            SwapStepInstruction,
        },
        simulation::pool_impact::AmmType,
        pools::{
            meteora_dlmm::{
                MeteoraDlmmPool,
            },
            raydium_v4::RaydiumV4Pool,
            pumpfun_amm::PumpfunAmmPool,
            raydium_clmm::clmm::RaydiumClmmPool,
            raydium_cpmm::RaydiumCpmmPool,
            
        },
        
    },
    solana_sdk::{
        system_instruction,
        address_lookup_table::AddressLookupTableAccount,
        message::{v0, VersionedMessage},
        pubkey::Pubkey,
        transaction::{Transaction, VersionedTransaction},
        commitment_config::CommitmentLevel,
        compute_budget::ComputeBudgetInstruction, instruction::AccountMeta, instruction::Instruction,
        hash::Hash as BlockHash,
        signature::{Signer, Keypair},
    },
    rand::Rng,
    crate::tokens::pools_manager::ConnectionManager,
    borsh::{BorshSerialize, BorshDeserialize},
    std::error::Error,
};

pub const SCALE_FEE_CONSTANT: u128 = 1_000_000_000;
pub const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";
pub struct PoolConstants;


impl PoolConstants {
    pub const BASE_COMPUTE_BUDGET_INSTRUCTION_LIMIT: u32 = 95_000;
    pub const MIN_COMPUT_BUDGET: u32 = 115_000;
    pub const DEPTH_COMPUTE_UNIT_LIMIT_INCREASE: u32 = 15_000;
    pub const MAX_COMPUTE_UNIT_LIMIT: u32 = 250_000;
    pub const CREATE_ATA_BUDGET: u32 = 27_000;
    pub const ARBITRAGE_INSTRUCTION_SELECTOR: u8 = 0;
    pub const ARBITRAGE_INSTRUCTION_AMM_ID_METEORA: u8 = 2;
    pub const ARBITRAGE_INSTRUCTION_AMM_ID_RAYDIUM: u8 = 1;
    pub const MOVE_FEE_VALUE: u32 = 100_000;
    pub const JITO_MOVE_FEE_VALUE: u32 = 5;
    pub const BASE_FEE: u32 = 0;
    pub const JITO_BASE_FEE: u64 = 5_000;
    pub const BASE_FEE_PERCENTAGE: u64 = 1;
    pub const JITO_FEE_PERCENTAGE: u64 = 10;
    pub const MAX_FEE: u32 = 10_000_000;
    pub const MAX_JITO_FEE: u32 = 85;
    pub const MAX_TOTAL_FEE: u64 = 500_000_000_000;
    pub const MAX_FEE_PER_COMPUTE_UNIT: u64 = 100_000_000;
    pub const MICRO_LAMPORTS_PER_LAMPORT: u64 = 1_000_000;
    pub const RAW_PROFIT_LOWER_BOUND: u64 = 2_500_000;
    pub const RAW_FEE_LOWER_BOUND: u64 = 1_100_000;
    pub const RAW_FEE_UPPER_BOUND: u64 = 2_200_000;
    pub const TEMP_MAX_PROFIT_TO_TRY: u64 = 0;
    pub const MIN_PROFIT: u64 = 22_000;
    

    // ********** Fee control constants **********

    const WIN_PERCENTAGE_INITIAL: f64 = 12.0;
    const JITO_WIN_PERCENTAGE_INITIAL: f64 = 12.0;
    pub const MAX_PERCENTAGE: f64 = 7.0;
    pub const JITO_MAX_PERCENTAGE: f64 = 90.0;
    pub const BASE_FACTOR: f64 = 1.05;
    const MIN_WIN_PERCENTAGE: u32 = 5;

    // ********** Profit control constants **********




    //**********************************************
    

    pub const TOKEN_PROGRAM_ID: Pubkey =
        Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
    pub const SYSTEM_PROGRAM: Pubkey = 
        Pubkey::from_str_const("11111111111111111111111111111111");
    pub const ARBITRAGE_PROGRAM: Pubkey =
        Pubkey::from_str_const("GmdqtNMiSEmFkBCyfU6cEAfjzprV78GXJKJkxwwEwZD9");

    pub const RAYDIUM_CLMM_PROGRAM: Pubkey =
        Pubkey::from_str_const("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK");

    pub const WSOL_ACCOUNT: Pubkey =
        Pubkey::from_str_const("So11111111111111111111111111111111111111112");

}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TokenLabel {
    X,
    Y,
    None,
}
#[derive(Clone, Eq, PartialEq)]
pub enum RoutingStep {
    Extremity,
    Intermediate,
}

impl RoutingStep {
    fn from_farmable_tokens(
        mint_x: &str,
        mint_y: &str,
        farm_token: &str
    ) -> Self {
        if mint_x == farm_token || mint_y == farm_token {
            Self::Extremity
        } else {
            Self::Intermediate
        }
    }
}
#[derive(Debug, Clone)]
pub enum PoolError {
    Acceptable,
    NeedReinitialize(Vec<String>),
    TrackedTickArrayNotFound(String),
    CurrentTickNotFound,
    NotEnoughLiquidity,
    InvalidRoute,
    NotEnoughProfit,
}
pub trait Pool: Any + Send + Sync {

    fn as_meteora_dlmm(&self) -> Option<&MeteoraDlmmPool> {
        None
    }

    fn as_meteora_dlmm_mut(&mut self) -> Option<&mut MeteoraDlmmPool> {
        None
    }

    fn as_raydium_v4(&self) -> Option<&RaydiumV4Pool> {
        None
    }

    fn as_raydium_v4_mut(&mut self) -> Option<&mut RaydiumV4Pool> {
        None
    }

    fn as_raydium_cpmm(&self) -> Option<&RaydiumCpmmPool> {
        None
    }

    fn as_raydium_cpmm_mut(&mut self) -> Option<&mut RaydiumCpmmPool> {
        None
    }

    fn as_pumpfun_amm(&self) -> Option<&PumpfunAmmPool> {
        None
    }

    fn as_pumpfun_amm_mut(&mut self) -> Option<&mut PumpfunAmmPool> {
        None
    }

    fn as_raydium_clmm(&self) -> Option<&RaydiumClmmPool> {
        None
    }

    fn as_raydium_clmm_mut(&mut self) -> Option<&mut RaydiumClmmPool> {
        None
    }

    
    fn get_pool_type(&self) -> String;

    fn get_liquidity_type(&self) -> LiquidityType;

    fn get_swap_instructions(&self,
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
        (vec![], 0)
    }
    
    

    /*fn as_raydium_clmm(&self) -> Option<&RaydiumClmmPool> {
        None
    }

    fn as_raydium_clmm_mut(&mut self) -> Option<&mut RaydiumClmmPool> {
        None
    }*/

    fn set_price(
        &self, 
        update_pool_sender: &Sender<(String, PoolUpdateType)>
    ) -> Result<bool, PoolError>;

    fn get_fee_bps(&self) -> u128;

    fn get_amount_out_with_cu(&self, amount_in: f64, direction: SwapDirection) -> Result<(f64, f64), PoolError>;
    fn get_amount_out_with_cu_extra_infos(&self, amount_in: f64, direction: SwapDirection) -> Result<(f64, f64, Option<Vec<String>>), PoolError>;
    fn get_amount_in_for_target_price(&self, target_price: u128, direction: SwapDirection) -> Result<u128, PoolError>;
    fn get_amount_in(&self, amount_out: u128, direction: SwapDirection) -> Result<u128, PoolError>;


    fn update_with_balances(
        &self, 
        x_amount: u128, 
        y_amount: u128, 
        optional_data: &[u8],
    ) -> Result<(bool, Option<(u128,u128,bool, u128, u128)>), PoolError>{
        Ok((false, None))
    }

    fn store_last_sent_slot(&self, slot: u64);

    fn get_last_sent_slot(&self) -> u64;

    fn get_last_amount_in(&self) -> u64;
    fn store_last_amount_in(&self, amount: u64);

    fn can_send(&self, current_slot: u64, amount_in: u64) -> bool {
        let last_amount_in = self.get_last_amount_in();
        if self.get_last_sent_slot() >= current_slot - 1 {
            if amount_in < last_amount_in {
                if last_amount_in - amount_in > last_amount_in / 5 {
                    return true;
                } else {
                    return false;
                }
            } else {
                if  amount_in - last_amount_in > amount_in / 5 {
                    return true;
                } else {
                    return false;
                }
            }
        } else {
            true
        }
    }



    fn get_address(&self) -> String;
    fn get_owner(&self) -> String;
    fn get_price(&self) -> u128;
    fn get_price_and_fee(&self) -> (u128, u128);
    fn get_price_and_fee_with_inverted(&self) -> (u128,u128, u128);

    fn get_prices_and_fee_with_init_check(
        &self,
        account_write_number: u64,
        initialize_sender: &Sender<PoolInitialize>
    ) -> (u128, u128, u128) {
        self.get_price_and_fee_with_inverted()
    }

    fn get_inverted_price_and_fee(&self) -> (Option<u128>, u128);

    fn get_farm_token_label(&self) -> TokenLabel;
    fn get_non_farm_token_mint(&self) -> Option<String>;
    fn get_farm_token_mint(&self) -> Option<String>;
    fn is_farm_token(&self, farm_token: &str) -> bool {
        if let Some(farm_token_mint) = self.get_farm_token_mint() {
            farm_token_mint == farm_token
        } else {
            false
        }
    }
    fn get_swap_direction_for_target_out(&self, target_out: &str) -> (SwapDirection, String) {
        if self.get_mint_x() == target_out {
            (SwapDirection::YtoX, self.get_mint_y())
        } else {
            (SwapDirection::XtoY, self.get_mint_x())
        } 
    }

    fn get_swap_direction_for_target_in(&self, target_in: &str) -> (SwapDirection, String) {
        if self.get_mint_x() == target_in {
            (SwapDirection::XtoY, self.get_mint_y())
        } else {
            (SwapDirection::YtoX, self.get_mint_x())
        }
    }

    fn get_complementary_mint(&self, base_mint: &str) -> String {
        let mint_x = self.get_mint_x();

        if mint_x == base_mint {
            self.get_mint_y()
        } else {
            mint_x
        }
    }

    fn get_mint_x(&self) -> String;

    fn get_mint_y(&self) -> String;

    

    fn get_associated_tracked_pubkeys(&self) -> Vec<String>;

    fn get_all_associated_tracked_pubkeys(&self) -> Vec<String> {
        let mut all_associated_tracked_pubkeys = self.get_associated_tracked_pubkeys();
        all_associated_tracked_pubkeys.push(self.get_address());
        all_associated_tracked_pubkeys
    }

    fn get_routing_step(&self) -> RoutingStep;

    fn get_reserve_x_account(&self) -> String {
        "".to_string()
    }
    fn get_reserve_y_account(&self) -> String {
        "".to_string()
    }
    // accounts_meta, accounts_meta_len, in_index, out_index

    fn append_swap_step(
        &self, 
        wallet_pubkey: Pubkey,
        accounts_meta: &mut Vec<AccountMeta>, 
        data: &mut Vec<u8>, 
        user_token_in: String,
        mint_in: &String,
        user_token_out: String,
        mint_out: &String,
        extra_accounts: &Option<Vec<String>>
    );

    fn get_mints_to_initialize(&self) -> Vec<String> {
        match self.get_farm_token_label() {
            TokenLabel::X => vec![self.get_mint_y()],
            TokenLabel::Y => vec![self.get_mint_x()],
            TokenLabel::None => vec![self.get_mint_x(), self.get_mint_y()],
        }
    }

    fn add_congestion(&self, slot: u64);
    fn get_congestion_rate(&self) -> (f32, u64, u8);


}

impl PartialEq for dyn Pool {
    fn eq(&self, other: &Self) -> bool {
        self.get_address() == other.get_address()
    }
}

impl Eq for dyn Pool {}

impl std::hash::Hash for dyn Pool {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.get_address().hash(state);
    }
}

impl std::fmt::Debug for dyn Pool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pool(address: {})", self.get_address())
    }
}

pub fn compute_kp_swap_amount(
    prev_reserve_x_amount: u128,
    prev_reserve_y_amount: u128,
    reserve_x_amount: u128,
    reserve_y_amount: u128,
    routing_step: RoutingStep,
    farm_token_label: &TokenLabel,
) -> Option<(u128, u128, bool)> {
    if routing_step == RoutingStep::Extremity {
        if prev_reserve_x_amount != 0 && prev_reserve_y_amount != 0 {
            let (y_amount_diff, x_amount_diff, y_for_x) = if prev_reserve_y_amount < reserve_y_amount {
                if prev_reserve_x_amount > reserve_x_amount {
                    (reserve_y_amount - prev_reserve_y_amount, prev_reserve_x_amount - reserve_x_amount, true)
                }
                else {
                    return None;
                }
            } else {
                if reserve_x_amount > prev_reserve_x_amount {
                    (prev_reserve_y_amount - reserve_y_amount, reserve_x_amount - prev_reserve_x_amount, false)
                }
                else {
                    return None;
                }
            };

            if farm_token_label == &TokenLabel::Y {
                Some((y_amount_diff, x_amount_diff, y_for_x))
            } else if farm_token_label == &TokenLabel::X {
                Some((x_amount_diff, y_amount_diff, !y_for_x))
            } else {
                return None;
            }
        } else {
            return None;
        }

    } else {
        return None;
    }
}

#[derive(Eq, PartialEq)]
pub enum LiquidityType {
    ConstantProduct,
    Concentrated,
}

pub struct PoolInitialize{
    pub pool: String,
    pub account_write_number: u64,
    pub accounts_to_init: Vec<(String, AmmType)>,
}

pub enum ArbitrageType {
    DlmmV4,
    DlmmClmm,
    DlmmOrcaWp,
    DlmmDlmm,
    V4OrcaWp,
    V4Clmm,
    V4Dlmm,
    V4V4,
    ClmmDlmm,
    ClmmV4,
    ClmmOrcaWp,
    ClmmClmm,
    AmmTypeError,
    None
}

pub enum PoolUpdateType{
    NeedLeftBins,
    NeedRightBins,
    NeedLeftTickArrayState,
    NeedRightTickArrayState,
}

use std::collections::VecDeque;

pub const CONGESTION_HISTORY_WINDOW: usize = 10;

pub struct CongestionStats {
    pub last_locked_slot: u64,
    pub congestion_history: VecDeque<u8>,
    pub congestion_rate: f32,
}

impl CongestionStats {
    pub fn new_at_slot(slot: u64) -> Self {
        let mut congestion_history = VecDeque::with_capacity(CONGESTION_HISTORY_WINDOW);
        for _ in 0..CONGESTION_HISTORY_WINDOW {
            congestion_history.push_back(0);
        }
        
        Self {
            last_locked_slot: slot,
            congestion_history,
            congestion_rate: 0.0,
        }
    }
    
    pub fn add_congestion(&mut self, slot: u64) {
        if slot > self.last_locked_slot {
            let slots_to_advance = (slot - self.last_locked_slot) as usize;
            
            // Remove old entries
            for _ in 0..std::cmp::min(slots_to_advance, CONGESTION_HISTORY_WINDOW) {
                self.congestion_history.pop_front();
            }
            
            // Add new zero entries
            for _ in 0..std::cmp::min(slots_to_advance, CONGESTION_HISTORY_WINDOW) {
                self.congestion_history.push_back(0);
            }
            let _ = self.congestion_history.back_mut().unwrap().saturating_add(1);
            self.last_locked_slot = slot;
        } else if slot == self.last_locked_slot {
            if let Some(last) = self.congestion_history.back_mut() {
                *last = last.saturating_add(1);
            }
        }
        
        // Calculate congestion rate
        let sum: u32 = self.congestion_history.iter().map(|&x| x as u32).sum();
        self.congestion_rate = sum as f32 / CONGESTION_HISTORY_WINDOW as f32;
    }
}

pub enum LimitOrder {
    TakeProfit(Order), // price, amount_pct
    StopLoss(Order),
}

pub struct Order{
    pub price: u128,
    pub amount: u128,
}