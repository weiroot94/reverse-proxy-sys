mod metrics;
mod buffer_pool;
mod proxy;
mod packet;
mod utils;

use crate::metrics::{start_metrics_server, Metrics};
use crate::buffer_pool::ShardedBufferPool;
use crate::proxy::{
    ProxyManager,
    Client,
    handle_slave_connections,
    handle_client_io
};
use crate::utils::{usage, MAX_CONCURRENT_REQUESTS};

use dashmap::DashMap;
use log::{error, info, LevelFilter};
use simple_logger::SimpleLogger;
use getopts::Options;
use std::sync::Arc;
use prometheus::Registry;
use tokio::{
    net::TcpListener,
    sync::{Semaphore, Mutex as AsyncMutex, mpsc},
    io,
};
use console_subscriber;

const POOL_SIZE: usize = 50;
const NUM_SHARDS: usize = 8;

#[tokio::main]
async fn main() -> io::Result<()>  {
    console_subscriber::init();

    let args: Vec<String> = std::env::args().collect();
    let program = args[0].clone();

    let mut opts = Options::new();

    opts.optopt("t",
                "transfer",
                "The address accept from slave socks5 server connection",
                "TRANSFER_ADDRESS");

    opts.optopt("s",
                "server",
                "The address on which to listen local socks5 server",
                "TRANSFER_ADDRESS");
    
    opts.optopt("p",
                "proxy_mode",
                "Set the proxy mode: stick (1) or nonstick (2)",
                "MODE");
    
    opts.optopt(
                "l",
                "allowed-locations",
                "Comma-separated list of allowed countries for slaves",
                "LOCATIONS",
                );

    opts.optopt("v",
                "verbosity",
                "Set the verbosity level (trace, debug, info, warn, error)",
                "LEVEL");

    let matches = opts.parse(&args[1..]).unwrap_or_else(|_| {
        usage(&program, &opts);
        std::process::exit(-1);
    });

    // Parse proxy_mode (stick or nonstick)
    let proxy_mode: String = matches.opt_str("p").unwrap_or_else(|| "stick".to_string());
    let client_assign_mode: u8 = match proxy_mode.as_str() {
        "stick" => 1,
        "nonstick" => 2,
        _ => {
            error!("Invalid proxy mode. Using default (nonstick).");
            2
        }
    };

    // Parse the allowed locations
    let allowed_locations = Arc::new(
        matches
            .opt_str("l")
            .unwrap_or_default()
            .split(',')
            .filter_map(|loc| {
                let trimmed = loc.trim();
                if !trimmed.is_empty() {
                    Some(trimmed.to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<String>>(),
    );

    // Determine the verbosity level
    let verbosity = matches.opt_str("v").unwrap_or_else(|| "info".to_string());

    let level_filter = match verbosity.as_str() {
        "trace" => LevelFilter::Trace,
        "debug" => LevelFilter::Debug,
        "info" => LevelFilter::Info,
        "warn" => LevelFilter::Warn,
        "error" => LevelFilter::Error,
        _ => {
            eprintln!("Invalid verbosity level specified. Defaulting to 'info'.");
            LevelFilter::Info
        }
    };

    // Initialize logging system with the specified verbosity level
    SimpleLogger::new()
        .with_utc_timestamps()
        .with_colors(true)
        .with_level(level_filter) // Set the verbosity level
        .init()
        .unwrap();
    
    ::log::set_max_level(level_filter);

    // Measuring metrics
    let metrics = Arc::new(Metrics::new());
    let registry = Registry::new();
    metrics.register(&registry);

    tokio::spawn(start_metrics_server(Arc::new(registry)));

    // Global store of proxy system
    let proxy_manager = Arc::new(ProxyManager {
        client_assign_mode,
        slaves: Arc::new(DashMap::new()),
        clients: Arc::new(DashMap::new()),
        cli_ip_to_slave: Arc::new(DashMap::new()),
    });

    // Initialize the semaphore for rate limiting
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS));

    // Initialize buffer pool
    let slave_buffer_pool = Arc::new(ShardedBufferPool::new(NUM_SHARDS, POOL_SIZE));
    let client_buffer_pool = Arc::new(ShardedBufferPool::new(NUM_SHARDS, POOL_SIZE));

    if matches.opt_count("t") > 0 {
        let master_addr: String = matches.opt_str("t").unwrap_or_else(|| {
            error!("Not found listen port. eg: net-relay -t 0.0.0.0:8000 -s 0.0.0.0:1080");
            std::process::exit(1);
        });

        let socks_addr: String = matches.opt_str("s").unwrap_or_else(|| {
            error!("Not found listen port. eg: net-relay -t 0.0.0.0:8000 -s 0.0.0.0:1080");
            std::process::exit(1);
        });

        info!("Waiting for Slave nodes on {}", master_addr);

        let slave_listener = match TcpListener::bind(&master_addr).await {
            Err(e) => {
                error!("Failed to bind slave listener: {}", e);
                return Ok(());
            }
            Ok(p) => p,
        };

        tokio::spawn({
            let proxy_manager = Arc::clone(&proxy_manager);
            let buffer_pool_clone = Arc::clone(&slave_buffer_pool);
            let metrics_clone = Arc::clone(&metrics);
            let allowed_locations = allowed_locations.clone();
            async move {
                if let Err(e) = handle_slave_connections(
                    slave_listener,
                    proxy_manager,
                    buffer_pool_clone,
                    metrics_clone,
                    allowed_locations,
                ).await {
                    error!("Connection handler error: {}", e);
                }
            }
        });

        log::info!("Waiting for SOCKS5 clients on {}", socks_addr);

        let client_listener = match TcpListener::bind(&socks_addr).await {
            Err(e) => {
                error!("Failed to bind SOCKS5 listener: {}", e);
                return Ok(());
            }
            Ok(p) => p,
        };

        loop {
            let (client_stream, client_addr) = match client_listener.accept().await {
                Ok(stream) => stream,
                Err(e) => {
                    error!("Error accepting client connection: {}", e);
                    continue; // Continue the loop on error
                }
            };

            // Retrieve an available slave stream
            let client_ip = client_addr.ip().to_string();
            if let Some(slave_tx) = proxy_manager.get_available_slave_tx(&client_ip).await {
                let client_stream = Arc::new(AsyncMutex::new(client_stream));
			    let session_id = rand::random::<u32>();

                let (client_tx, client_rx) = mpsc::channel(100);
                let client = Client::new(client_stream.clone(), client_tx);
    
                // Spawn a task to handle traffic between the client and the assigned slave
                let proxy_manager_clone = Arc::clone(&proxy_manager);
                let semaphore_clone = Arc::clone(&semaphore);
                let buffer_pool_clone = Arc::clone(&client_buffer_pool);
                tokio::spawn(handle_client_io(
                    session_id,
                    client,
                    slave_tx,
                    client_rx,
                    proxy_manager_clone,
                    semaphore_clone,
                    buffer_pool_clone,
                ));
            }
        }
    } else {
        usage(&program, &opts);
    }
    Ok(())
}
