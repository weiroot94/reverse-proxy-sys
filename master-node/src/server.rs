use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::sync::mpsc;
use std::sync::Arc;
use log::error;
use crate::proxy::{handle_slave_connections, handle_client_io, ProxyManager, Client};
use crate::buffer_pool::ShardedBufferPool;
use crate::metrics::Metrics;

pub async fn start_slave_listener(
    master_addr: &str,
    proxy_manager: Arc<ProxyManager>,
    slave_buffer_pool: Arc<ShardedBufferPool>,
    metrics: Arc<Metrics>,
    allowed_locations: Arc<Vec<String>>
) {
    let slave_listener = match TcpListener::bind(&master_addr).await {
        Err(e) => {
            error!("Failed to bind slave listener: {}", e);
            return;
        }
        Ok(p) => p,
    };

    tokio::spawn({
        let proxy_manager = Arc::clone(&proxy_manager);
        let buffer_pool_clone = Arc::clone(&slave_buffer_pool);
        let metrics_clone = Arc::clone(&metrics);
        let allowed_locations = Arc::clone(&allowed_locations);
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
}

pub async fn start_client_listener(
    socks_addr: &str,
    proxy_manager: Arc<ProxyManager>,
    semaphore: Arc<Semaphore>,
    client_buffer_pool: Arc<ShardedBufferPool>
) {
    let client_listener = match TcpListener::bind(&socks_addr).await {
        Err(e) => {
            error!("Failed to bind SOCKS5 listener: {}", e);
            return;
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
            let client_stream = Arc::new(tokio::sync::Mutex::new(client_stream));
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
}
