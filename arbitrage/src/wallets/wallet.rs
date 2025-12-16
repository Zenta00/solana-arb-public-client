use {
    crate::{
        simulation::router::ArbitrageRoute,
        pools::PoolConstants,
        sender::transaction_sender::SendEndpoint,
    },
    std::collections::HashMap,
    solana_sdk::{
        compute_budget::ComputeBudgetInstruction,
        pubkey::Pubkey,
        signature::Keypair,
        transaction::VersionedTransaction,
        message::{v0, VersionedMessage},
        address_lookup_table::AddressLookupTableAccount,
        hash::Hash as BlockHash,
        system_instruction,
        commitment_config::CommitmentConfig,
        instruction::Instruction,
        signature::Signer,
        account::Account,
    },
    spl_associated_token_account::get_associated_token_address,
    solana_client::nonblocking::rpc_client::RpcClient as AsyncRpcClient,
    std::sync::atomic::{AtomicU64, AtomicBool, Ordering},
    std::sync::Arc,
    std::sync::RwLock,
    std::collections::HashSet,
    std::error::Error,
    serde::{Serialize, Deserialize},
    spl_associated_token_account::instruction::create_associated_token_account,
    rand,
};

const SIMULATION_BUDGET: u32 = 400_000;
const CREATE_BUDGET: u32 = 27_000;
pub struct TokenAccount {
    pub mint: String,
    pub address: String,
    pub amount: AtomicU64,
    pub is_created: AtomicBool,
}

impl PartialEq for TokenAccount {
    fn eq(&self, other: &Self) -> bool {
        self.address == other.address
    }
}

impl Eq for TokenAccount {}

impl std::hash::Hash for TokenAccount {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.address.hash(state);
    }
}

#[derive(Clone)]
pub struct TokenAccounts {
    pub token_accounts: HashMap<String, Arc<TokenAccount>>,
}

impl TokenAccounts {
    pub fn add_token_account(
        &mut self, 
        mint: String, 
        address: String, 
        amount: u64,
        is_created: bool
    ) {
        self.token_accounts.insert(
            mint.clone(), 
            Arc::new(TokenAccount { 
                mint,
                address, 
                amount: AtomicU64::new(amount), 
                is_created: AtomicBool::new(is_created) 
            })
        );
    }

    pub fn get_token_account_and_state(&self, mint: &String) -> Option<(String, bool)> {
        if let Some(token_account) = self.token_accounts.get(mint) {
            Some((token_account.address.clone(), token_account.is_created.load(Ordering::Relaxed)))
        } else {
            None
        }
    }

    pub fn set_true_token_accounts_state(&self, mints: &Vec<String>) {
        for mint in mints {
            if let Some(token_account) = self.token_accounts.get(mint) {
                token_account.is_created.store(true, Ordering::Relaxed);
            }
        }
    }
}

pub struct Wallet{
    pub keypair: Keypair,
    pub sol_balance: AtomicU64,
    pub token_accounts: RwLock<TokenAccounts>,
    pub address_lookup_table_account: Arc<Vec<AddressLookupTableAccount>>,
}


impl Wallet{

    pub fn new_empty(
        private_key: String,
    ) -> Self {
        let keypair = Keypair::from_base58_string(&private_key);
        Self {
            keypair,
            sol_balance: AtomicU64::new(0),
            token_accounts: RwLock::new(TokenAccounts { token_accounts: HashMap::new() }),
            address_lookup_table_account: Arc::new(vec![])
        }
    }
    pub fn new(keypair: Keypair, sol_balance: u64, farmable_tokens: &Vec<String>, address_lookup_table_account: Arc<Vec<AddressLookupTableAccount>>) -> Self {
        let mut token_accounts = HashMap::new();
        for mint in farmable_tokens {
            token_accounts.insert(mint.clone(), Arc::new(TokenAccount {
                mint: mint.clone(),
                address: get_associated_token_address(
                    &keypair.pubkey(), 
                    &Pubkey::from_str_const(mint)
                ).to_string(),
                amount: AtomicU64::new(0),  
                is_created: AtomicBool::new(false)
            }));
        }
        Self {
            keypair, 
            sol_balance: AtomicU64::new(sol_balance), 
            token_accounts: RwLock::new(TokenAccounts { token_accounts }), 
            address_lookup_table_account 
        }
    }

    

    pub fn set_true_token_accounts_state(&self, mints: &Vec<String>) {
        let token_accounts = self.token_accounts.read().unwrap();
        for mint in mints {
            if let Some(token_account) = token_accounts.token_accounts.get(mint) {
                token_account.is_created.store(true, Ordering::Relaxed);
            }
        }
    }

    pub fn get_token_account_and_state(&self, mint: &String) -> Option<(String, bool)> {
        let token_accounts = self.token_accounts.read().unwrap();
        if let Some(token_account) = token_accounts.token_accounts.get(mint) {
            Some((token_account.address.clone(), token_account.is_created.load(Ordering::Relaxed)))
        } else {
            None
        }
    }

    pub fn get_token_account_amount(&self, mint: &String) -> u64 {
        let token_accounts = self.token_accounts.read().unwrap();
        token_accounts.token_accounts.get(mint).unwrap().amount.load(Ordering::Relaxed)
    }

    pub fn create_unsigned_arb_transaction(
        &self, 
        route: &ArbitrageRoute,
        blockhash: BlockHash,
        slot: u64,
    ) -> Result<(Instruction, Vec<Instruction>,VersionedTransaction, HashSet<Arc<TokenAccount>>), WalletError> {
        
        let simulation_result = &route.simulation_result;
        let amount_in = simulation_result.sol_in;
        let expected_profit = simulation_result.profit as u64;

        let (arbitrage_instruction, token_accounts_to_create) = 
            route.get_arbitrage_instruction(amount_in, expected_profit, &self, slot)?;

        let mut create_instructions = Vec::with_capacity(token_accounts_to_create.len());
        for token_account in &token_accounts_to_create {
            create_instructions.push(create_associated_token_account(
                &self.keypair.pubkey(),
                &self.keypair.pubkey(),
                &Pubkey::from_str_const(&token_account.mint),
                &PoolConstants::TOKEN_PROGRAM_ID,
            ));
        }

        let mut instructions = if !create_instructions.is_empty() {
            let mut instructions = create_instructions.clone();
            instructions.push(arbitrage_instruction.clone());
            instructions
        } else {
            vec![arbitrage_instruction.clone()]
        };


        instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(SIMULATION_BUDGET));
        let message = VersionedMessage::V0(v0::Message::try_compile(
            &self.keypair.pubkey(),
            &instructions,
            &self.address_lookup_table_account,
            blockhash,
        ).unwrap());
        let unsigned_tx = VersionedTransaction::try_new(message, &[&self.keypair]).unwrap();

        Ok((arbitrage_instruction, create_instructions, unsigned_tx, token_accounts_to_create))
    }

    pub fn create_signed_arb_transaction(
        &self,
        arbitrage_instruction: &Instruction,
        create_instructions: &Vec<Instruction>,
        send_endpoint: &SendEndpoint,
        blockhash: BlockHash,
        budget: u32,
        profit: u64,
        pseudo_random_index: u8,
    ) -> Result<(VersionedTransaction, u64), WalletError> {
        let tip_amount = send_endpoint.get_tip_amount(profit);
        
        let mut instructions = Vec::with_capacity(3);
        let mut arbitrage_instruction_clone = arbitrage_instruction.clone();
        
        match send_endpoint {
            SendEndpoint::Jito => {
                let final_budget = budget + 1_000;
                let budget_instruction = ComputeBudgetInstruction::set_compute_unit_limit(final_budget);
                /*let jito_tip_instruction = 
                    get_pre_transfer_tip_instruction(&self.keypair, tip_amount, pseudo_random_index);
                */
                arbitrage_instruction_clone.data[8..16].copy_from_slice(&tip_amount.to_le_bytes());
                //instructions.push(jito_tip_instruction);
                instructions.push(budget_instruction);
                for instruction in create_instructions {
                    instructions.push(instruction.clone());
                }
                instructions.push(arbitrage_instruction_clone);
            }
            SendEndpoint::Native(_) => {
                let budget_instruction = ComputeBudgetInstruction::set_compute_unit_limit(budget);
                let tip_instruction = get_fee_instruction_with_profit(tip_amount, budget);
                instructions.push(tip_instruction);
                instructions.push(budget_instruction);
                for instruction in create_instructions {
                    instructions.push(instruction.clone());
                }
                arbitrage_instruction_clone.data[8..16].copy_from_slice(&1_000_u64.to_le_bytes());
                instructions.push(arbitrage_instruction_clone);
            }
        }

        let tx = create_tx_with_address_table_lookup(
            &instructions, 
            &self.address_lookup_table_account, 
            &self.keypair, 
            &self.keypair,
            blockhash
        ).unwrap();
        Ok((tx, tip_amount))
    }

    pub fn refresh_balances(
        &self, 
        mints: &Vec<String>,
        accounts_info: &Vec<Option<Account>>
    ) {
    
        let sol_account = accounts_info.first().unwrap().as_ref().unwrap();
        self.sol_balance.store(sol_account.lamports, Ordering::Relaxed);
        let token_accounts = self.token_accounts.read().unwrap();
        for (i, account_info) in accounts_info.iter().skip(1).enumerate() {
            if let Some(account) = account_info {
                if let Some(token_account) = token_accounts.token_accounts.get(&mints[i]) {
                    if !account.data.is_empty() && account.data.len() >= 72 {
                        let amount_bytes: [u8; 8] = account.data[64..72].try_into().unwrap();
                        let amount = u64::from_le_bytes(amount_bytes);
                        token_account.amount.store(amount, Ordering::Relaxed);
                    }
                }
            }
        }
    }

    pub fn sign_tx_with_alt(
        &self, 
        instructions: &[Instruction],
        address_lookup_table_accounts: &[AddressLookupTableAccount],
        blockhash: BlockHash,
    ) -> Result<VersionedTransaction, Box<dyn Error>> {
        let tx = create_tx_with_address_table_lookup(
            instructions, 
            address_lookup_table_accounts,
            &self.keypair, 
            &self.keypair,
            blockhash
        )?;
        Ok(tx)
    }

    pub fn create_and_sign_jito_tip_tx(
        &self,
        tip_amount: u64,
        pseudo_random_index: u8,
        blockhash: BlockHash,
    ) -> Result<VersionedTransaction, Box<dyn Error>> {
        let tip_instruction = get_pre_transfer_tip_instruction(&self.keypair, tip_amount, pseudo_random_index);
        let budget_instruction = ComputeBudgetInstruction::set_compute_unit_limit(450);
        let instructions = vec![tip_instruction, budget_instruction];
        let tx = create_tx_with_address_table_lookup(&instructions, &[], &self.keypair, &self.keypair, blockhash)?;
        Ok(tx)
    }
    
}

pub fn create_tx_with_address_table_lookup(
    instructions: &[Instruction],
    address_lookup_table_account: &[AddressLookupTableAccount],
    signer: &Keypair,
    payer: &Keypair,
    blockhash: BlockHash,
) -> Result<VersionedTransaction, Box<dyn Error>> {
    let keypairs = if signer != payer {
        vec![signer, payer]
    } else {
        vec![payer]
    };
    let tx = VersionedTransaction::try_new(
        VersionedMessage::V0(v0::Message::try_compile(
            &payer.pubkey(),
            instructions,
            address_lookup_table_account,
            blockhash,
        )?),
        &keypairs,
    )?;
    Ok(tx)
}

#[derive(Debug, Clone)]
pub enum WalletError {
    NotEnoughBalance,
    PoolRecentlySent,
    NotEnoughProfit,
}

pub fn get_pre_transfer_tip_instruction_with_signer(signer: &Pubkey, tip_amount: u64) -> Instruction {
    let pseudo_random_index = rand::random::<u8>() % 8;
    let transfer_instruction = system_instruction::transfer(
        signer,
        &get_random_jito_tip_account(pseudo_random_index),
        tip_amount,
    );
    transfer_instruction
}

pub fn get_pre_transfer_tip_instruction(wallet: &Keypair, tip_amount: u64, pseudo_random_index: u8) -> Instruction {
    let transfer_instruction = system_instruction::transfer(
        &wallet.pubkey(),
        &get_random_jito_tip_account(pseudo_random_index),
        tip_amount,
    );
    transfer_instruction
}

const JITO_TIP_ACCOUNTS: [Pubkey; 8] = [
    Pubkey::from_str_const("96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5"),
    Pubkey::from_str_const("HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe"),
    Pubkey::from_str_const("Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY"),
    Pubkey::from_str_const("ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49"),
    Pubkey::from_str_const("DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh"),
    Pubkey::from_str_const("ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt"),
    Pubkey::from_str_const("DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL"),
    Pubkey::from_str_const("3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT"),
];

pub fn get_random_jito_tip_account(pseudo_random_index: u8) -> Pubkey {
    let random_index = pseudo_random_index % JITO_TIP_ACCOUNTS.len() as u8;
    JITO_TIP_ACCOUNTS[random_index as usize]
}

pub fn get_fee_instruction_with_profit(
    tip_amount: u64,
    compute_unit_limit: u32,
) -> Instruction{	
    let compute_unit_price = {
        let total_fee_in_micro_lamports = 
                tip_amount * PoolConstants::MICRO_LAMPORTS_PER_LAMPORT;
        let fee_per_compute_unit = total_fee_in_micro_lamports / (compute_unit_limit as u64);
        fee_per_compute_unit
    };

    ComputeBudgetInstruction::set_compute_unit_price(compute_unit_price)
}