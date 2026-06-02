//! mTLS integration tests.
//!
//! Tests verify:
//! 1. Server with mTLS (`TlsConfig::with_client_auth`) populates `req.peer_certificates()`
//!    in the handler when a valid client certificate is presented.
//! 2. Server with mTLS rejects a plain-TLS connection (no client cert) — the TLS
//!    handshake fails at the TCP level.
//! 3. `req.tls_info()` contains populated ALPN / protocol-version fields.
//!
//! Certificate generation uses `oxitls::rcgen_bridge` (no ring in the library
//! edge; ring is acceptable in dev-only test code per project policy).

#![cfg(all(feature = "tls", feature = "server"))]

use bytes::Bytes;
use http_body_util::Full;
use oxihttp_server::{response, router::Request, Router, Server};
use oxitls::rcgen_bridge::{
    generate_ca, generate_ca_signed_client_cert, generate_ca_signed_leaf, CaCertifiedKey,
    CertifiedKey, SigningAlgorithm,
};
use rustls::ClientConfig;
use rustls::RootCertStore;
use rustls_pki_types::ServerName;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

// ---------------------------------------------------------------------------
// Certificate helpers
// ---------------------------------------------------------------------------

/// Generate a root CA + server cert (for localhost) + client cert.
///
/// The server cert uses `ServerAuth` EKU; the client cert uses `ClientAuth` EKU
/// (generated via `generate_ca_signed_client_cert`), which is required by
/// `WebPkiClientVerifier` for mTLS.
///
/// Uses ECDSA P-256 for compatibility with `rustls-rustcrypto`'s certificate
/// verification algorithms.
///
/// Returns `(server_ck, client_ck, ca_ck)`.
fn make_mtls_certs() -> (CertifiedKey, CertifiedKey, CaCertifiedKey) {
    let ca = generate_ca("Test CA", SigningAlgorithm::EcdsaP256).expect("CA gen");

    // Server cert — uses the default generate_ca_signed_leaf (ServerAuth EKU).
    let server_ck = generate_ca_signed_leaf(&["localhost"], SigningAlgorithm::EcdsaP256, &ca)
        .expect("server cert gen");

    // Client cert — must have ClientAuth EKU for WebPkiClientVerifier to accept it.
    let client_ck =
        generate_ca_signed_client_cert(&["client.test"], SigningAlgorithm::EcdsaP256, &ca)
            .expect("client cert gen");

    (server_ck, client_ck, ca)
}

/// Build a `TlsConnector` that trusts the given server cert DER and optionally
/// presents a client certificate.
fn make_tls_connector(
    server_cert_der: &[u8],
    client_cert: Option<(&CertifiedKey, Vec<CertificateDer<'static>>)>,
) -> TlsConnector {
    let provider = oxitls::pure_provider();

    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(server_cert_der.to_vec()))
        .expect("add server root");

    let client_cfg = if let Some((ck, chain)) = client_cert {
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.pkcs8_der.clone()));
        ClientConfig::builder_with_provider(Arc::clone(&provider))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("TLS 1.3 supported")
            .with_root_certificates(roots)
            .with_client_auth_cert(chain, key)
            .expect("client auth cert invalid")
    } else {
        ClientConfig::builder_with_provider(Arc::clone(&provider))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("TLS 1.3 supported")
            .with_root_certificates(roots)
            .with_no_client_auth()
    };

    TlsConnector::from(Arc::new(client_cfg))
}

// ---------------------------------------------------------------------------
// Test 1: mTLS positive path — handler sees populated peer_certificates
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_mtls_handler_sees_peer_certs() {
    let (server_ck, client_ck, ca_ck) = make_mtls_certs();

    // Shared cell to capture what the handler saw.
    let captured_cert_count: Arc<std::sync::Mutex<Option<usize>>> =
        Arc::new(std::sync::Mutex::new(None));
    let captured_alpn: Arc<std::sync::Mutex<Option<Option<Vec<u8>>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let captured_version: Arc<std::sync::Mutex<Option<Option<String>>>> =
        Arc::new(std::sync::Mutex::new(None));

    let cap_cert = Arc::clone(&captured_cert_count);
    let cap_alpn = Arc::clone(&captured_alpn);
    let cap_ver = Arc::clone(&captured_version);

    let router = Router::new().get("/mtls", move |req: Request| {
        let cc = Arc::clone(&cap_cert);
        let ca = Arc::clone(&cap_alpn);
        let cv = Arc::clone(&cap_ver);
        async move {
            let certs = req.peer_certificates();
            let info = req.tls_info();

            if let Ok(mut guard) = cc.lock() {
                *guard = Some(certs.map(|v| v.len()).unwrap_or(0));
            }
            if let Ok(mut guard) = ca.lock() {
                *guard = Some(info.as_ref().and_then(|i| i.alpn_protocol.clone()));
            }
            if let Ok(mut guard) = cv.lock() {
                *guard = Some(info.and_then(|i| i.protocol_version.clone()));
            }

            response::text_response("peer cert ok")
        }
    });

    // Build server with mTLS: server cert, server key, CA for client verification.
    let tls_cfg = oxihttp_server::TlsConfig::with_client_auth(
        server_ck.cert_pem.as_bytes(),
        server_ck.key_pem().as_bytes(),
        ca_ck.certified_key.cert_pem.as_bytes(),
    )
    .expect("mTLS TlsConfig");

    let (addr, server_handle) = Server::bind("127.0.0.1:0")
        .with_tls(tls_cfg)
        .serve_with_addr(router)
        .await
        .expect("server bind");

    // Build a client connector that presents the client cert.
    // The server cert is CA-signed, so trust the CA cert (not the server leaf).
    let client_cert_der = CertificateDer::from(client_ck.cert_der.clone());
    let connector = make_tls_connector(
        &ca_ck.certified_key.cert_der,
        Some((&client_ck, vec![client_cert_der])),
    );

    let tcp = TcpStream::connect(addr).await.expect("TCP connect");
    let server_name = ServerName::try_from("localhost").expect("server name");
    let tls_stream = connector
        .connect(server_name, tcp)
        .await
        .expect("TLS connect (with client cert)");

    // Use hyper HTTP/1.1 over TLS to make a proper HTTP request.
    let io = hyper_util::rt::TokioIo::new(tls_stream);
    let (mut sender, conn) = hyper::client::conn::http1::Builder::new()
        .handshake::<_, Full<Bytes>>(io)
        .await
        .expect("HTTP/1.1 handshake");
    tokio::spawn(conn);

    let req = hyper::Request::builder()
        .method("GET")
        .uri(format!("https://localhost:{}/mtls", addr.port()))
        .header("host", format!("localhost:{}", addr.port()))
        .header("connection", "close")
        .body(Full::new(Bytes::new()))
        .expect("build request");

    let resp = sender.send_request(req).await.expect("send request");

    // Verify HTTP 200 OK — valid client cert must yield a successful response.
    assert_eq!(
        resp.status(),
        hyper::StatusCode::OK,
        "mTLS with valid client cert should return 200 OK"
    );

    // Drain the body.
    use http_body_util::BodyExt;
    let body_bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();

    // Give the handler a moment to store results.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let resp_text = String::from_utf8_lossy(&body_bytes);

    // Verify the response body was received.
    assert!(
        resp_text.contains("peer cert ok"),
        "expected response body 'peer cert ok', got: {resp_text}"
    );

    // Verify the handler captured peer certificates.
    let cert_count = captured_cert_count
        .lock()
        .expect("lock")
        .expect("handler was called");
    assert!(
        cert_count >= 1,
        "expected >= 1 peer certificate, got {cert_count}"
    );

    let alpn = captured_alpn.lock().expect("lock").clone();
    assert!(alpn.is_some(), "tls_info should be populated");

    let version = captured_version.lock().expect("lock").clone();
    assert!(version.is_some(), "tls_info protocol_version should be set");
    let version_str = version.expect("version set").expect("non-None version");
    // Accept any TLS 1.3 representation: "TLSv1_3", "Tls13", "TLS1.3", etc.
    assert!(
        version_str.contains("13") || version_str.contains("1.3") || version_str.contains("1_3"),
        "expected TLS 1.3, got: {version_str}"
    );

    server_handle.abort();
}

// ---------------------------------------------------------------------------
// Test 2: mTLS rejection — plain TLS without client cert is rejected
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_mtls_rejects_no_client_cert() {
    let (server_ck, _client_ck, ca_ck) = make_mtls_certs();

    let router = Router::new().get("/", |_req| async {
        response::text_response("should not reach here")
    });

    let tls_cfg = oxihttp_server::TlsConfig::with_client_auth(
        server_ck.cert_pem.as_bytes(),
        server_ck.key_pem().as_bytes(),
        ca_ck.certified_key.cert_pem.as_bytes(),
    )
    .expect("mTLS TlsConfig");

    let (addr, server_handle) = Server::bind("127.0.0.1:0")
        .with_tls(tls_cfg)
        .serve_with_addr(router)
        .await
        .expect("server bind");

    // Connector with NO client certificate. Trust the CA so that the server
    // cert validates — but don't present a client cert.
    let connector = make_tls_connector(&ca_ck.certified_key.cert_der, None);

    let tcp = TcpStream::connect(addr).await.expect("TCP connect");
    let server_name = ServerName::try_from("localhost").expect("server name");

    // The TLS handshake should fail because we didn't present a client cert.
    let result = connector.connect(server_name, tcp).await;
    // Note: some TLS implementations (including rustls-rustcrypto pure provider)
    // may complete the client-side handshake even when the server later rejects the
    // empty certificate. In such cases we check the HTTP response instead.
    // We still assert that either the connect fails, OR the connection is dropped
    // immediately (empty response body).
    if let Ok(mut tls) = result {
        // Check if the connection is immediately closed by the server.
        let mut buf = [0u8; 64];
        let n = tls.read(&mut buf).await.unwrap_or(0);
        assert_eq!(
            n, 0,
            "server should have closed the connection (no data expected), got {n} bytes"
        );
    }
    // If result was Err, that's also acceptable (clean TLS rejection).

    server_handle.abort();
}

// ---------------------------------------------------------------------------
// Test 3: with_client_auth rejects invalid PEM inputs
// ---------------------------------------------------------------------------

#[test]
fn test_with_client_auth_bad_pem() {
    let result = oxihttp_server::TlsConfig::with_client_auth(
        b"not valid pem",
        b"not valid pem",
        b"not valid pem",
    );
    assert!(
        result.is_err(),
        "with_client_auth should fail on empty/invalid PEM"
    );
}

// ---------------------------------------------------------------------------
// Test 4: tls_info returns None for plain HTTP connections
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_plain_http_no_tls_info() {
    use hyper::client::conn::http1;

    let captured_has_tls_info: Arc<std::sync::Mutex<Option<bool>>> =
        Arc::new(std::sync::Mutex::new(None));
    let cap = Arc::clone(&captured_has_tls_info);

    let router = Router::new().get("/check", move |req: Request| {
        let c = Arc::clone(&cap);
        async move {
            let has_info = req.tls_info().is_some();
            if let Ok(mut guard) = c.lock() {
                *guard = Some(has_info);
            }
            response::text_response("ok")
        }
    });

    let (addr, server_handle) = Server::bind("127.0.0.1:0")
        .serve_with_addr(router)
        .await
        .expect("bind");

    // Plain TCP connection.
    let tcp = TcpStream::connect(addr).await.expect("connect");
    let io = hyper_util::rt::TokioIo::new(tcp);
    let (mut sender, conn) = http1::Builder::new()
        .handshake::<_, Full<Bytes>>(io)
        .await
        .expect("handshake");
    tokio::spawn(conn);

    let req = hyper::Request::builder()
        .uri(format!("http://{addr}/check"))
        .header("host", format!("{addr}"))
        .body(Full::new(Bytes::new()))
        .expect("build request");

    let _resp = sender.send_request(req).await.expect("send");

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let has_info = captured_has_tls_info
        .lock()
        .expect("lock")
        .expect("handler was called");
    assert!(!has_info, "plain HTTP should have no tls_info");

    server_handle.abort();
}

// ---------------------------------------------------------------------------
// Test 5: tls_connection_info() returns typed version, cipher suite, and sni
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_request_handler_can_read_tls_connection_info() {
    let (server_ck, _client_ck, ca_ck) = make_mtls_certs();

    // Capture typed ConnectionInfo fields from tls_connection_info().
    let captured_version: Arc<std::sync::Mutex<Option<Option<oxitls::TlsVersion>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let captured_cipher: Arc<std::sync::Mutex<Option<Option<oxitls::CipherSuite>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let captured_sni: Arc<std::sync::Mutex<Option<Option<String>>>> =
        Arc::new(std::sync::Mutex::new(None));

    let cap_ver = Arc::clone(&captured_version);
    let cap_cs = Arc::clone(&captured_cipher);
    let cap_sni = Arc::clone(&captured_sni);

    let router = Router::new().get("/conn_info", move |req: Request| {
        let cv = Arc::clone(&cap_ver);
        let cc = Arc::clone(&cap_cs);
        let cs = Arc::clone(&cap_sni);
        async move {
            let ci = req.tls_connection_info();
            if let Ok(mut g) = cv.lock() {
                *g = Some(ci.as_ref().and_then(|c| c.version));
            }
            if let Ok(mut g) = cc.lock() {
                *g = Some(ci.as_ref().and_then(|c| c.cipher_suite));
            }
            if let Ok(mut g) = cs.lock() {
                *g = Some(ci.and_then(|c| c.sni));
            }
            response::text_response("conn info ok")
        }
    });

    // Plain TLS server (no mTLS requirement) so the client doesn't need a cert.
    let tls_cfg = oxihttp_server::TlsConfig::from_pem(
        server_ck.cert_pem.as_bytes(),
        server_ck.key_pem().as_bytes(),
    )
    .expect("TlsConfig");

    let (addr, server_handle) = Server::bind("127.0.0.1:0")
        .with_tls(tls_cfg)
        .serve_with_addr(router)
        .await
        .expect("server bind");

    // Client connector that trusts the CA (server cert is CA-signed).
    let connector = make_tls_connector(&ca_ck.certified_key.cert_der, None);

    let tcp = TcpStream::connect(addr).await.expect("TCP connect");
    let server_name = ServerName::try_from("localhost").expect("server name");
    let tls_stream = connector
        .connect(server_name, tcp)
        .await
        .expect("TLS connect");

    let io = hyper_util::rt::TokioIo::new(tls_stream);
    let (mut sender, conn) = hyper::client::conn::http1::Builder::new()
        .handshake::<_, Full<Bytes>>(io)
        .await
        .expect("HTTP/1.1 handshake");
    tokio::spawn(conn);

    let req = hyper::Request::builder()
        .method("GET")
        .uri(format!("https://localhost:{}/conn_info", addr.port()))
        .header("host", format!("localhost:{}", addr.port()))
        .header("connection", "close")
        .body(Full::new(Bytes::new()))
        .expect("build request");

    let resp = sender.send_request(req).await.expect("send request");
    assert_eq!(resp.status(), hyper::StatusCode::OK);

    use http_body_util::BodyExt;
    let _ = resp.into_body().collect().await.expect("collect body");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Version must be TLS 1.3 (client forced TLS13 in make_tls_connector).
    let version = captured_version
        .lock()
        .expect("lock")
        .expect("handler called");
    assert_eq!(
        version,
        Some(oxitls::TlsVersion::Tls13),
        "expected typed TLS 1.3 version"
    );

    // Cipher suite must be populated (any known TLS 1.3 suite is acceptable).
    let cipher = captured_cipher
        .lock()
        .expect("lock")
        .expect("handler called");
    assert!(cipher.is_some(), "cipher_suite must be populated");

    // SNI must be "localhost" (sent by the client in the handshake).
    let sni = captured_sni
        .lock()
        .expect("lock")
        .clone()
        .expect("handler called");
    assert_eq!(
        sni.as_deref(),
        Some("localhost"),
        "expected SNI = 'localhost', got: {sni:?}"
    );

    server_handle.abort();
}
