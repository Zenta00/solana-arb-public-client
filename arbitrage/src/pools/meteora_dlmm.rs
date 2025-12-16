use {
    crossbeam_channel::Sender,
    crate::{
        tokens::pools_searcher::FarmableTokens,
        pools::{
            price_maths::{find_new_target_price, price_differ, Fee, V4Maths}, raydium_v4::RaydiumV4Pool, *}, sender::{
        }, simulation::{simulator_safe_maths::*}, simulation::{simulator_safe_maths::{*}}

    }, borsh::{BorshDeserialize, BorshSerialize}, meteora::swap::{
        meteora_pool_info, MeteoraDlmmPoolInfo
    }, rand::Rng, rustls::crypto::hmac::Key, solana_client::nonblocking::rpc_client::RpcClient, solana_sdk::{
        clock::Slot, compute_budget::ComputeBudgetInstruction, hash::Hash as BlockHash, instruction::{AccountMeta, Instruction}, pubkey::Pubkey, signature::{
            Signature,
            Signer,
        }, system_instruction, transaction::{Transaction, VersionedTransaction}
    }, 
    std::{cmp::{Eq, PartialEq}, error::Error, f32::consts::E, hash::{Hash, Hasher}, 
    sync::{atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering}, Arc, RwLock}, time::{SystemTime, UNIX_EPOCH, Instant, Duration}}, tracing::{debug, error, info}
};

pub const BASIS_POINT_MAX: u16 = 10000;
pub const BASIS_POINT_MAX_U32: u32 = BASIS_POINT_MAX as u32;
pub const MAX_FEE_RATE: u128 = 100_000_000;

pub const BIN_SIZE: usize = 128;
const MAX_BIN_PER_ARRAY: i32 = 70;
pub const MAIN_STRUCT_SIZE: usize = 8 + 1 + 7 + 32 + BIN_SIZE * 70;

pub const MAX_SHIFT: u128 = 1 << 64;
const MAX_SHIFT_CONST: u32 = 64;
const HALF_MAX_SHIFT_CONST: u32 = 32;
const QUARTER_MAX_SHIFT_CONST: u32 = 16;
const SQRT_MAX_SHIFT: u64 = 1 << 32;

const ACCOUNTS_META_VEC_LEN: usize = 32;
const OVERALL_MAX_DEPTH: usize = 8;

const COMPUTE_TYPE_BASIC: u8 = 0;
const COMPUTE_TYPE_PRE_CALCULATED: u8 = 1;

const MIN_FEE_COST: u64 = 5_000;

const METEORA_BUDGET: u32 = 63_000;

const PROGRAM_BUDGET: u32 = 14_000;
const METEORA_DEPTH_INCREASE: u32 = 5_000;

pub const DEFAULT_ACCOUNTS_META_SIZE: usize = 17;

pub const K_CACHE_SIZE: i32 = 11;

#[derive(Debug, Clone)]
pub struct StaticParameters {
    pub base_factor: u16,
    pub filter_period: u16,
    pub decay_period: u16,
    pub reduction_factor: u16,
    pub variable_fee_control: u32,
    pub max_volatility_accumulator: u32,
    pub base_fee_power_factor: u32,
}

#[derive(Debug, Clone)]
pub struct VariableParameters {
    pub volatility_accumulator: u32,
    pub volatility_reference: u32,
    pub index_reference: i32,
    pub last_update_timestamp: i64,
}

#[derive(Debug, Clone)]
pub struct LiquidityStateDLMM {
    pub parameters: StaticParameters,
    pub v_parameters: VariableParameters,
    pub active_id: i32,
    pub bin_step: u16,
    pub write_number: u64,
}
pub fn parse_liquidity_state_dlmm(buffer: &[u8], write_number: u64) -> LiquidityStateDLMM {
    let mut offset = 8;

    let parameters = StaticParameters {
        base_factor: u16::from_le_bytes(buffer[offset..offset+2].try_into().unwrap()),
        filter_period: u16::from_le_bytes(buffer[offset+2..offset+4].try_into().unwrap()),
        decay_period: u16::from_le_bytes(buffer[offset+4..offset+6].try_into().unwrap()),
        reduction_factor: u16::from_le_bytes(buffer[offset+6..offset+8].try_into().unwrap()),
        variable_fee_control: u32::from_le_bytes(buffer[offset+8..offset+12].try_into().unwrap()),
        max_volatility_accumulator: u32::from_le_bytes(buffer[offset+12..offset+16].try_into().unwrap()),
        base_fee_power_factor: buffer[offset+26] as u32,
    };
    offset += 32; // 26 + 6 padding

    let v_parameters = VariableParameters {
        volatility_accumulator: u32::from_le_bytes(buffer[offset..offset+4].try_into().unwrap()),
        volatility_reference: u32::from_le_bytes(buffer[offset+4..offset+8].try_into().unwrap()),
        index_reference: i32::from_le_bytes(buffer[offset+8..offset+12].try_into().unwrap()),
        //skip padding
        last_update_timestamp: i64::from_le_bytes(buffer[offset+16..offset+24].try_into().unwrap()),
    };
    offset += 32; // 24 + 8 padding

    // Skip bumpSeed, binStepSeed, and pairType
    offset += 4;

    let active_id = i32::from_le_bytes(buffer[offset..offset+4].try_into().unwrap());
    offset += 4;

    let bin_step = u16::from_le_bytes(buffer[offset..offset+2].try_into().unwrap());

    LiquidityStateDLMM {
        parameters,
        v_parameters,
        active_id,
        bin_step,
        write_number,
    }
}


#[derive(Debug)]
pub enum BinError {
    InvalidData,
    ParseError,
    BinNotFound,
    ActiveBinNotFound,
    InvalidBinIndex(i32),
}

impl std::fmt::Display for BinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BinError::InvalidData => write!(f, "Invalid bin data"),
            BinError::ParseError => write!(f, "Error parsing bin"),
            BinError::BinNotFound => write!(f, "Bin not found"),
            BinError::ActiveBinNotFound => write!(f, "Active bin not found"),
            BinError::InvalidBinIndex(idx) => write!(f, "Invalid bin index: {}", idx),
        }
    }
}

impl std::error::Error for BinError {}

pub struct BinPriceReference{
    pub prev_reserve_x: u128,
    pub current_reserve_x: u128,
    pub next_reserve_x: u128,
}

impl BinPriceReference{
    pub fn new(prev_reserve_x: u128, current_reserve_x: u128, next_reserve_x: u128) -> Self {
        Self { prev_reserve_x, current_reserve_x, next_reserve_x }
    }
}

impl PartialEq for BinPriceReference {
    fn eq(&self, other: &Self) -> bool {
        self.prev_reserve_x == other.prev_reserve_x &&
        self.current_reserve_x == other.current_reserve_x &&
        self.next_reserve_x == other.next_reserve_x
    }
}

impl Eq for BinPriceReference {}

pub struct BinContainers {
    pub pool_address: String,
    pub bin_containers: Vec<BinContainer>,
    pub active_bin_container_index: usize,

}

impl BinContainers {
    pub fn new_from_pool_info(
        pool_address: String,
        bin_array_pubkeys: Vec<(Pubkey, i32)>,
        active_bin_container_index: usize
    ) -> Self {
        let mut bin_containers = Vec::new();
        for (bin_array_pubkey, index) in bin_array_pubkeys {
            bin_containers.push(BinContainer::new_empty(bin_array_pubkey.to_string(), index));
        }
        Self { pool_address, bin_containers, active_bin_container_index }
    }

    pub fn get_active_bin_container(&self) -> &BinContainer {
        self.bin_containers.get(self.active_bin_container_index as usize).unwrap()
    }

    pub fn get_near_bin_containers_address(&self) -> (String, String, String) {

        if self.active_bin_container_index == 0 {
            let next_bin_container = if self.bin_containers.len() > 1 {
                self.bin_containers[1].address.clone()
            } else {
                self.bin_containers[0].address.clone()
            };
            return (self.bin_containers[0].address.clone(), self.bin_containers[0].address.clone(), next_bin_container);
        }
        else if self.active_bin_container_index == self.bin_containers.len() - 1 {
            let previous_bin_container = if self.bin_containers.len() > 1 {
                self.bin_containers[self.bin_containers.len() - 2].address.clone()
            } else {
                self.bin_containers[0].address.clone()
            };
            return (previous_bin_container, self.bin_containers[self.bin_containers.len() - 1].address.clone(), self.bin_containers[self.bin_containers.len() - 1].address.clone());
        }
        else {
            let previous_bin_container = self.bin_containers[self.active_bin_container_index as usize - 1].address.clone();
            let active_bin_container = self.bin_containers[self.active_bin_container_index as usize].address.clone();
            let next_bin_container = self.bin_containers[self.active_bin_container_index as usize + 1].address.clone();

            (previous_bin_container, active_bin_container, next_bin_container)
        }
    }

    pub fn get_bin_price_reference(&self, active_id: i32) -> Result<BinPriceReference, BinError> {
        let active_bin_container = self.get_active_bin_container();
        if !active_bin_container.is_active_bin(active_id) {
            return Err(BinError::ActiveBinNotFound);
        }
        
        let active_bin_index = bin_id_to_bin_array_index(active_id) as usize;
        let current_reserve_x = active_bin_container.bins[active_bin_index].amount_x;
        
        let mut prev_reserve_x = 0;
        let mut next_reserve_x = 0;
        
        // Handle previous bin
        if active_bin_index == 0 {
            // Need to get the last bin from the previous container
            let active_container_idx = self.active_bin_container_index;
            if active_container_idx > 0 {
                let prev_container = &self.bin_containers[active_container_idx - 1];
                prev_reserve_x = prev_container.bins[69].amount_x;
            }
            else {
                println!("Error: No previous container found");
            }
        } else {
            let prev_bin_index = active_bin_index - 1;
            prev_reserve_x = active_bin_container.bins[prev_bin_index].amount_x;
        }
        
        // Handle next bin
        if active_bin_index == 69 {
            // Need to get the first bin from the next container
            let active_container_idx = self.active_bin_container_index;
            if active_container_idx < self.bin_containers.len() - 1 {
                let next_container = &self.bin_containers[active_container_idx + 1];
                next_reserve_x = next_container.bins[0].amount_x;
            }
            else {
                println!("Error: No next container found");
            }
        } else {
            let next_bin_index = active_bin_index + 1;
            next_reserve_x = active_bin_container.bins[next_bin_index].amount_x;
        }
            
        Ok(BinPriceReference::new(prev_reserve_x, current_reserve_x, next_reserve_x))
    }
    
    pub fn parse_into_bin(&mut self, address: String, data: &[u8], write_number: u64) -> Result<(), BinError>{
        for bin_container in self.bin_containers.iter_mut() {
            if bin_container.address == address {
                bin_container.parse(data, write_number);
                return Ok(());
            }
        }
        Err(BinError::BinNotFound)
    }

    pub fn set_price(
        &mut self, 
        active_id: i32,
        update_pool_sender: &Sender<(String, PoolUpdateType)>
    ) -> Result<u128, PoolError> {
        let active_bin_container = self.get_active_bin_container();
        if active_bin_container.is_active_bin(active_id) {
            let active_bin_index = bin_id_to_bin_array_index(active_id) as usize;
            return match active_bin_container.bins.get(active_bin_index){
                Some(bin) => Ok(bin.price),
                None => Ok(0)
            };
        }

        let previous_active_bin_index = self.active_bin_container_index;
        let total_bin_containers = self.bin_containers.len();
        for (bin_index, bin_container) in self.bin_containers.iter_mut().enumerate() {
            if bin_container.is_active_bin(active_id) {
                self.active_bin_container_index = bin_index;
                if bin_index < previous_active_bin_index {
                    if bin_index == 0 {
                        update_pool_sender.send((self.pool_address.clone(), PoolUpdateType::NeedLeftBins)).unwrap();
                    }
                } else if bin_index > previous_active_bin_index {
                    if bin_index == total_bin_containers - 1 {
                        update_pool_sender.send((self.pool_address.clone(), PoolUpdateType::NeedRightBins)).unwrap();
                    }
                }
                let active_bin_index = bin_id_to_bin_array_index(active_id) as usize;
                return match bin_container.bins.get(active_bin_index){
                    Some(bin) => Ok(bin.price),
                    None => Ok(0)
                };
            }
        }

        Err(PoolError::NeedReinitialize(vec![]))
    }

    pub fn get_amount_out(
        &mut self, 
        active_id: i32, 
        amount_in: u128, 
        direction: SwapDirection, 
        extra_infos: bool,
        fee_data: &mut FeeData
    ) -> Result<(f64, i32, Option<Vec<String>>), PoolError> {
        let mut amount_out = 0u128;
        let mut amount_in_remaining = amount_in;
        let mut current_bin_container = self.get_bin_container_from_id(active_id)
            .ok_or(PoolError::NeedReinitialize(vec![]))?;
        let mut current_bin_index = bin_id_to_bin_array_index(active_id);
        let mut loaded_bin_container_count = 0;
        let mut k = 0;
        let mut extra_infos_vec = if extra_infos {
            vec![current_bin_container.address.clone()]
        } else {
            vec![]
        };
        let mut require_non_zero_bin = false;
        match direction {
            SwapDirection::XtoY => {
                while amount_in_remaining > 0 {
                    let fee_bps = fee_data.get_fee_bps_with_k(k);
                    
                    let current_bin = current_bin_container.get_bin_from_idx(current_bin_index);

                    if current_bin.price == 0 {
                        break;
                    }

                    

                    let (max_amount_in_bin, max_amount_out_bin) = current_bin.max_amount_in_bin(SwapDirection::XtoY);
                    let max_amount_in_with_fee = FeeData::compute_max_amount_in_with_fee_bin(max_amount_in_bin, fee_bps);
                    if amount_in_remaining > max_amount_in_with_fee {
                        if current_bin.amount_y == 0 {
                            if require_non_zero_bin {
                                k+= 1;
                                break;
                            }
                            else {
                                k-=1;
                                require_non_zero_bin = true;
                            }
                        } else {
                            amount_in_remaining -= max_amount_in_with_fee;
                            amount_out += max_amount_out_bin;
                            k -= 1;
                        }
                    } else {
                        let amount_remaining_with_fee = FeeData::compute_amount_in_with_fee_bin(amount_in_remaining, fee_bps);
                        amount_out += current_bin.amount_out_bin(SwapDirection::XtoY, amount_remaining_with_fee);
                        break;
                    }

                    if current_bin_index == 0 {
                        loaded_bin_container_count += 1;
                        current_bin_container = match self.get_bin_container_from_id(
                            active_id - MAX_BIN_PER_ARRAY as i32 * loaded_bin_container_count
                        ) {
                            Some(bin_container) => {
                                extra_infos_vec.push(bin_container.address.clone());
                                bin_container
                            },
                            None => {
                                let extra_infos = if extra_infos_vec.len() > 0 {
                                    if let Some(next_bin_container) = self.get_bin_container_from_id(
                                        active_id - MAX_BIN_PER_ARRAY as i32 * (loaded_bin_container_count + 1)
                                    ) {
                                        extra_infos_vec.push(next_bin_container.address.clone());
                                    }
                                    Some(extra_infos_vec)
                                } else {
                                    None
                                };
                                return Ok((amount_out as f64, k, extra_infos));
                            }
                        };
                        current_bin_index = MAX_BIN_PER_ARRAY - 1;
                    } else {
                        current_bin_index -= 1;
                    }
                }

                let extra_infos = if extra_infos_vec.len() > 0 {
                    if let Some(next_bin_container) = self.get_bin_container_from_id(
                        active_id - MAX_BIN_PER_ARRAY as i32 * (loaded_bin_container_count + 1)
                    ) {
                        extra_infos_vec.push(next_bin_container.address.clone());
                    }
                    Some(extra_infos_vec)
                } else {
                    None
                };
                Ok((amount_out as f64, k, extra_infos))
            }
            SwapDirection::YtoX =>{
                while amount_in_remaining > 0{
                    let fee_bps = fee_data.get_fee_bps_with_k(k);
                    let current_bin = current_bin_container.get_bin_from_idx(current_bin_index);

                    if current_bin.price == 0 {
                        break;
                    }


                    let (max_amount_in_bin, max_amount_out_bin) = current_bin.max_amount_in_bin(SwapDirection::YtoX);
                    let max_amount_in_with_fee = FeeData::compute_max_amount_in_with_fee_bin(max_amount_in_bin, fee_bps);
                    if amount_in_remaining > max_amount_in_with_fee {
                        if current_bin.amount_x == 0 {
                            if require_non_zero_bin {
                                k-= 1;
                                break;
                            }
                            else {
                                k+=1;
                                require_non_zero_bin = true;
                            }
                        } else {
                            amount_in_remaining -= max_amount_in_with_fee;
                            amount_out += max_amount_out_bin;
                            k += 1;
                        }
                    } else {
                        let amount_remaining_with_fee = FeeData::compute_amount_in_with_fee_bin(amount_in_remaining, fee_bps);
                        amount_out += current_bin.amount_out_bin(SwapDirection::YtoX, amount_remaining_with_fee);
                        break;
                    }

                    if current_bin_index == MAX_BIN_PER_ARRAY -1 {
                        loaded_bin_container_count += 1;
                        current_bin_container = 
                            match self.get_bin_container_from_id(
                                active_id + MAX_BIN_PER_ARRAY as i32 * loaded_bin_container_count
                            ) {
                                Some(bin_container) => {
                                    extra_infos_vec.push(bin_container.address.clone());
                                    bin_container
                                },
                                None => {
                                    let extra_infos = if extra_infos_vec.len() > 0 {
                                        if let Some(next_bin_container) = self.get_bin_container_from_id(
                                            active_id + MAX_BIN_PER_ARRAY as i32 * (loaded_bin_container_count + 1)
                                        ) {
                                            extra_infos_vec.push(next_bin_container.address.clone());
                                        }
                                        Some(extra_infos_vec)
                                    } else {
                                        None
                                    };
                                    return Ok((amount_out as f64, k, extra_infos));
                                }
                            };
                        current_bin_index = 0;
                    } else {
                        current_bin_index += 1;
                    };
                }
                let extra_infos = if extra_infos_vec.len() > 0 {
                    if let Some(next_bin_container) = self.get_bin_container_from_id(
                        active_id + MAX_BIN_PER_ARRAY as i32 * (loaded_bin_container_count + 1)
                    ) {
                        extra_infos_vec.push(next_bin_container.address.clone());
                    }
                    Some(extra_infos_vec)
                } else {
                    None
                };

                Ok((amount_out as f64, k, extra_infos))
            }
            _ => {
                return Ok((0.0,0, None));
            }
        }
        
    }

    pub fn get_bin_container_from_id(&self, active_id: i32) -> Option<&BinContainer> {
        for bin_container in self.bin_containers.iter() {
            if active_id >= bin_container.lower_bin_id && active_id <= bin_container.upper_bin_id {
                return Some(&bin_container);
            }
        }
        None
    }

    pub fn get_amount_in_for_target_price(
        &self, 
        active_id: i32, 
        target_price: u128, 
        direction: SwapDirection
    ) -> Result<u128, PoolError> {
        let mut amount_in = 0u128;
        let mut current_bin_container = self.get_bin_container_from_id(active_id)
            .ok_or(PoolError::NeedReinitialize(vec![]))?;
        let mut current_bin_index = bin_id_to_bin_array_index(active_id);
        let mut require_non_zero_bin = false;
        let mut loaded_bin_container_count = 0;

        match direction {
            SwapDirection::XtoY => {
                loop {
                    let current_bin = current_bin_container.get_bin_from_idx(current_bin_index);
                    let bin_price = current_bin.price;
                    let bin_reserve_y = current_bin.amount_y;

                    if bin_price == 0 || bin_price <= target_price{
                        break;
                    }

                    if bin_reserve_y == 0 {
                        if require_non_zero_bin {
                            break;
                        }
                        else {
                            require_non_zero_bin = true;
                        }
                    }else {
                        amount_in += (bin_reserve_y << 64) / bin_price;
                    }
                    
                    current_bin_index = if current_bin_index == 0 {
                        loaded_bin_container_count += 1;
                        current_bin_container = match self.get_bin_container_from_id(
                            active_id - MAX_BIN_PER_ARRAY as i32 * loaded_bin_container_count
                        ) {
                            Some(bin_container) => bin_container,
                            None => {
                                return Ok(amount_in);
                            }
                        };
                        MAX_BIN_PER_ARRAY - 1
                    } else {
                        current_bin_index - 1
                    };
                }
            }
            SwapDirection::YtoX => {
                loop {
                    let current_bin = current_bin_container.get_bin_from_idx(current_bin_index);
                    let bin_price = current_bin.price;
                    let bin_reserve_x = current_bin.amount_x;

                    if bin_price == 0 || bin_price >= target_price{
                        break;
                    }

                    // Q64.64 fixed-point arithmetic
                    if bin_reserve_x == 0 {
                        if require_non_zero_bin {
                            break;
                        }
                        else {
                            require_non_zero_bin = true;
                        }
                    }else {
                        amount_in += (bin_reserve_x * bin_price) >> 64;
                    }
                    
                    if current_bin_index == MAX_BIN_PER_ARRAY - 1 {
                        loaded_bin_container_count += 1;
                        current_bin_container = match self.get_bin_container_from_id(
                            active_id + MAX_BIN_PER_ARRAY as i32 * loaded_bin_container_count
                        ) {
                            Some(bin_container) => bin_container,
                            None => {
                                return Ok(amount_in);
                            }
                        };
                        current_bin_index = 0;
                    } else {
                        current_bin_index += 1;
                    };
                }
            }
            _ => {
                return Ok(0);
            }
        }
        return Ok(amount_in);
        
    }

    pub fn get_amount_in(&self, active_id: i32, amount_out: u128, direction: SwapDirection) -> Result<(f64, i32), PoolError> {
        let mut amount_in = 0u128;
        let mut amount_out_remaining = amount_out;
        let mut current_bin_container = self.get_bin_container_from_id(active_id)
            .ok_or(PoolError::NeedReinitialize(vec![]))?;
        let mut current_bin_index = bin_id_to_bin_array_index(active_id);
        let mut loaded_bin_container_count = 0;
        let mut k = 0;
        let mut require_non_zero_bin = false;
        
        match direction {
            SwapDirection::XtoY => {
                while amount_out_remaining > 0 {
                    let current_bin = current_bin_container.get_bin_from_idx(current_bin_index);
                    let bin_price = current_bin.price;
                    let bin_reserve_y = current_bin.amount_y;

                    if bin_price == 0 {
                        break;
                    }

                    

                    if amount_out_remaining <= bin_reserve_y {
                        // Can fulfill entire remaining output from this bin
                        // Calculate required input: amount_in = amount_out / price (in Q64.64)
                        amount_in += (amount_out_remaining << 64) / bin_price;
                        amount_out_remaining = 0;
                        break;
                    } else {
                        // Can only partially fulfill from this bin
                        // Calculate input needed to use all of bin's reserve
                        if bin_reserve_y == 0  {
                            if require_non_zero_bin{
                                k+=1;
                                break;
                            }
                            else {
                                k-=1;
                                require_non_zero_bin = true;
                            }
                        } else {
                            amount_in += (bin_reserve_y << 64) / bin_price;
                            amount_out_remaining -= bin_reserve_y;
                            k -= 1;
                        }
                    }

                    if current_bin_index == 0 {
                        loaded_bin_container_count += 1;
                        let container = self.get_bin_container_from_id(
                            active_id - MAX_BIN_PER_ARRAY as i32 * loaded_bin_container_count
                        ).ok_or(PoolError::Acceptable)?;
                        current_bin_container = container;
                        current_bin_index = MAX_BIN_PER_ARRAY - 1;
                    } else {
                        current_bin_index -= 1;
                    }
                }
                Ok((amount_in as f64, k))
            },
            SwapDirection::YtoX => {
                while amount_out_remaining > 0 {
                    let current_bin = current_bin_container.get_bin_from_idx(current_bin_index);
                    let bin_price = current_bin.price;
                    let bin_reserve_x = current_bin.amount_x;

                    if bin_price == 0{
                        break;
                    }

                    if amount_out_remaining <= bin_reserve_x {
                        // Can fulfill entire remaining output from this bin
                        // Calculate required input: amount_in = amount_out * price (in Q64.64)
                        amount_in += (amount_out_remaining * bin_price) >> 64;
                        amount_out_remaining = 0;
                        break;
                    } else {
                        // Can only partially fulfill from this bin
                        // Calculate input needed to use all of bin's reserve

                        if bin_reserve_x == 0 {
                            if require_non_zero_bin {
                                k-=1;
                                break;
                            }
                            else {
                                k+=1;
                                require_non_zero_bin = true;
                            }
                        } else {
                            amount_in += (bin_reserve_x * bin_price) >> 64;
                            amount_out_remaining -= bin_reserve_x;
                            k += 1;
                        }
                    }

                    if current_bin_index == MAX_BIN_PER_ARRAY - 1 {
                        loaded_bin_container_count += 1;
                        current_bin_container = self.get_bin_container_from_id(
                            active_id + MAX_BIN_PER_ARRAY as i32 * loaded_bin_container_count
                        ).ok_or(PoolError::Acceptable)?;
                        current_bin_index = 0;
                    } else {
                        current_bin_index += 1;
                    }
                }
                Ok((amount_in as f64, k))
            },
            _ => {
                return Ok((0.0, 0));
            }
        }
    }

}


#[derive(Debug, Clone)]
pub struct Bin {
    pub amount_x: u128,
    pub amount_y: u128,
    pub price: u128,
}

impl Bin {
    pub fn max_amount_in_bin(&self, direction: SwapDirection) -> (u128, u128) {
        match direction {
            SwapDirection::XtoY => {
                ((self.amount_y << 64) / self.price, self.amount_y)
            },
            SwapDirection::YtoX => {
                ((self.amount_x * self.price) >> 64, self.amount_x)
            },
            _ => (0, 0),
        }
    }

    pub fn amount_out_bin(&self, direction: SwapDirection, amount_in: u128) -> u128 {
        match direction {
            SwapDirection::XtoY => (amount_in * self.price) >> 64,
            SwapDirection::YtoX => (amount_in << 64) / self.price,
            _ => 0,
        }
    }
}

#[derive(Debug)]
pub struct BinContainer {
    pub address: String,
    pub index: i32,
    pub lower_bin_id: i32,
    pub upper_bin_id: i32,
    pub bins: Vec<Bin>,
    pub write_number: u64,
}

impl BinContainer {
    pub fn new_empty(address: String, index: i32) -> Self {
        let (lower_bin_id, upper_bin_id) = get_bin_array_lower_upper_bin_id(index).unwrap();
        let bins = vec![
            Bin {
                amount_x: 0,
                amount_y: 0,
                price: 0,
            };
            70
        ];
        Self { address, index, lower_bin_id, upper_bin_id, bins, write_number: 0 }
    }

    pub fn parse(&mut self, buffer: &[u8], write_number: u64) {
        if write_number < self.write_number {
            return;
        }
        let mut offset = 0;
        self.index = i64::from_le_bytes(buffer[offset+8..offset+16].try_into().unwrap()) as i32;
        offset += 56; // version (1), padding (7), lb_pair (32)
        let bins = &mut self.bins;
        
        // Parse 70 bins
        for i in 0..70 {
            let bin = Bin {
                amount_x: u64::from_le_bytes(buffer[offset..offset+8].try_into().unwrap()) as u128,
                amount_y: u64::from_le_bytes(buffer[offset+8..offset+16].try_into().unwrap()) as u128,
                price: u128::from_le_bytes(buffer[offset+16..offset+32].try_into().unwrap()),
            };
            bins[i] = bin;
            offset += 144;
        }
        self.write_number = write_number;
    }

    pub fn get_bin_from_idx(&self, idx: i32) -> &Bin {
        &self.bins[idx as usize]
    }

    pub fn is_active_bin(&self, active_id: i32) -> bool {
        active_id >= self.lower_bin_id && active_id <= self.upper_bin_id
    }
}


pub fn bin_id_to_bin_array_index(bin_id: i32) -> i32 {
    let mut idx = bin_id % 70;

    if idx < 0 {
        idx += 70;
    }
    idx
}

fn get_current_time() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

#[derive(PartialEq, Eq, Clone, Debug)]
pub enum FeeState {
    Recent,
    Filtered,
    Decayed,
    Unset,
}

#[derive(Debug, Clone)]
pub struct FeeData {
    pub base_factor: u128,
    pub filter_period: u16,
    pub decay_period: u16,
    pub reduction_factor: u16,
    pub variable_fee_control: u128,
    pub volatility_reference: u32,
    pub index_reference: i32,
    pub active_id: i32,
    pub max_volatility_accumulator: u32,
    pub base_fee_power_factor: u32,
    pub volatility_accumulator: u32,
    pub bin_step: u128,
    pub bin_step_pow_2_times_fee_control: u128,
    pub base_fee: u128,
    pub k_cache: [u128; K_CACHE_SIZE as usize],
    pub last_update_timestamp: i64,
    pub fee_state: FeeState,
}

impl FeeData {
    pub fn new(
        base_factor: u128,
        filter_period: u16,
        decay_period: u16,
        reduction_factor: u16,
        variable_fee_control: u128,
        max_volatility_accumulator: u32,
        base_fee_power_factor: u32,
        volatility_accumulator: u32,
        volatility_reference: u32,
        index_reference: i32,
        last_update_timestamp: i64,
        active_id: i32,
        bin_step: u128,
    ) -> (Self, u128) {
        let current_time = get_current_time();
        let delta_time = current_time - last_update_timestamp;
        let mut index_reference = index_reference;
        let mut volatility_reference = volatility_reference;

        if delta_time >= filter_period as i64 {
            index_reference = active_id;
            if delta_time < decay_period as i64 {
                volatility_reference =
                    reduction_factor as u32 * volatility_accumulator / BASIS_POINT_MAX_U32
            } else {
                volatility_reference = 0
            }
        }

        let bin_step_pow_2_times_fee_control = bin_step * bin_step * variable_fee_control;
        let base_fee = base_factor * bin_step * 10u128 * (10u128.pow(base_fee_power_factor));

        let fee_bps = {
            assert!(volatility_accumulator <= max_volatility_accumulator);
            let volatility_accumulator = volatility_accumulator as u128;
            let va_k_pow_2 = volatility_accumulator * volatility_accumulator;
    
            let v_fee = va_k_pow_2 * bin_step_pow_2_times_fee_control;
            let mut fee = ((v_fee + 99_999_999_999) / 100_000_000_000) + base_fee;
            fee = std::cmp::min(fee, MAX_FEE_RATE);
            fee
        };

        (Self {
            base_factor,
            filter_period,
            decay_period,
            reduction_factor,
            variable_fee_control,
            max_volatility_accumulator,
            base_fee_power_factor,
            volatility_accumulator,
            volatility_reference,
            index_reference,
            active_id,
            bin_step,
            bin_step_pow_2_times_fee_control,
            base_fee,
            k_cache: [0; K_CACHE_SIZE as usize],
            last_update_timestamp,
            fee_state: FeeState::Recent,
            
        }, fee_bps)
    }

    fn to_instantaneous_fee_data(&self, precedent_state: FeeState) -> Option<(Self, u128)> {
        let current_time = get_current_time();
        let mut index_reference = self.index_reference;
        let mut volatility_reference = self.volatility_reference;
        let mut volatility_accumulator;

        let delta_time = current_time - self.last_update_timestamp;
        let mut fee_state = FeeState::Recent;
        if delta_time >= self.filter_period as i64 {
            index_reference = self.active_id;
            fee_state = FeeState::Filtered;
            if delta_time < self.decay_period as i64 {
                volatility_reference =
                    self.reduction_factor as u32 * self.volatility_accumulator / BASIS_POINT_MAX_U32
            } else {
                volatility_reference = 0;
                fee_state = FeeState::Decayed;
            }
        }

        if precedent_state == fee_state || fee_state == FeeState::Recent {
            return None;
        }

        let bin_step_pow_2_times_fee_control = self.bin_step_pow_2_times_fee_control;
        let base_fee = self.base_fee;
        let mut k_cache = [0; K_CACHE_SIZE as usize];
        let fee_bps = {
            volatility_accumulator = (
                volatility_reference
                + ((index_reference - self.active_id).abs() as u32 * BASIS_POINT_MAX_U32)
            ) as u32;
            volatility_accumulator = std::cmp::min(volatility_accumulator, self.max_volatility_accumulator);
            let va_k_pow_2 = u128::from(volatility_accumulator).pow(2);
    
            let v_fee = va_k_pow_2 * bin_step_pow_2_times_fee_control;
            let mut fee = ((v_fee + 99_999_999_999) / 100_000_000_000) + base_fee;
            fee = std::cmp::min(fee, MAX_FEE_RATE);
            k_cache[5] = fee;
            fee
        };
        Some((FeeData {
            base_factor: 0,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0,
            volatility_reference,
            index_reference,
            active_id: self.active_id,
            max_volatility_accumulator: self.max_volatility_accumulator,
            base_fee_power_factor: 0,
            volatility_accumulator,
            bin_step: self.bin_step,
            bin_step_pow_2_times_fee_control,
            base_fee,
            k_cache,
            last_update_timestamp: current_time,
            fee_state,
        }, fee_bps))
    }

    pub fn get_fee_bps_with_k(&mut self, k: i32) -> u128 {

        let cached_fee = self.get_k_cached(k);
        if cached_fee != 0 {
            return cached_fee;
        }
        
        let volatility_accumulator = (self.volatility_reference
            + ((self.index_reference - (self.active_id + k)).abs() as u32 * BASIS_POINT_MAX_U32))
            as u128;
        let volatility_accumulator = std::cmp::min(volatility_accumulator, self.max_volatility_accumulator as u128);
        let va_k_pow_2 = volatility_accumulator * volatility_accumulator;

        let v_fee = va_k_pow_2 * self.bin_step_pow_2_times_fee_control;
        let mut fee = ((v_fee + 99_999_999_999) / 100_000_000_000) + self.base_fee;
        fee = std::cmp::min(fee, MAX_FEE_RATE);
        self.set_k_cached(k, fee);
        fee
    }

    fn get_k_cached(&mut self, k: i32) -> u128 {
        let k_index = k + (K_CACHE_SIZE / 2);
        if let Some(cached_fee) = self.k_cache.get_mut(k_index as usize) {
            *cached_fee
        } else {
            0
        }
    }

    fn set_k_cached(&mut self, k: i32, fee: u128) {
        let k_index = k + (K_CACHE_SIZE / 2);
        if let Some(cached_fee) = self.k_cache.get_mut(k_index as usize) {
            *cached_fee = fee;
        }
    }

    fn compute_max_amount_in_with_fee_bin(amount_in: u128, fee_bps: u128) -> u128 {
        amount_in * SCALE_FEE_CONSTANT / (SCALE_FEE_CONSTANT - fee_bps)
    }

    fn compute_amount_in_with_fee_bin(amount_in: u128, fee_bps: u128) -> u128 {
        amount_in * (SCALE_FEE_CONSTANT - fee_bps) / SCALE_FEE_CONSTANT
    }
}

pub struct WritableData{
    pub bin_containers: BinContainers,
    pub liquidity_state: Option<LiquidityStateDLMM>,
    pub active_bin_reserve_token: u128,
    pub active_bin_reserve_sol: u128,
    pub fee_data: Option<FeeData>,
    pub instantaneous_fee_data: Option<FeeData>,
    pub fee_bps: u128,
    pub price: u128,
    pub inversed_price: u128,
    pub active_bin_id: i32,
    pub bin_price_reference: BinPriceReference,
    pub last_sent_slot: u64,
    pub last_init_check: Instant,
}

impl WritableData {
    pub fn new_with_bin_arrays(
        pool_address: String, 
        bin_arrrays: Vec<(Pubkey, i32)>,
        active_bin_container_index: usize
        ) -> Self {
        let bin_containers = BinContainers::new_from_pool_info(
            pool_address, 
            bin_arrrays, 
            active_bin_container_index
        );
        
        Self {
            bin_containers,
            liquidity_state: None,
            active_bin_reserve_token: 0,
            active_bin_reserve_sol: 0,
            fee_data: None,
            instantaneous_fee_data: None,
            fee_bps: 0,
            price: 0,
            inversed_price: 0,
            active_bin_id: 0,
            bin_price_reference: BinPriceReference::new(0, 0, 0),
            last_sent_slot: 0,
            last_init_check: Instant::now() + Duration::from_secs(120),
        }
    }

    fn get_accounts_to_init(&mut self, address: String) -> Vec<(String, AmmType)> {
        if Instant::now().duration_since(self.last_init_check) > Duration::from_secs(60) {
            self.last_init_check = Instant::now();
        } else {
            return vec![];
        }
        let mut accounts_to_init = Vec::new();
        if self.liquidity_state.is_none() {
            accounts_to_init.push((address, AmmType::MeteoraDlmmPool));
        }
        for bin_container in self.bin_containers.bin_containers.iter_mut() {
            if bin_container.write_number == 0 {
                accounts_to_init.push((bin_container.address.clone(), AmmType::MeteoraDlmmBinArray));
                bin_container.write_number = 1;
            }
        }
        accounts_to_init
    }

    pub fn get_current_fee_data(&mut self) -> &mut FeeData {
        if let Some(instantaneous_fee_data) = self.instantaneous_fee_data.as_mut() {
            instantaneous_fee_data
        } else {
            self.fee_data.as_mut().unwrap()
        }
    }

    pub fn get_fee_bps_with_k(&mut self, k: i32) -> u128 {
        let fee_data = if let Some(instantaneous_fee_data) = self.instantaneous_fee_data.as_mut() {
            instantaneous_fee_data
        } else {
            self.fee_data.as_mut().unwrap()
        };
        fee_data.get_fee_bps_with_k(k)
    }

    pub fn get_associated_bin_array_pubkey(&self) -> Vec<String> {
        self.bin_containers.bin_containers.iter().map(|bin_container| bin_container.address.clone()).collect::<Vec<String>>()
    }

    fn get_amount_out_with_cu_extra_infos(
        &mut self, 
        amount_in: f64, 
        direction: SwapDirection
    ) -> Result<(f64, i32, Option<Vec<String>>), PoolError> {
        let active_bin_id = self.active_bin_id;
        
        // Get mutable references to both parts using split_at_mut pattern
        let (bin_containers, fee_data) = {
            let this = self as *mut Self;
            unsafe {
                (&mut (*this).bin_containers, (*this).get_current_fee_data())
            }
        };

        bin_containers.get_amount_out(
            active_bin_id,
            amount_in as u128,
            direction,
            true,
            fee_data
        )
    }

    fn get_amount_out_with_cu(
        &mut self, 
        amount_in: f64, 
        direction: SwapDirection
    ) -> Result<(f64, i32, Option<Vec<String>>), PoolError> {
        let active_bin_id = self.active_bin_id;
        
        // Get mutable references to both parts using split_at_mut pattern
        let (bin_containers, fee_data) = {
            let this = self as *mut Self;
            unsafe {
                (&mut (*this).bin_containers, (*this).get_current_fee_data())
            }
        };

        bin_containers.get_amount_out(
            active_bin_id,
            amount_in as u128,
            direction,
            false,
            fee_data
        )
    }

    fn update_state(&mut self) {
        if let Some(fee_data) = self.fee_data.as_mut() {
            let precedent_fee_state = if let Some(instantaneous_fee_data) = self.instantaneous_fee_data.as_ref() {
                instantaneous_fee_data.fee_state.clone()
            } else {
                FeeState::Unset
            };
            if let Some((instantaneous_fee_data, fee_bps)) = fee_data.to_instantaneous_fee_data(precedent_fee_state) {
                self.instantaneous_fee_data = Some(instantaneous_fee_data);
                self.fee_bps = fee_bps;
            }
        } 
    }
}

pub struct MeteoraDlmmPool {
    pub mint_x: String,
    pub mint_y: String,
    pub farm_token_label: TokenLabel,
    pub address: String,
    pub writable_data: Arc<RwLock<WritableData>>,
    pub last_sent_slot: Arc<AtomicU64>,
    pub pool_info: MeteoraDlmmPoolInfo,
    pub last_amount_in: Arc<AtomicU64>,
    pub congestion_stats: RwLock<CongestionStats>,
}

impl Hash for MeteoraDlmmPool {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.address.hash(state);
    }
}

impl PartialEq for MeteoraDlmmPool {
    fn eq(&self, other: &Self) -> bool {
        self.address == other.address
    }
}

impl Eq for MeteoraDlmmPool {}


impl Pool for MeteoraDlmmPool {

    
    fn as_meteora_dlmm(&self) -> Option<&MeteoraDlmmPool> {
        Some(self)
    }

    fn as_meteora_dlmm_mut(&mut self) -> Option<&mut MeteoraDlmmPool> {
        Some(self)
    }

    fn get_address(&self) -> String {
        self.address.to_string()
    }

    fn get_owner(&self) -> String {
        self.pool_info.owner.clone()
    }

    

    fn get_pool_type(&self) -> String {
        "Meteora-Dlmm".to_string()
    }

    fn get_liquidity_type(&self) -> LiquidityType {
        LiquidityType::Concentrated
    }

    fn get_price_and_fee(&self) -> (u128, u128) {
        let writable_data = self.writable_data.read().unwrap();
        (writable_data.price, writable_data.fee_bps)
    }

    fn get_price_and_fee_with_inverted(&self) -> (u128, u128, u128) {
        let mut writable_data = self.writable_data.write().unwrap();
        writable_data.update_state();
        (writable_data.price, writable_data.inversed_price, writable_data.fee_bps)
    }

    fn get_prices_and_fee_with_init_check(
        &self,
        account_write_number: u64,
        initialize_sender: &Sender<PoolInitialize>
    ) -> (u128, u128, u128) {
        let mut writable_data = self.writable_data.write().unwrap();
        writable_data.update_state();
        let accounts_to_init = writable_data.get_accounts_to_init(self.get_address());
        if accounts_to_init.is_empty() {
            return (writable_data.price, writable_data.inversed_price, writable_data.fee_bps);
        }
        let _ = initialize_sender.send(PoolInitialize {
            pool: self.address.clone(),   
            account_write_number,
            accounts_to_init,
        });
        (writable_data.price, writable_data.inversed_price, writable_data.fee_bps,)
    }

    fn get_inverted_price_and_fee(&self) -> (Option<u128>, u128) {
        let writable_data = self.writable_data.read().unwrap();
        (Some(writable_data.inversed_price), writable_data.fee_bps)
    }

    fn get_price(&self) -> u128 {
        let writable_data = self.writable_data.read().unwrap();
        writable_data.price
    }

    fn set_price(&self, update_pool_sender: &Sender<(String, PoolUpdateType)>) -> Result<bool, PoolError> {
        let mut writable_data = self.writable_data.write().unwrap();
        
        // First, extract all values we need from liquidity_state
        let active_id;
        let parameters;
        let v_parameters;
        let bin_step;
        
        {
            // Use a separate scope to limit the lifetime of the borrow
            let liquidity_state = 
                writable_data.liquidity_state.as_ref().ok_or("Liquidity state not found")
                .map_err(|_e| PoolError::Acceptable)?;
            
            active_id = liquidity_state.active_id;
            parameters = liquidity_state.parameters.clone();
            v_parameters = liquidity_state.v_parameters.clone();
            bin_step = liquidity_state.bin_step;
        }
        
        // Now we can modify writable_data
        writable_data.active_bin_id = active_id;
        
        // Call set_price
        let new_price = writable_data.bin_containers.set_price(active_id, update_pool_sender).map_err(
            |_e| PoolError::NeedReinitialize(self.get_all_associated_tracked_pubkeys())
        )?;

        let bin_price_reference = writable_data.bin_containers.get_bin_price_reference(active_id).map_err(
            |_e| PoolError::NeedReinitialize(self.get_all_associated_tracked_pubkeys())
        )?;
        
        
        // Create FeeData with the extracted parameters
        let price_changed = if writable_data.bin_price_reference != bin_price_reference {
            let (fee_data, fee_bps) = FeeData::new(
                parameters.base_factor as u128,
                parameters.filter_period,
                parameters.decay_period,
                parameters.reduction_factor,
                parameters.variable_fee_control as u128,
                parameters.max_volatility_accumulator,
                parameters.base_fee_power_factor,
                v_parameters.volatility_accumulator,
                v_parameters.volatility_reference,
                v_parameters.index_reference,
                v_parameters.last_update_timestamp,
                active_id,
                bin_step as u128
            );
            writable_data.price = new_price;
            writable_data.inversed_price = new_price.inv_price().unwrap();
            writable_data.fee_data = Some(fee_data);
            writable_data.fee_bps = fee_bps;
            writable_data.instantaneous_fee_data = None;
            writable_data.bin_price_reference = bin_price_reference;
            true
        } else {
            false
        };

        Ok(price_changed)
    }

    fn get_amount_in(&self, amount_out: u128, direction: SwapDirection) -> Result<u128, PoolError> {
        let mut writable_data = self.writable_data.write().unwrap();
        let (amount_in, k) = writable_data.bin_containers.get_amount_in(writable_data.active_bin_id, amount_out, direction)?;
        let fee = writable_data.get_fee_bps_with_k(k);
        let amount_in_with_fee = (amount_in as u128) * SCALE_FEE_CONSTANT / (SCALE_FEE_CONSTANT - fee);
        Ok(amount_in_with_fee)
    }

    fn get_amount_in_for_target_price(&self, target_price: u128, direction: SwapDirection) -> Result<u128, PoolError> {
        let writable_data = self.writable_data.read().unwrap();
        writable_data.bin_containers.get_amount_in_for_target_price(
            writable_data.active_bin_id, 
            target_price, 
            direction
        )
    }

    fn get_amount_out_with_cu(&self, amount_in: f64, direction: SwapDirection) -> Result<(f64, f64), PoolError> {
        let mut writable_data = self.writable_data.write().unwrap();
        let (amount_out, k, _) = 
            writable_data.get_amount_out_with_cu(
                amount_in, 
                direction
            ).map_err(|_e| PoolError::NeedReinitialize(self.get_all_associated_tracked_pubkeys()))?;

        let cu = self.estimate_cu(k);

        Ok((amount_out as f64, cu))
    }

    fn get_amount_out_with_cu_extra_infos(
        &self, 
        amount_in: f64, 
        direction: SwapDirection
    ) -> Result<(f64, f64, Option<Vec<String>>), PoolError> {
        let mut writable_data = self.writable_data.write().unwrap();
        let (amount_out, k, extra_infos) = 
            writable_data.get_amount_out_with_cu_extra_infos(
                amount_in, 
                direction
            ).map_err(|_e| PoolError::NeedReinitialize(self.get_all_associated_tracked_pubkeys()))?;

        let cu = self.estimate_cu(k);

        Ok((amount_out as f64, cu, extra_infos))
    }

    fn get_fee_bps(&self) -> u128 {
        let writable_data = self.writable_data.read().unwrap();
        writable_data.fee_bps
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
    

    fn get_mint_x(&self) -> String {
        self.mint_x.clone()
    }

    fn get_mint_y(&self) -> String {
        self.mint_y.clone()
    }

    fn get_associated_tracked_pubkeys(&self) -> Vec<String> {
        self.writable_data.read().unwrap().get_associated_bin_array_pubkey()
    }

    fn get_routing_step(&self) -> RoutingStep {
        match self.farm_token_label {
            TokenLabel::X | TokenLabel::Y => RoutingStep::Extremity,
            TokenLabel::None => RoutingStep::Intermediate,
        }
    }

    fn get_farm_token_label(&self) -> TokenLabel {
        self.farm_token_label.clone()
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

    fn append_swap_step(
        &self,
        wallet_pubkey: Pubkey,
        accounts_meta: &mut Vec<AccountMeta>, 
        data: &mut Vec<u8>, 
        user_token_in: String,
        _mint_in: &String,
        user_token_out: String,
        _mint_out: &String,
        extra_accounts: &Option<Vec<String>>
    ) {
        
        let accounts = self.pool_info.keys.get_with_user_and_tokens(
            user_token_in,
            user_token_out,
            wallet_pubkey
        );

        let extra_len;
        let accounts_len = accounts.len();
        accounts_meta.extend(accounts);
        if let Some(extra_accounts) = extra_accounts {
            extra_len = extra_accounts.len();
            
            accounts_meta.extend(extra_accounts.iter().map(
                |s| AccountMeta::new(Pubkey::from_str_const(s), false)));
        }
        else {
            extra_len = 0;
        }
        data.push(self.pool_info.amm_flag);
        data.push((accounts_len + extra_len) as u8);
        
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


impl MeteoraDlmmPool {
    pub async fn new(
        address: Pubkey,
        connection: Arc<RpcClient>,
        farm_token_label: TokenLabel,
        ) -> Option<Self> {

        let result = 
            meteora_pool_info(connection, &address).await;
        


        if let Ok((pool_info, bin_array_pubkeys, active_bin_container_index)) = result {
            Some(
                Self { 
                    mint_x: pool_info.keys.vec[6].pubkey.to_string(),
                    mint_y: pool_info.keys.vec[7].pubkey.to_string(),
                    farm_token_label,
                    address: address.to_string(),
                    writable_data: Arc::new(RwLock::new(
                        WritableData::new_with_bin_arrays(
                            address.to_string(), 
                            bin_array_pubkeys,
                            active_bin_container_index
                        )
                    )),
                    last_sent_slot: Arc::new(AtomicU64::new(0)),
                    pool_info,
                    last_amount_in: Arc::new(AtomicU64::new(0)),
                    congestion_stats: RwLock::new(CongestionStats::new_at_slot(0)),
                }
            )
        }
        else {
            //println!("Error getting meteora pool info : {:?}", result.err());
            None
        }
    }

    pub fn get_spam_arb_accounts(&self, for_init_alt: bool) -> (Vec<AccountMeta>, bool) {
        let (previous_bin_container_address, active_bin_container_address, next_bin_container_address) = 
            self.writable_data.read().unwrap().bin_containers.get_near_bin_containers_address();


        let keys = &self.pool_info.keys.vec;
        let (reserve_x, reserve_y, inverse) = if self.farm_token_label == TokenLabel::Y {
            (keys[2].clone(), keys[3].clone(), false)
        }
        else {
            (keys[3].clone(), keys[2].clone(), true)
        };
        let oracle_account = keys[8].clone();

        let accounts_meta = if for_init_alt {
            vec![
                keys[1].clone(),
                reserve_x,
                reserve_y,
                oracle_account, 
            ]
        } else {
            vec![
                keys[1].clone(),
                //AccountMeta::new(Pubkey::from_str_const(&previous_bin_container_address), false),
                AccountMeta::new(Pubkey::from_str_const(&active_bin_container_address), false),
                //AccountMeta::new(Pubkey::from_str_const(&next_bin_container_address), false),
                reserve_x,
                reserve_y,
                oracle_account, 
            ]
        };
        
        (accounts_meta, inverse)
            
    }

    pub fn update_liquidity_state_from_data(&self, data: &[u8], write_number: u64) {
        let mut writable_data = self.writable_data.write().unwrap();
        if writable_data.liquidity_state.is_some() && writable_data.liquidity_state.as_ref().unwrap().write_number >= write_number {
            return;
        }
        let liquidity_state = parse_liquidity_state_dlmm(data, write_number);
        writable_data.liquidity_state = Some(liquidity_state);

    }

    pub fn update_bin_container_from_data(&self, address: String, data: &[u8], write_number: u64) {
        let mut writable_data = self.writable_data.write().unwrap();
        writable_data.bin_containers.parse_into_bin(address, data, write_number);
    }
        
    pub fn get_fee_bps(&self) -> u128 {
        let writable_data = self.writable_data.read().unwrap();
        writable_data.fee_bps
    }

    pub fn estimate_cu(&self, k: i32) -> f64 {
        METEORA_BUDGET as f64 + (k.abs() as f64 + 2.0) * METEORA_DEPTH_INCREASE as f64
    }

    pub fn set_left_bins(
        &self, 
        bin_pubkeys: Vec<(Pubkey, i32)>,
    ) -> (Vec<String>, Vec<String>) {
        let mut writable_data = self.writable_data.write().unwrap();
        let bin_containers = &mut writable_data.bin_containers;
        let mut pubkeys_to_remove = Vec::new();
        let mut pubkeys_to_add = Vec::new();
        
        // Find the index of the first bin container that matches any bin in bin_pubkeys
        let first_bin_container_pubkey = &bin_containers.bin_containers.first().map(|bc| bc.address.clone());
        
        if let Some(first_pubkey) = first_bin_container_pubkey {
            if let Some(corresponding_index) = bin_pubkeys.iter().position(
                |(pubkey, _)| *pubkey.to_string() == *first_pubkey
            ) {
                // Get the slice of bin_pubkeys to the left of the matching bin (not including the match)
                let left_bins = &bin_pubkeys[corresponding_index + 1..];
                
                // Reverse the slice to maintain correct order when prepending
                let left_bins_reversed: Vec<_> = left_bins.iter().rev().cloned().collect();
                let count = left_bins_reversed.len();
                
                // Prepend the reversed bins to the bin_containers vector
                for (pubkey, bin_id) in left_bins_reversed {
                    bin_containers.bin_containers.insert(0, BinContainer::new_empty(
                        pubkey.to_string(), 
                        bin_id)
                    );
                    pubkeys_to_add.push(pubkey.to_string());
                }
                
                // Update the active bin container index
                bin_containers.active_bin_container_index += count;
                
                // Check if we have more than 3 bin containers to the right of the active one
                let right_bins_count = bin_containers.bin_containers.len() - bin_containers.active_bin_container_index - 1;
                if right_bins_count > 3 {
                    // Remove excess bin containers from the right
                    let to_remove = right_bins_count - 3;
                    for _ in 0..to_remove {
                        let bin_container = bin_containers.bin_containers.pop().unwrap();
                        let pubkey = bin_container.address;
                        pubkeys_to_remove.push(pubkey);
                    }
                }
            }
        }
        (pubkeys_to_remove, pubkeys_to_add)
    }
    
    pub fn set_right_bins(&self, 
        bin_pubkeys: Vec<(Pubkey, i32)>,
    ) -> (Vec<String>, Vec<String>) {
        let mut writable_data = self.writable_data.write().unwrap();
        let bin_containers = &mut writable_data.bin_containers;
        
        // Find the index of the last bin container that matches any bin in bin_pubkeys
        let last_bin_container_pubkey = &bin_containers.bin_containers.last().map(|bc| bc.address.clone());
        let mut pubkeys_to_remove = Vec::new();
        let mut pubkeys_to_add = Vec::new();
        if let Some(last_pubkey) = last_bin_container_pubkey {
            if let Some(corresponding_index) = bin_pubkeys.iter().position(
                |(pubkey, _)| *pubkey.to_string() == *last_pubkey
            ) {
                // Get the slice of bin_pubkeys to the right of the matching bin (not including the match)
                let right_bins = &bin_pubkeys[corresponding_index+1..];
                
                
                // Append the bins to the bin_containers vector
                for (pubkey, bin_id) in right_bins {
                    bin_containers.bin_containers.push(BinContainer::new_empty(
                        pubkey.to_string(), 
                        *bin_id
                    )
                    );
                    pubkeys_to_add.push(pubkey.to_string());
                }
                
                
                // Check if we have more than 3 bin containers to the left of the active one
                let left_bins_count = bin_containers.active_bin_container_index;
                if left_bins_count > 3 {
                    // Remove excess bin containers from the left
                    let to_remove = left_bins_count - 3;
                    for _ in 0..to_remove {
                        let bin_container = bin_containers.bin_containers.remove(0);
                        let pubkey = bin_container.address;
                        pubkeys_to_remove.push(pubkey);
                    }
                    // Adjust the active bin container index after removing from the left
                    bin_containers.active_bin_container_index -= to_remove;
                }
            }
        }
        (pubkeys_to_remove, pubkeys_to_add)
    }


}


fn get_bin_array_lower_upper_bin_id(index: i32) -> Result<(i32, i32), SafeMathError> {
    let lower_bin_id = index.safe_mul(MAX_BIN_PER_ARRAY as i32)?;
    let upper_bin_id = lower_bin_id
        .safe_add(MAX_BIN_PER_ARRAY as i32)?
        .safe_sub(1)?;

    Ok((lower_bin_id, upper_bin_id))
}

