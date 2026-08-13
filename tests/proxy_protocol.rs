use rust_sproxy::{connect_via_proxy, ProxyKind, TargetAddr, UpstreamProxy};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[test]
fn parses_all_supported_proxy_urls() {
    let http = UpstreamProxy::parse("http://alice:secret@127.0.0.1:8080").unwrap();
    assert_eq!(http.kind(), ProxyKind::Http);
    assert_eq!(http.username(), Some("alice"));

    let socks4 = UpstreamProxy::parse("socks4://127.0.0.1:1080").unwrap();
    assert_eq!(socks4.kind(), ProxyKind::Socks4);

    let socks5 = UpstreamProxy::parse("socks5://bob:password@127.0.0.1:1081").unwrap();
    assert_eq!(socks5.kind(), ProxyKind::Socks5);
    assert_eq!(socks5.username(), Some("bob"));
}

#[test]
fn rejects_unsupported_proxy_scheme() {
    let error = UpstreamProxy::parse("ftp://127.0.0.1:21").unwrap_err();
    assert!(error.to_string().contains("unsupported proxy scheme"));
}

#[tokio::test]
async fn opens_tunnel_through_http_connect_proxy() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    let mock = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut byte = [0_u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
        }
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with("CONNECT example.com:443 HTTP/1.1\r\n"));
        assert!(request.contains("Proxy-Authorization: Basic YWxpY2U6c2VjcmV0\r\n"));
        stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .unwrap();
        echo_once(&mut stream).await;
    });

    let proxy = UpstreamProxy::parse(&format!("http://alice:secret@{proxy_addr}")).unwrap();
    let mut tunnel = connect_via_proxy(&proxy, &TargetAddr::Domain("example.com".into(), 443))
        .await
        .unwrap();
    assert_echo(&mut tunnel).await;
    mock.await.unwrap();
}

#[tokio::test]
async fn opens_tunnel_through_socks4a_proxy() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    let mock = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut fixed = [0_u8; 8];
        stream.read_exact(&mut fixed).await.unwrap();
        assert_eq!(&fixed[..2], &[4, 1]);
        assert_eq!(u16::from_be_bytes([fixed[2], fixed[3]]), 80);
        assert_eq!(&fixed[4..8], &[0, 0, 0, 1]);
        assert_eq!(read_c_string(&mut stream).await, b"");
        assert_eq!(read_c_string(&mut stream).await, b"example.com");
        stream.write_all(&[0, 90, 0, 80, 0, 0, 0, 1]).await.unwrap();
        echo_once(&mut stream).await;
    });

    let proxy = UpstreamProxy::parse(&format!("socks4://{proxy_addr}")).unwrap();
    let mut tunnel = connect_via_proxy(&proxy, &TargetAddr::Domain("example.com".into(), 80))
        .await
        .unwrap();
    assert_echo(&mut tunnel).await;
    mock.await.unwrap();
}

#[tokio::test]
async fn opens_authenticated_tunnel_through_socks5_proxy() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    let mock = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut greeting = [0_u8; 4];
        stream.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 2, 0, 2]);
        stream.write_all(&[5, 2]).await.unwrap();

        let mut auth_head = [0_u8; 2];
        stream.read_exact(&mut auth_head).await.unwrap();
        assert_eq!(auth_head, [1, 3]);
        let mut username = [0_u8; 3];
        stream.read_exact(&mut username).await.unwrap();
        assert_eq!(&username, b"bob");
        let mut password_len = [0_u8; 1];
        stream.read_exact(&mut password_len).await.unwrap();
        let mut password = vec![0; password_len[0] as usize];
        stream.read_exact(&mut password).await.unwrap();
        assert_eq!(password, b"password");
        stream.write_all(&[1, 0]).await.unwrap();

        let mut request_head = [0_u8; 5];
        stream.read_exact(&mut request_head).await.unwrap();
        assert_eq!(request_head, [5, 1, 0, 3, 11]);
        let mut host = [0_u8; 11];
        stream.read_exact(&mut host).await.unwrap();
        assert_eq!(&host, b"example.com");
        let mut port = [0_u8; 2];
        stream.read_exact(&mut port).await.unwrap();
        assert_eq!(u16::from_be_bytes(port), 443);
        stream
            .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        echo_once(&mut stream).await;
    });

    let proxy = UpstreamProxy::parse(&format!("socks5://bob:password@{proxy_addr}")).unwrap();
    let mut tunnel = connect_via_proxy(&proxy, &TargetAddr::Domain("example.com".into(), 443))
        .await
        .unwrap();
    assert_echo(&mut tunnel).await;
    mock.await.unwrap();
}

async fn read_c_string(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut output = Vec::new();
    loop {
        let byte = stream.read_u8().await.unwrap();
        if byte == 0 {
            return output;
        }
        output.push(byte);
    }
}

async fn echo_once(stream: &mut tokio::net::TcpStream) {
    let mut data = [0_u8; 4];
    stream.read_exact(&mut data).await.unwrap();
    stream.write_all(&data).await.unwrap();
}

async fn assert_echo(stream: &mut tokio::net::TcpStream) {
    stream.write_all(b"ping").await.unwrap();
    let mut data = [0_u8; 4];
    stream.read_exact(&mut data).await.unwrap();
    assert_eq!(&data, b"ping");
}
