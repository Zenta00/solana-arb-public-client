use crate::simulation::simulator_safe_maths::*;
use crate::simulation::router::SwapDirection;
pub struct ClmmMaths;

pub const F64_FEE_CONSTANT: f64 = 1_000_000_000.0;
pub const MAX_SHIFT_INTEGER: u32 = 64;
pub const SCALE_FEE_CONSTANT: u128 = 1_000_000_000;
pub const QUARTER_MAX_SHIFT_INTEGER: u32 = 16;
pub const HALF_MAX_SHIFT_INTEGER: u32 = 32;
pub const MAX_U_48_INTEGER: u32 = 48;


impl ClmmMaths {

    pub fn sqrt_price_x64_to_price_x64(sqrt_price_x64: u128) -> u128 {
        let to_square = sqrt_price_x64 >> 32;
        to_square * to_square
    }

    pub fn price_x64_to_sqrt_price_x64(price_x64: u128) -> u128 {
        let shifted_price = (price_x64 >> HALF_MAX_SHIFT_INTEGER) as f64;
        (shifted_price.sqrt() as u128) << MAX_U_48_INTEGER
    }

    pub fn abs_price_difference(price_a: u128, price_b: u128) -> u128 {
        if price_a > price_b {
        price_a - price_b
    } else {
        price_b - price_a
        }
    }

    pub fn constant_product_delta_token(price_64_difference: u128, liquidity: u128, x_is_sol: bool) -> u128 {
        if x_is_sol {
            ClmmMaths::constant_product_delta_y(price_64_difference, liquidity)
        } else {
            ClmmMaths::constant_product_delta_x(price_64_difference, liquidity)
        }
    }

    pub fn constant_product_delta_sol(price_64_difference: u128, liquidity: u128, x_is_sol: bool) -> u64 {
        if x_is_sol {
            ClmmMaths::constant_product_delta_x(price_64_difference, liquidity) as u64
        } else {
            ClmmMaths::constant_product_delta_y(price_64_difference, liquidity) as u64
        }
    }

    pub fn constant_product_delta_x(price_64_difference: u128, liquidity: u128) -> u128 {
        price_64_difference * liquidity / (1 << 64)
    }

    pub fn constant_product_delta_y(price_64_difference: u128, liquidity: u128) -> u128 {
        liquidity / price_64_difference * (1 << 64) 
    }
}

pub struct V4Maths;

impl V4Maths {
    
    pub fn new_price_after_swap(
        reserve_x: u128, 
        reserve_y: u128, 
        k: u128, 
        amount_in: u128,
        x_for_y: bool,
        should_inverse: bool
    ) -> Result<(u64, u128), SafeMathError> {
        if x_for_y {

            let amount_out = (reserve_y - (k / (reserve_x + amount_in))) as u64;
            let to_square = reserve_x + amount_in;
            let price_after_swap = if should_inverse {
                (to_square << 64) / k * to_square 
            } else {
                ((k / to_square) << 64) / to_square
            };
            Ok((amount_out, price_after_swap))
        } else {
            let amount_out = (reserve_x - (k / (reserve_y + amount_in))) as u64;
            let to_square = reserve_y + amount_in;
            let price_after_swap = if should_inverse {
                ((k / to_square) << 64) / to_square
            } else {
                (to_square << 64) / k * to_square 
            };
            Ok((amount_out, price_after_swap))
        }
    }

    pub fn get_amount_in_with_fee(
        amount_out: u128,
        reserve_source: u128,
        reserve_destination: u128,
        k: u128,
        fee: u128
    ) -> Result<u128, SafeMathError> {
        let amount_in_with_fees = k / ( reserve_destination.safe_sub(amount_out)?) - reserve_source;
        Ok(amount_in_with_fees * SCALE_FEE_CONSTANT / (SCALE_FEE_CONSTANT - fee))
    }

    pub fn get_amount_in_and_price_with_fee(
        amount_out: u128,
        reserve_x: u128,
        reserve_y: u128,
        k: u128,
        x_for_y: bool,
        should_inverse: bool,
        fee: u128
    ) -> Result<(u128, u128), SafeMathError> {
        let (amount_in, new_reserve_x, new_reserve_y) = if x_for_y {
            let amount_in = Self::get_amount_in_with_fee(amount_out, reserve_x, reserve_y, k, fee)?;
            let new_reserve_x = reserve_x + amount_in;
            let new_reserve_y = reserve_y - amount_out;
            (amount_in, new_reserve_x,new_reserve_y)
        } else {
            let amount_in = Self::get_amount_in_with_fee(amount_out, reserve_y, reserve_x, k, fee)?;
            let new_reserve_x = reserve_x - amount_out;
            let new_reserve_y = reserve_y + amount_in;
            (amount_in, new_reserve_x, new_reserve_y)
        };

        let price_after_swap = if should_inverse {
            (new_reserve_x << MAX_SHIFT_INTEGER) / new_reserve_y
        } else {
            (new_reserve_y << MAX_SHIFT_INTEGER) / new_reserve_x
        };
        Ok((amount_in, price_after_swap))
    }

    pub fn get_amount_out_and_price_with_fee(
        amount_in: u128,
        reserve_x: u128,
        reserve_y: u128,
        k: u128,
        x_for_y: bool,
        should_inverse: bool,
        fee: u128
    ) -> (u128, u128) {
        let (amount_out, new_reserve_x, new_reserve_y) = if x_for_y {

            let amount_out = Self::get_amount_out_with_fee(amount_in, reserve_x, reserve_y, k, true, fee, false);
            let new_reserve_x = reserve_x + amount_in;
            let new_reserve_y = reserve_y - amount_out;
            (amount_out, new_reserve_x,new_reserve_y)
        } else {
            let amount_out = Self::get_amount_out_with_fee(amount_in, reserve_x, reserve_y, k, false, fee, false);
            let new_reserve_x = reserve_x - amount_out;
            let new_reserve_y = reserve_y + amount_in;
            (amount_out, new_reserve_x, new_reserve_y)
        };

        let price_after_swap = if should_inverse {
            (new_reserve_x << MAX_SHIFT_INTEGER) / new_reserve_y
        } else {
            (new_reserve_y << MAX_SHIFT_INTEGER) / new_reserve_x
        };
        (amount_out, price_after_swap)
    }
        

    pub fn get_amount_out_with_fee(
        amount_in: u128,
        reserve_x: u128,
        reserve_y: u128,
        k: u128,
        x_for_y: bool,
        fee: u128,
        inverse_fee: bool,
    ) -> u128 {
        let amount_in_with_fee = if inverse_fee {
            amount_in * SCALE_FEE_CONSTANT / (SCALE_FEE_CONSTANT + fee)
        } else {
            amount_in * (SCALE_FEE_CONSTANT - fee) / SCALE_FEE_CONSTANT as u128
        };
        if x_for_y {
            return reserve_y - (k / (reserve_x + amount_in_with_fee));
        } else {
            return reserve_x - (k / (reserve_y + amount_in_with_fee));
        }
    }

    pub fn get_amount_in_for_target_price(
        reserve_x: u128,
        reserve_y: u128,
        k: u128,
        target_price: u128,
        direction: SwapDirection,
        f: u128
    ) -> Result<u128, SafeMathError> {
        if direction == SwapDirection::YtoX {
            let b =  reserve_y + reserve_y.apply_fee(f, false);
            let c = reserve_y - reserve_y.apply_fee(f, false);
            let scaled_price_fee = target_price.apply_fee(f, false) * 4;
            let d = (reserve_x >> QUARTER_MAX_SHIFT_INTEGER)
                * (scaled_price_fee >> HALF_MAX_SHIFT_INTEGER)
                * (reserve_y >> QUARTER_MAX_SHIFT_INTEGER);

            let numerator = (((c*c + d) as f64).sqrt() as u128).safe_sub(b)?;
            return Ok(numerator * SCALE_FEE_CONSTANT / (2 *(SCALE_FEE_CONSTANT - f)));
        } else {
            let b =  reserve_x + reserve_x.apply_fee(f, false);
            let c = reserve_x - reserve_x.apply_fee(f, false);
            let scaled_k_fee = k.apply_fee(f, false) * 4;
            let d = a_lshift_64_div_b(scaled_k_fee, target_price);
            let numerator = (((c*c + d) as f64).sqrt() as u128).safe_sub(b)?;
            return Ok(numerator * SCALE_FEE_CONSTANT / (2 *(SCALE_FEE_CONSTANT - f)));  
        }
    }

    pub fn compute_amount_in_v4_pairs(
        reserve_x_a: u128,
        reserve_y_a: u128,
        reserve_x_b: u128,
        reserve_y_b: u128,
        k_a: u128,
        k_b: u128,
        sqrt_k_a: f64,
        sqrt_k_b: f64,
        fee: f64
    ) -> Result<(u64, u64), SafeMathError> {
        let sqrt_k_a_k_b = sqrt_k_a * sqrt_k_b;
        let fee_denominator = 
            (F64_FEE_CONSTANT - fee / F64_FEE_CONSTANT).sqrt();
        let r_x_a_plus_r_x_b = reserve_x_a + reserve_x_b;
        let amount_in = 
            (( sqrt_k_a_k_b / fee_denominator) as u128 - reserve_y_a * reserve_x_b) / r_x_a_plus_r_x_b;

        let amount_out_denominator = r_x_a_plus_r_x_b - (k_a / (reserve_y_a + amount_in));
        
        let amount_out = reserve_y_b - (k_b / amount_out_denominator);
        Ok((amount_in as u64, amount_out as u64))
    }

    pub fn get_amount_in(reserve_x: u128, reserve_y: u128, k: u128, amount_out: u128, x_for_y: bool) -> u64 {
        let amount_in = if x_for_y {
            if amount_out >= reserve_y {
                let result = match reserve_x.checked_mul(100) {
                    Some(result) => result as u64,
                    None => return 10*reserve_x as u64,
                };
                return result;
            }
            let amount_in = k / ( reserve_y - amount_out) - reserve_x;
            amount_in
        } else {
            if amount_out >= reserve_x {
                let result = match reserve_y.checked_mul(100) {
                    Some(result) => result as u64,
                    None => return 10*reserve_y as u64,
                };
                return result;
            }
            let amount_in = k / ( reserve_x - amount_out) - reserve_y;
            amount_in
        };
        amount_in as u64
    }
    

    pub fn get_amount_in_with_price(reserve_x: u128, reserve_y: u128, k: u128, amount_out: u128, x_for_y: bool, should_inverse: bool) -> (u64, u128) {
        let amount_in = if x_for_y {
            let amount_in = k / ( reserve_y - amount_out) - reserve_x;
            amount_in
        } else {
            let amount_in = k / ( reserve_x - amount_out) - reserve_y;
            amount_in
        };

        let new_price = if should_inverse {
            ((reserve_x + amount_in) << MAX_SHIFT_INTEGER) / (reserve_y - amount_out)
        } else {
            ((reserve_y - amount_out) << MAX_SHIFT_INTEGER) / (reserve_x + amount_in)
        };
        (amount_in as u64, new_price)
    }

}

pub fn a_lshift_64_div_b(k: u128, price: u128) -> u128 {
    // Find how many bits we can shift k left safely
    let k_leading_zeros = k.leading_zeros();
    // Calculate how much we can shift k initially (maximum shift possible)
    let initial_shift = k_leading_zeros.min(MAX_SHIFT_INTEGER);
    
    // Shift k left by initial_shift
    let shifted_k = k << initial_shift;
    
    let div_result = shifted_k / price;

    if initial_shift < MAX_SHIFT_INTEGER {
        return div_result << (MAX_SHIFT_INTEGER - initial_shift);
    }
    
    div_result
}

pub fn a_times_b_rshift_64(a: u128, b: u128) -> u128 {
    let a_shifted = a >> 32;
    let b_shifted = b >> 32;
    a_shifted * b_shifted
}

pub fn combine_fee_bps(fee_a: u128, fee_b: u128) -> u128 {
    SCALE_FEE_CONSTANT - (SCALE_FEE_CONSTANT - fee_a) * (SCALE_FEE_CONSTANT - fee_b) / (SCALE_FEE_CONSTANT)
}

pub trait Fee {
    fn apply_fee(self, fee: u128, add_fee: bool) -> u128;

    fn apply_fee_big_numerator(self, fee: u128) -> u128;
}

impl Fee for u128 {
    fn apply_fee(self, fee: u128, add_fee: bool) -> u128 {
        let factor = if add_fee {
            SCALE_FEE_CONSTANT + fee
        } else {
            SCALE_FEE_CONSTANT - fee
        };
        if self > (1 << 96) {
            return self / SCALE_FEE_CONSTANT * factor;
        }
        self * factor / SCALE_FEE_CONSTANT
    }

    fn apply_fee_big_numerator(self, fee: u128) -> u128 {
        self / SCALE_FEE_CONSTANT * (SCALE_FEE_CONSTANT - fee)
    }
}

pub fn price_differ(value_a: u128, value_b: u128, fee_a: u128, fee_b: u128) -> (bool, bool) {
    // Find the absolute difference between the two values
    let a_is_cheaper;
    let (buy_price, sell_price, buy_fee, sell_fee) = if value_a < value_b {
        a_is_cheaper = true;
        (value_a, value_b, fee_a, fee_b)
    } else {    
        a_is_cheaper = false;
        (value_b, value_a, fee_b, fee_a)
    };

    let is_diff = buy_price * (SCALE_FEE_CONSTANT + buy_fee) < sell_price * (SCALE_FEE_CONSTANT - sell_fee);

    
    (is_diff, a_is_cheaper)
}

pub fn find_new_target_price(
    current_price: u128,
    current_fee: u128,
    target_fee: u128,
    target_is_higher: bool
) -> u128 {
    if target_is_higher {
        current_price * (SCALE_FEE_CONSTANT + current_fee) / (SCALE_FEE_CONSTANT - target_fee)
    } else {
        current_price * (SCALE_FEE_CONSTANT - current_fee) / (SCALE_FEE_CONSTANT + target_fee)
    }
}
    
        

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_f64_operations() {
        let base = 1_000_000_000 as u128;
        let integer_128 = (base << 64) as u128;
        let float = integer_128 as f64;
        println!("integer_128: {}", integer_128);
        println!("float u128: {}", float);
        let reserve_x = 181_023_987_564_257 as u128;
        let reserve_y = 15_154_163_927_456 as u128;
        let k = reserve_x * reserve_y;
        let price = reserve_y << 64 / reserve_x;
        let test = (price as f64 * (k as f64)) as u128;
        let result = test >> 64;
        println!("reserve_x: {}, reserve_y: {}, k: {}, price: {}, test: {}, result: {}", reserve_x, reserve_y, k, price, test, result);
    }
}
