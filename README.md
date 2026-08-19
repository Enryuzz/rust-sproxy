# rust-sproxy

A concurrent SOCKS5, SOCKS4/SOCKS4a, or HTTP CONNECT server that dynamically chains each connection through an upstream HTTP, SOCKS4/SOCKS4a, or SOCKS5 proxy.

## Features

- Selectable client listener with `--type socks5|socks4|http` (`socks5` by default)
- SOCKS5 CONNECT with IPv4, IPv6, and domain targets
- SOCKS4 and SOCKS4a CONNECT
- HTTP CONNECT tunneling
- HTTP CONNECT upstreams, including Basic proxy authentication
- SOCKS4 and SOCKS4a upstreams
- SOCKS5 upstreams with no authentication or username/password authentication
- Round-robin selection across all configured upstreams
- Direct mode when `upstreams = []` or the configuration file does not exist
- Configuration is loaded once and cached for all connections
- Asynchronous bidirectional tunneling with Tokio

## Build and run

```sh
cp sproxy.example.toml sproxy.toml
# Edit sproxy.toml with working upstream proxy URLs.
cargo run --release -- --listen 127.0.0.1:1080 --config sproxy.toml
```

Choose the client-facing protocol:

```sh
# Default
cargo run --release -- --type socks5 --listen 127.0.0.1:1080

# SOCKS4/SOCKS4a listener
cargo run --release -- --type socks4 --listen 127.0.0.1:1080

# HTTP CONNECT listener
cargo run --release -- --type http --listen 127.0.0.1:8080
```

Test it with curl:

```sh
curl --proxy socks5h://127.0.0.1:1080 https://example.com/
```

Use `socks5h` in clients when you want the hostname forwarded through the proxy chain instead of resolved by the client.

## Configuration

```toml
upstreams = [
  "http://user:pass@proxy.example:8080",
  "socks4://proxy.example:1080",
  "socks5://user:pass@proxy.example:1080",
]
```

Supported schemes are `http`, `socks4`, `socks4a`, `socks5`, and `socks5h`. Selection is round-robin. Set `upstreams = []` to connect directly without an upstream proxy. Restart the process after changing this file.

If the configured TOML file does not exist, the server defaults to direct mode. Existing files that cannot be read or parsed still produce connection failures. **A misspelled configuration path therefore bypasses upstream proxies and connects directly; verify the path when upstream routing is required.**

Credentials are percent-decoded as UTF-8. Percent-encode reserved characters in usernames or passwords. Protect the configuration file because it can contain plaintext credentials. **HTTP Basic, SOCKS4 user IDs, and SOCKS5 username/password authentication transmit these credentials in plaintext on the network**; use only trusted/private upstream links (or a separate encrypted tunnel).

## Current scope

- The client-facing SOCKS5 server intentionally supports no-authentication only. Non-loopback binds are refused unless `--allow-public-listen` is explicitly passed; use that override only with a firewall or private network.
- The SOCKS4 and HTTP listeners also have no client authentication.
- HTTP listener mode supports CONNECT tunneling only; ordinary `GET`, `POST`, and other forward-proxy requests receive `405 Method Not Allowed`.
- Handshakes time out after 10 seconds and simultaneous clients are capped at 1024 by default. Use `--handshake-timeout-secs` and `--max-connections` to tune these limits.
- Only TCP CONNECT is supported; SOCKS5 BIND and UDP ASSOCIATE are rejected.
- HTTP upstream support uses CONNECT and therefore requires an HTTP proxy that permits CONNECT to the requested destination port.

## Checks

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release
```
