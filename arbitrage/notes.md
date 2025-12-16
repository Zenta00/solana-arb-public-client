TODO: 
Leader TTI (Time To Insert) (modify the streamer to detect our failed tx)
Fix CustomBlockHashQueue
Detect sandwich

Priority : 
    Signature cost : 720 + ( SECP(6690) | ED25519(2400) * num sig)
    Write_Lock_cost: 300 * num_write_locks



pub struct UsageCostDetails<'a, Tx> {
    pub transaction: &'a Tx,
    pub signature_cost: u64,
    pub write_lock_cost: u64,
    pub data_bytes_cost: u64,
    pub programs_execution_cost: u64,
    pub loaded_accounts_data_size_cost: u64,
    pub allocated_accounts_data_size: u64,
}

pub fn sum(&self) -> u64 {
    self.signature_cost
        .saturating_add(self.write_lock_cost)
        .saturating_add(self.data_bytes_cost)
        .saturating_add(self.programs_execution_cost)
        .saturating_add(self.loaded_accounts_data_size_cost)
