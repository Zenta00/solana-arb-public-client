use {
    crate::{
        pools::spam_pools::PoolPair,
        wallets::Wallet,
    },
    solana_sdk::{
        pubkey::Pubkey,
        instruction::{
            AccountMeta,
        },
    },
    std::sync::Arc,
    tokio::sync::RwLock as TokioRwLock,
    crate::wallets::TokenAccounts,
};
pub const METEORA_DLMM_PROGRAM_ACCOUNT: Pubkey = Pubkey::from_str_const("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo");
pub const TOKEN_PROGRAM_ACCOUNT: Pubkey = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const PUMPFUN_AMM_ID: Pubkey = Pubkey::from_str_const("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA");
pub const GLOBAL_CONFIG: Pubkey = Pubkey::from_str_const("ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw");
pub const PROTOCOL_FEE_RECIPIENT: Pubkey = Pubkey::from_str_const("62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV");
pub const PROTOCOL_FEE_RECIPIENT_TOKEN_ACCOUNT: Pubkey = Pubkey::from_str_const("94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb");
pub const SYSTEM_PROGRAM_ACCOUNT: Pubkey = Pubkey::from_str_const("11111111111111111111111111111111");
pub const ASSOCIATED_TOKEN_PROGRAM_ACCOUNT: Pubkey = Pubkey::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
pub const EVENT_AUTHORITY: Pubkey = Pubkey::from_str_const("GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR");
pub const METEORA_EVENT_AUTHORITY: Pubkey = Pubkey::from_str_const("D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6");

impl PoolPair{
    pub async fn get_dlmm_pamm_spam_arb_ix_data(
        &self,
        shared_token_accounts: &Arc<TokioRwLock<TokenAccounts>>,
        for_init_alt: bool,
    ) -> (Vec<AccountMeta>, bool){
        let mut accounts_meta = Vec::with_capacity(12);
        let dlmm_pool = self.pool_a.as_meteora_dlmm().unwrap();
        let pamm_pool = self.pool_b.as_pumpfun_amm().unwrap();

        let mint_token = self.pool_b.get_mint_x();
        let user_ata_mint = shared_token_accounts.read().await.get_token_account_and_state(&mint_token).unwrap().0;
        
        accounts_meta.push(AccountMeta::new_readonly(Pubkey::from_str_const(&mint_token), false));
        accounts_meta.push(AccountMeta::new(Pubkey::from_str_const(&user_ata_mint), false));

        let (dlmm_accounts_meta, inverse) = dlmm_pool.get_spam_arb_accounts(for_init_alt);
        let pamm_accounts_meta = pamm_pool.get_spam_arb_accounts();

        accounts_meta.extend(dlmm_accounts_meta);
        accounts_meta.extend(pamm_accounts_meta);

        (accounts_meta, inverse)
    }
}


pub struct DlmmPammGlobalAccounts {
    pub signer: AccountMeta,
    pub farm_token_account: AccountMeta,
    pub token_farm: AccountMeta,
    pub token_program: AccountMeta,
    pub system_program: AccountMeta,
    pub associated_token_program: AccountMeta,
    pub meteora_dlmm_program: AccountMeta,
    pub meteora_event_authority: AccountMeta,
    pub pamm_program: AccountMeta,
    pub pamm_global_config: AccountMeta,
    pub protocol_fee_recipient: AccountMeta,
    pub token_fee_recipient: AccountMeta,
    pub pamm_event_authority: AccountMeta,
}

impl DlmmPammGlobalAccounts{
    pub fn new(
        signer: AccountMeta,
        farm_token_account: AccountMeta,
        token_farm: AccountMeta,
    ) -> Self{
        
        Self{
            signer,
            farm_token_account,
            token_farm,
            token_program: AccountMeta::new_readonly(TOKEN_PROGRAM_ACCOUNT, false),
            system_program: AccountMeta::new_readonly(SYSTEM_PROGRAM_ACCOUNT, false),
            associated_token_program: AccountMeta::new_readonly(ASSOCIATED_TOKEN_PROGRAM_ACCOUNT, false),
            meteora_dlmm_program: AccountMeta::new_readonly(METEORA_DLMM_PROGRAM_ACCOUNT, false),
            meteora_event_authority: AccountMeta::new_readonly(METEORA_EVENT_AUTHORITY, false),
            pamm_program: AccountMeta::new_readonly(PUMPFUN_AMM_ID, false),
            pamm_global_config: AccountMeta::new_readonly(GLOBAL_CONFIG, false),
            protocol_fee_recipient: AccountMeta::new(PROTOCOL_FEE_RECIPIENT, false),
            token_fee_recipient: AccountMeta::new(PROTOCOL_FEE_RECIPIENT_TOKEN_ACCOUNT, false),
            pamm_event_authority: AccountMeta::new_readonly(EVENT_AUTHORITY, false),
        }
    }

    pub fn update_mutable_accounts(&mut self, signer: AccountMeta, farm_token_account: AccountMeta, token_farm: AccountMeta){
        self.signer = signer;
        self.farm_token_account = farm_token_account;
        self.token_farm = token_farm;
    }

    pub fn get_accounts_meta(&self) -> Vec<AccountMeta>{
        vec![
            self.signer.clone(),
            self.farm_token_account.clone(),
            self.token_farm.clone(),
            self.token_program.clone(),
            self.system_program.clone(),
            self.associated_token_program.clone(),
            self.meteora_dlmm_program.clone(),
            self.meteora_event_authority.clone(),
            self.pamm_program.clone(),
            self.pamm_global_config.clone(),
            self.protocol_fee_recipient.clone(),
            self.token_fee_recipient.clone(),
            self.pamm_event_authority.clone(),
        ]
    }
}
