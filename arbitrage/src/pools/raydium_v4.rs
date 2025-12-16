use {
    crossbeam_channel::Sender,
    solana_sdk::{
        compute_budget::ComputeBudgetInstruction,
        
        clock::Slot,
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
            
        pools::price_maths::{
            V4Maths,
            SCALE_FEE_CONSTANT,
        },
        pools::pools_utils::{
            RaydiumV4PoolInfo,
            raydium_pool_info,
            raydium_pool_info_from_data 
        },  
        pools::{
            CongestionStats,
            LiquidityType,
            PoolUpdateType,
            RoutingStep,
            TokenLabel,
            Pool,
            PoolError,
            AmmType,
            WSOL_MINT,
            compute_kp_swap_amount,
        },
        tokens::pools_searcher::FarmableTokens,
    },
    std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        RwLock,
    },
    log::debug,
};

const RAYDIUM_V4_ID: Pubkey = Pubkey::from_str_const("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");

const RAYDIUM_DEFAULT_FEE_BPS: u128 = 2_500_000;
const PUMPFUN_BUDGET: u32 = 60_000;
const RAYDIUM_V4_SWAP_BUDGET: u32 = 150_000;

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

pub struct RaydiumV4Pool {
    pub address: String,
    pub mint_x: String,
    pub mint_y: String,
    pub reserve_x: String,
    pub reserve_y: String,
    pub fee_bps: u128,
    pub pool_info: RaydiumV4PoolInfo,
    pub writable_data: Arc<RwLock<WritableData>>,
    pub last_sent_slot: Arc<AtomicU64>,
    pub last_amount_in: Arc<AtomicU64>,
    pub farm_token_label: TokenLabel,
    pub congestion_stats: RwLock<CongestionStats>,
}

impl Hash for RaydiumV4Pool {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.address.hash(state);
    }
}

impl PartialEq for RaydiumV4Pool {
    fn eq(&self, other: &Self) -> bool {
        self.address == other.address
    }
}

impl Eq for RaydiumV4Pool {}

impl Pool for RaydiumV4Pool {

    fn as_raydium_v4(&self) -> Option<&RaydiumV4Pool> {
        Some(self)
    }

    fn as_raydium_v4_mut(&mut self) -> Option<&mut RaydiumV4Pool> {
        Some(self)
    }

    fn get_address(&self) -> String {
        self.address.to_string()
    }

    fn get_owner(&self) -> String {
        self.pool_info.owner.clone()
    }

    fn get_pool_type(&self) -> String {
        "Raydium-V4".to_string()
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

        Ok((writable_data.set_price(), swap_amount))
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

        let (user_token_in, user_token_out) = if is_buy {
            (user_wsol_account, user_token_account)
        } else {
            (user_token_account, user_wsol_account)
        };
        
        let accounts = self.pool_info.keys.get_with_user_and_tokens_pubkeys(
            user_token_in,
            user_token_out,
            signer_pubkey
        );

        let mut instruction_data = Vec::with_capacity(17);
        instruction_data.push(9); // Starting u8 value
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
                    program_id: RAYDIUM_V4_ID,
                    accounts: accounts,
                    data: instruction_data,
                }
            );
            instructions
        } else {

            let mut instructions = vec![
                Instruction {
                    program_id: RAYDIUM_V4_ID,
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
        instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(RAYDIUM_V4_SWAP_BUDGET));
        instructions.push(get_fee_instruction_with_profit(tip_amount, RAYDIUM_V4_SWAP_BUDGET));
        (instructions, RAYDIUM_V4_SWAP_BUDGET)
    }

    fn append_swap_step(
        &self,
        wallet_pubkey: Pubkey,
        accounts_meta: &mut Vec<AccountMeta>,
        data: &mut Vec<u8>,
        user_token_in: String,
        _mint_in: &String,
        user_token_out: String,
        _mint_out: &String,
        _extra_accounts: &Option<Vec<String>>
    ) {
        let accounts = self.pool_info.keys.get_with_user_and_tokens(
            user_token_in,
            user_token_out,
            wallet_pubkey
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

impl RaydiumV4Pool {
    pub fn new_from_data(
        address: Pubkey,
        farmable_tokens: &FarmableTokens,
        data: &[u8],
    ) -> Option<Self> {
        let pool_info = raydium_pool_info_from_data(&address, data);
        if let Ok((pool_info, mint_x, mint_y, reserve_x, reserve_y)) = pool_info {
            let farm_token_label = farmable_tokens.choose_farmable_tokens(&mint_x, &mint_y);
            Some(
                Self {
                    address: address.to_string(),
                    mint_x,
                    mint_y,
                    reserve_x,
                    reserve_y,
                    pool_info,
                    fee_bps: RAYDIUM_DEFAULT_FEE_BPS,
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
                    farm_token_label,
                    last_amount_in: Arc::new(AtomicU64::new(0)),
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
        let pool_info = raydium_pool_info(connection, &address).await;
        if let Ok((pool_info, mint_x, mint_y, reserve_x, reserve_y)) = pool_info {
            Some(
                Self {
                    address: address.to_string(),
                    mint_x,
                    mint_y,
                    reserve_x,
                    reserve_y,
                    pool_info,
                    fee_bps: RAYDIUM_DEFAULT_FEE_BPS,
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
                    farm_token_label,
                    last_amount_in: Arc::new(AtomicU64::new(0)),
                    congestion_stats: RwLock::new(CongestionStats::new_at_slot(0)),
                }
            )
        }
        else {
            None
        }
    }

}