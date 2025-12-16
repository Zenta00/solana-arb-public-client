
pub mod bonding_pool;
pub mod tracker_positions;
pub mod copytrade;
pub mod chart;
pub mod strategy;
pub mod order;

use {
    solana_sdk::{
        system_instruction::{
            transfer,
        },
        pubkey::Pubkey,
        instruction::Instruction,
    },
    spl_associated_token_account::{
        instruction::{
            create_associated_token_account_idempotent,
        },
        get_associated_token_address,
    },
    spl_token::instruction::{
        close_account,
    },
    spl_token_2022::instruction::{
        sync_native,
    }
};

const TOKEN_PROGRAM_ID: Pubkey = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const WSOL_MINT: Pubkey = Pubkey::from_str_const("So11111111111111111111111111111111111111112");

pub fn create_token_account(
    user_pubkey: Pubkey,
    token_mint: Pubkey,

) -> Instruction {
    let create_associated_token_account_instruction = create_associated_token_account_idempotent(
        &user_pubkey,
        &user_pubkey,
        &token_mint,
        &TOKEN_PROGRAM_ID
    );
    create_associated_token_account_instruction
}

pub fn create_wsol_account(
    user_pubkey: Pubkey,
) -> Instruction {
    let create_associated_token_account_instruction = create_associated_token_account_idempotent(
        &user_pubkey,
        &user_pubkey,
        &WSOL_MINT,
        &TOKEN_PROGRAM_ID
    );
    create_associated_token_account_instruction
}

pub fn wrap_sol_instruction(
    amount_in: u64,
    user_pubkey: Pubkey,
    user_wsol_account: Pubkey,
) -> Vec<Instruction> {
    /*let create_associated_token_account_instruction = create_associated_token_account_idempotent(
        &user_pubkey,
        &user_pubkey,
        &WSOL_MINT,
        &TOKEN_PROGRAM_ID
    );*/

    let transfer_instruction = transfer(
        &user_pubkey,
        &user_wsol_account,
        amount_in,
    );
    let sync_native_instruction = sync_native(
        &TOKEN_PROGRAM_ID,
        &user_wsol_account,
    ).unwrap();
    vec![transfer_instruction, sync_native_instruction]
}

pub fn close_token_account(
    user_pubkey: Pubkey,
    user_token_account: Pubkey,
) -> Instruction {
    let close_account_instruction = close_account(
        &TOKEN_PROGRAM_ID,
        &user_token_account,
        &user_pubkey,
        &user_pubkey,
        &[&user_pubkey]
    ).unwrap();
    close_account_instruction
}

pub fn find_token_account(
    user_pubkey: Pubkey,
    token_mint: Pubkey,
) -> Pubkey {
    get_associated_token_address(&user_pubkey, &token_mint)
}