use {
    crate::{
        tracker::copytrade::CopyTrader,
        tracker::strategy::{
            Strategy,
            BuyStrategy,
            SellStrategy,
        },
        simulation::pool_impact::BondingPoolType,
    },
    std::collections::VecDeque,
    std::time::{Instant, Duration},
};

pub struct OrderQueues {
    pub orders: Vec<TokenOrderQueue>,
    pub state_changed: bool,
    pub last_update_timestamp: Instant,
}

impl OrderQueues {
    pub fn new() -> Self {
        Self { orders: Vec::new(), state_changed: false, last_update_timestamp: Instant::now() }
    }

    pub fn add_order(&mut self, token_address: &String, order: Order) {
        let token_order_queue = self.orders.iter_mut().find(|queue| queue.token_address == *token_address);
        if let Some(token_order_queue) = token_order_queue {
            token_order_queue.add_order(order);
        } else {
            let mut token_order_queue = TokenOrderQueue::new(token_address.clone());
            token_order_queue.add_order(order);
            self.orders.push(token_order_queue);
        }
        self.state_changed = true;
    }

    pub fn confirm_order(&mut self, token_address: &String) -> Option<Order> {
        let token_order_queue = self.orders.iter_mut().find(|queue| queue.token_address == *token_address);
        if let Some(token_order_queue) = token_order_queue {
            self.state_changed = true;
            return token_order_queue.confirm_order();
        } else {
            println!("Token order queue not found");
            return None;
        }
    }

    pub fn advance_all_order_queues(
        &mut self,
        awaiting_orders: &mut Vec<(String, Order, u128, bool)>,
        copytrader: &mut CopyTrader
    ) {
        for token_order_queue in self.orders.iter_mut() {
            token_order_queue.advance_order_queue(awaiting_orders, copytrader);
        }
        self.state_changed = false;
        self.last_update_timestamp = Instant::now();
    }

    pub fn need_check_orders(&self) -> bool {
        self.last_update_timestamp.elapsed() > Duration::from_secs(5)
    }
    
}
    
pub struct TokenOrderQueue {
    pub token_address: String,
    pub last_update_timestamp: Instant,
    pub orders: VecDeque<Order>,
}

impl TokenOrderQueue {
    pub fn new(token_address: String) -> Self {
        Self { 
            token_address, 
            last_update_timestamp: Instant::now(), 
            orders: VecDeque::new() 
        }
    }

    pub fn add_order(&mut self, order: Order) {
        self.orders.push_back(order);
    }

    pub fn confirm_order(&mut self) -> Option<Order> {
        if let Some(order) = self.orders.pop_front() {
            return Some(order);
        } else {
            println!("Order not found");
            return None;
        }
    }

    pub fn advance_order_queue(
        &mut self,
        awaiting_orders: &mut Vec<(String, Order, u128, bool)>,
        copytrader: &mut CopyTrader
    ) {
        loop {
            if let Some(order) = self.orders.front_mut() {
                match order {
                    Order::Buy(buy_order) => {
                        if buy_order.order_status == OrderStatus::AwaitingSend {
                            buy_order.order_status = OrderStatus::Pending;
                            awaiting_orders.push((self.token_address.clone(), Order::Buy(buy_order.clone()), buy_order.buy_amount, false));
                            self.last_update_timestamp = Instant::now();
                            break;
                        } else if buy_order.order_status == OrderStatus::Pending {
                            if self.last_update_timestamp.elapsed() > Duration::from_secs(5) {
                                println!("Buy order timed out");
                                self.orders.pop_front();
                            }
                        } else {
                            self.orders.pop_front();
                        }
                    }
                    Order::Sell(sell_order) => {
                        if sell_order.order_status == OrderStatus::AwaitingSend {
                            let (token_balance, related_token_balance) = 
                                copytrader.get_related_positions(&self.token_address, &sell_order.whale_source);
                            if token_balance == 0 || related_token_balance == 0 {
                                self.orders.pop_front();
                                continue;
                            }
                            let (sell_amount, is_full_sell) = 
                                sell_order.strategy.compute_sell_amount(token_balance, related_token_balance, sell_order.sell_pct);
                            sell_order.order_status = OrderStatus::Pending;
                            awaiting_orders.push((self.token_address.clone(), Order::Sell(sell_order.clone()), sell_amount, is_full_sell));
                            self.last_update_timestamp = Instant::now();
                            break;
                        } else if sell_order.order_status == OrderStatus::Pending {
                            if self.last_update_timestamp.elapsed() > Duration::from_secs(5) {
                                println!("Sell order timed out");
                                self.orders.pop_front();
                            }
                        } else {
                            self.orders.pop_front();
                        }
                    }
                }
            } else {
                break;
            }
        }
    }
    
    
    
}

pub enum Order{
    Buy(BuyOrder),
    Sell(SellOrder),
}

impl Order {
    pub fn get_whale_address(&self) -> String {
        match self {
            Order::Buy(buy_order) => buy_order.whale_source.clone(),
            Order::Sell(sell_order) => sell_order.whale_source.clone(),
        }
    }

    pub fn is_buy(&self) -> bool {
        match self {
            Order::Buy(_) => true,
            Order::Sell(_) => false,
        }
    }
}
#[derive(PartialEq, Eq, Clone)]
pub enum OrderStatus {
    AwaitingSend,
    Pending,
    Filled,
}

#[derive(Clone)]
pub struct BuyOrder {
    buy_amount: u128,
    whale_source: String,
    order_status: OrderStatus,
    pub post_token_reserve: u128,
    pub post_sol_reserve: u128,
    pub pool: BondingPoolType,
}

impl BuyOrder {
    pub fn new(buy_amount: u128, whale_source: String, pool: BondingPoolType, post_token_reserve: u128, post_sol_reserve: u128) -> Self {
        Self { buy_amount, whale_source, order_status: OrderStatus::AwaitingSend, pool, post_token_reserve, post_sol_reserve }
    }
    
}

#[derive(Clone)]
pub struct SellOrder {
    sell_pct: u128,
    strategy: SellStrategy,
    whale_source: String,
    order_status: OrderStatus,
    pub pool: BondingPoolType,
}

impl SellOrder {
    pub fn new(sell_pct: u128, whale_source: String, strategy: SellStrategy, pool: BondingPoolType) -> Self {
        Self { sell_pct, strategy, whale_source, order_status: OrderStatus::AwaitingSend, pool }
    }

}
    