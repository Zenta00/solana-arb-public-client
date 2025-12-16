use futures::stream::{FuturesUnordered, StreamExt};
use quinn::Connection;
use scopeguard::guard;
use solana_ledger::leader_schedule_cache::LeaderScheduleCache;
use solana_net_utils::VALIDATOR_PORT_RANGE;
use solana_quic_client::nonblocking::quic_client::*;
use solana_sdk::clock::Slot;
use solana_sdk::pubkey::Pubkey;
use std::sync::atomic::AtomicU64;
use solana_sdk::{quic::QUIC_KEEP_ALIVE, signature::Keypair};
use solana_streamer::{
    nonblocking::quic::ALPN_TPU_PROTOCOL_ID,
};
use solana_tls_utils::{
    new_dummy_x509_certificate,
    SkipServerVerification
};
use crate::arbitrage_client::{CustomLeaderScheduleCache, CustomTpuInfo};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::sync::{atomic::Ordering, RwLock};
use std::time::Instant;
use std::{
    error::Error,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::runtime::Runtime;
use tokio::sync::RwLock as TokioRwLock;
use tokio::task::JoinHandle as TokioJoinHandle;
use tokio::time::timeout;

use log::debug;
use quinn::crypto::rustls::QuicClientConfig;
use quinn::{ClientConfig, Endpoint, EndpointConfig, IdleTimeout, TransportConfig};
use std::collections::HashSet;

pub const IDLE_TIMEOUT_CUSTOM: Duration = Duration::from_secs(2);
pub const SLOT_PRESHOT_DIFF: u64 = 20;
pub const LEADERS_TO_TRACK: usize = 5;

const AMS_1_JITO_PROXY: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(152,53,106,164)), 6969);

/* 
async fn get_leader_address(
    cluster_tpu_info: Arc<RwLock<CustomTpuInfo>>,
    leader_id: &Pubkey,
) -> Option<SocketAddr> {
    if let Some(addr) = cluster_tpu_info.read().unwrap().recent_peers.get(leader_id) {
        return Some(addr.1);
    }
    debug!("Leader not found in recent peers");
    return None;
}
*/

pub struct ConnectionHandler {
    addr: SocketAddr,
    _start_slot: Slot,
    end_slot: Slot,
    pub connection: Arc<TokioRwLock<Option<Connection>>>,
    leader_identity: String,
    is_refreshing: AtomicBool,
    should_abort: Arc<AtomicBool>,
}

impl ConnectionHandler {
    fn new(
        addr: SocketAddr,
        slot: Slot,
        leader_identity: String,
        leader_schedule_cache: Arc<RwLock<CustomLeaderScheduleCache>>,
        shared_connexion: Option<Arc<TokioRwLock<Option<Connection>>>>,
    ) -> Result<Self, quinn::ConnectionError> {
        debug!(
            "Creating new ConnectionHandler for leader {} at slot {}",
            leader_identity, slot
        );
        let start_time = Instant::now();

        // Find the start and end slot for this leader
        let mut start_slot = slot;
        let mut end_slot = slot;

        // Look backwards to find start slot
        let leader_schedule_cache = leader_schedule_cache.read().unwrap();
        while start_slot > 0 {
            if let Some(leader) = leader_schedule_cache.slot_leader_at(start_slot - 1) {
                if leader.to_string() != leader_identity {
                    break;
                }
            }
            start_slot -= 1;
        }

        // Look forwards to find end slot
        while let Some(leader) = leader_schedule_cache.slot_leader_at(end_slot + 1) {
            if leader.to_string() != leader_identity {
                break;
            }
            end_slot += 1;
        }

        debug!(
            "Leader schedule range calculated: slots {} to {} (took {:?})",
            start_slot,
            end_slot,
            start_time.elapsed()
        );

        if let Some(shared_connexion) = shared_connexion {
            return Ok(Self {
                addr,
                _start_slot: start_slot,
                end_slot,
                connection: shared_connexion,
                leader_identity,
                is_refreshing: AtomicBool::new(false),
                should_abort: Arc::new(AtomicBool::new(false)),
            });
        };

        /*

        let connection = tokio::time::timeout(
            Duration::from_secs(1),
            endpoint.connect(addr, &start_slot.to_string()).unwrap()
        )
        .await
        .ok() // Handle timeout error by converting to Option
        .and_then(|result| result.ok()); // Handle connection error by converting to Option


        debug!("Initial connection attempt for leader {}: {}",
            leader_identity,
            if connection.is_some() { "successful" } else { "failed" });
        */

        Ok(Self {
            addr,
            _start_slot: start_slot,
            end_slot,
            connection: Arc::new(TokioRwLock::new(None)),
            leader_identity,
            is_refreshing: AtomicBool::new(false),
            should_abort: Arc::new(AtomicBool::new(false)),
        })
    }

    async fn refresh(&self, endpoint: Arc<Endpoint>) -> Result<(), quinn::ConnectionError> {
        if self.is_refreshing.load(Ordering::Relaxed) {
            return Ok(());
        }
        self.is_refreshing.store(true, Ordering::Relaxed);

        // Use defer-like pattern to ensure is_refreshing is always set to false
        let _cleanup = guard((), |_| {
            self.is_refreshing.store(false, Ordering::Relaxed);
        });

        if let Some(conn) = self.connection.read().await.as_ref() {
            //debug!("Attempting to refresh existing connection for leader {}", self.leader_identity);
            if let Ok(mut send) = conn.open_uni().await {
                if let Err(e) = send.write_all(b"0").await {
                    debug!(
                        "Error writing to stream for leader {}: {:?}",
                        self.leader_identity, e
                    );
                }
                if let Err(e) = send.finish() {
                    debug!(
                        "Error finishing stream for leader {}: {:?}",
                        self.leader_identity, e
                    );
                } else {
                    return Ok(());
                }
            } else {
                debug!(
                    "Failed to open uni-directional stream for leader {}",
                    self.leader_identity
                );
            }
        }

        *self.connection.write().await = None;

        // Only attempt reconnection if we have no connection AND 200ms have passed
        if self.connection.read().await.is_none() {
            debug!(
                "Starting connection attempts for leader {}",
                self.leader_identity
            );
            let server_addr = self.addr;
            let leader_identity = self.leader_identity.clone();

            // FuturesUnordered to hold connection attempts
            let mut tasks = FuturesUnordered::<
                Pin<Box<dyn Future<Output = Option<quinn::Connection>> + Send>>,
            >::new();

            // Main reconnection attempt loop
            loop {
                // Check the atomic flag to decide if we should keep reconnecting
                if self.should_abort.load(Ordering::Relaxed) {
                    debug!(
                        "Aborting connection attempts for leader {}",
                        self.leader_identity
                    );
                    drop(tasks);
                    break;
                }

                // Add a delay future to regulate the connection attempt interval
                tasks.push(Box::pin(async {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    None // Return `None` to indicate it's just an interval delay
                }));

                // Add a connection attempt with a timeout to the task set
                let connection_future = endpoint
                    .connect(server_addr, "localhost")
                    .map_err(|_e| quinn::ConnectionError::Reset)?;
                tasks.push(Box::pin(async move {
                    match timeout(Duration::from_secs(1), connection_future).await {
                        Ok(Ok(conn)) => {
                            debug!("Connection successful for leader ");
                            Some(conn) // Return `Some(conn)` if successful
                        }
                        Ok(Err(_e)) => {
                            debug!("Connection failed ");
                            None
                        }
                        Err(_) => {
                            debug!("Connection attempt timed out for leader");
                            None
                        }
                    }
                }));

                // Await the next completed future in `tasks`
                if let Some(Some(new_connection)) = tasks.next().await {
                    // Set the successful connection and exit the loop
                    debug!(
                        "Successfully established connection for leader {}",
                        leader_identity
                    );
                    *self.connection.write().await = Some(new_connection);
                    break; // Abort future attempts as connection is now established
                }
                debug!(
                    "Connection attempt failed for leader {} at timestamp {:?}",
                    leader_identity,
                    Instant::now().elapsed()
                );
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct JitoLeaderCache {
    pub leaders: Arc<TokioRwLock<HashSet<String>>>,
}

impl JitoLeaderCache {
    pub async fn is_jito_leader(&self, leader_id: &str) -> bool {
        let leaders = self.leaders.read().await;
        leaders.contains(leader_id)
    }
}

pub struct LeaderConnexionCache {
    pub connections_handlers: TokioRwLock<Vec<Arc<ConnectionHandler>>>,
}
pub struct ConnexionHandlerWrapper {
    pub handler: Option<Arc<ConnectionHandler>>,
}

impl LeaderConnexionCache {

    pub async fn get_current_connexion(&self, slot: u64) -> Option<Arc<ConnexionHandlerWrapper>> {
        let handlers = self.connections_handlers.read().await;
        handlers.iter().find(|h| h._start_slot <= slot && slot <= h.end_slot)
            .map(|handler| Arc::new(ConnexionHandlerWrapper {
                handler: Some(handler.clone())
            }))
    }

    pub async fn get_current_and_next_connexions(&self, slot: u64) -> Vec<Arc<ConnexionHandlerWrapper>> {
        let handlers = self.connections_handlers.read().await;
        let mut result = Vec::new();
        
        // Get current slot handler
        if let Some(current_handler) = handlers.iter().find(|h| h._start_slot <= slot && slot <= h.end_slot) {
            result.push(Arc::new(ConnexionHandlerWrapper {
                handler: Some(current_handler.clone())
            }));
        }
        
        // Get next slot handler
        if let Some(next_handler) = handlers.iter().find(|h| h._start_slot <= slot + 1 && slot + 1 <= h.end_slot) {
            // Only add if it's different from the current handler
            if result.is_empty() || next_handler._start_slot != result[0].handler.as_ref().unwrap()._start_slot {
                result.push(Arc::new(ConnexionHandlerWrapper {
                    handler: Some(next_handler.clone())
                }));
            }
        }
        
        result
    }

    pub async fn get_current_and_next_connexions_rtt(
        &self,
        slot: u64,
    ) -> (Option<Duration>, Option<Duration>) {
        let handlers = self.connections_handlers.read().await;
        
        // Find the handler for the current slot
        let current_handler = handlers.iter().find(|h| 
            h._start_slot <= slot && slot <= h.end_slot
        );
        
        // Find the handler for the next slot
        let next_handler = handlers.iter().find(|h| 
            h._start_slot <= slot + 1 && slot + 1 <= h.end_slot
        );
        
        let current_rtt = if let Some(handler) = current_handler {
            if let Some(conn) = &*handler.connection.read().await {
                Some(conn.rtt())
            } else {
                None
            }
        } else {
            None
        };
        
        let next_rtt = if let Some(handler) = next_handler {
            if let Some(conn) = &*handler.connection.read().await {
                Some(conn.rtt())
            } else {
                None
            }
        } else {
            None
        };
        
        (current_rtt, next_rtt)
    }

    pub async fn get_next_leaders_connexions(
        &self,
        num_leaders_forwards: usize,
    ) -> Result<Vec<ConnexionHandlerWrapper>, Box<dyn Error + Send + Sync>> {
        let handlers = self.connections_handlers.read().await;
        let futures = handlers.iter().take(num_leaders_forwards).map(|h| async {
            match &h.connection.read().await.as_ref() {
                Some(conn) => {
                    if conn.close_reason().is_none() {
                        ConnexionHandlerWrapper {
                            handler: Some(h.clone()),
                        }
                    } else {
                        ConnexionHandlerWrapper {
                            handler: None,
                        }
                    }
                }
                None => ConnexionHandlerWrapper {
                    handler: None,
                },
            }
        });
        Ok(futures::future::join_all(futures).await)
    }
}

impl LeaderConnexionCache {
    fn new() -> Self {
        Self {
            connections_handlers: TokioRwLock::new(Vec::new()),
        }
    }
}

pub struct ConnexionRoutineService {
    pub leader_connexion_cache: Arc<LeaderConnexionCache>,
    pub runtime: Runtime,
}

impl ConnexionRoutineService {
    pub fn new(
        highest_slot: Arc<AtomicU64>,
        exit: Arc<AtomicBool>,
        leader_schedule_cache: Arc<RwLock<CustomLeaderScheduleCache>>,
        cluster_tpu_info: Arc<RwLock<CustomTpuInfo>>,
    ) -> Result<Self, quinn::ConnectionError> {
        debug!("Initializing ConnexionRoutineService test");

        let leader_connexion_cache = Arc::new(LeaderConnexionCache::new());
        let leader_connexion_cache_clone = leader_connexion_cache.clone();

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        let runtime_handle = runtime.handle();
        let _guard = runtime.enter();

        let jito_proxies = vec![
            AMS_1_JITO_PROXY,
        ];

        runtime_handle.spawn(async move {
            debug!("ConnexionRoutineService main loop started");
            let mut last_slot = highest_slot.load(Ordering::Relaxed);
            let endpoint = Arc::new(run_client().await.unwrap());
            let mut refresh_tasks = Vec::new();

            let mut proxy_last_try_connection = Instant::now();
            tokio::time::sleep(Duration::from_secs(5)).await;
           
            while !exit.load(Ordering::Relaxed) {

                let working_slot = highest_slot.load(Ordering::Relaxed);
                if working_slot > last_slot {
                    debug!("Slot advanced from {} to {}", last_slot, working_slot);
                    let update_start = Instant::now();

                    let mut handlers = leader_connexion_cache_clone
                        .connections_handlers
                        .write()
                        .await;
                    let mut removed_count = 0;

                    // Remove outdated connections
                    while let Some(handler) = handlers.first() {
                        if handler.end_slot < working_slot {
                            handler.should_abort.store(true, Ordering::Relaxed);
                            handlers.remove(0);
                            removed_count += 1;
                        } else {
                            break;
                        }
                    }
                    // Calculate the last slot we're currently tracking
                    let last_tracked_slot =
                        handlers.last().map(|h| h.end_slot).unwrap_or(working_slot);
                    // Look ahead for new leaders
                    let mut current_slot = last_tracked_slot + 1;
                    while handlers.len() < LEADERS_TO_TRACK {
                        let leader_schedule = leader_schedule_cache.read().unwrap();
                        if let Some(leader_id) =
                            leader_schedule.slot_leader_at(current_slot)
                        {
                            let leader_str = leader_id.to_string();

                            // Only add if we don't already have a connection for this leader
                            let matching_handler =
                                handlers.iter().find(|h| h.leader_identity == leader_str);
                            let cluster_tpu_info = cluster_tpu_info.read().unwrap();
                            let addr = match cluster_tpu_info.get_leader_address(
                                &leader_id,
                            )
                            {
                                Some(addr) => addr,
                                None => {
                                    debug!("Leader {} not found in recent peers", leader_str);
                                    let dummy_addr = SocketAddr::new(
                                        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                                        1234,
                                    );
                                    dummy_addr
                                }
                            };
                            /* 
                            let is_jito_leader =
                                jito_leader_cache_clone.is_jito_leader(&leader_str).await;
                            */
                            match matching_handler {
                                Some(h) if current_slot > h.end_slot => {
                                    debug!(
                                        "Reusing connection for leader {} for slots {} to {}",
                                        leader_str, current_slot, h.end_slot
                                    );
                                    if let Ok(handler) = ConnectionHandler::new(
                                        addr,
                                        current_slot,
                                        leader_str.clone(),
                                        leader_schedule_cache.clone(),
                                        Some(Arc::clone(&h.connection)),
                                    ) {
                                        handlers.push(Arc::new(handler));
                                    }
                                }
                                None => {
                                    debug!(
                                        "Creating new connection for leader {} starting at slot {}",
                                        leader_str, current_slot
                                    );
                                    if let Ok(handler) = ConnectionHandler::new(
                                        addr,
                                        current_slot,
                                        leader_str.clone(),
                                        leader_schedule_cache.clone(),
                                        None,
                                    ) {
                                        handlers.push(Arc::new(handler));
                                    }
                                }
                                _ => {} // Skip if current_slot <= h.end_slot
                            }
                        }
                        current_slot += 1;
                    }
                    debug!(
                        "Connection handlers updated: {} removed, {} total (took {:?})",
                        removed_count,
                        handlers.len(),
                        update_start.elapsed()
                    );

                    last_slot = working_slot;
                }
                let handlers = leader_connexion_cache_clone
                    .connections_handlers
                    .read()
                    .await;
                // Track which leaders we've already refreshed
                refresh_tasks.retain(|task: &TokioJoinHandle<()>| !task.is_finished());

                // Create a HashSet to track unique leader identities we've seen
                let mut seen_leaders = std::collections::HashSet::new();

                for handler in handlers.iter() {
                    // Only process this handler if we haven't seen its leader_identity yet
                    // and it's not already refreshing
                    if seen_leaders.insert(&handler.leader_identity)
                        && !handler.is_refreshing.load(Ordering::Relaxed)
                    {
                        let handler = Arc::clone(handler);
                        let endpoint_clone = endpoint.clone();

                        // Spawn a new refresh task
                        let refresh_task = tokio::spawn(async move {
                            if let Err(e) = handler.refresh(endpoint_clone).await {
                                debug!(
                                    "Refresh failed for leader {}: {:?}",
                                    handler.leader_identity, e
                                );
                            }
                        });

                        refresh_tasks.push(refresh_task);
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });

        debug!("ConnexionRoutineService initialization completed");
        Ok(Self {
            leader_connexion_cache,
            runtime,
        })
    }
}


async fn run_client() -> Result<Endpoint, Box<dyn Error + Send + Sync + 'static>> {
    let (cert, priv_key) = new_dummy_x509_certificate(&Keypair::new());
    let mut endpoint = {
        let client_socket = solana_net_utils::bind_in_range(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            VALIDATOR_PORT_RANGE,
        )
        .expect("QuicLazyInitializedEndpoint::create_endpoint bind_in_range")
        .1;

        QuicNewConnection::create_endpoint(EndpointConfig::default(), client_socket)
    };

    let mut crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(SkipServerVerification::new())
        .with_client_auth_cert(vec![cert.clone()], priv_key.clone_key())
        .expect("Failed to set QUIC client certificates");
    crypto.enable_early_data = true;
    crypto.alpn_protocols = vec![ALPN_TPU_PROTOCOL_ID.to_vec()];

    let mut config = ClientConfig::new(Arc::new(QuicClientConfig::try_from(crypto).unwrap()));
    let mut transport_config = TransportConfig::default();

    let timeout = IdleTimeout::try_from(IDLE_TIMEOUT_CUSTOM).unwrap();
    transport_config.max_idle_timeout(Some(timeout));
    transport_config.keep_alive_interval(Some(QUIC_KEEP_ALIVE));
    config.transport_config(Arc::new(transport_config));

    endpoint.set_default_client_config(config);
    Ok(endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::pools_manager::ConnectionManager;
    use bincode::serialize;
    use solana_client::rpc_client::RpcClient;
    use solana_rpc_client_api::config::RpcSendTransactionConfig;
    use solana_sdk::commitment_config::CommitmentConfig;
    use solana_sdk::commitment_config::CommitmentLevel;

    /* 

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_connection_handler() -> Result<(), Box<dyn std::error::Error>> {
        let endpoint = Arc::new(run_client().await.unwrap());
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(64, 130, 50, 104)), 11228);
        let rpc_url = "https://solana-mainnet.core.chainstack.com/e8122c48af8083c0eee6531e86d7fbaf";
        let rpc_client = RpcClient::new_with_commitment(rpc_url, CommitmentConfig::confirmed());

        let connection_manager = ConnectionManager::new();
        let _ = connection_manager.update();
        let arbitrage_pair = ArbitragePair::new(
            "2qEHjDLDLbuBgRYvsxhc5D6uDWAivNFZGan56P1tpump".to_string(),
            &"69Bea5SXp3JPCduZFDW1751G9Ss4mPDApQYDbQyd35iq".to_string(),
            &"4AZRPNEfCJ7iw28rJu5aUyeQhYcvdcNm8cswyL51AY9i".to_string(),
            "meteoraDlmm".to_string(),
            "raydium".to_string(),
            Pubkey::from_str_const("CBomB513173WTtFBCfoqRC8Xs9FM3zTc8UHJ9xjWPcdP"),
            Pubkey::from_str_const("6dKxJwmc2dFSzru3DSBQk6cbMZESQ5DXUJYY39NwMk2t"),
            &connection_manager,
        )?;
        println!("Arbitrage pair initialized");

        let test_timeout = Duration::from_secs(300);
        let test_start = Instant::now();
        let mut last_connection_attempt = Instant::now();

        // Initial connection attempt
        let mut connection = match tokio::time::timeout(
            Duration::from_secs(1),
            endpoint.connect(addr, "test").unwrap(),
        )
        .await
        {
            Ok(Ok(conn)) => {
                println!("Initial connection established");
                Some(conn)
            }
            _ => {
                println!("Failed to establish initial connection");
                None
            }
        };

        while test_start.elapsed() < test_timeout {
            match &mut connection {
                Some(conn) => {
                    // Try to send transaction if we have a connection
                    if let Ok(mut stream) = conn.open_uni().await {
                        let _ = connection_manager.update();
                        arbitrage_pair.update_tx(true, false, false, &connection_manager)?;
                        println!(
                            " blockhash: {:?}",
                            connection_manager.get_recent_blockhash(CommitmentLevel::Processed)
                        );
                        let tx = arbitrage_pair.get_transaction(false)?;
                        let tx_bytes = serialize(&tx)?;
                        println!("simulated json: {:?}", rpc_client.simulate_transaction(&tx));
                        rpc_client.send_transaction_with_config(
                            &tx,
                            RpcSendTransactionConfig {
                                skip_preflight: true,
                                preflight_commitment: Some(CommitmentLevel::Confirmed),
                                encoding: None,
                                max_retries: Some(3),
                                min_context_slot: None,
                            },
                        )?;
                        println!("Sending transaction");
                        println!("Tx bytes: {:?}", tx_bytes);
                        //println!("Tx bytes base64: {}", base64::encode(&tx_bytes));
                        println!("tx: {:?}", tx);

                        // Add timeout for write operation
                        if let Err(e) = tokio::time::timeout(
                            Duration::from_secs(5),
                            stream.write_all(&tx_bytes),
                        )
                        .await?
                        {
                            println!("Error writing transaction to stream: {}", e);
                            connection = None; // Reset connection on error
                            continue;
                        }
                        let _ = stream.finish();
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        break;
                    } else {
                        connection = None; // Reset connection if we can't open stream
                    }
                }
                None => {
                    // Only attempt reconnection if 200ms have passed
                    if last_connection_attempt.elapsed() > Duration::from_millis(200) {
                        println!("Attempting to reconnect...");
                        match tokio::time::timeout(
                            Duration::from_secs(1),
                            endpoint.connect(addr, "test").unwrap(),
                        )
                        .await
                        {
                            Ok(Ok(new_connection)) => {
                                connection = Some(new_connection);
                                println!("Successfully reconnected");
                            }
                            Ok(Err(e)) => println!("Failed to reconnect: {}", e),
                            Err(_) => println!("Connection attempt timed out"),
                        }
                        last_connection_attempt = Instant::now();
                    }
                }
            }

            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        Ok(())
    }
    */

    #[tokio::test]
    async fn test_connection_handler_refresh() -> Result<(), Box<dyn std::error::Error>> {
        // Initialize the QUIC endpoint
        let endpoint = Arc::new(run_client().await.unwrap());

        // Create connection handler with test values
        let handler = ConnectionHandler {
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(145, 40, 126, 41)), 8010),
            _start_slot: 0,
            end_slot: 100,
            connection: Arc::new(TokioRwLock::new(None)),
            leader_identity: "722RdWmHC5TGXBjTejzNjbc8xEiduVDLqZvoUGz6Xzbp".to_string(),
            is_refreshing: AtomicBool::new(false),
            should_abort: Arc::new(AtomicBool::new(false)),
        };
        let handler = Arc::new(handler);

        // Run refresh loop for 10 seconds
        let start_time = Instant::now();
        let timeout = Duration::from_secs(10);

        while start_time.elapsed() < timeout {
            match handler.refresh(endpoint.clone()).await {
                Ok(_) => {
                    let connection_status = handler.connection.read().await.is_some();
                    println!(
                        "Refresh successful, connection status: {} at time {:?}",
                        if connection_status {
                            "connected"
                        } else {
                            "disconnected"
                        },
                        start_time.elapsed()
                    );
                }
                Err(e) => {
                    println!("Refresh failed: {:?}", e);
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        Ok(())
    }
}
