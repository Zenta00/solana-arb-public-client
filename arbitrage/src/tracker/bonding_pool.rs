use {
    std::any::Any,
    std::sync::{Arc, RwLock},
    solana_sdk::{
        pubkey::Pubkey,
        compute_budget::ComputeBudgetInstruction,
        instruction::{Instruction, AccountMeta},
    },
    crate::{
        tracker::*,
        wallets::wallet::{get_fee_instruction_with_profit, get_pre_transfer_tip_instruction_with_signer},
        

        pools::{
            price_maths::V4Maths,
            compute_kp_swap_amount,
            WSOL_MINT,
            RoutingStep,
            TokenLabel,
        },
        simulation::router::SwapDirection,
    },
    borsh::de::BorshDeserialize
};

const TOKEN_PROGRAM_ID: Pubkey = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey = Pubkey::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
const PUMP_BONDING_FEE_BPS: u128 = 10_000_000;
const RAYDIUM_BONDING_FEE_BPS: u128 = 2_500_000;
const PUMPFUN_BONDING_BUDGET: u32 = 100_000;

const SYSTEM_PROGRAM_ID: Pubkey = Pubkey::from_str_const("11111111111111111111111111111111");

const PUMPFUN_AMM_ID: Pubkey = Pubkey::from_str_const("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
const GLOBAL_CONFIG_ID: Pubkey = Pubkey::from_str_const("4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf");
const PROTOCOL_FEE: Pubkey = Pubkey::from_str_const("AVmoTthdrX6tKt4nDjco2D775W2YK3sDhxPcMmzUAmTY");
const TOKEN_PROTOCOL_FEE: Pubkey = Pubkey::from_str_const("FGptqdxjahafaCzpZ1T6EDtCzYMv7Dyn5MgBLyB3VUFW");
const EVENT_AUTHORITY: Pubkey = Pubkey::from_str_const("Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1");

pub trait BondingPool: Any + Send + Sync {
    fn as_pump_bonding_pool(&self) -> Option<&PumpBondingPool> {
        None
    }
    fn as_raydium_bonding_pool(&self) -> Option<&RaydiumBondingPool> {
        None
    }

    fn get_address(&self) -> String {
        "".to_string()
    }

    fn get_token_mint(&self) -> String;
    fn update_with_data(&self, data: &[u8]) -> Option<(u128, u128, bool, u128, u128)>;

    fn get_amount_out(&self, amount_in: u128, direction: SwapDirection) -> u128;

    fn get_price(&self) -> u128 {
        0
    }

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
    ) -> (Vec<Instruction>, u32);
}

impl BondingPool for PumpBondingPool {
    fn as_pump_bonding_pool(&self) -> Option<&PumpBondingPool> {
        Some(self)
    }

    fn get_address(&self) -> String {
        self.address.clone()
    }

    fn get_token_mint(&self) -> String {
        self.token_mint.clone()
    }

    fn update_with_data(&self, data: &[u8]) -> Option<(u128, u128, bool, u128, u128)> {
        self.writable_data.write().unwrap().update_with_data(data)
    }

    fn get_amount_out(&self, amount_in: u128, direction: SwapDirection) -> u128 {
        self.writable_data.read().unwrap().get_amount_out(amount_in, direction)
    }

    fn get_price(&self) -> u128 {
        self.writable_data.read().unwrap().price
    }

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
        

        let accounts = self.get_account_keys(
            user_token_account,
            signer_pubkey,
            is_buy,
        );

        let mut instruction_data = Vec::with_capacity(24);

        let mut instructions = if is_buy {
            instruction_data.extend_from_slice(&[0x66, 0x06, 0x3d, 0x12, 0x01, 0xda, 0xeb, 0xea]);
            let k = reserve_base_amount * reserve_quote_amount;
            let amount_in_with_fee = amount_in as u128 * 10_000 / 10_100;
            let amount_out =
                (reserve_base_amount - (k / (reserve_quote_amount + amount_in_with_fee)) - 1) as u64;
            instruction_data.extend_from_slice(&amount_out.to_le_bytes());
            let max_amount_in = (amount_in + 2) * (100 + slippage) / 100;
            instruction_data.extend_from_slice(&max_amount_in.to_le_bytes());

            let mut instructions = wrap_sol_instruction(
                max_amount_in,
                signer_pubkey,
                user_wsol_account,
            );
            instructions.push(create_token_account(
                signer_pubkey,
                Pubkey::from_str_const(&self.token_mint),
            ));
            instructions.push(
                Instruction {
                    program_id: PUMPFUN_AMM_ID,
                    accounts: accounts,
                    data: instruction_data,
                }
            );
            instructions
        } else {
            instruction_data.extend_from_slice(&[0x33, 0xe6, 0x85, 0xa4, 0x01, 0x7f, 0x83, 0xad]);
            instruction_data.extend_from_slice(&amount_in.to_le_bytes());
            instruction_data.extend_from_slice(&0_u64.to_le_bytes());

            let mut instructions = vec![
                Instruction {
                    program_id: PUMPFUN_AMM_ID,
                    accounts: accounts,
                    data: instruction_data,
                },
                close_token_account(
                    signer_pubkey,
                    user_wsol_account,
                ),
                create_wsol_account(
                    signer_pubkey,
                ),
            ];

            if full_out {
                instructions.push(close_token_account(
                    signer_pubkey,
                    user_token_account,
                ));
            }

            instructions
        };

        
        //instructions.push(get_pre_transfer_tip_instruction_with_signer(&signer_pubkey, tip_amount));
        instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(PUMPFUN_BONDING_BUDGET));
        instructions.push(get_fee_instruction_with_profit(tip_amount, PUMPFUN_BONDING_BUDGET));
        (instructions, PUMPFUN_BONDING_BUDGET)
    }


    
}


pub struct PumpWritableData {
    pub virtual_token_reserves: u128,
    pub virtual_sol_reserves: u128,
    pub real_token_reserves: u128,
    pub real_sol_reserves: u128,
    pub k: u128,
    pub graduated: bool,
    pub price: u128,
}

impl PumpWritableData {
    pub fn new_empty() -> Self {
        Self {
            virtual_token_reserves: 0,
            virtual_sol_reserves: 0,
            real_token_reserves: 0,
            real_sol_reserves: 0,
            k: 0,
            graduated: false,
            price: 0,
        }
    }

    pub fn update_with_data(&mut self, data: &[u8]) -> Option<(u128, u128, bool, u128, u128)> {
        let prev_virtual_token_reserves = self.virtual_token_reserves;
        let prev_virtual_sol_reserves = self.virtual_sol_reserves;

        self.virtual_token_reserves = u64::from_le_bytes(data[8..16].try_into().unwrap()) as u128;
        self.virtual_sol_reserves =  u64::from_le_bytes(data[16..24].try_into().unwrap()) as u128;
        self.real_token_reserves = u64::from_le_bytes(data[24..32].try_into().unwrap()) as u128;
        self.real_sol_reserves = u64::from_le_bytes(data[32..40].try_into().unwrap()) as u128;
        self.graduated = data[48] == 1;
        self.price = (self.virtual_sol_reserves << 64).checked_div(self.virtual_token_reserves).unwrap_or(0);
        self.k = self.virtual_sol_reserves * self.virtual_token_reserves;

        if self.price == 0 {
            return None;
        }
        let kp_swap =compute_kp_swap_amount(
            prev_virtual_token_reserves, 
            prev_virtual_sol_reserves, 
            self.virtual_token_reserves, 
            self.virtual_sol_reserves, 
            RoutingStep::Extremity, 
            &TokenLabel::Y
        );
        kp_swap.map(|(sol_diff, token_diff, is_buy)| (sol_diff, token_diff, is_buy, self.virtual_sol_reserves, self.virtual_token_reserves))
    }

    fn get_amount_out(&self, amount_in: u128, direction: SwapDirection) -> u128 {
        V4Maths::get_amount_out_with_fee(
            amount_in, 
            self.virtual_token_reserves, 
            self.virtual_sol_reserves, 
            self.k, 
            direction == SwapDirection::XtoY, 
            10_000_000_u128,
            true,
        )
    }

}
 
pub struct PumpBondingPool {
    pub address: String,
    pub writable_data: Arc<RwLock<PumpWritableData>>,
    pub associated_bonding_address: Pubkey,
    pub creator_vault: Pubkey,
    pub token_mint: String,
    pub fee_bps: u128,
}

impl PumpBondingPool {
    pub fn new(address: String, data: &[u8], token_mint: String) -> Self {
        let writable_data = Arc::new(RwLock::new(PumpWritableData::new_empty()));
        writable_data.write().unwrap().update_with_data(data);

        let creator_vault_key = Pubkey::try_from_slice(&data[49..81]).unwrap();
        let (creator_vault, _bump) = Pubkey::find_program_address(
            &[
                b"creator-vault",
                creator_vault_key.as_ref(), // creator's pubkey as bytes
            ],
            &PUMPFUN_AMM_ID, // the program ID that owns this PDA
        );
        let (associated_bonding_key, _) = Pubkey::find_program_address(
            &[&Pubkey::from_str_const(&address).to_bytes(),
            &TOKEN_PROGRAM_ID.to_bytes(),
            &Pubkey::from_str_const(&token_mint).to_bytes()],
            &ASSOCIATED_TOKEN_PROGRAM_ID);
        
        Self {
            address,
            writable_data,
            associated_bonding_address: associated_bonding_key,
            creator_vault,
            token_mint,
            fee_bps: PUMP_BONDING_FEE_BPS,
        }
    }

    fn get_account_keys(&self,
    user_token_account: Pubkey,
    signer_pubkey: Pubkey,
    is_buy: bool,
    ) -> Vec<AccountMeta> {

        if is_buy {
            vec![
                AccountMeta::new_readonly(GLOBAL_CONFIG_ID, false),
                AccountMeta::new(PROTOCOL_FEE, false),
                AccountMeta::new_readonly(Pubkey::from_str_const(&self.token_mint), false),
                AccountMeta::new(Pubkey::from_str_const(&self.address), false),
                AccountMeta::new(self.associated_bonding_address, false),
                AccountMeta::new(user_token_account, false),
                AccountMeta::new(signer_pubkey, true),
                AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
                AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
                AccountMeta::new(self.creator_vault, false),
                AccountMeta::new_readonly(EVENT_AUTHORITY, false),
                AccountMeta::new_readonly(PUMPFUN_AMM_ID, false),
            ]
        } else {
            vec![
                AccountMeta::new_readonly(GLOBAL_CONFIG_ID, false),
                AccountMeta::new(PROTOCOL_FEE, false),
                AccountMeta::new_readonly(Pubkey::from_str_const(&self.token_mint), false),
                AccountMeta::new(Pubkey::from_str_const(&self.address), false),
                AccountMeta::new(self.associated_bonding_address, false),
                AccountMeta::new(user_token_account, false),
                AccountMeta::new(signer_pubkey, true),
                AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
                AccountMeta::new(self.creator_vault, false),
                AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
                AccountMeta::new_readonly(EVENT_AUTHORITY, false),
                AccountMeta::new_readonly(PUMPFUN_AMM_ID, false),
            ]
        }
    }

    
    

    pub fn get_associated_bonding_address(&self) -> Pubkey {
        self.associated_bonding_address
    }
}

pub const RLL_AUTHORITY: Pubkey = Pubkey::from_str_const("WLHv2UAZm6z4KyaaELi5pjdbJh6RESMva1Rnn8pJVVh");
pub const RLL_GLOBAL_CONFIG_ID: Pubkey = Pubkey::from_str_const("6s1xP3hpbAfFoNtUNF8mfHsjr2Bd97JxFJRWLbL6aHuX");
pub const RLL_PLATFORM_CONFIG_ID: Pubkey = Pubkey::from_str_const("12XE4efSCudQadBA76Zfb5aVuD7B5EHQ7AkBLJD2ySmq");
pub const RLL_EVENT_AUTHORITY: Pubkey = Pubkey::from_str_const("2DPAtwB8L12vrMRExbLuyGnC7n2J5LNoZQSejeQGpwkr");
pub const RLL_PROGRAM_ID: Pubkey = Pubkey::from_str_const("LanMV9sAd7wArD4vJFi2qDdfnVhFxYSUg6eADduJ3uj");

pub struct RaydiumBondingPool {
    pub address: String,
    pub writable_data: Arc<RwLock<RaydiumWritableData>>,
    pub global_config: Pubkey,
    pub platform_config: Pubkey,
    pub token_mint: String,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub fee_bps: u128,
}

impl RaydiumBondingPool {
    pub fn new(address: String, data: &[u8]) -> Self {
        
        let global_config: Pubkey = Pubkey::try_from_slice(&data[141..173]).unwrap();
        let platform_config: Pubkey = Pubkey::try_from_slice(&data[173..205]).unwrap();
        let token_mint: Pubkey = Pubkey::try_from_slice(&data[205..237]).unwrap();
        let base_vault: Pubkey = Pubkey::try_from_slice(&data[269..301]).unwrap();
        let quote_vault: Pubkey = Pubkey::try_from_slice(&data[301..333]).unwrap();
        
        let mut writable_data = RaydiumWritableData::new_empty();
        writable_data.update_with_data(data);


        Self {
            address,
            writable_data: Arc::new(RwLock::new(writable_data)),
            global_config,
            platform_config,
            token_mint: token_mint.to_string(),
            base_vault,
            quote_vault,
            fee_bps: RAYDIUM_BONDING_FEE_BPS,
        }
    }

    pub fn update_with_data(&self, data: &[u8]) -> Option<(u128, u128, bool, u128, u128)> {
        self.writable_data.write().unwrap().update_with_data(data)
    }

    pub fn get_account_keys(&self,
        user_token_account: Pubkey,
        user_wsol_account: Pubkey,
        signer_pubkey: Pubkey,
    ) -> Vec<AccountMeta> {
        
        vec![
            AccountMeta::new(signer_pubkey, true),
            AccountMeta::new_readonly(RLL_AUTHORITY, false),
            AccountMeta::new_readonly(self.global_config, false),
            AccountMeta::new_readonly(self.platform_config, false),
            AccountMeta::new(Pubkey::from_str_const(&self.address), false),
            AccountMeta::new(user_token_account, false),
            AccountMeta::new(user_wsol_account, false),
            AccountMeta::new(self.base_vault, false),
            AccountMeta::new(self.quote_vault, false),
            AccountMeta::new_readonly(Pubkey::from_str_const(&self.token_mint), false),
            AccountMeta::new_readonly(Pubkey::from_str_const(&WSOL_MINT), false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(RLL_EVENT_AUTHORITY, false),
            AccountMeta::new_readonly(RLL_PROGRAM_ID, false),
        ]
    }
}

pub struct RaydiumWritableData {
    pub virtual_token_reserves: u128,
    pub virtual_sol_reserves: u128,
    pub real_token_reserves: u128,
    pub real_sol_reserves: u128,
    pub k: u128,
    pub price: u128,
}

impl RaydiumWritableData {
    pub fn new_empty() -> Self {
        Self {
            virtual_token_reserves: 0,
            virtual_sol_reserves: 0,
            real_token_reserves: 0,
            real_sol_reserves: 0,
            k: 0,
            price: 0,
        }
    }

    pub fn update_with_data(&mut self, data: &[u8]) -> Option<(u128, u128, bool, u128, u128)> {
        let prev_real_token_reserves = self.real_token_reserves;
        let prev_real_sol_reserves = self.real_sol_reserves;

        self.virtual_token_reserves = u64::from_le_bytes(data[37..45].try_into().unwrap()) as u128;
        self.virtual_sol_reserves =  u64::from_le_bytes(data[45..53].try_into().unwrap()) as u128;
        self.real_token_reserves = u64::from_le_bytes(data[53..61].try_into().unwrap()) as u128;
        self.real_sol_reserves = u64::from_le_bytes(data[61..69].try_into().unwrap()) as u128;
        self.price = (self.real_sol_reserves << 64).checked_div(self.real_token_reserves).unwrap_or(0);
        self.k = self.real_sol_reserves * self.real_token_reserves;
        if self.price == 0 {
            return None;
        }
        RaydiumWritableData::compute_swap_amount(
            prev_real_token_reserves, 
            prev_real_sol_reserves, 
            self.real_token_reserves,
            self.real_sol_reserves,
        ).map(|(sol_diff, token_diff, is_buy)| (sol_diff, token_diff, is_buy, self.real_sol_reserves, self.real_token_reserves))
    }

    fn compute_swap_amount(
        prev_real_token_reserves: u128,
        prev_real_sol_reserves: u128,
        real_token_reserves: u128,
        real_sol_reserves: u128,
    ) -> Option<(u128, u128, bool)> {
        if prev_real_token_reserves < real_token_reserves &&
            prev_real_sol_reserves < real_sol_reserves {
                Some((real_sol_reserves - prev_real_sol_reserves, real_token_reserves - prev_real_token_reserves, true))
            }
        else if prev_real_token_reserves > real_token_reserves &&
            prev_real_sol_reserves > real_sol_reserves {
                Some((prev_real_sol_reserves - real_sol_reserves, prev_real_token_reserves - real_token_reserves, false))
            }
        else {
            None
        }
    }

    fn get_amount_out(&self, amount_in: u128, direction: SwapDirection) -> u128 {
        V4Maths::get_amount_out_with_fee(
            amount_in, 
            self.real_token_reserves, 
            self.real_sol_reserves, 
            self.k, 
            direction == SwapDirection::XtoY, 
            2_500_000_u128,
            true,
        )
    }
}

impl BondingPool for RaydiumBondingPool {
    fn as_raydium_bonding_pool(&self) -> Option<&RaydiumBondingPool> {
        Some(self)
    }

    fn get_address(&self) -> String {
        self.address.clone()
    }

    fn get_token_mint(&self) -> String {
        self.token_mint.clone()
    }

    fn update_with_data(&self, data: &[u8]) -> Option<(u128, u128, bool, u128, u128)> {
        self.writable_data.write().unwrap().update_with_data(data)
    }

    fn get_price(&self) -> u128 {
        self.writable_data.read().unwrap().price
    }  

    fn get_amount_out(&self, amount_in: u128, direction: SwapDirection) -> u128 {
        self.writable_data.read().unwrap().get_amount_out(amount_in, direction)
    }

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
        

        let accounts = self.get_account_keys(
            user_token_account,
            user_wsol_account,
            signer_pubkey,
        );

        let mut instruction_data = Vec::with_capacity(32);

        let mut instructions = if is_buy {
            instruction_data.extend_from_slice(&[0xfa, 0xea, 0x0d ,0x7b ,0xd5 ,0x9c ,0x13 ,0xec]);
            let k = reserve_base_amount * reserve_quote_amount;
            let amount_in_with_fee = amount_in as u128 * 10_000 / 10_100;
            let amount_out =
                (reserve_base_amount - (k / (reserve_quote_amount + amount_in_with_fee)) - 1) as u64;
            instruction_data.extend_from_slice(&amount_in.to_le_bytes());
            let min_amount_out = (amount_out + 2) * (100 - slippage) / 100;
            instruction_data.extend_from_slice(&min_amount_out.to_le_bytes());
            instruction_data.extend_from_slice(&0_u64.to_le_bytes());

            let mut instructions = wrap_sol_instruction(
                amount_in,
                signer_pubkey,
                user_wsol_account,
            );
            instructions.push(create_token_account(
                signer_pubkey,
                Pubkey::from_str_const(&self.token_mint),
            ));
            instructions.push(
                Instruction {
                    program_id: RLL_PROGRAM_ID,
                    accounts: accounts,
                    data: instruction_data,
                }
            );
            instructions
        } else {
            instruction_data.extend_from_slice(&[0x95, 0x27, 0xde, 0x9b, 0xd3, 0x7c, 0x98, 0x1a]);
            instruction_data.extend_from_slice(&amount_in.to_le_bytes());
            instruction_data.extend_from_slice(&0_u64.to_le_bytes());
            instruction_data.extend_from_slice(&0_u64.to_le_bytes());

            let mut instructions = vec![
                Instruction {
                    program_id: RLL_PROGRAM_ID,
                    accounts: accounts,
                    data: instruction_data,
                },
                close_token_account(
                    signer_pubkey,
                    user_wsol_account,
                ),
                create_wsol_account(
                    signer_pubkey,
                ),
            ];

            if full_out {
                instructions.push(close_token_account(
                    signer_pubkey,
                    user_token_account,
                ));
            }

            instructions
        };

        
        //instructions.push(get_pre_transfer_tip_instruction_with_signer(&signer_pubkey, tip_amount));
        instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(PUMPFUN_BONDING_BUDGET));
        instructions.push(get_fee_instruction_with_profit(tip_amount, PUMPFUN_BONDING_BUDGET));
        (instructions, PUMPFUN_BONDING_BUDGET)
    }

}