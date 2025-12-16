pub mod transaction_sender;
pub mod decision_maker;

use std::net::{IpAddr, SocketAddr, Ipv4Addr};
use std::time::{Duration, Instant};

pub fn create_durable_client(local_addr: Option<IpAddr>) -> reqwest::Client {
    let client = reqwest::Client::builder()
            .local_address(local_addr)
            // Keep connections alive with a very long timeout
            .pool_idle_timeout(Some(std::time::Duration::from_secs(300)))
            // Maximize connection reuse
            .pool_max_idle_per_host(100)
            // Enable HTTP/2 support for better multiplexing
            .http2_prior_knowledge()
            // Enable TCP keepalive with 1 second interval
            .tcp_keepalive(Some(std::time::Duration::from_secs(1)))
            // Increase timeout values
            .connect_timeout(std::time::Duration::from_secs(300))
            // Enable TLS session resumption
            .tls_built_in_root_certs(true)
            .https_only(true)
            // Build the client
            .build()
            .expect("Failed to build reqwest client");

    client
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::runtime::Runtime;

    #[test]
    fn test_client_performance() {
        let rt = Runtime::new().unwrap();
        let test_url = "https://mainnet.block-engine.jito.wtf";
        let num_requests = 25;

        // Test with new client for each request
        let start = Instant::now();
        rt.block_on(async {
            for _ in 0..num_requests {
                let client = reqwest::Client::new();
                let _ = client.head(test_url).send().await;
            }
        });
        let new_client_duration = start.elapsed();

        // Test with durable client
        
        let durable_client = create_durable_client(None);
        let start = Instant::now();
        rt.block_on(async {
            for _ in 0..num_requests {
                let _ = durable_client.head(test_url).send().await;
            }
        });
        let durable_client_duration = start.elapsed();

        println!("Performance comparison for {} requests:", num_requests);
        println!("New client for each request: {:?}", new_client_duration);
        println!("Durable client: {:?}", durable_client_duration);
        println!("Performance improvement: {:.2}x", 
            new_client_duration.as_secs_f64() / durable_client_duration.as_secs_f64());

        assert!(durable_client_duration < new_client_duration, 
            "Durable client should be faster than creating new clients");
    }
}