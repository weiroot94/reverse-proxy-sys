use dashmap::DashMap;
use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use log::{trace, debug, info, warn, error, LevelFilter};
use simple_logger::SimpleLogger;
use getopts::Options;
use tokio::{io::{self, AsyncWriteExt, AsyncReadExt}, net::{TcpListener, TcpStream}};
use tokio::time::{self, Duration, Instant, timeout};
use std::sync::Arc;
use tokio::sync::{Mutex as AsyncMutex, mpsc, Semaphore};
use bytes::{BytesMut, Bytes, Buf, BufMut};
use std::collections::VecDeque;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

const KEEP_ALIVE_DURATION: u64 = 10;
const MAX_BUF_SIZE: usize = 8192;

const MAX_CONCURRENT_REQUESTS: usize = 100;
const CLIENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

const POOL_SIZE: usize = 50;
const NUM_SHARDS: usize = 8;

// Sharded Buffer Pool for high concurrency
#[derive(Clone)]
struct ShardedBufferPool {
    shards: Vec<Arc<BufferPoolShard>>,
}

impl ShardedBufferPool {
    fn new(num_shards: usize, pool_size: usize) -> Self {
        let shards = (0..num_shards)
            .map(|_| Arc::new(BufferPoolShard::new(pool_size)))
            .collect();
        Self { shards }
    }

    async fn get_buffer(&self, id: usize) -> BytesMut {
        let shard = &self.shards[id % self.shards.len()];
        shard.get_buffer().await
    }

    async fn return_buffer(&self, id: usize, buffer: BytesMut) {
        let shard = &self.shards[id % self.shards.len()];
        shard.return_buffer(buffer).await;
    }
}

struct BufferPoolShard {
    buffers: AsyncMutex<VecDeque<BytesMut>>,
}

impl BufferPoolShard {
    fn new(pool_size: usize) -> Self {
        let mut buffers = VecDeque::with_capacity(pool_size);
        for _ in 0..pool_size {
            buffers.push_back(BytesMut::with_capacity(MAX_BUF_SIZE));
        }
        Self {
            buffers: AsyncMutex::new(buffers),
        }
    }

    async fn get_buffer(&self) -> BytesMut {
        let mut buffers = self.buffers.lock().await;
        if let Some(mut buffer) = buffers.pop_front() {
            if buffer.capacity() < MAX_BUF_SIZE {
                debug!("Discarding undersized buffer, creating new one");
                buffer = BytesMut::with_capacity(MAX_BUF_SIZE);
            } else {
                buffer.clear(); // Prepare buffer for reuse
            }
            debug!("Borrowed buffer: capacity = {}", buffer.capacity());
            buffer
        } else {
            debug!("No available buffer, creating new one");
            BytesMut::with_capacity(MAX_BUF_SIZE)
        }
    }

    async fn return_buffer(&self, buffer: BytesMut) {
        let mut buffers = self.buffers.lock().await;
        if buffers.len() < POOL_SIZE {
            debug!("Returning buffer to pool: capacity = {}", buffer.capacity());
            buffers.push_back(buffer);
        } else {
            debug!("Buffer pool full, discarding buffer");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PacketType {
    Data = 0x00,
    Command = 0x01,
}

impl PacketType {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x00 => Some(PacketType::Data),
            0x01 => Some(PacketType::Command),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum CommandType {
    SpeedCheck = 0x01,
    VersionCheck = 0x02,
    Heartbeat = 0x03,
}

impl CommandType {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x01 => Some(CommandType::SpeedCheck),
            0x02 => Some(CommandType::VersionCheck),
            0x03 => Some(CommandType::Heartbeat),
            _ => None,
        }
    }
}

#[derive(Clone)]
struct Slave {
    ip_addr: String,
    version: Option<String>,
    // Weight for round robin
    net_speed: f64,
    stream: Arc<AsyncMutex<TcpStream>>,
    // Sender to receive data from clients
    tx: mpsc::Sender<(u32, Bytes)>,
    // Track the last successful heartbeat time
    last_seen: Arc<AsyncMutex<Instant>>,
    is_disconnected: Arc<AtomicBool>,
}

impl Slave {
    fn new(ip_addr: String, stream: TcpStream) -> (Self, mpsc::Receiver<(u32, Bytes)>) {
        let (tx, rx) = mpsc::channel(100);
        let slave = Self {
            ip_addr,
            version: None,
            net_speed: 0.0,
            stream: Arc::new(AsyncMutex::new(stream)),
            tx,
            last_seen: Arc::new(AsyncMutex::new(Instant::now())),
            is_disconnected: Arc::new(AtomicBool::new(false)),
        };
        (slave, rx)
    }

    // Helper method to read from the slave stream
    async fn read_stream(&self, buffer: &mut BytesMut) -> Result<usize, std::io::Error> {
        let mut slave_stream = self.stream.lock().await;
        slave_stream.read_buf(buffer).await
    }

    // Helper method to write to the slave stream
    async fn write_stream(&self, frame: &Bytes) -> Result<(), std::io::Error> {
        let mut stream = self.stream.lock().await;
        stream.write_all(frame).await?;
        stream.flush().await?;
        Ok(())
    }

    async fn update_last_seen(&self) {
        let mut last_seen = self.last_seen.lock().await;
        *last_seen = Instant::now();
    }
}

#[derive(Clone)]
struct Client {
    stream: Arc<AsyncMutex<TcpStream>>,
    to_client_tx: mpsc::Sender<Bytes>,
}

impl Client {
    fn new(stream: Arc<AsyncMutex<TcpStream>>, to_client_tx: mpsc::Sender<Bytes>) -> Self {
        Self {
            stream,
            to_client_tx,
        }
    }
}
struct ProxyManager {
    // Mode how master node works
    client_assign_mode: u8, // 1 or 2
    slave_recovery_mode: u8, // 1 or 2

    // Slaves and clients
    slaves: Arc<DashMap<String, Slave>>,
    clients: Arc<DashMap<u32, Client>>,

    // Map client IP addresses to slave IPs in Client Assignment Mode 1
    cli_ip_to_slave: Arc<DashMap<String, String>>,
}

impl ProxyManager {
    // Get tx of avaiable Slave using weighted round-robin
    async fn get_available_slave_tx(&self, client_ip: &String) -> Option<mpsc::Sender<(u32, Bytes)>> {
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
    async fn route_to_client(&self, session_id: u32, payload: Bytes) {
        if let Some(client) = self.clients.get(&session_id) {
            if let Err(e) = client.to_client_tx.send(payload).await {
                error!("Failed to send data to client {}: {}", session_id, e);
            }
        }
    }

    async fn update_slave_speed(&self, slave_ip: &str, net_speed: f64) {
        if let Some(mut slave) = self.slaves.get_mut(slave_ip) {
            slave.net_speed = net_speed;
        }
    }

    async fn update_slave_version(&self, slave_ip: &str, version: String) {
        if let Some(mut slave) = self.slaves.get_mut(slave_ip) {
            slave.version = Some(version.clone());
        }
    }

    async fn monitor_slave_health(self: Arc<Self>, slave: Slave) {
        let timeout_duration = Duration::from_secs(20);
        loop {
            tokio::time::sleep(Duration::from_secs(KEEP_ALIVE_DURATION)).await;

            let last_seen = *slave.last_seen.lock().await;
            if last_seen.elapsed() > timeout_duration {
                warn!("Slave {} timed out.", slave.ip_addr);
                slave.is_disconnected.store(true, Ordering::Relaxed);
                self.handle_slave_disconnection(&slave.ip_addr).await;
                break;
            }
        }
    }

    async fn handle_slave_disconnection(&self, slave_ip: &str) {
        info!("Slave {} disconnected", slave_ip);
        if self.slaves.remove(slave_ip).is_some() {
            debug!("Removed disconnected slave: {}", slave_ip);
        }
    }
}

fn usage(program: &str, opts: &Options) {
    let program_path = std::path::PathBuf::from(program);
    let program_name = program_path.file_stem().unwrap().to_str().unwrap();
    let brief = format!("Usage: {} [-ltr] [IP_ADDRESS] [-s] [IP_ADDRESS]",
                        program_name);
    print!("{}", opts.usage(&brief));
}

// Utility function to convert bytes to an integer
fn bytes_to_u32(bytes: &[u8]) -> u32 {
    let mut array = [0u8; 4];
    array.copy_from_slice(bytes);
    u32::from_be_bytes(array)
}

fn build_command_frame(packet_type: PacketType, session_id: u32, command_type: Option<CommandType>, payload: &[u8]) -> Bytes {
    let mut frame = BytesMut::with_capacity(10 + payload.len());
    frame.put_u8(packet_type as u8);
    frame.put_u32(session_id);
    
    if let Some(cmd) = command_type {
        frame.put_u8(cmd as u8);
    } else {
        frame.put_u8(0x00);
    }

    frame.put_u32(payload.len() as u32);
    frame.put_slice(payload);
    frame.freeze()
}

fn build_speed_test_command(url: &str) -> Bytes {
    build_command_frame(PacketType::Command, 0, Some(CommandType::SpeedCheck), url.as_bytes())
}

fn build_version_check_command() -> Bytes {
    build_command_frame(PacketType::Command, 0, Some(CommandType::VersionCheck), &[])
}

fn build_heartbeat_command() -> Bytes {
    build_command_frame(PacketType::Command, 0, Some(CommandType::Heartbeat), &[])
}

fn build_data_frame(session_id: u32, payload: &[u8]) -> Bytes {
    build_command_frame(PacketType::Data, session_id, None, payload)
}

fn parse_header(buffer: &[u8]) -> (Option<PacketType>, u32, usize, Option<CommandType>) {
    if buffer.len() < 10 {
        return (None, 0, 0, None);
    }

    let packet_type = PacketType::from_u8(buffer[0]);
    let session_id = bytes_to_u32(&buffer[1..5]);
    let payload_len = u32::from_be_bytes([
        buffer[6],
        buffer[7],
        buffer[8],
        buffer[9],
    ]) as usize;

    // Check for CommandType if the packet is a command
    let command_type = if packet_type == Some(PacketType::Command) {
        CommandType::from_u8(buffer[5])
    } else {
        None
    };

    (packet_type, session_id, payload_len, command_type)
}

fn hash_ip(ip_addr: &str) -> usize {
    let mut hasher = DefaultHasher::new();
    ip_addr.hash(&mut hasher);
    hasher.finish() as usize
}

// Function to handle a single slave's I/O operations for all clients using it (multiplexing)
async fn handle_slave_io(
    slave: Slave,
    mut cli_rx: mpsc::Receiver<(u32, Bytes)>,
    proxy_manager: Arc<ProxyManager>,
    buffer_pool: Arc<ShardedBufferPool>,
) -> Result<(), std::io::Error> {
    let shard_id = hash_ip(&slave.ip_addr);
    let mut buffer = buffer_pool.get_buffer(shard_id).await;

    // Prepare the commands
    let speed_check_command = build_speed_test_command("https://speed.cloudflare.com/__down?bytes=5000000");
    let version_check_command = build_version_check_command();
    let heartbeat_command = build_heartbeat_command();

    // Send commands concurrently by spawning async tasks
    {
        let slave_clone = slave.clone(); // Clone to move into async task
        let send_speed_check = tokio::spawn(async move {
            slave_clone.write_stream(&speed_check_command).await
        });

        let slave_clone = slave.clone(); // Another clone for version check
        let send_version_check = tokio::spawn(async move {
            slave_clone.write_stream(&version_check_command).await
        });

        // Await both tasks to ensure commands are sent before proceeding
        let _ = tokio::try_join!(send_speed_check, send_version_check)?;
    }

    // Heartbeat setup
    let mut heartbeat_interval = time::interval(Duration::from_secs(KEEP_ALIVE_DURATION));

    loop {
        if slave.is_disconnected.load(Ordering::Relaxed) {
            info!("Slave {} already marked as disconnected. Exiting IO handler.", slave.ip_addr);
            return Ok(());
        }

        tokio::select! {
            // Handle incoming traffic from the slave
            len = slave.read_stream(&mut buffer) => {
                let len = len?;
                if len == 0 {
                    break;  // Slave connection closed
                }

                while buffer.len() >= 10 {
                    let (current_packet_type, session_id, payload_len, current_command_type) = parse_header(&buffer);

                    if current_packet_type.is_none() || buffer.len() < 10 + payload_len {
                        break;
                    }

                    buffer.advance(10);

                    // Process the packet with the current_packet_type and current_command_type
                    let payload = buffer.split_to(payload_len).freeze();

                    debug!("Processing packet from slave: {} | PacketType: {:?} | CommandType: {:?}", 
                          slave.ip_addr, current_packet_type, current_command_type);
                    process_packet(
                        current_packet_type,
                        current_command_type,
                        payload,
                        session_id,
                        &slave,
                        &proxy_manager
                    ).await?;
                }
            }

            // Handle traffic from clients
            Some((client_session_id, client_data)) = cli_rx.recv() => {
                trace!("Master -> Slave: sid {}, size: {} bytes", client_session_id, client_data.len());
                let frame = build_data_frame(client_session_id, &client_data);
                slave.write_stream(&frame).await?;
            }

            // Heartbeat timer to send periodic heartbeats to the slave
            _ = heartbeat_interval.tick() => {
                slave.write_stream(&heartbeat_command).await?;
            }
        }
    }
    
    buffer_pool.return_buffer(shard_id, buffer).await;
    Ok(())
}

async fn process_packet(
    packet_type: Option<PacketType>,
    command_type: Option<CommandType>,
    payload: Bytes,
    session_id: u32,
    slave: &Slave,
    proxy_manager: &Arc<ProxyManager>,
) -> Result<(), std::io::Error> {
    match packet_type {
        Some(PacketType::Command) => {
            match command_type {
                Some(CommandType::SpeedCheck) => {
                    let speed_str = String::from_utf8(payload.to_vec()).unwrap_or_default();
                    if let Ok(speed) = speed_str.parse::<f64>() {
                        proxy_manager.update_slave_speed(&slave.ip_addr, speed).await;
                        info!("Slave: {} | Net speed: {:.2} Mbps", slave.ip_addr, speed);
                    } else {
                        warn!("Failed to parse SpeedCheck response from slave: {}", slave.ip_addr);
                    }
                }
                Some(CommandType::VersionCheck) => {
                    let version = String::from_utf8(payload.to_vec()).unwrap_or_default();
                    proxy_manager.update_slave_version(&slave.ip_addr, version.clone()).await;
                    info!("Slave: {} | Version: {}", slave.ip_addr, version);
                }
                Some(CommandType::Heartbeat) => {
                    // Update last_seen on valid heartbeat response
                    if payload.as_ref() == b"ALIVE" {
                        slave.update_last_seen().await;
                        trace!("Received heartbeat response from slave {}", slave.ip_addr);
                    } else {
                        warn!("Invalid heartbeat response from slave {}: {:?}", slave.ip_addr, payload);
                        proxy_manager.handle_slave_disconnection(&slave.ip_addr).await;
                    }
                }
                None => error!("Unknown CommandType received from slave: {}", slave.ip_addr),
            }
        }
        Some(PacketType::Data) => {
            proxy_manager.route_to_client(session_id, payload).await;
        }
        None => {
            error!("Unknown PacketType received from slave: {}", slave.ip_addr);
        }
    }

    Ok(())
}

// Function to handle traffic between a client and the slave
async fn handle_client_io(
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
                            debug!("Client {} closed connection", session_id);
                            break;
                        }

                        trace!("Client -> Master: sid {}, size: {} bytes", session_id, len);

                        let data = buffer.split().freeze();
                        if slave_tx.send((session_id, data)).await.is_err() {
                            warn!("Failed to send data to slave for session {}", session_id);
                            break;
                        }
                    }
                    Ok(Err(e)) => {
                        error!("Error reading from client {}: {}", session_id, e);
                        break;
                    }
                    Err(_) => {
                        trace!("Timeout reading from client {}", session_id);
                        break;
                    }
                }
            }

            // Handle traffic from the slave to the client
            Some(payload) = client_rx.recv() => {
                trace!("Master -> Client: sid {}, size: {} bytes", session_id, payload.len());
                if let Err(e) = cli_stream.write_all(&payload).await {
                    error!("Failed to send data to client {}: {}", session_id, e);
                    break;
                }
                if let Err(e) = cli_stream.flush().await {
                    error!("Failed to flush stream for client {}: {}", session_id, e);
                    break;
                }
            }
        }

        buffer_pool.return_buffer(shard_id, buffer).await;
        drop(permit);
    }

    // Cleanup after the session ends
    proxy_manager.clients.remove(&session_id);

    // Close the client stream
    debug!("Closing client stream for session ID {}.", session_id);
    drop(cli_stream);

    Ok(())
}

async fn handle_slave_connections(
    slave_listener: TcpListener,
    proxy_manager: Arc<ProxyManager>,
    buffer_pool: Arc<ShardedBufferPool>
) -> Result<(), Box<dyn Error>> {
    loop {
        let (slave_stream, slave_addr) = match slave_listener.accept().await {
            Err(e) => {
                error!("Error accepting connection: {}", e);
                return Err(Box::new(e)); // Return the error instead of Ok
            }
            Ok(p) => p,
        };

        info!("New Slave {}:{}", slave_addr.ip(), slave_addr.port());

        // Create a new Slave object
        let (new_slave, slave_rx) = Slave::new(slave_addr.ip().to_string(), slave_stream);
        proxy_manager.slaves.insert(new_slave.ip_addr.clone(), new_slave.clone());

        // Spawn a task to handle the slave's I/O operations
        let proxy_manager_clone = Arc::clone(&proxy_manager);
        let new_slave_clone = new_slave.clone();
        let buffer_pool_clone = Arc::clone(&buffer_pool);
        tokio::spawn(async move {
            // Call the slave IO handler for the new slave
            let result = handle_slave_io(new_slave_clone, slave_rx, proxy_manager_clone, buffer_pool_clone).await;
            if let Err(e) = result {
                error!("Error handling IO for slave {}: {}", slave_addr.ip(), e);
            }
        });

        // Spawn a task to monitor the slave's health
        let proxy_manager_clone = Arc::clone(&proxy_manager);
        tokio::spawn(proxy_manager_clone.monitor_slave_health(new_slave.clone()));
    }
}

#[tokio::main]
async fn main() -> io::Result<()>  {
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
    
    opts.optopt("r",
                "slave_recovery",
                "Set the slave recovery mode: wait (1) or reconnect (2). Only available when proxy_mode=stick",
                "MODE");

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
            error!("Invalid proxy mode. Using default (stick).");
            1
        }
    };

    // Parse slave_recovery (wait or reconnect) only if proxy_mode is stick
    let slave_recovery_mode: u8 = if client_assign_mode == 1 {
        matches.opt_str("r")
            .unwrap_or_else(|| "reconnect".to_string())
            .parse::<String>()
            .map(|recovery_mode| match recovery_mode.as_str() {
                "wait" => 1,
                "reconnect" => 2,
                _ => {
                    error!("Invalid slave recovery mode. Using default (reconnect).");
                    1
                }
            })
            .unwrap_or(1)
    } else {
        // For nonstick mode, we don't handle slave_recovery
        info!("Slave recovery is ignored in nonstick mode.");
        0 // Use 0 to signify that slave recovery isn't relevant in nonstick mode
    };

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

    // Global store of proxy system
    let proxy_manager = Arc::new(ProxyManager {
        client_assign_mode,
        slave_recovery_mode,
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
            async move {
                if let Err(e) = handle_slave_connections(slave_listener, proxy_manager, buffer_pool_clone).await {
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

            debug!("New Client: {}", client_addr.ip());

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
                    buffer_pool_clone
                ));
            }
        }
    } else {
        usage(&program, &opts);
    }
    Ok(())
}
