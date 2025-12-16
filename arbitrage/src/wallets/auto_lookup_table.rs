use {
    std::sync::Arc,
    solana_client::nonblocking::rpc_client::RpcClient as AsyncRpcClient,
    solana_sdk::{
        address_lookup_table::{state::AddressLookupTable, AddressLookupTableAccount},
        pubkey::Pubkey,
        instruction::Instruction,
        address_lookup_table::instruction::{
            create_lookup_table,
            extend_lookup_table,
        },
        signature::{
            Keypair,
            Signer,
        },
    },
    std::collections::HashSet,
    crate::pools::spam_pools::PoolPair,
};

#[derive(PartialEq, Clone)]
pub enum LookupTableChange{
    Create,
    Extend,
    NoChange,
}
pub struct AutoLookupTables {
    pub lookup_tables: Vec<AutoLookupTable>,
}

#[derive(Debug)]
pub struct AutoLookupTable {
    pub contained_pubkeys: HashSet<Pubkey>,
    pub lookup_table: [AddressLookupTableAccount; 1],
}

impl AutoLookupTable {
    pub fn new(lookup_table: AddressLookupTableAccount) -> Self {
        Self {
            contained_pubkeys: lookup_table.addresses.iter().cloned().collect(),
            lookup_table: [lookup_table],
        }
    }

    pub fn extend_or_create_ix(
        &self, 
        unique_addresses: HashSet<Pubkey>,
        recent_slot: u64,
        wallet: &Keypair,
    ) -> (LookupTableChange, Vec<Instruction>, Vec<Pubkey>, Pubkey) {
        let mut instructions = Vec::new();
        
        // Get addresses not in contained_pubkeys
        let new_addresses: Vec<Pubkey> = unique_addresses
            .iter()
            .filter(|addr| !self.contained_pubkeys.contains(addr))
            .cloned()
            .collect();
        
        if new_addresses.is_empty() {
            return (LookupTableChange::NoChange, instructions, new_addresses, self.lookup_table[0].key);
        }
        
        // Check if we need to create a new lookup table
        let current_size = self.lookup_table[0].addresses.len();
        let new_size = current_size + new_addresses.len();
        
        let (lookup_table_change, lookup_table_address) = if new_size > 256 {
            
            // Create lookup table instruction
            let (create_ix, lookup_table_address) = create_lookup_table(
                wallet.pubkey(),
                wallet.pubkey(),
                recent_slot,
            );
            
            instructions.push(create_ix);
            
            // Extend with remaining addresses in chunks of 15
            for chunk in new_addresses.chunks(15) {
                let extend_ix = extend_lookup_table(
                    lookup_table_address,
                    wallet.pubkey(),
                    Some(wallet.pubkey()),
                    chunk.to_vec(),
                );
                instructions.push(extend_ix);
            }
            (LookupTableChange::Create, lookup_table_address)
        } else {
            // Extend with addresses in chunks of 15
            for chunk in new_addresses.chunks(15) {
                let extend_ix = solana_sdk::address_lookup_table::instruction::extend_lookup_table(
                    self.lookup_table[0].key,
                    wallet.pubkey(),
                    Some(wallet.pubkey()),
                    chunk.to_vec(),
                );
                instructions.push(extend_ix);
            }
            (LookupTableChange::Extend, self.lookup_table[0].key)
        };
        
        (lookup_table_change, instructions, new_addresses, lookup_table_address)
    }

    pub async fn update_and_check_lookup_table(&mut self, client: Arc<AsyncRpcClient>, new_keys: Vec<Pubkey>, lookup_table_address: Pubkey) -> bool {
        self.lookup_table = [get_lookup_table_account(client.clone(), lookup_table_address).await];
        let new_contained_keys: HashSet<Pubkey> = self.lookup_table[0].addresses.iter().cloned().collect();

        for key in new_keys {
            if !new_contained_keys.contains(&key) {
                return false;
            }
        }
        true
    }
}

pub async fn get_lookup_table_account(client: Arc<AsyncRpcClient>, lookup_table_key: Pubkey) -> AddressLookupTableAccount {
    let raw_account = client.get_account(&lookup_table_key).await.unwrap();
    let address_lookup_table = AddressLookupTable::deserialize(&raw_account.data).unwrap();
    let address_lookup_table_account = AddressLookupTableAccount {
        key: lookup_table_key,
        addresses: address_lookup_table.addresses.to_vec(),
    };
    address_lookup_table_account
}