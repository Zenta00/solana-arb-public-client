use {
    std::collections::{HashMap, VecDeque, BTreeMap},
    std::cmp::Reverse,
    crate::pools::spam_pools::PoolPair,
};


// Represents a single arbitrage opportunity
#[derive(Debug)]
struct ArbitrageEvent {
    slot: u64,
    profit: u64,  // Optional: store profit for more detailed analysis
}

// Main structure to track arbitrage rankings
pub struct ArbitrageRankingTracker {
    // Maps pool pairs to their queue of arbitrage events
    pool_pairs: HashMap<PoolPair, VecDeque<ArbitrageEvent>>,
    // Cache of current counts for quick ranking
    current_counts: BTreeMap<Reverse<usize>, Vec<PoolPair>>,
    // Current slot for cleanup
    current_slot: u64,
    // How many slots of history to maintain
    history_slots: u64,
}

impl ArbitrageRankingTracker {
    pub fn new(history_slots: u64) -> Self {
        Self {
            pool_pairs: HashMap::new(),
            current_counts: BTreeMap::new(),
            current_slot: 0,
            history_slots,
        }
    }

    pub fn record_arbitrage(
        &mut self, 
        pool_pair: PoolPair,
        slot: u64, 
        profit: u64
    ) {
        self.current_slot = slot;
        

        // Remove old count from current_counts
        if let Some(events) = self.pool_pairs.get(&pool_pair) {
            let old_count = events.len();
            self.remove_from_current_counts(&pool_pair, old_count);
        }

        // Add new event
        let events = self.pool_pairs.entry(pool_pair.clone()).or_insert_with(|| VecDeque::new());
        events.push_back(ArbitrageEvent { slot, profit });

        // Cleanup old events
        while let Some(front) = events.front() {
            if front.slot + self.history_slots < slot {
                events.pop_front();
            } else {
                break;
            }
        }

        // Update current_counts with new count
        let new_count = events.len();
        self.add_to_current_counts(pool_pair, new_count);
    }

    fn remove_from_current_counts(&mut self, pool_pair: &PoolPair, count: usize) {
        if let Some(pairs) = self.current_counts.get_mut(&Reverse(count)) {
            pairs.retain(|p| p != pool_pair);
            if pairs.is_empty() {
                self.current_counts.remove(&Reverse(count));
            }
        }
    }

    fn add_to_current_counts(&mut self, pool_pair: PoolPair, count: usize) {
        self.current_counts
            .entry(Reverse(count))
            .or_insert_with(Vec::new)
            .push(pool_pair);
    }

    // Get top N pairs by arbitrage count
    pub fn get_top_pairs(&self, n: usize) -> Vec<PoolPair> {
        let mut result = Vec::new();
        for (Reverse(_count), pairs) in self.current_counts.iter() {
            for pool_pair in pairs {
                result.push(pool_pair.clone());
                if result.len() >= n {
                    return result;
                }
            }
        }
        result
    }

    // Cleanup old data for all pairs
    pub fn cleanup(&mut self) {
        let slot = self.current_slot;
        let cutoff = slot.saturating_sub(self.history_slots);
        
        // First collect pairs that need updating
        let mut updates = Vec::new();
        for (pool_pair, events) in self.pool_pairs.iter_mut() {
            let old_count = events.len();
            events.retain(|event| event.slot > cutoff);
            let new_count = events.len();
            
            if old_count != new_count {
                updates.push((pool_pair.clone(), old_count, new_count));
            }
        }

        // Then apply the updates
        for (pair, old_count, new_count) in updates {
            self.remove_from_current_counts(&pair, old_count);
            self.add_to_current_counts(pair, new_count);
        }
    }
}
