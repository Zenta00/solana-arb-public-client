use {
    std::{
        time::{Instant, Duration},
        sync::{Arc, RwLock, atomic::{AtomicU64, Ordering}},
        collections::{HashMap, HashSet},
    },
    solana_sdk::{
        pubkey::Pubkey,
        signature::{
            Keypair,
            Signer,
        },
        compute_budget::ComputeBudgetInstruction,
        transaction::VersionedTransaction,
        address_lookup_table::AddressLookupTableAccount,
        hash::Hash as BlockHash,
        system_instruction::transfer,
    },
    crate::{
        sender::transaction_sender::TransactionSender,
        tracker::{
            find_token_account,
            chart::WhaleSwap,
            strategy::{BuyStrategy, SellStrategy},
            order::Order,
        },
        wallets::wallet::{
            create_tx_with_address_table_lookup,
            get_fee_instruction_with_profit,
        },
        simulation::pool_impact::BondingPoolType,
    }
};

const SWAP_TIP_AMOUNT: u64 = 250_000;

const PRE_FUND_TIP_AMOUNT: u64 = 70_000;

const TX_EXPIRATION_TIME: Duration = Duration::from_secs(3);

pub struct TradePosition {
    total_sol_in: u128,
    total_sol_out: u128,
    total_token_bought: u128,
    total_token_sold: u128,
    num_buys: u128,
    num_sells: u128,
}

impl TradePosition {
    pub fn new() -> Self {
        Self { total_sol_in: 0, total_sol_out: 0, total_token_bought: 0, total_token_sold: 0, num_buys: 0, num_sells: 0 }
    }

    fn buy(&mut self, sol_amount: u128, token_amount: u128) -> u128 {
        self.total_sol_in += sol_amount;
        self.total_token_bought += token_amount;
        self.num_buys += 1;
        sol_amount
    }

    fn sell(&mut self, sol_amount: u128, token_amount: u128) -> u128 {
        self.total_sol_out += sol_amount;
        self.total_token_sold += token_amount;
        self.num_sells += 1;
        token_amount
    }

    pub fn total_token_held(&self) -> u128 {
        self.total_token_bought.saturating_sub(self.total_token_sold)
    }
}

pub struct CopyTradePosition {
    pub trade_position: TradePosition,
    pub related_whale_positions: HashMap<String, TradePosition>,
}

impl CopyTradePosition {

    pub fn new() -> Self {
        Self { trade_position: TradePosition::new(), related_whale_positions: HashMap::new() }
    }
    pub fn get_whale_position(&self, whale_address: &String) -> Option<&TradePosition> {
        self.related_whale_positions.get(whale_address)
    }

    pub fn get_whale_position_mut(&mut self, whale_address: &String) -> Option<&mut TradePosition> {
        self.related_whale_positions.get_mut(whale_address)
    }

    pub fn get_or_create_whale_position(&mut self, whale_address: &String) -> &mut TradePosition {
        self.related_whale_positions.entry(whale_address.clone()).or_insert(TradePosition::new())
    }

    pub fn update_position(&mut self, sol_amount: u128, token_amount: u128, direction: TradeDirection, whale_address: &String) {
            
        if direction == TradeDirection::Buy {
            let whale_position = self.get_or_create_whale_position(whale_address);
            whale_position.buy(sol_amount, token_amount);
            self.trade_position.buy(sol_amount, token_amount);
        } else {
            let whale_position = self.get_whale_position_mut(whale_address);
            if let Some(whale_position) = whale_position {
                whale_position.sell(sol_amount, token_amount);
                self.trade_position.sell(sol_amount, token_amount);
            }
            
        }
    }
}

#[derive(Eq, PartialEq)]
pub enum TradeDirection {
    Buy,
    Sell,
}

#[derive(Eq, PartialEq)]
pub enum TradeDecision {
    Buy(u128),
    Sell(u128),
    None,
}
pub struct CopyTrader {
    pub positions: HashMap<String, CopyTradePosition>,
    pub buy_strategy: BuyStrategy,
    pub sell_strategy: SellStrategy,
    pub buy_pct: u64,
    pub whales_tracked: HashSet<String>,
    pub wallet: Wallet,
    pub transactions: HashMap<String, (String,u64, Pubkey)>,
    pub pre_funded_wallets: PreFundedWallets,
    pub is_ready: bool,
}

impl CopyTrader {
    pub fn new(
        buy_strategy: BuyStrategy, 
        sell_strategy: SellStrategy, 
        buy_pct: u64, 
        whales_tracked: HashSet<String>,
        keypair: Keypair,
        address_lookup_table_account: Arc<Vec<AddressLookupTableAccount>>,
        wsol_account: Pubkey,
    ) -> Self {
        Self { 
            positions: HashMap::new(), 
            buy_strategy, 
            sell_strategy, 
            buy_pct, 
            whales_tracked,
            wallet: Wallet::new(keypair, address_lookup_table_account, wsol_account),
            transactions: HashMap::new(),
            pre_funded_wallets: PreFundedWallets::new(),
            is_ready: false,
        }
    }

    pub fn make_trade_decision(&mut self, swap: &WhaleSwap, token_held_by_whale: u128, pool: BondingPoolType) -> Option<Order> {
        if self.whales_tracked.contains(&swap.whale_address) {

            if swap.is_buy {
                return self.buy_strategy.get_order(swap, self.wallet.sol_balance, pool);
            } else {
                return self.sell_strategy.get_order(swap, token_held_by_whale, pool);
            }
        } 
        return None;
    }

    pub fn update_position(&mut self, token_address: &String, sol_amount: u128, token_amount: u128, direction: TradeDirection, whale_address: &String) {
        let position = self.positions.entry(token_address.clone()).or_insert(CopyTradePosition::new());
        if direction == TradeDirection::Buy {
            self.wallet.sol_balance -= sol_amount;
        } else {
            self.wallet.sol_balance += sol_amount;
        }
        position.update_position(sol_amount, token_amount, direction, whale_address);
        
    }

    pub fn get_related_positions(&self, token_address: &String, whale_address: &String) -> (u128, u128) {
        if let Some(position) = self.positions.get(token_address) {
            let token_balance = position.trade_position.total_token_held();
            if let Some(whale_position) = position.get_whale_position(whale_address) {
                return (token_balance, whale_position.total_token_held());
            }
        } 
        return (0, 0);
    }

    pub fn get_tx_from_order(
        &mut self, 
        order: &Order, 
        amount: u128, 
        token_address: &String,
        full_out: bool,
        blockhash: BlockHash,
        arbitrary_tip: u64,
        slot: u64,
    ) -> Result<VersionedTransaction, PreFundError> {
        let token_account = find_token_account(self.wallet.keypair.pubkey(), Pubkey::from_str_const(token_address));
        let (instructions, budget) = match order {
            Order::Buy(buy_order) => {
                buy_order.pool.get_swap_instructions(
                    amount as u64,
                    1, 
                    true, 
                    token_account, 
                    self.wallet.wsol_account, 
                    self.wallet.keypair.pubkey(), 
                    buy_order.post_token_reserve, 
                    buy_order.post_sol_reserve, 
                    false, 
                    SWAP_TIP_AMOUNT + arbitrary_tip
                )
            }
            Order::Sell(sell_order) => {
                sell_order.pool.get_swap_instructions(
                    amount as u64,
                    0,
                    false, 
                    token_account, 
                    self.wallet.wsol_account, 
                    self.wallet.keypair.pubkey(), 
                    0,
                    0, 
                    full_out, 
                    SWAP_TIP_AMOUNT + arbitrary_tip
                )
            }
        };
        let fee_payer = if let Ok(fee_payer) = self.pre_funded_wallets.get_pre_funded_wallet() {
            fee_payer
        } else {
            return Err(PreFundError::NoWalletAvailable);
        };
        let tx = create_tx_with_address_table_lookup(
            &instructions, 
            &self.wallet.address_lookup_table_account, 
            &self.wallet.keypair, 
            &fee_payer,
            blockhash
        ).unwrap();
        self.transactions.insert(tx.clone().signatures[0].to_string(), (token_address.clone(), slot, fee_payer.pubkey()));
        Ok(tx)
    }

    pub fn get_duplicate_txs_from_order(
        &mut self, 
        order: &Order, 
        amount: u128, 
        token_address: &String,
        full_out: bool,
        blockhash: BlockHash,
        slot: u64,
    ) -> Result<Vec<VersionedTransaction>, PreFundError> {
        let token_account = find_token_account(self.wallet.keypair.pubkey(), Pubkey::from_str_const(token_address));
        let num_tx;
        let (instructions, budget) = match order {
            Order::Buy(buy_order) => {
                num_tx = 15;
                buy_order.pool.get_swap_instructions(
                    amount as u64,
                    1, 
                    true, 
                    token_account, 
                    self.wallet.wsol_account, 
                    self.wallet.keypair.pubkey(), 
                    buy_order.post_token_reserve, 
                    buy_order.post_sol_reserve, 
                    false, 
                    SWAP_TIP_AMOUNT
                )
            }
            Order::Sell(sell_order) => {
                num_tx = 25;
                sell_order.pool.get_swap_instructions(
                    amount as u64,
                    0,
                    false, 
                    token_account, 
                    self.wallet.wsol_account, 
                    self.wallet.keypair.pubkey(), 
                    0,
                    0, 
                    full_out, 
                    SWAP_TIP_AMOUNT
                )
            }
        };

        let ixs_len = instructions.len();

        let mut txs = Vec::new();
        
        let fee_payer = if let Ok(fee_payer) = self.pre_funded_wallets.get_pre_funded_wallet() {
            fee_payer
        } else {
            return Err(PreFundError::NoWalletAvailable);
        };

        for i in 0..num_tx {
            let mut instructions = instructions.clone();
            instructions[ixs_len - 1] = get_fee_instruction_with_profit(SWAP_TIP_AMOUNT + i, budget);
            let tx = create_tx_with_address_table_lookup(
                &instructions, 
                &self.wallet.address_lookup_table_account,
                &self.wallet.keypair,
                &fee_payer,
                blockhash
            ).unwrap();
            self.transactions.insert(tx.clone().signatures[0].to_string(), (token_address.clone(), slot, fee_payer.pubkey()));
            txs.push(tx);
        }
        
        Ok(txs)
    }

    pub fn clear_transactions_until_slot(&mut self, slot: u64) {
        self.transactions.retain(|_, (_, tx_slot, _)| *tx_slot < slot);
    }

    pub fn set_is_ready(&mut self) {
        if self.pre_funded_wallets.is_ready() {
            self.is_ready = true;
        }
    }

    pub fn is_ready(&self) -> bool {
        self.is_ready
    }
}

pub struct Wallet {
    pub wallet_str: String,
    pub keypair: Keypair,
    pub sol_balance: u128,
    pub token_balances: HashMap<String, u128>,
    pub address_lookup_table_account: Arc<Vec<AddressLookupTableAccount>>,
    pub wsol_account: Pubkey,
}

impl Wallet {
    pub fn new(keypair: Keypair, address_lookup_table_account: Arc<Vec<AddressLookupTableAccount>>, wsol_account: Pubkey) -> Self {
        let wallet_str = keypair.pubkey().to_string();
        Self { 
            wallet_str, 
            keypair, 
            sol_balance: 1_000_000_000, 
            token_balances: HashMap::new(), 
            address_lookup_table_account, 
            wsol_account
        }
    }

    pub fn add_sol_balance(&mut self, amount: u128) {
        self.sol_balance += amount;
    }

    pub fn add_token_balance(&mut self, amount: u128, token_address: &String) {
        *self.token_balances.entry(token_address.clone()).or_insert(0) += amount;
    }

    pub fn remove_token_balance(&mut self, amount: u128, token_address: &String) {
        *self.token_balances.get_mut(token_address).unwrap() -= amount;
    }

    pub fn get_token_balance(&self, token_address: &String) -> u128 {
        *self.token_balances.get(token_address).unwrap_or(&0)
    }
    
    
}

pub struct PreFundedWallets {
    pub wallets: Vec<PreFundedWallet>,
    pub initialized: bool,
}

impl PreFundedWallets {
    pub fn new() -> Self {
        Self { wallets: Vec::new(), initialized: false }
    }

    pub fn generate_new_pre_funded_wallets(
        &mut self,
        main_wallet: &Wallet, 
        num_wallets: u64, 
        with_balance: u64, 
        blockhash: BlockHash,
        tx_sender: &Arc<TransactionSender>,
    ) {
        let mut instructions = Vec::new();
        let main_wallet_pubkey = main_wallet.keypair.pubkey();
        let mut wallets_balances = Vec::new();
        for _ in 0..num_wallets {
            let keypair = Keypair::new();
            let pubkey = keypair.pubkey();
            let wallet = PreFundedWallet {
                pubkey,
                keypair: keypair.insecure_clone(),
                balance: Arc::new(AtomicU64::new(0)),
                last_time_used: Instant::now(),
            };

            instructions.push(
                transfer(
                    &main_wallet_pubkey,
                    &keypair.pubkey(),
                    with_balance
                )
            );
            wallets_balances.push(wallet.balance.clone());
            self.wallets.push(wallet);
        }
        instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(10_000));
        instructions.push(get_fee_instruction_with_profit(PRE_FUND_TIP_AMOUNT, 10_000));

        let tx = create_tx_with_address_table_lookup(
            &instructions, 
            &main_wallet.address_lookup_table_account, 
            &main_wallet.keypair,
            &main_wallet.keypair,
            blockhash
        ).unwrap();

        tx_sender.send_and_confirm_pre_funding(
            tx,
            10,
            200,
            Duration::from_secs(10),
            wallets_balances,
            SWAP_TIP_AMOUNT,
        );
    }

    pub fn get_pre_funded_wallet(&mut self) -> Result<Keypair, PreFundError> {
        for wallet in self.wallets.iter_mut() {
            if wallet.last_time_used.elapsed() > TX_EXPIRATION_TIME && wallet.balance.load(Ordering::Relaxed) > 0 {
                wallet.last_time_used = Instant::now();
                return Ok(wallet.keypair.insecure_clone());
            }
        }
        return Err(PreFundError::NoWalletAvailable);
    }

    pub fn remove_prefunded_wallet(&mut self, pubkey: &Pubkey) {
        self.wallets.retain(|w| w.pubkey != *pubkey);
    }

    pub fn set_wallet_balance(&mut self, pubkey: &Pubkey, balance: u64) {
        for wallet in self.wallets.iter_mut() {
            if wallet.pubkey == *pubkey {
                wallet.balance.store(balance, Ordering::Relaxed);
                self.initialized = true;
            }
        }
    }

    pub fn is_ready(&self) -> bool {
        for wallet in self.wallets.iter() {
            if wallet.balance.load(Ordering::Relaxed) > 0 {
                return true;
            }
        }
        return false;
    }
}

pub struct PreFundedWallet {
    pubkey: Pubkey,
    pub keypair: Keypair,
    pub balance: Arc<AtomicU64>,
    pub last_time_used: Instant,
}

pub enum PreFundError {
    NoWalletAvailable,
}