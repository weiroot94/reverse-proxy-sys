use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

pub const CLIENT_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub const MAX_CONCURRENT_REQUESTS: usize = 100;

pub fn hash_ip(ip_addr: &str) -> usize {
    let mut hasher = DefaultHasher::new();
    ip_addr.hash(&mut hasher);
    hasher.finish() as usize
}

pub fn bytes_to_u32(bytes: &[u8]) -> u32 {
    let mut array = [0u8; 4];
    array.copy_from_slice(bytes);
    u32::from_be_bytes(array)
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
