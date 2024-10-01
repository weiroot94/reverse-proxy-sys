mod utils;
mod socks;
mod LBlib;

use LBlib::{random_weight::*, roundrobin_weight::*, smooth_weight::*, Weight}; // Import items from the LBlib module

use std::collections::HashMap;
use std::error::Error; // Import the Error trait
use log::LevelFilter;
use net2::TcpStreamExt;
use simple_logger::SimpleLogger;
use getopts::Options;
use tokio::{io::{self, AsyncWriteExt, AsyncReadExt}, task, net::{TcpListener, TcpStream}};
use utils::MAGIC_FLAG;
use utils::MODULAS;
use std::sync::{Arc, Mutex};

struct ProxyManager {
    round_robin_weight: Arc<Mutex<RoundrobinWeight<String>>>,
    streams: Arc<Mutex<HashMap<String, Arc<Mutex<TcpStream>>>>>,
    matches: Arc<Mutex<HashMap<String, String>>>,
}

impl ProxyManager {
    // Constructor for initializing ProxyManager
    fn new() -> Self {
        ProxyManager {
            round_robin_weight: Arc::new(Mutex::new(RoundrobinWeight::new())),
            streams: Arc::new(Mutex::new(HashMap::new())),
            matches: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    // Add weight for slave IP
    fn add_weight(&self, ip: String, weight: isize) {
        let mut g_sw = self.round_robin_weight.lock().unwrap();
        g_sw.add(ip, weight);
    }

    // Insert a new stream for the slave IP
    fn insert_stream(&self, ip: String, stream: TcpStream) {
        let mut g_streams = self.streams.lock().unwrap();
        g_streams.insert(ip, Arc::new(Mutex::new(stream)));
    }

    // Get a stream by IP
    fn get_stream(&self, ip: &String) -> Option<Arc<Mutex<TcpStream>>> {
        let g_streams = self.streams.lock().unwrap();
        g_streams.get(ip).cloned()
    }

    // Get or assign a new slave IP based on the client IP
    fn get_or_assign_slave(&self, client_ip: String) -> String {
        let mut g_matches = self.matches.lock().unwrap();
        match g_matches.get(&client_ip) {
            Some(slave_ip) => slave_ip.clone(),
            None => {
                let mut g_sw = self.round_robin_weight.lock().unwrap();
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
    let brief = format!("Usage: {} [-ltdr] [IP_ADDRESS] [-s] [IP_ADDRESS]",
                        program_name);
    print!("{}", opts.usage(&brief));
}

async fn handle_connections(slave_listener: TcpListener, proxy_manager: Arc<ProxyManager>) -> Result<(), Box<dyn Error>> {
    loop {
        let (slave_stream, slave_addr) = match slave_listener.accept().await {
            Err(e) => {
                log::error!("error: {}", e);
                return Err(Box::new(e)); // Return the error instead of Ok
            }
            Ok(p) => p,
        };

        let raw_stream = slave_stream.into_std().unwrap();
        raw_stream.set_keepalive(Some(std::time::Duration::from_secs(10))).unwrap();
        let mut slave_stream = TcpStream::from_std(raw_stream).unwrap();

        log::info!("Accepted slave from: {}:{}", slave_addr.ip(), slave_addr.port());

		// Send reserved code to announce speed test to the client side
		if let Err(e) = slave_stream.write_all(&[MAGIC_FLAG[1]]).await{
			log::error!("error : {}" , e);
		};

		// 5MB data to simulate download speed test
        let test_data = vec![0u8; 5_000_000];

		// Send the test data to the client
        match slave_stream.write_all(&test_data).await {
            Ok(_) => log::info!("Sent dummy payload to check client's net speed"),
            Err(e) => {
                log::error!("Failed to send data to client: {}", e);
                continue;
            }
        }

		// Wait for the client to acknowledge the time taken to receive the data
        let mut buffer = [0u8; 1024];
        let bytes_read = match slave_stream.read(&mut buffer).await {
            Ok(bytes) if bytes > 0 => bytes,
            _ => continue,
        };

		// Parse the response time sent by the client
        let received_time = String::from_utf8_lossy(&buffer[..bytes_read]);
        let client_time: f64 = match received_time.trim().parse() {
            Ok(value) => value,
            Err(_) => {
                log::warn!("Failed to parse client's time");
                continue;
            }
        };

		// Calculate the speed (5MB in Mbps)
        let data_size = 5_000_000.0 * 8.0 / 1_000_000.0;
        let download_speed_mbps = data_size / client_time;

        log::info!(
            "Client {} download speed: {:.2} Mbps",
            slave_addr,
            download_speed_mbps
        );

		let weight = (download_speed_mbps.round() as isize);

        log::info!("{}'s weight is {}", slave_addr.ip(), weight);

		proxy_manager.add_weight(slave_addr.ip().to_string(), weight);
        proxy_manager.insert_stream(slave_addr.ip().to_string(), slave_stream);
    }
}

#[tokio::main]
async fn main() -> io::Result<()>  {
	SimpleLogger::new().with_utc_timestamps().with_utc_timestamps().with_colors(true).init().unwrap();
	::log::set_max_level(LevelFilter::Info);

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

	opts.optopt("d",
				"transfer data",
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
		let local_address : String = match match matches.opt_str("l"){
			Some(p) => p,
			None => {
				log::error!("not found listen port . eg : net-relay -l 0.0.0.0:8000");
				return Ok(());
			},
		}.parse(){
			Err(_) => {
				log::error!("not found listen port . eg : net-relay -l 0.0.0.0:8000");
				return Ok(());
			},
			Ok(p) => p
		};
		log::info!("listen to : {}" ,local_address);
		
		let listener = match TcpListener::bind(&local_address).await{
			Err(e) => {
				log::error!("error : {}", e);
				return Ok(());
			},
			Ok(p) => p
		};

		loop{
			let (stream , addr) = listener.accept().await.unwrap();
			log::info!("accept from : {}" ,addr);
			let raw_stream = stream.into_std().unwrap();
			raw_stream.set_keepalive(Some(std::time::Duration::from_secs(10))).unwrap();
			let stream = TcpStream::from_std(raw_stream).unwrap();

			tokio::spawn(async {
				socks::socksv5_handle(stream).await;
			});
		}
	} else if matches.opt_count("t") > 0 {
		let master_addr : String = match match matches.opt_str("t"){
			Some(p) => p,
			None => {
				log::error!("not found listen port . eg : net-relay -t 0.0.0.0:8000 -s 0.0.0.0:1080");
				return Ok(());
			},
		}.parse(){
			Err(_) => {
				log::error!("not found listen port . eg : net-relay -t 0.0.0.0:8000 -s 0.0.0.0:1080");
				return Ok(());
			},
			Ok(p) => p
		};

		let master_data_addr : String = match match matches.opt_str("d"){
			Some(p) => p,
			None => {
				log::error!("not found listen port . eg : net-relay -t 0.0.0.0:8000 -d 0.0.0.0:8001 -s 0.0.0.0:1080");
				return Ok(());
			},
		}.parse(){
			Err(_) => {
				log::error!("not found listen port . eg : net-relay -t 0.0.0.0:8000 -d 0.0.0.0:8001 -s 0.0.0.0:1080");
				return Ok(());
			},
			Ok(p) => p
		};

		let socks_addr : String = match match matches.opt_str("s"){
			Some(p) => p,
			None => {
				log::error!("not found listen port . eg : net-relay -t 0.0.0.0:8000 -s 0.0.0.0:1080");
				return Ok(());
			},
		}.parse(){
			Err(_) => {
				log::error!("not found listen port . eg : net-relay -t 0.0.0.0:8000 -s 0.0.0.0:1080");
				return Ok(());
			},
			Ok(p) => p
		};

		log::info!("listen to : {} waiting for slave" , master_addr);
		
		let slave_listener = match TcpListener::bind(&master_addr).await{
			Err(e) => {
				log::error!("error : {}", e);
				return Ok(());
			},
			Ok(p) => p
		};

		let slave_data_listener = match TcpListener::bind(&master_data_addr).await{
			Err(e) => {
				log::error!("error : {}", e);
				return Ok(());
			},
			Ok(p) => p
		};

		// Create a new thread
		tokio::spawn({
			let proxy_manager = Arc::clone(&proxy_manager); // Clone the Arc to move into the async block
			async move {
				if let Err(e) = handle_connections(slave_listener, proxy_manager).await {
					log::error!("Connection handler error: {}", e);
				}
			}
		});
	
		
		log::info!("listen to : {}" , socks_addr);
		
		let listener = match TcpListener::bind(&socks_addr).await{
			Err(e) => {
				log::error!("error : {}", e);
				return Ok(());
			},
			Ok(p) => p
		};

		loop {
			let (stream , client_addr) = listener.accept().await.unwrap();
			log::info!("accept from client : {}" , client_addr.ip());

			// Check if there are any slave streams available
			{
				let g_streams = proxy_manager.streams.lock().unwrap();
				if g_streams.is_empty() {
					log::warn!("No slave is connected to master");
					continue;
				}
			}

			let raw_stream = stream.into_std().unwrap();
			raw_stream.set_keepalive(Some(std::time::Duration::from_secs(10))).unwrap();
			let mut stream = TcpStream::from_std(raw_stream).unwrap();

			// Get or assign a corresponding slave node for the current client using ProxyManager
			let selected_slave_ip = proxy_manager.get_or_assign_slave(client_addr.ip().to_string());

			// Retrieve the stream corresponding to the selected slave IP
			if let Some(slave_stream_arc) = proxy_manager.get_stream(&selected_slave_ip) {
				log::info!("Found TcpStream for IP: {}", selected_slave_ip);
	
				let mut slave_stream = slave_stream_arc.lock().unwrap();
	
				// Send a magic flag to the slave node
				if let Err(e) = slave_stream.write_all(&[MAGIC_FLAG[0]]).await {
					log::error!("Error: {}", e);
					break;
				}
	
				// Wait for the secondary connection to be established (slave connection)
				let (proxy_stream, slave_addr) = match slave_data_listener.accept().await {
					Err(e) => {
						log::error!("Error: {}", e);
						return Ok(());
					}
					Ok(p) => p,
				};
	
				let raw_proxy_stream = proxy_stream.into_std().unwrap();
				raw_proxy_stream.set_keepalive(Some(std::time::Duration::from_secs(10))).unwrap();
				let mut proxy_stream = TcpStream::from_std(raw_proxy_stream).unwrap();
	
				// Spawn a task to handle the proxying between the client and the slave
				tokio::spawn(async move {
					let mut buf1 = [0u8; 1024];
					let mut buf2 = [0u8; 1024];
	
					loop {
						tokio::select! {
							a = proxy_stream.read(&mut buf1) => {
								let len = match a {
									Err(_) => break,
									Ok(p) => p
								};
								match stream.write_all(&buf1[..len]).await {
									Err(_) => break,
									Ok(_) => {}
								}
								if len == 0 {
									break;
								}
							},
							b = stream.read(&mut buf2) =>  {
								let len = match b {
									Err(_) => break,
									Ok(p) => p
								};
								match proxy_stream.write_all(&buf2[..len]).await {
									Err(_) => break,
									Ok(_) => {}
								}
								if len == 0 {
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
	 }else if matches.opt_count("r") > 0 {
		let fulladdr : String = match match matches.opt_str("r"){
			Some(p) => p,
			None => {
				log::error!("not found ip . eg : net-relay -r 192.168.0.1:8000");
				return Ok(());
			},
		}.parse(){
			Err(_) => {
				log::error!("not found ip . eg : net-relay -r 192.168.0.1:8000");
				return Ok(());
			},
			Ok(p) => p
		};
		
		let master_stream = match TcpStream::connect(fulladdr.clone()).await{
			Err(e) => {
				log::error!("error : {}", e);
				return Ok(());
			},
			Ok(p) => p
		};

		let raw_stream = master_stream.into_std().unwrap();
		raw_stream.set_keepalive(Some(std::time::Duration::from_secs(10))).unwrap();
		let mut master_stream = TcpStream::from_std(raw_stream).unwrap();

		log::info!("connect to {} success" ,fulladdr );
		loop {
			let mut buf = [0u8 ; 1];
			match master_stream.read_exact(&mut buf).await{
				Err(e) => {
					log::error!("error : {}", e);
					return Ok(());
				},
				Ok(p) => p
			};

			if buf[0] == MAGIC_FLAG[0] {
				let stream = match TcpStream::connect(fulladdr.clone()).await{
					Err(e) => {
						log::error!("error : {}", e);
						return Ok(());
					},
					Ok(p) => p
				};

				let raw_stream = stream.into_std().unwrap();
				raw_stream.set_keepalive(Some(std::time::Duration::from_secs(10))).unwrap();
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
