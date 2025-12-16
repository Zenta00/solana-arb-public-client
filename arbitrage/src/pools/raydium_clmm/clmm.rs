use {
    crossbeam_channel::Sender,
    solana_sdk::{
        clock::Slot,
        pubkey::Pubkey,
        transaction::{Transaction, VersionedTransaction},
        signature::{
            Signature,
            Signer,
        },
        system_instruction,
        compute_budget::ComputeBudgetInstruction, instruction::AccountMeta, instruction::Instruction,
        hash::Hash as BlockHash
    },
    std::time::{Instant, Duration},
    std::time::{SystemTime, UNIX_EPOCH},
    std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    std::hash::{Hash, Hasher},
    std::cmp::{Eq, PartialEq},
    std::ops::Neg,
    std::sync::{Arc, RwLock},
    crate::{
        simulation::simulator_safe_maths::*,
        pools::{
            PoolError,
            raydium_clmm::{
                liquidity_math,
                big_num::U128,
                swap_math,
                tick_array::{TickArrayState, TickState},
                tick_array_bit_map,
                tick_math,
                tickarray_bitmap_extension::TickArrayBitmapExtension,
                tick_array_bit_map::check_current_tick_array_is_initialized,
                error::ErrorCode,
                big_num::U1024,
            },
            *,
            price_maths::*,
            raydium_v4::RaydiumV4Pool,
        },

    },
    meteora::swap::{
        MeteoraDlmmPoolInfo,
        meteora_pool_info,
    },
    solana_client::nonblocking::rpc_client::RpcClient,
    rand::Rng,
    borsh::{BorshSerialize, BorshDeserialize},
    std::error::Error,
    tracing::{info,debug, error},
};

pub const TICK_ARRAY_SIZE: i32 = 60;
pub const TICK_ARRAY_SEED: &str = "tick_array";
pub const POOL_TICK_ARRAY_BITMAP_SEED: &str = "pool_tick_array_bitmap_extension";
pub const FEE_RATE_TO_BPS_FACTOR: u32 = 1_000;

pub const MAX_U_23: u128 = 1 << 32;
pub const TICK_STATE_START_OFFSET: usize = 36;
pub const TICK_STATE_SIZE: usize = 160;
pub const TICK_STATE_COUNT: usize = 60;
pub const MIN_TICK_ARRAY_STATES_COUNT_SIDE: usize = 3;
pub const FEE_RATE_DENOMINATOR_VALUE: u32 = 1_000_000;
pub const MAX_TOKEN_AMOUNT: u64 = u64::MAX;
pub const CLMM_BASE_BUDGET_COST: f64 = 100_000.0;
pub const CLMM_DEPTH_INCREASE_COST: f64 = 5_000.0;

pub struct WritableData {
    address: String,
    tick_spacing: u16,
    trade_fee_rate: u32,
    protocol_fee_rate: u32,
    fund_fee_rate: u32,
    liquidity: u128,
    sqrt_price_x64: u128,
    price: u128,
    inversed_price: u128,
    tick_array_states: Vec<TickArrayState>,
    active_tick_array_index: usize,
    tick_current: i32,
    tick_previous: i32,
    default_tickarray_bitmap: [u64; 16],
    tickarray_bitmap_extension: TickArrayBitmapExtension,
    write_number: u64,
    last_init_check: Instant,
}

impl WritableData {

    pub fn new(
        address: String,
        tick_spacing: u16,
        protocol_fee_rate: u32,
        trade_fee_rate: u32,
        fund_fee_rate: u32,
        tick_array_states: Vec<TickArrayState>,
        tickarray_bitmap_extension: TickArrayBitmapExtension,
        default_tickarray_bitmap: [u64; 16],
    ) -> Self {
        Self {
            address,
            tick_spacing,
            trade_fee_rate,
            protocol_fee_rate,
            fund_fee_rate,
            liquidity: 0,
            sqrt_price_x64: 0,
            price: 0,
            inversed_price: 0,
            tick_array_states,
            active_tick_array_index: 0,
            tick_current: 0,
            tick_previous: 0,
            default_tickarray_bitmap,
            tickarray_bitmap_extension,
            write_number: 0,
            last_init_check: Instant::now() + Duration::from_secs(120),
        }
    }

    fn get_local_tick_array_state_index(&self, tick_current: i32) -> Result<usize, PoolError> {
    // Find the local tick array state that contains the current tick
    for (i, tick_array_state) in self.tick_array_states.iter().enumerate() {
        let start_index = tick_array_state.start_tick_index;
        let end_index = start_index + TICK_ARRAY_SIZE * self.tick_spacing as i32;
        
        if tick_current >= start_index && tick_current < end_index {
            return Ok(i);
            }
        }
        Err(PoolError::CurrentTickNotFound)
    }

    fn count_tick_array_states_on_sides(&self, current_tick_array_index: usize) -> (usize, usize) {
        let left_count = current_tick_array_index;
        let right_count = self.tick_array_states.len() - current_tick_array_index - 1;
        (left_count, right_count)
    }

    pub fn set_price(&mut self, update_pool_sender: &Sender<(String, PoolUpdateType)>) -> Result<(), PoolError> {
        if self.tick_current != self.tick_previous {
            let index = self.get_local_tick_array_state_index(self.tick_current)?;
            self.active_tick_array_index = index;
            let (left_count, right_count) = self.count_tick_array_states_on_sides(index);
            if left_count < MIN_TICK_ARRAY_STATES_COUNT_SIDE {
                update_pool_sender.send((self.address.clone(), PoolUpdateType::NeedLeftTickArrayState));
            }
            if right_count < MIN_TICK_ARRAY_STATES_COUNT_SIDE {
                update_pool_sender.send((self.address.clone(), PoolUpdateType::NeedRightTickArrayState));
            }
            self.tick_previous = self.tick_current;
        }

        let price_x64 = ClmmMaths::sqrt_price_x64_to_price_x64(self.sqrt_price_x64);
        self.price = price_x64;
        self.inversed_price = match price_x64.inv_price() {
            Ok(inversed_price) => inversed_price,
            Err(_) => {self.price = 0; 0},
        };
        Ok(())
    }
    pub fn parse_into_liquidity(&mut self, data: &[u8], write_number: u64) { //[8..] in calldata
        if write_number < self.write_number {
            return;
        }
        self.tick_previous = self.tick_current;
        self.liquidity = u128::from_le_bytes(data[229..245].try_into().unwrap());
        self.sqrt_price_x64 = u128::from_le_bytes(data[245..261].try_into().unwrap());
        self.tick_current = i32::from_le_bytes(data[261..265].try_into().unwrap());
        
        // Convert the bytes from 896 to 1024 into 16 u64 values
        let mut bitmap = [0u64; 16];
        for i in 0..16 {
            let start = 896 + i * 8;
            bitmap[i] = u64::from_le_bytes(data[start..start + 8].try_into().unwrap());
        }
        self.default_tickarray_bitmap = bitmap;
        self.write_number = write_number;
    }
    pub fn parse_into_tick_array_state(&mut self, address: String, data: &[u8], write_number: u64) -> Result<(), PoolError> { // needs [8..] in calldata

        for tick_array_state in self.tick_array_states.iter_mut() {
            if tick_array_state.address == address {
                tick_array_state.parse_from_data(data, write_number);
                return Ok(());
            }
        }
        Err(PoolError::TrackedTickArrayNotFound(address))
    }

    pub fn parse_into_tick_array_bitmap_extension(&mut self, data: &[u8], write_number: u64) {

        if write_number < self.tickarray_bitmap_extension.write_number {
            return;
        }
    // Parse positive tick array bitmap (14 arrays of 8 u64s each)
        let mut positive_tick_array_bitmap = [[0u64; 8]; 14];
        
        let mut offset = 32;
        
        for i in 0..14 {
            for j in 0..8 {
                positive_tick_array_bitmap[i][j] = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                offset += 8;
            }
        }

        // Parse negative tick array bitmap (14 arrays of 8 u64s each)
        let mut negative_tick_array_bitmap = [[0u64; 8]; 14];
        for i in 0..14 {
            for j in 0..8 {
                negative_tick_array_bitmap[i][j] = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                offset += 8;
            }
        }

        self.tickarray_bitmap_extension.positive_tick_array_bitmap = positive_tick_array_bitmap;
        self.tickarray_bitmap_extension.negative_tick_array_bitmap = negative_tick_array_bitmap;
        self.tickarray_bitmap_extension.write_number = write_number;
    }   

    pub fn get_tick_array_states_iter<'a>(&'a self, zero_for_one: bool) -> (Box<dyn Iterator<Item = &'a TickArrayState> + 'a>, usize) {
        let slice = if zero_for_one {
            &self.tick_array_states[0..=self.active_tick_array_index]
        } else {
            &self.tick_array_states[self.active_tick_array_index..self.tick_array_states.len()]
        };
        
        if zero_for_one {
            (Box::new(slice.iter().rev()), slice.len())
        } else {
            (Box::new(slice.iter()), slice.len())
        }
    }

    pub fn get_price(&self) -> u128 {
        self.price
    }

    pub fn get_inversed_price(&self) -> u128 {
        self.inversed_price
    }

    fn get_amount_in(&self, amount_out: u64, direction: SwapDirection) -> Result<u64, ErrorCode> {
        let (price_limit, zero_for_one) = if direction == SwapDirection::XtoY {
            (0_u128, true)
        } else {
            (u128::MAX, false)
        };
        let (amount_x, amount_y, _, _) = self.swap_internal(amount_out, price_limit, zero_for_one, false, false)?;
        if zero_for_one {
            Ok(amount_x)
        } else {
            Ok(amount_y)
        }
    }

    fn get_amount_in_for_target_price(&self, sqrt_price_limit_x64: u128, direction: SwapDirection) -> Result<u64, ErrorCode> {
        let zero_for_one = direction == SwapDirection::XtoY;
        let (amount_x, amount_y, _, _) = 
            self.swap_internal(MAX_TOKEN_AMOUNT, sqrt_price_limit_x64, zero_for_one, false, false)?;

        if zero_for_one {
            Ok(amount_x)
        } else {
            Ok(amount_y)
        }
    }

    fn get_amount_out_with_cu(&self, amount_in: u64, direction: SwapDirection) -> Result<(f64, f64), ErrorCode> {
        let (price_limit, zero_for_one) = if direction == SwapDirection::XtoY {
            (0_u128, true)
        } else {
            (u128::MAX, false)
        };
        let (amount_x, amount_y, k, _extra_infos) = self.swap_internal(amount_in, price_limit, zero_for_one, true, true)?;
        let cu = self.estimate_cu(k);
        if zero_for_one {
            Ok((amount_y as f64, cu))
        } else {
            Ok((amount_x as f64, cu))
        }
    }

    fn get_amount_out_with_cu_extra_infos(&self, amount_in: u64, direction: SwapDirection) -> Result<(f64, f64, Option<Vec<String>>), ErrorCode> {
        let (price_limit, zero_for_one) = if direction == SwapDirection::XtoY {
            (0_u128, true)
        } else {
            (u128::MAX, false)
        };
        let (amount_x, amount_y, k, extra_infos) = self.swap_internal(amount_in, price_limit, zero_for_one, true, true)?;
        let cu = self.estimate_cu(k);
        if zero_for_one {
            Ok((amount_y as f64, cu, extra_infos))
        } else {
            Ok((amount_x as f64, cu, extra_infos))
        }
    }

    fn estimate_cu(&self, k: u32) -> f64 {
        let cu = CLMM_BASE_BUDGET_COST + CLMM_DEPTH_INCREASE_COST * k as f64;
        cu
    }

    fn get_accounts_to_init(&mut self) -> Vec<(String, AmmType)> {
        if Instant::now().duration_since(self.last_init_check) > Duration::from_secs(60) {
            self.last_init_check = Instant::now();
        } else {
            return vec![];
        }
        let mut accounts_to_init = Vec::new();

        if self.write_number == 0 {
            accounts_to_init.push((self.address.clone(), AmmType::RaydiumClmmPool));
        }
        if self.tickarray_bitmap_extension.write_number == 0 {
            self.tickarray_bitmap_extension.write_number = 1;
            accounts_to_init.push((self.tickarray_bitmap_extension.address.clone(), AmmType::RaydiumClmmBitmapExt));
        }
        for tick_array_state in self.tick_array_states.iter_mut() {
            if tick_array_state.write_number == 0 {
                tick_array_state.write_number = 1;
                accounts_to_init.push((tick_array_state.address.clone(), AmmType::RaydiumClmmTickArray));
            }
        }
        accounts_to_init
    }
    
    
}

pub struct SwapState {
    // the amount remaining to be swapped in/out of the input/output asset
    pub amount_specified_remaining: u64,
    // the amount already swapped out/in of the output/input asset
    pub amount_calculated: u64,
    // current sqrt(price)
    pub sqrt_price_x64: u128,
    // the tick associated with the current price
    pub tick: i32,
    // the global fee of the input token
    pub fee_amount: u64,
    // amount of input token paid as protocol fee
    pub protocol_fee: u64,
    // amount of input token paid as fund fee
    pub fund_fee: u64,
    // the current liquidity in range
    pub liquidity: u128,
}

#[derive(Default)]
struct StepComputations {
    // the price at the beginning of the step
    sqrt_price_start_x64: u128,
    // the next tick to swap to from the current tick in the swap direction
    tick_next: i32,
    // whether tick_next is initialized or not
    initialized: bool,
    // sqrt(price) for the next tick (1/0)
    sqrt_price_next_x64: u128,
    // how much is being swapped in in this step
    amount_in: u64,
    // how much is being swapped out
    amount_out: u64,
    // how much fee is being paid in
    fee_amount: u64,
}

impl WritableData {

    pub fn swap_internal(
        &self,
        amount_specified: u64,
        sqrt_price_limit_x64: u128,
        zero_for_one: bool,
        is_base_input: bool,
        extra_infos: bool,
    ) -> Result<(u64, u64, u32, Option<Vec<String>>), ErrorCode> {

        if amount_specified == 0 {
            return Ok((0, 0, 0, None));
        }

        let mut optional_extra_infos = Vec::with_capacity(1);
        let mut k = 0;

        let mut state = SwapState {
            amount_specified_remaining: amount_specified,
            amount_calculated: 0,
            sqrt_price_x64: self.sqrt_price_x64,
            tick: self.tick_current,
            fee_amount: 0,
            protocol_fee: 0,
            fund_fee: 0,
            liquidity: self.liquidity,
        };

        let (mut is_match_pool_current_tick_array, first_vaild_tick_array_start_index) =
            self.get_first_initialized_tick_array(&self.tickarray_bitmap_extension, zero_for_one)?;
        let mut current_vaild_tick_array_start_index = first_vaild_tick_array_start_index;

        let (mut tick_array_states_iter, count) = self.get_tick_array_states_iter(zero_for_one);
        let mut tick_array_current = tick_array_states_iter.next().ok_or(ErrorCode::NotEnoughTickArrayAccount)?;
        // find the first active tick array account
        for _ in 0..count {
            if tick_array_current.start_tick_index == current_vaild_tick_array_start_index {
                if extra_infos {
                    optional_extra_infos.push(tick_array_current.address.clone());
                }
                break;
            }
            tick_array_current = tick_array_states_iter.next().ok_or(ErrorCode::NotEnoughTickArrayAccount)?;
        }

        while state.amount_specified_remaining != 0 && state.sqrt_price_x64 != sqrt_price_limit_x64 {
            k += 1;
    
            let mut step = StepComputations::default();
            step.sqrt_price_start_x64 = state.sqrt_price_x64;
    
            let mut next_initialized_tick = if let Some(tick_state) = tick_array_current
                .next_initialized_tick(state.tick, self.tick_spacing, zero_for_one)?
            {
                tick_state
            } else {
                if !is_match_pool_current_tick_array {
                    is_match_pool_current_tick_array = true;
                    tick_array_current.first_initialized_tick(zero_for_one)?
                } else {
                    &TickState::default()
                }
            };

            if !next_initialized_tick.is_initialized() {
                let next_initialized_tickarray_index = self.next_initialized_tick_array_start_index(
                        current_vaild_tick_array_start_index,
                        zero_for_one,
                    )?;
                if next_initialized_tickarray_index.is_none() {
                    return Err(ErrorCode::LiquidityInsufficient);
                }
    
                while tick_array_current.start_tick_index != next_initialized_tickarray_index.unwrap() {
                    tick_array_current = tick_array_states_iter.next().ok_or(ErrorCode::NotEnoughTickArrayAccount)?;
                }
                if extra_infos {
                    optional_extra_infos.push(tick_array_current.address.clone());
                }
                current_vaild_tick_array_start_index = next_initialized_tickarray_index.unwrap();
    
                let first_initialized_tick = tick_array_current.first_initialized_tick(zero_for_one)?;
                next_initialized_tick = first_initialized_tick;
            }
            step.tick_next = next_initialized_tick.tick;
            step.initialized = next_initialized_tick.is_initialized();
    
            if step.tick_next < tick_math::MIN_TICK {
                step.tick_next = tick_math::MIN_TICK;
            } else if step.tick_next > tick_math::MAX_TICK {
                step.tick_next = tick_math::MAX_TICK;
            }
            step.sqrt_price_next_x64 = tick_math::get_sqrt_price_at_tick(step.tick_next)?;
    
            let target_price = if (zero_for_one && step.sqrt_price_next_x64 < sqrt_price_limit_x64)
                || (!zero_for_one && step.sqrt_price_next_x64 > sqrt_price_limit_x64)
            {
                sqrt_price_limit_x64
            } else {
                step.sqrt_price_next_x64
            };
    
            if zero_for_one {
                assert!(state.tick >= step.tick_next);
                assert!(step.sqrt_price_start_x64 >= step.sqrt_price_next_x64);
                assert!(step.sqrt_price_start_x64 >= target_price);
            } else {
                assert!(step.tick_next > state.tick);
                assert!(step.sqrt_price_next_x64 >= step.sqrt_price_start_x64);
                assert!(target_price >= step.sqrt_price_start_x64);
            }

            let swap_step = swap_math::compute_swap_step(
                step.sqrt_price_start_x64,
                target_price,
                state.liquidity,
                state.amount_specified_remaining,
                self.trade_fee_rate,
                is_base_input,
                zero_for_one,
            )?;

            if zero_for_one {
                assert!(swap_step.sqrt_price_next_x64 >= target_price);
            } else {
                assert!(target_price >= swap_step.sqrt_price_next_x64);
            }

            state.sqrt_price_x64 = swap_step.sqrt_price_next_x64;
            step.amount_in = swap_step.amount_in;
            step.amount_out = swap_step.amount_out;
            step.fee_amount = swap_step.fee_amount;
    
            if is_base_input {
                state.amount_specified_remaining = state
                    .amount_specified_remaining
                    .checked_sub(step.amount_in + step.fee_amount)
                    .unwrap();
                state.amount_calculated = state
                    .amount_calculated
                    .checked_add(step.amount_out)
                    .unwrap();
            } else {
                state.amount_specified_remaining = state
                    .amount_specified_remaining
                    .checked_sub(step.amount_out)
                    .unwrap();
    
                let step_amount_calculate = step
                    .amount_in
                    .checked_add(step.fee_amount)
                    .ok_or(ErrorCode::CalculateOverflow)?;
                state.amount_calculated = state
                    .amount_calculated
                    .checked_add(step_amount_calculate)
                    .ok_or(ErrorCode::CalculateOverflow)?;
            }
    
            let step_fee_amount = step.fee_amount;
            // if the protocol fee is on, calculate how much is owed, decrement fee_amount, and increment protocol_fee
            if self.protocol_fee_rate > 0 {
                let delta = U128::from(step_fee_amount)
                    .checked_mul(self.protocol_fee_rate.into())
                    .unwrap()
                    .checked_div(FEE_RATE_DENOMINATOR_VALUE.into())
                    .unwrap()
                    .as_u64();
                step.fee_amount = step.fee_amount.checked_sub(delta).unwrap();
                state.protocol_fee = state.protocol_fee.checked_add(delta).unwrap();
            }
            // if the fund fee is on, calculate how much is owed, decrement fee_amount, and increment fund_fee
            if self.fund_fee_rate > 0 {
                let delta = U128::from(step_fee_amount)
                    .checked_mul(self.fund_fee_rate.into())
                    .unwrap()
                    .checked_div(FEE_RATE_DENOMINATOR_VALUE.into())
                    .unwrap()
                    .as_u64();
                step.fee_amount = step.fee_amount.checked_sub(delta).unwrap();
                state.fund_fee = state.fund_fee.checked_add(delta).unwrap();
            }
    
            // update global fee tracker
            if state.liquidity > 0 {
                state.fee_amount = state.fee_amount.checked_add(step.fee_amount).unwrap();
            }
            // shift tick if we reached the next price
            if state.sqrt_price_x64 == step.sqrt_price_next_x64 {
                // if the tick is initialized, run the tick transition
                if step.initialized {
    
                    let mut liquidity_net = next_initialized_tick.cross();
    
                    if zero_for_one {
                        liquidity_net = liquidity_net.neg();
                    }
                    state.liquidity = liquidity_math::add_delta(state.liquidity, liquidity_net)?;
                }
    
                state.tick = if zero_for_one {
                    step.tick_next - 1
                } else {
                    step.tick_next
                };
            } else if state.sqrt_price_x64 != step.sqrt_price_start_x64 {
                // recompute unless we're on a lower tick boundary (i.e. already transitioned ticks), and haven't moved
                // if only a small amount of quantity is traded, the input may be consumed by fees, resulting in no price change. If state.sqrt_price_x64, i.e., the latest price in the pool, is used to recalculate the tick, some errors may occur.
                // for example, if zero_for_one, and the price falls exactly on an initialized tick t after the first trade, then at this point, pool.sqrtPriceX64 = get_sqrt_price_at_tick(t), while pool.tick = t-1. if the input quantity of the
                // second trade is very small and the pool price does not change after the transaction, if the tick is recalculated, pool.tick will be equal to t, which is incorrect.
                state.tick = tick_math::get_tick_at_sqrt_price(state.sqrt_price_x64)?;
            }
        }
    
        let (amount_0, amount_1) = if zero_for_one == is_base_input {
            (
                amount_specified
                    .checked_sub(state.amount_specified_remaining)
                    .unwrap(),
                state.amount_calculated,
            )
        } else {
            (
                state.amount_calculated,
                amount_specified
                    .checked_sub(state.amount_specified_remaining)
                    .unwrap(),
            )
        };

        Ok((amount_0, amount_1, k, Some(optional_extra_infos)))
    }

    pub fn get_first_initialized_tick_array(
        &self,
        tickarray_bitmap_extension: &TickArrayBitmapExtension,
        zero_for_one: bool,
    ) -> Result<(bool, i32), ErrorCode> {
        let (is_initialized, start_index) =
            if self.is_overflow_default_tickarray_bitmap(vec![self.tick_current], self.tick_spacing) {
                tickarray_bitmap_extension
                    .check_tick_array_is_initialized(
                        TickArrayState::get_array_start_index(self.tick_current, self.tick_spacing),
                        self.tick_spacing,
                    )?
            } else {
                check_current_tick_array_is_initialized(
                    U1024(self.default_tickarray_bitmap),
                    self.tick_current,
                    self.tick_spacing.into(),
                )?
            };
        if is_initialized {
            return Ok((true, start_index));
        }
        let next_start_index = self.next_initialized_tick_array_start_index(
            TickArrayState::get_array_start_index(self.tick_current, self.tick_spacing),
            zero_for_one,
        )?;
        if next_start_index.is_none() {
            return Err(ErrorCode::InsufficientLiquidityForDirection);
        }
        return Ok((false, next_start_index.unwrap()));
    }


    pub fn is_overflow_default_tickarray_bitmap(&self, tick_indexs: Vec<i32>, tick_spacing: u16) -> bool {
        let (min_tick_array_start_index_boundary, max_tick_array_index_boundary) =
            self.tick_array_start_index_range(tick_spacing);
        for tick_index in tick_indexs {
            let tick_array_start_index =
                TickArrayState::get_array_start_index(tick_index, tick_spacing);
            if tick_array_start_index >= max_tick_array_index_boundary
                || tick_array_start_index < min_tick_array_start_index_boundary
            {
                return true;
            }
        }
        false
    }

    pub fn tick_array_start_index_range(&self, tick_spacing: u16) -> (i32, i32) {
        // the range of ticks that default tickarrary can represent
        let mut max_tick_boundary =
            tick_array_bit_map::max_tick_in_tickarray_bitmap(tick_spacing);
        let mut min_tick_boundary = -max_tick_boundary;
        if max_tick_boundary > tick_math::MAX_TICK {
            max_tick_boundary =
                TickArrayState::get_array_start_index(tick_math::MAX_TICK, tick_spacing);
            // find the next tick array start index
            max_tick_boundary = max_tick_boundary + TickArrayState::tick_count(tick_spacing);
        }
        if min_tick_boundary < tick_math::MIN_TICK {
            min_tick_boundary =
                TickArrayState::get_array_start_index(tick_math::MIN_TICK, tick_spacing);
        }
        (min_tick_boundary, max_tick_boundary)
    }

    pub fn next_initialized_tick_array_start_index(
        &self,
        mut last_tick_array_start_index: i32,
        zero_for_one: bool,
    ) -> Result<Option<i32>, ErrorCode> {
        last_tick_array_start_index =
            TickArrayState::get_array_start_index(last_tick_array_start_index, self.tick_spacing);

        loop {
            let (is_found, start_index) =
                tick_array_bit_map::next_initialized_tick_array_start_index(
                    U1024(self.default_tickarray_bitmap),
                    last_tick_array_start_index,
                    self.tick_spacing,
                    zero_for_one,
                );
            if is_found {
                return Ok(Some(start_index));
            }
            last_tick_array_start_index = start_index;

            /*if !self.tickarray_bitmap_extension.is_initialized {
                return Err(ErrorCode::MissingTickArrayBitmapExtensionAccount);
            }*/

            let (is_found, start_index) = self.tickarray_bitmap_extension
                .next_initialized_tick_array_from_one_bitmap(
                    last_tick_array_start_index,
                    self.tick_spacing,
                    zero_for_one,
                )?;
            if is_found {
                return Ok(Some(start_index));
            }
            last_tick_array_start_index = start_index;

            if last_tick_array_start_index < tick_math::MIN_TICK
                || last_tick_array_start_index > tick_math::MAX_TICK
            {
                return Ok(None);
            }
        }
    }
}

pub struct RaydiumClmmPool {
    pub mint_x: String,
    pub mint_y: String,
    pub farm_token_label: TokenLabel,
    pub address: String,
    pub writable_data: Arc<RwLock<WritableData>>,
    pub tick_spacing: u16,
    pub fee_bps: u128,
    pub last_sent_slot: Arc<AtomicU64>,
    pub pool_info: RaydiumClmmPoolInfo,
    pub last_amount_in: Arc<AtomicU64>,
    pub congestion_stats: RwLock<CongestionStats>,
}
impl Hash for RaydiumClmmPool {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.address.hash(state);
    }
}

impl PartialEq for RaydiumClmmPool {
    fn eq(&self, other: &Self) -> bool {
        self.address == other.address
    }
}

impl Eq for RaydiumClmmPool {}

impl Pool for RaydiumClmmPool {
    fn as_raydium_clmm(&self) -> Option<&RaydiumClmmPool> {
        Some(self)
    }

    fn as_raydium_clmm_mut(&mut self) -> Option<&mut RaydiumClmmPool> {
        Some(self)
    }
    fn get_address(&self) -> String {
        self.address.clone()
    }

    fn get_pool_type(&self) -> String {
        "Raydium-Clmm".to_string()
    }

    fn get_liquidity_type(&self) -> LiquidityType {
        LiquidityType::Concentrated
    }

    fn get_fee_bps(&self) -> u128 {
        self.fee_bps
    }

    fn set_price(&self, update_pool_sender: &Sender<(String, PoolUpdateType)>) -> Result<bool, PoolError> {
        let mut writable_data = self.writable_data.write().unwrap();
        let prev_price = writable_data.price;
        writable_data.set_price(update_pool_sender)?;
        Ok(prev_price != writable_data.price)
    }

    fn get_amount_out_with_cu(&self, amount_in: f64, direction: SwapDirection) -> Result<(f64, f64), PoolError> {
        let writable_data = self.writable_data.read().unwrap();
        match writable_data.get_amount_out_with_cu(amount_in as u64, direction) {
            Ok((amount_out, cu)) => Ok((amount_out as f64, cu)),
            Err(_e) => Err(PoolError::Acceptable),
        }
    }

    fn get_amount_out_with_cu_extra_infos(&self, amount_in: f64, direction: SwapDirection) -> Result<(f64, f64, Option<Vec<String>>), PoolError> {
        let writable_data = self.writable_data.read().unwrap();
        match writable_data.get_amount_out_with_cu_extra_infos(amount_in as u64, direction) {
            Ok((amount_out, cu, extra_infos)) => Ok((amount_out as f64, cu, extra_infos)),
            Err(_e) => Err(PoolError::Acceptable),
        }
    }

    fn get_amount_in_for_target_price(&self, target_price_x64: u128, direction: SwapDirection) -> Result<u128, PoolError> {
        let sqrt_price_limit_x64 = ClmmMaths::price_x64_to_sqrt_price_x64(target_price_x64);
        let writable_data = self.writable_data.read().unwrap();
        match writable_data.get_amount_in_for_target_price(sqrt_price_limit_x64, direction) {
            Ok(amount_in) => Ok(amount_in as u128),
            Err(_e) => Err(PoolError::Acceptable),
        }
    }

    fn get_amount_in(&self, amount_out: u128, direction: SwapDirection) -> Result<u128, PoolError> {
        let writable_data = self.writable_data.read().unwrap();
        match writable_data.get_amount_in(amount_out as u64, direction) {
            Ok(amount_in) => Ok(amount_in as u128),
            Err(_e) => Err(PoolError::Acceptable),
        }
    }

    fn store_last_sent_slot(&self, slot: u64) {
        self.last_sent_slot.store(slot, Ordering::Relaxed);
    }

    fn get_last_sent_slot(&self) -> u64 {
        self.last_sent_slot.load(Ordering::Relaxed)
    }

    fn get_last_amount_in(&self) -> u64 {
        self.last_amount_in.load(Ordering::Relaxed)
    }

    fn store_last_amount_in(&self, amount: u64) {
        self.last_amount_in.store(amount, Ordering::Relaxed);
    }

    fn get_owner(&self) -> String {
       "".to_string()
    }
    
    fn get_price(&self) -> u128 {
        let writable_data = self.writable_data.read().unwrap();
        writable_data.price
    }

    fn get_price_and_fee(&self) -> (u128, u128) {
        let writable_data = self.writable_data.read().unwrap();
        (writable_data.price, self.fee_bps)
    }

    fn get_price_and_fee_with_inverted(&self) -> (u128, u128, u128) {
        let writable_data = self.writable_data.read().unwrap();
        (writable_data.price, writable_data.inversed_price, self.fee_bps)
    }

    fn get_prices_and_fee_with_init_check(
        &self,
        account_write_number: u64,
        initialize_sender: &Sender<PoolInitialize>
    ) -> (u128, u128, u128) {
        let mut writable_data = self.writable_data.write().unwrap();
        let accounts_to_init = writable_data.get_accounts_to_init();
        if accounts_to_init.is_empty() {
            return (writable_data.price, writable_data.inversed_price,  self.fee_bps);
        }
        initialize_sender.send(PoolInitialize {
            pool: self.address.clone(),   
            account_write_number,
            accounts_to_init,
        });
        (writable_data.price, writable_data.inversed_price, self.fee_bps)
    }

    fn get_inverted_price_and_fee(&self) -> (Option<u128>, u128) {
        let writable_data = self.writable_data.read().unwrap();
        (Some(writable_data.inversed_price), self.fee_bps)
    }

    fn get_farm_token_label(&self) -> TokenLabel {
        self.farm_token_label.clone()
    }

    fn get_routing_step(&self) -> RoutingStep {
        match self.farm_token_label {
            TokenLabel::X | TokenLabel::Y => RoutingStep::Extremity,
            TokenLabel::None => RoutingStep::Intermediate,
        }
    }

    fn get_farm_token_mint(&self) -> Option<String> {
        match self.farm_token_label {
            TokenLabel::X => Some(self.mint_x.clone()),
            TokenLabel::Y => Some(self.mint_y.clone()),
            TokenLabel::None => None,
        }
    }

    fn get_non_farm_token_mint(&self) -> Option<String> {
        match self.farm_token_label {
            TokenLabel::X => Some(self.mint_y.clone()),
            TokenLabel::Y => Some(self.mint_x.clone()),
            TokenLabel::None => None,
        }
    }

    fn get_mint_x(&self) -> String {
        self.mint_x.clone()
    }

    fn get_mint_y(&self) -> String {
        self.mint_y.clone()
    }

    fn get_associated_tracked_pubkeys(&self) -> Vec<String> {
        let writable_data = self.writable_data.read().unwrap();
        let mut tracked_pubkeys = Vec::new();
        
        // Add all tick array addresses
        for tick_array in &writable_data.tick_array_states {
            tracked_pubkeys.push(tick_array.address.clone());
        }
        
        tracked_pubkeys.push(writable_data.tickarray_bitmap_extension.address.clone());
        
        tracked_pubkeys
    }

    fn append_swap_step(
        &self,
        wallet_pubkey: Pubkey,
        accounts_meta: &mut Vec<AccountMeta>,
        data: &mut Vec<u8>,
        user_token_in: String,
        mint_in: &String,
        user_token_out: String,
        _mint_out: &String,
        extra_accounts: &Option<Vec<String>>
    ) {

        let inverse_x_y = if *mint_in == self.mint_x {
            false
        } else {
            true
        };

        let (accounts, bitmap_extension) = self.pool_info.keys.get_with_user_and_tokens(
            user_token_in,
            user_token_out,
            wallet_pubkey,
            inverse_x_y
        );

        accounts_meta.extend(accounts);
        if let Some(extra_accounts) = extra_accounts { 
            accounts_meta.extend(extra_accounts.iter().map(
                |s| AccountMeta::new(Pubkey::from_str_const(s), false)));
        }
        accounts_meta.push(bitmap_extension);
        data.push(self.pool_info.amm_flag);
        data.push((accounts_meta.len()) as u8);
        
    }

    fn add_congestion(&self, slot: u64) {
        let mut congestion_stats = self.congestion_stats.write().unwrap();
        congestion_stats.add_congestion(slot);
    }

    fn get_congestion_rate(&self) -> (f32, u64, u8) {
        let congestion_stats = self.congestion_stats.read().unwrap();
        (congestion_stats.congestion_rate, congestion_stats.last_locked_slot, *congestion_stats.congestion_history.back().unwrap())
    }
}


impl RaydiumClmmPool {
    pub async fn new(
        address: Pubkey,
        connection: Arc<RpcClient>,
        farm_token_label: TokenLabel,
        ) -> Option<Self> {
        if let Some((
            pool_info, 
            mint_x, 
            mint_y, 
            protocol_fee_rate, 
            trade_fee_rate, 
            fund_fee_rate, 
            tick_array_states, 
            tickarray_bitmap_extension, 
            tick_spacing,
            default_tickarray_bitmap
        )) = RaydiumClmmPoolInfo::new(address, connection).await {
            let fee_bps = trade_fee_rate * 1_000;
            return Some(Self {
                mint_x,
                mint_y,
                farm_token_label,
                address: address.to_string(),
                writable_data: Arc::new(RwLock::new(WritableData::new(
                    address.to_string(),
                    tick_spacing,
                    protocol_fee_rate,
                    trade_fee_rate,
                    fund_fee_rate,
                    tick_array_states,
                    tickarray_bitmap_extension,
                    default_tickarray_bitmap
                ))),
                fee_bps: fee_bps as u128,
                last_sent_slot: Arc::new(AtomicU64::new(0)),
                pool_info,
                last_amount_in: Arc::new(AtomicU64::new(0)),
                tick_spacing,
                congestion_stats: RwLock::new(CongestionStats::new_at_slot(0)),
            });
        }
        None
    }

    pub fn update_tick_array_from_data(&self, address: String, data: &[u8], write_number: u64) {
        let mut writable_data = self.writable_data.write().unwrap();
        writable_data.parse_into_tick_array_state(address, &data[8..], write_number);
    }

    pub fn update_tick_array_bitmap_extension_from_data(&self, data: &[u8], write_number: u64) {   
        let mut writable_data = self.writable_data.write().unwrap();
        writable_data.parse_into_tick_array_bitmap_extension(&data[8..], write_number);
    }

    pub fn update_liquidity_state_from_data(&self, data: &[u8], write_number: u64) {
        let mut writable_data = self.writable_data.write().unwrap();
        writable_data.parse_into_liquidity(&data[8..], write_number);
    }


    pub async fn extend_right_tick_array_states(&self, connection: &Arc<RpcClient>) -> Result<(Vec<String>, Vec<String>), Box<dyn Error + Send + Sync>> {
        let (left_count, right_count, end_index) = {
            let writable_data_r = self.writable_data.read().unwrap();
            let index = writable_data_r.get_local_tick_array_state_index(writable_data_r.tick_current).unwrap();
            let (left_count, right_count) = writable_data_r.count_tick_array_states_on_sides(index);
            let end_index = writable_data_r.tick_array_states.last().unwrap().start_tick_index;
            (left_count, right_count, end_index)
        };
        let mut pubkeys_to_remove = Vec::new();
        let mut pubkeys_str_to_add = Vec::new();

        
        if right_count < 3 {
            
            // Create vector to store new tick array pubkeys and their indices
            let mut tick_array_pubkeys = Vec::with_capacity(2);
            
            // Calculate and store pubkeys for the next 2 tick arrays
            for i in 1..3 {
                let array_index = end_index + (self.tick_spacing as i32 * 60) * i;
                let tick_array_pubkey = Pubkey::find_program_address(
                    &[
                        TICK_ARRAY_SEED.as_bytes(),
                        self.address.as_bytes().as_ref(),
                        &array_index.to_be_bytes(),
                    ],
                    &PoolConstants::RAYDIUM_CLMM_PROGRAM,
                )
                .0;
                tick_array_pubkeys.push((tick_array_pubkey, array_index));
            }

            // Get all pubkeys for batch fetching
            let all_pubkeys: Vec<Pubkey> = tick_array_pubkeys.iter()
                .map(|(pubkey, _)| *pubkey)
                .collect();

            pubkeys_str_to_add = tick_array_pubkeys.iter()
                .map(|(pubkey, _)| pubkey.to_string())
                .collect();

            // Fetch all accounts in one RPC call
            let accounts_result = if let Ok(accounts_result) = connection.get_multiple_accounts(&all_pubkeys).await {
                accounts_result
            } else {
                return Err("".into());
            };
            
            // Create vector of (pubkey, Option<AccountInfo>, array_index) for new tick arrays
            let mut tick_array_with_pubkeys = Vec::with_capacity(2);
            for (i, (pubkey, array_index)) in tick_array_pubkeys.iter().enumerate() {
                tick_array_with_pubkeys.push((*pubkey, accounts_result.get(i).unwrap().clone(), *array_index));
            }

            // Create new tick array states
            let new_tick_array_states = TickArrayState::new_vec(self.address.clone(), tick_array_with_pubkeys);
            
            // Drop the read lock and acquire write lock to modify the data
            let mut writable_data_w = self.writable_data.write().unwrap();
            
            // Extend the existing tick array states with new ones
            writable_data_w.tick_array_states.extend(new_tick_array_states);
        }

        if left_count > 5 {
            
            let mut writable_data_w = self.writable_data.write().unwrap();
            
            // Calculate how many tick arrays to remove
            let excess_count = left_count - 5;
            
            // Collect addresses before removing
            for _ in 0..excess_count {
                pubkeys_to_remove.push(writable_data_w.tick_array_states[0].address.clone());
                writable_data_w.tick_array_states.remove(0);
            }
        }
        Ok((pubkeys_to_remove, pubkeys_str_to_add))
    }

    pub async fn extend_left_tick_array_states(&self, connection: &Arc<RpcClient>) -> Result<(Vec<String>, Vec<String>), Box<dyn Error + Send + Sync>> {
        
        let (left_count, right_count, first_index) = {
            let writable_data_r = self.writable_data.read().unwrap();
            let index = writable_data_r.get_local_tick_array_state_index(writable_data_r.tick_current).unwrap();
            let (left_count, right_count) = writable_data_r.count_tick_array_states_on_sides(index);
            let first_index = writable_data_r.tick_array_states.first().unwrap().start_tick_index;
            (left_count, right_count, first_index)
        };

        let mut pubkeys_to_remove = Vec::new();
        let mut pubkeys_str_to_add = Vec::new();
        
        
        if left_count < 3 {
            // Get the last tick array's start index
            
            
            // Create vector to store new tick array pubkeys and their indices
            let mut tick_array_pubkeys = Vec::with_capacity(2);
            
            // Calculate and store pubkeys for the next 2 tick arrays
            for i in -3..0{
                let array_index = first_index + (self.tick_spacing as i32 * 60) * i;
                let tick_array_pubkey = Pubkey::find_program_address(
                    &[
                        TICK_ARRAY_SEED.as_bytes(),
                        self.address.as_bytes().as_ref(),
                        &array_index.to_be_bytes(),
                    ],
                    &PoolConstants::RAYDIUM_CLMM_PROGRAM,
                )
                .0;
                tick_array_pubkeys.push((tick_array_pubkey, array_index));
            }

            // Get all pubkeys for batch fetching
            let all_pubkeys: Vec<Pubkey> = tick_array_pubkeys.iter()
                .map(|(pubkey, _)| *pubkey)
                .collect();

            pubkeys_str_to_add = tick_array_pubkeys.iter()
                .map(|(pubkey, _)| pubkey.to_string())
                .collect();
            
            // Fetch all accounts in one RPC call
            let accounts_result = if let Ok(accounts_result) = connection.get_multiple_accounts(&all_pubkeys).await {
                accounts_result
            } else {
                return Err("".into());
            };
            
            // Create vector of (pubkey, Option<AccountInfo>, array_index) for new tick arrays
            let mut tick_array_with_pubkeys = Vec::with_capacity(2);

            for (i, (pubkey, array_index)) in tick_array_pubkeys.iter().enumerate() {
                tick_array_with_pubkeys.push((*pubkey, accounts_result.get(i).unwrap().clone(), *array_index));
            }

            // Create new tick array states
            let mut new_tick_array_states = TickArrayState::new_vec(self.address.clone(), tick_array_with_pubkeys);
            
            // Drop the read lock and acquire write lock to modify the data
            let mut writable_data_w = self.writable_data.write().unwrap();
            
            // Extend the existing tick array states with new ones
            new_tick_array_states.extend(writable_data_w.tick_array_states.drain(..));
            writable_data_w.tick_array_states = new_tick_array_states;
        }

        if right_count > 5 {
            
            let mut writable_data_w = self.writable_data.write().unwrap();
            
            // Calculate how many tick arrays to remove
            let excess_count = right_count - 5;
            
            // Collect addresses before removing
            for _ in 0..excess_count {
                pubkeys_to_remove.push(writable_data_w.tick_array_states[writable_data_w.tick_array_states.len() - 1].address.clone());
                writable_data_w.tick_array_states.pop();
            }
        }
        Ok((pubkeys_to_remove, pubkeys_str_to_add))
    }
}

pub struct SwapAccountMetas{
    pub vec: Vec<AccountMeta>,
}

impl SwapAccountMetas{
    pub fn get_with_user_and_tokens(
        &self, 
        in_mint: String, 
        out_mint: String, 
        user_pubkey: Pubkey,
        inverse_x_y: bool
    ) -> (Vec<AccountMeta>, AccountMeta) {
        let mut vec = self.vec.clone();
        vec[4] = AccountMeta::new(Pubkey::from_str_const(&in_mint), false);
        vec[5] = AccountMeta::new(Pubkey::from_str_const(&out_mint), false);
        vec[1] = AccountMeta::new(user_pubkey, true);

        if inverse_x_y {
            // Swap positions 7 and 8
            let temp = vec[6].clone();
            vec[6] = vec[7].clone();
            vec[7] = temp;    
        }
        
        // Pop the last element and return it separately
        let bitmap_extension = vec.pop().unwrap();
        (vec, bitmap_extension)
    }
}


pub struct RaydiumClmmPoolInfo {
        pub keys: SwapAccountMetas,
        pub amm_flag: u8, // Assuming amm_flag is a u8, adjust if needed
}

impl RaydiumClmmPoolInfo {
    pub async fn new(
        address: Pubkey, 
        connection: Arc<RpcClient>
    ) -> Option<(
        Self, 
        String, 
        String, 
        u32, 
        u32, 
        u32, 
        Vec<TickArrayState>, 
        TickArrayBitmapExtension, 
        u16, 
        [u64; 16]
    )> {
        if let Some(pool_state_account) = connection.get_account(&address).await.ok() {
            let data = pool_state_account.data;
            let amm_config_pubkey = Pubkey::new_from_array(data[9..41].try_into().unwrap());
            let mint_x = Pubkey::new_from_array(data[73..105].try_into().unwrap());
            let mint_y = Pubkey::new_from_array(data[105..137].try_into().unwrap());
            let reserve_x = Pubkey::new_from_array(data[137..169].try_into().unwrap());
            let reserve_y = Pubkey::new_from_array(data[169..201].try_into().unwrap());
            let observation_state_pubkey = Pubkey::new_from_array(data[201..233].try_into().unwrap());
            let tick_spacing: u16 = u16::from_le_bytes(data[235..237].try_into().unwrap());
            let tick_current: i32 = i32::from_le_bytes(data[269..273].try_into().unwrap());
            let mut default_tickarray_bitmap = [0u64; 16];
            for i in 0..16 {
                let start = 896 + i * 8;
                default_tickarray_bitmap[i] = u64::from_le_bytes(data[start..start + 8].try_into().unwrap());
            }
            let start_index = RaydiumClmmPoolInfo::get_array_start_index(tick_current, tick_spacing);
            // Create a vector of 7 tick array indices centered around start_index
            let mut tick_array_pubkeys = Vec::with_capacity(7);
            for i in -3..=3 {
                
                let array_index = RaydiumClmmPoolInfo::get_array_start_index(start_index + (tick_spacing as i32 * 60) * i, tick_spacing);
                let tick_array_pubkey = Pubkey::find_program_address(
                    &[
                        TICK_ARRAY_SEED.as_bytes(),
                        address.to_bytes().as_ref(),
                        &array_index.to_be_bytes(),
                    ],
                    &PoolConstants::RAYDIUM_CLMM_PROGRAM,
                )
                .0;
                tick_array_pubkeys.push((tick_array_pubkey, array_index));
            }

            // Get the bitmap extension pubkey
            let tickarray_bitmap_extension = Pubkey::find_program_address(
                &[
                    POOL_TICK_ARRAY_BITMAP_SEED.as_bytes(),
                    address.to_bytes().as_ref(),
                ],
                &PoolConstants::RAYDIUM_CLMM_PROGRAM,
            )
            .0;

            // Combine all pubkeys for batch fetching
            let mut all_pubkeys = tick_array_pubkeys.iter().map(|(pubkey, _)| *pubkey).collect::<Vec<_>>();
            all_pubkeys.push(tickarray_bitmap_extension);
            all_pubkeys.push(amm_config_pubkey);

            // Fetch all accounts in one RPC call
            let accounts_result = connection.get_multiple_accounts(&all_pubkeys).await.ok()?;
            
            // Create a vector of (pubkey, Option<AccountInfo>, array_index) to keep track of all pubkeys
            let mut tick_array_with_pubkeys = Vec::with_capacity(7);
            for (i, (pubkey, array_index)) in tick_array_pubkeys.iter().enumerate() {
                tick_array_with_pubkeys.push((*pubkey, accounts_result.get(i).unwrap().clone(), *array_index));
            }
            let bitmap_account = &accounts_result[7];
            let amm_config_account = &accounts_result[8];

            let (protocol_fee_rate, trade_fee_rate, fund_fee_rate) = {
                if let Some(amm_config_account) = amm_config_account {
                    let data = &amm_config_account.data;
                    (
                        u32::from_le_bytes(data[43..47].try_into().unwrap()),
                        u32::from_le_bytes(data[47..51].try_into().unwrap()),
                        u32::from_le_bytes(data[53..57].try_into().unwrap())
                    )
                } else {
                    return None;
                }
            };

            let tick_array_states = TickArrayState::new_vec(address.to_string(), tick_array_with_pubkeys);
            let bitmap_extension = if let Some(bitmap_account) = bitmap_account {
                TickArrayBitmapExtension::initialize_from_data(
                    address.to_string(),
                    tickarray_bitmap_extension.to_string(),
                    bitmap_account.data.as_ref()
                )
            } else {
                TickArrayBitmapExtension::new(address.to_string(), tickarray_bitmap_extension.to_string())
            };
            let clmm_program_id = PoolConstants::RAYDIUM_CLMM_PROGRAM;
            let keys = SwapAccountMetas {
                vec: vec![
                    AccountMeta::new(clmm_program_id, false),
                    AccountMeta::default(),
                    AccountMeta::new_readonly(amm_config_pubkey, false),
                    AccountMeta::new(address, false),
                    AccountMeta::default(),
                    AccountMeta::default(),
                    AccountMeta::new_readonly(mint_x, false),
                    AccountMeta::new_readonly(mint_y, false),
                    AccountMeta::new(reserve_x, false),
                    AccountMeta::new(reserve_y, false),
                    AccountMeta::new(observation_state_pubkey, false),
                    AccountMeta::new_readonly(TOKEN_PROGRAM_ACCOUNT, false),
                    AccountMeta::new_readonly(tickarray_bitmap_extension, false),
                ]
            };
            return Some((Self {
                keys,
                amm_flag: 4,
            }, 
            mint_x.to_string(),
            mint_y.to_string(),
            trade_fee_rate,
            protocol_fee_rate,
            fund_fee_rate,
            tick_array_states,
            bitmap_extension,
            tick_spacing,
            default_tickarray_bitmap
        ));
        }
        None
    }

    pub fn tick_count(tick_spacing: u16) -> i32 {
        TICK_ARRAY_SIZE * i32::from(tick_spacing)
    }

    pub fn get_array_start_index(tick_index: i32, tick_spacing: u16) -> i32 {
        let ticks_in_array = RaydiumClmmPoolInfo::tick_count(tick_spacing);
        let mut start = tick_index / ticks_in_array;
        if tick_index < 0 && tick_index % ticks_in_array != 0 {
            start = start - 1
        }
        start * ticks_in_array
    }


}
