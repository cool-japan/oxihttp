//! Integration tests for HTTP CONNECT proxy, SOCKS5 proxy, and Response::cookies().

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request as HyperRequest, Response as HyperResponse};
use std::convert::Infallible;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use oxihttp_client::{Client, ClientBuilder};

// ---------------------------------------------------------------------------
// Origin server — tiny HTTP/1.1 server that returns a fixed body
// ---------------------------------------------------------------------------

async fn spawn_origin_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind origin");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let _ = http1::Builder::new()
                    .serve_connection(
                        hyper_util::rt::TokioIo::new(stream),
                        service_fn(|_req: HyperRequest<hyper::body::Incoming>| async {
                            Ok::<_, Infallible>(HyperResponse::new(Full::new(Bytes::from(
                                "proxied",
                            ))))
                        }),
                    )
                    .await;
            });
        }
    });
    addr
}

// ---------------------------------------------------------------------------
// Mock HTTP CONNECT proxy
// ---------------------------------------------------------------------------

/// Spawn a minimal HTTP CONNECT proxy server.
///
/// If `require_auth` is `Some("user:pass")`, the proxy reads the
/// `Proxy-Authorization` header and stores it so the test can inspect it.
///
/// Returns `(proxy_addr, received_auth_tx)` where `received_auth_tx` is a
/// channel to receive the base64 `user:pass` credential that was presented by
/// the client, or `None` if no auth header was found.
async fn spawn_http_connect_proxy(
    require_auth: bool,
) -> (
    SocketAddr,
    tokio::sync::mpsc::UnboundedReceiver<Option<String>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
    let proxy_addr = listener.local_addr().expect("local_addr");

    let (auth_tx, auth_rx) = tokio::sync::mpsc::unbounded_channel::<Option<String>>();

    tokio::spawn(async move {
        loop {
            let Ok((mut client_stream, _)) = listener.accept().await else {
                break;
            };
            let auth_tx = auth_tx.clone();
            tokio::spawn(async move {
                // Read the CONNECT request up to \r\n\r\n
                let mut buf = Vec::with_capacity(1024);
                let mut tmp = [0u8; 1];
                loop {
                    match client_stream.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    buf.push(tmp[0]);
                    if buf.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }

                let req_str = String::from_utf8_lossy(&buf);
                let lines: Vec<&str> = req_str.lines().collect();

                // Check for auth header
                let auth_value: Option<String> = lines.iter().find_map(|l| {
                    let lower = l.to_lowercase();
                    if lower.starts_with("proxy-authorization:") {
                        Some(
                            l[l.find(':').map(|i| i + 1).unwrap_or(0)..]
                                .trim()
                                .to_owned(),
                        )
                    } else {
                        None
                    }
                });

                if require_auth {
                    let _ = auth_tx.send(auth_value.clone());
                } else {
                    let _ = auth_tx.send(None);
                }

                // Parse CONNECT target: first line is "CONNECT host:port HTTP/1.1"
                let first_line = lines.first().copied().unwrap_or("");
                let parts: Vec<&str> = first_line.split_whitespace().collect();
                if parts.len() < 2 || parts[0] != "CONNECT" {
                    return;
                }
                let target = parts[1];

                // Connect to the origin
                let Ok(mut origin_stream) = tokio::net::TcpStream::connect(target).await else {
                    let _ = client_stream
                        .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                        .await;
                    return;
                };

                // Reply 200
                let _ = client_stream
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await;

                // Bidirectional pipe
                let _ = tokio::io::copy_bidirectional(&mut client_stream, &mut origin_stream).await;
            });
        }
    });

    (proxy_addr, auth_rx)
}

// ---------------------------------------------------------------------------
// Mock SOCKS5 proxy
// ---------------------------------------------------------------------------

#[cfg(feature = "socks")]
async fn spawn_socks5_proxy(require_auth: bool) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind socks5 proxy");
    let proxy_addr = listener.local_addr().expect("local_addr");

    tokio::spawn(async move {
        loop {
            let Ok((mut client_stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                if socks5_handle_connection(&mut client_stream, require_auth)
                    .await
                    .is_err()
                {
                    // ignore errors in test mock
                }
            });
        }
    });

    proxy_addr
}

#[cfg(feature = "socks")]
async fn socks5_handle_connection(
    stream: &mut tokio::net::TcpStream,
    require_auth: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Step 1: Read greeting
    let mut ver_nmethods = [0u8; 2];
    stream.read_exact(&mut ver_nmethods).await?;
    let _ver = ver_nmethods[0]; // should be 0x05
    let nmethods = ver_nmethods[1] as usize;
    let mut methods = vec![0u8; nmethods];
    stream.read_exact(&mut methods).await?;

    // Step 2: Select method
    let selected = if require_auth && methods.contains(&0x02) {
        0x02u8
    } else if methods.contains(&0x00) {
        0x00u8
    } else {
        stream.write_all(&[0x05, 0xFF]).await?;
        return Err("no acceptable method".into());
    };
    stream.write_all(&[0x05, selected]).await?;

    // Step 3: User/pass auth if selected
    if selected == 0x02 {
        let mut auth_ver = [0u8; 1];
        stream.read_exact(&mut auth_ver).await?;
        let mut ulen = [0u8; 1];
        stream.read_exact(&mut ulen).await?;
        let mut user = vec![0u8; ulen[0] as usize];
        stream.read_exact(&mut user).await?;
        let mut plen = [0u8; 1];
        stream.read_exact(&mut plen).await?;
        let mut pass = vec![0u8; plen[0] as usize];
        stream.read_exact(&mut pass).await?;
        // Accept any credentials in the mock
        stream.write_all(&[0x01, 0x00]).await?;
    }

    // Step 4: Read CONNECT command
    let mut cmd_hdr = [0u8; 4]; // VER, CMD, RSV, ATYP
    stream.read_exact(&mut cmd_hdr).await?;
    let atyp = cmd_hdr[3];

    let target_host = match atyp {
        0x01 => {
            let mut ipv4 = [0u8; 4];
            stream.read_exact(&mut ipv4).await?;
            std::net::IpAddr::V4(std::net::Ipv4Addr::from(ipv4)).to_string()
        }
        0x04 => {
            let mut ipv6 = [0u8; 16];
            stream.read_exact(&mut ipv6).await?;
            std::net::IpAddr::V6(std::net::Ipv6Addr::from(ipv6)).to_string()
        }
        0x03 => {
            let mut len_buf = [0u8; 1];
            stream.read_exact(&mut len_buf).await?;
            let mut domain = vec![0u8; len_buf[0] as usize];
            stream.read_exact(&mut domain).await?;
            String::from_utf8(domain)?
        }
        other => return Err(format!("unknown ATYP {other}").into()),
    };

    let mut port_bytes = [0u8; 2];
    stream.read_exact(&mut port_bytes).await?;
    let target_port = u16::from_be_bytes(port_bytes);
    let target_addr = format!("{target_host}:{target_port}");

    // Step 5: Connect to target
    let Ok(mut origin) = tokio::net::TcpStream::connect(&target_addr).await else {
        // SOCKS5 connection refused reply
        stream
            .write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;
        return Err("could not connect to target".into());
    };

    // Success reply
    stream
        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await?;

    // Pipe
    let _ = tokio::io::copy_bidirectional(stream, &mut origin).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Test 1: HTTP CONNECT proxy round-trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_http_connect_proxy_roundtrip() {
    let origin_addr = spawn_origin_server().await;
    let (proxy_addr, _auth_rx) = spawn_http_connect_proxy(false).await;

    let proxy_uri = format!("http://127.0.0.1:{}", proxy_addr.port())
        .parse::<http::Uri>()
        .expect("proxy uri");

    let client: Client<_> = ClientBuilder::new()
        .with_http_proxy(proxy_uri)
        .build_proxy()
        .expect("build_proxy");

    let url = format!("http://127.0.0.1:{}/", origin_addr.port());
    let resp = client.get(&url).expect("get").send().await.expect("send");
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.body_text().await.expect("body");
    assert_eq!(body, "proxied");
}

// ---------------------------------------------------------------------------
// Test 2: HTTP CONNECT proxy with Proxy-Authorization
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_http_connect_proxy_authorization() {
    let origin_addr = spawn_origin_server().await;
    let (proxy_addr, mut auth_rx) = spawn_http_connect_proxy(true).await;

    // Embed credentials in the proxy URI
    let proxy_uri = format!("http://alice:secret@127.0.0.1:{}", proxy_addr.port())
        .parse::<http::Uri>()
        .expect("proxy uri with auth");

    let client: Client<_> = ClientBuilder::new()
        .with_http_proxy(proxy_uri)
        .build_proxy()
        .expect("build_proxy");

    let url = format!("http://127.0.0.1:{}/", origin_addr.port());
    let resp = client.get(&url).expect("get").send().await.expect("send");
    assert_eq!(resp.status(), http::StatusCode::OK);

    // Check that the proxy received the Proxy-Authorization header
    let received_auth = tokio::time::timeout(std::time::Duration::from_secs(2), auth_rx.recv())
        .await
        .expect("auth recv timeout")
        .expect("auth channel closed");

    let auth_str = received_auth.expect("should have received auth header");
    // "Basic " followed by base64 of "alice:secret"
    assert!(
        auth_str.starts_with("Basic "),
        "expected Basic auth, got: {auth_str}"
    );
    let encoded = &auth_str["Basic ".len()..];
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("base64 decode");
    assert_eq!(decoded, b"alice:secret");
}

// ---------------------------------------------------------------------------
// Test 3: SOCKS5 proxy, no-auth round-trip
// ---------------------------------------------------------------------------

#[cfg(feature = "socks")]
#[tokio::test]
async fn test_socks5_proxy_no_auth_roundtrip() {
    use oxihttp_client::Socks5Connector;

    let origin_addr = spawn_origin_server().await;
    let proxy_addr = spawn_socks5_proxy(false).await;

    let proxy_uri = format!("socks5://127.0.0.1:{}", proxy_addr.port())
        .parse::<http::Uri>()
        .expect("socks5 uri");

    let client: Client<Socks5Connector> = ClientBuilder::new()
        .with_socks5_proxy(proxy_uri)
        .build_socks5_proxy()
        .expect("build_socks5_proxy");

    let url = format!("http://127.0.0.1:{}/", origin_addr.port());
    let resp = client.get(&url).expect("get").send().await.expect("send");
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.body_text().await.expect("body");
    assert_eq!(body, "proxied");
}

// ---------------------------------------------------------------------------
// Test 4: SOCKS5 proxy with user/pass auth
// ---------------------------------------------------------------------------

#[cfg(feature = "socks")]
#[tokio::test]
async fn test_socks5_proxy_user_pass_auth() {
    use oxihttp_client::Socks5Connector;

    let origin_addr = spawn_origin_server().await;
    let proxy_addr = spawn_socks5_proxy(true).await;

    let proxy_uri = format!("socks5://bob:pass@127.0.0.1:{}", proxy_addr.port())
        .parse::<http::Uri>()
        .expect("socks5 uri with auth");

    let client: Client<Socks5Connector> = ClientBuilder::new()
        .with_socks5_proxy(proxy_uri)
        .build_socks5_proxy()
        .expect("build_socks5_proxy");

    let url = format!("http://127.0.0.1:{}/", origin_addr.port());
    let resp = client.get(&url).expect("get").send().await.expect("send");
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.body_text().await.expect("body");
    assert_eq!(body, "proxied");
}

// ---------------------------------------------------------------------------
// Test 5: Response::cookies() — parses multiple Set-Cookie headers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_response_cookies() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let _ = http1::Builder::new()
                    .serve_connection(
                        hyper_util::rt::TokioIo::new(stream),
                        service_fn(|_req: HyperRequest<hyper::body::Incoming>| async {
                            let mut resp = HyperResponse::new(Full::new(Bytes::from("ok")));
                            let hdrs = resp.headers_mut();
                            hdrs.append(
                                http::header::SET_COOKIE,
                                http::HeaderValue::from_static("session=abc123; HttpOnly"),
                            );
                            hdrs.append(
                                http::header::SET_COOKIE,
                                http::HeaderValue::from_static("lang=en; Path=/"),
                            );
                            hdrs.append(
                                http::header::SET_COOKIE,
                                http::HeaderValue::from_static(
                                    "pref=dark; Max-Age=3600; SameSite=Lax",
                                ),
                            );
                            Ok::<_, Infallible>(resp)
                        }),
                    )
                    .await;
            });
        }
    });

    let client = oxihttp::Client::builder().build().expect("client build");
    let url = format!("http://{addr}/cookies");
    let resp = client.get(&url).expect("get").send().await.expect("send");

    let cookies = resp.cookies();
    assert_eq!(
        cookies.len(),
        3,
        "expected 3 cookies, got {}",
        cookies.len()
    );

    let session = cookies
        .iter()
        .find(|c| c.name() == "session")
        .expect("session cookie");
    assert_eq!(session.value(), "abc123");
    assert!(session.is_http_only());

    let lang = cookies
        .iter()
        .find(|c| c.name() == "lang")
        .expect("lang cookie");
    assert_eq!(lang.value(), "en");
    assert_eq!(lang.path(), Some("/"));

    let pref = cookies
        .iter()
        .find(|c| c.name() == "pref")
        .expect("pref cookie");
    assert_eq!(pref.value(), "dark");
    assert_eq!(pref.max_age(), Some(Duration::from_secs(3600)));
    assert_eq!(pref.same_site(), Some(oxihttp::SameSite::Lax));
}
