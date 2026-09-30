# rust-sproxy

A concurrent SOCKS5, SOCKS4/SOCKS4a, or HTTP CONNECT server that dynamically chains each connection through an upstream HTTP, SOCKS4/SOCKS4a, or SOCKS5 proxy.

## Features

- Selectable client listener with `--type socks5|socks4|http|auto` (`auto` by default); `auto` detects all three protocols on one port
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

Build with the helper script:

```sh
# Optimized release build (default)
scripts/build.sh

# Build and run all tests
scripts/build.sh --test

# Development/debug build
scripts/build.sh --debug
```

The binaries are written to `target/release/rust-sproxy` or `target/debug/rust-sproxy`.
To build and then start the proxy in one command:

```sh
scripts/run.sh
scripts/run.sh -- --type http --listen 127.0.0.1:8080
scripts/run.sh --debug --test -- --config sproxy.toml
```

`scripts/run.sh` calls the build script before starting the binary. With no server
arguments it listens on `0.0.0.0:8181`, enables `--allow-public-listen`, and uses
`--type auto` to detect SOCKS5, SOCKS4, and HTTP CONNECT clients on the same port.
Arguments after `--` replace these defaults and are forwarded to the server; relative
config paths use your current working directory. Use `scripts/run.sh -- --help` for
server options. This is an unauthenticated public listener; use a firewall or private
network.

You can also build directly with Cargo:

```sh
cp sproxy.example.toml sproxy.toml
# Edit sproxy.toml with working upstream proxy URLs.
cargo build --release
./target/release/rust-sproxy --listen 127.0.0.1:1080 --config sproxy.toml
```

Choose the client-facing protocol:

```sh
# Default: auto-detect SOCKS5, SOCKS4, or HTTP CONNECT
cargo run --release -- --listen 0.0.0.0:8181 --allow-public-listen

# SOCKS5 only
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

- The client-facing SOCKS5 server intentionally supports no-authentication only. The default listener is the unauthenticated public address `0.0.0.0:8181`; use a firewall or private network, or bind to loopback explicitly for local-only use.
- The SOCKS4 and HTTP listeners also have no client authentication.
- HTTP listener mode supports CONNECT tunneling only; ordinary `GET`, `POST`, and other forward-proxy requests receive `405 Method Not Allowed`.
- Handshakes time out after 10 seconds and simultaneous clients are capped at 256 by default. A tunnel uses two sockets, so this leaves headroom under the common 1024-file-descriptor limit. Use `--handshake-timeout-secs` and `--max-connections` to tune these limits; raise the OS open-file limit before raising the connection cap.
- Only TCP CONNECT is supported; SOCKS5 BIND and UDP ASSOCIATE are rejected.
- HTTP upstream support uses CONNECT and therefore requires an HTTP proxy that permits CONNECT to the requested destination port.

## Checks

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release
```
