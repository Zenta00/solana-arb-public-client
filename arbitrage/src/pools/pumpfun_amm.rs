use {
    crate::{
        wallets::wallet::get_pre_transfer_tip_instruction_with_signer,
        pools::{compute_kp_swap_amount, price_maths::{
            V4Maths,
            SCALE_FEE_CONSTANT,
        }, CongestionStats, LiquidityType, Pool, PoolError, PoolUpdateType, RoutingStep, TokenLabel, WSOL_MINT}, simulation::{pool_impact::AmmType, router::SwapDirection}, tracker::*, wallets::wallet::get_fee_instruction_with_profit
    }, borsh::{BorshDeserialize, BorshSerialize}, crossbeam_channel::Sender, log::debug, solana_client::nonblocking::rpc_client::RpcClient, solana_sdk::{
        clock::Slot, compute_budget::ComputeBudgetInstruction, instruction::{AccountMeta, Instruction}, pubkey::Pubkey, signature::{
            Signature,
            Signer,
        }, system_instruction, transaction::Transaction
    }, std::{cmp::{Eq, PartialEq}, error::Error, hash::{Hash, Hasher}, sync::{
        atomic::{AtomicU64, Ordering}, Arc, RwLock
    }}
};


const PUMPSWAP_DEFAULT_FEE_BPS: u128 = 2_500_000;
const PUMPFUN_BUDGET: u32 = 60_000;
const PUMPSWAP_BUDGET: u32 = 150_000;

struct WritableData {
    pub price: u128,
    pub inversed_price: u128,
    pub prev_reserve_x_amount: u128,
    pub prev_reserve_y_amount: u128,
    pub reserve_x_amount: u128,
    pub reserve_y_amount: u128,
    pub k: u128,
}
impl WritableData {
    fn set_price(&mut self) -> bool {
        let prev_price = self.price;
        self.price = (self.reserve_y_amount << 64) / self.reserve_x_amount;
        self.inversed_price = (self.reserve_x_amount << 64) / self.reserve_y_amount;
        prev_price != self.price
    }
}

pub struct PumpfunAmmPool {
    pub address: String,
    pub mint_x: String,
    pub mint_y: String,
    pub reserve_x: String,
    pub reserve_y: String,
    pub fee_bps: u128,
    pub pool_info: PumpfunAmmPoolInfo,
    pub writable_data: Arc<RwLock<WritableData>>,
    pub last_sent_slot: Arc<AtomicU64>,
    pub farm_token_label: TokenLabel,
    pub last_amount_in: Arc<AtomicU64>,
    pub congestion_stats: RwLock<CongestionStats>,
}

impl Hash for PumpfunAmmPool {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.address.hash(state);
    }
}

impl PartialEq for PumpfunAmmPool {
    fn eq(&self, other: &Self) -> bool {
        self.address == other.address
    }
}

impl Eq for PumpfunAmmPool {}

impl Pool for PumpfunAmmPool {

    fn as_pumpfun_amm(&self) -> Option<&PumpfunAmmPool> {
        Some(self)
    }

    fn as_pumpfun_amm_mut(&mut self) -> Option<&mut PumpfunAmmPool> {
        Some(self)
    }

    fn get_address(&self) -> String {
        self.address.to_string()
    }

    fn get_owner(&self) -> String {
        self.pool_info.owner.clone()
    }

    fn get_pool_type(&self) -> String {
        "Pumpfun-Kp".to_string()
    }

    fn get_liquidity_type(&self) -> LiquidityType {
        LiquidityType::ConstantProduct
    }

    fn get_fee_bps(&self) -> u128 {
        self.fee_bps
    }

    fn set_price(&self, _update_pool_sender: &Sender<(String, PoolUpdateType)>) -> Result<bool, PoolError> {
        let mut writable_data = self.writable_data.write().unwrap();
        let is_price_diff = writable_data.set_price();
        Ok(is_price_diff)
    }

    

    fn update_with_balances(&self, x_amount: u128, y_amount: u128, _optional_data: &[u8]) -> Result<(bool, Option<(u128,u128,bool, u128, u128)>), PoolError> {
        
        let mut writable_data = self.writable_data.write().unwrap();

        writable_data.prev_reserve_x_amount = writable_data.reserve_x_amount;
        writable_data.prev_reserve_y_amount = writable_data.reserve_y_amount;
        writable_data.reserve_x_amount = x_amount;
        writable_data.reserve_y_amount = y_amount;
        
        
        

        
        
        writable_data.k = x_amount * y_amount;
        if x_amount == 0 || y_amount == 0 {
            writable_data.price = 0;
            writable_data.inversed_price = 0;
            return Err(PoolError::NotEnoughLiquidity);
        }

        let swap_amount = if self.get_farm_token_mint() == Some(WSOL_MINT.to_string()) {
            let kp_swap = compute_kp_swap_amount(
                writable_data.prev_reserve_x_amount, 
                writable_data.prev_reserve_y_amount, 
                writable_data.reserve_x_amount, 
                writable_data.reserve_y_amount, 
                self.get_routing_step(), 
                &self.farm_token_label,
            );
            
            kp_swap.map(|(amount1, amount2, direction)| 
                (amount1, amount2, direction, writable_data.reserve_y_amount, writable_data.reserve_x_amount)
            )
        } else {
            None
        };

        let is_price_diff = writable_data.set_price();
        Ok((is_price_diff, swap_amount))
    }


    fn get_last_sent_slot(&self) -> u64 {
        self.last_sent_slot.load(Ordering::Relaxed)
    }

    fn store_last_sent_slot(&self, slot: u64) {
        self.last_sent_slot.store(slot, Ordering::Relaxed);
    }

    fn get_last_amount_in(&self) -> u64 {
        self.last_amount_in.load(Ordering::Relaxed)
    }

    fn store_last_amount_in(&self, amount: u64) {
        self.last_amount_in.store(amount, Ordering::Relaxed);
    }

    fn get_reserve_x_account(&self) -> String {
        self.reserve_x.clone()
    }

    fn get_reserve_y_account(&self) -> String {
        self.reserve_y.clone()
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

    fn get_inverted_price_and_fee(&self) -> (Option<u128>, u128) {
        let writable_data = self.writable_data.read().unwrap();
        (Some(writable_data.inversed_price), self.fee_bps)
    }

    fn get_amount_out_with_cu(&self, amount_in: f64, direction: SwapDirection) -> Result<(f64, f64), PoolError> {
        let writable_data = self.writable_data.read().unwrap();
        let amount_out = V4Maths::get_amount_out_with_fee(
            amount_in as u128, 
            writable_data.reserve_x_amount, 
            writable_data.reserve_y_amount, 
            writable_data.k, 
            direction == SwapDirection::XtoY, 
            self.fee_bps,
            true,
        );
        Ok((amount_out as f64, PUMPFUN_BUDGET as f64))
    }

    fn get_amount_out_with_cu_extra_infos(&self, amount_in: f64, direction: SwapDirection) -> Result<(f64, f64, Option<Vec<String>>), PoolError> {
        let (amount_out, budget) = self.get_amount_out_with_cu(amount_in, direction)?;
        Ok((amount_out, budget, None))
    }

    fn get_amount_in_for_target_price(&self, target_price: u128, direction: SwapDirection) -> Result<u128, PoolError> {
        let writable_data = self.writable_data.read().unwrap();
        let amount_in = V4Maths::get_amount_in_for_target_price(
            writable_data.reserve_x_amount, 
            writable_data.reserve_y_amount, 
            writable_data.k, 
            target_price, 
            direction, 
            self.fee_bps
        ).unwrap_or(0);

        Ok(amount_in)
    }

    fn get_amount_in(&self, amount_out: u128, direction: SwapDirection) -> Result<u128, PoolError> {
        let writable_data = self.writable_data.read().unwrap();

        let amount_in = V4Maths::get_amount_in(
            writable_data.reserve_x_amount, 
            writable_data.reserve_y_amount, 
            writable_data.k, 
            amount_out, 
            direction == SwapDirection::XtoY
        );

        let amount_in_with_fee = (amount_in as u128) * SCALE_FEE_CONSTANT / (SCALE_FEE_CONSTANT - self.fee_bps);
        Ok(amount_in_with_fee)
    }


    fn get_associated_tracked_pubkeys(&self) -> Vec<String> {
        vec![self.reserve_x.clone(), self.reserve_y.clone()]
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

    fn get_mint_x(&self) -> String {
        self.mint_x.clone()
    }

    fn get_mint_y(&self) -> String {
        self.mint_y.clone()
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
        
        let accounts = self.pool_info.keys.get_with_user_and_tokens_pubkeys(
            user_token_account,
            user_wsol_account,
            signer_pubkey,
        );
        let mut instruction_data = Vec::with_capacity(24);

        let mut instructions = if is_buy {
            instruction_data.extend_from_slice(&[0x66, 0x06, 0x3d, 0x12, 0x01, 0xda, 0xeb, 0xea]);
            let k = reserve_base_amount * reserve_quote_amount;
            let amount_in_with_fee = amount_in as u128 * 10_000 / 10_025;
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
                Pubkey::from_str_const(&self.mint_x),
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

        
        instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(PUMPSWAP_BUDGET));
        instructions.push(get_fee_instruction_with_profit(tip_amount, PUMPSWAP_BUDGET));
        //instructions.push(get_pre_transfer_tip_instruction_with_signer(&signer_pubkey, tip_amount));
        (instructions, PUMPSWAP_BUDGET)
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
        _extra_accounts: &Option<Vec<String>>
    ) {

        let (base_user_token, quote_user_token, swap_data) = if *mint_in == self.mint_x {
            (user_token_in, user_token_out, 1)
        } else {
            (user_token_out, user_token_in, 0)
        };

        let accounts = self.pool_info.keys.get_with_user_and_tokens(
            base_user_token,
            quote_user_token,
            wallet_pubkey
        );

        accounts_meta.extend(accounts);
        
        data.push(self.pool_info.amm_flag);
        data.push(swap_data);
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

impl PumpfunAmmPool {
    pub fn new(
        address: Pubkey, 
        pool_info: PumpfunAmmPoolInfo,
        farm_token_label: TokenLabel,
    ) -> Option<Self> {
        let (
            mint_x,
            mint_y,
            reserve_x,
            reserve_y,
        ) = pool_info.keys.get_x_y_infos();
        Some(
            Self {
                address: address.to_string(),
                mint_x,
                mint_y,
                reserve_x,
                reserve_y,
                    pool_info,
                    fee_bps: PUMPSWAP_DEFAULT_FEE_BPS,
                    last_sent_slot: Arc::new(AtomicU64::new(0)),
                    writable_data: Arc::new(RwLock::new(WritableData {
                        price: 0,
                        inversed_price: 0,
                        reserve_x_amount: 0,
                        reserve_y_amount: 0,
                        prev_reserve_x_amount: 0,
                        prev_reserve_y_amount: 0,
                        k: 0,
                    })),
                    farm_token_label,
                    last_amount_in: Arc::new(AtomicU64::new(0)),
                    congestion_stats: RwLock::new(CongestionStats::new_at_slot(0)),
                }
        )
    }

    pub fn get_spam_arb_accounts(&self) -> Vec<AccountMeta>{
        let keys = &self.pool_info.keys.vec;
        vec![
            keys[1].clone(),
            keys[8].clone(),
            keys[9].clone(),
        ]
    }

}

#[derive(Debug, Clone)]
pub struct SwapAccountMetas {
    pub vec: Vec<AccountMeta>,
}

impl SwapAccountMetas {

    fn new_from_pool_state(pool_id: &Pubkey, pool_state: &PumpFunMarketState) -> Self {

        let (coin_creator_vault_authority, _bump) = Pubkey::find_program_address(
            &[
                b"creator_vault",
                pool_state.coin_creator.as_ref(),
            ],
            &PUMPFUN_AMM_ID,
        );
        let (coin_creator_vault_ata, _bump) = Pubkey::find_program_address(
            &[
                pool_state.quote_mint.as_ref(),
                coin_creator_vault_authority.as_ref(),
                TOKEN_PROGRAM_ACCOUNT.as_ref(),
            ],
            &PUMPFUN_AMM_ID,
        );
        SwapAccountMetas {
            vec: vec![
                AccountMeta::new_readonly(PUMPFUN_AMM_ID, false),
                AccountMeta::new_readonly(*pool_id, false),
                AccountMeta::default(),
                AccountMeta::new_readonly(GLOBAL_CONFIG, false),
                AccountMeta::new_readonly(pool_state.base_mint, false),
                AccountMeta::new_readonly(pool_state.quote_mint, false),
                AccountMeta::default(),
                AccountMeta::default(),
                AccountMeta::new(pool_state.pool_base_reserve, false),
                AccountMeta::new(pool_state.pool_quote_reserve, false),
                AccountMeta::new(PROTOCOL_FEE_RECIPIENT, false),
                AccountMeta::new(PROTOCOL_FEE_RECIPIENT_TOKEN_ACCOUNT, false),
                AccountMeta::new_readonly(TOKEN_PROGRAM_ACCOUNT, false),
                AccountMeta::new_readonly(SYSTEM_PROGRAM_ACCOUNT, false),
                AccountMeta::new_readonly(ASSOCIATED_TOKEN_PROGRAM_ACCOUNT, false),
                AccountMeta::new_readonly(EVENT_AUTHORITY, false),
                AccountMeta::new(coin_creator_vault_ata, false),
                AccountMeta::new_readonly(coin_creator_vault_authority, false),
            ]
        }
    }
    pub fn get_with_user_and_tokens(
        &self, 
        in_mint: String, 
        out_mint: String, 
        user_pubkey: Pubkey
    ) -> Vec<AccountMeta> {
        let mut vec = self.vec.clone();
        vec[6] = AccountMeta::new(Pubkey::from_str_const(&in_mint), false);
        vec[7] = AccountMeta::new(Pubkey::from_str_const(&out_mint), false);
        vec[2] = AccountMeta::new(user_pubkey, true);
        
        vec
    }

    pub fn get_with_user_and_tokens_pubkeys(
        &self, 
        in_mint: Pubkey, 
        out_mint: Pubkey, 
        user_pubkey: Pubkey
    ) -> Vec<AccountMeta> {
        let mut vec = self.vec.clone();
        vec[6] = AccountMeta::new(in_mint, false);
        vec[7] = AccountMeta::new(out_mint, false);
        vec[2] = AccountMeta::new(user_pubkey, true);
        
        vec.insert(13, AccountMeta::new_readonly(TOKEN_PROGRAM_ACCOUNT, false));
        let pumpfun_amm = vec.pop().unwrap();
        vec.insert(16, pumpfun_amm);
        vec
    }

    fn get_x_y_infos(&self) -> (String, String, String, String) {
        (
            self.vec[4].pubkey.to_string(),
            self.vec[5].pubkey.to_string(),
            self.vec[8].pubkey.to_string(),
            self.vec[9].pubkey.to_string(),
        )
    }
}

pub struct PumpFunMarketState {
    base_mint: Pubkey,
    quote_mint: Pubkey,
    pool_base_reserve: Pubkey,
    pool_quote_reserve: Pubkey,
    lp_supply: u64,
    coin_creator: Pubkey,
}

impl PumpFunMarketState {
    pub fn try_deserialize(data: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            base_mint: Pubkey::try_from_slice(&data[35..67])?,
            quote_mint: Pubkey::try_from_slice(&data[67..99])?,
            pool_base_reserve: Pubkey::try_from_slice(&data[131..163])?,
            pool_quote_reserve: Pubkey::try_from_slice(&data[163..195])?,
            lp_supply: u64::try_from_slice(&data[195..203])?,
            coin_creator: Pubkey::try_from_slice(&data[203..235])?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct PumpfunAmmPoolInfo {
    pub keys: SwapAccountMetas,
    pub amm_flag: u8,
    pub owner: String,
}

impl PumpfunAmmPoolInfo {
    pub fn new(pool_id: &Pubkey, data: &[u8]) -> Option<Self> {
        let pool_state = PumpFunMarketState::try_deserialize(&data[8..]).unwrap();
        if pool_state.lp_supply <= 1_000_000 {
            return None;
        }
        let keys = SwapAccountMetas::new_from_pool_state(pool_id, &pool_state);
        Some(Self { keys, amm_flag: 3, owner: pool_id.to_string() })
    }
}

// Constants for market versions
pub const PUMPFUN_AMM_ID: Pubkey = Pubkey::from_str_const("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA");
pub const GLOBAL_CONFIG: Pubkey = Pubkey::from_str_const("ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw");
pub const PROTOCOL_FEE_RECIPIENT: Pubkey = Pubkey::from_str_const("G5UZAVbAf46s7cKWoyKu8kYTip9DGTpbLZ2qa9Aq69dP");
pub const PROTOCOL_FEE_RECIPIENT_TOKEN_ACCOUNT: Pubkey = Pubkey::from_str_const("BWXT6RUhit9FfJQM3pBmqeFLPYmuxgmyhMGC5sGr8RbA");
pub const TOKEN_PROGRAM_ACCOUNT: Pubkey = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const SYSTEM_PROGRAM_ACCOUNT: Pubkey = Pubkey::from_str_const("11111111111111111111111111111111");
pub const ASSOCIATED_TOKEN_PROGRAM_ACCOUNT: Pubkey = Pubkey::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
pub const EVENT_AUTHORITY: Pubkey = Pubkey::from_str_const("GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR");

pub async fn pumpfun_amm_pool_info(
    connection: Arc<RpcClient>,
    pool_id: &Pubkey,
) -> Result<(PumpfunAmmPoolInfo, String, String, String, String), Box<dyn Error>> {
    let amm_account_info = connection.get_account(pool_id).await?;

    let pool_state = PumpFunMarketState::try_deserialize(&amm_account_info.data[8..])?;

    let keys = SwapAccountMetas::new_from_pool_state(pool_id, &pool_state);

    let mint_x = pool_state.base_mint.to_string();
    let mint_y = pool_state.quote_mint.to_string();
    let reserve_x = pool_state.pool_base_reserve.to_string();
    let reserve_y = pool_state.pool_quote_reserve.to_string();
    let pool_info = PumpfunAmmPoolInfo {
        keys,
        amm_flag: 3,
        owner: pool_id.to_string(),
    };
    Ok((pool_info, mint_x, mint_y, reserve_x, reserve_y))

}
