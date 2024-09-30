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

// Using lazy_static to define global variables
#[macro_use]
extern crate lazy_static;

lazy_static! {
	//for selecting slave node using weight
    static ref G_SW: Arc<Mutex<RoundrobinWeight<String>>> = Arc::new(Mutex::new(RoundrobinWeight::new()));

	//for getting tcp stream from ip
    static ref G_STREAMS: Arc<Mutex<HashMap<String, Arc<Mutex<TcpStream>>>>> = Arc::new(Mutex::new(HashMap::new()));

	//for getting slave ip from client ip
    static ref G_MATCHES: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
}

// Function to get the value from the HashMap
fn get_value( key: String) -> String {
	let mut map = G_MATCHES.lock().unwrap();

    match map.get(&key) {
        Some(value) => value.clone(), // Return the value if it exists
        None => "EMPTY".to_string(),  // Return "EMPTY" if it doesn't
    }
}

fn usage(program: &str, opts: &Options) {
    let program_path = std::path::PathBuf::from(program);
    let program_name = program_path.file_stem().unwrap().to_str().unwrap();
    let brief = format!("Usage: {} [-ltdr] [IP_ADDRESS] [-s] [IP_ADDRESS]",
                        program_name);
    print!("{}", opts.usage(&brief));
}

async fn handle_connections(slave_listener: TcpListener) -> Result<(), Box<dyn Error>> {
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

        let mut weightbuffer = [0u8; 1024];
        let bytes_read = slave_stream.read(&mut weightbuffer).await?;

        // Convert the buffer to a string slice
        let string_data = std::str::from_utf8(&weightbuffer[..bytes_read])
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // Parse the string to f64, multiply by 100, and convert to isize
        let value: isize = string_data.trim().parse::<f64>()
            .map(|v| (v ).round() as isize)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        log::info!("{}'s weight is {}, {}", slave_addr.ip(), string_data, value/MODULAS);

		let mut g_sw = G_SW.lock().unwrap();
		let mut g_streams = G_STREAMS.lock().unwrap();

        g_sw.add(slave_addr.ip().to_string(), value/MODULAS);
        g_streams.insert(slave_addr.ip().to_string(),  Arc::new(Mutex::new(slave_stream)));
    }
}

#[tokio::main]
async fn main() -> io::Result<()>  {
	SimpleLogger::new().with_utc_timestamps().with_utc_timestamps().with_colors(true).init().unwrap();
	::log::set_max_level(LevelFilter::Info);

    let args: Vec<String> = std::env::args().collect();
    let program = args[0].clone();

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
		tokio::spawn(async move {
			if let Err(e) = handle_connections(slave_listener).await {
				log::error!("Connection handler error: {}", e);
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

			let mut g_streams = G_STREAMS.lock().unwrap();
			if(g_streams.is_empty())
			{
				log::warn!("no slave is connected to master");
				continue;
			}

			let raw_stream = stream.into_std().unwrap();
			raw_stream.set_keepalive(Some(std::time::Duration::from_secs(10))).unwrap();
			let mut stream = TcpStream::from_std(raw_stream).unwrap();

			// Check corresponding slave node with current client
			let matched_slave = get_value(client_addr.ip().to_string());
			let mut selected_slave_ip: String = "".to_string();
			let mut new_string: String = "".to_string();

			if matched_slave == "EMPTY" {
				log::warn!("should select new slave");
				//select slave node according to algorithm
				let mut g_sw = G_SW.lock().unwrap();
				new_string = g_sw.next().unwrap();
				selected_slave_ip = new_string.clone();
				
				//add 2 ips into hash map
				let mut g_ipmatch = G_MATCHES.lock().unwrap();
				let ipclone: String = new_string.clone();
				g_ipmatch.insert( client_addr.ip().to_string(), ipclone);
			} else {
				new_string = matched_slave.clone();
				selected_slave_ip = matched_slave.clone();
			}

			if let Some(stream_s) = g_streams.get(&selected_slave_ip) {
				log::info!("Found TcpStream for IP: {}", new_string);
				let mut slave_stream = stream_s.lock().unwrap();

				if let Err(e) = slave_stream.write_all(&[MAGIC_FLAG[0]]).await{
					log::error!("error : {}" , e);
					break;
				};

				let (proxy_stream , slave_addr) = match slave_data_listener.accept().await{
					Err(e) => {
						log::error!("error : {}", e);
						return Ok(());
					},
					Ok(p) => p
				};

				let raw_stream = proxy_stream.into_std().unwrap();
				raw_stream.set_keepalive(Some(std::time::Duration::from_secs(10))).unwrap();
				let mut proxy_stream = TcpStream::from_std(raw_stream).unwrap();
	
				//log::info!("accept from slave : {}:{}" , slave_addr.ip() , slave_addr.port() );
				task::spawn(async move {
					let mut buf1 = [0u8 ; 1024];
					let mut buf2 = [0u8 ; 1024];
	
					loop{
						tokio::select! {
							a = proxy_stream.read(&mut buf1) => {
			
								let len = match a {
									Err(_) => {
										break;
									}
									Ok(p) => p
								};
								match stream.write_all(&buf1[..len]).await {
									Err(_) => {
										break;
									}
									Ok(p) => p
								};
			
								if len == 0 {
									break;
								}
							},
							b = stream.read(&mut buf2) =>  { 
								let len = match b{
									Err(_) => {
										break;
									}
									Ok(p) => p
								};
								match proxy_stream.write_all(&buf2[..len]).await {
									Err(_) => {
										break;
									}
									Ok(p) => p
								};
								if len == 0 {
									break;
								}
							},
						}
					}
					//log::info!("transfer [{}:{}] finished" , slave_addr.ip() , slave_addr.port());
				});
			} else {
				println!("TcpStream not found.");
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
