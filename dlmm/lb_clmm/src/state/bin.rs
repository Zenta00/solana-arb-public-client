use super::lb_pair::LbPair;
use crate::{
    constants::{BASIS_POINT_MAX, MAX_BIN_ID, MAX_BIN_PER_ARRAY, MIN_BIN_ID, NUM_REWARDS},
    errors::*,
    math::{
        price_math::get_price_from_id,
        safe_math::SafeMath,
        u128x128_math::Rounding,
        u64x64_math::SCALE_OFFSET,
        utils_math::{safe_mul_div_cast, safe_mul_shr_cast, safe_shl_div_cast},
    },
};
use num_enum::{IntoPrimitive, TryFromPrimitive};
use num_integer::Integer;
use solana_program::pubkey::Pubkey;
use std::cell::RefMut;

#[derive(Default, Debug)]
pub struct Bin {
    /// Amount of token X in the bin. This already excluded protocol fees.
    pub amount_x: u64,
    /// Amount of token Y in the bin. This already excluded protocol fees.
    pub amount_y: u64,
    /// Bin price
    pub price: u128,
    /// Liquidities of the bin. This is the same as LP mint supply. q-number
    pub liquidity_supply: u128,
    /// reward_a_per_token_stored
    pub reward_per_token_stored: [u128; NUM_REWARDS],
    /// Swap fee amount of token X per liquidity deposited.
    pub fee_amount_x_per_token_stored: u128,
    /// Swap fee amount of token Y per liquidity deposited.
    pub fee_amount_y_per_token_stored: u128,
    /// Total token X swap into the bin. Only used for tracking purpose.
    pub amount_x_in: u128,
    /// Total token Y swap into he bin. Only used for tracking purpose.
    pub amount_y_in: u128,
}

#[derive(Debug, PartialEq, Eq, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
/// Layout version
pub enum LayoutVersion {
    V0,
    V1,
}

#[derive(Debug)]
/// An account to contain a range of bin. For example: Bin 100 <-> 200.
/// For example:
/// BinArray index: 0 contains bin 0 <-> 599
/// index: 2 contains bin 600 <-> 1199, ...
pub struct BinArray {
    pub index: i64, // Larger size to make bytemuck "safe" (correct alignment)
    /// Version of binArray
    pub version: u8,
    pub _padding: [u8; 7],
    pub lb_pair: Pubkey,
    pub bins: [Bin; MAX_BIN_PER_ARRAY],
}

impl BinArray {
    /// Get bin from bin array

    /// Check whether the bin id is within the bin array range
    /*
    pub fn is_bin_id_within_range(&self, bin_id: i32) -> Result<(), LBError> {
        let (lower_bin_id, upper_bin_id) =
            BinArray::get_bin_array_lower_upper_bin_id(self.index as i32)?;

        require!(
            bin_id >= lower_bin_id && bin_id <= upper_bin_id,
            LBError::InvalidBinId
        );

        Ok(())
    }
    */

    /// Get bin array index from bin id
    /// n
    pub fn bin_id_to_bin_array_index(bin_id: i32) -> Result<i32, LBError> {
        let (idx, rem) = bin_id.div_rem(&(MAX_BIN_PER_ARRAY as i32));

        if bin_id.is_negative() && rem != 0 {
            Ok(idx.safe_sub(1)?)
        } else {
            Ok(idx)
        }
    }
    /*
    /// Get lower and upper bin id of the given bin array index
    pub fn get_bin_array_lower_upper_bin_id(index: i32) -> Result<(i32, i32)> {
        let lower_bin_id = index.safe_mul(MAX_BIN_PER_ARRAY as i32)?;
        let upper_bin_id = lower_bin_id
            .safe_add(MAX_BIN_PER_ARRAY as i32)?
            .safe_sub(1)?;

        Ok((lower_bin_id, upper_bin_id))
    }

    /// Check that the index within MAX and MIN bin id

    pub fn check_valid_index(index: i32) -> Result<()> {
        let (lower_bin_id, upper_bin_id) = BinArray::get_bin_array_lower_upper_bin_id(index)?;

        require!(
            lower_bin_id >= MIN_BIN_ID && upper_bin_id <= MAX_BIN_ID,
            LBError::InvalidStartBinIndex
        );

        Ok(())
    }

    /// Update the bin reward(s) per liquidity share stored for the active bin.
    pub fn update_all_rewards(
        &mut self,
        lb_pair: &mut RefMut<'_, LbPair>,
        current_time: u64,
    ) -> Result<()> {
        for reward_idx in 0..NUM_REWARDS {
            let bin = self.get_bin_mut(lb_pair.active_id)?;
            let reward_info = &mut lb_pair.reward_infos[reward_idx];

            if reward_info.initialized() {
                if bin.liquidity_supply > 0 {
                    let reward_per_token_stored_delta = reward_info
                        .calculate_reward_per_token_stored_since_last_update(
                            current_time,
                            bin.liquidity_supply
                                .safe_shr(SCALE_OFFSET.into())?
                                .try_into()
                                .map_err(|_| LBError::TypeCastFailed)?,
                        )?;

                    bin.reward_per_token_stored[reward_idx] = bin.reward_per_token_stored
                        [reward_idx]
                        .safe_add(reward_per_token_stored_delta)?;
                } else {
                    // Time period which the reward was distributed to empty bin
                    let time_period =
                        reward_info.get_seconds_elapsed_since_last_update(current_time)?;

                    // Save the time window of empty bin reward, and reward it in the next time window
                    reward_info.cumulative_seconds_with_empty_liquidity_reward = reward_info
                        .cumulative_seconds_with_empty_liquidity_reward
                        .safe_add(time_period)?;
                }

                reward_info.update_last_update_time(current_time);
            }
        }
        Ok(())
    }
    */
}
