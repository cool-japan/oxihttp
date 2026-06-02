//! Integration tests for per-request TLS config overrides.
//!
//! Tests verify that [`HttpsClient::with_request_tls_config`] correctly
//! overrides the trust store on a per-request (per-client-clone) basis,
//! enabling certificate pinning and per-endpoint trust without rebuilding the
//! entire client configuration.

#![cfg(feature = "tls")]

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::Request as HyperRequest;
use hyper::Response as HyperResponse;
use oxihttp::RequestTlsConfig;
use oxitls::rcgen_bridge::{generate_self_signed_ed25519, CertifiedKey};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

// ---------------------------------------------------------------------------
// TLS server helpers (re-used from https_client.rs pattern)
// ---------------------------------------------------------------------------

/// Generate a self-signed Ed25519 certificate/key pair for `localhost`.
fn make_cert() -> CertifiedKey {
    generate_self_signed_ed25519(&["localhost"]).expect("cert gen")
}

/// Build a `TlsAcceptor` from the given `CertifiedKey`.
fn make_acceptor(ck: &CertifiedKey) -> TlsAcceptor {
    let cert = CertificateDer::from(ck.cert_der.clone());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.pkcs8_der.clone()));
    let server_cfg = oxitls::tls13::ServerBuilder::new()
        .with_der_cert_and_key(vec![cert], key)
        .build()
        .expect("server TLS config");
    TlsAcceptor::from(Arc::new(server_cfg))
}

/// A minimal HTTP handler that just echoes a caller-supplied static string.
async fn echo_handler(
    _req: HyperRequest<hyper::body::Incoming>,
    body: &'static str,
) -> Result<HyperResponse<Full<Bytes>>, Infallible> {
    Ok(HyperResponse::new(Full::new(Bytes::from_static(
        body.as_bytes(),
    ))))
}

/// Spawn a TLS server that serves `body` on every request.
///
/// Returns the bound `SocketAddr`.
async fn spawn_tls_server(acceptor: TlsAcceptor, body: &'static str) -> SocketAddr {
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
                    .serve_connection(io, service_fn(move |req| echo_handler(req, body)))
                    .await;
            });
        }
    });
    addr
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Core scenario: global client trusts cert A; per-request override adds cert B.
///
/// - `GET server_a` with global client  → 200 OK (cert A trusted globally)
/// - `GET server_b` with per-request override (trust B) → 200 OK
/// - `GET server_b` with global client (no override) → TLS error (cert B not trusted)
#[tokio::test]
async fn per_request_trusted_cert_overrides_global_config() {
    // Generate two independent self-signed certs (A and B).
    let ck_a = make_cert();
    let ck_b = make_cert();

    let acceptor_a = make_acceptor(&ck_a);
    let acceptor_b = make_acceptor(&ck_b);

    let addr_a = spawn_tls_server(acceptor_a, "server-a").await;
    let addr_b = spawn_tls_server(acceptor_b, "server-b").await;

    // Global client trusts only cert A.
    let client = oxihttp::Client::builder()
        .with_trusted_cert_der(ck_a.cert_der.clone())
        .build_https()
        .expect("build_https (cert A)");

    // 1. Request to server A with global config → OK.
    let url_a = format!("https://localhost:{}/", addr_a.port());
    let resp = client
        .get(&url_a)
        .expect("GET server_a")
        .send()
        .await
        .expect("send to server_a");
    assert_eq!(
        resp.status(),
        oxihttp::StatusCode::OK,
        "server_a (trusted globally) should return 200"
    );
    let body = resp.body_text().await.expect("body text server_a");
    assert_eq!(body, "server-a");

    // 2. Request to server B with per-request override that trusts cert B → OK.
    let pinned_client = client
        .with_request_tls_config(RequestTlsConfig::new().with_trusted_cert(ck_b.cert_der.clone()))
        .expect("build per-request client for server_b");

    let url_b = format!("https://localhost:{}/", addr_b.port());
    let resp = pinned_client
        .get(&url_b)
        .expect("GET server_b (overridden)")
        .send()
        .await
        .expect("send to server_b with override");
    assert_eq!(
        resp.status(),
        oxihttp::StatusCode::OK,
        "server_b (trusted via override) should return 200"
    );
    let body = resp.body_text().await.expect("body text server_b");
    assert_eq!(body, "server-b");

    // 3. Request to server B with the original (global) client → TLS error.
    let result = client
        .get(&url_b)
        .expect("GET server_b (global)")
        .send()
        .await;
    assert!(
        result.is_err(),
        "server_b cert is not trusted by global client; expected TLS error"
    );
    let err = result.expect_err("expected TLS error");
    let is_tls_like = matches!(err, oxihttp::OxiHttpError::Tls(_))
        || matches!(&err, oxihttp::OxiHttpError::Hyper(msg) if msg.contains("Connect"));
    assert!(is_tls_like, "expected TLS-related error, got: {err:?}");
}

/// Verify that `accept_invalid_certs` in the override disables verification
/// even though the global client does NOT have the server cert in its trust store.
#[tokio::test]
async fn per_request_accept_invalid_bypasses_verification() {
    let ck = make_cert();
    let acceptor = make_acceptor(&ck);
    let addr = spawn_tls_server(acceptor, "bypass-ok").await;

    // Global client trusts only the Mozilla CA bundle — NOT this self-signed cert.
    let client = oxihttp::Client::builder()
        .with_webpki_roots()
        .build_https()
        .expect("build_https (webpki roots)");

    // Without override → TLS error.
    let url = format!("https://localhost:{}/", addr.port());
    let result_no_override = client.get(&url).expect("GET").send().await;
    assert!(
        result_no_override.is_err(),
        "self-signed cert should fail without override"
    );

    // With accept_invalid_certs override → should succeed.
    let bypass_client = client
        .with_request_tls_config(RequestTlsConfig::new().with_accept_invalid_certs())
        .expect("build per-request bypass client");

    let resp = bypass_client
        .get(&url)
        .expect("GET with override")
        .send()
        .await
        .expect("send with accept_invalid_certs override");
    assert_eq!(
        resp.status(),
        oxihttp::StatusCode::OK,
        "accept_invalid_certs override should bypass cert verification"
    );
    let body = resp.body_text().await.expect("body text");
    assert_eq!(body, "bypass-ok");
}

/// Verify that chaining `with_request_tls_config` calls works correctly:
/// an override derived from an already-overridden client should reflect
/// the merged state of the previous override.
#[tokio::test]
async fn per_request_config_is_chainable() {
    let ck_a = make_cert();
    let ck_b = make_cert();

    let acceptor_b = make_acceptor(&ck_b);
    let addr_b = spawn_tls_server(acceptor_b, "chain-ok").await;

    // Start: client trusts cert A only.
    let client_a = oxihttp::Client::builder()
        .with_trusted_cert_der(ck_a.cert_der.clone())
        .build_https()
        .expect("build_https (cert A)");

    // First override: replace trust with cert B.
    let client_b = client_a
        .with_request_tls_config(RequestTlsConfig::new().with_trusted_cert(ck_b.cert_der.clone()))
        .expect("first override");

    // Second (chained) override: same config again — should still trust cert B.
    let client_b2 = client_b
        .with_request_tls_config(RequestTlsConfig::new())
        .expect("chained override (no change)");

    let url_b = format!("https://localhost:{}/", addr_b.port());
    let resp = client_b2
        .get(&url_b)
        .expect("GET")
        .send()
        .await
        .expect("send");
    assert_eq!(
        resp.status(),
        oxihttp::StatusCode::OK,
        "chained override should still trust cert B"
    );
    let body = resp.body_text().await.expect("body text");
    assert_eq!(body, "chain-ok");
}
