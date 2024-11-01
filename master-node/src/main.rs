use std::collections::HashMap;
use std::error::Error;
use log::{trace, debug, info, warn, error, LevelFilter};
use simple_logger::SimpleLogger;
use getopts::Options;
use net2::TcpStreamExt;
use tokio::{io::{self, AsyncWriteExt, AsyncReadExt}, net::{TcpListener, TcpStream}};
use std::sync::Arc;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

const KEEP_ALIVE_DURATION: u64 = 10;
const MAX_BUF_SIZE: usize = 8192;

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
}

impl CommandType {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x01 => Some(CommandType::SpeedCheck),
            0x02 => Some(CommandType::VersionCheck),
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
    tx: mpsc::Sender<(u32, Vec<u8>)>,
}

impl Slave {
    fn new(ip_addr: String, stream: TcpStream) -> (Self, mpsc::Receiver<(u32, Vec<u8>)>) {
        let (tx, rx) = mpsc::channel(100);
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
    async fn read_stream(&self, buffer: &mut [u8]) -> Result<usize, std::io::Error> {
        let mut slave_stream = self.stream.lock().await;
        slave_stream.read(buffer).await
    }

    // Helper method to write to the slave stream
    async fn write_stream(&self, frame: &[u8]) -> Result<(), std::io::Error> {
        let mut stream = self.stream.lock().await;
        stream.write_all(frame).await?;
        stream.flush().await?;
        Ok(())
    }
}

#[derive(Clone)]
struct Client {
    stream: Arc<AsyncMutex<TcpStream>>,
    to_client_tx: mpsc::Sender<Vec<u8>>,
}

impl Client {
    fn new(stream: Arc<AsyncMutex<TcpStream>>, to_client_tx: mpsc::Sender<Vec<u8>>) -> Self {
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
    slaves: Arc<AsyncMutex<HashMap<String, Slave>>>,
    clients: Arc<AsyncMutex<HashMap<u32, Client>>>,

    // Map client IP addresses to slave IPs in Client Assignment Mode 1
    cli_ip_to_slave: Arc<AsyncMutex<HashMap<String, String>>>,
}

impl ProxyManager {
    // Get an available slave stream using weighted round-robin
    async fn get_available_slave_ip(&self, client_ip: &String) -> Option<String> {
        let mut cli_ip_to_slave = self.cli_ip_to_slave.lock().await;
        let slave_list = self.slaves.lock().await;

        // Mode 1: Assign the same slave to clients from the same IP
        if self.client_assign_mode == 1 {
            // Mode 1: Assign the same slave to clients from the same IP
            if let Some(slave_ip) = cli_ip_to_slave.get(client_ip) {
                return Some(slave_ip.clone());
            }
        }

        let mut total_weight = 0.0;
        let available_slaves: Vec<_> = slave_list.iter()
            .filter_map(|(ip, slave)| {
                let weight = slave.net_speed;
                if weight > 0.0 {
                    total_weight += weight;
                    Some((ip, weight))
                } else {
                    None
                }
            })
            .collect();

        if available_slaves.is_empty() {
            return None;
        }

        // Weighted round-robin selection
        if total_weight > 0.0 {
            let mut random_weight: f64 = rand::random::<f64>() * total_weight;
            for (ip, weight) in available_slaves {
                if random_weight < weight {
                    if self.client_assign_mode == 1 {
                        cli_ip_to_slave.insert(client_ip.clone(), ip.to_string());
                    }
                    return Some(ip.to_string());
                }
                random_weight -= weight;
            }
        }

        None
    }

    // Route data to the appropriate client using the session ID
    async fn route_to_client(&self, session_id: u32, payload: Vec<u8>) {
        let client = {
            let clients = self.clients.lock().await;
            clients.get(&session_id).cloned()
        };

        if let Some(client) = client {
            if let Err(e) = client.to_client_tx.send(payload).await {
                error!("Failed to send data to client {}: {}", session_id, e);
            }
        }
    }

    async fn update_slave_speed(&self, slave_ip: &str, net_speed: f64) {
        let mut slaves = self.slaves.lock().await;
        if let Some(slave) = slaves.get_mut(slave_ip) {
            slave.net_speed = net_speed;
        }
    }

    async fn update_slave_version(&self, slave_ip: &str, version: String) {
        let mut slaves = self.slaves.lock().await;
        if let Some(slave) = slaves.get_mut(slave_ip) {
            slave.version = Some(version.clone());
        }
    }

    // Handle slave disconnection based on slave_recovery_mode
    async fn handle_slave_disconnection(&self, slave_ip: &str) {
        {
            let mut slaves = self.slaves.lock().await;

            if slaves.remove(slave_ip).is_some() {
                debug!("Removed disconnected slave: {}", slave_ip);
            } else {
                warn!("Attempted to remove non-existent slave: {}", slave_ip);
                return;
            }
        }

        if self.slave_recovery_mode == 1 {
            info!("Waiting for slave {} to recover", slave_ip);
            // In this mode, we simply wait for the slave to reconnect
            // Recovery logic should be handled in the connection loop
        } else if self.slave_recovery_mode == 2 {
            let mut ip_to_slave_map = self.cli_ip_to_slave.lock().await;

            // Remove all IPs that were mapped to the disconnected slave
            ip_to_slave_map.retain(|_, mapped_slave_ip| mapped_slave_ip != slave_ip);
        }

        info!("Slave {} disconnected", slave_ip);
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

fn build_command_frame(packet_type: PacketType, session_id: u32, command_type: Option<CommandType>, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(10 + payload.len());
    frame.push(packet_type as u8);
    frame.extend_from_slice(&session_id.to_be_bytes());
    
    if let Some(cmd) = command_type {
        frame.push(cmd as u8);
    } else {
        frame.push(0x00);
    }

    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

fn build_speed_test_command(url: &str) -> Vec<u8> {
    build_command_frame(PacketType::Command, 0, Some(CommandType::SpeedCheck), url.as_bytes())
}

fn build_version_check_command() -> Vec<u8> {
    build_command_frame(PacketType::Command, 0, Some(CommandType::VersionCheck), &[])
}

fn build_data_frame(session_id: u32, payload: &[u8]) -> Vec<u8> {
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

// Function to handle a single slave's I/O operations for all clients using it (multiplexing)
async fn handle_slave_io(
    slave: Slave,
    mut cli_rx: mpsc::Receiver<(u32, Vec<u8>)>,
    proxy_manager: Arc<ProxyManager>,
) -> Result<(), std::io::Error> {
    let mut buffer = [0u8; MAX_BUF_SIZE];
    let mut accumulated_buffer: Vec<u8> = Vec::new();

    // Prepare the commands
    let speed_check_command = build_speed_test_command("https://speed.cloudflare.com/__down?bytes=5000000");
    let version_check_command = build_version_check_command();

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

    loop {
        tokio::select! {
            // Handle incoming traffic from the slave
            len = slave.read_stream(&mut buffer) => {
                let len = len?;
                if len == 0 {
                    break;  // Slave connection closed
                }

                accumulated_buffer.extend_from_slice(&buffer[..len]);

                while accumulated_buffer.len() >= 10 {
                    let (current_packet_type, session_id, payload_len, current_command_type) = parse_header(&accumulated_buffer);

                    if current_packet_type.is_none() {
                        break;
                    }

                    // Ensure we have the entire payload before processing
                    if accumulated_buffer.len() < 10 + payload_len {
                        break;
                    }

                    // Remove the header from the buffer
                    accumulated_buffer.drain(0..10);

                    // Process the packet with the current_packet_type and current_command_type
                    let payload = accumulated_buffer.drain(0..payload_len).collect::<Vec<u8>>();

                    debug!("Processing packet from slave: {} | PacketType: {:?} | CommandType: {:?}", 
                          slave.ip_addr, current_packet_type, current_command_type);

                    match current_packet_type {
                        Some(PacketType::Command) => {
                            match current_command_type {
                                Some(CommandType::SpeedCheck) => {
                                    let speed_str = String::from_utf8(payload.clone()).unwrap_or_default();
                                    if let Ok(speed) = speed_str.parse::<f64>() {
                                        proxy_manager.update_slave_speed(&slave.ip_addr, speed).await;
                                        info!("Slave: {} | Net speed: {:.2} Mbps", slave.ip_addr, speed);
                                    } else {
                                        warn!("Failed to parse SpeedCheck response from slave: {}", slave.ip_addr);
                                    }
                                }
                                Some(CommandType::VersionCheck) => {
                                    let version = String::from_utf8(payload.clone()).unwrap_or_default();
                                    proxy_manager.update_slave_version(&slave.ip_addr, version.clone()).await;
                                    info!("Slave: {} | Version: {}", slave.ip_addr, version);
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
                }
            }

            // Handle traffic from clients
            Some((client_session_id, client_data)) = cli_rx.recv() => {
                trace!("Master -> Slave: sid {}, size: {} bytes", client_session_id, client_data.len());
                let frame = build_data_frame(client_session_id, &client_data);
                slave.write_stream(&frame).await?;
            }
        }
    }

    // Slave recovery is only available when proxy stick mode
    if proxy_manager.client_assign_mode == 1 {
        proxy_manager.handle_slave_disconnection(&slave.ip_addr).await;
    }
    
    Ok(())
}

// Function to handle traffic between a client and the slave
async fn handle_client_io(
    session_id: u32,
    slave_tx: mpsc::Sender<(u32, Vec<u8>)>,
    mut client_rx: mpsc::Receiver<Vec<u8>>,
    proxy_manager: Arc<ProxyManager>,
) -> Result<(), std::io::Error> {
    let mut client_buffer = [0u8; MAX_BUF_SIZE];

    let client = {
        let clients = proxy_manager.clients.lock().await;
        if let Some(client) = clients.get(&session_id) {
            client.clone() // Assuming Client is Clone
        } else {
            return Ok(()); // Exit if the client is not found
        }
    };

    // Lock the receiver
    let mut cli_stream = client.stream.lock().await;

    // Main loop to handle continuous traffic between client and slave
    loop {
        tokio::select! {
            client_read = cli_stream.read(&mut client_buffer) => {
                let len = client_read?;
                if len > 0 {
                    trace!("Client -> Master: sid {}, size: {} bytes", session_id, len);
                    let _ = slave_tx.send((session_id, client_buffer[..len].to_vec())).await;
                } else {
                    break; // Client connection closed
                }
            }

            Some(payload) = client_rx.recv() => {
                trace!("Master -> Client: sid {}, size: {} bytes", session_id, payload.len());
                if let Err(e) = cli_stream.write_all(&payload).await {
                    error!("Failed to send data to client {}: {}", session_id, e);
                }
                if let Err(e) = cli_stream.flush().await {
                    error!("Failed to flush stream for client {}: {}", session_id, e);
                }
            }
        }
    }

    // Cleanup after the session ends
    {
        let mut clients = proxy_manager.clients.lock().await;
        clients.remove(&session_id);
    }

    // Close the client stream
    debug!("Closing client stream for session ID {}.", session_id);
    drop(cli_stream);

    Ok(())
}

async fn handle_slave_connections(slave_listener: TcpListener, proxy_manager: Arc<ProxyManager>) -> Result<(), Box<dyn Error>> {
    loop {
        let (slave_stream, slave_addr) = match slave_listener.accept().await {
            Err(e) => {
                error!("Error accepting connection: {}", e);
                return Err(Box::new(e)); // Return the error instead of Ok
            }
            Ok(p) => p,
        };

        let raw_stream = slave_stream.into_std().unwrap();
        raw_stream.set_keepalive(Some(std::time::Duration::from_secs(KEEP_ALIVE_DURATION))).unwrap();
        let slave_stream = TcpStream::from_std(raw_stream).unwrap();

        info!("New Slave {}:{}", slave_addr.ip(), slave_addr.port());

        // Create a new Slave object
        let (new_slave, slave_rx) = Slave::new(slave_addr.ip().to_string(), slave_stream);

        // Update ProxyManager with the new slave
        proxy_manager.slaves.lock().await.insert(new_slave.ip_addr.clone(), new_slave.clone());

        // Spawn a task to handle the slave's I/O operations
        let proxy_manager_clone = Arc::clone(&proxy_manager);
        let new_slave_clone = new_slave.clone();
        tokio::spawn(async move {
            // Call the slave IO handler for the new slave
            let result = handle_slave_io(new_slave_clone, slave_rx, proxy_manager_clone).await;
            if let Err(e) = result {
                error!("Error handling IO for slave {}: {}", slave_addr.ip(), e);
            }
        });
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
        slaves: Arc::new(AsyncMutex::new(HashMap::new())),
        clients: Arc::new(AsyncMutex::new(HashMap::new())),
        cli_ip_to_slave: Arc::new(AsyncMutex::new(HashMap::new())),
    });

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
            async move {
                if let Err(e) = handle_slave_connections(slave_listener, proxy_manager).await {
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

			// Generate a new session ID for this client
			let session_id = rand::random::<u32>();

            // Retrieve an available slave stream
            let client_ip = client_addr.ip().to_string();
            if let Some(slave_ip) = proxy_manager.get_available_slave_ip(&client_ip).await {
                // Lock the incoming client stream
                let raw_stream = client_stream.into_std().unwrap();
                raw_stream.set_keepalive(Some(std::time::Duration::from_secs(KEEP_ALIVE_DURATION))).unwrap();
                let client_stream = Arc::new(AsyncMutex::new(TcpStream::from_std(raw_stream).unwrap()));

                let slave = {
                    let slaves = proxy_manager.slaves.lock().await;
                    slaves.get(&slave_ip).unwrap().clone()
                };

                let (client_tx, client_rx) = mpsc::channel(100);

                let client = Client::new(client_stream.clone(), client_tx);
    
                proxy_manager.clients.lock().await.insert(session_id, client);

                // Spawn a task to handle traffic between the client and the assigned slave
                let proxy_manager_clone = Arc::clone(&proxy_manager);
                tokio::spawn(handle_client_io(session_id, slave.tx.clone(), client_rx, proxy_manager_clone));
            } else {
                warn!("No available slave for client with session ID {}", session_id);
            }
        }
    } else {
        usage(&program, &opts);
    }
    Ok(())
}
