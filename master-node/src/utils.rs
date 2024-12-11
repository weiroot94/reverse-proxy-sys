use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

pub const CLIENT_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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
