use {
    arbitrage::arbitrage_client::ArbitrageClient,
    std::sync::{Arc, RwLock},
    std::thread,
    std::time::Duration,
    std::sync::atomic::{AtomicBool, AtomicU64, Ordering},
    std::collections::HashMap,
    std::error::Error,
    arbitrage::arbitrage_service::ArbitrageService,
    tracing::{info, warn, error, debug},
    tracing_appender::rolling::{RollingFileAppender, Rotation},
    tracing_subscriber::{
        fmt::Layer,
        layer::SubscriberExt,
        util::SubscriberInitExt,
        filter::EnvFilter,
    },
};

fn main() {
    // Create logs directory if it doesn't exist
    std::fs::create_dir_all("arbitrage/logs").expect("Failed to create logs directory");

    let file_appender = RollingFileAppender::new(
        Rotation::DAILY,
        "arbitrage/logs",
        "arbitrage.log",
    );
    
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    
    
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env()
            .add_directive("arbitrage=info".parse().unwrap()))
        .with(Layer::new()
            .with_writer(non_blocking)
            .with_ansi(false)
            .with_thread_ids(false)
            .with_thread_names(false)
            .with_file(false)
            .with_line_number(false)
            .with_target(false))
        .init();

    let exit = Arc::new(AtomicBool::new(false));
    let arbitrage_service = match ArbitrageService::new(exit.clone()) {
        Ok(arbitrage_service) => {
            info!("ArbitrageService successfully created");
            arbitrage_service
        },
        Err(e) => {
            error!("Failed to create ArbitrageService: {:?}", e);
            return;
        }
    };
    
    info!("Starting arbitrage service...");
    arbitrage_service.join().unwrap();
    info!("Arbitrage service terminated");
}
