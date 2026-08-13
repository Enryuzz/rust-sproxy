use rust_sproxy::{connect_via_proxy, read_socks5_target, serve, TargetAddr, UpstreamProxy};
use std::time::Duration;
use tempfile::NamedTempFile;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[tokio::test]
async fn rejects_domains_unsafe_for_http_authority() {
    for host in [
        "",
        "evil\r\nInjected: yes",
        "space host",
        "tab\thost",
        "a/b",
        "nul\0host",
        "control\u{001f}",
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy =
            UpstreamProxy::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            connect_via_proxy(&proxy, &TargetAddr::Domain(host.into(), 443)),
        )
        .await;
        assert!(
            matches!(result, Ok(Err(_))),
            "unsafe host {host:?} was not rejected"
        );
    }
}

#[tokio::test]
async fn percent_decodes_credentials_and_rejects_invalid_utf8() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mock = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(stream.read_u8().await.unwrap());
        }
        let text = String::from_utf8(request).unwrap();
        // "user name:p@ss" in base64.
        assert!(text.contains("Proxy-Authorization: Basic dXNlciBuYW1lOnBAc3M=\r\n"));
        stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
    });
    let proxy = UpstreamProxy::parse(&format!("http://user%20name:p%40ss@{addr}")).unwrap();
    connect_via_proxy(&proxy, &TargetAddr::Domain("example.com".into(), 443))
        .await
        .unwrap();
    mock.await.unwrap();

    assert!(UpstreamProxy::parse("http://bad%FF:pass@127.0.0.1").is_err());
    assert!(UpstreamProxy::parse("http://user:bad%FF@127.0.0.1").is_err());
}

#[tokio::test]
async fn client_request_requires_zero_rsv_and_nonempty_domain() {
    for request in [
        vec![5, 1, 1, 1, 127, 0, 0, 1, 0, 80],
        vec![5, 1, 0, 3, 0, 0, 80],
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_socks5_target(&mut stream).await
        });
        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(&request).await.unwrap();
        assert!(task.await.unwrap().is_err());
    }
}

#[tokio::test]
async fn validates_socks5_upstream_reserved_byte() {
    let error = run_bad_socks5_upstream(&[5, 0, 1, 1, 0, 0, 0, 0, 0, 0], None).await;
    assert!(error.contains("reserved"), "{error}");
}

#[tokio::test]
async fn rejects_empty_domain_in_socks5_upstream_reply() {
    let error = run_bad_socks5_upstream(&[5, 0, 0, 3, 0, 0, 0], None).await;
    assert!(error.contains("empty"), "{error}");
}

#[tokio::test]
async fn validates_rfc1929_auth_reply_version() {
    let error = run_bad_socks5_upstream(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0], Some([2, 0])).await;
    assert!(error.contains("authentication reply version"), "{error}");
}

async fn run_bad_socks5_upstream(reply: &[u8], auth_reply: Option<[u8; 2]>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let reply = reply.to_vec();
    let mock = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut greeting = [0; 4];
        if let Some(auth_reply) = auth_reply {
            stream.read_exact(&mut greeting).await.unwrap();
            stream.write_all(&[5, 2]).await.unwrap();
            let version = stream.read_u8().await.unwrap();
            assert_eq!(version, 1);
            let user_len = stream.read_u8().await.unwrap() as usize;
            let mut user = vec![0; user_len];
            stream.read_exact(&mut user).await.unwrap();
            let pass_len = stream.read_u8().await.unwrap() as usize;
            let mut pass = vec![0; pass_len];
            stream.read_exact(&mut pass).await.unwrap();
            stream.write_all(&auth_reply).await.unwrap();
        } else {
            stream.read_exact(&mut greeting[..3]).await.unwrap();
            stream.write_all(&[5, 0]).await.unwrap();
        }
        if auth_reply.map(|r| r == [1, 0]).unwrap_or(true) {
            let mut request = [0; 18];
            let _ = stream.read(&mut request).await.unwrap();
            stream.write_all(&reply).await.unwrap();
        }
    });
    let url = if auth_reply.is_some() {
        format!("socks5://user:pass@{addr}")
    } else {
        format!("socks5://{addr}")
    };
    let proxy = UpstreamProxy::parse(&url).unwrap();
    let error = connect_via_proxy(&proxy, &TargetAddr::Domain("x".into(), 80))
        .await
        .unwrap_err()
        .to_string();
    mock.await.unwrap();
    error
}

#[tokio::test]
async fn validates_socks4_response_version_and_embedded_nuls() {
    assert!(UpstreamProxy::parse("socks4://bad%00user@127.0.0.1:1080").is_err());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mock = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 64];
        let _ = stream.read(&mut request).await.unwrap();
        stream.write_all(&[4, 90, 0, 80, 0, 0, 0, 1]).await.unwrap();
    });
    let proxy = UpstreamProxy::parse(&format!("socks4://{addr}")).unwrap();
    let error = connect_via_proxy(&proxy, &TargetAddr::Domain("example.com".into(), 80))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("version"), "{error}");
    mock.await.unwrap();

    let proxy = UpstreamProxy::parse("socks4://127.0.0.1:1").unwrap();
    let error = connect_via_proxy(&proxy, &TargetAddr::Domain("bad\0host".into(), 80))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("NUL"), "{error}");
}

#[test]
fn http_default_port_is_80() {
    assert_eq!(
        UpstreamProxy::parse("http://127.0.0.1").unwrap().address(),
        "127.0.0.1:80"
    );
}

#[tokio::test]
async fn config_failure_gets_socks5_failure_reply() {
    let file = NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "this is not toml").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let path = file.path().to_owned();
    let server = tokio::spawn(async move { serve(listener, path).await });
    let mut client = TcpStream::connect(addr).await.unwrap();
    client.write_all(&[5, 1, 0]).await.unwrap();
    let mut method = [0; 2];
    client.read_exact(&mut method).await.unwrap();
    client
        .write_all(&[5, 1, 0, 1, 127, 0, 0, 1, 0, 80])
        .await
        .unwrap();
    let mut reply = [0; 10];
    tokio::time::timeout(Duration::from_secs(1), client.read_exact(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&reply[..2], &[5, 1]);
    server.abort();
}
