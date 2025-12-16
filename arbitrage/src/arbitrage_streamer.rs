use {
    std::thread::{
        Builder,
        JoinHandle,
        sleep,
    },
    std::sync::{
        Arc,
        atomic::{
            AtomicBool,
            Ordering,
        },
        RwLock,
    },
    itertools::izip,
    std::collections::HashMap,
    std::time::Instant,
    solana_ledger::blockstore_processor::{TransactionStatusBatch, TransactionStatusMessage},
    crate::simulation::arbitrage_simulator::PoolNotification,
    crossbeam_channel::Receiver,
    std::time::Duration,
    serde::{Serialize, Deserialize},
    std::error::Error,
    std::str::FromStr,
    std::os::unix::net::{UnixStream, UnixListener},
    solana_transaction_status::token_balances::TransactionTokenBalances,
    std::io::{Read, Write},
    std::fs,
    std::collections::HashSet,
    log::debug,
    solana_sdk::{
        signature::Signature,
        clock::Slot,
        transaction::SanitizedTransaction,
    },
    bs58,
};


const UNIX_SOCKET_PATH: &str = "/tmp/solana-arbitrage-streamer.sock";


pub enum CustomTransactionStatusMessage {
    Batch(CustomStreamerTransactionStatusBatch),
    Freeze(Slot),
}

pub struct CustomStreamerTransactionStatusBatch {
    pub slot: Slot,
    pub transactions: Vec<SanitizedTransaction>,
    pub post_token_balances: TransactionTokenBalances,
}

fn serialize_slot(slot: Slot) -> Vec<u8> {

    let mut result = Vec::with_capacity(9);
    result.push(0x03);
    result.extend_from_slice(&slot.to_le_bytes());
    result
}

fn serialize_pool_notifications(
    pool_notifications: &[PoolNotification],
) -> Result<Vec<u8>, Box<dyn Error>> {
    if pool_notifications.is_empty() {
        return Ok(vec![]);
    }
    let total_len = pool_notifications.iter().map(|pn| pn.data.len()).sum::<usize>() + pool_notifications.len() * 120 + 2;
    let mut result = Vec::with_capacity(total_len);
    result.push(0xac);
    result.push(pool_notifications.len() as u8);

    for pn in pool_notifications.iter() {
        // Convert base58 string to bytes
        let pubkey_bytes = bs58::decode(&pn.pubkey)
            .into_vec()
            .map_err(|e| format!("Failed to decode base58 pubkey: {}", e))?;

        result.extend_from_slice(&pubkey_bytes);
        result.extend_from_slice(pn.txn_sig.as_ref());
        result.extend_from_slice(&pn.slot.to_le_bytes());
        result.extend_from_slice(&(pn.data.len() as u16).to_le_bytes());
        result.extend_from_slice(&pn.data);
        result.extend_from_slice(&pn.write_version.to_le_bytes());
    }
    Ok(result)
}

fn serialize_post_token_balances_into(
    post_token_balances: &TransactionTokenBalances,
    result: &mut Vec<u8>,
) -> Result<(), Box<dyn Error>> {
    let start_index = result.len();
    
    // Reserve space for the count byte and all token balances
    let total_additional_capacity = 1 + post_token_balances.iter().map(|vec| vec.len() * 41).sum::<usize>();
    result.reserve(total_additional_capacity);
    
    // Push a placeholder for the count byte
    result.push(0);
    
    let mut count = 0;
    for vec in post_token_balances {
        for token_balance in vec {
            let pubkey_bytes = bs58::decode(&token_balance.owner)
                .into_vec()
                .map_err(|e| format!("Failed to decode base58 pubkey: {}", e))?;

            let amount = u64::from_str(&token_balance.ui_token_amount.amount).unwrap();
            let account_index = token_balance.account_index;
            result.push(account_index as u8);
            result.extend_from_slice(&pubkey_bytes);
            result.extend_from_slice(&amount.to_le_bytes());
            count += 1;
        }
    }
    
    // Update the count at the start
    result[start_index] = count as u8;
    Ok(())
}

fn serialize_transaction_status_messages(
    transaction_status_messages: &[CustomTransactionStatusMessage],
) -> Vec<u8> {
    if transaction_status_messages.is_empty() {
        return vec![];
    }
    let mut result = Vec::new();
    result.push(0x00);
    result.push(transaction_status_messages.len() as u8);

    for message in transaction_status_messages {
        match message {
            CustomTransactionStatusMessage::Freeze(slot) => {
                result.reserve(9);
                result.push(0x01); // Null byte for Freeze
                result.extend_from_slice(&slot.to_le_bytes()); // Serialize slot as u64
            }
            CustomTransactionStatusMessage::Batch(CustomStreamerTransactionStatusBatch { slot, transactions, post_token_balances, .. }) => {
                for transaction in transactions {
                    let account_keys = transaction.message().account_keys();
                    
                    // Serialize transaction signature
                    let signature = transaction.signature();

                    let total_length = 1 + 8 + 1 + account_keys.len() * 32 + 64;

                    result.reserve(total_length);
                    result.push(0x02);
                    result.extend_from_slice(&slot.to_le_bytes());
                    result.extend_from_slice(&(account_keys.len() as u8).to_le_bytes());
                    for key in account_keys.iter() {
                        result.extend_from_slice(key.as_ref());
                    }
                    result.extend_from_slice(signature.as_ref());

                    serialize_post_token_balances_into(&post_token_balances, &mut result);
                }
            }
        }
    }

    result
}

pub struct TransactionNotification {
    pub txn_sig: Signature,
    pub account_keys: Vec<String>,
}

pub struct ArbitrageStreamer {
    pub thread_hdl: JoinHandle<()>,
}


impl ArbitrageStreamer {
    pub fn new(
        tracked_pubkey_receiver: Receiver<PoolNotification>,
        transaction_status_receiver: Receiver<TransactionStatusMessage>,
        slot_status_receiver: Receiver<Slot>,
        tracked_pubkeys: Arc<RwLock<HashSet<String>>>,
        exit: Arc<AtomicBool>,
    ) -> Self {
        let socket_path = UNIX_SOCKET_PATH.to_string();

        // Remove the socket file if it already exists
        let _ = fs::remove_file(&socket_path);

        let thread_hdl = Builder::new()
            .name("solTxStreamer".to_string())
            .spawn(move || {
                let mut last_checked_accept = Instant::now().checked_sub(Duration::from_secs(5)).unwrap();
                // Create the Unix domain socket listener
                let listener = match UnixListener::bind(&socket_path) {
                    Ok(listener) => listener,
                    Err(e) => {
                        debug!("Failed to create Unix socket listener: {}", e);
                        return;
                    }
                };
                debug!("Unix socket listener created at {}", socket_path);

                // Set non-blocking mode
                if let Err(e) = listener.set_nonblocking(true) {
                    debug!("Failed to set non-blocking mode: {}", e);
                    return;
                }

                let mut active_connection: Option<UnixStream> = None;
                let mut last_receive_check = Instant::now();

                let mut recent_signatures: HashSet<Signature> = HashSet::new();

                while !exit.load(Ordering::Relaxed) {
                    let mut new_slots: Vec<Slot> = match slot_status_receiver.try_recv() {
                        Ok(slot) => {
                            debug!("Received new slot: {}", slot);
                            vec![slot]
                        },
                        Err(e) => {
                            match e {
                                crossbeam_channel::TryRecvError::Empty => vec![],
                                crossbeam_channel::TryRecvError::Disconnected => {
                                    debug!("Slot channel disconnected");
                                    vec![]
                                }
                            }
                        },
                    };
                    new_slots.extend(slot_status_receiver.try_iter());
                    // Accept new connections if we don't have an active one
                    // Collect data from the receivers
                    let mut pool_notifications: Vec<PoolNotification> = match tracked_pubkey_receiver.try_recv() {
                        Ok(notification) => vec![notification],
                        Err(_) => vec![],
                    };
                    pool_notifications.extend(tracked_pubkey_receiver.try_iter());

                    let mut transaction_status_messages: Vec<TransactionStatusMessage> = match transaction_status_receiver.try_recv() {
                        Ok(message) => vec![message],
                        Err(_) => vec![],
                    };
                    transaction_status_messages.extend(transaction_status_receiver.try_iter());

                    if last_checked_accept.elapsed() >= Duration::from_secs(1) {
                        last_checked_accept = Instant::now();
                        if active_connection.is_none() {
                            match listener.accept() {
                                Ok((stream, _addr)) => {
                                    debug!("New client connected to Unix socket at time {:?}", Instant::now());
                                    stream.set_nonblocking(true).unwrap();
                                    active_connection = Some(stream);
                                }
                                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                    debug!("No connection available at the moment");
                                }
                                Err(e) => {
                                    debug!("Error accepting connection: {}", e);
                                }
                            }
                        }
                    }


                    if last_receive_check.elapsed() >= Duration::from_secs(5) {
                        last_receive_check = Instant::now();
                        // Ensure socket is connected
                        if active_connection.is_none() {
                            continue;
                        }
                        // Check for updates from the separate process
                        if let Some(ref mut stream) = active_connection {
                            let mut length_buffer = [0; 4];
                            let mut code_buffer = [0; 1];
                            
                            let code;
                            match stream.read_exact(&mut code_buffer) {
                                Ok(_) => {
                                    code = code_buffer[0];
                                }
                                Err(_e) => {
                                    let mut clear_buffer = [0; 1];
                                    loop {
                                        match stream.read_exact(&mut clear_buffer) {
                                            Ok(_) => {},
                                            Err(_) => {debug!("Emptied buffer"); break}
                                        }
                                    }
                                    continue;
                                }
                            }
                            match stream.read_exact(&mut length_buffer) {
                                Ok(_) => {
                                    let expected_length = u32::from_le_bytes(length_buffer) as usize;
                                    let mut buffer = vec![0; expected_length];
                                    let mut received_bytes = 0;

                                    while received_bytes < expected_length {
                                        match stream.read_exact(&mut buffer[received_bytes..received_bytes + 1]) {
                                            Ok(_) => {
                                                received_bytes += 1;
                                            }
                                            Err(_e) => {}
                                        }
                                    }

                                    if buffer.len() % 32 != 0 {
                                        debug!("Received buffer length {} is not a multiple of 32", buffer.len());
                                        let mut clear_buffer = vec![0; 1];
                                        loop {
                                            match stream.read_exact(&mut clear_buffer) {
                                                Ok(_) => {},
                                                Err(_) => {debug!("Emptied buffer"); break}
                                            }
                                        }
                                        continue;
                                    }
                                    
                                    let mut tracked_pubkeys = tracked_pubkeys.write().unwrap();
                                    for chunk in buffer.chunks(32) {
                                        let base58_string = bs58::encode(chunk).into_string();
                                        match code {
                                            0 => {
                                                tracked_pubkeys.remove(&base58_string);
                                            },
                                            1 => {
                                                tracked_pubkeys.insert(base58_string);
                                            },
                                            2 => {
                                                tracked_pubkeys.clear();
                                            },
                                            _ => {}
                                        }
                                    }
                                }
                                Err(_e) => {}
                            }
                        }
                    }
                    

                    pool_notifications.iter().for_each(|pn| {
                        recent_signatures.insert(pn.txn_sig);
                    });

                    let tracked_pubkeys = tracked_pubkeys.read().unwrap();

                    let custom_transaction_status_messages: Vec<CustomTransactionStatusMessage> = transaction_status_messages.iter().filter_map(|tsm| {
                        match tsm {
                            TransactionStatusMessage::Freeze(bank) => {
                                Some(CustomTransactionStatusMessage::Freeze(bank.slot()	))
                            }
                            TransactionStatusMessage::Batch(TransactionStatusBatch {
                                slot,
                                transactions,
                                commit_results,
                                balances,
                                token_balances,
                                costs,
                                transaction_indexes,
                            }) => {
                                let mut filtered_transactions = Vec::new();
                                let mut filtered_token_balances = Vec::new();

                                for (
                                    transaction,
                                    commit_result,
                                    //_pre_balances,
                                    _post_balances,
                                    //_pre_token_balances,
                                    post_token_balance,
                                    //_transaction_index,
                                ) in izip!(
                                    transactions,
                                    commit_results,
                                    //&balances.pre_balances,
                                    &balances.post_balances,
                                    //&token_balances.pre_token_balances,
                                    &token_balances.post_token_balances,
                                    //transaction_indexes,
                                ) {
                                    if recent_signatures.remove(&transaction.signature()) {
                                        filtered_transactions.push(transaction.clone());
                                        filtered_token_balances.push(post_token_balance.clone());
                                    } else {
                                        let commited_tx = if commit_result.is_err() {
                                            continue;
                                        } else {
                                            commit_result.as_ref().unwrap()
                                        };
                                        if commited_tx.status.is_err() {
                                            let signer = transaction.message().account_keys()[0];
                                            if tracked_pubkeys.contains(&signer.to_string()) {
                                                filtered_transactions.push(transaction.clone());
                                                filtered_token_balances.push(post_token_balance.clone());
                                                pool_notifications.push(PoolNotification {
                                                    pubkey: signer.to_string(),
                                                    txn_sig: *transaction.signature(),
                                                    slot: *slot,
                                                    data: vec![],
                                                    write_version: 0,
                                                });
                                                continue;
                                            }
                                            let matching_keys: Vec<_> = transaction.message().account_keys().iter()
                                                .filter(|key| tracked_pubkeys.contains(&key.to_string()))
                                                .collect();
                                            
                                            if matching_keys.len() > 1 {
                                                filtered_transactions.push(transaction.clone());
                                                filtered_token_balances.push(vec![]);
                                                
                                                // Create a new pool notification for each matching key
                                                for key in matching_keys {
                                                    pool_notifications.push(PoolNotification {
                                                        pubkey: key.to_string(),
                                                        txn_sig: *transaction.signature(),
                                                        slot: *slot,
                                                        data: vec![],
                                                        write_version: 0,
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }

                                if !filtered_transactions.is_empty() {
                                    Some(CustomTransactionStatusMessage::Batch(CustomStreamerTransactionStatusBatch {
                                        slot: *slot,
                                        transactions: filtered_transactions,
                                        post_token_balances: filtered_token_balances,
                                    }))
                                } else {
                                    None
                                }
                            }
                        }
                    }).collect();

                    let serialized_pool_notifications = match serialize_pool_notifications(&pool_notifications) {
                        Ok(data) => data,
                        Err(e) => {
                            debug!("Failed to serialize pool notifications: {}", e);
                            Vec::new()
                        }
                    };
                    // Serialize data
                    let serialized_transaction_status_messages = 
                        serialize_transaction_status_messages(&custom_transaction_status_messages);


                    // Send data over the socket
                    let mut should_drop_connection = false;
                    if let Some(ref mut stream) = active_connection {

                        if !new_slots.is_empty() {
                            if let Some(last_slot) = new_slots.last() {
                                if let Err(e) = stream.write_all(&serialize_slot(*last_slot)) {
                                    debug!("Failed to write to socket: {}", e);
                                    should_drop_connection = true;
                                }
                            }
                        }

                        if !serialized_pool_notifications.is_empty() {
                            if let Err(e) = stream.write_all(&serialized_pool_notifications) {
                                debug!("Failed to write to socket: {}", e);
                                should_drop_connection = true;
                            }
                        }

                        if !serialized_transaction_status_messages.is_empty() {
                            if let Err(e) = stream.write_all(&serialized_transaction_status_messages) {
                                debug!("Failed to write to socket: {}", e);
                                should_drop_connection = true;
                            }
                        }
                    }

                    if should_drop_connection {
                        active_connection = None;
                    }
                }

                // Cleanup: remove the socket file
                let _ = fs::remove_file(&socket_path);
            })
            .unwrap();

        Self { thread_hdl }
    }

    pub fn join(self) -> Result<(), Box<dyn Error>> {
        self.thread_hdl.join().unwrap();
        Ok(())
    }
}



#[cfg(test)]
mod tests {
    use super::*;
    use crate::arbitrage_client::*;
    use solana_sdk::{
        signature::Signature,
        pubkey::Pubkey,
        transaction::SanitizedTransaction,
        transaction::Transaction,
        message::Message,
        instruction::Instruction,
        hash::Hash,
        signature::{Keypair, Signer},
        instruction::AccountMeta,
    };
    use tokio::runtime::Runtime;
    use std::sync::atomic::AtomicU64;
    use crossbeam_channel::unbounded;
    use solana_transaction_status_client_types::TransactionTokenBalance;
    use solana_account_decoder::parse_token::UiTokenAmount;
    use solana_runtime::bank::TransactionBalancesSet;
    use solana_transaction_status::token_balances::TransactionTokenBalancesSet;

    pub fn dummy_transaction() -> Transaction {
        let keypair = Keypair::from_bytes(&[
            255, 101, 36, 24, 124, 23, 167, 21, 132, 204, 155, 5, 185, 58, 121, 75, 156, 227, 116,
            193, 215, 38, 142, 22, 8, 14, 229, 239, 119, 93, 5, 218, 36, 100, 158, 252, 33, 161,
            97, 185, 62, 89, 99, 195, 250, 249, 187, 189, 171, 118, 241, 90, 248, 14, 68, 219, 231,
            62, 157, 5, 142, 27, 210, 117,
        ])
        .unwrap();
        let to = Pubkey::from([
            1, 1, 1, 4, 5, 6, 7, 8, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 8, 7, 6, 5, 4,
            1, 1, 1,
        ]);
    
        let program_id = Pubkey::from([
            2, 2, 2, 4, 5, 6, 7, 8, 9, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 9, 8, 7, 6, 5, 4,
            2, 2, 2,
        ]);
        let account_metas = vec![
            AccountMeta::new(keypair.pubkey(), true),
            AccountMeta::new(to, false),
        ];
        let instruction =
            Instruction::new_with_bincode(program_id, &(1u8, 2u8, 3u8), account_metas);
        let message = Message::new(&[instruction], Some(&keypair.pubkey()));
        Transaction::new(&[&keypair], message, Hash::default())
    }
    
    #[test]
    fn test_serialize_slot() {
        let slot = 305780466;
        let serialized = serialize_slot(slot);
        debug!("{:?}", serialized);

    }

    #[test]
    fn test_serialize_pool_notifications() {
        let pool_notifications = vec![PoolNotification {
            pubkey: "2qEHjDLDLbuBgRYvsxhc5D6uDWAivNFZGan56P1tpump".to_string(),
            txn_sig: Signature::new_unique(),
            slot: 305780466,
            data: vec![0; 100],
            write_version: 1,
        },
        PoolNotification {
            pubkey: "2qEHjDLDLbuBgRYvsxhc5D6uDWAivNFZGan56P1tpump".to_string(),
            txn_sig: Signature::new_unique(),
            slot: 305780466,
            data: vec![0; 100],
            write_version: 1,
        }];
        let serialized = serialize_pool_notifications(&pool_notifications);
        debug!("{:?}", serialized);
    }

    #[test]
    fn test_serialize_transaction_status_messages() {
        let dummy_transaction = dummy_transaction();
        let transaction: SanitizedTransaction = SanitizedTransaction::from_transaction_for_tests(dummy_transaction);
        let transaction_token_balance: TransactionTokenBalances = vec![vec![
            TransactionTokenBalance {
                account_index: 0,
                mint: "2qEHjDLDLbuBgRYvsxhc5D6uDWAivNFZGan56P1tpump".to_string(),
                ui_token_amount: UiTokenAmount {
                    ui_amount: Some(100.0),
                    decimals: 0,
                    amount: "100".to_string(),
                    ui_amount_string: "100".to_string(),
                },
                owner: "2qEHjDLDLbuBgRYvsxhc5D6uDWAivNFZGan56P1tpump".to_string(),
                program_id: "2qEHjDLDLbuBgRYvsxhc5D6uDWAivNFZGan56P1tpump".to_string(),
            },
            TransactionTokenBalance {
                account_index: 5,
                mint: "2qEHjDLDLbuBgRYvsxhc5D6uDWAivNFZGan56P1tpump".to_string(),
                ui_token_amount: UiTokenAmount {
                    ui_amount: Some(24142.0),
                    decimals: 0,
                    amount: "24142".to_string(),
                    ui_amount_string: "24142".to_string(),
                },
                owner: "2qEHjDLDLbuBgRYvsxhc5D6uDWAivNFZGan56P1tpump".to_string(),
                program_id: "2qEHjDLDLbuBgRYvsxhc5D6uDWAivNFZGan56P1tpump".to_string(),
            }
        ]];
        let transaction_status_messages = vec![
            CustomTransactionStatusMessage::Freeze(305780466),
            CustomTransactionStatusMessage::Batch(CustomStreamerTransactionStatusBatch {
                slot: 305780466,
                transactions: vec![transaction],
                post_token_balances: transaction_token_balance,
            }),
        ];
        let serialized = serialize_transaction_status_messages(&transaction_status_messages);
        debug!("{:?}", serialized);
    }

    
}
