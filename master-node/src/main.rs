mod utils;
mod socks;

use std::collections::HashMap;
use std::error::Error;
use log::LevelFilter;
use net2::TcpStreamExt;
use simple_logger::SimpleLogger;
use getopts::Options;
use utils::MAGIC_FLAG;
use tokio::{io::{self, AsyncWriteExt, AsyncReadExt}, task, net::{TcpListener, TcpStream}};
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc;
use tokio::sync::broadcast;

const KEEP_ALIVE_DURATION: u64 = 10;
const MAX_BUF_SIZE: usize = 4096;

#[derive(Clone)]
struct Slave {
    ip_addr: String,
    net_speed: f64, // Weight for round robin
    stream: Arc<AsyncMutex<TcpStream>>,
    cli_sids: Arc<AsyncMutex<Vec<u32>>>,
}

impl Slave {
    fn new(ip_addr: String, net_speed: f64, stream: TcpStream) -> Self {
        Self {
            ip_addr,
            net_speed,
            stream: Arc::new(AsyncMutex::new(stream)),
            cli_sids: Arc::new(AsyncMutex::new(Vec::new())),
        }
    }

    async fn add_client_session(&self, session_id: u32) {
        let mut cli_sids = self.cli_sids.lock().await;
        cli_sids.push(session_id);
    }

    // Method to remove a session ID
    async fn remove_client_session(&self, session_id: u32) {
        let mut cli_sids = self.cli_sids.lock().await;
        cli_sids.retain(|&id| id != session_id); // Retain only those IDs that do not match
    }

    // Method to get the current number of connected sessions
    async fn num_connected_clients(&self) -> usize {
        let cli_sids = self.cli_sids.lock().await;
        cli_sids.len()
    }
}

#[derive(Clone)]
struct Client {
    ip_addr: String,
    sid: u32,
    stream: Arc<AsyncMutex<TcpStream>>,
    tx: mpsc::Sender<Vec<u8>>,
    rx: Arc<AsyncMutex<mpsc::Receiver<Vec<u8>>>>,
    selected_slave_ip: Option<String>,
}

impl Client {
    fn new(ip_addr: String, sid: u32, stream: Arc<AsyncMutex<TcpStream>>) -> Self {
        let (tx, rx) = mpsc::channel(100);
        let client = Self { ip_addr, sid, stream: stream.clone(), tx: tx.clone(), rx: Arc::new(AsyncMutex::new(rx)), selected_slave_ip: None };
        client
    }
}

struct ProxyManager {
    // Mode how master node works
    client_assign_mode: u8, // 1 or 2
    slave_recovery_mode: u8, // 1 or 2

    // Slaves and clients
    slaves: Arc<AsyncMutex<HashMap<String, Slave>>>,
    clients: Arc<AsyncMutex<HashMap<u32, Client>>>,

    // Broadcast channel from client to slaves
    broadcast_tx: broadcast::Sender<(u32, Vec<u8>)>,
    broadcast_rx: broadcast::Receiver<(u32, Vec<u8>)>,

    // Map client IP addresses to slave IPs in Client Assignment Mode 1
    cli_ip_to_slave: Arc<AsyncMutex<HashMap<String, String>>>,
}

impl ProxyManager {
    fn new() -> Self {
        let (broadcast_tx, broadcast_rx) = broadcast::channel(100);
        Self {
            client_assign_mode: 1,
            slave_recovery_mode: 2,
            slaves: Arc::new(AsyncMutex::new(HashMap::new())),
            clients: Arc::new(AsyncMutex::new(HashMap::new())),
            broadcast_tx,
            broadcast_rx,
            cli_ip_to_slave: Arc::new(AsyncMutex::new(HashMap::new())),
        }
    }

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

        let available_slaves: Vec<_> = slave_list.iter()
            .filter_map(|(ip, slave)| {
                let weight = slave.net_speed; // Use net_speed as weight
                if weight > 0.0 {
                    Some((ip, weight))
                } else {
                    None
                }
            })
            .collect();

        if available_slaves.is_empty() {
            return None;
        }

        // If there's only one available slave, return it
        if available_slaves.len() == 1 {
            let (ip, _) = &available_slaves[0];
            return Some(ip.to_string());
        }

        let total_weight: f64 = available_slaves.iter().map(|(_, weight)| *weight).sum();
        let random_weight: f64 = rand::random::<f64>() * total_weight;
        let mut cumulative_weight = 0.0;

        // Select a slave based on weighted random selection
        for (ip, weight) in available_slaves {
            cumulative_weight += weight;
            if cumulative_weight >= random_weight {
                // If in Mode 1, store this slave for the client IP
                if self.client_assign_mode == 1 {
                    cli_ip_to_slave.insert(client_ip.clone(), ip.to_string());
                }

                return Some(ip.to_string());
            }
        }

        None
    }

    // Route data to the appropriate client using the session ID
    async fn route_to_client(&self, session_id: u32, payload: Vec<u8>) {
        // Lock the clients hashmap
        let clients = self.clients.lock().await;

        // Check if the session ID exists in the clients hashmap
        if let Some(client) = clients.get(&session_id) {
            // Send the payload to the client's mpsc sender
            if let Err(e) = client.tx.send(payload).await {
                log::error!("Failed to send data using client's mpsc channel tx {}: {}", session_id, e);
            }
        }
    }

    // Add a session ID to a specific slave
    async fn add_session_to_slave(&self, slave_ip: &str, session_id: u32) {
        if let Some(slave) = self.slaves.lock().await.get(slave_ip) {
            slave.add_client_session(session_id).await;
        } else {
            log::warn!("No slave found for IP: {}", slave_ip);
        }
    }

    // Remove a session ID from a specific slave
    async fn remove_session_from_slave(&self, slave_ip: &str, session_id: u32) {
        if let Some(slave) = self.slaves.lock().await.get(slave_ip) {
            slave.remove_client_session(session_id).await;
        } else {
            log::warn!("No slave found for IP: {}", slave_ip);
        }
    }

    // Assign a slave to the client
    async fn assign_slave_to_client(&self, client_ip: String, slave_ip: String, session_id: u32) {
        let mut ip_map = self.cli_ip_to_slave.lock().await;
        ip_map.insert(client_ip.clone(), slave_ip.clone());

        self.add_session_to_slave(&slave_ip, session_id).await;
    }

    // Handle slave disconnection based on slave_recovery_mode
    async fn handle_slave_disconnection(&self, slave_ip: &str) {
        if self.slave_recovery_mode == 1 {
            log::info!("Waiting for slave {} to recover", slave_ip);
            // In this mode, we simply wait for the slave to reconnect
            // Recovery logic should be handled in the connection loop
        } else if self.slave_recovery_mode == 2 {
            let mut slaves = self.slaves.lock().await;

            if slaves.remove(slave_ip).is_some() {
                log::info!("Removed disconnected slave: {}", slave_ip);
            } else {
                log::warn!("Attempted to remove non-existent slave: {}", slave_ip);
                return;
            }

            let mut ip_to_slave_map = self.cli_ip_to_slave.lock().await;

            // Remove all IPs that were mapped to the disconnected slave
            ip_to_slave_map.retain(|_, mapped_slave_ip| {
                if mapped_slave_ip == slave_ip {
                    log::info!("Removing IP mapped to disconnected slave: {}", slave_ip);
                    false
                } else {
                    true
                }
            });

            log::info!("Slave {} disconnected. Clients will be assigned to new slaves on new connections.", slave_ip);
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

// Utility function to convert an integer to bytes
fn u32_to_bytes(value: u32) -> [u8; 4] {
    value.to_be_bytes()
}

// Prepend the session ID and the payload length
fn build_frame(session_id: u32, data: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(8 + data.len());
    frame.extend_from_slice(&session_id.to_be_bytes()); // 4 bytes for session ID
    frame.extend_from_slice(&(data.len() as u32).to_be_bytes()); // 4 bytes for payload length
    frame.extend_from_slice(data); // actual data
    frame
}

// Function to handle a single slave's I/O operations for all clients using it (multiplexing)
async fn handle_slave_io(
    slave: Slave,
    proxy_manager: Arc<ProxyManager>,
) -> Result<(), std::io::Error> {
    let mut slave_stream = slave.stream.lock().await;
    let mut session_id = 0;
    let mut buffer = [0u8; MAX_BUF_SIZE];
    let mut accumulated_buffer: Vec<u8> = Vec::new();
    let mut header_read = false;
    let mut expected_payload_len = 0;

    // Resubscribe to the broadcast channel
    let mut cli_broadcast_rx = proxy_manager.broadcast_rx.resubscribe();

    loop {
        tokio::select! {
            // Handle incoming traffic from the slave
            len = slave_stream.read(&mut buffer) => {
                let len = len?;
                if len == 0 {
                    break;  // Slave connection closed
                }

                accumulated_buffer.extend_from_slice(&buffer[..len]);

                // Process the incoming buffer
                while accumulated_buffer.len() >= 8 {
                    if !header_read && accumulated_buffer.len() >= 8 {
                        session_id = bytes_to_u32(&accumulated_buffer[0..4]);
                        expected_payload_len = u32::from_be_bytes([
                            accumulated_buffer[4],
                            accumulated_buffer[5],
                            accumulated_buffer[6],
                            accumulated_buffer[7],
                        ]) as usize;
                        header_read = true;
                        accumulated_buffer.drain(0..8);  // Remove the header bytes
                    }

                    if header_read && accumulated_buffer.len() >= expected_payload_len {
                        // Extract the payload and route to the correct client
                        let payload = accumulated_buffer.drain(0..expected_payload_len).collect::<Vec<u8>>();
                        log::info!("Slave -> Client: sid {}, size: {} bytes", session_id.clone(), expected_payload_len);
                        proxy_manager.route_to_client(session_id, payload).await;

                        header_read = false;
                        expected_payload_len = 0;
                    } else {
                        break;  // Wait for more data
                    }
                }
            }

            // Handle traffic from clients (broadcast receiver)
            Ok((client_session_id, client_data)) = cli_broadcast_rx.recv() => {
                // Check if the session ID is associated with this slave
                let session_ids = slave.cli_sids.lock().await;
                if session_ids.contains(&client_session_id) {
                    log::info!("Client -> Slave: sid {}, size: {} bytes", client_session_id, client_data.len());
                    let frame = build_frame(client_session_id, &client_data);

                    // Write the frame to the slave stream
                    match slave_stream.write_all(&frame).await {
                        Ok(_) => {
                            log::info!("Successfully wrote frame to slave: sid {}", client_session_id);
                        }
                        Err(e) => {
                            log::error!("Failed to write frame to slave: sid {}, error: {}", client_session_id, e);
                        }
                    }
                    slave_stream.flush().await?;
                }
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
    proxy_manager: Arc<ProxyManager>,
) -> Result<(), std::io::Error> {
    let mut client_buffer = [0u8; MAX_BUF_SIZE];

    let client = {
        let clients = proxy_manager.clients.lock().await;
        if let Some(client) = clients.get(&session_id) {
            client.clone() // Assuming Client is Clone
        } else {
            log::warn!("Client not found for session ID {}", session_id);
            return Ok(()); // Exit if the client is not found
        }
    };

    // Lock the receiver
    let mut rx = client.rx.lock().await;
    let mut cli_stream = client.stream.lock().await;

    // Main loop to handle continuous traffic between client and slave
    loop {
        tokio::select! {
            // Read from client and send to slave via the mpsc channel
            client_read = {
                cli_stream.read(&mut client_buffer)
            } => {
                let len = client_read?;
                if len > 0 {
                    log::info!("Broadcasting: sid {}, size: {} bytes", session_id.clone(), len);
                    let _ = proxy_manager.broadcast_tx.send((session_id, client_buffer[..len].to_vec()));
                } else {
                    break;  // Client connection closed
                }
            },

            // Read from the slave and send to client
            Some(response) = rx.recv() => {
                // Send the response back to the client
                cli_stream.write_all(&response[..]).await?;
                cli_stream.flush().await?;
            },
        }
    }

    // Cleanup after the session ends
    {
        let mut clients = proxy_manager.clients.lock().await;
        if let Some(client) = clients.remove(&session_id) {
            log::info!("Removed client session ID {} from ProxyManager.", session_id);
    
            // Remove the session ID from the associated slave541
            if let Some(selected_slave_ip) = client.selected_slave_ip {
                proxy_manager.remove_session_from_slave(&selected_slave_ip, session_id).await;
            }
        }
    }

    // Close the client stream
    log::info!("Closing client stream for session ID {}.", session_id);
    drop(cli_stream);

    Ok(())
}

// Send a command to the slave to download a file and report back the download speed
async fn send_speed_test_command(slave_stream: &mut TcpStream) -> Result<f64, Box<dyn std::error::Error>> {
    let speed_test_url = "https://speed.cloudflare.com/__down?bytes=5000000";

    // Send the speed test command to the slave
    let command = format!("SPEED_TEST {}\n", speed_test_url);
    slave_stream.write_all(command.as_bytes()).await?;

    // Read the response from the slave (assuming the slave sends back the speed in Mbps)
    let mut response = vec![0u8; 1024];
    let bytes_read = slave_stream.read(&mut response).await?;
    
    // Parse the response (expecting a string like "SPEED 12.34\n")
    let response_str = String::from_utf8_lossy(&response[..bytes_read]);
    if let Some(speed_str) = response_str.strip_prefix("SPEED ") {
        let download_speed_mbps: f64 = speed_str.trim().parse()?;
        Ok(download_speed_mbps)
    } else {
        Err("Invalid response from slave".into())
    }
}

async fn handle_slave_connections(slave_listener: TcpListener, proxy_manager: Arc<ProxyManager>) -> Result<(), Box<dyn Error>> {
    loop {
        let (slave_stream, slave_addr) = match slave_listener.accept().await {
            Err(e) => {
                log::error!("Error accepting connection: {}", e);
                return Err(Box::new(e)); // Return the error instead of Ok
            }
            Ok(p) => p,
        };

        let raw_stream = slave_stream.into_std().unwrap();
        raw_stream.set_keepalive(Some(std::time::Duration::from_secs(KEEP_ALIVE_DURATION))).unwrap();
        let mut slave_stream = TcpStream::from_std(raw_stream).unwrap();

        log::info!("New Slave {}:{}", slave_addr.ip(), slave_addr.port());

		// Send a speed test request to the slave and measure the internet speed
        let download_speed_mbps = match send_speed_test_command(&mut slave_stream).await {
            Ok(speed) => speed,
            Err(e) => {
                log::error!("Failed to measure internet speed for slave {}: {}", slave_addr.ip(), e);
                1.0
            }
        };

        // Assign weight based on measured download speed
        let weight = download_speed_mbps.round() as isize;

        log::info!("Slave {} Weight: {}", slave_addr.ip(), weight);

        // Create a new Slave object
        let new_slave = Slave::new(slave_addr.ip().to_string(), weight as f64, slave_stream);

        // Update ProxyManager with the new slave
        proxy_manager.slaves.lock().await.insert(new_slave.ip_addr.clone(), new_slave.clone());

        // Spawn a task to handle the slave's I/O operations
        let proxy_manager_clone = Arc::clone(&proxy_manager);
        let new_slave_clone = new_slave.clone();
        tokio::spawn(async move {
            // Call the slave IO handler for the new slave
            let result = handle_slave_io(new_slave_clone, proxy_manager_clone).await;
            if let Err(e) = result {
                log::error!("Error handling IO for slave {}: {}", slave_addr.ip(), e);
            }
        });
    }
}

#[tokio::main]
async fn main() -> io::Result<()>  {
    // Initialize logging system
    SimpleLogger::new()
        .with_utc_timestamps()
        .with_colors(true)
        .init()
        .unwrap();
    ::log::set_max_level(LevelFilter::Info);

    // Parse command line arguments
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
            log::error!("Invalid proxy mode. Using default (stick).");
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
                    log::error!("Invalid slave recovery mode. Using default (reconnect).");
                    1
                }
            })
            .unwrap_or(1)
    } else {
        // For nonstick mode, we don't handle slave_recovery
        log::info!("Slave recovery is ignored in nonstick mode.");
        0 // Use 0 to signify that slave recovery isn't relevant in nonstick mode
    };

    // Global store of proxy system
    let proxy_manager = Arc::new(ProxyManager {
        client_assign_mode,
        slave_recovery_mode,
        slaves: Arc::new(AsyncMutex::new(HashMap::new())),
        clients: Arc::new(AsyncMutex::new(HashMap::new())),
        broadcast_tx: broadcast::channel(100).0,
        broadcast_rx: broadcast::channel(100).1,
        cli_ip_to_slave: Arc::new(AsyncMutex::new(HashMap::new())),
    });

    if matches.opt_count("t") > 0 {
        let master_addr: String = matches.opt_str("t").unwrap_or_else(|| {
            log::error!("Not found listen port. eg: net-relay -t 0.0.0.0:8000 -s 0.0.0.0:1080");
            std::process::exit(1);
        });

        let socks_addr: String = matches.opt_str("s").unwrap_or_else(|| {
            log::error!("Not found listen port. eg: net-relay -t 0.0.0.0:8000 -s 0.0.0.0:1080");
            std::process::exit(1);
        });

        log::info!("Waiting for Slave nodes on {}", master_addr);

        let slave_listener = match TcpListener::bind(&master_addr).await {
            Err(e) => {
                log::error!("Failed to bind slave listener: {}", e);
                return Ok(());
            }
            Ok(p) => p,
        };

        tokio::spawn({
            let proxy_manager = Arc::clone(&proxy_manager);
            async move {
                if let Err(e) = handle_slave_connections(slave_listener, proxy_manager).await {
                    log::error!("Connection handler error: {}", e);
                }
            }
        });

        log::info!("Waiting for SOCKS5 clients on {}", socks_addr);

        let client_listener = match TcpListener::bind(&socks_addr).await {
            Err(e) => {
                log::error!("Failed to bind SOCKS5 listener: {}", e);
                return Ok(());
            }
            Ok(p) => p,
        };

        loop {
            let (client_stream, client_addr) = match client_listener.accept().await {
                Ok(stream) => stream,
                Err(e) => {
                    log::error!("Error accepting client connection: {}", e);
                    continue; // Continue the loop on error
                }
            };

            log::info!("New Client: {}", client_addr.ip());

			// Generate a new session ID for this client
			let session_id = rand::random::<u32>();

            // Retrieve an available slave stream
            let client_ip = client_addr.ip().to_string();
            if let Some(slave_ip) = proxy_manager.get_available_slave_ip(&client_ip).await {
                // Lock the incoming client stream
                let raw_stream = client_stream.into_std().unwrap();
                raw_stream.set_keepalive(Some(std::time::Duration::from_secs(KEEP_ALIVE_DURATION))).unwrap();
                let client_stream = Arc::new(AsyncMutex::new(TcpStream::from_std(raw_stream).unwrap()));

                // Create a new Client object
                let client = Client::new(client_ip, session_id, client_stream.clone());

                // Register the client in the ProxyManager
                proxy_manager.clients.lock().await.insert(session_id, client);

                // Add the session ID to the selected slave
                proxy_manager.add_session_to_slave(&slave_ip, session_id).await;

                // Spawn a task to handle traffic between the client and the assigned slave
                let proxy_manager_clone = Arc::clone(&proxy_manager);
                tokio::spawn(handle_client_io(session_id, proxy_manager_clone));
            } else {
                log::warn!("No available slave for client with session ID {}", session_id);
            }
        }
    } else {
        usage(&program, &opts);
    }
    Ok(())
}
