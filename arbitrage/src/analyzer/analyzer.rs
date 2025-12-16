use {
    crate::{
        sender::create_durable_client,
        pools::Pool,
    },
    reqwest::Client,
    tokio::runtime::Handle,
    std::sync::Arc,
    std::time::{SystemTime, UNIX_EPOCH},
    std::error::Error,
    tracing::info,
    std::time::Duration,
};


const ANALYZER_URL: &str = "http://localhost:5555";


pub struct Analyzer {
    client: Arc<Client>,
    runtime_handler: Handle,
}

impl Analyzer {
    pub fn new(runtime_handler: Handle) -> Self {
        // Create client with explicit configuration
        let client = Client::builder()
            .build()
            .expect("Failed to create HTTP client");
        
        Self { 
            client: Arc::new(client), 
            runtime_handler 
        }
    }

    pub async fn report_opportunity(
        &self,
        simulation_sol_in: u64,
        simulation_sol_out: u64,
        simulation_profit: u64,
        signature: String,
        strategy: String,
        tip_amount: u64,
        slot: u64,
        pools: &Vec<Arc<dyn Pool>>,
        landed_on_be: bool,
        landed_on_quic: bool,
        timestamp: i64,
        rtt_elapsed: Duration,
        leader_id: Option<String>,
        target_connection_rtt: Duration,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let client = self.client.clone();
        let pools_addresses = pools.iter().map(|pool| pool.get_address()).collect::<Vec<String>>();

        let transaction_data = serde_json::json!({
            "simulation_sol_in": simulation_sol_in,
            "simulation_sol_out": simulation_sol_out,
            "simulation_profit": simulation_profit,
            "tip_amount": tip_amount,
            "slot": slot,
            "strategy": strategy,
            "timestamp": timestamp,
            "landed_on_be": landed_on_be,
            "landed_on_quic": landed_on_quic,
            "rtt_elapsed": rtt_elapsed.as_millis(),
            "pools": pools_addresses,
            "signature": signature,
            "leader_id": leader_id,
            "target_connection_rtt": target_connection_rtt.as_millis(),
        });

        info!("Sending transaction data to analyzer: {:?}", transaction_data);

        
        if let Err(e) = client.post(format!("{}/transactions/entry", ANALYZER_URL))
            .json(&transaction_data)
            .send()
            .await {
            tracing::error!("Failed to send transaction data to analyzer: {:?}", e);
        }

        Ok(())
    }

    pub fn report_transaction_success(
        &self,
        signature: String,
        slot_accepted: u64,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let client = self.client.clone();

        let success_data = serde_json::json!({
            "signature": signature,
            "slot_accepted": slot_accepted
        });

        info!("Sending transaction success data to analyzer: {:?}", success_data);

        let _future = self.runtime_handler.spawn(async move {
            if let Err(e) = client.post(format!("{}/transactions/success", ANALYZER_URL))
                .json(&success_data)
                .send()
                .await {
                tracing::error!("Failed to send transaction success data to analyzer: {:?}", e);
            }
            Ok::<(), Box<dyn Error + Send + Sync>>(())
        });
        Ok(())
    }
}

