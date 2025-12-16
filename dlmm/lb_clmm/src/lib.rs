#![allow(warnings)]

use solana_program::pubkey::Pubkey;
pub mod constants;
pub mod errors;
pub mod math;
pub mod state;
pub mod utils;

pub const ID: Pubkey = solana_program::pubkey!("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo");
