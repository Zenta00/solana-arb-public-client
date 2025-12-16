use {
    crate::wallets::Wallet, solana_client::nonblocking::rpc_client::RpcClient as AsyncRpcClient, solana_sdk::{
        pubkey::Pubkey, signature::{
            Keypair,
            Signer,
        }
    }, spl_associated_token_account::{
        get_associated_token_address,
        instruction::create_associated_token_account,
    }, std::sync::{
        atomic::{AtomicU64, Ordering}, Arc, RwLock
    }, 
    solana_sdk::address_lookup_table::{state::AddressLookupTable, AddressLookupTableAccount},
    solana_client::rpc_client::RpcClient,
    solana_sdk::commitment_config::CommitmentConfig,
};

const RPC_URL: &str = "http://localhost:8899";

pub const ADDRESS_LOOKUP_TABLE_KEY: Pubkey = Pubkey::from_str_const("8zFmxUnBiw3JUEZkn8WpPqznEHLHeu4nbKozzHkHXrz3");

#[derive(Clone)]
pub struct WalletsPool {
    pub wallets: Vec<Arc<Wallet>>,
}

impl WalletsPool {
    pub fn new_from_private_keys(private_keys: Vec<String>, farmable_tokens: &Vec<String>) -> Self {
        let confirmed_client = Arc::new(RpcClient::new_with_commitment(RPC_URL.to_string(), CommitmentConfig::confirmed()));
        let raw_account = confirmed_client.get_account(&ADDRESS_LOOKUP_TABLE_KEY).unwrap();
        let address_lookup_table = AddressLookupTable::deserialize(&raw_account.data).unwrap();
        let address_lookup_table_account = AddressLookupTableAccount {
            key: ADDRESS_LOOKUP_TABLE_KEY,
            addresses: address_lookup_table.addresses.to_vec(),
        };
        let address_lookup_table_account = Arc::new(vec![address_lookup_table_account]);
        let wallets = private_keys.into_iter().map(
            |private_key| {
                let keypair = Keypair::from_base58_string(&private_key);
                Arc::new(Wallet::new(keypair, 0, farmable_tokens, address_lookup_table_account.clone()))
            }
        ).collect();
        Self { wallets }
    }

    pub fn get_wallet_from_counter(&self, counter: u8) -> Arc<Wallet> {
        self.wallets[counter as usize % self.wallets.len()].clone()
    }

    pub fn get_wallet(&self, index: usize) -> Arc<Wallet> {
        self.wallets[index].clone()
    }

    pub async fn initalize_all_with_mints(
        &self, 
        connection: Arc<AsyncRpcClient>,
        
        mints: &Vec<String>
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for wallet in &self.wallets {
            // Create ata_accounts without holding the lock across await
            let ata_accounts = {
                let pubkey = wallet.keypair.pubkey();
                mints.iter().map(
                    |mint| get_associated_token_address(
                    &pubkey,
                    &Pubkey::from_str_const(mint)
                    )
                ).collect::<Vec<Pubkey>>()
            }; // wallet_r is dropped here
            
            // Process accounts in chunks of 20
            let chunk_size = 20;
            let mut all_accounts_info = Vec::with_capacity(ata_accounts.len());
            
            for chunk in ata_accounts.chunks(chunk_size) {
                // Make the async call for each chunk
                let chunk_accounts_info = connection.get_multiple_accounts(chunk).await?;
                all_accounts_info.extend(chunk_accounts_info);
                
                // Small delay to avoid rate limiting
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            
            let mut token_accounts = wallet.token_accounts.write().unwrap();
            for (index, account_info) in all_accounts_info.iter().enumerate() {
                let mint = &mints[index];
                if let Some(token_account) = token_accounts.token_accounts.get(mint) {
                    // Mint already exists in token_accounts
                    if account_info.is_some() {
                        token_account.is_created.store(true, Ordering::Relaxed);
                    } else {
                        token_account.is_created.store(false, Ordering::Relaxed);
                    }
                } else {
                    // Mint doesn't exist in token_accounts, need to insert it
                    let address = ata_accounts[index].to_string();
                    let amount = 0;
                    let is_created = account_info.is_some();
                    
                    token_accounts.add_token_account(
                        mint.clone(),
                        address,
                        amount,
                        is_created
                    );
                }
            }
        }
        Ok(())
    }

    pub async fn refresh_wallets(
        &self, 
        connection: &AsyncRpcClient, 
        mints: &Vec<String>
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for wallet in &self.wallets {
            // Collect all necessary data before the await
            let (_wallet_pubkey, mint_pubkeys) = {
                let wallet_pubkey = wallet.keypair.pubkey();
                
                // Create a vector of all pubkeys we need to query
                let mut pubkeys = Vec::with_capacity(mints.len() + 1);
                pubkeys.push(wallet_pubkey);
                
                // Add all mint pubkeys
                for mint in mints {
                    pubkeys.push(
                        get_associated_token_address(
                            &wallet_pubkey,
                            &Pubkey::from_str_const(mint)
                        )
                    );
                }
                
                (wallet_pubkey, pubkeys)
            }; // wallet_r is dropped here
            
            // Now make the async call without holding any locks
            let accounts_info = connection.get_multiple_accounts(&mint_pubkeys).await?;
            
            // After the await, acquire the lock again to update the wallet
            wallet.refresh_balances(mints, &accounts_info);
        }
        Ok(())
    }

}

