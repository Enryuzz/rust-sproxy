use rust_sproxy::{serve_with_protocol, ListenerProtocol, ServerOptions};
use tempfile::NamedTempFile;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[tokio::test]
async fn socks4_listener_connects_directly() {
    let (target_addr, target) = spawn_echo_target().await;
    let (server_addr, server, _config) = spawn_server(ListenerProtocol::Socks4).await;

    let mut client = TcpStream::connect(server_addr).await.unwrap();
    let mut request = vec![4, 1];
    request.extend_from_slice(&target_addr.port().to_be_bytes());
    request.extend_from_slice(&[127, 0, 0, 1]);
    request.extend_from_slice(b"client-user\0");
    client.write_all(&request).await.unwrap();

    let mut reply = [0_u8; 8];
    client.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply[0], 0);
    assert_eq!(reply[1], 90);
    assert_relay(&mut client).await;

    target.await.unwrap();
    server.abort();
}

#[tokio::test]
async fn socks4a_listener_connects_domain_directly() {
    let (target_addr, target) = spawn_echo_target().await;
    let (server_addr, server, _config) = spawn_server(ListenerProtocol::Socks4).await;

    let mut client = TcpStream::connect(server_addr).await.unwrap();
    let mut request = vec![4, 1];
    request.extend_from_slice(&target_addr.port().to_be_bytes());
    request.extend_from_slice(&[0, 0, 0, 1]);
    request.push(0);
    request.extend_from_slice(b"localhost\0");
    client.write_all(&request).await.unwrap();

    let mut reply = [0_u8; 8];
    client.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply[1], 90);
    assert_relay(&mut client).await;

    target.await.unwrap();
    server.abort();
}

#[tokio::test]
async fn socks4_listener_rejects_oversized_user_id_with_reply() {
    let (server_addr, server, _config) = spawn_server(ListenerProtocol::Socks4).await;
    let mut client = TcpStream::connect(server_addr).await.unwrap();
    let mut request = vec![4, 1, 0, 80, 127, 0, 0, 1];
    request.extend(std::iter::repeat_n(b'x', 256));
    client.write_all(&request).await.unwrap();

    let mut reply = [0_u8; 8];
    client.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply, [0, 91, 0, 0, 0, 0, 0, 0]);
    server.abort();
}

#[tokio::test]
async fn socks4a_listener_rejects_invalid_hostname_with_reply() {
    let (server_addr, server, _config) = spawn_server(ListenerProtocol::Socks4).await;
    let mut client = TcpStream::connect(server_addr).await.unwrap();
    client
        .write_all(&[4, 1, 0, 80, 0, 0, 0, 1, 0, 0xff, 0])
        .await
        .unwrap();

    let mut reply = [0_u8; 8];
    client.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply, [0, 91, 0, 0, 0, 0, 0, 0]);
    server.abort();
}

#[tokio::test]
async fn http_connect_listener_connects_directly() {
    let (target_addr, target) = spawn_echo_target().await;
    let (server_addr, server, _config) = spawn_server(ListenerProtocol::Http).await;

    let mut client = TcpStream::connect(server_addr).await.unwrap();
    client
        .write_all(
            format!(
                "CONNECT 127.0.0.1:{} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
                target_addr.port(),
                target_addr.port()
            )
            .as_bytes(),
        )
        .await
        .unwrap();

    let response = read_http_headers(&mut client).await;
    assert!(response.starts_with("HTTP/1.1 200 Connection Established\r\n"));
    assert_relay(&mut client).await;

    target.await.unwrap();
    server.abort();
}

#[tokio::test]
async fn http_listener_rejects_non_connect_method() {
    let (server_addr, server, _config) = spawn_server(ListenerProtocol::Http).await;
    let mut client = TcpStream::connect(server_addr).await.unwrap();
    client
        .write_all(b"GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\n\r\n")
        .await
        .unwrap();
    let response = read_http_headers(&mut client).await;
    assert!(response.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"));
    server.abort();
}

#[tokio::test]
async fn http_listener_rejects_oversized_headers_with_431() {
    let (server_addr, server, _config) = spawn_server(ListenerProtocol::Http).await;
    let mut client = TcpStream::connect(server_addr).await.unwrap();
    let mut request = b"CONNECT example.com:443 HTTP/1.1\r\nX: ".to_vec();
    request.extend(std::iter::repeat_n(b'x', 16 * 1024));
    client.write_all(&request).await.unwrap();

    let response = read_http_headers(&mut client).await;
    assert!(response.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"));
    server.abort();
}

#[tokio::test]
async fn http_listener_rejects_non_utf8_headers_with_400() {
    let (server_addr, server, _config) = spawn_server(ListenerProtocol::Http).await;
    let mut client = TcpStream::connect(server_addr).await.unwrap();
    client
        .write_all(b"CONNECT example.com:443 HTTP/1.1\r\nX: \xff\r\n\r\n")
        .await
        .unwrap();

    let response = read_http_headers(&mut client).await;
    assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
    server.abort();
}

#[tokio::test]
async fn http_listener_rejects_malformed_header_line_with_400() {
    let (server_addr, server, _config) = spawn_server(ListenerProtocol::Http).await;
    let mut client = TcpStream::connect(server_addr).await.unwrap();
    client
        .write_all(b"CONNECT example.com:443 HTTP/1.1\r\nMalformedHeader\r\n\r\n")
        .await
        .unwrap();

    let response = read_http_headers(&mut client).await;
    assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
    server.abort();
}

#[tokio::test]
async fn http_listener_rejects_truncated_headers_with_400() {
    let (server_addr, server, _config) = spawn_server(ListenerProtocol::Http).await;
    let mut client = TcpStream::connect(server_addr).await.unwrap();
    client
        .write_all(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com")
        .await
        .unwrap();
    client.shutdown().await.unwrap();

    let response = read_http_headers(&mut client).await;
    assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
    server.abort();
}

async fn spawn_echo_target() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut data = [0_u8; 4];
        stream.read_exact(&mut data).await.unwrap();
        assert_eq!(&data, b"ping");
        stream.write_all(b"pong").await.unwrap();
    });
    (address, task)
}

async fn spawn_server(
    protocol: ListenerProtocol,
) -> (
    std::net::SocketAddr,
    tokio::task::JoinHandle<anyhow::Result<()>>,
    NamedTempFile,
) {
    let config = NamedTempFile::new().unwrap();
    std::fs::write(config.path(), "upstreams = []\n").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let path = config.path().to_owned();
    let task = tokio::spawn(async move {
        serve_with_protocol(listener, path, protocol, ServerOptions::default()).await
    });
    (address, task, config)
}

async fn assert_relay(stream: &mut TcpStream) {
    stream.write_all(b"ping").await.unwrap();
    let mut response = [0_u8; 4];
    stream.read_exact(&mut response).await.unwrap();
    assert_eq!(&response, b"pong");
}

async fn read_http_headers(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        bytes.push(stream.read_u8().await.unwrap());
    }
    String::from_utf8(bytes).unwrap()
}
