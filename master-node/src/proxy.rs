use crate::buffer_pool::{ShardedBufferPool, MAX_BUF_SIZE};
use crate::metrics::Metrics;
use crate::packet::{
    parse_header,
    process_packet,
    build_speed_test_command,
    build_heartbeat_command,
    build_version_check_command,
    build_location_check_command,
    build_close_session_command,
    build_data_frame
};
use crate::utils::{hash_ip, CLIENT_REQUEST_TIMEOUT};

use dashmap::DashMap;
use std::error::Error;
use log::{trace, debug, info, warn, error};
use tokio::{io::{AsyncWriteExt, AsyncReadExt}, net::{TcpListener, TcpStream}};
use tokio::time::{self, Duration, Instant, timeout};
use std::sync::Arc;
use tokio::sync::{Mutex as AsyncMutex, mpsc, Semaphore};
use bytes::{BytesMut, Bytes, Buf};
use serde_json;

const KEEP_ALIVE_DURATION: u64 = 10;
const ALLOWED_SLAVE_VERSIONS: &[&str] = &["1.0.7"];

#[derive(Clone)]
pub struct Slave {
    pub ip_addr: String,
    version: Option<String>,
    // Weight for round robin
    net_speed: f64,
    stream: Arc<AsyncMutex<TcpStream>>,
    // Sender to receive data from clients
    tx: mpsc::Sender<(u32, Bytes)>,
}

impl Slave {
    pub fn new(ip_addr: String, stream: TcpStream) -> (Self, mpsc::Receiver<(u32, Bytes)>) {
        let (tx, rx) = mpsc::channel(500);
        let slave = Self {
            ip_addr,
            version: None,
            net_speed: 0.0,
            stream: Arc::new(AsyncMutex::new(stream)),
            tx,
        };
        (slave, rx)
    }

    // Helper method to read from the slave stream
    pub async fn read_stream(&self, buffer: &mut BytesMut) -> Result<usize, std::io::Error> {
        let mut slave_stream = self.stream.lock().await;
        slave_stream.read_buf(buffer).await
    }

    // Helper method to write to the slave stream
    pub async fn write_stream(&self, frame: &Bytes) -> Result<(), std::io::Error> {
        let mut stream = self.stream.lock().await;
        stream.write_all(frame).await?;
        stream.flush().await?;
        Ok(())
    }

    pub fn set_version(&mut self, version: String) {
        self.version = Some(version);
    }

    pub fn set_speed(&mut self, speed: f64) {
        self.net_speed = speed;
    }
}

#[derive(Clone)]
pub struct Client {
    stream: Arc<AsyncMutex<TcpStream>>,
    to_client_tx: mpsc::Sender<Bytes>,
}

impl Client {
    pub fn new(stream: Arc<AsyncMutex<TcpStream>>, to_client_tx: mpsc::Sender<Bytes>) -> Self {
        Self {
            stream,
            to_client_tx,
        }
    }
}
pub struct ProxyManager {
    // Mode how master node works
    pub client_assign_mode: u8, // 1 or 2

    // Slaves and clients
    pub slaves: Arc<DashMap<String, Slave>>,
    pub clients: Arc<DashMap<u32, Client>>,

    // Map client IP addresses to slave IPs in Client Assignment Mode 1
    pub cli_ip_to_slave: Arc<DashMap<String, String>>,
}

impl ProxyManager {
    // Get tx of avaiable Slave using weighted round-robin
    pub async fn get_available_slave_tx(&self, client_ip: &String) -> Option<mpsc::Sender<(u32, Bytes)>> {
        // Mode 1: Assign the same slave to clients from the same IP
        if self.client_assign_mode == 1 {
            if let Some(slave_ip) = self.cli_ip_to_slave.get(client_ip).map(|v| v.clone()) {
                return self.slaves.get(&slave_ip).map(|slave| slave.tx.clone());
            }
        }

        let mut total_weight = 0.0;
        let available_slaves: Vec<_> = self.slaves.iter()
            .filter_map(|entry| {
                let slave = entry.value();
                let weight = slave.net_speed;
                if weight > 0.0 {
                    total_weight += weight;
                    Some((slave.clone(), weight))
                } else {
                    None
                }
            })
            .collect();

        if available_slaves.is_empty() {
            return None;
        }

        // Weighted round-robin selection
        let mut random_weight: f64 = rand::random::<f64>() * total_weight;
        for (slave, weight) in available_slaves {
            if random_weight < weight {
                // Save the selected slave's IP for future requests from this client in sticky mode
                if self.client_assign_mode == 1 {
                    self.cli_ip_to_slave.insert(client_ip.clone(), slave.ip_addr.clone());
                }
                return Some(slave.tx.clone());
            }
            random_weight -= weight;
        }

        None
    }

    // Route data to the appropriate client using the session ID
    pub async fn route_to_client(&self, session_id: u32, payload: Bytes) {
        if let Some(client) = self.clients.get(&session_id) {
            if let Err(e) = client.to_client_tx.send(payload).await {
                error!("Failed to send data to client {}: {}", session_id, e);
            }
        } else {
            trace!("No client found with session ID {}. Dropping data.", session_id);
        }
    }

    pub async fn handle_slave_disconnection(&self, slave_ip: &str) {
        debug!("Slave {} disconnected", slave_ip);
        self.slaves.remove(slave_ip);
    }
}

// Function to handle a single slave's I/O operations for all clients using it (multiplexing)
pub async fn handle_slave_io(
    slave: Slave,
    mut cli_rx: mpsc::Receiver<(u32, Bytes)>,
    proxy_manager: Arc<ProxyManager>,
    buffer_pool: Arc<ShardedBufferPool>,
    metrics: Arc<Metrics>,
) -> Result<(), std::io::Error> {
    let shard_id = hash_ip(&slave.ip_addr);
    let mut buffer = buffer_pool.get_buffer(shard_id).await;

    metrics.slave_active_connections.inc();
    metrics.slave_total_connections.inc();

    let max_heartbeat_timeout = Duration::from_secs(KEEP_ALIVE_DURATION * 3);
    let heartbeat_interval = Duration::from_secs(KEEP_ALIVE_DURATION);
    let mut last_seen = Instant::now();
    let mut last_heartbeat_sent = Instant::now();

    let mut error_occurred = false;

    loop {
        tokio::select! {
            // Handle incoming traffic from the slave
            len = slave.read_stream(&mut buffer) => {
                let len = len?;
                if len == 0 {
                    break;  // Slave connection closed
                }

                last_seen = Instant::now();

                while buffer.len() >= 10 {
                    let (current_packet_type, session_id, payload_len, current_command_type) = parse_header(&buffer);

                    if current_packet_type.is_none() || buffer.len() < 10 + payload_len {
                        break;
                    }

                    buffer.advance(10);

                    // Process the packet with the current_packet_type and current_command_type
                    let payload = buffer.split_to(payload_len).freeze();

                    trace!("Processing packet from slave: {} | PacketType: {:?} | CommandType: {:?}", 
                          slave.ip_addr, current_packet_type, current_command_type);

                    if let Err(_err) = process_packet(
                        current_packet_type,
                        current_command_type,
                        payload,
                        session_id,
                        &slave,
                        &proxy_manager,
                        &mut last_seen,
                    ).await {
                        trace!("Critical error processing packet: {}. Exiting loop.", _err);
                        error_occurred = true;
                        break;
                    }

                    if error_occurred {
                        break;
                    }
                }
            }

            // Handle traffic from clients
            Some((client_session_id, client_data)) = cli_rx.recv() => {
                if client_data.is_empty() {
                    // Null data received, build and send CloseSession command
                    trace!("Received session termination signal for session {}", client_session_id);
                    let close_session_command = build_close_session_command(client_session_id);
                    if let Err(e) = slave.write_stream(&close_session_command).await {
                        error!("Failed to send CloseSession command to slave {}: {}", slave.ip_addr, e);
                        break;
                    }
                    continue;
                }

                debug!("sid {}, {} bytes: forwarded to SLAVE", client_session_id, client_data.len());
                let frame = build_data_frame(client_session_id, &client_data);
                if let Err(e) = slave.write_stream(&frame).await {
                    error!("Failed to write to slave {}: {}", slave.ip_addr, e);
                    break;
                }
            }

            // Periodically send heartbeat
            _ = tokio::time::sleep_until(last_heartbeat_sent + heartbeat_interval) => {
                if last_heartbeat_sent.elapsed() >= heartbeat_interval {
                    let heartbeat_command = build_heartbeat_command();
                    if let Err(err) = slave.write_stream(&heartbeat_command).await {
                        warn!("Failed to send heartbeat to slave {}: {}. Disconnecting.", slave.ip_addr, err);
                        return Err(err);
                    }
                    trace!("Sent heartbeat to slave {}", slave.ip_addr);
                    last_heartbeat_sent = Instant::now();
                }
            }

            // Monitor for heartbeat timeout
            _ = tokio::time::sleep_until(last_seen + max_heartbeat_timeout) => {
                if last_seen.elapsed() >= max_heartbeat_timeout {
                    warn!("Slave {} did not respond within the maximum allowed time. Disconnecting.", slave.ip_addr);

                    buffer_pool.return_buffer(shard_id, buffer).await;

                    // Handle disconnection
                    proxy_manager.handle_slave_disconnection(&slave.ip_addr).await;

                    metrics.slave_active_connections.dec();
                    metrics.slave_disconnections.inc();

                    return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "Heartbeat timeout"));
                }
            }
        }

        if error_occurred {
            break;
        }
    }
    
    buffer_pool.return_buffer(shard_id, buffer).await;

    // Handle disconnection
    proxy_manager.handle_slave_disconnection(&slave.ip_addr).await;

    metrics.slave_active_connections.dec();
    metrics.slave_disconnections.inc();

    Ok(())
}

// Function to handle traffic between a client and the slave
pub async fn handle_client_io(
    session_id: u32,
    client: Client,
    slave_tx: mpsc::Sender<(u32, Bytes)>,
    mut client_rx: mpsc::Receiver<Bytes>,
    proxy_manager: Arc<ProxyManager>,
    semaphore: Arc<Semaphore>,
    buffer_pool: Arc<ShardedBufferPool>,
) -> Result<(), std::io::Error> {
    // Add client session
    proxy_manager.clients.insert(session_id, client.clone());

    let mut cli_stream = client.stream.lock().await;
    let shard_id = session_id as usize;

    // Main loop to handle continuous traffic between client and slave
    loop {
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        let mut buffer = buffer_pool.get_buffer(shard_id).await;

        tokio::select! {
            client_read = timeout(CLIENT_REQUEST_TIMEOUT, cli_stream.read_buf(&mut buffer)) => {
                match client_read {
                    Ok(Ok(len)) => {
                        if len == 0 {
                            trace!("Client {} closed connection", session_id);
                            break;
                        }

                        debug!("sid {}, {} bytes: CLIENT sent", session_id, len);

                        let data = buffer.split().freeze();
                        if slave_tx.send((session_id, data)).await.is_err() {
                            warn!("Failed to send data to slave for session {}", session_id);
                            break;
                        }
                    }
                    Ok(Err(e)) => {
                        trace!("Error reading from client session id {}: {}", session_id, e);
                        break;
                    }
                    Err(_) => {
                        trace!("Timeout reading from client session id {}", session_id);
                        break;
                    }
                }
            }

            // Handle traffic from the slave to the client
            Some(payload) = client_rx.recv() => {
                debug!("sid {}, {} bytes: MASTER replied", session_id, payload.len());
                if let Err(e) = cli_stream.write_all(&payload).await {
                    error!("Failed to send data to client session id {}: {}", session_id, e);
                    break;
                }
                if let Err(e) = cli_stream.flush().await {
                    error!("Failed to flush stream for client session id {}: {}", session_id, e);
                    break;
                }
            }
        }

        buffer_pool.return_buffer(shard_id, buffer).await;
        drop(permit);
    }

    // Inform Slave to delete this session
    if slave_tx.send((session_id, Bytes::new())).await.is_err() {
        error!("Failed to notify slave about session {} closure", session_id);
    }

    // Cleanup after the session ends
    proxy_manager.clients.remove(&session_id);

    // Close the client stream
    debug!("Closing client stream for session ID {}.", session_id);
    drop(cli_stream);

    Ok(())
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
