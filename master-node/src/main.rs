mod utils;
mod socks;
mod LBlib;

use LBlib::{random_weight::*, roundrobin_weight::*, smooth_weight::*, Weight};
use std::collections::HashMap;
use std::error::Error;
use log::LevelFilter;
use net2::TcpStreamExt;
use simple_logger::SimpleLogger;
use getopts::Options;
use tokio::{io::{self, AsyncWriteExt, AsyncReadExt}, task, net::{TcpListener, TcpStream}, signal};
use utils::MAGIC_FLAG;
use utils::MODULAS;
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc;
use tokio::sync::broadcast;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

const KEEP_ALIVE_DURATION: u64 = 10;
const MAX_BUF_SIZE: usize = 4096;

struct ProxyManager {
    // Slave IP Round Robin Weighted List
    slave_ip_list_weighted: Arc<AsyncMutex<RoundrobinWeight<String>>>,
    // Slave IP to Stream
    slave_streams: Arc<AsyncMutex<HashMap<String, Arc<AsyncMutex<TcpStream>>>>>,
    // Slave IP to Client Count (Load)
    slave_load: Arc<AsyncMutex<HashMap<String, usize>>>,
    // Slave IP to flag indicating if slave io handler is running
    slave_is_running: Arc<AsyncMutex<HashMap<String, Arc<AtomicBool>>>>,
    // New field: Maps Slave IP to a list of session IDs
    slave_sessions: Arc<AsyncMutex<HashMap<String, Vec<u32>>>>,
    // Session ID to Client Sender
    clients: Arc<AsyncMutex<HashMap<u32, mpsc::Sender<Vec<u8>>>>>,
}

impl ProxyManager {
    fn new() -> Self {
        ProxyManager {
            slave_ip_list_weighted: Arc::new(AsyncMutex::new(RoundrobinWeight::new())),
            slave_streams: Arc::new(AsyncMutex::new(HashMap::new())),
            slave_load: Arc::new(AsyncMutex::new(HashMap::new())),
            slave_is_running: Arc::new(AsyncMutex::new(HashMap::new())),
            slave_sessions: Arc::new(AsyncMutex::new(HashMap::new())),
            clients: Arc::new(AsyncMutex::new(HashMap::new())),
        }
    }

    // Add weight for slave IP
    async fn add_weight(&self, ip: String, weight: isize) {
        let mut g_sw = self.slave_ip_list_weighted.lock().await;
        let mut g_load = self.slave_load.lock().await;
        g_sw.add(ip.clone(), weight);
        // Initialize the load as 0
        g_load.insert(ip, 0);
    }

    // Insert a new stream for the slave IP
    async fn insert_stream(&self, ip: String, stream: TcpStream) {
        let mut g_streams = self.slave_streams.lock().await;
        g_streams.insert(ip, Arc::new(AsyncMutex::new(stream)));
    }

    // Get a stream by IP
    async fn get_stream(&self, ip: &String) -> Option<Arc<AsyncMutex<TcpStream>>> {
        let g_streams = self.slave_streams.lock().await;
        g_streams.get(ip).cloned()
    }

    // Get or assign a new slave IP based on session Id
    // Assign a client to a slave with the least load
    async fn get_matched_slave_ip(&self, session_id: u32) -> String {
        let mut g_sw = self.slave_ip_list_weighted.lock().await;
        let mut g_load = self.slave_load.lock().await;
        let mut g_sessions = self.slave_sessions.lock().await;

        let slave_ip = g_sw.next().unwrap();
        g_load.entry(slave_ip.clone()).and_modify(|load| *load += 1);
        
        // Register the session ID to this slave
        g_sessions.entry(slave_ip.clone()).or_default().push(session_id);
    
        slave_ip
    }

    // Register a client sender for the session ID
    async fn register_client_sender(&self, session_id: u32, sender: mpsc::Sender<Vec<u8>>) {
        let mut clients = self.clients.lock().await;
        clients.insert(session_id, sender);
    }

    // Unregister a client sender and release the associated slave
    async fn unregister_client_sender(&self, session_id: u32) {
        let mut clients = self.clients.lock().await;
        clients.remove(&session_id);
    }

    // Route data to the appropriate client using the session ID
    async fn route_to_client(&self, session_id: u32, payload: Vec<u8>) {
        let clients = self.clients.lock().await;
        if let Some(sender) = clients.get(&session_id) {
            let _ = sender.send(payload).await;
        }
    }

    // Decrement load for a slave when a client disconnects
    // async fn release_slave(&self, session_id: u32) {
    //     let mut g_matches = self.matches.lock().await;
    //     if let Some(slave_ip) = g_matches.remove(&session_id) {
    //         let mut g_load = self.slave_load.lock().await;
    //         g_load.entry(slave_ip).and_modify(|load| *load -= 1);
    //     }
    // }

    // Function to check if the slave's I/O handler is already running
    async fn is_slave_running(&self, slave_ip: &str) -> bool {
        let g_running = self.slave_is_running.lock().await;
        if let Some(is_running) = g_running.get(slave_ip) {
            is_running.load(Ordering::Acquire)  // Check if I/O handler is running
        } else {
            false
        }
    }

    // Function to set the slave's running status
    async fn set_slave_running(&self, slave_ip: &str, running: bool) {
        let g_running = self.slave_is_running.lock().await;
        if let Some(is_running) = g_running.get(slave_ip) {
            is_running.store(running, Ordering::Release);  // Set the running status
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
    slave_ip: String,
    slave_stream_arc: Arc<AsyncMutex<TcpStream>>,
    proxy_manager: Arc<ProxyManager>,
    mut cli_broadcast_rx: broadcast::Receiver<(u32, Vec<u8>)>,
) -> Result<(), std::io::Error> {
    let mut slave_stream = slave_stream_arc.lock().await;
    let mut session_id = 0;
    let mut buffer = [0u8; MAX_BUF_SIZE];
    let mut accumulated_buffer: Vec<u8> = Vec::new();
    let mut header_read = false;
    let mut expected_payload_len = 0;

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
                let g_sessions = proxy_manager.slave_sessions.lock().await;
                if let Some(session_ids) = g_sessions.get(&slave_ip) {
                    if session_ids.contains(&client_session_id) {
                        log::info!("Client -> Slave: sid {}, size: {} bytes", client_session_id, client_data.len());
                        let frame = build_frame(client_session_id, &client_data);
                        slave_stream.write_all(&frame).await?;
                        slave_stream.flush().await?;
                    } else {
                        log::warn!("Client session ID {} is not associated with slave {}", client_session_id, slave_ip);
                    }
                }
            }
        }
    }

    // Mark the slave I/O as no longer running
    proxy_manager.set_slave_running(&slave_ip, false);

    Ok(())
}

// Function to handle traffic between a client and the slave
async fn handle_client_io(
    session_id: u32,
    mut client_stream: TcpStream,
    cli_broadcast_tx: broadcast::Sender<(u32, Vec<u8>)>,
    proxy_manager: Arc<ProxyManager>,
) -> Result<(), std::io::Error> {
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(100);

    // Register the client's sender in the ProxyManager
    proxy_manager.register_client_sender(session_id, tx).await;

    let mut client_buffer = [0u8; MAX_BUF_SIZE];

    // Main loop to handle continuous traffic between client and slave
    loop {
        tokio::select! {
            // Read from client and send to slave via the mpsc channel
            client_read = client_stream.read(&mut client_buffer) => {
                let len = client_read?;
                if len > 0 {
                    log::info!("Broadcasting: sid {}, size: {} bytes", session_id.clone(), len);
                    let _ = cli_broadcast_tx.send((session_id, client_buffer[..len].to_vec()));
                } else {
                    break;  // Client connection closed
                }
            },

            // Read from the slave and send to client
            Some(response) = rx.recv() => {
                client_stream.write_all(&response).await?;
                client_stream.flush().await?;
            }
        }
    }

    // Cleanup after the session ends
    proxy_manager.unregister_client_sender(session_id).await;

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

        proxy_manager.add_weight(slave_addr.ip().to_string(), weight).await;
        proxy_manager.insert_stream(slave_addr.ip().to_string(), slave_stream).await;
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

    // Global store of proxy system
    let proxy_manager = Arc::new(ProxyManager::new());

    let mut opts = Options::new();

    opts.optopt("l",
                "bind",
                "The address on which to listen socks5 server for incoming requests",
                "BIND_ADDR");

    opts.optopt("t",
                "transfer",
                "The address accept from slave socks5 server connection",
                "TRANSFER_ADDRESS");
    opts.optopt("r",
                "reverse",
                "reverse socks5 server connect to master",
                "TRANSFER_ADDRESS");

    opts.optopt("s",
                "server",
                "The address on which to listen local socks5 server",
                "TRANSFER_ADDRESS");

    let matches = opts.parse(&args[1..]).unwrap_or_else(|_| {
        usage(&program, &opts);
        std::process::exit(-1);
    });

    if matches.opt_count("l") > 0{
        let local_address: String = matches.opt_str("l").unwrap_or_else(|| {
            log::error!("Not found listen port. eg: net-relay -l 0.0.0.0:8000");
            std::process::exit(1);
        });
        log::info!("Listening on: {}", local_address);

        let listener = match TcpListener::bind(&local_address).await {
            Err(e) => {
                log::error!("Error: {}", e);
                return Ok(());
            }
            Ok(p) => p,
        };

        loop {
            let (stream, addr) = listener.accept().await.unwrap();
            log::info!("Accepted from: {}", addr);
            let raw_stream = stream.into_std().unwrap();
            raw_stream.set_keepalive(Some(std::time::Duration::from_secs(KEEP_ALIVE_DURATION))).unwrap();
            let stream = TcpStream::from_std(raw_stream).unwrap();

            tokio::spawn(async {
                socks::socksv5_handle(stream).await;
            });
        }
    } else if matches.opt_count("t") > 0 {
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

        let (cli_broadcast_tx, cli_broadcast_rx) = broadcast::channel::<(u32, Vec<u8>)>(100);

        loop {
            let (client_stream, client_addr) = client_listener.accept().await.unwrap();
            log::info!("New Client: {}", client_addr.ip());

			// Generate a new session ID for this client
			let session_id = rand::random::<u32>();

            let streams_available = {
                let g_streams = proxy_manager.slave_streams.lock().await;
                !g_streams.is_empty()
            };

            if !streams_available {
                log::warn!("No slave is connected to master");
                continue;
            }

            let raw_stream = client_stream.into_std().unwrap();
            raw_stream.set_keepalive(Some(std::time::Duration::from_secs(KEEP_ALIVE_DURATION))).unwrap();
            let client_stream = TcpStream::from_std(raw_stream).unwrap();

            // Subscribe to the broadcast channel for the slave
            let cli_broadcast_rx_clone = cli_broadcast_rx.resubscribe();

            // Get or assign a corresponding slave node for the current client using ProxyManager
            let selected_slave_ip = proxy_manager.get_matched_slave_ip(session_id).await;

            // Spawn a task to handle traffic between the client and the assigned slave
            if let Some(slave_stream_arc) = proxy_manager.get_stream(&selected_slave_ip).await {
                let proxy_manager_clone = Arc::clone(&proxy_manager);
                let slave_stream_arc_clone = Arc::clone(&slave_stream_arc);

                tokio::spawn(handle_client_io(session_id, client_stream, cli_broadcast_tx.clone(), proxy_manager_clone.clone()));

                if !proxy_manager.is_slave_running(&selected_slave_ip).await {
                    // Set the slave as running
                    proxy_manager.set_slave_running(&selected_slave_ip, true).await;

                    // Spawn only once per slave for slave -> client traffic
                    tokio::spawn(handle_slave_io(selected_slave_ip.clone(), slave_stream_arc_clone, proxy_manager_clone, cli_broadcast_rx_clone));
                }
            } else {
                log::warn!("No available slave for client");
            }
        }
    } else if matches.opt_count("r") > 0 {
        let fulladdr: String = matches.opt_str("r").unwrap_or_else(|| {
            log::error!("Not found IP. eg: net-relay -r 192.168.0.1:8000");
            std::process::exit(1);
        });

        let master_stream = TcpStream::connect(fulladdr.clone()).await?;

        let raw_stream = master_stream.into_std().unwrap();
        raw_stream.set_keepalive(Some(std::time::Duration::from_secs(KEEP_ALIVE_DURATION))).unwrap();
        let mut master_stream = TcpStream::from_std(raw_stream).unwrap();

        log::info!("Connected to {}", fulladdr);
        loop {
            let mut buf = [0u8; 1];
            if let Err(e) = master_stream.read_exact(&mut buf).await {
                log::error!("Error: {}", e);
                return Ok(());
            }

            if buf[0] == MAGIC_FLAG[0] {
                let stream = TcpStream::connect(fulladdr.clone()).await?;

                let raw_stream = stream.into_std().unwrap();
                raw_stream.set_keepalive(Some(std::time::Duration::from_secs(KEEP_ALIVE_DURATION))).unwrap();
                let stream = TcpStream::from_std(raw_stream).unwrap();

                task::spawn(async {
                    socks::socksv5_handle(stream).await;
                });
            }
        }
    } else {
        usage(&program, &opts);
    }
    Ok(())
}
