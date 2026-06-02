//! Integration tests for TLS 1.3 0-RTT early-data support in the HTTP client.
//!
//! Verifies that:
//! 1. A client built with `with_early_data()` can complete a TLS handshake and
//!    receive a successful response (early data is offered but silently
//!    downgraded to a full handshake when no session ticket is cached, which
//!    is the correct RFC 8446 behaviour).
//! 2. `ClientBuilder::with_early_data()` does not break normal HTTPS requests.
//!
//! Uses a loopback TLS server backed by oxihttp's `Server`/`Router`.
//! Cert generation uses `oxitls::rcgen_bridge`.

#![cfg(all(feature = "tls", feature = "server", feature = "client"))]

use oxihttp::Router;
use oxihttp::Server;
use oxihttp_server::TlsConfig;
use oxitls::rcgen_bridge::{generate_self_signed_ed25519, CertifiedKey};

// ---------------------------------------------------------------------------
// Certificate helpers
// ---------------------------------------------------------------------------

fn make_localhost_cert() -> CertifiedKey {
    generate_self_signed_ed25519(&["localhost"]).expect("cert gen")
}

// ---------------------------------------------------------------------------
// Test: early_data client option connects successfully
// ---------------------------------------------------------------------------

/// A client configured with `with_early_data()` must successfully complete a
/// TLS 1.3 handshake and receive a 200 OK response.
///
/// On a first connection (no cached session ticket) the early-data extension
/// is offered by the client but the server ignores it and performs a full
/// handshake — this is safe and correct per RFC 8446 §8.  The request
/// completes normally in all cases.
#[tokio::test]
async fn early_data_client_option_connects_successfully() {
    let ck = make_localhost_cert();

    let tls_cfg = TlsConfig::from_pem(ck.cert_pem.as_bytes(), ck.key_pem().as_bytes())
        .expect("server TlsConfig");

    let router = Router::new().get("/ping", |_req| async {
        oxihttp::response::text_response("pong")
    });

    let (addr, server_handle) = Server::bind("127.0.0.1:0")
        .with_tls(tls_cfg)
        .serve_with_addr(router)
        .await
        .expect("server bind");

    // Build a TLS-capable client with early data enabled, trusting the
    // self-signed certificate.
    let client = oxihttp::Client::builder()
        .with_trusted_cert_der(ck.cert_der.clone())
        .with_early_data()
        .build_https()
        .expect("build_https with early_data");

    let url = format!("https://localhost:{}/ping", addr.port());
    let resp = client.get(&url).expect("GET").send().await.expect("send");
    assert_eq!(
        resp.status(),
        oxihttp::StatusCode::OK,
        "expected 200 OK from /ping endpoint"
    );

    server_handle.abort();
}

// ---------------------------------------------------------------------------
// Test: early_data flag is idempotent (calling twice is fine)
// ---------------------------------------------------------------------------

/// Calling `with_early_data()` more than once on a builder must not cause
/// any error or change in observable behaviour — the flag is idempotent.
#[tokio::test]
async fn early_data_flag_is_idempotent() {
    let ck = make_localhost_cert();

    let tls_cfg = TlsConfig::from_pem(ck.cert_pem.as_bytes(), ck.key_pem().as_bytes())
        .expect("server TlsConfig");

    let router = Router::new().get("/ping", |_req| async {
        oxihttp::response::text_response("pong")
    });

    let (addr, server_handle) = Server::bind("127.0.0.1:0")
        .with_tls(tls_cfg)
        .serve_with_addr(router)
        .await
        .expect("server bind");

    // Call with_early_data() twice — must still work.
    let client = oxihttp::Client::builder()
        .with_trusted_cert_der(ck.cert_der.clone())
        .with_early_data()
        .with_early_data()
        .build_https()
        .expect("build_https with early_data (x2)");

    let url = format!("https://localhost:{}/ping", addr.port());
    let resp = client.get(&url).expect("GET").send().await.expect("send");
    assert_eq!(resp.status(), oxihttp::StatusCode::OK);

    server_handle.abort();
}
