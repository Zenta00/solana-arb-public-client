use {
    tokio::net::UnixStream,
    tokio::io::{AsyncReadExt, AsyncWriteExt, Error},
    crossbeam_channel::{Sender, Receiver},
    std::collections::HashMap,
    tokio::time::{sleep, Duration},
    std::sync::Arc,
    std::sync::atomic::{AtomicBool, Ordering, AtomicU64},
    crate::simulation::{
        arbitrage_simulator::PoolNotification,
        simulator_safe_maths::SafeMath,
    },
    solana_sdk::{
        signature::Signature,
        pubkey::Pubkey,
    },
    serde::{Serialize, Deserialize},
    std::net::SocketAddr,
    solana_client::rpc_client::RpcClient,
    tokio::runtime::Runtime,
    std::time::Instant,
    log::debug,

};

pub const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";
pub const TRADE_PROGRAM_ID: Pubkey = Pubkey::from_str_const("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
pub const BONDING_ADDR_SEED: [u8; 13] = [98, 111, 110, 100, 105, 110, 103, 45, 99, 117, 114, 118, 101];
pub struct TrackedPubkeys {
    pub pubkeys: Vec<String>,
    pub code: u8,
}

impl TrackedPubkeys {
    pub fn new_add(pubkeys: Vec<String>) -> Self {
        Self { pubkeys, code: 1 }
    }

    pub fn new_remove(pubkeys: Vec<String>) -> Self {
        Self { pubkeys, code: 0 }
    }

    pub fn new_clear() -> Self {
        let dummy_vec = vec!["8LjfG2dAMgyTQmvKKT3VtwLgfxJc79RwkcLGuq19LSfC".to_string()];
        Self { pubkeys: dummy_vec, code: 2 }
    }
}

pub struct ArbitrageClient {
    pub thread_hdl: tokio::task::JoinHandle<()>,
    pub runtime: Arc<Runtime>,
}

impl ArbitrageClient {
    pub fn new(
        transactions_info_sender: Sender<Vec<CustomTransactionStatusBatch>>,
        highest_slot: Arc<AtomicU64>,
        pool_notifications_sender: Sender<Vec<PoolNotification>>,
        tracked_pubkeys_receiver: Receiver<TrackedPubkeys>,
        runtime: Arc<Runtime>,
        exit: Arc<AtomicBool>,
    ) -> Result<Self, Error> {

        let guard = runtime.enter();
        let thread_hdl = runtime.spawn(async move {
            let mut stream: Option<UnixStream> = None;
            let mut total_bytes_received = 0;
            let mut start_time = Instant::now();
            while !exit.load(Ordering::Relaxed) {
                if stream.is_none() {
                    println!("Attempting to connect to arbitrage streamer at time {:?}", Instant::now());
                    match tokio::time::timeout(
                        Duration::from_secs(2),
                        UnixStream::connect("/tmp/solana-arbitrage-streamer.sock")
                    ).await {
                        Ok(Ok(stream_connected)) => {
                            stream = Some(stream_connected);
                            println!("Successfully connected to arbitrage streamer at time {:?}", Instant::now());
                        }
                        Ok(Err(e)) => {
                            println!("Failed to connect to arbitrage streamer: {} (error kind: {:?})", e, e.kind());
                            sleep(Duration::from_secs(1)).await;
                            continue;
                        }
                        Err(_) => {
                            println!("Connection attempt timed out after 5 seconds");
                            sleep(Duration::from_secs(1)).await;
                            continue;
                        }
                    }
                }
                if let Some(ref mut stream_ref) = stream {
                    match read_message(stream_ref).await {
                        Ok(Message::NewSlot(slot)) => {
                            //println!("New highest slot: {}", slot);
                            highest_slot.store(slot, Ordering::Relaxed);
                        },
                        Ok(Message::CustomTransactionStatusBatch(transactions)) => {
                            let _ = transactions_info_sender.send(transactions);
                        },
                        Ok(Message::PoolNotifications(notifications)) => {
                            for notification in &notifications {
                                total_bytes_received += notification.data.len();
                            }
                            let _ = pool_notifications_sender.send(notifications);
                        },
                        Ok(Message::Unknown) => {
                            println!("Unknown message received");
                        }
                        Err(_) => {
                            stream = None;
                            sleep(Duration::from_secs(1)).await;
                            continue;
                        }
                    }
                    let tracked_pubkeys = tracked_pubkeys_receiver.try_recv();
                    if let Ok(tracked_pubkeys) = tracked_pubkeys {
                        
                        let length_buffer = ((tracked_pubkeys.pubkeys.len() * 32) as u32).to_le_bytes();
                        let _ = stream_ref.write_all(&[tracked_pubkeys.code]).await;
                        let _ = stream_ref.write_all(&length_buffer).await;
                        for chunk in tracked_pubkeys.pubkeys.chunks(100) {
                            let mut chunk_bytes = Vec::with_capacity(32 * chunk.len());
                            for pubkey in chunk {
                                let pubkey_bytes = bs58::decode(pubkey)
                                .into_vec()
                                .unwrap();
                                chunk_bytes.extend(pubkey_bytes);
                            }
                            let _ = stream_ref.write_all(&chunk_bytes).await;
                        }
                    }

                    if start_time.elapsed().as_secs() > 1 {
                        total_bytes_received = 0;
                        start_time = Instant::now();
                    }
                }

                

            }
        });
        Ok(Self { thread_hdl, runtime })
    }
}

async fn read_message(stream: &mut UnixStream) -> Result<Message, Box<dyn std::error::Error + Send + Sync>> {
    let mut header = [0u8; 1];
    stream.read_exact(&mut header).await?;
    match header[0] {
        0x00 => read_transaction_status(stream).await,
        0x03 => read_new_slot(stream).await,
        0xac => read_pool_notifications(stream).await,
        _ => Err("Unknown message type".into()),
    }
}

async fn read_new_slot(stream: &mut UnixStream) -> Result<Message, Box<dyn std::error::Error + Send + Sync>> {
    let mut slot_bytes = [0u8; 8];
    stream.read_exact(&mut slot_bytes).await?;
    let slot = u64::from_le_bytes(slot_bytes);
    Ok(Message::NewSlot(slot))
}

async fn read_pool_notifications(stream: &mut UnixStream) -> Result<Message, Box<dyn std::error::Error + Send + Sync>> {
    // Read number of notifications
    let mut count_buf = [0u8; 1];
    stream.read_exact(&mut count_buf).await?;
    let count = count_buf[0] as usize;
    let mut notifications = Vec::with_capacity(count);
    for _ in 0..count {
        let mut pubkey_bytes = [0u8; 32];
        stream.read_exact(&mut pubkey_bytes).await?;
        let pubkey = bs58::encode(&pubkey_bytes).into_string();

        let mut txn_sig_bytes = [0u8; 64];
        stream.read_exact(&mut txn_sig_bytes).await?;
        let txn_sig = Signature::from(txn_sig_bytes);

        let mut slot_bytes = [0u8; 8];
        stream.read_exact(&mut slot_bytes).await?;
        let slot = u64::from_le_bytes(slot_bytes);

        let mut data_len_bytes = [0u8; 2];
        stream.read_exact(&mut data_len_bytes).await?;
        let data_len = u16::from_le_bytes(data_len_bytes);

        let mut data = vec![0u8; data_len.into()];
        stream.read_exact(&mut data).await?;
        

        let mut write_version_bytes = [0u8; 8];
        stream.read_exact(&mut write_version_bytes).await?;
        let write_version = u64::from_le_bytes(write_version_bytes);

        notifications.push(PoolNotification { pubkey, txn_sig, slot, data, write_version });
    }
    Ok(Message::PoolNotifications(notifications))
}

async fn read_transaction_status(stream: &mut UnixStream) -> Result<Message, Box<dyn std::error::Error + Send + Sync>> {
    // Read total length
    let mut lenght_bytes = [0u8; 1];
    stream.read_exact(&mut lenght_bytes).await?;
    let lenght = lenght_bytes[0] as usize;
    let mut transactions = Vec::with_capacity(lenght);
    for _ in 0..lenght {
        let mut header = [0u8; 1];
        stream.read_exact(&mut header).await?;
        match header[0] {
            0x01 => {
                let mut slot_bytes = [0u8; 8];
                stream.read_exact(&mut slot_bytes).await?;
                let slot = u64::from_le_bytes(slot_bytes);
                transactions.push(CustomTransactionStatusBatch::FreezeSlot(slot));
            },
            0x02 => {
                let mut slot_bytes = [0u8; 8];
                stream.read_exact(&mut slot_bytes).await?;
                let slot = u64::from_le_bytes(slot_bytes);
                
                let mut account_keys_lenght_bytes = [0u8; 1];
                stream.read_exact(&mut account_keys_lenght_bytes).await?;
                let account_keys_lenght = account_keys_lenght_bytes[0] as usize;

                let mut account_keys = Vec::with_capacity(account_keys_lenght * 32);
                for _ in 0..account_keys_lenght {
                    let mut account_key_bytes = [0u8; 32];
                    stream.read_exact(&mut account_key_bytes).await?;
                    let account_key = bs58::encode(&account_key_bytes).into_string();
                    account_keys.push(account_key);
                }

                let mut signature_bytes = [0u8; 64];
                stream.read_exact(&mut signature_bytes).await?;

                let mut post_token_balances = Vec::new();
                let mut post_token_balances_lenght_bytes = [0u8; 1];
                stream.read_exact(&mut post_token_balances_lenght_bytes).await?;
                let post_token_balances_lenght = post_token_balances_lenght_bytes[0] as usize;
                for _ in 0..post_token_balances_lenght {
                    let mut account_index_bytes = [0u8; 1];
                    stream.read_exact(&mut account_index_bytes).await?;
                    let account_index = account_index_bytes[0] as u8;

                    let mut owner_bytes = [0u8; 32];
                    stream.read_exact(&mut owner_bytes).await?;
                    let owner = bs58::encode(owner_bytes).into_string();

                    let mut ui_token_amount_bytes = [0u8; 8];
                    stream.read_exact(&mut ui_token_amount_bytes).await?;
                    let ui_token_amount = u64::from_le_bytes(ui_token_amount_bytes);

                    post_token_balances.push(PostTokenBalance { account_index, owner, ui_token_amount });
                }
                transactions.push(CustomTransactionStatusBatch::Transaction(TransactionInfo { signature: Signature::from(signature_bytes), account_keys, slot, post_token_balances }));
            },
            _ => {},
        }
    }
    Ok(Message::CustomTransactionStatusBatch(transactions))
}


#[derive(Debug)]
pub struct PostTokenBalance {
    pub account_index: u8,
    pub owner: String,
    pub ui_token_amount: u64,
}
#[derive(Debug)]
pub enum Message {
    PoolNotifications(Vec<PoolNotification>),
    CustomTransactionStatusBatch(Vec<CustomTransactionStatusBatch>),
    NewSlot(u64),
    Unknown,
}

#[derive(Debug)]
pub enum CustomTransactionStatusBatch {
    FreezeSlot(u64),
    Transaction(TransactionInfo),
}

#[derive(Debug)]
pub struct TransactionInfo {
    pub signature: Signature,
    pub account_keys: Vec<String>,
    pub slot: u64,
    pub post_token_balances: Vec<PostTokenBalance>,
}

impl TransactionInfo {
    pub fn find_mint(&self, pool_addr: &String) -> Option<String> {
        for account_key in &self.account_keys {
            let pubkey_bytes = Pubkey::from_str_const(account_key).to_bytes();
            let (bonding_addr, _) = Pubkey::find_program_address(&[&BONDING_ADDR_SEED, &pubkey_bytes], &TRADE_PROGRAM_ID);
            
            if bonding_addr == Pubkey::from_str_const(pool_addr) {
                return Some(account_key.clone());
            }
        }
        None
    }
}

pub struct CustomLeaderScheduleCache {
    pub leader_schedule: Vec<(u64, String)>,
}

impl CustomLeaderScheduleCache {
    pub fn new(
        connexion: &RpcClient,
        slots_to_advance: u64,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {

        let highest_slot = connexion.get_slot()?;
        let mut leader_schedule = Vec::new();
        let rpc_leader_schedule = match connexion.get_slot_leaders(highest_slot, slots_to_advance) {
            Ok(rpc_leader_schedule) => rpc_leader_schedule,
            Err(_) => {
                println!("Failed to get leader schedule");
                return Err("Failed to get leader schedule".into());
            }
        };
        for (index, leader) in rpc_leader_schedule.iter().enumerate() {
            leader_schedule.push((highest_slot + index as u64, leader.to_string()));
        }

        println!("Leader schedule initalized");

        Ok(Self { leader_schedule })
    }

    pub fn advance_n_slots(&mut self, connexion: Arc<RpcClient>, n_slots: u64, next_starting_slot: u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let last_slot = self.leader_schedule.last().unwrap().0;
        let rpc_leader_schedule = connexion.get_slot_leaders(last_slot + 1, n_slots)?;
        for (index, leader) in rpc_leader_schedule.iter().enumerate() {
            self.leader_schedule.push((last_slot + 1 + index as u64, leader.to_string()));
        }
        let first_slot = self.leader_schedule.first().unwrap().0;
        if next_starting_slot > first_slot {
            let starting_slot_index = next_starting_slot - first_slot;
            self.leader_schedule.drain(0..starting_slot_index as usize);
        }
        Ok(())
    }

    pub fn slot_leader_at(&self, slot: u64) -> Option<String> {
        let first_slot = self.leader_schedule.first().unwrap().0;
        match slot.checked_sub(first_slot) {
            Some(target_slot_index) => {
                if target_slot_index < self.leader_schedule.len() as u64 {
                    return Some(self.leader_schedule[target_slot_index as usize].1.clone());
                }
            }
            None => {
                return None;
            }
        }
        None
    }
}


pub struct CustomTpuInfo {
    pub tpu_infos: HashMap<String, Option<SocketAddr>>,
}

impl CustomTpuInfo {
    pub fn new(connexion: &RpcClient) -> Self {
        let tpu_infos = connexion.get_cluster_nodes().unwrap();
        let mut tpu_infos_map = HashMap::new();
        for tpu_info in tpu_infos {
            tpu_infos_map.insert(tpu_info.pubkey.to_string(), tpu_info.tpu_quic);
        }
        Self { tpu_infos: tpu_infos_map }
    }

    pub fn refresh_recent_peers(&mut self, connexion: &RpcClient) {
        let tpu_infos = connexion.get_cluster_nodes().unwrap();
        let mut tpu_infos_map = HashMap::new();
        for tpu_info in tpu_infos {
            tpu_infos_map.insert(tpu_info.pubkey.to_string(), tpu_info.tpu_quic);
        }
        self.tpu_infos = tpu_infos_map;
    }

    pub fn get_leader_address(&self, leader_id: &String) -> Option<SocketAddr> {
        if let Some(result) = self.tpu_infos.get(leader_id) {
            return *result;
        }
        None
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_clear_tracked_pubkeys() {
        let tracked_pubkeys: TrackedPubkeys = TrackedPubkeys::new_clear();
        let mut stream: Option<UnixStream> = None;
        let highest_slot = Arc::new(AtomicU64::new(0));
        loop {
            if stream.is_none() {
                println!("Attempting to connect to arbitrage streamer at time {:?}", Instant::now());
                match tokio::time::timeout(
                    Duration::from_secs(2),
                    UnixStream::connect("/tmp/solana-arbitrage-streamer.sock")
                ).await {
                    Ok(Ok(stream_connected)) => {
                        stream = Some(stream_connected);
                        println!("Successfully connected to arbitrage streamer at time {:?}", Instant::now());
                    }
                    Ok(Err(e)) => {
                        println!("Failed to connect to arbitrage streamer: {} (error kind: {:?})", e, e.kind());
                        sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                    Err(_) => {
                        println!("Connection attempt timed out after 5 seconds");
                        sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                }
            }
            if let Some(ref mut stream_ref) = stream {
                match read_message(stream_ref).await {
                    Ok(Message::NewSlot(slot)) => {
                        //println!("New highest slot: {}", slot);
                        highest_slot.store(slot, Ordering::Relaxed);
                    },
                    Ok(Message::Unknown) => {
                        println!("Unknown message received");
                    }
                    Err(_) => {
                        stream = None;
                        sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                    _ => {
                        sleep(Duration::from_secs(1)).await;
                    }
                }

                
                let length_buffer = ((tracked_pubkeys.pubkeys.len() * 32) as u32).to_le_bytes();
                let _ = stream_ref.write_all(&[tracked_pubkeys.code]).await;
                let _ = stream_ref.write_all(&length_buffer).await;
                for chunk in tracked_pubkeys.pubkeys.chunks(100) {
                    let mut chunk_bytes = Vec::with_capacity(32 * chunk.len());
                    for pubkey in chunk {
                        let pubkey_bytes = bs58::decode(pubkey)
                        .into_vec()
                        .unwrap();
                        chunk_bytes.extend(pubkey_bytes);
                    }
                    let _ = stream_ref.write_all(&chunk_bytes).await;
                }
                std::thread::sleep(Duration::from_secs(10));
                break;
                

            }
    }
}
}
