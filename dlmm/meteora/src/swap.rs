use solana_client::nonblocking::rpc_client::RpcClient;
use solana_program::instruction::AccountMeta;
use solana_sdk::pubkey::Pubkey;

use commons::quote::get_bin_array_pubkeys_for_swap;
use lb_clmm::state::bin_array_bitmap_extension::BinArrayBitmapExtension;
use lb_clmm::state::lb_pair::LbPair;
use lb_clmm::utils::pda::*;
use log::debug;
use std::sync::Arc;

pub const TOKEN_PROGRAM_ACCOUNT: Pubkey = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

#[derive(Debug)]
pub struct SwapAccountMetas{
    pub vec: Vec<AccountMeta>,
}

impl SwapAccountMetas{
    pub fn get_with_user_and_tokens(
        &self, 
        in_mint: String, 
        out_mint: String, 
        user_pubkey: Pubkey
    ) -> Vec<AccountMeta>{
        let mut vec = self.vec.clone();
        vec[4] = AccountMeta::new(Pubkey::from_str_const(&in_mint), false);
        vec[5] = AccountMeta::new(Pubkey::from_str_const(&out_mint), false);
        vec[9] = AccountMeta::new(user_pubkey, true);
        vec
    }
}


#[derive(Debug)]
pub struct MeteoraDlmmPoolInfo {
    pub keys: SwapAccountMetas,
    pub amm_flag: u8, // Assuming amm_flag is a u8, adjust if needed
    pub owner: String,
}

#[derive(Debug)]
pub struct SwapExactInParameters {
    pub lb_pair: Pubkey,
    pub amount_in: u64,
    pub swap_for_y: bool,
}

pub async fn get_bin_arrays_for_swap(
    connection: Arc<RpcClient>,
    lb_pair: &Pubkey,
    swap_for_y: bool,
) -> Result<Vec<(Pubkey, i32)>, Box<dyn std::error::Error + Send + Sync>> {
    let lb_pair_account = connection.get_account(lb_pair).await?;
    let lb_pair_state: LbPair = LbPair::try_deserialize(&lb_pair_account.data[8..])
        .map_err(|e| Box::<dyn std::error::Error + Send + Sync>::from(e.to_string()))?;
    let (bitmap_extension_key, _bump) = derive_bin_array_bitmap_extension(*lb_pair);
    debug!("Bitmap extension key: {:?}", bitmap_extension_key);
    let bitmap_extension_state = match connection.get_account(&bitmap_extension_key).await {
        Ok(account) => match BinArrayBitmapExtension::try_deserialize(&account.data[8..]) {
            Ok(state) => {
                debug!("Bitmap extension state: {:?}", state);
                Some(state)
            }
            Err(e) => {
                debug!("Failed to deserialize bitmap extension: {:?}", e);
                None
            }
        },
        Err(e) => {
            debug!("Failed to get bitmap extension account: {:?}", e);
            None
        }
    };
    let bin_arrays_for_swap = get_bin_array_pubkeys_for_swap(
        *lb_pair,
        &lb_pair_state,
        bitmap_extension_state.as_ref(),
        swap_for_y,
        3,
    )?;

    Ok(bin_arrays_for_swap)
}

pub async fn meteora_pool_info(
    connection: Arc<RpcClient>,
    lb_pair: &Pubkey,
) -> Result<(MeteoraDlmmPoolInfo, Vec<(Pubkey, i32)>, usize), Box<dyn std::error::Error>> {
    debug!("Getting meteora pool info for: {:?}", lb_pair);

    let lb_pair_account = connection.get_account(lb_pair).await?;
    let lb_pair_state: LbPair = LbPair::try_deserialize(&lb_pair_account.data[8..])?;
    let (bitmap_extension_key, _bump) = derive_bin_array_bitmap_extension(*lb_pair);
    debug!("Bitmap extension key: {:?}", bitmap_extension_key);
    let bitmap_extension_state = match connection.get_account(&bitmap_extension_key).await {
        Ok(account) => match BinArrayBitmapExtension::try_deserialize(&account.data[8..]) {
            Ok(state) => {
                debug!("Bitmap extension state: {:?}", state);
                Some(state)
            }
            Err(e) => {
                debug!("Failed to deserialize bitmap extension: {:?}", e);
                None
            }
        },
        Err(e) => {
            debug!("Failed to get bitmap extension account: {:?}", e);
            None
        }
    };
    let mut bin_arrays_for_swap = get_bin_array_pubkeys_for_swap(
        *lb_pair,
        &lb_pair_state,
        bitmap_extension_state.as_ref(),
        true,
        3,
    )?;

    //println!("bin_arrays_for_swap: {:?}", bin_arrays_for_swap);

    let bin_arrays_for_swap_opposite = get_bin_array_pubkeys_for_swap(
        *lb_pair,
        &lb_pair_state,
        bitmap_extension_state.as_ref(),
        false,
        3,
    )?;

    //println!("bin_arrays_for_swap_opposite: {:?}", bin_arrays_for_swap_opposite);

    // Check if arrays have enough elements before accessing
    if bin_arrays_for_swap.is_empty() || bin_arrays_for_swap_opposite.is_empty() {
        return Err("Swap arrays are empty".into());
    }

    // Check if first elements match (with proper unwrapping)
    let active_bin_pubkey = bin_arrays_for_swap.get(0).unwrap().0;
    if active_bin_pubkey != bin_arrays_for_swap_opposite.get(0).unwrap().0 {
        return Err("First elements of swap arrays don't match".into());
    }

    // Safely combine arrays in format (f,e,a,b,c)
    let mut combined = Vec::new();
    
    // Add reversed elements from opposite array (if they exist)
    if bin_arrays_for_swap.len() > 1 {
        let opposite_tail = &bin_arrays_for_swap[1..];
        combined.extend(opposite_tail.iter().rev().cloned());
    }
    
    // Add elements from the main array
    combined.extend(bin_arrays_for_swap_opposite.iter().cloned());

    let active_bin_container_index = 
        bin_arrays_for_swap.iter().position(|(pubkey, _)| pubkey == &active_bin_pubkey).unwrap();
    
    bin_arrays_for_swap = combined;
    debug!("Bin arrays for swap: {:?}", bin_arrays_for_swap);

    let (event_authority, _bump) =
        Pubkey::find_program_address(&[b"__event_authority"], &lb_clmm::ID);

    debug!("Event authority: {:?}", event_authority);

    // Store the length before using the vector in the loop
    let bin_arrays_len = bin_arrays_for_swap.len();
    if bin_arrays_len == 0 {
        //println!("No bin arrays found for pair: {:?}", lb_pair);
        return Err("No bin arrays found for pair".into());
    }
    
    let reserve_x = AccountMeta::new(lb_pair_state.reserve_x, false);
    let reserve_y = AccountMeta::new(lb_pair_state.reserve_y, false);
    let mint_x = AccountMeta::new_readonly(lb_pair_state.token_x_mint, false);
    let mint_y = AccountMeta::new_readonly(lb_pair_state.token_y_mint, false);
    debug!("finished meteora pool info");

    let meteora_program = AccountMeta::new_readonly(lb_clmm::ID, false);

    let keys = SwapAccountMetas {
        vec: vec![
            meteora_program,
            AccountMeta::new(*lb_pair, false),
            reserve_x,
            reserve_y,
            AccountMeta::default(),
            AccountMeta::default(),
            mint_x,
            mint_y,
            AccountMeta::new(lb_pair_state.oracle, false),
            AccountMeta::default(),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ACCOUNT, false),
            AccountMeta::new_readonly(event_authority, false),
        ],
    };


    Ok((MeteoraDlmmPoolInfo {
        keys,
        amm_flag: 1, // Use stored length
        owner: "".to_string(),
    }, bin_arrays_for_swap, active_bin_container_index))
}
