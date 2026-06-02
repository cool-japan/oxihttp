//! Integration tests for the Compression middleware.
//!
//! Run with:
//!   cargo test -p oxihttp --features compression --test compression_test

#![cfg(feature = "compression")]

use bytes::Bytes;
use http::StatusCode;
use http_body_util::Full;
use oxihttp_server::{Compression, CompressionAlgorithm};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

/// Large enough payload to exceed the default min_size of 1024 bytes.
const LARGE_PAYLOAD: &str = "PAYLOAD_DATA_";
const LARGE_REPEAT: usize = 200; // 200 * 13 = 2600 bytes

/// Small payload below the default min_size of 1024 bytes.
const SMALL_PAYLOAD: &str = "tiny";

/// Spawn a hyper/tokio HTTP server that applies compression before returning.
///
/// The server:
/// 1. Reads the `Accept-Encoding` request header.
/// 2. Builds a response with the given body.
/// 3. Applies `Compression::new()` with the provided config overrides.
/// 4. Returns the (possibly compressed) response.
async fn spawn_compression_server(
    body_content: String,
    min_size: usize,
) -> (SocketAddr, tokio::sync::oneshot::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let body_content = Arc::new(body_content);

    tokio::spawn(async move {
        let compression = Arc::new(Compression::new().min_size(min_size));

        tokio::select! {
            _ = rx => {}
            _ = async {
                loop {
                    let Ok((stream, _)) = listener.accept().await else { break };
                    let compression = Arc::clone(&compression);
                    let body_content = Arc::clone(&body_content);

                    tokio::spawn(async move {
                        let io = hyper_util::rt::TokioIo::new(stream);
                        let svc = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                            let compression = Arc::clone(&compression);
                            let body_content = Arc::clone(&body_content);
                            async move {
                                let accept_encoding = req
                                    .headers()
                                    .get("accept-encoding")
                                    .and_then(|v| v.to_str().ok())
                                    .map(|s| s.to_string());

                                let resp = hyper::Response::builder()
                                    .status(StatusCode::OK)
                                    .header("content-type", "text/plain")
                                    .body(Full::new(Bytes::from(body_content.as_str().to_string())))
                                    .expect("build response");

                                let compressed = compression
                                    .apply(accept_encoding.as_deref(), resp)
                                    .await
                                    .expect("compression apply");

                                Ok::<_, Infallible>(compressed)
                            }
                        });

                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(io, svc)
                            .await;
                    });
                }
            } => {}
        }
    });

    tokio::time::sleep(Duration::from_millis(10)).await;

    (addr, tx)
}

/// Build a large body string.
fn large_body() -> String {
    LARGE_PAYLOAD.repeat(LARGE_REPEAT)
}

/// Send a raw HTTP/1.1 request and return the full raw response bytes.
async fn raw_request(addr: SocketAddr, accept_encoding: Option<&str>) -> Vec<u8> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let mut stream = TcpStream::connect(addr).await.expect("connect");

    let ae_header = match accept_encoding {
        Some(ae) => format!("Accept-Encoding: {ae}\r\n"),
        None => String::new(),
    };

    let request =
        format!("GET / HTTP/1.1\r\nHost: localhost\r\n{ae_header}Connection: close\r\n\r\n");

    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    stream.flush().await.expect("flush");

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read response");

    response
}

/// Parse HTTP/1.1 response bytes into (status line, headers map, body bytes).
fn parse_response(raw: &[u8]) -> (String, std::collections::HashMap<String, String>, Vec<u8>) {
    let separator = b"\r\n\r\n";
    let header_end = raw
        .windows(separator.len())
        .position(|w| w == separator)
        .expect("header/body separator");

    let header_bytes = &raw[..header_end];
    let body_bytes = raw[header_end + separator.len()..].to_vec();

    let header_str = String::from_utf8_lossy(header_bytes);
    let mut lines = header_str.lines();

    let status_line = lines.next().unwrap_or_default().to_string();

    let mut headers = std::collections::HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }

    (status_line, headers, body_bytes)
}

#[tokio::test]
async fn test_gzip_response() {
    let (addr, _shutdown) = spawn_compression_server(large_body(), 1024).await;

    let raw = raw_request(addr, Some("gzip")).await;
    let (status_line, headers, body) = parse_response(&raw);

    assert!(
        status_line.contains("200"),
        "Expected 200, got: {status_line}"
    );
    assert_eq!(
        headers.get("content-encoding").map(|s| s.as_str()),
        Some("gzip"),
        "Expected content-encoding: gzip. Headers: {headers:?}"
    );

    // Decompress and verify the original payload
    let decompressed =
        oxiarc_deflate::gzip_decompress(&body).expect("gzip_decompress should succeed");
    let decompressed_str = String::from_utf8(decompressed).expect("valid UTF-8");
    assert_eq!(decompressed_str, large_body());
}

#[tokio::test]
async fn test_deflate_response() {
    let (addr, _shutdown) = spawn_compression_server(large_body(), 1024).await;

    let raw = raw_request(addr, Some("deflate")).await;
    let (status_line, headers, body) = parse_response(&raw);

    assert!(
        status_line.contains("200"),
        "Expected 200, got: {status_line}"
    );
    assert_eq!(
        headers.get("content-encoding").map(|s| s.as_str()),
        Some("deflate"),
        "Expected content-encoding: deflate. Headers: {headers:?}"
    );

    // Decompress (deflate = zlib-wrapped DEFLATE per RFC 7230)
    let decompressed =
        oxiarc_deflate::zlib_decompress(&body).expect("zlib_decompress should succeed");
    let decompressed_str = String::from_utf8(decompressed).expect("valid UTF-8");
    assert_eq!(decompressed_str, large_body());
}

#[tokio::test]
async fn test_no_accept_encoding_uncompressed() {
    let (addr, _shutdown) = spawn_compression_server(large_body(), 1024).await;

    let raw = raw_request(addr, None).await;
    let (status_line, headers, body) = parse_response(&raw);

    assert!(
        status_line.contains("200"),
        "Expected 200, got: {status_line}"
    );
    assert!(
        !headers.contains_key("content-encoding"),
        "Expected no content-encoding header. Headers: {headers:?}"
    );

    // Body should be the raw uncompressed payload
    let body_str = String::from_utf8(body).expect("valid UTF-8");
    assert_eq!(body_str, large_body());
}

#[tokio::test]
async fn test_small_body_uncompressed() {
    // Small payload (below default min_size of 1024)
    let (addr, _shutdown) = spawn_compression_server(SMALL_PAYLOAD.to_string(), 1024).await;

    let raw = raw_request(addr, Some("gzip")).await;
    let (_status_line, headers, body) = parse_response(&raw);

    assert!(
        !headers.contains_key("content-encoding"),
        "Small body should not be compressed. Headers: {headers:?}"
    );

    let body_str = String::from_utf8(body).expect("valid UTF-8");
    assert_eq!(body_str, SMALL_PAYLOAD);
}

#[tokio::test]
async fn test_gzip_q0_deflate_fallback() {
    let (addr, _shutdown) = spawn_compression_server(large_body(), 1024).await;

    // gzip explicitly refused; deflate should be selected
    let raw = raw_request(addr, Some("gzip;q=0, deflate")).await;
    let (status_line, headers, body) = parse_response(&raw);

    assert!(
        status_line.contains("200"),
        "Expected 200, got: {status_line}"
    );
    assert_eq!(
        headers.get("content-encoding").map(|s| s.as_str()),
        Some("deflate"),
        "Expected content-encoding: deflate (gzip q=0). Headers: {headers:?}"
    );

    let decompressed =
        oxiarc_deflate::zlib_decompress(&body).expect("zlib_decompress should succeed");
    let decompressed_str = String::from_utf8(decompressed).expect("valid UTF-8");
    assert_eq!(decompressed_str, large_body());
}

#[tokio::test]
async fn test_vary_header_set_on_compression() {
    let (addr, _shutdown) = spawn_compression_server(large_body(), 1024).await;

    let raw = raw_request(addr, Some("gzip")).await;
    let (_status_line, headers, _body) = parse_response(&raw);

    assert_eq!(
        headers.get("vary").map(|s| s.as_str()),
        Some("Accept-Encoding"),
        "Vary: Accept-Encoding must be set when compression is applied. Headers: {headers:?}"
    );
}

#[tokio::test]
async fn test_gzip_q_values_negotiated() {
    // Client prefers br (not offered), then gzip;q=0.9, then deflate;q=0.5
    // Our server prefers gzip → should select gzip
    let (addr, _shutdown) = spawn_compression_server(large_body(), 1024).await;

    let raw = raw_request(addr, Some("br, gzip;q=0.9, deflate;q=0.5")).await;
    let (_status_line, headers, body) = parse_response(&raw);

    assert_eq!(
        headers.get("content-encoding").map(|s| s.as_str()),
        Some("gzip"),
        "Expected gzip (server preference). Headers: {headers:?}"
    );

    let decompressed =
        oxiarc_deflate::gzip_decompress(&body).expect("gzip_decompress should succeed");
    let decompressed_str = String::from_utf8(decompressed).expect("valid UTF-8");
    assert_eq!(decompressed_str, large_body());
}

#[tokio::test]
async fn test_custom_min_size() {
    // min_size = 10, payload = 13 bytes (should compress)
    let payload = "hello_world!!".to_string(); // 13 bytes
    let (addr, _shutdown) = spawn_compression_server(payload.clone(), 10).await;

    let raw = raw_request(addr, Some("gzip")).await;
    let (_status_line, headers, body) = parse_response(&raw);

    assert_eq!(
        headers.get("content-encoding").map(|s| s.as_str()),
        Some("gzip"),
        "Expected gzip compression for payload above custom min_size. Headers: {headers:?}"
    );

    let decompressed =
        oxiarc_deflate::gzip_decompress(&body).expect("gzip_decompress should succeed");
    assert_eq!(String::from_utf8(decompressed).expect("UTF-8"), payload);
}

/// Verify that a client that explicitly requests gzip via Accept-Encoding
/// receives a compressed response and can decompress it to the original payload.
///
/// This test uses a raw connection to send Accept-Encoding and manually
/// decompresses the body.  See also `test_auto_decompression_enabled_via_builder`
/// which exercises the same flow through `ClientBuilder::with_decompression(true)`.
#[tokio::test]
async fn test_client_receives_gzip_and_decompresses() {
    let (addr, _shutdown) = spawn_compression_server(large_body(), 1024).await;

    // Send the request with Accept-Encoding: gzip so the server compresses it.
    let raw = raw_request(addr, Some("gzip")).await;
    let (status_line, headers, body) = parse_response(&raw);

    assert!(
        status_line.contains("200"),
        "Expected 200, got: {status_line}"
    );

    // Confirm the server actually compressed the response.
    assert_eq!(
        headers.get("content-encoding").map(|s| s.as_str()),
        Some("gzip"),
        "Expected gzip content-encoding from server. Headers: {headers:?}"
    );

    // Manual client-side decompression to verify the compressed payload round-trips.
    let plaintext =
        oxiarc_deflate::gzip_decompress(&body).expect("client-side gzip_decompress failed");
    let text = String::from_utf8(plaintext).expect("valid UTF-8 after decompression");
    assert_eq!(
        text,
        large_body(),
        "Decompressed body does not match original payload"
    );
}

/// Verify that `ClientBuilder::with_decompression(true)` automatically sends
/// `Accept-Encoding: gzip, deflate` and transparently decompresses the response
/// body so that `body_text()` returns the original plaintext without manual
/// decompression.
///
/// This test proves the full wiring: builder flag → Accept-Encoding header →
/// Content-Encoding on response → automatic decompression inside `body_bytes()`.
#[tokio::test]
#[cfg(feature = "decompression")]
async fn test_auto_decompression_enabled_via_builder() {
    // Spin up the shared compression server with a large payload so the server
    // actually compresses (min_size = 1024, payload > 1024 bytes).
    let (addr, _shutdown) = spawn_compression_server(large_body(), 1024).await;
    let url = format!("http://{addr}/");

    // Build a client with auto-decompression enabled.
    let client = oxihttp::Client::builder()
        .with_decompression(true)
        .build()
        .expect("client build");

    let resp = client
        .get(&url)
        .expect("GET builder")
        .send()
        .await
        .expect("GET send");

    assert_eq!(
        resp.status(),
        oxihttp::StatusCode::OK,
        "Expected 200 OK from compression server"
    );

    // body_text() must return the original uncompressed payload — no manual
    // decompress step should be needed when with_decompression(true) is set.
    let text = resp.body_text().await.expect("body_text");
    assert_eq!(
        text,
        large_body(),
        "auto-decompressed body does not match original payload"
    );
}

// Unit tests for CompressionAlgorithm
#[test]
fn test_algorithm_as_str() {
    assert_eq!(CompressionAlgorithm::Gzip.as_str(), "gzip");
    assert_eq!(CompressionAlgorithm::Deflate.as_str(), "deflate");
}

#[test]
fn test_compression_config_defaults() {
    use oxihttp_server::CompressionConfig;
    let config = CompressionConfig::default();
    assert_eq!(config.min_size, 1024);
    assert_eq!(config.level, 6);
    assert_eq!(
        config.algorithms,
        vec![CompressionAlgorithm::Gzip, CompressionAlgorithm::Deflate]
    );
}
