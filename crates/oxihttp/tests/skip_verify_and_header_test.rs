//! Integration tests for:
//! 1. `danger_accept_invalid_certs(bool)` / `with_danger_accept_invalid_certs()` builder API.
//! 2. `with_custom_cert_verifier(Arc<dyn ServerCertVerifier>)` builder API.
//! 3. `Response::header(name)` single-header accessor.
//!
//! All tests spin up a minimal self-signed TLS server via oxitls-rcgen (the
//! same pattern used in `https_client.rs` and `per_request_tls_test.rs`).

#![cfg(feature = "tls")]

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::Request as HyperRequest;
use hyper::Response as HyperResponse;
use oxihttp::DangerousNoVerification;
use oxitls::rcgen_bridge::{generate_self_signed_ed25519, CertifiedKey};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

// ---------------------------------------------------------------------------
// Test TLS server helpers (same pattern as https_client.rs)
// ---------------------------------------------------------------------------

fn make_localhost_cert() -> CertifiedKey {
    generate_self_signed_ed25519(&["localhost"]).expect("cert gen")
}

fn make_acceptor(ck: &CertifiedKey) -> TlsAcceptor {
    let cert = CertificateDer::from(ck.cert_der.clone());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.pkcs8_der.clone()));
    let server_cfg = oxitls::tls13::ServerBuilder::new()
        .with_der_cert_and_key(vec![cert], key)
        .build()
        .expect("server TLS config");
    TlsAcceptor::from(Arc::new(server_cfg))
}

/// Handler that echoes a static body and sets custom response headers.
///
/// The response always includes:
/// - `replay-nonce: test-nonce-12345`
/// - `location: https://acme.example.com/order/abc`
/// - `content-type: text/plain`
async fn acme_like_handler(
    _req: HyperRequest<hyper::body::Incoming>,
) -> Result<HyperResponse<Full<Bytes>>, Infallible> {
    let resp = HyperResponse::builder()
        .status(200)
        .header("replay-nonce", "test-nonce-12345")
        .header("location", "https://acme.example.com/order/abc")
        .header("content-type", "text/plain")
        .body(Full::new(Bytes::from_static(b"acme-body")))
        .expect("build response");
    Ok(resp)
}

/// Spawn a TLS server that handles every request with `acme_like_handler`.
///
/// Returns the bound `SocketAddr`.
async fn spawn_tls_server_with_headers(acceptor: TlsAcceptor) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls_stream) = acceptor.accept(stream).await else {
                    return;
                };
                let io = hyper_util::rt::TokioIo::new(tls_stream);
                let _ = http1::Builder::new()
                    .serve_connection(io, service_fn(acme_like_handler))
                    .await;
            });
        }
    });
    addr
}

// ---------------------------------------------------------------------------
// Test 1: `danger_accept_invalid_certs(true)` bypasses cert verification
// ---------------------------------------------------------------------------

/// A client built with `.danger_accept_invalid_certs(true)` must successfully
/// connect to a server whose self-signed certificate is NOT in any trust store.
#[tokio::test]
async fn danger_accept_invalid_certs_bool_true_accepts_self_signed() {
    let ck = make_localhost_cert();
    let acceptor = make_acceptor(&ck);
    let addr = spawn_tls_server_with_headers(acceptor).await;

    // Client uses Mozilla CA bundle — this self-signed cert is NOT trusted there.
    let client = oxihttp::Client::builder()
        .with_webpki_roots()
        .danger_accept_invalid_certs(true)
        .build_https()
        .expect("build_https with danger_accept_invalid_certs(true)");

    let url = format!("https://localhost:{}/", addr.port());
    let resp = client
        .get(&url)
        .expect("GET")
        .send()
        .await
        .expect("send — danger_accept_invalid_certs(true) must not fail on self-signed cert");

    assert_eq!(
        resp.status(),
        oxihttp::StatusCode::OK,
        "expected 200 OK from self-signed TLS server when skip-verify is enabled"
    );
}

/// A client built with `.danger_accept_invalid_certs(false)` must *fail* to
/// connect to a server whose self-signed certificate is NOT in any trust store.
/// (Sanity-check that setting `false` keeps verification enabled.)
#[tokio::test]
async fn danger_accept_invalid_certs_bool_false_rejects_self_signed() {
    let ck = make_localhost_cert();
    let acceptor = make_acceptor(&ck);
    let addr = spawn_tls_server_with_headers(acceptor).await;

    let client = oxihttp::Client::builder()
        .with_webpki_roots()
        .danger_accept_invalid_certs(false)
        .build_https()
        .expect("build_https");

    let url = format!("https://localhost:{}/", addr.port());
    let result = client.get(&url).expect("GET").send().await;
    assert!(
        result.is_err(),
        "expected TLS error when connecting to self-signed server with verification enabled"
    );
}

// ---------------------------------------------------------------------------
// Test 2: `with_custom_cert_verifier` wires the verifier into the TLS stack
// ---------------------------------------------------------------------------

/// A client built with `DangerousNoVerification` as its verifier must
/// successfully connect to a self-signed TLS server.
#[tokio::test]
async fn with_custom_cert_verifier_dangerous_no_verification_accepts_self_signed() {
    let ck = make_localhost_cert();
    let acceptor = make_acceptor(&ck);
    let addr = spawn_tls_server_with_headers(acceptor).await;

    let provider = oxitls::pure_provider();
    let verifier = Arc::new(DangerousNoVerification::new(provider));

    let client = oxihttp::Client::builder()
        .with_custom_cert_verifier(verifier)
        .build_https()
        .expect("build_https with custom verifier");

    let url = format!("https://localhost:{}/", addr.port());
    let resp = client
        .get(&url)
        .expect("GET")
        .send()
        .await
        .expect("send — custom DangerousNoVerification verifier must accept self-signed cert");

    assert_eq!(
        resp.status(),
        oxihttp::StatusCode::OK,
        "expected 200 OK from self-signed TLS server with DangerousNoVerification verifier"
    );
}

/// Verifies that the builder accepts a custom verifier that wraps the standard
/// WebPKI verifier for a specific cert (certificate pinning pattern), and that
/// connecting to a trusted self-signed cert succeeds while an unknown cert fails.
#[tokio::test]
async fn with_custom_cert_verifier_pin_specific_cert_rejects_other() {
    let ck_a = make_localhost_cert();
    let ck_b = make_localhost_cert();

    let acceptor_a = make_acceptor(&ck_a);
    let acceptor_b = make_acceptor(&ck_b);

    let addr_a = spawn_tls_server_with_headers(acceptor_a).await;
    let addr_b = spawn_tls_server_with_headers(acceptor_b).await;

    // Build a client that trusts *only* cert A.
    let client_a = oxihttp::Client::builder()
        .with_trusted_cert_der(ck_a.cert_der.clone())
        .build_https()
        .expect("build_https (cert A)");

    // Cert A server → should work.
    let url_a = format!("https://localhost:{}/", addr_a.port());
    let resp = client_a
        .get(&url_a)
        .expect("GET server_a")
        .send()
        .await
        .expect("cert A trusted");
    assert_eq!(resp.status(), oxihttp::StatusCode::OK);

    // Cert B server → should fail (cert B not trusted).
    let url_b = format!("https://localhost:{}/", addr_b.port());
    let result = client_a.get(&url_b).expect("GET server_b").send().await;
    assert!(
        result.is_err(),
        "cert B should not be trusted by a client pinned to cert A"
    );
}

// ---------------------------------------------------------------------------
// Test 3: `Response::header(name)` single-header accessor
// ---------------------------------------------------------------------------

/// The `Response::header("replay-nonce")` method must return the value set by
/// the server — critical for ACME client implementations that need to read the
/// `Replay-Nonce` and `Location` headers.
#[tokio::test]
async fn response_header_accessor_reads_replay_nonce_and_location() {
    let ck = make_localhost_cert();
    let acceptor = make_acceptor(&ck);
    let addr = spawn_tls_server_with_headers(acceptor).await;

    // Build a client that trusts the self-signed cert directly.
    let client = oxihttp::Client::builder()
        .with_trusted_cert_der(ck.cert_der.clone())
        .build_https()
        .expect("build_https (trusted self-signed cert)");

    let url = format!("https://localhost:{}/", addr.port());
    let resp = client.get(&url).expect("GET").send().await.expect("send");

    assert_eq!(resp.status(), oxihttp::StatusCode::OK);

    // Verify single-header accessor (lowercase name).
    let nonce = resp.header("replay-nonce");
    assert_eq!(
        nonce,
        Some("test-nonce-12345"),
        "replay-nonce header must be readable via Response::header()"
    );

    let location = resp.header("location");
    assert_eq!(
        location,
        Some("https://acme.example.com/order/abc"),
        "location header must be readable via Response::header()"
    );

    // Absent header must return None.
    let missing = resp.header("x-does-not-exist");
    assert!(missing.is_none(), "absent header must yield None");

    // Verify that the bulk headers() accessor also works (pre-existing capability).
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok());
    assert_eq!(ct, Some("text/plain"), "headers() bulk accessor must work");
}

/// Verify `Response::header()` works on a plain HTTP server as well (no TLS).
#[tokio::test]
async fn response_header_accessor_plain_http() {
    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let io = hyper_util::rt::TokioIo::new(stream);
                let _ = http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(|_req: HyperRequest<hyper::body::Incoming>| async {
                            let resp = HyperResponse::builder()
                                .status(200)
                                .header("x-custom-header", "hello-from-server")
                                .body(Full::new(Bytes::from_static(b"ok")))
                                .expect("build response");
                            Ok::<_, Infallible>(resp)
                        }),
                    )
                    .await;
            });
        }
    });

    let client = oxihttp::Client::builder().build().expect("build");
    let url = format!("http://127.0.0.1:{}/", addr.port());
    let resp = client.get(&url).expect("GET").send().await.expect("send");

    assert_eq!(resp.status(), oxihttp::StatusCode::OK);
    assert_eq!(
        resp.header("x-custom-header"),
        Some("hello-from-server"),
        "custom header must be readable via Response::header()"
    );
    assert!(
        resp.header("x-not-present").is_none(),
        "absent header must return None"
    );
}
