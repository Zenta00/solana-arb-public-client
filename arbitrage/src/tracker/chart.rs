use {
    std::fs::{File, create_dir_all},
    std::path::Path,
    std::io::Write,

    std::collections::HashMap,
    std::time::{
        SystemTime,
        UNIX_EPOCH,
    },
};

pub const chart_file_path: &str = "/home/thsmg/solana-arb-client/arbitrage/tracker/charts/";
pub enum SwapDirection {
    Buy,
    Sell,
}

pub struct WhaleSwap {
    pub sol_amount: u128,
    pub token_amount: u128,
    pub avg_mc: u128,
    pub post_sol_reserve: u128,
    pub post_token_reserve: u128,
    pub is_buy: bool,
    pub timestamp: u64,
    pub slot: u64,
    pub whale_address: String,
}

impl WhaleSwap {
    pub fn new(
        sol_amount: u128, 
        token_amount: u128, 
        post_sol_reserve: u128,
        post_token_reserve: u128, 
        is_buy: bool, 
        slot: u64,
        whale_address: String
    ) -> Self {
        let avg_mc = sol_amount * 155_000_000 / token_amount;
        Self {
            sol_amount,
            token_amount,
            avg_mc,
            post_sol_reserve,
            post_token_reserve,
            is_buy,
            timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
            slot,
            whale_address,
        }
    }
}
pub struct Chart {
    pub token_mint: String,
    pub is_tracked: bool,
    pub swaps: Vec<WhaleSwap>,
}
impl Chart {
    pub fn new(token_mint: String, is_tracked: bool) -> Self {
        Self {
            token_mint,
            swaps: vec![],
            is_tracked,
        }
    }   
    pub fn add_swap(&mut self, swap: WhaleSwap, maker_is_whale: bool) {
        self.swaps.push(swap);
        if maker_is_whale {
            self.is_tracked = true;
        }
    }

    pub fn save_and_clear(&mut self) {
        

        let file_path = format!("{}{}.csv", chart_file_path, self.token_mint);
        let path = Path::new(&file_path);
        
        let file_exists = path.exists();
        let mut file = File::options()
            .write(true)
            .create(true)
            .append(file_exists)
            .open(path)
            .unwrap();
            
        // Write headers if file is new
        if !file_exists {
            writeln!(file, "timestamp,slot,sol_amount,token_amount,avg_mc,post_sol_reserve,post_token_reserve,is_buy,whale_address").unwrap();
        }
        
        // Write each swap as a CSV row
        for swap in &self.swaps {
            writeln!(
                file,
                "{},{},{},{},{},{},{},{},{}",
                swap.timestamp,
                swap.slot,
                swap.sol_amount,
                swap.token_amount,
                swap.avg_mc,
                swap.post_sol_reserve,
                swap.post_token_reserve,
                swap.is_buy,
                swap.whale_address
            ).unwrap();
        }
        
        self.swaps.clear();
    }
}
pub struct Charts {
    pub charts: HashMap<String, Chart>,
}

impl Charts {
    pub fn new() -> Self {
        Self {
            charts: HashMap::new(),
        }
    }

    pub fn add_swap(&mut self, token_mint: String, swap: WhaleSwap, maker_is_whale: bool) {
        self.charts.entry(token_mint.clone()).or_insert(Chart::new(token_mint, maker_is_whale)).add_swap(swap, maker_is_whale);
    }

    pub fn save_and_clear(&mut self) {
        let mut to_remove = Vec::new();
        
        for chart in self.charts.values_mut() {
            if chart.is_tracked {
                chart.save_and_clear();
            } else {
                to_remove.push(chart.token_mint.clone());
            }
        }
        
        for token_mint in to_remove {
            self.charts.remove(&token_mint);
        }
    }
}