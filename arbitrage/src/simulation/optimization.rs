use {
    argmin::solver::brent::BrentOpt,
    argmin::core::{
        Problem, Solver, IterState, CostFunction, Error as ArgminError, State
    },
    std::error::Error,
    crate::simulation::router::{ArbitrageRoute, SimulationResult}
};

pub const MAX_BRENT_ITER: u128 = 100;

impl CostFunction for ArbitrageRoute {
    type Param = f64;
    type Output = f64;

    fn cost(&self, amount_in: &Self::Param) -> Result<Self::Output, ArgminError> {
        // Simulate the route with this input amount
        let (profit_per_cu, _profit) = self.simulate_route(*amount_in).unwrap_or((0.0, 0.0));
        
        Ok(-profit_per_cu)
    }
}

pub trait OptimizeBrent {
    fn optimize_arbitrage_profit(&self, min_amount: f64, max_amount: f64) -> Result<f64, Box<dyn std::error::Error>>;
}

pub fn optimize_arbitrage_profit(route: ArbitrageRoute, min_amount: f64, max_amount: f64, max_iter: u128) -> Result<f64, Box<dyn std::error::Error>> {
    let mut problem = Problem::new(route);
    
    let mut solver = BrentOpt::new(min_amount, max_amount);
    let state = IterState::new();
    
    // Initialize the solver
    let (mut state, _) = solver.init(&mut problem, state)
        .map_err(|e| Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
    
    // Run iterations until convergence or max iterations
    for _ in 0..max_iter {
        let (new_state, _) = solver.next_iter(&mut problem, state)
            .map_err(|e| Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        state = new_state;
        
        if state.terminated() {
            break;
        }
    }
    
    // Get the optimal input amount
    let optimal_amount = state.param.unwrap();

    Ok(optimal_amount)
}
