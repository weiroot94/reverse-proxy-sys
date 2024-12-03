mod conf;
mod logger;
mod server;
mod proxy;
mod buffer_pool;
mod metrics;
mod utils;
mod packet;

use conf::parse_args;
use logger::init_logging;
use server::{start_slave_listener, start_client_listener};
use std::sync::Arc;
use prometheus::Registry;
use tokio::sync::Semaphore;
use log::info;
use crate::metrics::{start_metrics_server, Metrics};
use crate::proxy::ProxyManager;
use crate::buffer_pool::ShardedBufferPool;

#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

const MAX_CONCURRENT_REQUESTS: usize = 30;
const POOL_SIZE: usize = 50;
const NUM_SHARDS: usize = 8;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let config = parse_args(); 

    init_logging(&config.verbosity);

    // Metrics
    let metrics = Arc::new(Metrics::new());
    let registry = Registry::new();
    metrics.register(&registry);

    tokio::spawn(start_metrics_server(Arc::new(registry)));

    // Proxy manager and buffer pool
    let proxy_manager = Arc::new(ProxyManager::new(config.proxy_mode));
    let slave_buffer_pool = Arc::new(ShardedBufferPool::new(NUM_SHARDS, POOL_SIZE));
    let client_buffer_pool = Arc::new(ShardedBufferPool::new(NUM_SHARDS, POOL_SIZE));

    // Start Slave listener and Client listener
    info!("Waiting for Slave nodes on {}", config.master_addr);
    start_slave_listener(
        &config.master_addr,
        Arc::clone(&proxy_manager),
        Arc::clone(&slave_buffer_pool),
        Arc::clone(&metrics),
        Arc::clone(&config.allowed_locations)
    ).await;

    log::info!("Waiting for SOCKS5 clients on {}", config.socks_addr);
    start_client_listener(
        &config.socks_addr,
        Arc::clone(&proxy_manager),
        Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS)),
        Arc::clone(&client_buffer_pool)
    ).await;

    Ok(())
}
