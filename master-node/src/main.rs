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

const KEEP_ALIVE_DURATION: u64 = 10;

struct ProxyManager {
    round_robin_weight: Arc<AsyncMutex<RoundrobinWeight<String>>>,
    streams: Arc<AsyncMutex<HashMap<String, Arc<AsyncMutex<TcpStream>>>>>,
    matches: Arc<AsyncMutex<HashMap<String, String>>>,
}

impl ProxyManager {
    fn new() -> Self {
        ProxyManager {
            round_robin_weight: Arc::new(AsyncMutex::new(RoundrobinWeight::new())),
            streams: Arc::new(AsyncMutex::new(HashMap::new())),
            matches: Arc::new(AsyncMutex::new(HashMap::new())),
        }
    }

    // Add weight for slave IP
    async fn add_weight(&self, ip: String, weight: isize) {
        let mut g_sw = self.round_robin_weight.lock().await;
        g_sw.add(ip, weight);
    }

    // Insert a new stream for the slave IP
    async fn insert_stream(&self, ip: String, stream: TcpStream) {
        let mut g_streams = self.streams.lock().await;
        g_streams.insert(ip, Arc::new(AsyncMutex::new(stream)));
    }

    // Get a stream by IP
    async fn get_stream(&self, ip: &String) -> Option<Arc<AsyncMutex<TcpStream>>> {
        let g_streams = self.streams.lock().await;
        g_streams.get(ip).cloned()
    }

    // Get or assign a new slave IP based on the client IP
    async fn get_or_assign_slave(&self, client_ip: String) -> String {
        let mut g_matches = self.matches.lock().await;
        match g_matches.get(&client_ip) {
            Some(slave_ip) => slave_ip.clone(),
            None => {
                let mut g_sw = self.round_robin_weight.lock().await;
                let slave_ip = g_sw.next().unwrap();
                g_matches.insert(client_ip.clone(), slave_ip.clone());
                slave_ip
            }
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

async fn handle_connections(slave_listener: TcpListener, proxy_manager: Arc<ProxyManager>) -> Result<(), Box<dyn Error>> {
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

        log::info!("Accepted slave from: {}:{}", slave_addr.ip(), slave_addr.port());

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

        log::info!("Client {}'s weight: {}", slave_addr.ip(), weight);

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

        log::info!("Listening on {} waiting for slave", master_addr);

        let slave_listener = match TcpListener::bind(&master_addr).await {
            Err(e) => {
                log::error!("Failed to bind slave listener: {}", e);
                return Ok(());
            }
            Ok(p) => p,
        };

        // Create a new thread
        tokio::spawn({
            let proxy_manager = Arc::clone(&proxy_manager);
            async move {
                if let Err(e) = handle_connections(slave_listener, proxy_manager).await {
                    log::error!("Connection handler error: {}", e);
                }
            }
        });

        log::info!("SOCKS5 server is listening on {}", socks_addr);

        let client_listener = match TcpListener::bind(&socks_addr).await {
            Err(e) => {
                log::error!("Failed to bind SOCKS5 listener: {}", e);
                return Ok(());
            }
            Ok(p) => p,
        };

        loop {
            let (client_stream, client_addr) = client_listener.accept().await.unwrap();
            log::info!("Accepted connection from client: {}", client_addr.ip());

            let streams_available = {
                let g_streams = proxy_manager.streams.lock().await;
                !g_streams.is_empty()
            };

            if !streams_available {
                log::warn!("No slave is connected to master");
                continue;
            }

            let raw_stream = client_stream.into_std().unwrap();
            raw_stream.set_keepalive(Some(std::time::Duration::from_secs(KEEP_ALIVE_DURATION))).unwrap();
            let mut client_stream = TcpStream::from_std(raw_stream).unwrap();

            // Get or assign a corresponding slave node for the current client using ProxyManager
            let selected_slave_ip = proxy_manager.get_or_assign_slave(client_addr.ip().to_string()).await;

            // Retrieve the stream corresponding to the selected slave IP
            if let Some(slave_stream_arc) = proxy_manager.get_stream(&selected_slave_ip).await {
                log::info!("Found TcpStream for IP: {}", selected_slave_ip);
				let slave_stream_arc_clone = Arc::clone(&slave_stream_arc);

                // Spawn a task to handle the proxying between the client and the slave
                tokio::spawn(async move {
					let mut slave_stream = slave_stream_arc_clone.lock().await;
                    let mut buf1 = [0u8; 1024];
                    let mut buf2 = [0u8; 1024];

                    loop {
                        tokio::select! {
                            a = slave_stream.read(&mut buf1) => {
                                let len = match a {
                                    Err(_) => break,
                                    Ok(p) => p
                                };
                                if len == 0 {
                                    break;
                                }
                                if let Err(e) = client_stream.write_all(&buf1[..len]).await {
                                    log::error!("Write error: {}", e);
                                    break;
                                }
                            },
                            b = client_stream.read(&mut buf2) => {
                                let len = match b {
                                    Err(_) => break,
                                    Ok(p) => p
                                };
                                if len == 0 {
                                    break;
                                }
                                if let Err(e) = slave_stream.write_all(&buf2[..len]).await {
                                    log::error!("Write error: {}", e);
                                    break;
                                }
                            },
                        }
                    }
                });
            } else {
                log::warn!("TcpStream for selected slave IP not found.");
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
