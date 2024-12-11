use crate::buffer_pool::ShardedBufferPool;
use crate::metrics::Metrics;
use crate::packet::{
    parse_header,
    process_packet,
    build_heartbeat_command,
    build_init_session_command,
    build_data_frame
};
use crate::socks5::handle_client_handshake;
use crate::load_balancing::{
    Balancer,
    BalanceCtx,
    Strategy
};
use crate::utils::{
    hash_ip,
    CLIENT_REQUEST_TIMEOUT
};

use dashmap::DashMap;
use log::{debug, error, info, trace, warn};
use tokio::{io::{AsyncWriteExt, AsyncReadExt}, net::TcpStream};
use tokio::time::{Duration, Instant, timeout};
use std::sync::Arc;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicU8, Ordering};
use tokio::sync::{Mutex as AsyncMutex, mpsc, Semaphore};
use bytes::{BytesMut, Bytes, Buf};

const KEEP_ALIVE_DURATION: u64 = 10;

#[derive(Clone)]
pub struct Slave {
    pub ip_addr: String,
    pub id_token: u8,
    pub version: Option<String>,
    pub location: Option<String>,
    // Weight for round robin
    net_speed: f64,
    stream: Arc<AsyncMutex<TcpStream>>,
    // Sender to receive data from clients
    tx: mpsc::Sender<Bytes>,
}

impl Slave {
    pub fn new(ip_addr: String, stream: TcpStream) -> (Self, mpsc::Receiver<Bytes>) {
        let (tx, rx) = mpsc::channel::<Bytes>(500);
        let slave = Self {
            ip_addr,
            id_token: 0,
            version: None,
            location: None,
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

    pub fn set_location(&mut self, location: String) {
        self.location = Some(location);
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
    pub slaves: DashMap<String, Slave>, // ID String -> Slave
    pub clients: DashMap<u32, Client>,  // Map SessionId -> Client
    
    // Load balancing strategy
    pub balancer: Arc<AsyncMutex<Balancer>>,
    pub balancing_strategy: Strategy,
    token_counter: AtomicU8,
}

impl ProxyManager {
    pub fn new(client_assign_mode: u8) -> Self {
        let strategy = match client_assign_mode {
            1 => Strategy::IpHash,
            2 => Strategy::RoundRobin,
            _ => panic!("Invalid client_assign_mode. Must be 1 (IpHash) or 2 (RoundRobin)."),
        };

        ProxyManager {
            slaves: DashMap::new(),
            clients: DashMap::new(),
            balancer: Arc::new(AsyncMutex::new(Balancer::new(strategy, &[]))),
            balancing_strategy: strategy,
            token_counter: AtomicU8::new(0),
        }
    }

    pub fn generate_token(&self) -> u8 {
        self.token_counter.fetch_add(1, Ordering::Relaxed)
    }

    pub async fn update_balancer(&mut self) {
        let weights: Vec<u8> = self
            .slaves
            .iter()
            .map(|entry| entry.value().net_speed as u8)
            .collect();
        let mut balancer = self.balancer.lock().await;
        *balancer = Balancer::new(self.balancing_strategy, &weights);
    }

    pub async fn add_slave(&mut self, mut slave: Slave) {
        let new_token = self.generate_token();
        slave.id_token = new_token;

        self.slaves.insert(new_token.to_string(), slave);
        self.update_balancer().await;
    }

    pub async fn remove_slave(&mut self, slave_id_token: &u8) {
        self.slaves.remove(&slave_id_token.to_string());
        self.update_balancer().await;
    }

    // Get tx of avaiable Slave using weighted round-robin
    pub async fn get_available_slave_tx(
        &self,
        client_ip: &String,
        requested_location: Option<&String>
    ) -> Option<mpsc::Sender<Bytes>> {
        match IpAddr::from_str(client_ip) {
            Ok(parsed_ip) => {
                if let Some(token) = self.balancer.lock().await.next(BalanceCtx { src_ip: &parsed_ip }) {
                    if let Some(slave) = self.slaves.get(&token.0.to_string()) {
                        // If restricted location is empty, ignore location filtering
                        if let Some(requested_location) = requested_location {
                            if let Some(slave_location) = &slave.location {
                                // Check if the slave matches the requested location
                                if slave_location.eq_ignore_ascii_case(requested_location) {
                                    return Some(slave.tx.clone());
                                } else {
                                    trace!(
                                        "Slave {} does not match requested location {}",
                                        token.0,
                                        requested_location
                                    );
                                }
                            } else {
                                trace!(
                                    "Slave {} has no location data, skipping location match",
                                    token.0
                                );
                            }
                        } else {
                            // No location requested, return the slave
                            return Some(slave.tx.clone());
                        }
                    }
                }
            }
            Err(_) => {
                error!("Invalid IP address format: {}", client_ip);
            }
        }
        None
    }

    // Route data to the appropriate client using the session ID
    pub async fn route_to_client(&self, session_id: u32, payload: Bytes) {
        if let Some(client) = self.clients.get(&session_id) {
            if let Err(e) = client.to_client_tx.send(payload).await {
                error!("Failed to send to client mpsc channel: session id: {}: {}", session_id, e);
            }
        } else {
            trace!("No client found with session ID {}. Dropping data.", session_id);
        }
    }
}

// Function to handle a single slave's I/O operations for all clients using it (multiplexing)
pub async fn handle_slave_io(
    slave: Slave,
    mut cli_rx: mpsc::Receiver<Bytes>,
    proxy_manager: Arc<AsyncMutex<ProxyManager>>,
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
            Some(payload) = cli_rx.recv() => {
                if let Err(e) = slave.write_stream(&payload).await {
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

                    proxy_manager.lock().await.remove_slave(&slave.id_token).await;

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
    info!("Slave {} disconnected", slave.ip_addr);
    proxy_manager.lock().await.remove_slave(&slave.id_token).await;

    metrics.slave_active_connections.dec();
    metrics.slave_disconnections.inc();

    Ok(())
}

// Function to handle traffic between a client and the slave
pub async fn handle_client_io(
    session_id: u32,
    client: Client,
    mut client_rx: mpsc::Receiver<Bytes>,
    proxy_manager: Arc<AsyncMutex<ProxyManager>>,
    semaphore: Arc<Semaphore>,
    buffer_pool: Arc<ShardedBufferPool>,
) -> Result<(), std::io::Error> {
    let mut cli_stream = client.stream.lock().await;

    // Process SOCKS5 handshake and extract username, destination information
    let (username, dest_address, dest_port) = match handle_client_handshake(&mut cli_stream).await {
        Ok(result) => result,
        Err(e) => {
            error!(
                "Error during SOCKS5 handshake for session {}: {}",
                session_id, e
            );
            return Err(e); // Exit if handshake fails
        }
    };

    let slave_tx = proxy_manager.lock().await
        .get_available_slave_tx(&cli_stream.local_addr()?.ip().to_string(), username.as_ref())
        .await;

    let slave_tx = match slave_tx {
        Some(tx) => tx,
        None => {
            error!(
                "No suitable slave found for session {} (username: {:?})",
                session_id, username
            );
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "No suitable slave found",
            ));
        }
    };

    // Step 3: Forward destination info to the slave
    let dest_info = format!("{}:{}", dest_address, dest_port);
    let init_session_packet = build_init_session_command(session_id, &dest_info);
    if timeout(CLIENT_REQUEST_TIMEOUT, slave_tx.send(init_session_packet))
        .await
        .is_err()
    {
        warn!("Failed to send data to slave for session {}", session_id);
        return Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "Failed to send to slave tx",
        ));
    }

    // Add client session
    proxy_manager.lock().await.clients.insert(session_id, client.clone());

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

                        debug!("sid {}, {} bytes: CLIENT -> SLAVE", session_id, len);

                        let data = buffer.split().freeze();
                        let data_packet = build_data_frame(session_id, &data);

                        if slave_tx.send(data_packet).await.is_err() {
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

    // Cleanup after the session ends
    proxy_manager.lock().await.clients.remove(&session_id);

    // Close the client stream
    debug!("Closing client stream for session ID {}.", session_id);
    drop(cli_stream);

    Ok(())
}
