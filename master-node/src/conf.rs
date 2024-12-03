use getopts::Options;
use std::env;
use log::error;
use std::sync::Arc;
pub struct Config {
    pub proxy_mode: u8,      // 1 for sticky, 2 for non-sticky
    pub allowed_locations: Arc<Vec<String>>, // Comma-separated list of allowed countries
    pub verbosity: String,   // Verbosity level (trace, debug, info, warn, error)
    pub master_addr: String, // Master address for slave connections
    pub socks_addr: String,  // Address for SOCKS5 client connections
}

pub fn parse_args() -> Config {
    let args: Vec<String> = env::args().collect();
    let program = args[0].clone();

    let mut opts = Options::new();
    opts.optopt("t", "transfer", "The address accept from slave socks5 server connection", "TRANSFER_ADDRESS");
    opts.optopt("s", "server", "The address on which to listen local socks5 server", "TRANSFER_ADDRESS");
    opts.optopt("p", "proxy_mode", "Set the proxy mode: stick (1) or nonstick (2)", "MODE");
    opts.optopt("l", "allowed-locations", "Comma-separated list of allowed countries for slaves", "LOCATIONS");
    opts.optopt("v", "verbosity", "Set the verbosity level (trace, debug, info, warn, error)", "LEVEL");

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
            error!("Invalid proxy mode. Using default (nonstick).");
            2
        }
    };

    // Parse the allowed locations
    let allowed_locations = Arc::new(
        matches
            .opt_str("l")
            .unwrap_or_default()
            .split(',')
            .filter_map(|loc| {
                let trimmed = loc.trim();
                if !trimmed.is_empty() {
                    Some(trimmed.to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<String>>(),
    );

    // Determine the verbosity level
    let verbosity = matches.opt_str("v").unwrap_or_else(|| "info".to_string());

    // Parse the master and socks server addresses
    let master_addr = matches.opt_str("t").unwrap_or_else(|| {
        error!("Missing transfer address (-t).");
        std::process::exit(1);
    });
    let socks_addr = matches.opt_str("s").unwrap_or_else(|| {
        error!("Missing server address (-s).");
        std::process::exit(1);
    });

    Config {
        proxy_mode: client_assign_mode,
        allowed_locations,
        verbosity,
        master_addr,
        socks_addr,
    }
}


pub fn usage(program: &str, opts: &getopts::Options) {
    let binding = std::path::PathBuf::from(program);
    let program_name = binding
        .file_stem()
        .unwrap()
        .to_str()
        .unwrap();
    let brief = format!("Usage: {} [OPTIONS]", program_name);
    print!("{}", opts.usage(&brief));
}

