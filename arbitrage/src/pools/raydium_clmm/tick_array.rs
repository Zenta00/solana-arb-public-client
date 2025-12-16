use {
    crate::pools::raydium_clmm::{
        error::ErrorCode,
        tick_math,
    },
    solana_account::Account,
    solana_sdk::pubkey::Pubkey,
};

pub const TICK_ARRAY_SEED: &str = "tick_array";
pub const TICK_ARRAY_SIZE_USIZE: usize = 60;
pub const TICK_ARRAY_SIZE: i32 = 60;
// pub const MIN_TICK_ARRAY_START_INDEX: i32 = -443636;
// pub const MAX_TICK_ARRAY_START_INDEX: i32 = 306600;
pub struct TickArrayState {
    pub pool_id: String,
    pub address: String,
    pub start_tick_index: i32,
    pub ticks: [TickState; TICK_ARRAY_SIZE_USIZE],
    pub initialized_tick_count: u8,
    pub write_number: u64,
}

impl TickArrayState {

    pub fn new_vec(
        pool_key: String,
        tick_array_infos_vec: Vec<(Pubkey, Option<Account>, i32)>,
    ) -> Vec<Self> {
        tick_array_infos_vec.iter().map(|
            (tick_array_key, tick_array_account_info, start_index)| {
                let tick_array_key_str = tick_array_key.to_string();
                let mut tick_array_state = Self::new(*start_index, pool_key.clone(), tick_array_key_str);
                if let Some(tick_array_account_info) = tick_array_account_info {
                    tick_array_state.initialize_from_data(&tick_array_account_info.data[8..]);
                }
                tick_array_state
            }
        ).collect()
    }

    pub fn new(
        start_index: i32,
        pool_key: String,
        tick_array_key: String,
    ) -> Self {
        Self {
            start_tick_index: start_index,
            pool_id: pool_key,
            address: tick_array_key,
            ticks: [TickState::default(); TICK_ARRAY_SIZE_USIZE],
            initialized_tick_count: 0,
            write_number: 0,
        }
    }

    pub fn get_array_start_index(tick_index: i32, tick_spacing: u16) -> i32 {
        let ticks_in_array = TickArrayState::tick_count(tick_spacing);
        let mut start = tick_index / ticks_in_array;
        if tick_index < 0 && tick_index % ticks_in_array != 0 {
            start = start - 1
        }
        start * ticks_in_array
    }

    pub fn tick_count(tick_spacing: u16) -> i32 {
        TICK_ARRAY_SIZE * i32::from(tick_spacing)
    }

    pub fn check_is_valid_start_index(tick_index: i32, tick_spacing: u16) -> bool {
        if TickState::check_is_out_of_boundary(tick_index) {
            if tick_index > tick_math::MAX_TICK {
                return false;
            }
            let min_start_index =
                TickArrayState::get_array_start_index(tick_math::MIN_TICK, tick_spacing);
            return tick_index == min_start_index;
        }
        tick_index % TickArrayState::tick_count(tick_spacing) == 0
    }

    /// Base on swap directioin, return the first initialized tick in the tick array.
    pub fn first_initialized_tick(&self, zero_for_one: bool) -> Result<&TickState, ErrorCode> {
        if zero_for_one {
            let mut i = TICK_ARRAY_SIZE - 1;
            while i >= 0 {
                if self.ticks[i as usize].is_initialized() {
                    return Ok(self.ticks.get(i as usize).unwrap());
                }
                i = i - 1;
            }
        } else {
            let mut i = 0;
            while i < TICK_ARRAY_SIZE_USIZE {
                if self.ticks[i].is_initialized() {
                    return Ok(self.ticks.get(i).unwrap());
                }
                i = i + 1;
            }
        }
        Err(ErrorCode::InvalidTickArray)
    }

    pub fn next_initialized_tick(
        &self,
        current_tick_index: i32,
        tick_spacing: u16,
        zero_for_one: bool,
    ) -> Result<Option<&TickState>, ErrorCode> {
        let current_tick_array_start_index =
            TickArrayState::get_array_start_index(current_tick_index, tick_spacing);
        if current_tick_array_start_index != self.start_tick_index {
            return Ok(None);
        }
        let mut offset_in_array =
            (current_tick_index - self.start_tick_index) / i32::from(tick_spacing);

        if zero_for_one {
            while offset_in_array >= 0 {
                if self.ticks[offset_in_array as usize].is_initialized() {
                    return Ok(self.ticks.get(offset_in_array as usize));
                }
                offset_in_array = offset_in_array - 1;
            }
        } else {
            offset_in_array = offset_in_array + 1;
            while offset_in_array < TICK_ARRAY_SIZE {
                if self.ticks[offset_in_array as usize].is_initialized() {
                    return Ok(self.ticks.get(offset_in_array as usize));
                }
                offset_in_array = offset_in_array + 1;
            }
        }
        Ok(None)
    }

    pub fn initialize_from_data(&mut self, data: &[u8]) {
        let mut offset = 36;
        for i in 0..TICK_ARRAY_SIZE_USIZE {
            let tick = &mut self.ticks[i];
            tick.tick = i32::from_le_bytes(data[offset..offset+4].try_into().unwrap());
            offset += 4;
            tick.liquidity_net = i128::from_le_bytes(data[offset..offset+16].try_into().unwrap());
            offset += 16;
            tick.liquidity_gross = u128::from_le_bytes(data[offset..offset+16].try_into().unwrap());
            offset += 148;
        }
        self.initialized_tick_count = data[offset];
    }

    pub fn parse_from_data(&mut self, data: &[u8], write_number: u64) {
        if write_number < self.write_number {
            return;
        }
        let mut offset = 36;
        for i in 0..TICK_ARRAY_SIZE_USIZE {
            let tick = &mut self.ticks[i];
            offset += 4;
            tick.liquidity_net = i128::from_le_bytes(data[offset..offset+16].try_into().unwrap());
            offset += 16;
            tick.liquidity_gross = u128::from_le_bytes(data[offset..offset+16].try_into().unwrap());
            offset += 148;
        }
        self.initialized_tick_count = data[offset];
        self.write_number = write_number;
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TickState {
    pub tick: i32,
    /// Amount of net liquidity added (subtracted) when tick is crossed from left to right (right to left)
    pub liquidity_net: i128,
    /// The total position liquidity that references this tick
    pub liquidity_gross: u128,
}

impl Default for TickState {
    fn default() -> Self {
        Self { tick: 0, liquidity_net: 0, liquidity_gross: 0 }
    }
}

impl TickState {

    pub fn check_is_out_of_boundary(tick: i32) -> bool {
        tick < tick_math::MIN_TICK || tick > tick_math::MAX_TICK
    }

    pub fn is_initialized(self) -> bool {
        self.liquidity_gross != 0
    }

    /// Transitions to the current tick as needed by price movement, returning the amount of liquidity
    /// added (subtracted) when tick is crossed from left to right (right to left)
    pub fn cross(
        &self,
    ) -> i128 {
        self.liquidity_net
    }
}

