use serde::{Deserialize, Serialize};
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction, instruction::AccountMeta, instruction::Instruction,
    pubkey::Pubkey, signer::Signer, transaction::Transaction, hash::Hash as BlockHash
};
use std::fmt;

use crate::tokens::pools_manager::ConnectionManager;
use crate::pools::{
    pools_utils::*,
    PoolConstants
};
use std::sync::atomic::{AtomicBool, Ordering};
use spl_associated_token_account::instruction::create_associated_token_account;
use base64::prelude::*;
use borsh::{BorshDeserialize, BorshSerialize};
use log::debug;
use meteora::swap::meteora_pool_info;
use rand::Rng;
use solana_sdk::commitment_config::CommitmentLevel;
use std::hash::{Hash, Hasher};
use std::sync::RwLock;
use std::sync::Arc;
const WSOL_MINT: Pubkey = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
const TOKEN_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const SYSTEM_PROGRAM: Pubkey = Pubkey::from_str_const("11111111111111111111111111111111");
const ARBITRAGE_PROGRAM: Pubkey =
    Pubkey::from_str_const("5GWu2jYc3SDCnBGqbz6ZHdF8WYbt8WuYpD6aNZynrC7A");

const JITO_TIP_ACCOUNTS: [Pubkey; 8] = [
    Pubkey::from_str_const("96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5"),
    Pubkey::from_str_const("HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe"),
    Pubkey::from_str_const("Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY"),
    Pubkey::from_str_const("ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49"),
    Pubkey::from_str_const("DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh"),
    Pubkey::from_str_const("ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt"),
    Pubkey::from_str_const("DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL"),
    Pubkey::from_str_const("3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT"),
];



pub fn get_random_jito_tip_account() -> Pubkey {
    let random_index = rand::thread_rng().gen_range(0..JITO_TIP_ACCOUNTS.len());
    JITO_TIP_ACCOUNTS[random_index]
}

fn add_sub_fee(fee: u32, add: bool) -> u32 {
    if add {
        (fee + PoolConstants::MOVE_FEE_VALUE).min(PoolConstants::MAX_FEE)
    } else {
        (fee - PoolConstants::MOVE_FEE_VALUE).max(100_000)
    }
}

fn jito_add_sub_fee(fee: u32, add: bool) -> u32 {
    if add {
        (fee + PoolConstants::JITO_MOVE_FEE_VALUE).min(PoolConstants::MAX_JITO_FEE)
    } else {
        (fee - PoolConstants::JITO_MOVE_FEE_VALUE).max(10)
    }
}