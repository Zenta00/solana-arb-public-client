use {
    crate::{
        wallets::wallet::TokenAccount,
        pools::Pool,
        sender::transaction_sender::{
            Opportunity,
            SendEndpoint,
        },
    },
    solana_sdk::signature::Signature,
    std::sync::{
        Arc,
        RwLock,
        atomic::AtomicBool,
    },
    rand::{Rng, thread_rng},
    std::collections::HashSet,
};

pub const VALID_SLOT_DURATION: u64 = 25;
pub const DEFAULT_JITO_FIXED_TIP: u64 = 85;
pub const NON_JITO_MAX_TIP_AMOUNT: u64 = 1_800_000;
pub const NON_JITO_FIXED_TIP: u64 = 7;
pub const MIN_PROFIT_TARGET: i64 = 50_000;

pub struct FeeTracker {
    pub signature: Signature,
    pub pools: Vec<Arc<dyn Pool>>,
    pub send_endpoint: SendEndpoint,
    pub is_valid_jito_tx: Option<Arc<AtomicBool>>,
    pub ata_states: HashSet<Arc<TokenAccount>>,
    pub last_valid_slot: u64,
}

impl FeeTracker {
    pub fn new_from_opportunity(
        signature: Signature,
        opportunity: &Opportunity,
        pool_vec: Vec<Arc<dyn Pool>>,
        slot: u64,
    ) -> Self {
        Self {
            signature,
            pools: pool_vec,
            send_endpoint: opportunity.send_endpoint.clone(),
            is_valid_jito_tx: opportunity.is_valid_jito_tx.clone(),
            ata_states: opportunity.ata_states.clone(),
            last_valid_slot: slot + VALID_SLOT_DURATION,
        }
    }
}

impl SendEndpoint {
    pub fn get_tipping_strategy(&self) -> TippingStrategy {
        match self {
            SendEndpoint::Jito => TippingStrategy::Fixed(DEFAULT_JITO_FIXED_TIP),
            SendEndpoint::Native(tipping_strategy) => tipping_strategy.clone(),
            _ => TippingStrategy::Fixed(1),
        }
    }

    pub fn get_tip_amount(&self, expected_profit: u64) -> u64 {
        self.get_tipping_strategy().get_tip_amount(expected_profit)
    }
}



#[derive(Clone, Hash, Eq, PartialEq, Debug)]
pub enum TippingStrategy {
    Random(u64, u64),  // (min_percentage, max_percentage)
    Fixed(u64),        // fixed_percentage
    FixedWithMax(u64, u64), // fixed_percentage, max_amount
    None,
}

impl TippingStrategy {
    pub fn get_tip_amount(&self, expected_profit: u64) -> u64 {
        match self {
            TippingStrategy::Random(min_percentage, max_percentage) => {
                let percentage = thread_rng()
                    .gen_range(*min_percentage..=*max_percentage);
                (expected_profit * percentage) / 100
            }
            TippingStrategy::Fixed(percentage) => {
                (expected_profit * percentage) / 100
            }
            TippingStrategy::FixedWithMax(percentage, max_amount) => {
                let tip_amount = (expected_profit * percentage) / 100;
                if tip_amount > *max_amount {
                    *max_amount
                } else {
                    tip_amount
                }
            }
            TippingStrategy::None => 0,
        }
    }
}
