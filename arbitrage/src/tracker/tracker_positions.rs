use {
    std::collections::HashMap,
    std::time::{Instant, SystemTime, UNIX_EPOCH},
};

pub const SOL_PRICE: u128 = 173;

pub struct TradeResult {
    pub total_sol_spent: u128,
    pub total_sol_received: u128,
    pub profit: i64,
    pub roi: f64,
    pub trade_duration: u64,
}

impl TradeResult {
    pub fn from_position(position: &TrackerPosition) -> Self {
        Self {
            total_sol_spent: position.total_sol_spent,
            total_sol_received: position.total_sol_received,
            profit: position.total_sol_received as i64 - position.total_sol_spent as i64,
            roi: (position.total_sol_received as f64 - position.total_sol_spent as f64) / (position.total_sol_spent as f64),
            trade_duration: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() - position.first_buy_timestamp,
        }
    }
}

pub enum PositionResult {
    FullyExited(TradeResult),
    InitialBuy(u128),
    In,
    UnsyncError,
}
pub struct TrackerPosition {
    pub token_address: String,
    pub total_token_amount_bought: u128,
    pub total_token_amount_sold: u128,
    pub token_remaining: u128,
    pub total_sol_spent: u128,
    pub total_sol_received: u128,
    pub first_buy_timestamp: u64,
    pub first_buy_mc: u128,
}

impl TrackerPosition {
    pub fn new(token_address: String, sol_amount: u128, token_amount: u128) -> Self {
        Self {
            token_address,
            total_token_amount_bought: token_amount,
            total_token_amount_sold: 0,
            token_remaining: token_amount,
            total_sol_spent: sol_amount,
            total_sol_received: 0,
            first_buy_mc: sol_amount * SOL_PRICE *1_000_000 / token_amount,
            first_buy_timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
        }
    }

    pub fn update(&mut self, sol_amount: u128, token_amount: u128, is_buy: bool) -> PositionResult {
        if is_buy {

            self.total_sol_spent += sol_amount;
            self.total_token_amount_bought += token_amount;
            self.token_remaining += token_amount;
        } else {
            self.total_token_amount_sold += token_amount;
            
            self.token_remaining.checked_sub(token_amount).ok_or(PositionResult::UnsyncError);
            self.total_sol_received += sol_amount;

            if self.total_token_amount_sold == self.total_token_amount_bought {
                return PositionResult::FullyExited(TradeResult::from_position(self));
            }
        }

        return PositionResult::In;
    }

    
}

pub struct TrackerWalletsPositions {
    pub positions: HashMap<String, Vec<TrackerPosition>>,
}
impl TrackerWalletsPositions {
    pub fn new_from_wallets(wallets: &Vec<String>) -> Self {
        Self {
            positions: wallets.iter().map(|wallet| (wallet.clone(), vec![])).collect(),
        }
    }

    pub fn update_with_swap(
        &mut self, 
        wallet_address: String, 
        token_address: String, 
        is_buy: bool, 
        sol_amount: u128, 
        token_amount: u128,
        ) -> (PositionResult, u128) {
        if !self.positions.contains_key(&wallet_address) {
            if is_buy {
                self.positions.insert(wallet_address.clone(), vec![TrackerPosition::new(token_address.clone(), sol_amount, token_amount)]);
                println!("Whale :{} \nInitial Buy token :{} \nAmount :{} \nSol In:{}\n\n", wallet_address, token_address, token_amount, sol_amount);

                return (PositionResult::InitialBuy(sol_amount), 0);
            }
            println!("Whale :{} \nUnsync Error token :{} \nAmount :{} \nSol Out:{}\n\n", wallet_address, token_address, token_amount, sol_amount);
            return (PositionResult::UnsyncError, 0);
            
            
        } else {
            let token_positions = self.positions.get_mut(&wallet_address).unwrap();
            match token_positions.iter_mut().enumerate().find(|(_, position)| position.token_address == token_address) {
                Some((index, position)) => {
                    let token_amount_remaining = position.token_remaining;
                    let position_result = position.update(sol_amount, token_amount, is_buy);
                    match &position_result {
                        PositionResult::FullyExited(trade_result) => {
                            println!(
                                "Whale :{} \n
                                Full Sell token :{} \n
                                Amount :{} \n
                                Sol Out:{}\n
                                Profit :{}\n
                                ROI :{}\n
                                Trade Duration :{}\n\n", 
                                wallet_address, token_address, token_amount, sol_amount, trade_result.profit, trade_result.roi, trade_result.trade_duration
                            );
                            token_positions.remove(index);
                        },
                        PositionResult::In => {
                            if is_buy {
                                println!("Whale :{} \nBuy token :{} \nAmount :{} \nSol In:{}\n\n", wallet_address, token_address, token_amount, sol_amount);
                            } else {
                                println!("Whale :{} \nSell token :{} \nAmount :{} \nSol Out:{}\n\n", wallet_address, token_address, token_amount, sol_amount);
                            }
                        }
                        PositionResult::UnsyncError => {
                            println!("Whale :{} \nUnsync Error token :{} \nAmount :{} \nSol Out:{}\n\n", wallet_address, token_address, token_amount, sol_amount);
                            token_positions.remove(index);
                        },
                        PositionResult::InitialBuy(_sol_amount) => {}
                    }
                    return (position_result, token_amount_remaining);
                },
                None => {
                    if is_buy {
                        println!("Whale :{} \nInitial Buy token :{} \nAmount :{} \nSol In:{}\n\n", wallet_address, token_address, token_amount, sol_amount);
                        token_positions.push(TrackerPosition::new(token_address.clone(), sol_amount, token_amount));
                        return (PositionResult::InitialBuy(sol_amount), 0);
                    }

                    println!("Whale :{} \nUnsync Error token :{} \nAmount :{} \nSol Out:{}\n\n", wallet_address, token_address, token_amount, sol_amount);
                    return (PositionResult::UnsyncError, 0);
                }
            }
        }
    }
}

pub fn read_whale_file(file_path: &str) -> Vec<String> {
    match std::fs::read_to_string(file_path) {
        Ok(content) => content
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect(),
        Err(e) => {
            println!("Failed to read whales.txt file: {}", e);
            Vec::new()
        }
    }
}