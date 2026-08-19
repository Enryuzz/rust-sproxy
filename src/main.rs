use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use rust_sproxy::{serve_with_protocol, ListenerProtocol, ServerOptions};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::TcpListener;

#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Address on which the proxy server listens.
    #[arg(short, long, default_value = "127.0.0.1:1080")]
    listen: SocketAddr,

    /// TOML file containing upstream proxy URLs. A missing file enables direct mode.
    #[arg(short, long, default_value = "sproxy.toml")]
    config: PathBuf,

    /// Client-facing listener protocol.
    #[arg(
        long = "type",
        value_enum,
        value_name = "TYPE",
        default_value_t = ListenerType::Socks5
    )]
    listener_type: ListenerType,

    /// Explicitly permit this unauthenticated proxy to listen off loopback.
    #[arg(long)]
    allow_public_listen: bool,

    /// Maximum simultaneous client connections.
    #[arg(long, default_value_t = 1024)]
    max_connections: usize,

    /// Maximum seconds for client and upstream handshakes.
    #[arg(long, default_value_t = 10)]
    handshake_timeout_secs: u64,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ListenerType {
    Socks5,
    Socks4,
    Http,
}

impl From<ListenerType> for ListenerProtocol {
    fn from(value: ListenerType) -> Self {
        match value {
            ListenerType::Socks5 => Self::Socks5,
            ListenerType::Socks4 => Self::Socks4,
            ListenerType::Http => Self::Http,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if !args.listen.ip().is_loopback() && !args.allow_public_listen {
        anyhow::bail!(
            "refusing unauthenticated non-loopback listen {}; pass --allow-public-listen to override",
            args.listen
        );
    }
    let listener = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("failed to listen on {}", args.listen))?;
    println!(
        "{:?} server listening on {}",
        args.listener_type,
        listener.local_addr()?
    );
    println!("loading upstream proxies from {}", args.config.display());
    serve_with_protocol(
        listener,
        args.config,
        args.listener_type.into(),
        ServerOptions {
            handshake_timeout: Duration::from_secs(args.handshake_timeout_secs),
            max_connections: args.max_connections,
        },
    )
    .await
}
