use std::net::IpAddr;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token(pub u8);

pub trait Balance {
    type State;

    fn new(weights: &[u8]) -> Self;
    fn next(&self, state: &Self::State) -> Option<Token>;
}

#[derive(Debug)]
struct Node {
    hash: u32,
    token: Token,
}

#[derive(Debug)]
pub struct IpHash {
    nodes: Vec<Node>,
    total: u8,
}

impl Balance for IpHash {
    type State = IpAddr;

    fn new(weights: &[u8]) -> Self {
        assert!(weights.len() <= u8::MAX as usize);

        if weights.len() <= 1 {
            return Self {
                nodes: Vec::new(),
                total: weights.len() as u8,
            };
        }

        let ratio = replica_ratio(weights);
        let count = weights.iter().map(|x| *x as usize * ratio as usize).sum();
        let mut nodes: Vec<Node> = Vec::with_capacity(count);

        for (n, weight) in weights.iter().map(|x| *x as usize * ratio as usize).enumerate() {
            let token = Token(n as u8);

            for vidx in 0..=weight {
                let buf = format!("{0} 114514", vidx);
                let hash = chash(buf.as_bytes());
                nodes.push(Node { hash, token });
            }
        }

        nodes.sort_unstable_by_key(|node| node.hash);

        Self {
            nodes,
            total: weights.len() as u8,
        }
    }

    fn next(&self, state: &Self::State) -> Option<Token> {
        if self.total <= 1 {
            return Some(Token(0));
        }

        let hash = match state {
            IpAddr::V4(x) => chash_for_ip(&x.octets()),
            IpAddr::V6(x) => chash_for_ip(&x.octets()),
        };

        let idx = match self.nodes.binary_search_by_key(&hash, |node| node.hash) {
            Ok(idx) => idx,
            Err(idx) if idx >= self.nodes.len() as usize => 0,
            Err(idx) => idx,
        };

        Some(self.nodes[idx].token)
    }
}

#[derive(Debug)]
struct RRNode {
    cw: i16,
    ew: u8,
    weight: u8,
    token: Token,
}

#[derive(Debug)]
pub struct RoundRobin {
    nodes: Mutex<Vec<RRNode>>,
    total: u8,
}

impl Balance for RoundRobin {
    type State = ();

    fn new(weights: &[u8]) -> Self {
        assert!(weights.len() <= u8::MAX as usize);

        if weights.is_empty() {
            return Self {
                nodes: Mutex::new(Vec::new()),
                total: 0,
            };
        }

        let nodes = weights
            .iter()
            .enumerate()
            .map(|(i, &weight)| RRNode {
                cw: 0,
                ew: weight,
                weight,
                token: Token(i as u8),
            })
            .collect();

        Self {
            nodes: Mutex::new(nodes),
            total: weights.len() as u8,
        }
    }

    fn next(&self, _: &Self::State) -> Option<Token> {
        if self.total == 0 {
            return None;
        }
        if self.total == 1 {
            return Some(Token(0));
        }

        let mut nodes = self.nodes.lock().unwrap();
        let mut tw: i16 = 0; // Total weight
        let mut best: Option<&mut RRNode> = None;

        for node in nodes.iter_mut() {
            tw += node.ew as i16; // Accumulate total weight
            node.cw += node.ew as i16; // Increment current weight by effective weight

            if node.ew < node.weight {
                node.ew += 1; // Slowly restore the effective weight
            }

            if best.as_ref().map_or(true, |best_node| node.cw > best_node.cw) {
                best = Some(node);
            }
        }

        if let Some(best_node) = best {
            best_node.cw -= tw; // Adjust the current weight of the best node
            return Some(best_node.token);
        }

        None
    }     
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    IpHash,
    RoundRobin,
}

pub struct BalanceCtx<'a> {
    pub src_ip: &'a IpAddr,
}

#[derive(Debug, Clone)]
pub enum Balancer {
    IpHash(Arc<IpHash>),
    RoundRobin(Arc<RoundRobin>),
}

impl Balancer {
    pub fn new(strategy: Strategy, weights: &[u8]) -> Self {
        match strategy {
            Strategy::IpHash => Balancer::IpHash(Arc::new(IpHash::new(weights))),
            Strategy::RoundRobin => Balancer::RoundRobin(Arc::new(RoundRobin::new(weights))),
        }
    }

    pub fn next(&self, ctx: BalanceCtx) -> Option<Token> {
        match self {
            Balancer::IpHash(balancer) => balancer.next(ctx.src_ip),
            Balancer::RoundRobin(balancer) => balancer.next(&()),
        }
    }
}

use chash::{chash, chash_for_ip};
mod chash {
    const SEED: u32 = 0xbc9f1d34;
    const M: u32 = 0xc6a4a793;

    pub fn chash(buf: &[u8]) -> u32 {
        let mut h = SEED ^ (buf.len() as u32).wrapping_mul(M);
        let mut b = buf;
        let mut len = buf.len();

        while len >= 4 {
            h = h.wrapping_add(
                (b[0] as u32)
                    | ((b[1] as u32) << 8)
                    | ((b[2] as u32) << 16)
                    | ((b[3] as u32) << 24),
            );

            h = h.wrapping_mul(M);
            h ^= h >> 16;
            b = &b[4..];
            len -= 4;
        }

        if len == 3 {
            h = h.wrapping_add((b[2] as u32) << 16);
            len -= 1;
        }

        if len == 2 {
            h = h.wrapping_add((b[1] as u32) << 8);
            len -= 1;
        }

        if len == 1 {
            h = h.wrapping_add(b[0] as u32);
            h = h.wrapping_mul(M);
            h ^= h >> 24;
        }

        h
    }

    pub fn chash_for_ip(buf: &[u8]) -> u32 {
        let mut h = SEED ^ (buf.len() as u32).wrapping_mul(M);

        let (_, buf, _) = unsafe { buf.align_to::<u32>() };

        for b in buf.iter().map(|x| x.to_le()) {
            h = h.wrapping_add(b);
            h = h.wrapping_mul(M);
            h ^= h >> 16;
        }

        h
    }
}

// Replication ratio for virtual nodes
fn replica_ratio(weights: &[u8]) -> u8 {
    const MIN_REPLICA: u8 = 128;
    let max = *weights.iter().max().unwrap();

    if max >= MIN_REPLICA {
        1
    } else {
        f64::ceil(MIN_REPLICA as f64 / max as f64) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use average::{Max, Mean, Min};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn ih_replica_ratios() {
        macro_rules! run {
            ($weights: expr, $ratio: expr) => {{
                assert_eq!(replica_ratio($weights), $ratio);
            }};
        }

        run!(&[1], 128);
        run!(&[1, 1, 2], 64);
        run!(&[1, 1, 2, 2, 3], 43);
        run!(&[1, 1, 2, 2, 3, 3, 4], 32);
        run!(&[1, 1, 2, 2, 3, 3, 4, 4, 5], 26);
        run!(&[1, 1, 2, 2, 3, 3, 4, 4, 5, 10], 13);
        run!(&[1, 1, 2, 2, 3, 3, 4, 4, 5, 10, 20], 7);
        run!(&[1, 1, 2, 2, 3, 3, 4, 4, 5, 10, 20, 30], 5);
        run!(&[1, 1, 2, 2, 3, 3, 4, 4, 5, 10, 20, 30, 50], 3);
        run!(&[1, 1, 2, 2, 3, 3, 4, 4, 5, 10, 20, 30, 50, 100], 2);
        run!(&[1, 2, 3, 4, 128], 1);
        run!(&[1, 2, 3, 4, 200], 1);
        run!(&[1, 2, 3, 4, 255], 1);
    }

    #[test]
    fn ih_any_hash() {
        macro_rules! run {
            ($str: expr, $res: expr) => {{
                assert_eq!(chash($str), $res);
            }};
        }

        run!(b"", 3164544308);
        run!(b"123", 4219602657);
        run!(b"1234567", 897539970);
        run!(b"abc", 2237464879);
        run!(b"abcdefg", 2383090994);
        run!(b"123abc", 2851751921);
        run!(b"abc123", 4002724297);
        run!(b"realm", 885396906);
        run!(b"1 realm", 4115282535);
        run!(b"2 realm", 1326782105);
        run!(b"3 realm", 1796078392);
        run!(b"10 realm", 2265248424);
        run!(b"100 realm", 4289654351);
    }

    #[test]
    fn ih_ip_hash() {
        macro_rules! run {
            ($ip: expr) => {{
                let b = $ip.octets();
                assert_eq!(chash(&b), chash_for_ip(&b));
            }};
            (=> $ip: expr) => {{
                let ip = $ip.parse::<Ipv6Addr>().unwrap();
                run!(ip);
            }};
        }

        for i in (0..=u32::MAX).step_by(127) {
            run!(Ipv4Addr::from(i));
        }

        run!(=>"::0");
        run!(=>"::1");
        run!(=>"::ffff:127.0.0.1");
        run!(=>"2001:4860:4860::8844");
        run!(=>"2001:4860:4860::8888");
        run!(=>"2606:4700:4700::1001");
        run!(=>"2606:4700:4700::1111");
        run!(=>"fd9d:bb35:94bf:c38a:ee1:c75d:8df3:c909");
    }

    #[test]
    fn ih_same_ip() {
        let ip1 = "1.1.1.1".parse::<IpAddr>().unwrap();
        let ip2 = "8.8.8.8".parse::<IpAddr>().unwrap();
        let ip3 = "114.51.4.19".parse::<IpAddr>().unwrap();
        let ip4 = "2001:4860:4860::8888".parse::<IpAddr>().unwrap();

        let iphash = IpHash::new(&vec![1, 2, 3, 4]);
        assert_eq!(iphash.total, 4);
        assert!(iphash.nodes.len() >= (1 + 2 + 3 + 4) * 128 / 4);

        let ip1_node = iphash.next(&ip1);
        let ip2_node = iphash.next(&ip2);
        let ip3_node = iphash.next(&ip3);
        let ip4_node = iphash.next(&ip4);

        for _ in 0..16 {
            assert_eq!(iphash.next(&ip1), ip1_node);
            assert_eq!(iphash.next(&ip2), ip2_node);
            assert_eq!(iphash.next(&ip3), ip3_node);
            assert_eq!(iphash.next(&ip4), ip4_node);
        }
    }

    #[test]
    fn ih_same_weight() {
        let iphash = IpHash::new(&vec![1; 16]);
        let mut distro = [0f64; 16];

        let mut total: usize = 0;
        for ip in (0..=u32::MAX).map(Ipv4Addr::from).map(IpAddr::from).step_by(127) {
            let token = iphash.next(&ip).unwrap();
            distro[token.0 as usize] += 1 as f64;
            total += 1;
        }

        let diffs: Vec<f64> = distro
            .iter()
            .map(|x| *x / total as f64 - 1.0 / 16.0)
            .map(f64::abs)
            .collect();

        let min_diff: Min = diffs.iter().collect();
        let max_diff: Max = diffs.iter().collect();
        let mean_diff: Mean = diffs.iter().collect();

        println!("{:?}", distro);
        println!("min diff: {}", min_diff.min());
        println!("max diff: {}", max_diff.max());
        println!("mean diff: {}", mean_diff.mean());
    }

    #[test]
    fn ih_all_weights() {
        let weights: Vec<u8> = (1..=16).collect();
        let iphash = IpHash::new(&weights);
        let mut distro = [0f64; 16];

        let mut total: usize = 0;
        for ip in (0..=u32::MAX).map(Ipv4Addr::from).map(IpAddr::from).step_by(127) {
            let token = iphash.next(&ip).unwrap();
            distro[token.0 as usize] += 1 as f64;
            total += 1;
        }

        let diffs: Vec<f64> = distro
            .iter()
            .enumerate()
            .map(|(i, x)| *x / total as f64 - (i as f64 + 1.0) / 16.0)
            .map(f64::abs)
            .collect();

        let min_diff: Min = diffs.iter().collect();
        let max_diff: Max = diffs.iter().collect();
        let mean_diff: Mean = diffs.iter().collect();

        println!("{:?}", distro);
        println!("min diff: {}", min_diff.min());
        println!("max diff: {}", max_diff.max());
        println!("mean diff: {}", mean_diff.mean());
    }
    // Test equal weights for uniform distribution.
    #[test]
    fn rr_same_weight() {
        let rr = RoundRobin::new(&vec![1; 255]); // 255 nodes with weight 1
        let mut distro = [0f64; 255];

        for _ in 0..1_000_000 {
            let token = rr.next(&()).unwrap();
            distro[token.0 as usize] += 1.0;
        }

        let diffs: Vec<f64> = distro
            .iter()
            .map(|x| *x / 1_000_000.0 - 1.0 / 255.0) // Compare observed vs expected
            .map(f64::abs)
            .inspect(|x| assert!(*x < 1e-3)) // Ensure small difference
            .collect();

        let min_diff: Min = diffs.iter().collect();
        let max_diff: Max = diffs.iter().collect();
        let mean_diff: Mean = diffs.iter().collect();

        println!("{:?}", distro);
        println!("min diff: {}", min_diff.min());
        println!("max diff: {}", max_diff.max());
        println!("mean diff: {}", mean_diff.mean());
    }

    /// Test unequal weights for proportional distribution.
    #[test]
    fn rr_all_weights() {
        let weights: Vec<u8> = (1..=255).collect(); // Increasing weights from 1 to 255
        let total_weight: f64 = weights.iter().map(|&w| w as f64).sum();
        let rr = RoundRobin::new(&weights);
        let mut distro = [0f64; 255];

        for _ in 0..1_000_000 {
            let token = rr.next(&()).unwrap();
            distro[token.0 as usize] += 1.0;
        }

        let diffs: Vec<f64> = distro
            .iter()
            .enumerate()
            .map(|(i, x)| *x / 1_000_000.0 - (i as f64 + 1.0) / total_weight) // Expected proportion
            .map(f64::abs)
            .inspect(|x| assert!(*x < 1e-3)) // Ensure small difference
            .collect();

        let min_diff: Min = diffs.iter().collect();
        let max_diff: Max = diffs.iter().collect();
        let mean_diff: Mean = diffs.iter().collect();

        println!("{:?}", distro);
        println!("min diff: {}", min_diff.min());
        println!("max diff: {}", max_diff.max());
        println!("mean diff: {}", mean_diff.mean());
    }
}
