use {
    crate::{
        tracker::{
            copytrade::{TradeDirection, TradeDecision, TradePosition},
            chart::WhaleSwap,
            order::{Order, BuyOrder, SellOrder},
        },
        simulation::pool_impact::BondingPoolType,
    },
};

const MIN_SWAP_SOL_AMOUNT: u128 = 200_000_000;
const MAX_SWAP_TOKEN_AMOUNT: u128 = 75_000_000_000_000;

pub enum Strategy {
    Buy(BuyStrategy),
    Sell(SellStrategy),
}

impl Strategy {
    pub fn get_order(&self, swap: &WhaleSwap, sol_balance: u128, token_held_by_whale: u128, pool: BondingPoolType) -> Option<Order> {
        match self {
            Strategy::Buy(buy_strategy) => buy_strategy.get_order(swap, sol_balance, pool),
            Strategy::Sell(sell_strategy) => sell_strategy.get_order(swap, token_held_by_whale, pool),
        }
    }
}
#[derive(Clone)]
pub enum BuyStrategy {
    FixedAmount(u128),
    Percentage(u128),
}

impl BuyStrategy{

    pub fn get_order(&self, swap: &WhaleSwap, sol_balance: u128, pool: BondingPoolType) -> Option<Order> {
        let trade_decision = self.get_trade_decision(swap);
        match trade_decision {
            TradeDecision::Buy(amount) => {
                if amount > sol_balance + 100_000_000{
                    return None;
                }
                Some(Order::Buy(BuyOrder::new(amount, swap.whale_address.clone(), pool, swap.post_token_reserve, swap.post_sol_reserve)))
            }
            _ => None,
        }
    }
    pub fn get_trade_decision(&self, swap: &WhaleSwap) -> TradeDecision {

        if swap.sol_amount < MIN_SWAP_SOL_AMOUNT || swap.token_amount >= MAX_SWAP_TOKEN_AMOUNT {
            return TradeDecision::None;
        }

        match self {
            BuyStrategy::FixedAmount(amount) => {
                TradeDecision::Buy(*amount)
            }
            BuyStrategy::Percentage(pct) => {
                TradeDecision::Buy(swap.sol_amount * pct / 100)
            }
        }
    }
}

#[derive(Clone)]
pub enum SellStrategy {
    FirstLead,
    Mimic,
}

impl SellStrategy {

    pub fn compute_sell_amount(&self, token_balance: u128, related_token_balance: u128, sell_pct: u128) -> (u128, bool) {
        let amount = match self {
            SellStrategy::FirstLead => {
                token_balance * sell_pct / 100

            }
            SellStrategy::Mimic => {
                related_token_balance * sell_pct / 100
            }
        };
        if amount > 98 * token_balance / 100 {
            return (token_balance, true);
        }
        return (amount, false);
    }

    pub fn get_order(&self, swap: &WhaleSwap, token_held_by_whale: u128, pool: BondingPoolType) -> Option<Order> {
        if token_held_by_whale == 0 {
            return None;
        }
        let mut whale_sell_pct = swap.token_amount * 100 / token_held_by_whale;
        if whale_sell_pct > 98{
            whale_sell_pct = 100;
        }
        return Some(Order::Sell(SellOrder::new(
            whale_sell_pct, 
            swap.whale_address.clone(), 
            self.clone(),
            pool
        )));
    }

    pub fn get_trade_decision(
        &self, 
        swap: &WhaleSwap, 
        token_held_by_whale: u128, 
        related_position_held: u128,
        self_position_held: u128
    ) -> TradeDecision {
        let mut whale_sell_pct = swap.token_amount * 100 / token_held_by_whale;
            if whale_sell_pct > 98{
                whale_sell_pct = 100;       
            }

        let amount_to_sell = match self {
            SellStrategy::FirstLead => {
                self_position_held * whale_sell_pct / 100
            },
            SellStrategy::Mimic => {
                related_position_held * whale_sell_pct / 100
            }
        };

        if amount_to_sell > 0 {
            TradeDecision::Sell(amount_to_sell)
        } else {
            TradeDecision::None
        }
    }
}

