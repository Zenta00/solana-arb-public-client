pub mod dlmm_pamm;
use {
    std::sync::Arc,
    crate::pools::Pool,
    std::hash::{Hash, Hasher},
};

#[derive(Clone, Hash, Eq, PartialEq, Debug)]
pub enum ArbSpamPoolType{
    DlmmPamm,
    Uncovered,
}

#[derive(Clone, Debug)]
pub struct PoolPair {
    pub pool_a: Arc<dyn Pool>,
    pub pool_b: Arc<dyn Pool>,
    pub pool_type: ArbSpamPoolType,
}


impl Hash for PoolPair {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.pool_a.get_address().hash(state);
        self.pool_b.get_address().hash(state);
    }
}

impl PartialEq for PoolPair {
    fn eq(&self, other: &Self) -> bool {
        self.pool_a.get_address() == other.pool_a.get_address() &&
        self.pool_b.get_address() == other.pool_b.get_address()
    }
}

impl Eq for PoolPair {}

impl PoolPair{
    pub fn new(pool_a: Arc<dyn Pool>, pool_b: Arc<dyn Pool>, pool_type: ArbSpamPoolType) -> Self{
        Self{pool_a, pool_b, pool_type}
    }
}