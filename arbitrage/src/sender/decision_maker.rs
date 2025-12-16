use {
    crate::{fees::fee_tracker::TippingStrategy, spam::spammer::chose_dummy_fee_amount}, futures::stream::once, std::{collections::HashSet, sync::{Arc, RwLock}, time::{Duration, Instant}}
};

const AVERAGE_SLOT_DURATION: Duration = Duration::from_millis(400);
const INSERTION_TRESHOLD: Duration = Duration::from_millis(60);
const MAX_RTT_LATENCY: Duration = Duration::from_millis(120);

const VERY_CLOSE_CONNECTION_RTT_THRESHOLD: Duration = Duration::from_millis(5);
const CLOSE_CONNECTION_RTT_THRESHOLD: Duration = Duration::from_millis(10);
const MID_CONNECTION_RTT_THRESHOLD: Duration = Duration::from_millis(15);
const MID_HIGH_CONNECTION_RTT_THRESHOLD: Duration = Duration::from_millis(30);
const HIGH_CONNECTION_RTT_THRESHOLD: Duration = Duration::from_millis(150);

pub const NON_JITO_HIGH_TIP_MAX_TIP_AMOUNT: u64 = 1_500_000;
pub const NON_JITO_HIGH_TIP: u64 = 2;

pub const NON_JITO_LOW_TIP: u64 = 2;
pub const NON_JITO_LOW_TIP_MAX_TIP_AMOUNT: u64 = 550_000;

pub const CONGESTION_RATE_THRESHOLD: f32 = 3.0;

#[derive(Clone)]
pub struct LeaderBlacklist {
    blacklist: HashSet<String>,
}

impl LeaderBlacklist {
    pub fn new() -> Self {
        Self { blacklist: HashSet::new() }
    }

    pub fn add_leader(&mut self, leader: String) {
        self.blacklist.insert(leader);
    }
    
    pub fn is_blacklisted(&self, leader: &String) -> bool {
        self.blacklist.contains(leader)
    }
}
pub enum EndpointDecision {
    QueueJito,
    Native,
    NativeNextLeader,
    Both,
    Discard,
}

#[derive(Clone)]
pub struct DecisionMaker {
    blacklist: Arc<RwLock<LeaderBlacklist>>,
}

impl DecisionMaker {
    pub fn new(blacklist: Arc<RwLock<LeaderBlacklist>>) -> Self {
        Self { blacklist }
    }

    pub fn make_decision(
        &self,
        leader_id: &String,
        profit: u64,
        is_jito: bool,
        connection_rtt: Duration,
        is_node_synced: bool,
        delta_last_congestion_slot: u64,
        avg_recent_congestion_rate: f32,
    ) -> (EndpointDecision, Duration, TippingStrategy) {

        return (EndpointDecision::Discard, connection_rtt, TippingStrategy::Fixed(chose_dummy_fee_amount(10, 90)));
        if self.is_blacklisted(leader_id) || !is_node_synced  {
            return (EndpointDecision::Discard, connection_rtt, TippingStrategy::None);
        };

        if delta_last_congestion_slot >= 2 || avg_recent_congestion_rate <= CONGESTION_RATE_THRESHOLD {

            return (EndpointDecision::Native, connection_rtt, TippingStrategy::FixedWithMax(2, 450_000));

            if connection_rtt <= MID_CONNECTION_RTT_THRESHOLD && profit > 2_000_000{
                return (EndpointDecision::Native, connection_rtt, TippingStrategy::FixedWithMax(4, 3_000_000));
            }

            /*if connection_rtt <= VERY_CLOSE_CONNECTION_RTT_THRESHOLD {
                return (EndpointDecision::Native, connection_rtt, TippingStrategy::FixedWithMax(4, 4_350_000));
            }
            else if connection_rtt <= CLOSE_CONNECTION_RTT_THRESHOLD {
                return (EndpointDecision::Native, connection_rtt, TippingStrategy::FixedWithMax(3, 2_175_000));
            }
            else if connection_rtt <= MID_CONNECTION_RTT_THRESHOLD {
                return (EndpointDecision::Native, connection_rtt, TippingStrategy::FixedWithMax(3, 1_000_000));
            }
            else if connection_rtt <= MID_HIGH_CONNECTION_RTT_THRESHOLD {
                return (EndpointDecision::Native, connection_rtt, TippingStrategy::FixedWithMax(3, 750_000));
            }
            else if connection_rtt <= HIGH_CONNECTION_RTT_THRESHOLD {
                return (EndpointDecision::Native, connection_rtt, TippingStrategy::FixedWithMax(2, 450_000));
            }*/
        }
        return (EndpointDecision::Discard, connection_rtt, TippingStrategy::None);
    }

    fn make_insert_decision(
        connection_rtt: Duration,
        highest_slot_time: Instant,
    ) -> (EndpointDecision, Duration) {
        let connection_latency = connection_rtt / 2;
        let start_slot_time = highest_slot_time - connection_latency;
        let estimated_detection_time = Instant::now() - start_slot_time;


        if estimated_detection_time <= AVERAGE_SLOT_DURATION / 2 {
            if connection_latency <= INSERTION_TRESHOLD {
                return (EndpointDecision::Native, estimated_detection_time);
            }
            else {
                return (EndpointDecision::QueueJito, estimated_detection_time);
            }
        }

        if estimated_detection_time <= AVERAGE_SLOT_DURATION {
            if connection_latency <= INSERTION_TRESHOLD {
                return (EndpointDecision::Both, estimated_detection_time);
            }
            else {
                return (EndpointDecision::QueueJito, estimated_detection_time);
            }
        }

        (EndpointDecision::QueueJito, estimated_detection_time)
    }

    fn is_blacklisted(&self, leader_id: &String) -> bool {
        self.blacklist.read().unwrap().is_blacklisted(leader_id)
    }

}