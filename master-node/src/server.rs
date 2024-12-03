use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::sync::mpsc;
use tokio::time;
use std::sync::Arc;
use bytes::{BytesMut, Buf};
use std::error::Error;
use log::{trace, debug, info, error};
use serde_json;
use crate::proxy::{handle_client_io, handle_slave_io, ProxyManager, Client, Slave};
use crate::buffer_pool::ShardedBufferPool;
use crate::metrics::Metrics;
use crate::packet::{
    parse_header,
    build_speed_test_command,
    build_version_check_command,
    build_location_check_command
};
use crate::buffer_pool::MAX_BUF_SIZE;
use crate::utils::CLIENT_REQUEST_TIMEOUT;

const ALLOWED_SLAVE_VERSIONS: &[&str] = &["1.0.7"];

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

        // Set TCP_NODELAY
        if let Err(e) = client_stream.set_nodelay(true) {
            log::error!("Failed to set TCP_NODELAY on client socket: {}", e);
            continue;
        }

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

pub async fn handle_slave_connections(
    slave_listener: TcpListener,
    proxy_manager: Arc<ProxyManager>,
    buffer_pool: Arc<ShardedBufferPool>,
    metrics: Arc<Metrics>,
    allowed_locations: Arc<Vec<String>>,
) -> Result<(), Box<dyn Error>> {
    loop {
        let (slave_stream, slave_addr) = match slave_listener.accept().await {
            Err(e) => {
                error!("Error accepting connection: {}", e);
                return Err(Box::new(e));
            }
            Ok(p) => p,
        };

        // Set TCP_NODELAY
        if let Err(e) = slave_stream.set_nodelay(true) {
            log::error!("Failed to set TCP_NODELAY on slave socket: {}", e);
            continue;
        }

        trace!("New Slave attempting to connect: {}:{}", slave_addr.ip(), slave_addr.port());

        // Create a new Slave object
        let (new_slave, slave_rx) = Slave::new(slave_addr.ip().to_string(), slave_stream);

        // Spawn a task to handle the slave's I/O operations
        let proxy_manager_clone = Arc::clone(&proxy_manager);
        let allowed_locations_clone = Arc::clone(&allowed_locations);
        let buffer_pool_clone = Arc::clone(&buffer_pool);
        let metrics_clone = Arc::clone(&metrics);

        tokio::spawn(async move {
            let mut slave = new_slave.clone();
            
            // Perform validation
            match verify_slave_session(&mut slave, &allowed_locations_clone).await {
                Ok(_) => {
                    debug!("Slave {} validation passed.", slave.ip_addr);

                    // Add the validated slave to the proxy manager
                    proxy_manager_clone.slaves.insert(slave.ip_addr.clone(), slave.clone());
                    info!("Slave {} successfully registered.", new_slave.ip_addr);

                    // Spawn a task to handle I/O for the validated slave
                    if let Err(e) = handle_slave_io(
                        slave,
                        slave_rx,
                        proxy_manager_clone,
                        buffer_pool_clone,
                        metrics_clone,
                    )
                    .await
                    {
                        error!("Error handling IO for slave {}: {}", slave_addr.ip(), e);
                    }
                }
                Err(e) => {
                    debug!("Slave {} validation failed: {}", slave.ip_addr, e);
                }
            }
        });
    }
}

async fn verify_slave_session(
    temp_slave: &mut Slave,
    allowed_locations: &Arc<Vec<String>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Step 1: Perform Version Check
    let version_command = build_version_check_command();
    temp_slave.write_stream(&version_command).await?;
    let mut buffer = BytesMut::with_capacity(MAX_BUF_SIZE);

    if let Err(_) = time::timeout(CLIENT_REQUEST_TIMEOUT, temp_slave.read_stream(&mut buffer)).await {
        return Err("Version check response timed out".into());
    }
    let (_, _, payload_len, _) = parse_header(&buffer);

    if payload_len == 0 || buffer.len() < 10 + payload_len {
        return Err("Invalid or empty version response".into());
    }

    buffer.advance(10);
    let version = String::from_utf8(buffer.split_to(payload_len).to_vec())?;
    if !ALLOWED_SLAVE_VERSIONS.contains(&version.as_str()) {
        return Err(format!("Slave {} has unsupported version: {}", temp_slave.ip_addr, version).into());
    }
    temp_slave.set_version(version.clone());
    trace!("Slave {} version check passed: {}", temp_slave.ip_addr, version);

    // Step 2: (Optional) Perform Geolocation Check
    if !allowed_locations.is_empty() {
        let location_command = build_location_check_command(&temp_slave.ip_addr);
        temp_slave.write_stream(&location_command).await?;
        buffer.clear();

        if let Err(_) = time::timeout(CLIENT_REQUEST_TIMEOUT, temp_slave.read_stream(&mut buffer)).await {
            return Err("Location check response timed out".into());
        }
        
        let (_, _, payload_len, _) = parse_header(&buffer);

        if payload_len == 0 || buffer.len() < 10 + payload_len {
            return Err("Location check response is empty".into());
        }

        buffer.advance(10);
        let location_data = String::from_utf8(buffer.split_to(payload_len).to_vec())?;
        trace!("Received location data: {}", location_data);

        // Parse the location response
        let location: serde_json::Value = serde_json::from_str(&location_data)?;
        if let Some(country) = location["data"]["country"].as_str() {
            if !allowed_locations.iter().any(|loc| loc.eq_ignore_ascii_case(country)) {
                return Err(format!(
                    "Slave {} is in a restricted location: {}",
                    temp_slave.ip_addr, country
                )
                .into());
            }
            trace!("Slave {} location check passed: {}", temp_slave.ip_addr, country);
        } else {
            return Err("Failed to parse location response".into());
        }
    }

    // Step 3: Perform Speed Test
    let speed_test_command = build_speed_test_command("https://speed.cloudflare.com/__down?bytes=5000000");
    temp_slave.write_stream(&speed_test_command).await?;
    buffer.clear();

    if let Err(_) = time::timeout(CLIENT_REQUEST_TIMEOUT, temp_slave.read_stream(&mut buffer)).await {
        return Err("Speed test response timed out".into());
    }

    let (_, _, payload_len, _) = parse_header(&buffer);

    if payload_len == 0 || buffer.len() < 10 + payload_len {
        return Err("Invalid or empty speed test response".into());
    }

    buffer.advance(10);
    let speed_str = String::from_utf8(buffer.split_to(payload_len).to_vec())?;
    let speed = speed_str.parse::<f64>()?;
    temp_slave.set_speed(speed);
    trace!("Slave {} speed test passed: {:.2} Mbps", temp_slave.ip_addr, speed);

    Ok(())
}

