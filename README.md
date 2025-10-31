# baymax-relay

![Status](https://img.shields.io/badge/status-alpha-orange.svg) ![License: MIT](https://img.shields.io/badge/license-MIT-green.svg) ![Rust Version](https://img.shields.io/badge/rust-1.70%2B-orange.svg)

Reverse SOCKS5 relay platform that keeps privately hosted devices reachable through a hardened Rust control plane. `baymax-relay` lets you operate a network of outbound "slave" nodes without exposing ports, while clients enjoy a single, always-on SOCKS entrypoint.

> ⚠️ `baymax-relay` is in **Alpha**—APIs and protocol details are subject to change.

---

## Table of Contents
1. [Why Baymax Relay](#why-baymax-relay)
2. [Architecture](#architecture)
3. [Product Highlights](#product-highlights)
4. [Quick Start](#quick-start)
5. [Configuration Reference](#configuration-reference)
6. [Operating Modes](#operating-modes)
7. [Observability & Operations](#observability--operations)
8. [Client & Slave SDKs](#client--slave-sdks)
9. [Use Cases](#use-cases)
10. [Security Guidance](#security-guidance)
11. [Roadmap](#roadmap)
12. [Support & Contributing](#support--contributing)
13. [License](#license)

---

## Why Baymax Relay
- **Reach any device**: Bridge SOCKS5 traffic through NAT or carrier-grade firewalls with outbound-only tunnels.
- **Operate at scale**: Weighted load balancing and automated health checks keep fleets of exit nodes healthy.
- **Developer friendly**: Ready-to-run slaves for Java, Android, Flutter, and a standalone Dart TUN adapter mean you can embed tunnels anywhere.
- **Observability built-in**: Prometheus metrics and a browser dashboard ship by default.

## Architecture
```
┌─────────────┐      reverse TCP       ┌───────────────┐      SOCKS5       ┌──────────────┐
│ Slave Node  │ ─────────────────────▶ │   Master       │ ◀──────────────── │ Client Apps   │
│ (Java/etc.) │ ◀───────────────────── │ (Rust)         │ ──────► Internet  │ (Browsers,    │
└─────────────┘   multiplexed sessions └───────────────┘    outbound flows │ services)     │
          ▲            │  metrics@9090                       │               └──────────────┘
          └────────────┴────── heartbeat & control ──────────┘
```

1. Slaves initiate an outbound control tunnel to the master (`-t`).
2. Masters authenticate, geofence, and bandwidth-test each slave before activation.
3. Clients connect to the exposed SOCKS5 interface (`-s`), after which the master allocates an appropriate slave and multiplexes traffic over the existing tunnel.

## Product Highlights
- **Async Rust core** tuned for low latency, memory reuse, and back-pressure control.
- **Routing strategies**: `stick` (IP-hash) for affinity or `nonstick` (weighted round-robin) for throughput-driven distribution.
- **Validation pipeline**: version gating (`1.0.8` by default), `ipinfo` geolocation checks, and Cloudflare download probes ensure slave trustworthiness.
- **Pluggable packet protocol** (`packet.rs`) supports future commands (compression, TLS negotiation, etc.).
- **Prometheus-ready** metrics server on `http://<master>:9090` with a live HTML dashboard for quick inspections.

## Quick Start

### 1. Build the master
```
cd master-node
cargo build --release
```
Binary output: `master-node/target/release/baymax-relay`

### 2. Launch the master
```
./target/release/baymax-relay \
  -t 0.0.0.0:8000 \
  -s 0.0.0.0:1080 \
  -p nonstick \
  -v info
```

### 3. Connect a slave
- Java example:
  ```
  cd slave-node/java
  ./gradlew run --args="your.master.host:8000"
  ```
- Flutter example: open `slave-node/flutter.dart` in your IDE, update `proxy_client.dart` with the master host, and run on any platform.

### 4. Configure a client
- Point any SOCKS5-capable client (browser, curl, proxychains) at `your.master.host:1080`.
- Optional: supply SOCKS5 username to request a specific country if your slave implementation maps usernames to geolocation preferences.

## Configuration Reference

| Flag | Description | Example |
| ---- | ----------- | ------- |
| `-t, --transfer <ADDR>` | TCP address the master listens on for slave tunnels. Required. | `-t 0.0.0.0:8000` |
| `-s, --server <ADDR>` | SOCKS5 listener for clients. Required. | `-s 0.0.0.0:1080` |
| `-p, --proxy_mode <stick|nonstick>` | Slave selection strategy. Default `nonstick`. | `-p stick` |
| `-l, --allowed-locations <LIST>` | Whitelist of country names (comma-separated). Rejects other geos. | `-l "United States,Canada"` |
| `-v, --verbosity <LEVEL>` | Log verbosity (`trace`..`error`). Default `info`. | `-v debug` |

> Tip: Extend `conf.rs` to inject configuration via environment variables or config files for production rollouts.

## Operating Modes

### Bind Mode (local SOCKS5 service)
```
./baymax-relay -t 0.0.0.0:8000 -s 127.0.0.1:1080 -p stick
```
Use when slaves run on the same network and you only need a local listener.

### Reverse Mode (NAT traversal)
```
./baymax-relay -t 0.0.0.0:8000 -s 0.0.0.0:1080 -p nonstick -l "Germany"
```
Recommended for Internet-facing deployments. Any number of remote slaves maintain outbound tunnels to the master, no inbound firewall rules required.

## Observability & Operations
- **Metrics**: visit `http://<master>:9090` for the live dashboard or scrape with Prometheus. Default counters include active slave sessions, join events, and disconnections.
- **Logging**: adjust via `-v`. Integrate `simple_logger` output with your preferred log shipping stack.
- **Scaling**: increase `MAX_CONCURRENT_REQUESTS` and buffer pool sizing (`POOL_SIZE`, `NUM_SHARDS`) in `main.rs` to match workload.

## Client & Slave SDKs
- `master-node/` – Core Rust service (MIT licensed).
- `slave-node/java/` – Netty-based desktop agent demonstrating the wire protocol end-to-end.
- `slave-node/android/` – Android Studio project: mobile background service with reconnection logic.
- `slave-node/flutter.dart/` – Flutter reference UI and `proxy_client.dart` helper.
- `slave-node/socks_five_test/` – Flutter integration test harness covering multiple platforms.
- `slave-node/tun_socks/` – Standalone Dart package for TUN interface bridging.

> All SDKs are examples: add TLS, credential management, and retry logic before production use.

## Use Cases
- Residential/mobile proxy networks that rotate exit nodes without port forwarding.
- Remote workforce tooling that needs secure SOCKS access to machines behind restrictive firewalls.
- QA teams validating geolocated content by routing traffic through specific countries or carrier networks.
- Embedded devices that must proxy data back to HQ via a single managed control plane.

## Security Guidance
- Wrap slave-to-master tunnels in TLS or place them inside an IPSec/WireGuard mesh for confidentiality.
- Enable SOCKS5 username/password auth in `socks5.rs` and map credentials to policy (location affinity, rate limiting).
- Maintain the `ALLOWED_SLAVE_VERSIONS` list in `server.rs` to enforce binary provenance.
- Monitor geolocation API rate limits and cache responses if deploying at large scale.

## Roadmap
- TLS transport with mutual authentication.
- Persistent metadata store for slave history and health scoring.
- CLI-driven configuration profiles (YAML/JSON) for easier automation.
- Expanded metrics (per-session latency, bandwidth histograms) and Grafana dashboards.

Feedback and PRs welcome—open an issue to discuss additional features.

## Support & Contributing
- **Issues & Discussions**: Use the GitHub issue tracker to report bugs or request features.
- **Pull Requests**: Please include tests where possible and describe how your change impacts the control plane or slaves.
- **Custom builds**: Reach out if you need commercial support or tailored integrations.

## License
- Core master service: MIT (see `master-node/Cargo.toml`).
- Refer to each submodule’s README/license for derivative works.