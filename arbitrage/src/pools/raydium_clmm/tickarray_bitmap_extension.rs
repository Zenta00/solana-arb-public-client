
use {
    crate::pools::raydium_clmm::{
        error::ErrorCode,
        big_num::U512,
        tick_array::TickArrayState,
        tick_array_bit_map::{TickArrayBitmap, get_bitmap_tick_boundary},
        tick_math,
    },
};
const EXTENSION_TICKARRAY_BITMAP_SIZE: usize = 14;
pub const TICK_ARRAY_BITMAP_SIZE: i32 = 512;
pub const TICK_ARRAY_SIZE: i32 = 60;

pub struct TickArrayBitmapExtension {
    pub pool_id: String,
    pub address: String,
    /// Packed initialized tick array state for start_tick_index is positive
    pub positive_tick_array_bitmap: [[u64; 8]; EXTENSION_TICKARRAY_BITMAP_SIZE],
    /// Packed initialized tick array state for start_tick_index is negitive
    pub negative_tick_array_bitmap: [[u64; 8]; EXTENSION_TICKARRAY_BITMAP_SIZE],
    pub write_number: u64,
}


impl TickArrayBitmapExtension {

    pub fn initialize_from_data(pool_id: String, address: String, data: &[u8]) -> Self {

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

        Self {
            pool_id,
            address,
            positive_tick_array_bitmap,
            negative_tick_array_bitmap,
            write_number: 0,
        }
        
    }
    pub fn new(pool_id: String, bitmap_extension_address: String) -> Self {
        Self {
            pool_id,
            address: bitmap_extension_address,
            positive_tick_array_bitmap: [[0; 8]; EXTENSION_TICKARRAY_BITMAP_SIZE],
            negative_tick_array_bitmap: [[0; 8]; EXTENSION_TICKARRAY_BITMAP_SIZE],
            write_number: 0,
        }
    }

    /// Check if the tick array is initialized
    pub fn check_tick_array_is_initialized(
        &self,
        tick_array_start_index: i32,
        tick_spacing: u16,
    ) -> Result<(bool, i32), ErrorCode> {
        let (_, tickarray_bitmap) = self.get_bitmap(tick_array_start_index, tick_spacing)?;

        let tick_array_offset_in_bitmap =
            Self::tick_array_offset_in_bitmap(tick_array_start_index, tick_spacing);

        if U512(tickarray_bitmap).bit(tick_array_offset_in_bitmap as usize) {
            return Ok((true, tick_array_start_index));
        }
        Ok((false, tick_array_start_index))
    }

    fn get_bitmap(&self, tick_index: i32, tick_spacing: u16) -> Result<(usize, TickArrayBitmap), ErrorCode> {
        let offset = Self::get_bitmap_offset(tick_index, tick_spacing)?;
        if tick_index < 0 {
            Ok((offset, self.negative_tick_array_bitmap[offset]))
        } else {
            Ok((offset, self.positive_tick_array_bitmap[offset]))
        }
    }

    fn get_bitmap_offset(tick_index: i32, tick_spacing: u16) -> Result<usize, ErrorCode> {
        Self::check_extension_boundary(tick_index, tick_spacing)?;
        let ticks_in_one_bitmap = max_tick_in_tickarray_bitmap(tick_spacing);
        let mut offset = tick_index.abs() / ticks_in_one_bitmap - 1;
        if tick_index < 0 && tick_index.abs() % ticks_in_one_bitmap == 0 {
            offset -= 1;
        }
        Ok(offset as usize)
    }

    pub fn check_extension_boundary(tick_index: i32, tick_spacing: u16) -> Result<(), ErrorCode> {
        let positive_tick_boundary = max_tick_in_tickarray_bitmap(tick_spacing);
        let negative_tick_boundary = -positive_tick_boundary;
        if tick_index >= negative_tick_boundary && tick_index < positive_tick_boundary {
            return Err(ErrorCode::InvalidTickArrayBoundary);
        }
        Ok(())
    }

    pub fn next_initialized_tick_array_from_one_bitmap(
        &self,
        last_tick_array_start_index: i32,
        tick_spacing: u16,
        zero_for_one: bool,
    ) -> Result<(bool, i32), ErrorCode> {
        let multiplier = TickArrayState::tick_count(tick_spacing);
        let next_tick_array_start_index = if zero_for_one {
            last_tick_array_start_index - multiplier
        } else {
            last_tick_array_start_index + multiplier
        };
        let min_tick_array_start_index =
            TickArrayState::get_array_start_index(tick_math::MIN_TICK, tick_spacing);
        let max_tick_array_start_index =
            TickArrayState::get_array_start_index(tick_math::MAX_TICK, tick_spacing);

        if next_tick_array_start_index < min_tick_array_start_index
            || next_tick_array_start_index > max_tick_array_start_index
        {
            return Ok((false, next_tick_array_start_index));
        }

        let (_, tickarray_bitmap) = self.get_bitmap(next_tick_array_start_index, tick_spacing)?;

        Ok(Self::next_initialized_tick_array_in_bitmap(
            tickarray_bitmap,
            next_tick_array_start_index,
            tick_spacing,
            zero_for_one,
        ))
    }

    pub fn next_initialized_tick_array_in_bitmap(
        tickarray_bitmap: TickArrayBitmap,
        next_tick_array_start_index: i32,
        tick_spacing: u16,
        zero_for_one: bool,
    ) -> (bool, i32) {
        let (bitmap_min_tick_boundary, bitmap_max_tick_boundary) =
            get_bitmap_tick_boundary(next_tick_array_start_index, tick_spacing);

        let tick_array_offset_in_bitmap =
            Self::tick_array_offset_in_bitmap(next_tick_array_start_index, tick_spacing);
        if zero_for_one {
            // tick from upper to lower
            // find from highter bits to lower bits
            let offset_bit_map = U512(tickarray_bitmap)
                << (TICK_ARRAY_BITMAP_SIZE - 1 - tick_array_offset_in_bitmap);

            let next_bit = if offset_bit_map.is_zero() {
                None
            } else {
                Some(u16::try_from(offset_bit_map.leading_zeros()).unwrap())
            };

            if next_bit.is_some() {
                let next_array_start_index = next_tick_array_start_index
                    - i32::from(next_bit.unwrap()) * TickArrayState::tick_count(tick_spacing);
                return (true, next_array_start_index);
            } else {
                // not found til to the end
                return (false, bitmap_min_tick_boundary);
            }
        } else {
            // tick from lower to upper
            // find from lower bits to highter bits
            let offset_bit_map = U512(tickarray_bitmap) >> tick_array_offset_in_bitmap;

            let next_bit = if offset_bit_map.is_zero() {
                None
            } else {
                Some(u16::try_from(offset_bit_map.trailing_zeros()).unwrap())
            };
            if next_bit.is_some() {
                let next_array_start_index = next_tick_array_start_index
                    + i32::from(next_bit.unwrap()) * TickArrayState::tick_count(tick_spacing);
                return (true, next_array_start_index);
            } else {
                // not found til to the end
                return (
                    false,
                    bitmap_max_tick_boundary - TickArrayState::tick_count(tick_spacing),
                );
            }
        }
    }

    pub fn tick_array_offset_in_bitmap(tick_array_start_index: i32, tick_spacing: u16) -> i32 {
        let m = tick_array_start_index.abs() % max_tick_in_tickarray_bitmap(tick_spacing);
        let mut tick_array_offset_in_bitmap = m / TickArrayState::tick_count(tick_spacing);
        if tick_array_start_index < 0 && m != 0 {
            tick_array_offset_in_bitmap = TICK_ARRAY_BITMAP_SIZE - tick_array_offset_in_bitmap;
        }
        tick_array_offset_in_bitmap
    }
}

pub fn max_tick_in_tickarray_bitmap(tick_spacing: u16) -> i32 {
    i32::from(tick_spacing) * TICK_ARRAY_SIZE * TICK_ARRAY_BITMAP_SIZE
}