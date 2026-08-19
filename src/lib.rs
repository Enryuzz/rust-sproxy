use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OnceCell, Semaphore};
use tokio::time::timeout;
use url::Url;

#[derive(serde::Deserialize)]
struct ProxyConfig {
    upstreams: Vec<String>,
}

pub struct RuntimeProxyPool {
    path: PathBuf,
    cursor: AtomicUsize,
    routes: OnceCell<Vec<UpstreamProxy>>,
}

impl RuntimeProxyPool {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            cursor: AtomicUsize::new(0),
            routes: OnceCell::new(),
        }
    }

    pub async fn next(&self) -> Result<UpstreamProxy> {
        self.next_route()
            .await?
            .context("proxy configuration has no upstreams")
    }

    async fn next_route(&self) -> Result<Option<UpstreamProxy>> {
        let routes = self
            .routes
            .get_or_try_init(|| async {
                let content = match tokio::fs::read_to_string(&self.path).await {
                    Ok(content) => content,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(Vec::new());
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("failed to read config {}", self.path.display())
                        });
                    }
                };
                let config: ProxyConfig =
                    toml::from_str(&content).context("invalid proxy configuration")?;
                config
                    .upstreams
                    .iter()
                    .map(|value| UpstreamProxy::parse(value))
                    .collect()
            })
            .await?;
        if routes.is_empty() {
            return Ok(None);
        }
        let index = self.cursor.fetch_add(1, Ordering::Relaxed) % routes.len();
        Ok(Some(routes[index].clone()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProxyKind {
    Http,
    Socks4,
    Socks5,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetAddr {
    Ip(IpAddr, u16),
    Domain(String, u16),
}

impl TargetAddr {
    pub fn port(&self) -> u16 {
        match self {
            Self::Ip(_, port) | Self::Domain(_, port) => *port,
        }
    }

    pub fn host(&self) -> String {
        match self {
            Self::Ip(ip, _) => ip.to_string(),
            Self::Domain(host, _) => host.clone(),
        }
    }

    fn authority(&self) -> String {
        match self {
            Self::Ip(IpAddr::V6(ip), port) => format!("[{ip}]:{port}"),
            _ => format!("{}:{}", self.host(), self.port()),
        }
    }

    fn validate_http_authority(&self) -> Result<()> {
        if let Self::Domain(host, _) = self {
            if host.is_empty()
                || host
                    .bytes()
                    .any(|byte| byte <= b' ' || byte == 0x7f || byte == b'/')
            {
                bail!("target hostname is unsafe for an HTTP authority");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct UpstreamProxy {
    kind: ProxyKind,
    host: String,
    port: u16,
    username: Option<String>,
    password: Option<String>,
}

impl UpstreamProxy {
    pub fn parse(value: &str) -> Result<Self> {
        let url = Url::parse(value).context("invalid proxy URL")?;
        let kind = match url.scheme() {
            "http" => ProxyKind::Http,
            "socks4" | "socks4a" => ProxyKind::Socks4,
            "socks5" | "socks5h" => ProxyKind::Socks5,
            scheme => bail!("unsupported proxy scheme: {scheme}"),
        };
        let host = url.host_str().context("proxy URL has no host")?.to_owned();
        let port = url
            .port()
            .or_else(|| (kind == ProxyKind::Http).then_some(80))
            .context("proxy URL has no port")?;
        let username = (!url.username().is_empty())
            .then(|| percent_decode_utf8(url.username()))
            .transpose()
            .context("invalid proxy username")?;
        let password = url
            .password()
            .map(percent_decode_utf8)
            .transpose()
            .context("invalid proxy password")?;
        if kind == ProxyKind::Socks4
            && username
                .as_deref()
                .is_some_and(|value| value.contains('\0'))
        {
            bail!("SOCKS4 user ID contains NUL");
        }
        Ok(Self {
            kind,
            host,
            port,
            username,
            password,
        })
    }

    pub fn kind(&self) -> ProxyKind {
        self.kind
    }
    pub fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }
    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

fn percent_decode_utf8(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                bail!("incomplete percent escape");
            }
            let high = (bytes[index + 1] as char)
                .to_digit(16)
                .context("invalid percent escape")?;
            let low = (bytes[index + 2] as char)
                .to_digit(16)
                .context("invalid percent escape")?;
            decoded.push(((high << 4) | low) as u8);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).context("percent-decoded credential is not UTF-8")
}

pub async fn connect_via_proxy(proxy: &UpstreamProxy, target: &TargetAddr) -> Result<TcpStream> {
    timeout(
        Duration::from_secs(10),
        connect_via_proxy_inner(proxy, target),
    )
    .await
    .context("upstream connect/handshake timed out")?
}

async fn connect_via_proxy_inner(proxy: &UpstreamProxy, target: &TargetAddr) -> Result<TcpStream> {
    if proxy.kind == ProxyKind::Http {
        target.validate_http_authority()?;
    }
    if proxy.kind == ProxyKind::Socks4
        && matches!(target, TargetAddr::Domain(host, _) if host.contains('\0'))
    {
        bail!("SOCKS4 hostname contains NUL");
    }
    let mut stream = TcpStream::connect((proxy.host.as_str(), proxy.port))
        .await
        .with_context(|| format!("failed to connect to upstream proxy {}", proxy.address()))?;
    match proxy.kind {
        ProxyKind::Http => http_connect(&mut stream, proxy, target).await?,
        ProxyKind::Socks4 => socks4_connect(&mut stream, proxy, target).await?,
        ProxyKind::Socks5 => socks5_connect(&mut stream, proxy, target).await?,
    }
    Ok(stream)
}

async fn connect_direct(target: &TargetAddr) -> Result<TcpStream> {
    match target {
        TargetAddr::Ip(ip, port) => TcpStream::connect((*ip, *port)).await,
        TargetAddr::Domain(host, port) => TcpStream::connect((host.as_str(), *port)).await,
    }
    .with_context(|| format!("failed to connect directly to {}", target.authority()))
}

async fn http_connect(
    stream: &mut TcpStream,
    proxy: &UpstreamProxy,
    target: &TargetAddr,
) -> Result<()> {
    let authority = target.authority();
    let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(username) = &proxy.username {
        let credentials = format!("{}:{}", username, proxy.password.as_deref().unwrap_or(""));
        let encoded = base64::engine::general_purpose::STANDARD.encode(credentials);
        request.push_str(&format!("Proxy-Authorization: Basic {encoded}\r\n"));
    }
    request.push_str("Proxy-Connection: Keep-Alive\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;

    let mut response = Vec::new();
    let mut byte = [0_u8; 1];
    while !response.ends_with(b"\r\n\r\n") {
        if response.len() >= 16 * 1024 {
            bail!("HTTP proxy response headers are too large");
        }
        stream
            .read_exact(&mut byte)
            .await
            .context("HTTP proxy closed before sending a response")?;
        response.push(byte[0]);
    }
    let status_line = String::from_utf8_lossy(&response)
        .lines()
        .next()
        .unwrap_or("")
        .to_owned();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok());
    if !matches!(status, Some(200..=299)) {
        bail!("HTTP CONNECT failed: {status_line}");
    }
    Ok(())
}

async fn socks4_connect(
    stream: &mut TcpStream,
    proxy: &UpstreamProxy,
    target: &TargetAddr,
) -> Result<()> {
    let mut request = vec![4, 1];
    request.extend_from_slice(&target.port().to_be_bytes());
    match target {
        TargetAddr::Ip(IpAddr::V4(ip), _) => request.extend_from_slice(&ip.octets()),
        TargetAddr::Ip(IpAddr::V6(_), _) | TargetAddr::Domain(_, _) => {
            request.extend_from_slice(&[0, 0, 0, 1])
        }
    }
    request.extend_from_slice(proxy.username.as_deref().unwrap_or("").as_bytes());
    request.push(0);
    if !matches!(target, TargetAddr::Ip(IpAddr::V4(_), _)) {
        request.extend_from_slice(target.host().as_bytes());
        request.push(0);
    }
    stream.write_all(&request).await?;
    let mut response = [0_u8; 8];
    stream.read_exact(&mut response).await?;
    if response[0] != 0 {
        bail!("invalid SOCKS4 response version {}", response[0]);
    }
    if response[1] != 90 {
        bail!("SOCKS4 CONNECT failed with status {}", response[1]);
    }
    Ok(())
}

async fn socks5_connect(
    stream: &mut TcpStream,
    proxy: &UpstreamProxy,
    target: &TargetAddr,
) -> Result<()> {
    if proxy.username.is_some() {
        stream.write_all(&[5, 2, 0, 2]).await?;
    } else {
        stream.write_all(&[5, 1, 0]).await?;
    }
    let mut selection = [0_u8; 2];
    stream.read_exact(&mut selection).await?;
    if selection[0] != 5 {
        bail!("invalid SOCKS5 proxy greeting");
    }
    match selection[1] {
        0 => {}
        2 => {
            let username = proxy
                .username
                .as_deref()
                .context("SOCKS5 proxy requested credentials")?;
            let password = proxy.password.as_deref().unwrap_or("");
            if username.len() > 255 || password.len() > 255 {
                bail!("SOCKS5 credentials exceed 255 bytes");
            }
            let mut auth = vec![1, username.len() as u8];
            auth.extend_from_slice(username.as_bytes());
            auth.push(password.len() as u8);
            auth.extend_from_slice(password.as_bytes());
            stream.write_all(&auth).await?;
            let mut reply = [0_u8; 2];
            stream.read_exact(&mut reply).await?;
            if reply[0] != 1 {
                bail!("invalid SOCKS5 authentication reply version {}", reply[0]);
            }
            if reply[1] != 0 {
                bail!("SOCKS5 authentication failed");
            }
        }
        255 => bail!("SOCKS5 proxy rejected authentication methods"),
        method => bail!("SOCKS5 proxy selected unsupported authentication method {method}"),
    }

    let mut request = vec![5, 1, 0];
    encode_socks5_addr(&mut request, target)?;
    stream.write_all(&request).await?;
    let mut head = [0_u8; 4];
    stream.read_exact(&mut head).await?;
    if head[0] != 5 {
        bail!("invalid SOCKS5 CONNECT reply version {}", head[0]);
    }
    if head[2] != 0 {
        bail!("invalid SOCKS5 reserved byte {}", head[2]);
    }
    if head[1] != 0 {
        bail!("SOCKS5 CONNECT failed with status {}", head[1]);
    }
    discard_socks5_addr(stream, head[3]).await?;
    Ok(())
}

fn encode_socks5_addr(output: &mut Vec<u8>, target: &TargetAddr) -> Result<()> {
    match target {
        TargetAddr::Ip(IpAddr::V4(ip), port) => {
            output.push(1);
            output.extend_from_slice(&ip.octets());
            output.extend_from_slice(&port.to_be_bytes());
        }
        TargetAddr::Ip(IpAddr::V6(ip), port) => {
            output.push(4);
            output.extend_from_slice(&ip.octets());
            output.extend_from_slice(&port.to_be_bytes());
        }
        TargetAddr::Domain(host, port) => {
            if host.is_empty() {
                bail!("target hostname is empty");
            }
            if host.len() > 255 {
                bail!("target hostname exceeds 255 bytes");
            }
            output.extend_from_slice(&[3, host.len() as u8]);
            output.extend_from_slice(host.as_bytes());
            output.extend_from_slice(&port.to_be_bytes());
        }
    }
    Ok(())
}

async fn discard_socks5_addr(stream: &mut TcpStream, kind: u8) -> Result<()> {
    let length = match kind {
        1 => 4,
        4 => 16,
        3 => {
            let length = stream.read_u8().await? as usize;
            if length == 0 {
                bail!("SOCKS5 proxy returned an empty domain");
            }
            length
        }
        _ => return Err(anyhow!("invalid SOCKS5 address type {kind}")),
    };
    let mut rest = vec![0_u8; length + 2];
    stream.read_exact(&mut rest).await?;
    Ok(())
}

pub async fn read_socks5_target(stream: &mut TcpStream) -> Result<TargetAddr> {
    let mut head = [0_u8; 4];
    stream.read_exact(&mut head).await?;
    if head[0] != 5 {
        bail!("unsupported client SOCKS version {}", head[0]);
    }
    if head[2] != 0 {
        bail!("SOCKS5 reserved byte must be zero");
    }
    if head[1] != 1 {
        bail!("only SOCKS5 CONNECT is supported");
    }
    let target = match head[3] {
        1 => {
            let mut bytes = [0_u8; 4];
            stream.read_exact(&mut bytes).await?;
            TargetAddr::Ip(IpAddr::V4(Ipv4Addr::from(bytes)), stream.read_u16().await?)
        }
        4 => {
            let mut bytes = [0_u8; 16];
            stream.read_exact(&mut bytes).await?;
            TargetAddr::Ip(IpAddr::V6(Ipv6Addr::from(bytes)), stream.read_u16().await?)
        }
        3 => {
            let len = stream.read_u8().await? as usize;
            if len == 0 {
                bail!("target hostname is empty");
            }
            let mut bytes = vec![0_u8; len];
            stream.read_exact(&mut bytes).await?;
            TargetAddr::Domain(
                String::from_utf8(bytes).context("target hostname is not UTF-8")?,
                stream.read_u16().await?,
            )
        }
        kind => bail!("unsupported SOCKS5 address type {kind}"),
    };
    Ok(target)
}

pub async fn serve(listener: TcpListener, config_path: impl AsRef<Path>) -> Result<()> {
    serve_with_protocol(
        listener,
        config_path,
        ListenerProtocol::Socks5,
        ServerOptions::default(),
    )
    .await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListenerProtocol {
    Socks5,
    Socks4,
    Http,
}

#[derive(Clone, Debug)]
pub struct ServerOptions {
    pub handshake_timeout: Duration,
    pub max_connections: usize,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            handshake_timeout: Duration::from_secs(10),
            max_connections: 1024,
        }
    }
}

pub async fn serve_with_options(
    listener: TcpListener,
    config_path: impl AsRef<Path>,
    options: ServerOptions,
) -> Result<()> {
    serve_with_protocol(listener, config_path, ListenerProtocol::Socks5, options).await
}

pub async fn serve_with_protocol(
    listener: TcpListener,
    config_path: impl AsRef<Path>,
    protocol: ListenerProtocol,
    options: ServerOptions,
) -> Result<()> {
    if options.max_connections == 0 {
        bail!("max_connections must be greater than zero");
    }
    let pool = Arc::new(RuntimeProxyPool::new(config_path.as_ref().to_owned()));
    let permits = Arc::new(Semaphore::new(options.max_connections));
    loop {
        let permit = permits.clone().acquire_owned().await?;
        let (client, peer) = listener.accept().await?;
        let pool = pool.clone();
        let handshake_timeout = options.handshake_timeout;
        tokio::spawn(async move {
            match timeout(handshake_timeout, establish_client(client, &pool, protocol)).await {
                Ok(Ok((mut client, mut upstream))) => {
                    if let Err(error) =
                        tokio::io::copy_bidirectional(&mut client, &mut upstream).await
                    {
                        eprintln!("connection from {peer} relay failed: {error:#}");
                    }
                }
                Ok(Err(error)) => eprintln!("connection from {peer} failed: {error:#}"),
                Err(error) => eprintln!("connection from {peer} failed: client handshake {error}"),
            }
            drop(permit);
        });
    }
}

async fn establish_client(
    client: TcpStream,
    pool: &RuntimeProxyPool,
    protocol: ListenerProtocol,
) -> Result<(TcpStream, TcpStream)> {
    match protocol {
        ListenerProtocol::Socks5 => establish_socks5(client, pool).await,
        ListenerProtocol::Socks4 => establish_socks4(client, pool).await,
        ListenerProtocol::Http => establish_http(client, pool).await,
    }
}

async fn connect_route(pool: &RuntimeProxyPool, target: &TargetAddr) -> Result<TcpStream> {
    match pool.next_route().await? {
        Some(proxy) => connect_via_proxy_inner(&proxy, target).await,
        None => connect_direct(target).await,
    }
}

async fn establish_socks5(
    mut client: TcpStream,
    pool: &RuntimeProxyPool,
) -> Result<(TcpStream, TcpStream)> {
    let version = client.read_u8().await?;
    if version != 5 {
        bail!("unsupported client SOCKS version {version}");
    }
    let method_count = client.read_u8().await? as usize;
    let mut methods = vec![0_u8; method_count];
    client.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        client.write_all(&[5, 255]).await?;
        bail!("client does not support no-authentication SOCKS5");
    }
    client.write_all(&[5, 0]).await?;
    let target = match read_socks5_target(&mut client).await {
        Ok(target) => target,
        Err(error) => {
            let _ = client.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await;
            return Err(error);
        }
    };
    let upstream = match connect_route(pool, &target).await {
        Ok(stream) => stream,
        Err(error) => {
            let _ = client.write_all(&[5, 1, 0, 1, 0, 0, 0, 0, 0, 0]).await;
            return Err(error);
        }
    };
    client.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    Ok((client, upstream))
}

async fn establish_socks4(
    mut client: TcpStream,
    pool: &RuntimeProxyPool,
) -> Result<(TcpStream, TcpStream)> {
    let mut header = [0_u8; 8];
    client.read_exact(&mut header).await?;
    if header[0] != 4 || header[1] != 1 {
        let _ = client.write_all(&[0, 91, 0, 0, 0, 0, 0, 0]).await;
        bail!("only SOCKS4 CONNECT is supported");
    }
    let target = match read_socks4_target(&mut client, header).await {
        Ok(target) => target,
        Err(error) => {
            let _ = client.write_all(&[0, 91, 0, 0, 0, 0, 0, 0]).await;
            return Err(error);
        }
    };
    let upstream = match connect_route(pool, &target).await {
        Ok(stream) => stream,
        Err(error) => {
            let _ = client.write_all(&[0, 91, 0, 0, 0, 0, 0, 0]).await;
            return Err(error);
        }
    };
    client.write_all(&[0, 90, 0, 0, 0, 0, 0, 0]).await?;
    Ok((client, upstream))
}

async fn read_socks4_target(stream: &mut TcpStream, header: [u8; 8]) -> Result<TargetAddr> {
    let port = u16::from_be_bytes([header[2], header[3]]);
    let _user_id = read_nul_terminated(stream, 255, "SOCKS4 user ID").await?;
    let ip = [header[4], header[5], header[6], header[7]];
    if ip[..3] != [0, 0, 0] || ip[3] == 0 {
        return Ok(TargetAddr::Ip(IpAddr::V4(Ipv4Addr::from(ip)), port));
    }
    let hostname = read_nul_terminated(stream, 255, "SOCKS4a hostname").await?;
    if hostname.is_empty() {
        bail!("SOCKS4a hostname is empty");
    }
    Ok(TargetAddr::Domain(
        String::from_utf8(hostname).context("SOCKS4a hostname is not UTF-8")?,
        port,
    ))
}

async fn read_nul_terminated(
    stream: &mut TcpStream,
    maximum: usize,
    field: &str,
) -> Result<Vec<u8>> {
    let mut value = Vec::new();
    loop {
        let byte = stream.read_u8().await?;
        if byte == 0 {
            return Ok(value);
        }
        if value.len() >= maximum {
            bail!("{field} exceeds {maximum} bytes");
        }
        value.push(byte);
    }
}

async fn establish_http(
    mut client: TcpStream,
    pool: &RuntimeProxyPool,
) -> Result<(TcpStream, TcpStream)> {
    let headers = match read_http_request_headers(&mut client).await {
        Ok(headers) => headers,
        Err(HttpHeaderReadError::TooLarge) => {
            let _ = client
                .write_all(
                    b"HTTP/1.1 431 Request Header Fields Too Large\r\nConnection: close\r\n\r\n",
                )
                .await;
            bail!("HTTP request headers are too large");
        }
        Err(HttpHeaderReadError::Io(error)) => {
            let _ = client
                .write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")
                .await;
            return Err(error.into());
        }
    };
    let request = match String::from_utf8(headers).context("HTTP request headers are not UTF-8") {
        Ok(request) => request,
        Err(error) => {
            let _ = client
                .write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")
                .await;
            return Err(error);
        }
    };
    let request_line = request.lines().next().context("HTTP request is empty")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let authority = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("");
    if parts.next().is_some() || !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        client
            .write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")
            .await?;
        bail!("invalid HTTP request line");
    }
    if request
        .split("\r\n")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .any(|line| !valid_http_header_line(line))
    {
        client
            .write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")
            .await?;
        bail!("invalid HTTP header line");
    }
    if method != "CONNECT" {
        client
            .write_all(b"HTTP/1.1 405 Method Not Allowed\r\nConnection: close\r\n\r\n")
            .await?;
        bail!("HTTP listener supports CONNECT only");
    }
    let target = match parse_http_authority(authority) {
        Ok(target) => target,
        Err(error) => {
            let _ = client
                .write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")
                .await;
            return Err(error);
        }
    };
    let upstream = match connect_route(pool, &target).await {
        Ok(stream) => stream,
        Err(error) => {
            let _ = client
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n")
                .await;
            return Err(error);
        }
    };
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    Ok((client, upstream))
}

enum HttpHeaderReadError {
    TooLarge,
    Io(std::io::Error),
}

async fn read_http_request_headers(
    stream: &mut TcpStream,
) -> std::result::Result<Vec<u8>, HttpHeaderReadError> {
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        if headers.len() >= 16 * 1024 {
            return Err(HttpHeaderReadError::TooLarge);
        }
        headers.push(stream.read_u8().await.map_err(HttpHeaderReadError::Io)?);
    }
    Ok(headers)
}

fn valid_http_header_line(line: &str) -> bool {
    let Some((name, _value)) = line.split_once(':') else {
        return false;
    };
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn parse_http_authority(authority: &str) -> Result<TargetAddr> {
    if authority.starts_with('[') {
        let closing = authority
            .find(']')
            .context("invalid IPv6 CONNECT authority")?;
        let ip: Ipv6Addr = authority[1..closing]
            .parse()
            .context("invalid IPv6 CONNECT address")?;
        let port = authority
            .get(closing + 1..)
            .and_then(|rest| rest.strip_prefix(':'))
            .context("CONNECT authority has no port")?
            .parse::<u16>()
            .context("invalid CONNECT port")?;
        return Ok(TargetAddr::Ip(IpAddr::V6(ip), port));
    }
    let (host, port) = authority
        .rsplit_once(':')
        .context("CONNECT authority has no port")?;
    let target = match host.parse::<IpAddr>() {
        Ok(ip) => TargetAddr::Ip(ip, port.parse().context("invalid CONNECT port")?),
        Err(_) => TargetAddr::Domain(
            host.to_owned(),
            port.parse().context("invalid CONNECT port")?,
        ),
    };
    target.validate_http_authority()?;
    Ok(target)
}
