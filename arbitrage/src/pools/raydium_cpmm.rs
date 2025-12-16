use {
    borsh::{BorshDeserialize, BorshSerialize},
    std::error::Error,
    crossbeam_channel::Sender,
    solana_sdk::{
        clock::Slot,
        compute_budget::ComputeBudgetInstruction,
        pubkey::Pubkey,
        transaction::Transaction,
        signature::{
            Signature,
            Signer,
        },
        system_instruction,
        instruction::{AccountMeta, Instruction},
    },
    solana_client::nonblocking::rpc_client::RpcClient,
    std::hash::{Hash, Hasher},
    std::cmp::{Eq, PartialEq},
    crate::{
        wallets::wallet::{get_fee_instruction_with_profit, get_pre_transfer_tip_instruction_with_signer},
        tracker::*, 
        simulation::router::SwapDirection,
        pools::{
            CongestionStats,
            WSOL_MINT
        },
            
        pools::price_maths::{
            V4Maths,
            SCALE_FEE_CONSTANT,
        },
        tokens::pools_searcher::FarmableTokens,
        pools::{
            LiquidityType,
            PoolUpdateType,
            RoutingStep,
            TokenLabel,
            Pool,
            PoolError,
            AmmType,
            compute_kp_swap_amount,
        },
    },
    std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        RwLock,
    },
    log::debug,
};

pub const RAYDIUM_CPMM_BUDGET: u32 = 50_000;
pub const RAYDIUM_CPMM_SWAP_BUDGET: u32 = 150_000;
pub const TOKEN_PROGRAM_ACCOUNT: Pubkey = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const RAYDIUM_AUTHORITY_ID: Pubkey = Pubkey::from_str_const("GpMZbSM2GgvTKHJirzeGfMFoaZ8UR2X7F4v8vHTvxFbL");

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

pub struct RaydiumCpmmPool {
    pub address: String,
    pub mint_x: String,
    pub mint_y: String,
    pub reserve_x: String,
    pub reserve_y: String,
    pub fee_bps: u128,
    pub pool_info: RaydiumCpmmPoolInfo,
    pub writable_data: Arc<RwLock<WritableData>>,
    pub last_sent_slot: Arc<AtomicU64>,
    pub last_amount_in: Arc<AtomicU64>,
    pub farm_token_label: TokenLabel,
    pub congestion_stats: RwLock<CongestionStats>,
}

impl Hash for RaydiumCpmmPool {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.address.hash(state);
    }
}

impl PartialEq for RaydiumCpmmPool {
    fn eq(&self, other: &Self) -> bool {
        self.address == other.address
    }
}

impl Eq for RaydiumCpmmPool {}

impl Pool for RaydiumCpmmPool {

    fn as_raydium_cpmm(&self) -> Option<&RaydiumCpmmPool> {
        Some(self)
    }

    fn as_raydium_cpmm_mut(&mut self) -> Option<&mut RaydiumCpmmPool> {
        Some(self)
    }

    fn get_address(&self) -> String {
        self.address.to_string()
    }

    fn get_owner(&self) -> String {
        self.pool_info.owner.clone()
    }

    fn get_pool_type(&self) -> String {
        "Raydium-Cpmm".to_string()
    }

    fn get_liquidity_type(&self) -> LiquidityType {
        LiquidityType::ConstantProduct
    }

    fn get_fee_bps(&self) -> u128 {
        self.fee_bps
    }

    fn set_price(&self, _update_pool_sender: &Sender<(String, PoolUpdateType)>) -> Result<bool, PoolError> {
        let mut writable_data = self.writable_data.write().unwrap();
        Ok(writable_data.set_price())
    }

    

    fn update_with_balances(&self, x_amount: u128, y_amount: u128, optional_data: &[u8]) -> Result<(bool, Option<(u128,u128,bool, u128, u128)>), PoolError> {

        let protocol_fee_x = u64::from_le_bytes(optional_data[341..349].try_into().unwrap()) as u128;
        let protocol_fee_y = u64::from_le_bytes(optional_data[349..357].try_into().unwrap()) as u128;
        let fund_fee_x = u64::from_le_bytes(optional_data[357..365].try_into().unwrap()) as u128;
        let fund_fee_y = u64::from_le_bytes(optional_data[365..373].try_into().unwrap()) as u128;

        let total_fee_x = protocol_fee_x + fund_fee_x;
        let total_fee_y = protocol_fee_y + fund_fee_y;

        if total_fee_x > x_amount {
            return Err(PoolError::NotEnoughLiquidity);
        }

        if total_fee_y > y_amount {
            return Err(PoolError::NotEnoughLiquidity);
        }

        let amount_x_after_fees = x_amount - total_fee_x;
        let amount_y_after_fees = y_amount - total_fee_y;
                
        let mut writable_data = self.writable_data.write().unwrap();
        
        writable_data.prev_reserve_x_amount = writable_data.reserve_x_amount;
        writable_data.prev_reserve_y_amount = writable_data.reserve_y_amount;
        writable_data.reserve_x_amount = amount_x_after_fees;
        writable_data.reserve_y_amount = amount_y_after_fees;
        writable_data.k = amount_x_after_fees * amount_y_after_fees;
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
                &self.farm_token_label
            );

            let (post_reserve_sol_amount, post_reserve_token_amount) = if self.farm_token_label == TokenLabel::Y {
                (writable_data.reserve_y_amount, writable_data.reserve_x_amount)
            } else {
                (writable_data.reserve_x_amount, writable_data.reserve_y_amount)
            };

            kp_swap.map(|(amount1, amount2, direction)| 
                (amount1, amount2, direction, post_reserve_sol_amount, post_reserve_token_amount)
            )
        } else {
            None
        };

        Ok((
            writable_data.set_price(), 
            swap_amount,
        ))
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
            false,
        );
        Ok((amount_out as f64, RAYDIUM_CPMM_BUDGET as f64))
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
        vec![]
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
        reserve_token_amount: u128,
        reserve_sol_amount: u128,
        full_out: bool,
        tip_amount: u64,
    ) -> (Vec<Instruction>, u32) {

        let (user_token_in, user_token_out, inverse_x_y) = if is_buy {
            (user_wsol_account, user_token_account, &self.mint_x == WSOL_MINT)
        } else {
            (user_token_account, user_wsol_account, &self.mint_y == WSOL_MINT)
        };
        
        let accounts = self.pool_info.keys.get_with_user_and_tokens_pubkeys(
            user_token_in,
            user_token_out,
            signer_pubkey,
            inverse_x_y
        );

        let mut instruction_data = Vec::with_capacity(24);
        let disciminator = [0x8f, 0xbe, 0x5a, 0xda, 0xc4, 0x1e, 0x33, 0xde];
        instruction_data.extend_from_slice(&disciminator);
        instruction_data.extend_from_slice(&amount_in.to_le_bytes());

        let k = reserve_token_amount * reserve_sol_amount;
        let amount_out = V4Maths::get_amount_out_with_fee(
            amount_in as u128, 
            reserve_token_amount, 
            reserve_sol_amount, 
            k, 
            !is_buy, 
            self.fee_bps,
            false,
        ) as u64;
        let min_amount_out = amount_out * (100 - slippage) / 100;
        instruction_data.extend_from_slice(&min_amount_out.to_le_bytes());

        let mut instructions = if is_buy {
            let mut instructions = wrap_sol_instruction(
                amount_in,
                signer_pubkey,
                user_wsol_account,
            );
            instructions.push(create_token_account(
                signer_pubkey,
                Pubkey::from_str_const(&self.mint_x),
            ));
            instructions.push(
                Instruction {
                    program_id: RAYDIUM_CPMM_ID,
                    accounts: accounts,
                    data: instruction_data,
                }
            );
            instructions
        } else {

            let mut instructions = vec![
                Instruction {
                    program_id: RAYDIUM_CPMM_ID,
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
        instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(RAYDIUM_CPMM_SWAP_BUDGET));
        instructions.push(get_fee_instruction_with_profit(tip_amount, RAYDIUM_CPMM_SWAP_BUDGET));
        (instructions, RAYDIUM_CPMM_SWAP_BUDGET)
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
        let inverse_x_y = if mint_in == &self.mint_x {
            false
        } else {
            true
        };
        let accounts = self.pool_info.keys.get_with_user_and_tokens(
            user_token_in,
            user_token_out,
            wallet_pubkey,
            inverse_x_y
        );

        accounts_meta.extend(accounts);
        
        data.push(self.pool_info.amm_flag);
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

impl RaydiumCpmmPool {

    pub fn new_from_data(
        address: Pubkey,
        farmable_tokens: &FarmableTokens,
        data: &[u8],
    ) -> Option<Self> {
        let pool_info = cpmm_pool_info_from_data(&address, data);
        if let Ok((pool_info, mint_x, mint_y, reserve_x, reserve_y, fee_bps)) = pool_info {
            let farm_token_label = farmable_tokens.choose_farmable_tokens(&mint_x, &mint_y);
            Some(
                Self {
                    address: address.to_string(),
                    mint_x,
                    mint_y,
                    reserve_x,
                    reserve_y,
                    pool_info,
                    fee_bps: fee_bps as u128,
                    last_sent_slot: Arc::new(AtomicU64::new(0)),
                    writable_data: Arc::new(RwLock::new(WritableData {
                        price: 0,
                        inversed_price: 0,
                        prev_reserve_x_amount: 0,
                        prev_reserve_y_amount: 0,
                        reserve_x_amount: 0,
                        reserve_y_amount: 0,
                        k: 0,
                    })),
                    last_amount_in: Arc::new(AtomicU64::new(0)),
                    farm_token_label,
                    congestion_stats: RwLock::new(CongestionStats::new_at_slot(0)),
                }
            )
        }
        else {
            None
        }
    }
    pub async fn new(
        address: Pubkey, 
        connection: Arc<RpcClient>,
        farm_token_label: TokenLabel,
    ) -> Option<Self> {
        let pool_info = cpmm_pool_info(connection, &address).await;
        if let Ok((pool_info, mint_x, mint_y, reserve_x, reserve_y, fee_bps)) = pool_info {
            Some(
                Self {
                    address: address.to_string(),
                    mint_x,
                    mint_y,
                    reserve_x,
                    reserve_y,
                    pool_info,
                    fee_bps: fee_bps as u128,
                    last_sent_slot: Arc::new(AtomicU64::new(0)),
                    writable_data: Arc::new(RwLock::new(WritableData {
                        price: 0,
                        inversed_price: 0,
                        prev_reserve_x_amount: 0,
                        prev_reserve_y_amount: 0,
                        reserve_x_amount: 0,
                        reserve_y_amount: 0,
                        k: 0,
                    })),
                    last_amount_in: Arc::new(AtomicU64::new(0)),
                    farm_token_label,
                    congestion_stats: RwLock::new(CongestionStats::new_at_slot(0)),
                }
            )
        }
        else {
            None
        }
    }

}

#[derive(Debug, Clone)]
pub struct CpmmMarketState {
    pub amm_config: Pubkey,             // publicKey
    pub pool_creator: Pubkey,           // publicKey
    pub token0_vault: Pubkey,           // publicKey
    pub token1_vault: Pubkey,           // publicKey
    pub lp_mint: Pubkey,                // publicKey
    pub token0_mint: Pubkey,            // publicKey
    pub token1_mint: Pubkey,            // publicKey
    pub token0_program: Pubkey,         // publicKey
    pub token1_program: Pubkey,         // publicKey
    pub observation_key: Pubkey,        // publicKey
}

impl CpmmMarketState {
    pub fn try_deserialize(data: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        let mut slice = data;
        Ok(Self {
            amm_config: Pubkey::try_from_slice(&slice[..32])?,
            pool_creator: Pubkey::try_from_slice(&slice[32..64])?,
            token0_vault: Pubkey::try_from_slice(&slice[64..96])?,
            token1_vault: Pubkey::try_from_slice(&slice[96..128])?,
            lp_mint: Pubkey::try_from_slice(&slice[128..160])?,
            token0_mint: Pubkey::try_from_slice(&slice[160..192])?,
            token1_mint: Pubkey::try_from_slice(&slice[192..224])?,
            token0_program: Pubkey::try_from_slice(&slice[224..256])?,
            token1_program: Pubkey::try_from_slice(&slice[256..288])?,
            observation_key: Pubkey::try_from_slice(&slice[288..320])?,
        })
    }
}


#[derive(Debug, Clone)]
pub struct SwapAccountMetas {
    pub vec: Vec<AccountMeta>,
}

impl SwapAccountMetas {
    pub fn get_with_user_and_tokens_pubkeys(
        &self, 
        in_mint: Pubkey, 
        out_mint: Pubkey, 
        user_pubkey: Pubkey,
        inverse_x_y: bool
    ) -> Vec<AccountMeta> {
        let mut vec = self.vec.clone();
        vec[5] = AccountMeta::new(in_mint, false);
        vec[6] = AccountMeta::new(out_mint, false);
        vec[1] = AccountMeta::new(user_pubkey, true);
        
        if inverse_x_y {
            // Swap positions 7 and 8
            let temp = vec[7].clone();
            vec[7] = vec[8].clone();
            vec[8] = temp;
            
            // Swap positions 10 and 11
            let temp = vec[10].clone();
            vec[10] = vec[11].clone();
            vec[11] = temp;
        }
        
        vec.insert(10, AccountMeta::new_readonly(TOKEN_PROGRAM_ACCOUNT, false));
        let raydium_cpmm = vec.pop().unwrap();
        vec.push(raydium_cpmm);
        vec
    } 

    pub fn get_with_user_and_tokens(
        &self, 
        in_mint: String, 
        out_mint: String, 
        user_pubkey: Pubkey,
        inverse_x_y: bool
    ) -> Vec<AccountMeta> {
        let mut vec = self.vec.clone();
        vec[5] = AccountMeta::new(Pubkey::from_str_const(&in_mint), false);
        vec[6] = AccountMeta::new(Pubkey::from_str_const(&out_mint), false);
        vec[1] = AccountMeta::new(user_pubkey, true);
        
        if inverse_x_y {
            // Swap positions 7 and 8
            let temp = vec[7].clone();
            vec[7] = vec[8].clone();
            vec[8] = temp;
            
            // Swap positions 10 and 11
            let temp = vec[10].clone();
            vec[10] = vec[11].clone();
            vec[11] = temp;
        }
        
        vec
    }   
}
#[derive(Debug, Clone)]
pub struct RaydiumCpmmPoolInfo {
    pub keys: SwapAccountMetas,
    pub amm_flag: u8,
    pub owner: String,
}

// Constants for market versions
pub const RAYDIUM_CPMM_ID: Pubkey = Pubkey::from_str_const("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C");

pub fn cpmm_pool_info_from_data(
    address: &Pubkey,
    data: &[u8],
) -> Result<(RaydiumCpmmPoolInfo, String, String, String, String, u64), Box<dyn Error>> {
    let pool_state = CpmmMarketState::try_deserialize(&data[8..])?;
    let fee_bps = 2_500_000;

    let reserve_x = AccountMeta::new(pool_state.token0_vault, false);
    let reserve_y = AccountMeta::new(pool_state.token1_vault, false);
    let reserve_x_pubkey = pool_state.token0_vault.to_string();
    let reserve_y_pubkey = pool_state.token1_vault.to_string();

    let keys= SwapAccountMetas{
        vec: vec![
            AccountMeta::new_readonly(RAYDIUM_CPMM_ID.clone(), false),
            AccountMeta::default(),
            AccountMeta::new_readonly(RAYDIUM_AUTHORITY_ID.clone(), false),
            AccountMeta::new_readonly(pool_state.amm_config, false),
            AccountMeta::new(*address, false),
            AccountMeta::default(),
            AccountMeta::default(),
            reserve_x,
            reserve_y,
            AccountMeta::new_readonly(TOKEN_PROGRAM_ACCOUNT.clone(), false),
            AccountMeta::new_readonly(pool_state.token0_mint, false),
            AccountMeta::new_readonly(pool_state.token1_mint, false),
            AccountMeta::new(pool_state.observation_key, false),
        ]
    };

    Ok((
        RaydiumCpmmPoolInfo {
            keys,
            amm_flag: 2,
            owner: RAYDIUM_AUTHORITY_ID.to_string(),
        },
        pool_state.token0_mint.to_string(),
        pool_state.token1_mint.to_string(),
        reserve_x_pubkey,
        reserve_y_pubkey,
        fee_bps
    ))
    
}

pub async fn cpmm_pool_info(
    connection: Arc<RpcClient>,
    pool_id: &Pubkey,
) -> Result<(RaydiumCpmmPoolInfo, String, String, String, String, u64), Box<dyn Error>> {
    let amm_account_info = connection.get_account(pool_id).await?;

    let pool_state = CpmmMarketState::try_deserialize(&amm_account_info.data[8..])?;

    let amm_config = connection.get_account(&pool_state.amm_config).await?;

    // Extract fee information from the amm_config data
    // The fee is stored as a u64 preceded by a u32 in the data structure
    let fee_offset = 8+4; // Skip the first u32
    let fee_bytes = &amm_config.data[fee_offset..fee_offset + 8]; // Read 8 bytes for u64
    let fee_bps = u64::from_le_bytes(fee_bytes.try_into().unwrap()) * 1_000;

    

    let reserve_x = AccountMeta::new(pool_state.token0_vault, false);
    let reserve_y = AccountMeta::new(pool_state.token1_vault, false);
    let reserve_x_pubkey = pool_state.token0_vault.to_string();
    let reserve_y_pubkey = pool_state.token1_vault.to_string();

    let keys= SwapAccountMetas{
        vec: vec![
            AccountMeta::new_readonly(RAYDIUM_CPMM_ID.clone(), false),
            AccountMeta::default(),
            AccountMeta::new_readonly(RAYDIUM_AUTHORITY_ID.clone(), false),
            AccountMeta::new_readonly(pool_state.amm_config, false),
            AccountMeta::new(*pool_id, false),
            AccountMeta::default(),
            AccountMeta::default(),
            reserve_x,
            reserve_y,
            AccountMeta::new_readonly(TOKEN_PROGRAM_ACCOUNT.clone(), false),
            AccountMeta::new_readonly(pool_state.token0_mint, false),
            AccountMeta::new_readonly(pool_state.token1_mint, false),
            AccountMeta::new(pool_state.observation_key, false),
        ]
    };

    Ok((
        RaydiumCpmmPoolInfo {
            keys,
            amm_flag: 2,
            owner: RAYDIUM_AUTHORITY_ID.to_string(),
        },
        pool_state.token0_mint.to_string(),
        pool_state.token1_mint.to_string(),
        reserve_x_pubkey,
        reserve_y_pubkey,
        fee_bps
    ))
}