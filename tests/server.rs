use rust_sproxy::{serve, RuntimeProxyPool};
use std::time::Duration;
use tempfile::NamedTempFile;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[tokio::test]
async fn reloads_proxy_file_and_selects_upstreams_round_robin() {
    let file = NamedTempFile::new().unwrap();
    std::fs::write(
        file.path(),
        "upstreams = [\"http://127.0.0.1:8001\", \"socks4://127.0.0.1:8002\"]\n",
    )
    .unwrap();
    let pool = RuntimeProxyPool::new(file.path().to_owned());

    assert_eq!(pool.next().await.unwrap().address(), "127.0.0.1:8001");
    assert_eq!(pool.next().await.unwrap().address(), "127.0.0.1:8002");
    assert_eq!(pool.next().await.unwrap().address(), "127.0.0.1:8001");

    std::fs::write(file.path(), "upstreams = [\"socks5://127.0.0.1:9001\"]\n").unwrap();
    assert_eq!(pool.next().await.unwrap().address(), "127.0.0.1:9001");
}

#[tokio::test]
async fn socks5_server_relays_via_configured_upstream() {
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream_listener.local_addr().unwrap();
    let upstream = tokio::spawn(async move {
        let (mut stream, _) = upstream_listener.accept().await.unwrap();
        let mut greeting = [0_u8; 3];
        stream.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 1, 0]);
        stream.write_all(&[5, 0]).await.unwrap();
        let mut request = [0_u8; 22];
        stream.read_exact(&mut request).await.unwrap();
        assert_eq!(&request[..4], &[5, 1, 0, 4]);
        assert_eq!(u16::from_be_bytes([request[20], request[21]]), 8080);
        stream
            .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        let mut payload = [0_u8; 5];
        stream.read_exact(&mut payload).await.unwrap();
        assert_eq!(&payload, b"hello");
        stream.write_all(b"world").await.unwrap();
    });

    let file = NamedTempFile::new().unwrap();
    std::fs::write(
        file.path(),
        format!("upstreams = [\"socks5://{upstream_addr}\"]\n"),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = listener.local_addr().unwrap();
    let config_path = file.path().to_owned();
    let server = tokio::spawn(async move { serve(listener, config_path).await });

    let mut client = TcpStream::connect(server_addr).await.unwrap();
    client.write_all(&[5, 1, 0]).await.unwrap();
    let mut selection = [0_u8; 2];
    client.read_exact(&mut selection).await.unwrap();
    assert_eq!(selection, [5, 0]);

    let mut request = vec![5, 1, 0, 4];
    request.extend_from_slice(&std::net::Ipv6Addr::LOCALHOST.octets());
    request.extend_from_slice(&8080_u16.to_be_bytes());
    client.write_all(&request).await.unwrap();
    let mut reply = [0_u8; 10];
    client.read_exact(&mut reply).await.unwrap();
    assert_eq!(&reply[..2], &[5, 0]);

    client.write_all(b"hello").await.unwrap();
    let mut payload = [0_u8; 5];
    client.read_exact(&mut payload).await.unwrap();
    assert_eq!(&payload, b"world");

    tokio::time::timeout(Duration::from_secs(1), upstream)
        .await
        .unwrap()
        .unwrap();
    server.abort();
}

#[tokio::test]
async fn empty_upstream_list_connects_directly() {
    let target_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_addr = target_listener.local_addr().unwrap();
    let target = tokio::spawn(async move {
        let (mut stream, _) = target_listener.accept().await.unwrap();
        let mut payload = [0_u8; 5];
        stream.read_exact(&mut payload).await.unwrap();
        assert_eq!(&payload, b"hello");
        stream.write_all(b"world").await.unwrap();
    });

    let file = NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "upstreams = []\n").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = listener.local_addr().unwrap();
    let config_path = file.path().to_owned();
    let server = tokio::spawn(async move { serve(listener, config_path).await });

    let mut client = TcpStream::connect(server_addr).await.unwrap();
    client.write_all(&[5, 1, 0]).await.unwrap();
    let mut selection = [0_u8; 2];
    client.read_exact(&mut selection).await.unwrap();
    assert_eq!(selection, [5, 0]);

    let mut request = vec![5, 1, 0, 1];
    request.extend_from_slice(
        &target_addr
            .ip()
            .to_string()
            .parse::<std::net::Ipv4Addr>()
            .unwrap()
            .octets(),
    );
    request.extend_from_slice(&target_addr.port().to_be_bytes());
    client.write_all(&request).await.unwrap();
    let mut reply = [0_u8; 10];
    client.read_exact(&mut reply).await.unwrap();
    assert_eq!(&reply[..2], &[5, 0]);

    client.write_all(b"hello").await.unwrap();
    let mut payload = [0_u8; 5];
    client.read_exact(&mut payload).await.unwrap();
    assert_eq!(&payload, b"world");

    tokio::time::timeout(Duration::from_secs(1), target)
        .await
        .unwrap()
        .unwrap();
    server.abort();
}
