use {
    super::pool_impact::PoolImpacted, crate::{
        
        wallets::wallet::{
            WalletError,
            Wallet
        },
        pools::{
            spam_pools::{
                PoolPair,
                ArbSpamPoolType,
            },
            PoolInitialize,
            TokenLabel,
            price_maths::{
            a_lshift_64_div_b, a_times_b_rshift_64, combine_fee_bps, price_differ, Fee
        }, Pool, PoolConstants, PoolError, RoutingStep}, 
        simulation::{
            optimization::{
                optimize_arbitrage_profit, OptimizeBrent
            }, simulator_engine::{
                MintPools,
                SimulatorEngine
            }, simulator_safe_maths::SafeMath
        }, wallets::wallet::TokenAccount
    }, crossbeam_channel::Sender, solana_sdk::instruction::{AccountMeta, Instruction}, std::{collections::{
        HashMap,
        HashSet,
    }, error::Error, sync::{
        atomic::Ordering, Arc, RwLock
    }}, solana_sdk::signature::Signer
};

pub const PROFIT_SCALE: f64 = 1_000.0;
pub const MAX_STEP_COUNT: u8 = 1;
pub const PARTIAL_SIMULATION_MAX_ITER: u128 = 30;
pub const FULL_SIMULATION_MAX_ITER: u128 = 100;
pub const MIN_PROFIT: u64 = 50_000;

#[derive(PartialEq, Eq)]
pub enum InvertLabel{
    A,
    B,
    None,
}

#[derive(PartialEq, Eq, Clone)]
pub enum SwapDirection {
    XtoY,
    YtoX,
    Unknown,
}

impl SwapDirection {
    pub fn from_price_difference(a_is_cheaper: bool, invert_label: InvertLabel) -> (Self, Self) {
        let (swap_direction_a, swap_direction_b) = if a_is_cheaper {
            (SwapDirection::YtoX, SwapDirection::XtoY)
        } else {
            (SwapDirection::XtoY, SwapDirection::YtoX)
        };

        match invert_label {
            InvertLabel::A => {
                return (swap_direction_a.invert(), swap_direction_b);
            }
            InvertLabel::B => {
                return (swap_direction_a, swap_direction_b.invert());
            }
            InvertLabel::None => {
                return (swap_direction_a, swap_direction_b);
            }
        }
        
    }

    

    pub fn invert(&self) -> Self {
        match self {
            Self::XtoY => Self::YtoX,
            Self::YtoX => Self::XtoY,
            Self::Unknown => Self::Unknown,
        }
    }
}
pub struct Router {
    pub routes: Vec<ArbitrageRoute>,
}

impl Router {
    pub fn new_routes_from_price_movement(
        pool_impacted: &PoolImpacted,
        simulator_engine: &SimulatorEngine,
        delete_pool_sender: &Sender<Vec<String>>,
        initialize_sender: &Sender<PoolInitialize>,
    ) -> Result<Vec<ArbitrageRoute>, Box<dyn Error>> {

        let mut mint_routes: HashMap<String, (ArbitrageRoute, f64)> = HashMap::new();
        let write_number = pool_impacted.write_number;
        for moving_pool in pool_impacted.iter_pools() {
            if moving_pool.get_routing_step() == RoutingStep::Intermediate {
                continue;
            }
            let (price_a, inverted_price_a, fee_a) = moving_pool.get_price_and_fee_with_inverted();
            let farm_token = moving_pool.get_farm_token_mint().unwrap();

            let mint_y = moving_pool.get_mint_y();
            let quote_is_mint_a = mint_y != *farm_token;
            let mint_token = if quote_is_mint_a {
                mint_y
            } else {
                moving_pool.get_mint_x()
            };

            let mut best_candidates = Vec::new();


            match Router::explore_path(
                moving_pool.clone(),
                simulator_engine,
                vec![(moving_pool.clone(), SwapDirection::Unknown, None)],
                &mut best_candidates,
                initialize_sender,
                write_number,
                price_a,
                inverted_price_a,
                fee_a,
                quote_is_mint_a,
                &mint_token,
                &farm_token,
                0,
                delete_pool_sender,
            ) {
                Ok(()) => {
                    let mut best_route = None;
                    let mut best_profit_per_cu = 0.0;
                    for mut route in best_candidates.into_iter() {
                        let max_amount_in = route.simulation_result.max_amount_in;
                        match Router::handle_route(&mut route, FULL_SIMULATION_MAX_ITER, max_amount_in) {
                            Ok(_) => {
                                let route_profit = route.simulation_result.profit_per_cu;
                                if route_profit > best_profit_per_cu {
                                    best_profit_per_cu = route_profit;
                                    best_route = Some(route);
                                }
                            }
                            Err(_) => {
                                continue;
                            }
                        }
                    }

                    if let Some(route) = best_route {
                        if let Some((_existing_route, existing_profit)) = mint_routes.get(&mint_token) {
                            if best_profit_per_cu > *existing_profit {
                                mint_routes.insert(mint_token.clone(), (route, best_profit_per_cu));
                            }
                        } else {
                            mint_routes.insert(mint_token.clone(), (route, best_profit_per_cu));
                        }
                    }
                }
                Err(_) => {
                    continue;
                }
            }
        }

        Ok(mint_routes.into_iter().map(|(_, (route, _))| route).collect())
    }

    fn explore_path(
        moving_pool: Arc<dyn Pool>,
        simulator_engine: &SimulatorEngine,
        current_route: Vec<(Arc<dyn Pool>, SwapDirection, Option<Vec<String>>)>,
        best_candidates: &mut Vec<ArbitrageRoute>,
        initialize_sender: &Sender<PoolInitialize>,
        write_number: u64,
        price_a: u128,
        inverted_price_a: u128,
        fee_a: u128,
        quote_is_mint_a: bool,
        mint_token: &String,
        farm_token: &String,
        step_count: u8,
        delete_pool_sender: &Sender<Vec<String>>,
    ) -> Result<(), Box<dyn Error>> {
        if price_a == 0 {
            return Ok(());
        }
        let mint_pools = simulator_engine.get_related_mint_pools(mint_token);
        if let Some(mint_pools) = mint_pools {
            let pools = &mint_pools.read().unwrap().pools.pools;
            for pool_b_entry in pools {
                let pool_b = pool_b_entry.pool.clone();
                let quote_is_mint_b = pool_b_entry.quote_is_mint;
                let (price_b, inverted_price_b, fee_b) = pool_b.get_prices_and_fee_with_init_check(
                    write_number,
                    initialize_sender,
                    
                );

                if pool_b.get_address() == moving_pool.get_address() || price_b == 0 {
                    continue;
                }

                match pool_b.get_routing_step() {
                    RoutingStep::Extremity => {
                        if !pool_b.is_farm_token(farm_token) {
                            continue;
                        }

                        
                        if let Some(swap_directions) = 
                            Router::compare_pools(
                                price_a,
                                inverted_price_a,
                                fee_a,
                                price_b,
                                inverted_price_b,
                                fee_b,
                                quote_is_mint_a,
                                quote_is_mint_b
                            ) {
                            

                            let mut route = current_route.clone();
                            route.push((pool_b.clone(), swap_directions.1, None));
                            let final_arb_route = match ArbitrageRoute::new(
                                route, 
                                farm_token, 
                                price_a, 
                                price_b, 
                                inverted_price_a, 
                                inverted_price_b,
                                quote_is_mint_a,
                                quote_is_mint_b
                            ) {
                                Ok(route) => route,
                                Err(PoolError::NotEnoughLiquidity) => continue,
                                Err(PoolError::InvalidRoute) => continue,
                                Err(PoolError::NeedReinitialize(errors)) => {
                                    delete_pool_sender.send(errors)?;
                                    continue;
                                }
                                Err(_e) => continue,
                            };

                            // Insert route into best_candidates, maintaining sort by profit_per_cu
                            let profit_per_cu = final_arb_route.simulation_result.profit_per_cu;
                            let insert_pos = best_candidates.iter()
                                .position(|route| route.simulation_result.profit_per_cu < profit_per_cu)
                                .unwrap_or(best_candidates.len());

                            if insert_pos < 3 {
                                best_candidates.insert(insert_pos, final_arb_route);
                                if best_candidates.len() > 3 {
                                    best_candidates.pop();
                                }
                            }
                        }
                        return Ok(());
                    }
                    RoutingStep::Intermediate => {
                        if step_count + 1 > MAX_STEP_COUNT {
                            return Ok(());
                        }
                        let (intermediate_price, quote_is_mint_c) = Router::get_intermediate_price(
                            price_a, 
                            price_b, 
                            quote_is_mint_a, 
                            quote_is_mint_b
                        );

                        let intermediate_fee = combine_fee_bps(fee_a, fee_b);
                        let mut next_current_route = current_route.clone();
                        next_current_route.push((pool_b.clone(), SwapDirection::Unknown, None));
                        let complementary_mint = pool_b.get_complementary_mint(mint_token);
                        Router::explore_path(
                            pool_b.clone(),
                            simulator_engine,
                            next_current_route,
                            best_candidates,
                            initialize_sender,
                            write_number,
                            intermediate_price, 
                            intermediate_price.inv_price()?,
                            intermediate_fee,
                            quote_is_mint_c, 
                            &complementary_mint, 
                            farm_token,
                            step_count + 1,
                            delete_pool_sender
                        )?;
                        return Ok(());
                        
                    }
                }
            }
        }
        return Ok(());
    }

    pub fn handle_route(
        route: &mut ArbitrageRoute,
        iters: u128,
        max_amount: f64,
    ) -> Result<(), PoolError> {
        // Generate data for plotting
        let step_size = 40_000.0;
        let max_plot_amount = max_amount*2.0;
        let mut plot_data = Vec::new();
        
        let mut current_amount = 0.0;
        if iters == 20 && route.route.len() == 2 && false{
            while current_amount <= max_plot_amount {
                match route.simulate_route(current_amount) {
                    Ok((profit_per_cu, profit)) => {
                        plot_data.push((current_amount, profit));
                    },
                    Err(_) => {
                        // If simulation fails, stop collecting data
                        break;
                    }
                }
                current_amount += step_size;
            }
            
            // Save plot data to file
            if !plot_data.is_empty() {
                use std::fs::File;
                use std::io::Write;
                
                if let Ok(mut file) = File::create("arbitrage_plot_data.csv") {
                    // Write header
                    let _ = writeln!(file, "amount_in,profit");
                    
                    // Write data points
                    for (amount, profit) in plot_data {
                        let _ = writeln!(file, "{},{}", amount, profit);
                    }
                }
            }

            std::thread::sleep(std::time::Duration::from_nanos(1000));
        }
        
        
        // Continue with original optimization logic
        let optimal_amount = 
            match optimize_arbitrage_profit(route.clone(), 0.0, max_amount as f64, iters) {
                Ok(amount) => amount,
                Err(_) => return Err(PoolError::NotEnoughLiquidity),
        };
        
        let (profit_per_cu, profit, cu) = if iters == PARTIAL_SIMULATION_MAX_ITER {
            let (profit_per_cu, profit) = route.simulate_route(optimal_amount)?;
            (profit_per_cu, profit, 0_u32)
        } else {
            route.simulate_route_extra_infos(optimal_amount)?
        };
        if profit < 0.0 {
            return Err(PoolError::NotEnoughProfit);
        }
        let simulation_result = &mut route.simulation_result;
        simulation_result.profit_per_cu = profit_per_cu;
        simulation_result.max_amount_in = max_amount;
        simulation_result.profit = profit as i64;
        simulation_result.sol_in = optimal_amount as u64;
        simulation_result.sol_out = optimal_amount as u64 + profit as u64;
        simulation_result.cu = cu;
        Ok(())
    }

    fn compare_pools(
        price_a: u128,
        inverted_price_a: u128,
        fee_a: u128,
        price_b: u128, 
        inverted_price_b: u128,
        fee_b: u128, 
        quote_is_mint_a: bool, 
        quote_is_mint_b: bool
    ) -> Option<(SwapDirection, SwapDirection)> {
        let swap_directions_result = if quote_is_mint_a == quote_is_mint_b {
            Router::compare_same_quotes(price_a, fee_a, price_b, fee_b)
        } else {
            let (price_a, price_b, invert_label) = if quote_is_mint_a {
                (price_a, inverted_price_b, InvertLabel::B)
            } else {
                (inverted_price_a, price_b, InvertLabel::A)
            };
            Router::compare_opposite_quotes(
                price_a,
                fee_a, 
                price_b, 
                fee_b,
                invert_label
            )
        };

        swap_directions_result
    }

    #[inline]
    fn compare_same_quotes(
        price_a: u128,
        fee_a: u128,
        price_b: u128,
        fee_b: u128,
    ) -> Option<(SwapDirection, SwapDirection)> {
        
        let (is_diff, a_is_cheaper) = price_differ(price_a, price_b, fee_a, fee_b);

        if is_diff {
            Some(SwapDirection::from_price_difference( a_is_cheaper, InvertLabel::None))
        } else {
            None
        }
    }

    #[inline]
    fn compare_opposite_quotes(
        price_a: u128,
        fee_a: u128,
        price_b: u128,
        fee_b: u128,
        invert_label: InvertLabel
    ) -> Option<(SwapDirection, SwapDirection)> {

        let (is_diff, a_is_cheaper) = price_differ(price_a, price_b, fee_a, fee_b);
        if is_diff {
            Some(SwapDirection::from_price_difference(a_is_cheaper, invert_label))
        } else {
            None
        }
    }

    fn compare_opposite_prices(
        price_a: u128,
        price_b: u128,
        fee_a: u128,
        fee_b: u128,
    ) -> (bool, bool) {
        let a_is_cheaper = {
            price_a.checked_mul(price_b).is_some()
        };

        let is_diff = if a_is_cheaper {
            price_a.apply_fee(fee_a, true).checked_mul(price_b.apply_fee(fee_b, false)).is_some()
        } else {
            price_a.apply_fee(fee_a, false).checked_mul(price_b.apply_fee(fee_b, true)).is_none()
        };

        (is_diff, a_is_cheaper)
    }

    fn get_intermediate_price(
        price_a: u128,
        price_b: u128,
        a_quote_is_mint: bool,
        b_quote_is_mint: bool,
    ) -> (u128, bool) {
        match (a_quote_is_mint, b_quote_is_mint) {
            (true, true) => (a_lshift_64_div_b(price_a, price_b), true),
            (false, false) => (a_lshift_64_div_b(price_b, price_a), true),
            (true, false) => (a_times_b_rshift_64(price_a, price_b), true),
            (false, true) => (a_times_b_rshift_64(price_a, price_b), false),
        }
    }
        

    pub fn try_route_from_file(
        pools_map: &Arc<RwLock<HashMap<String, Arc<dyn Pool>>>>,
    ) -> Result<(), Box<dyn Error>> {
        use std::fs::File;
        use std::io::{BufRead, BufReader};
        
        let file = match File::open("/home/thsmg/solana-arb-client/arbitrage/route.txt") {
            Ok(file) => file,
            Err(e) => {
                println!("Failed to open route.txt: {}", e);
                return Ok(());
            }
        };
        
        let reader = BufReader::new(file);
        let mut lines = reader.lines();
        
        // Read amount_in from the first line
        let amount_in = match lines.next() {
            Some(Ok(line)) => match line.trim().parse::<f64>() {
                Ok(amount) => amount,
                Err(e) => {
                    println!("Failed to parse amount_in: {}", e);
                    return Ok(());
                }
            },
            _ => {
                let err = std::io::Error::new(std::io::ErrorKind::InvalidData, "Missing amount_in in route.txt");
                return Ok(());
            }
        };
        
        // Build the route from the remaining lines
        let mut route = Vec::new();
        let pools_map_guard = pools_map.read().unwrap();
        
        for line in lines {
            if let Ok(line) = line {
                let parts: Vec<&str> = line.split(';').collect();
                if parts.len() != 2 {
                    continue;
                }
                
                let pool_address = parts[0].trim();
                let direction_value = match parts[1].trim().parse::<u8>() {
                    Ok(val) => val,
                    Err(_) => continue,
                };
                
                let swap_direction = match direction_value {
                    1 => SwapDirection::XtoY,
                    0 => SwapDirection::YtoX,
                    _ => continue,
                };
                
                if let Some(pool) = pools_map_guard.get(pool_address) {
                    if pool.get_pool_type() == "Meteora-Dlmm" {
                        let meteora_pool = pool.as_meteora_dlmm().unwrap();
                        if meteora_pool.writable_data.read().unwrap().liquidity_state.is_none() {
                            println!("Pool not initialized: {}", pool_address);
                            return Ok(());
                        }
                    }
                    route.push((pool.clone(), swap_direction, None));
                } else {
                    println!("Pool not found: {}", pool_address);
                    continue;
                }
            }
        }
        
        if route.is_empty() {
            let err = std::io::Error::new(std::io::ErrorKind::InvalidData, "No valid pools found in route.txt");
            return Ok(());
        }
        
        // Create the arbitrage route
        let mut arb_route = ArbitrageRoute {
            route,
            revert_route: false,
            simulation_result: SimulationResult::new(),
        };
        
        // Simulate the route
        if let Err(e) = arb_route.simulate_route(amount_in) {
            println!("Failed to simulate route: {:?}", e);
            return Ok(());
        }

        let real_out_amount = arb_route.simulation_result.sol_out;
        let real_profit = arb_route.simulation_result.profit;
        let real_profit_per_cu = arb_route.simulation_result.profit_per_cu;
        
        // Handle the route
        if let Err(e) = Self::handle_route(&mut arb_route, FULL_SIMULATION_MAX_ITER, amount_in) {
            println!("Failed to handle route: {:?}", e);
            return Ok(());
        }
        
        // Print results
        println!("Route from file simulation results:");
        println!("Input amount from 55N: {}", amount_in);
        println!("Output amount from 55N: {}", real_out_amount);
        println!("Profit from 55N: {}", real_profit);
        println!("Profit per CU from 55N: {}", real_profit_per_cu);
        println!("\nBest amount in: {}", arb_route.simulation_result.sol_in);
        println!("Best amount out: {}", arb_route.simulation_result.sol_out);
        println!("Profit: {}", arb_route.simulation_result.profit) ;
        println!("Profit per CU: {}", arb_route.simulation_result.profit_per_cu);
        
        Ok(())
    }
    


}


#[derive(Clone)]
pub struct SimulationResult{
    pub sol_in: u64,
    pub sol_out: u64,
    pub profit: i64,
    pub profit_per_cu: f64,
    pub max_amount_in: f64,
    pub cu: u32,
}

impl SimulationResult {
    pub fn new() -> Self {
        Self { sol_in: 0, sol_out: 0, profit_per_cu: 0.0, profit: 0, max_amount_in: 0.0, cu: 0 }
    }

    pub fn new_with_values(sol_in: u64, sol_out: u64, profit: i64, profit_per_cu: f64, max_amount_in: f64, cu: u32) -> Self {
        Self { sol_in, sol_out, profit, profit_per_cu, max_amount_in, cu }
    }

    pub fn replace_if_better(&mut self, in_amount: u64, out_amount: u64, profit_per_cu: f64) {
        if profit_per_cu < self.profit_per_cu {
            self.sol_in = in_amount;
            self.sol_out = out_amount;
            self.profit_per_cu = profit_per_cu;
        }
    }
}

pub enum ArbitrageType {
    PammDlmm,
    MultipleHop,
    Uncovered,
}

#[derive(Clone)]
pub struct ArbitrageRoute {
    pub route: Vec<(Arc<dyn Pool>, SwapDirection, Option<Vec<String>>)>,
    revert_route: bool,
    pub simulation_result: SimulationResult,
}

impl ArbitrageRoute {

    pub fn new_empty() -> Self {
        let route = Vec::with_capacity(2);
        Self { route, revert_route: false, simulation_result: SimulationResult::new() }
    }


    pub fn new(
        mut route: Vec<(Arc<dyn Pool>, SwapDirection, Option<Vec<String>>)>, 
        farmable_token: &String,
        price_a: u128,
        price_b: u128,
        inverted_price_a: u128,
        inverted_price_b: u128,
        quote_is_mint_a: bool,
        quote_is_mint_b: bool,
    ) -> Result<Self, PoolError> {
        let last_step = route.last().ok_or(PoolError::InvalidRoute)?;
        //let last_pool = route.last().ok_or("Route is empty")?;

        let (revert_route, mut target_mint) = {
            let mint_x = last_step.0.get_mint_x();
            if farmable_token == &mint_x {
                if last_step.1 == SwapDirection::XtoY {
                    (true, last_step.0.get_mint_y())
                } else {
                    (false, last_step.0.get_mint_y())
                }
            } else {
                if last_step.1 == SwapDirection::XtoY {
                    (false, mint_x)
                } else {
                    (true, mint_x)
                }
            }
        };

        let last_step_price = if quote_is_mint_a != quote_is_mint_b {
                inverted_price_a
        } else {
                price_a
        };

        ArbitrageRoute::propagate_swap_direction(&mut route, revert_route, &mut target_mint);


        let mut arbitrage_route = ArbitrageRoute { route, revert_route, simulation_result: SimulationResult::new() };
        
        let max_amount = arbitrage_route.find_max_amount_in(last_step_price)? as f64;
        Router::handle_route(&mut arbitrage_route, PARTIAL_SIMULATION_MAX_ITER, max_amount)?;
        Ok(arbitrage_route)
    }

    pub fn propagate_swap_direction(
        route: &mut Vec<(Arc<dyn Pool>, SwapDirection, Option<Vec<String>>)>,
        revert_route: bool,
        last_step_target_mint: &mut String,
    ) {

        if revert_route {
            for (pool, direction, _) in route.iter_mut().rev().skip(1) {
                let (swap_direction, mint_out) = 
                    pool.get_swap_direction_for_target_in(last_step_target_mint);
                *direction = swap_direction;
                *last_step_target_mint = mint_out;
            }  
        } else {
            for (pool, direction, _) in route.iter_mut().rev().skip(1) {
                let (swap_direction, mint_out) = 
                    pool.get_swap_direction_for_target_out(last_step_target_mint);
                *direction = swap_direction;
                *last_step_target_mint = mint_out;
            }
        }
    }

    pub fn find_max_amount_in(&self, last_step_price: u128) -> Result<u128, PoolError> {
        let last_step = self.route.last().unwrap();

        let current_amount_in = if self.revert_route {
            last_step.0.get_amount_in_for_target_price(last_step_price, last_step.1.clone())?
        } else {
            let mut current_amount_in = last_step.0.get_amount_in_for_target_price(last_step_price, last_step.1.clone())?;
        

            for (pool, direction, _ ) in self.iter_from_end().skip(1) {
                
                current_amount_in = pool.get_amount_in(current_amount_in, direction.clone())?;
                if current_amount_in == 0 {
                    return Err(PoolError::NotEnoughLiquidity);
                }
            }
            current_amount_in
        };
        
        Ok(current_amount_in)
    }

    pub fn simulate_route_extra_infos(&mut self, amount_in: f64) -> Result<(f64, f64, u32), PoolError> {
        let mut current_amount_in = amount_in;
        let mut amount_out = 0.0;
        let mut cu = 0.0;

        for (pool, direction, extra_pubkeys) in self.iter_mut_from_start() {
            let (amount_out_step, cu_out, extra_pubkeys_step) = 
                pool.get_amount_out_with_cu_extra_infos(current_amount_in, direction.clone())?;
            cu += cu_out;
            current_amount_in = amount_out_step;
            amount_out = amount_out_step;
            *extra_pubkeys = extra_pubkeys_step;
        }
        
        let profit = amount_out - amount_in;
        let profit_per_cu = profit / cu;
        Ok((profit_per_cu, profit, cu as u32))
    }

    pub fn simulate_route(&self, amount_in: f64) -> Result<(f64, f64), PoolError> {
        let mut current_amount_in = amount_in;
        let mut amount_out = 0.0;
        let mut cu = 0.0;
        
        for (pool, direction, _) in self.iter_from_start() {
            let (amount_out_step, cu_out) = pool.get_amount_out_with_cu(current_amount_in, direction.clone())?;
            cu += cu_out;
            current_amount_in = amount_out_step;
            amount_out = amount_out_step;
        }
        

        let profit = amount_out - amount_in;
        let profit_per_cu = profit / cu;
        Ok((profit_per_cu, profit))
    }

    pub fn get_arbitrage_instruction(
        &self, amount_in: u64, 
        expected_profit: u64, 
        wallet: &Wallet,
        slot: u64,
    ) -> Result<(Instruction, HashSet<Arc<TokenAccount>>), WalletError> {

        let mut accounts_meta = Vec::new();
        let mut data = Vec::with_capacity(16 + 2 * self.route.len());
        let token_accounts = &wallet.token_accounts.read().unwrap().token_accounts;
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&1_000_u64.to_le_bytes());
        data.push(self.route.len() as u8);
        let mut seen_token_accounts = HashSet::new();

        let max_amount_in = self.get_max_amount_in_with_balance(token_accounts);

        
        if amount_in > max_amount_in {
            return Err(WalletError::NotEnoughBalance);
        }

        if expected_profit < MIN_PROFIT {
            return Err(WalletError::NotEnoughProfit);
        }


        for (pool, direction, extra_pubkeys) in self.iter_from_start() {
            if !pool.can_send(slot, amount_in) {
                return Err(WalletError::PoolRecentlySent);
            }

            let mint_x = pool.get_mint_x();
            let mint_y = pool.get_mint_y();
            let ata_x = match token_accounts.get(&mint_x) {
                Some(ata) => ata,
                None => return Err(WalletError::NotEnoughBalance),
            };
            let ata_y = match token_accounts.get(&mint_y) {
                Some(ata) => ata,
                None => return Err(WalletError::NotEnoughBalance),
            };
                        
            match pool.get_farm_token_label() {
                TokenLabel::X => {
                    if !ata_y.is_created.load(Ordering::Relaxed) {
                        seen_token_accounts.insert(ata_y.clone());
                    }
                },
                TokenLabel::Y => {
                    if !ata_x.is_created.load(Ordering::Relaxed) {
                        seen_token_accounts.insert(ata_x.clone());
                    }
                },
                TokenLabel::None => {
                    if !ata_x.is_created.load(Ordering::Relaxed) {
                        seen_token_accounts.insert(ata_x.clone());
                    } 
                    if !ata_y.is_created.load(Ordering::Relaxed) {
                        seen_token_accounts.insert(ata_y.clone());
                    }
                }
            }

            let (ata_in, mint_in, ata_out, mint_out) = if *direction == SwapDirection::XtoY {
                (ata_x, mint_x, ata_y, mint_y)
            } else {
                (ata_y, mint_y, ata_x, mint_x)
            };
                
            pool.append_swap_step(
                wallet.keypair.pubkey(),
                &mut accounts_meta, 
                &mut data, 
                ata_in.address.clone(), 
                &mint_in,
                ata_out.address.clone(), 
                &mint_out,
                extra_pubkeys,
            );
        }

        for (pool, _, _) in self.iter_from_start() {
            pool.store_last_sent_slot(slot);
            pool.store_last_amount_in(amount_in);
        }

        Ok((
            Instruction {
                program_id: PoolConstants::ARBITRAGE_PROGRAM,
                accounts: accounts_meta,
                data,
            },
            seen_token_accounts
        ))
    }

    pub fn get_max_amount_in_with_balance(&self, token_accounts: &HashMap<String, Arc<TokenAccount>>) -> u64 {
        let (first_pool, direction, _) = self.first_step_in_path();
        let mint_in = if *direction == SwapDirection::XtoY {
            first_pool.get_mint_x()
        } else {
            first_pool.get_mint_y()
        };
        token_accounts.get(&mint_in).unwrap().amount.load(Ordering::Relaxed)
    }

    pub fn first_step_in_path(&self) -> &(Arc<dyn Pool>, SwapDirection, Option<Vec<String>>) {
        if self.revert_route {
            self.route.last().unwrap()
        } else {
            self.route.first().unwrap()
        }
    }

    pub fn iter_from_start(&self) -> Box<dyn Iterator<Item = &(Arc<dyn Pool>, SwapDirection, Option<Vec<String>>)> + '_> {
        if self.revert_route {
            Box::new(self.route.iter().rev())
        } else {
            Box::new(self.route.iter())
        }
    }

    pub fn iter_from_end(&self) -> Box<dyn Iterator<Item = &(Arc<dyn Pool>, SwapDirection, Option<Vec<String>>)> + '_> {
        if !self.revert_route {  // Note the inverted condition here
            Box::new(self.route.iter().rev())
        } else {
            Box::new(self.route.iter())
        }
    }

    pub fn iter_mut_from_start(&mut self) -> Box<dyn Iterator<Item = &mut (Arc<dyn Pool>, SwapDirection, Option<Vec<String>>)> + '_> {
        if self.revert_route {
            Box::new(self.route.iter_mut().rev())
        } else {
            Box::new(self.route.iter_mut())
        }
    }

    pub fn iter_mut_from_end(&mut self) -> Box<dyn Iterator<Item = &mut (Arc<dyn Pool>, SwapDirection, Option<Vec<String>>)> + '_> {
        if !self.revert_route {  // Note the inverted condition here
            Box::new(self.route.iter_mut().rev())
        } else {
            Box::new(self.route.iter_mut())
        }
    }

    pub fn get_min_congestion_rates(&self) -> (f32, u64, u8) {
        let mut min_congestion_rate = f32::MAX;
        let mut min_congestion_slot = 0;
        let mut min_num_congestion_last_slot = 0;
        for (pool, _, _) in self.iter_from_start() {
            let (congestion_rate, last_congestion_slot, num_congestion_last_slot) = pool.get_congestion_rate();
            if congestion_rate < min_congestion_rate {
                min_congestion_rate = congestion_rate;
                min_congestion_slot = last_congestion_slot;
                min_num_congestion_last_slot = num_congestion_last_slot;
            }
        }
        (min_congestion_rate, min_congestion_slot, min_num_congestion_last_slot)
    }

    pub fn get_arbitrage_type(&self) -> (ArbSpamPoolType, bool) {
        if self.route.len() != 2 {
            return (ArbSpamPoolType::Uncovered, false);
        }
        let pool_type_a = self.route[0].0.get_pool_type();
        let pool_type_b = self.route[1].0.get_pool_type();
        if pool_type_a == "Meteora-Dlmm" && pool_type_b == "Pumpfun-Kp" {
            (ArbSpamPoolType::DlmmPamm, false)
        } else if pool_type_a == "Pumpfun-Kp" && pool_type_b == "Meteora-Dlmm" {
            (ArbSpamPoolType::DlmmPamm, true)
        } else {
            (ArbSpamPoolType::Uncovered, false)
        }
    }
}

pub struct SwapStepInstruction{
    in_idx:u8,
    out_idx:u8,
    accounts_meta: Vec<AccountMeta>,
    accounts_meta_len: u8,
}

impl SwapStepInstruction {
    pub fn append_to_data(&self, data: &mut Vec<u8>) {
        data.push(self.accounts_meta_len);
        data.push(self.in_idx);
        data.push(self.out_idx);
    }
}

