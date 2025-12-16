use {
    std::collections::HashMap,
    std::error::Error,
    std::future::Future,
    std::pin::Pin,
    std::sync::Arc,
    std::sync::RwLock,
    std::sync::atomic::AtomicBool,
    std::sync::atomic::AtomicU64,
    std::sync::atomic::Ordering,
    tokio::{
        sync::RwLock as TokioRwLock,
        runtime::Handle,
    },
    std::time::Duration,
    crate::{
        spam::spammer::chose_dummy_fee_amount,
        pools::Pool,
        simulation::router::ArbitrageRoute,
        wallets::wallet::{TokenAccount, Wallet},
        analyzer::Analyzer,
        fees::fee_tracker::{FeeTracker, TippingStrategy},
        sender::{
            decision_maker::EndpointDecision,
            create_durable_client,
        },
        simulation::router::SimulationResult,
        simulation::arbitrage_simulator::JitoQueue,
        connexion_routine_service::LeaderConnexionCache,
    },
    solana_client::{
        nonblocking::rpc_client::RpcClient as AsyncRpcClient,
        rpc_client::RpcClient,
        rpc_config::RpcTransactionConfig,
    },
    solana_sdk::{
        transaction::VersionedTransaction,
        instruction::Instruction,
        clock::Slot,
        commitment_config::CommitmentConfig,
        address_lookup_table::AddressLookupTableAccount,
        hash::Hash as BlockHash,
        signature::Keypair,
    },
    crossbeam_channel::Sender,
    tracing::{info,debug, error},
    serde_json::json,
    solana_rpc_client_api::config::RpcSimulateTransactionConfig,
    std::net::{IpAddr, SocketAddr, Ipv4Addr},
    base64::{engine::general_purpose, Engine as _},
    bincode::serialize,
    std::collections::HashSet,
    std::time::{SystemTime, UNIX_EPOCH, Instant},
};

pub const MAINNET_RPC_URL: &str =
    "https://mainnet.helius-rpc.com/?api-key=";

pub const JITO_BUNDLES_URL_MAIN: &str = "https://mainnet.block-engine.jito.wtf/api/v1/bundles";
pub const JITO_BUNDLES_URL_AMS: &str = "https://amsterdam.mainnet.block-engine.jito.wtf/api/v1/bundles";
pub const JITO_BUNDLES_URL_FRA: &str = "https://frankfurt.mainnet.block-engine.jito.wtf/api/v1/bundles";
pub const JITO_BUNDLES_URL_NY: &str = "https://ny.mainnet.block-engine.jito.wtf/api/v1/bundles";
pub const JITO_BUNDLES_URL_TKY: &str = "https://tokyo.mainnet.block-engine.jito.wtf/api/v1/bundles";
pub const JITO_BUNDLES_URL_SLC: &str = "https://slc.mainnet.block-engine.jito.wtf/api/v1/bundles";
pub const JITO_BUNDLES_URL_LDN: &str = "https://london.mainnet.block-engine.jito.wtf/api/v1/bundles";

pub const FORWARD_LEADER_COUNT: usize = 1;
pub const HELIUS_STACKED_URL: &str =
    "https://mainnet.helius-rpc.com/?api-key=";

pub const LOCAL_ADDR_1: IpAddr = IpAddr::V4(Ipv4Addr::new(66, 248, 206, 125));
pub const LOCAL_ADDR_2: IpAddr = IpAddr::V4(Ipv4Addr::new(66, 248, 206, 100));
pub const LOCAL_ADDR_3: IpAddr = IpAddr::V4(Ipv4Addr::new(66, 248, 206, 80));
pub const LOCAL_ADDR_4: IpAddr = IpAddr::V4(Ipv4Addr::new(66, 248, 206, 31));
pub const LOCAL_ADDR_5: IpAddr = IpAddr::V4(Ipv4Addr::new(66, 248, 206, 75));

pub const CREATE_BUDGET: u32 = 27_000;
pub const BASE_BUDGET: u32 = 15_000;

#[derive(Clone)]
pub struct TransactionSender {
    pub analyzer: Arc<Analyzer>,
    pub jito_sender: Arc<JitoSender>,
    pub native_sender: Arc<NativeSender>,
    pub simulation_sender: Arc<SimulationSender>,
    pub runtime_handler: Handle,
    pub fee_tracker_sender: Sender<FeeTracker>,
    pub helius_rpc_client: Arc<AsyncRpcClient>,

}

//Notifier
impl TransactionSender {
    pub fn new(
        leader_connexion_cache: Arc<LeaderConnexionCache>, 
        runtime_handler: Handle, 
        analyzer: Arc<Analyzer>,
        fee_tracker_sender: Sender<FeeTracker>,

    ) -> Self {
        Self {
            analyzer,
            jito_sender: Arc::new(JitoSender::new()),
            native_sender: Arc::new(NativeSender::new(leader_connexion_cache)),
            simulation_sender: Arc::new(SimulationSender::new()),
            runtime_handler,
            fee_tracker_sender,
            helius_rpc_client: Arc::new(AsyncRpcClient::new(HELIUS_STACKED_URL.to_string())),
        }
    }

    pub fn send_duplicate_native_txs(
        &self,
        txs: Vec<Vec<VersionedTransaction>>,
        wait_time: u64,
    ) {
        let native_sender = self.native_sender.clone();

        self.runtime_handler.spawn(async move {
            // Get the maximum length of any inner vector
            let max_len = txs.iter().map(|v| v.len()).max().unwrap_or(0);
            
            // Iterate through each position
            for i in 0..max_len {
                // For each position, iterate through all vectors
                for tx_vec in txs.iter() {
                    // Only process if this vector has a transaction at this position
                    if let Some(tx) = tx_vec.get(i) {
                        let _ = native_sender.send_native_transaction(tx, 2).await;
                        
                    }
                }
                tokio::time::sleep(Duration::from_millis(wait_time)).await;
            }
        });
    }

    pub fn send_and_confirm_pre_funding(
        &self,
        tx: VersionedTransaction,
        num_retries: u8,
        wait_time: u64,
        confirmation_wait_time: Duration,
        wallets_balances: Vec<Arc<AtomicU64>>,
        amount_to_fund: u64,
    ) {
        let native_sender = self.native_sender.clone();
        let helius_rpc_client = self.helius_rpc_client.clone();
        let tx_config = RpcTransactionConfig {
            commitment: Some(CommitmentConfig::confirmed()),
            ..RpcTransactionConfig::default()
        };

        self.runtime_handler.spawn(async move {
            for _ in 0..num_retries + 1 {
                let _ = native_sender.send_native_transaction(&tx, 2).await;
                tokio::time::sleep(Duration::from_millis(wait_time)).await;
            }
            
            let start_time = Instant::now();

            loop {
                let tx_result = helius_rpc_client.get_transaction_with_config(
                    &tx.signatures[0],
                    tx_config
                ).await;
    
                match tx_result {
                    Ok(tx_result) => {
                        if tx_result.transaction.meta.unwrap().err.is_some() {
                            println!("Transaction failed");
                        }
                        else {
                            for wallet_balance in wallets_balances.iter() {
                                wallet_balance.store(amount_to_fund, Ordering::Relaxed);
                            }
                            println!("Transaction confirmed");
                            break;
                        }
                    }
                    Err(e) => {
                        tokio::time::sleep(Duration::from_millis(2_000)).await;
                    }
                }
                if start_time.elapsed() > confirmation_wait_time {
                    println!("Transaction not confirmed after 10 seconds");
                    break;
                }
            }
            
            
        });
    }

    pub fn send_swap_transaction(
        &self,
        txs: Vec<VersionedTransaction>,
        num_retries: u8,
    ) {
        let jito_sender = self.jito_sender.clone();
        self.runtime_handler.spawn(async move {
            for _ in 0..num_retries {
                for tx in txs.iter() {
                    let _ = jito_sender.send_jito_bundle(vec![tx.clone()]).await;
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        });
    }

    pub fn send_swap_transaction_native(
        &self,
        txs: Vec<VersionedTransaction>,
        num_retries: u8,
        slot: u64,
    ) {
        let native_sender = self.native_sender.clone();
        self.runtime_handler.spawn(async move {
            for _ in 0..num_retries {
                for tx in txs.iter() {
                    let _ = native_sender.send_native_transaction(tx, FORWARD_LEADER_COUNT).await;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        });
    }
    pub async fn simulate_budget_and_send(
        &self,
        opportunity: Opportunity,
        pseudo_random_index: u8,
        blockhash: BlockHash,
        slot: u64,
        leader_id: String,
        skip_simulation: bool,
        tip_wallet: Arc<Wallet>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut cu;
        let simulation_result = &opportunity.arbitrage_route.simulation_result;

        if opportunity.send_endpoint == SendEndpoint::Jito {
            cu = simulation_result.cu + opportunity.create_instructions.len() as u32 * CREATE_BUDGET + BASE_BUDGET;
        }
        else {
            cu = simulation_result.cu + opportunity.create_instructions.len() as u32 * CREATE_BUDGET + BASE_BUDGET;
        }

        
        let profit = simulation_result.profit as u64;

        
        let (signed_transaction, tip_amount) = if let Ok(result) = {
            opportunity.wallet.create_signed_arb_transaction(
                &opportunity.arbitrage_instruction,
                &opportunity.create_instructions,
                &opportunity.send_endpoint,
                blockhash,
                cu,
                profit,
                pseudo_random_index
            )
        } {
            result
        } else {
            return Ok(());
        };

        let signature = signed_transaction.signatures[0];
        let vec_pools = opportunity.arbitrage_route.route.iter().map(|(pool, _, _)| pool.clone()).collect();
        self.fast_send_transaction(
            signed_transaction, 
            &opportunity.send_endpoint, 
            &opportunity.arbitrage_route.simulation_result, 
            signature.to_string(), 
            tip_amount,
            slot, 
            pseudo_random_index, 
            &vec_pools, 
            Some(leader_id),
            opportunity.current_connection_rtt,
            tip_wallet,
            blockhash,
        ).await?;

        let fee_tracker = FeeTracker::new_from_opportunity(
            signature,
            &opportunity,
            vec_pools,
            slot
        );
        let _ = self.fee_tracker_sender.send(fee_tracker);
        
        Ok(())
    }

    /*pub async fn simulate_budget_and_send(
        &self,
        opportunity: Opportunity,
        pseudo_random_index: u8,
        blockhash: BlockHash,
        slot: u64,
        leader_id: String,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {

        let mut simulation_config = RpcSimulateTransactionConfig::default();
        let client = &self.simulation_sender.simulation_client;
        simulation_config.commitment = Some(CommitmentConfig::processed());

        let start_time = Instant::now();
        let node_simulation_result = client.simulate_transaction_with_config(
            &opportunity.simulation_transaction,
            simulation_config
        ).await;
        let simulation_duration = start_time.elapsed();
        println!("Simulation time: {:?}", simulation_duration);
        println!("node_simulation_result {:?}", node_simulation_result);
        let node_simulation_result = if let Ok(node_simulation_result) = node_simulation_result {
            node_simulation_result
        } else {
            return Err("No node simulation result".into());
        };
        let unit_budget = if let Some(units_consumed) = node_simulation_result.value.units_consumed {
            units_consumed as u32
        }
        else {
            return Err("No units consumed".into());
        };
        let profit = opportunity.arbitrage_route.simulation_result.profit as u64;

        let failed_simulation = node_simulation_result.value.err.is_some();

        println!("estimated_detection_time: {:?}", opportunity.estimated_detection_time);
        println!("current_connection_rtt: {:?}", opportunity.current_connection_rtt);

        println!("simulation slot {:?}", node_simulation_result.context.slot);
        println!("current slot {:?}", slot);

        if failed_simulation {

            if opportunity.send_endpoint == SendEndpoint::Jito {
                println!("Jito simulation failed, not supposed. sent both : {}", opportunity.is_valid_jito_tx.is_some());
                return Ok(());
            }
            else {
                println!("Native simulation failed, as supposed. sent both : {}", opportunity.is_valid_jito_tx.is_some());
            }
        } else {
            if opportunity.send_endpoint == SendEndpoint::Jito {
                println!("Jito simulation succeeded, as supposed. sent both : {}", opportunity.is_valid_jito_tx.is_some());
            }
            else {
                println!("Native simulation succeeded, not supposed. sent both : {}", opportunity.is_valid_jito_tx.is_some());
            }
        }

        return Ok(());

        
        let (signed_transaction, tip_amount) = if let Ok(result) = {
            let wallet = opportunity.wallet.read().unwrap();
            wallet.create_signed_arb_transaction(
                &opportunity.arbitrage_instruction,
                &opportunity.create_instructions,
                &opportunity.send_endpoint,
                blockhash,
                unit_budget * 110/100,
                profit,
                pseudo_random_index
            )
        } {
            result
        } else {
            return Ok(());
        };

        let signature = signed_transaction.signatures[0];
        let vec_pools = opportunity.arbitrage_route.route.iter().map(|(pool, _, _)| pool.clone()).collect();
        self.fast_send_transaction(
            signed_transaction, 
            &opportunity.send_endpoint, 
            &opportunity.arbitrage_route.simulation_result, 
            signature.to_string(), 
            tip_amount,
            slot, 
            pseudo_random_index, 
            &vec_pools, 
            Some(leader_id),
            opportunity.current_connection_rtt,
        ).await?;

        let fee_tracker = FeeTracker::new_from_opportunity(
            signature,
            &opportunity,
            vec_pools,
            slot
        );
        let _ = self.fee_tracker_sender.send(fee_tracker);
        
        Ok(())
    }*/


    pub async fn fast_send_transaction(
        &self,
        versioned_tx: VersionedTransaction,
        send_endpoint: &SendEndpoint,
        simulation_result: &SimulationResult,
        signature: String,
        tip_amount: u64,
        slot: u64,
        pseudo_random_index: u8,
        pools: &Vec<Arc<dyn Pool>>,
        leader_id: Option<String>,
        target_connection_rtt: Duration,
        tip_wallet: Arc<Wallet>,
        blockhash: BlockHash,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let sol_in = simulation_result.sol_in;
        let sol_out = simulation_result.sol_out;
        let profit = simulation_result.profit as u64; 
        let send_endpoint_string = send_endpoint.to_string();

        let analyzer = self.analyzer.clone();

        match send_endpoint {
            SendEndpoint::Jito => {
                let jito_sender = self.jito_sender.clone();

                let rtt_start = Instant::now();
                let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
                let landed_on_be = jito_sender.send_duplicate_jito_tx(
                    versioned_tx,
                    tip_amount,
                    pseudo_random_index,
                    tip_wallet,
                    blockhash
                ).await.is_ok();
                let rtt_elapsed = rtt_start.elapsed();
                let _ = analyzer.report_opportunity(
                    sol_in,
                    sol_out,
                    profit,
                    signature,
                    send_endpoint_string,
                    tip_amount,
                    slot, 
                    pools,
                    landed_on_be,
                    false,
                    timestamp,
                    rtt_elapsed,
                    leader_id,
                    target_connection_rtt,
                ).await;
            }
            SendEndpoint::Native(_tipping_strategy) => {
                let native_sender = self.native_sender.clone();
                let versioned_tx = versioned_tx.clone();
                let rtt_start = Instant::now();
                let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
                let landed_on_quic = native_sender.send_native_transaction(&versioned_tx, 1).await.is_ok();
                let rtt_elapsed = rtt_start.elapsed();
                let _ = analyzer.report_opportunity(
                    sol_in,
                    sol_out,
                    profit,
                    signature,
                    send_endpoint_string,
                    tip_amount,
                    slot, 
                    pools,
                    false,
                    landed_on_quic,
                    timestamp,
                    rtt_elapsed,
                    leader_id,
                    target_connection_rtt,
                ).await;
            }
        }

        Ok(())
    }

}



pub struct JitoSender {
    pub clients: Vec<JitoClient>,
    pub urls: Vec<String>,

}

impl JitoSender {
    pub fn new() -> Self {
        let local_client_1 = JitoClient::new(LOCAL_ADDR_1);
        let local_client_2 = JitoClient::new(LOCAL_ADDR_2);
        let local_client_3 = JitoClient::new(LOCAL_ADDR_3);
        let local_client_4 = JitoClient::new(LOCAL_ADDR_4);
        let local_client_5 = JitoClient::new(LOCAL_ADDR_5);
        let clients = vec![local_client_1, local_client_2, local_client_3, local_client_4, local_client_5];
        let urls = vec![
            JITO_BUNDLES_URL_MAIN.to_string(),
            JITO_BUNDLES_URL_AMS.to_string(),
            JITO_BUNDLES_URL_FRA.to_string(),
            JITO_BUNDLES_URL_NY.to_string(),
            JITO_BUNDLES_URL_TKY.to_string(),
            JITO_BUNDLES_URL_SLC.to_string(),
            JITO_BUNDLES_URL_LDN.to_string(),
        ];
        Self {
            clients,
            urls,
        }
    }

    fn get_client(&self, pseudo_random_index: u8) -> &JitoClient {
        &self.clients[pseudo_random_index as usize % self.clients.len()]
    }

    async fn get_first_available_client(&self) -> Option<&JitoClient> {
        let now = Instant::now();
        let mut best_client = None;
        let mut best_duration = Duration::from_millis(1000);
        for client in &self.clients {
            let last_used_timestamp = client.last_used_timestamp.read().await;
            let duration = now.duration_since(*last_used_timestamp);
            if duration > best_duration {
                best_duration = duration;
                best_client = Some(client);
            }
        }
        best_client
    }

    pub async fn send_spam_bundle(
        &self,
        transactions: Vec<VersionedTransaction>,
    ) -> String {
        let _result_str = match tokio::time::timeout(
            Duration::from_millis(200),
            self.send_jito_bundle(transactions)
        ).await {
            Ok(result) => {
                match result {
                    Ok(text) => {
                        println!("result_str {:?}", text);
                        text
                    },
                    Err(e) => {
                        match e {
                            JitoClientError::NoClientAvailable => {
                                println!("No client available");
                                String::new()
                            }
                            JitoClientError::RateLimited((ip, last_used_timestamp)) => {
                                println!("Rate limited on {}, last used timestamp: {}s", ip, Instant::now().duration_since(last_used_timestamp).as_secs());
                                String::new()
                            }
                            JitoClientError::NoBEWentThrough((ip, last_used_timestamp)) => {
                                println!("No BE went through on {}, last used timestamp: {}s", ip, Instant::now().duration_since(last_used_timestamp).as_secs());
                                String::new()
                            }
                            _ => {
                                println!("Other error");
                                String::new()
                            }
                        }
                    }
                }
            }
            Err(_) => {
                println!("Timeout");
                String::new()
            }
        };
        _result_str
    }

    pub async fn send_duplicate_jito_tx(
        &self,
        main_tx: VersionedTransaction,
        tip_amount: u64,
        pseudo_random_index: u8,
        tip_payer: Arc<Wallet>,
        blockhash: BlockHash,
    ) -> Result<String, JitoClientError> {
        let client = self.get_first_available_client().await;
        let be_length = self.urls.len();
        let mut requests = Vec::with_capacity(be_length);

        if let Some(client) = client {
            for _i in 0..be_length {
                let tip_tx = 
                    tip_payer.create_and_sign_jito_tip_tx(
                        chose_dummy_fee_amount(tip_amount - 1_000, tip_amount), 
                        pseudo_random_index,
                        blockhash
                    ).unwrap();

                let mut bundle_transactions = Vec::with_capacity(2);

                let jito_tip_tx_wire = serialize(&tip_tx).unwrap();
                let wire_tx = serialize(&main_tx).unwrap();

                let jito_tip_tx_encoded = general_purpose::STANDARD.encode(&jito_tip_tx_wire);
                let wire_tx_encoded = general_purpose::STANDARD.encode(&wire_tx);

                bundle_transactions.push(jito_tip_tx_encoded);
                bundle_transactions.push(wire_tx_encoded);
                
                let request = Arc::new(json!({
                    "id": 1,
                    "jsonrpc": "2.0",
                    "method": "sendBundle",
                    "params": [
                        bundle_transactions,
                        {
                            "encoding": "base64"
                        }
                    ]
                }));
                requests.push(request);
            }
           

            let result = client.send_duplicate_bundle_to_block_engines(
                requests, &self.urls
            ).await;
            match result {
                Ok(text) => Ok(text),
                Err(e) => Err(e),
            }
        } else {
            Err(JitoClientError::NoClientAvailable)
        }  
    }

    pub async fn send_jito_bundle(
        &self, 
        transactions: Vec<VersionedTransaction>,
    ) -> Result<String, JitoClientError> {
        let client = self.get_first_available_client().await;

        
        if let Some(client) = client {
            let mut bundle_transactions = Vec::with_capacity(transactions.len());
            for tx in transactions {
                let wire_tx = serialize(&tx).unwrap();
                let encoded_tx = general_purpose::STANDARD.encode(&wire_tx);
                bundle_transactions.push(encoded_tx);
            }
            let request = Arc::new(json!({
                "id": 1,
                "jsonrpc": "2.0",
                "method": "sendBundle",
                "params": [
                    bundle_transactions,
                    {
                        "encoding": "base64"
                    }
                ]
            }));

            let result = client.send_bundle_to_block_engines(request, &self.urls).await;
            match result {
                Ok(text) => Ok(text),
                Err(e) => Err(e),
            }
        } else {
            Err(JitoClientError::NoClientAvailable)
        }   
    }

    pub async fn send_jito_transaction(
        &self,
        tx: &VersionedTransaction,
        jito_sender: Arc<JitoSender>,
        pseudo_random_index: u8,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let client = self.get_client(pseudo_random_index);
        let wire_tx = serialize(tx).unwrap();
        let encoded_tx = general_purpose::STANDARD.encode(&wire_tx);
        let mut bundle_transactions = Vec::with_capacity(1);

        bundle_transactions.push(encoded_tx);

        let request = Arc::new(json!({
            "id": 1,
            "jsonrpc": "2.0",
            "method": "sendBundle",
            "params": [
                bundle_transactions,
                {
                    "encoding": "base64"
                }
            ]
        }));

        
        //return Ok(()); //TOREMOVE

        // Create futures for HTTP requests
        let mut all_futures: Vec<Pin<Box<dyn Future<Output = Result<(), Box<dyn Error + Send + Sync>>> + Send>>> = Vec::new();
        

        for url in &self.urls {
            let request_clone = request.clone();
            let future: Pin<Box<dyn Future<Output = Result<(), Box<dyn Error + Send + Sync>>> + Send>> = 
                Box::pin(async move {
                    match JitoClient::send_request(url, client.client.clone(), request_clone).await {
                        Ok(response) => {
                            match response.text().await {
                                Ok(text) => {
                                    if text.contains("Rate limit exceeded") {
                                        error!("Rate limited on {}", url);
                                        return Err("Rate limited".into());
                                    }
                                    if url == JITO_BUNDLES_URL_AMS {
                                        info!("Signature {}: {}", tx.signatures[0], text);
                                    }
                                    Ok(())
                                },
                                Err(e) => {
                                    error!("Error getting response text from {}: {}", url , e);
                                    Ok(())
                                }
                            }
                        }
                        Err(e) => {
                            error!("Error sending to {}: {}", url , e);
                            Ok(())
                        }
                    }
                });
            all_futures.push(future);
        }

        // Wait for all futures to complete and check results
        let results = futures::future::join_all(all_futures).await;
        
        // Check if any request was rate limited
        if results.iter().any(|r| r.is_err()) {
            return Err("One or more requests were rate limited".into());
        }

        Ok(())
    }
}

#[derive(Debug)]
pub enum JitoClientError {
    RateLimited((IpAddr, Instant)),
    ResponseTextError,
    NoBEWentThrough((IpAddr, Instant)),
    NoClientAvailable,
}

unsafe impl Send for JitoClientError {}
unsafe impl Sync for JitoClientError {}

pub struct JitoClient {
    pub client: Arc<reqwest::Client>,
    pub local_addr: IpAddr,
    pub last_used_timestamp: TokioRwLock<Instant>,
}

impl JitoClient {
    pub fn new(local_addr: IpAddr) -> Self {

        let client = create_durable_client(Some(local_addr));
        Self { 
            client: Arc::new(client),
            local_addr,
            last_used_timestamp: TokioRwLock::new(Instant::now() - Duration::from_secs(1000)),
        }
    }

    pub async fn send_request(url: &str, client: Arc<reqwest::Client>, payload: Arc<serde_json::Value>) -> Result<reqwest::Response, reqwest::Error> {
        client
            .post(url)
            .json(&payload)
            .send()
            .await
    }


    pub async fn send_duplicate_bundle_to_block_engines(
        &self,
        payload: Vec<Arc<serde_json::Value>>,
        be_urls: &Vec<String>,
    ) -> Result<String, JitoClientError> {
        let client = &self.client;
        let mut all_futures: Vec<Pin<Box<dyn Future<Output = Result<String, JitoClientError>> + Send>>> = Vec::new();

        // Iterate through bundles and urls together, matching them 1:1
        for (i, request) in payload.iter().enumerate() {
            // Only process if we have a corresponding URL
            if let Some(url) = be_urls.get(i) {
                let request_clone = request.clone();
                let client_clone = client.clone();
                let url_clone = url.clone();
                
                let future: Pin<Box<dyn Future<Output = Result<String, JitoClientError>> + Send>> = 
                    Box::pin(async move {
                        match JitoClient::send_request(&url_clone, client_clone, request_clone).await {
                            Ok(response) => {
                                match response.text().await {
                                    Ok(text) => {
                                        println!("jito response from {}: {}", url_clone, text);
                                        if text.contains("Rate limit exceeded") {
                                            error!("Rate limited on {}", url_clone);
                                            return Err(JitoClientError::RateLimited((self.local_addr, self.last_used_timestamp.read().await.clone())));
                                        }
                                        Ok(text)
                                    },
                                    Err(e) => {
                                        error!("Error getting response text from {}: {}", url_clone, e);
                                        return Err(JitoClientError::ResponseTextError);
                                    }
                                }
                            }
                            Err(e) => {
                                error!("Error sending to {}: {}", url_clone, e);
                                return Err(JitoClientError::ResponseTextError);
                            }
                        }
                    });
                all_futures.push(future);
            }
        }

        // Wait for all futures to complete and check results
        let results = futures::future::join_all(all_futures).await;
        
        // Check if any request went through
        if let Some(result) = results.iter().find(|r| r.is_ok()) {
            let mut last_used_timestamp = self.last_used_timestamp.write().await;
            *last_used_timestamp = Instant::now();
            return Ok(result.as_ref().unwrap().clone());
        }

        Err(JitoClientError::NoBEWentThrough((self.local_addr, self.last_used_timestamp.read().await.clone())))
    }
    
    pub async fn send_bundle_to_block_engines(
        &self,
        payload: Arc<serde_json::Value>,
        be_urls: &Vec<String>,
    ) -> Result<String, JitoClientError> {
        let client = &self.client;
        let mut all_futures: Vec<Pin<Box<dyn Future<Output = Result<String, JitoClientError>> + Send>>> = Vec::new();

        for url in be_urls {
            let request_clone = payload.clone();
            let client_clone = client.clone();
            let future: Pin<Box<dyn Future<Output = Result<String, JitoClientError>> + Send>> = 
                Box::pin(async move {
                    match JitoClient::send_request(url, client_clone, request_clone).await {
                        Ok(response) => {
                            match response.text().await {
                                Ok(text) => {
                                    println!("jito response:{}", text);
                                    if text.contains("Rate limit exceeded") {
                                        error!("Rate limited on {}", url);
                                        return Err(JitoClientError::RateLimited((self.local_addr, self.last_used_timestamp.read().await.clone())));
                                    }
                                    Ok(text)
                                },
                                Err(e) => {
                                    error!("Error getting response text from {}: {}", url , e);
                                    return Err(JitoClientError::ResponseTextError);
                                }
                            }
                        }
                        Err(e) => {
                            error!("Error sending to {}: {}", url , e);
                            return Err(JitoClientError::ResponseTextError);
                        }
                    }
                });
            all_futures.push(future);
        }

        // Wait for all futures to complete and check results
        let results = futures::future::join_all(all_futures).await;
        
        // Check if any request went through
        if let Some(result) = results.iter().find(|r| r.is_ok()) {
            let mut last_used_timestamp = self.last_used_timestamp.write().await;
            *last_used_timestamp = Instant::now();
            return Ok(result.as_ref().unwrap().clone());
        }

        Err(JitoClientError::NoBEWentThrough((self.local_addr, self.last_used_timestamp.read().await.clone())))
    }
}


pub struct SimulationSender {
    pub simulation_client: AsyncRpcClient,
}

impl SimulationSender {
    pub fn new() -> Self {
        Self { simulation_client: AsyncRpcClient::new("http://localhost:8899".to_string()) }
    }

    pub async fn send_simulation_transaction(
        &self, 
        tx: &VersionedTransaction
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        //tokio::time::sleep(Duration::from_millis(600)).await;
        let mut simulation_config = RpcSimulateTransactionConfig::default();
        simulation_config.commitment = Some(CommitmentConfig::processed());
        simulation_config.sig_verify = true;
        
            
        let simulation_result = 
            self.simulation_client.simulate_transaction_with_config(
                tx,
                simulation_config
            ).await?;
        println!("simulation result {:?}", simulation_result);
        let mut should_print = true;
        //let mut filtered_logs = Vec::new();
        if let Some(logs) = &simulation_result.value.logs {
            if logs.len() == 0 {
                error!("No logs found {:?}", &simulation_result);
            }
            println!("logs {:?}", &logs);
            
            /*for log in logs {
                if log.contains("error") || log.contains("Buy") || log.contains("VA") || log.contains("round") {
                    filtered_logs.push(log.clone());
                }
                if log.contains("NA") {
                    should_print = false;
                }
            }*/
        }
        else {
            error!("No logs found {:?}", simulation_result);
        }
        /*if should_print {
            info!("logs {:?}", filtered_logs);
        }*/

        Ok(())
    }
}


pub struct NativeSender {
    pub leader_connexion_cache: Arc<LeaderConnexionCache>,
    pub helius_client: reqwest::Client,
}

impl NativeSender {
    pub fn new(leader_connexion_cache: Arc<LeaderConnexionCache>) -> Self {
        let helius_client = create_durable_client(Some(LOCAL_ADDR_1));
        Self { 
            leader_connexion_cache,
            helius_client,
        }
    }

    pub async fn send_native_transaction(
        &self,
        tx: &VersionedTransaction,
        num_forward_leaders: usize,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let connexions = self.leader_connexion_cache.get_next_leaders_connexions(num_forward_leaders).await?;

        for connexion in connexions {
            let connexion = &connexion.handler;
            if let Some(conn) = connexion {
                let guard = conn.connection.read().await;
                let connexion = guard.as_ref();
                match connexion {
                    Some(connexion) => {
                        
                        let wire_tx = serialize(tx)?;
                        println!(
                                "send_arb : sending transaction to {}",
                                connexion.remote_address()
                            );
                        //return Ok(()); //TODO remove when testing is done
                        let mut stream = connexion.open_uni().await?;
                        let _ = stream.write_all(&wire_tx).await;
                        let _ = stream.finish();
                        
                    }
                    None => {
                        error!("Missing connection in connexion handler, sending to helius");
                        
                        let wire_tx = serialize(tx).unwrap();
                        self.send_helius_transaction(wire_tx.to_vec()).await?;
                    }
                }
            } else {
                error!("Missing connection handler, sending to helius");
                let wire_tx = serialize(tx).unwrap();
                self.send_helius_transaction(wire_tx.to_vec()).await?;
            }
        }
        Ok(())
    }

    pub async fn send_helius_transaction(&self, wire_tx: Vec<u8>) -> Result<(), Box<dyn Error + Send + Sync>> {
        let client = &self.helius_client;
        let encoded_tx = general_purpose::STANDARD.encode(&wire_tx);

        let params = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendTransaction",
            "params": [
                encoded_tx,
                {
                    "skipPreflight": true
                }
            ]
        });

        let client = client.clone();
        let response = client.post(HELIUS_STACKED_URL).json(&params).send().await;

        debug!("Sent helius transaction response: {:?}", response);
        Ok(())
    }
}

#[derive(Eq, PartialEq, Debug, Clone)]
pub enum LeaderTiming{
    Current,
    Next,
}

impl std::fmt::Display for LeaderTiming {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LeaderTiming::Current => write!(f, "Current"),
            LeaderTiming::Next => write!(f, "Next"),
        }
    }
}

#[derive(Eq, PartialEq, Clone)]
pub enum SendEndpoint{
    Native(TippingStrategy),
    Jito,
}

impl std::fmt::Display for SendEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendEndpoint::Native(tipping_strategy) => write!(f, "Native({:?})", tipping_strategy),
            SendEndpoint::Jito => write!(f, "Jito"),
        }
    }
}

pub struct Opportunity {
    pub arbitrage_route: ArbitrageRoute,
    pub ata_states: HashSet<Arc<TokenAccount>>,
    pub arbitrage_instruction: Instruction,
    pub create_instructions: Vec<Instruction>,
    pub simulation_transaction: VersionedTransaction,
    pub send_endpoint: SendEndpoint,
    pub wallet: Arc<Wallet>,
    pub is_valid_jito_tx: Option<Arc<AtomicBool>>,
    pub estimated_detection_time: Duration,
    pub current_connection_rtt: Duration,
}

impl std::fmt::Display for Opportunity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sim_result = &self.arbitrage_route.simulation_result;
        writeln!(f, "Transaction: SOL in: {}, SOL out: {}, Profit: {}", 
            sim_result.sol_in, 
            sim_result.sol_out, 
            sim_result.profit)?;
        
        writeln!(f, "Route:")?;
        for (pool, _,_) in &self.arbitrage_route.route {
            writeln!(f, "  Pool: {} Type: {}", pool.get_address(), pool.get_pool_type())?;
        }
        
        Ok(())
    }
}

impl Opportunity {
    
    pub fn new_with_send_endpoint(
        send_endpoint: SendEndpoint,
        arbitrage_route: ArbitrageRoute,
        ata_states: HashSet<Arc<TokenAccount>>,
        arbitrage_instruction: Instruction,
        create_instructions: Vec<Instruction>,
        simulation_transaction: VersionedTransaction,
        wallet: Arc<Wallet>,
        is_valid_jito_tx: Option<Arc<AtomicBool>>,
        estimated_detection_time: Duration,
        current_connection_rtt: Duration,
    ) -> Self {
        Self {
            arbitrage_route,
            ata_states,
            arbitrage_instruction,
            create_instructions,
            simulation_transaction,
            send_endpoint,
            wallet,
            is_valid_jito_tx,
            estimated_detection_time,
            current_connection_rtt,
        }
    }

    pub fn new_from_decision(
        next_slot: u64,
        jito_opportunity_queue: &mut JitoQueue,
        decision: EndpointDecision,
        arbitrage_route: ArbitrageRoute,
        ata_states: HashSet<Arc<TokenAccount>>,
        arbitrage_instruction: Instruction,
        create_instructions: Vec<Instruction>,
        simulation_transaction: VersionedTransaction,
        tipping_strategy: TippingStrategy,
        estimated_detection_time: Duration,
        current_connection_rtt: Duration,
        wallet: Arc<Wallet>,
    ) -> Option<Self> {


        match decision {
            EndpointDecision::Discard => {
                return None;
            }
            EndpointDecision::QueueJito => {
                let is_valid_jito_tx = Arc::new(AtomicBool::new(false));
                return Some(
                    Opportunity::new_with_send_endpoint(
                        SendEndpoint::Jito,
                        arbitrage_route,
                        ata_states,
                        arbitrage_instruction,
                        create_instructions,
                        simulation_transaction,
                        wallet,
                        Some(is_valid_jito_tx.clone()),
                        estimated_detection_time,
                        current_connection_rtt,
                    )
                )
            }
            EndpointDecision::Native => {
                return Some(Opportunity::new_with_send_endpoint(
                    SendEndpoint::Native(tipping_strategy),
                    arbitrage_route,
                    ata_states,
                    arbitrage_instruction,
                    create_instructions,
                    simulation_transaction,
                    wallet,
                    None,
                    estimated_detection_time,
                    current_connection_rtt,
                ));
            }
            EndpointDecision::Both => {

                let is_valid_jito_tx = Arc::new(AtomicBool::new(false));
                jito_opportunity_queue.add(next_slot, Opportunity::new_with_send_endpoint(
                    SendEndpoint::Jito,
                    arbitrage_route.clone(),
                    ata_states.clone(),
                    arbitrage_instruction.clone(),
                    create_instructions.clone(),
                    simulation_transaction.clone(),
                    wallet.clone(),
                    Some(is_valid_jito_tx.clone()),
                    estimated_detection_time,
                    current_connection_rtt,
                ));
                return Some(Opportunity::new_with_send_endpoint(
                    SendEndpoint::Native(tipping_strategy),
                    arbitrage_route,    
                    ata_states,
                    arbitrage_instruction,
                    create_instructions,
                    simulation_transaction,
                    wallet,
                    Some(is_valid_jito_tx),
                    estimated_detection_time,
                    current_connection_rtt,
                ));
            }
            EndpointDecision::NativeNextLeader => {
                return Some(Opportunity::new_with_send_endpoint(
                    SendEndpoint::Native(tipping_strategy),
                    arbitrage_route,
                    ata_states,
                    arbitrage_instruction,
                    create_instructions,
                    simulation_transaction,
                    wallet,
                    None,
                    estimated_detection_time,
                    current_connection_rtt,
                ));
            }

        }
        

    }
}