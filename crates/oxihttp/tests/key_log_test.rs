//! Integration tests for SSLKEYLOGFILE / TLS key-logging support.
//!
//! Verifies that:
//! 1. A client configured with `with_key_log_file` writes NSS key-log format
//!    lines (prefixed with `CLIENT_RANDOM`) after a successful TLS handshake.
//! 2. A server configured with `from_pem_with_key_log` writes NSS key-log
//!    format lines after a successful TLS handshake.
//!
//! Uses a loopback TLS server backed by oxihttp's `Server`/`Router`.
//! Cert generation uses `oxitls::rcgen_bridge`.

#![cfg(all(feature = "tls", feature = "server", feature = "client"))]

use oxihttp::Router;
use oxihttp::Server;
use oxihttp_server::TlsConfig;
use oxitls::rcgen_bridge::{generate_self_signed_ed25519, CertifiedKey};
use std::fs;

// ---------------------------------------------------------------------------
// Certificate helpers
// ---------------------------------------------------------------------------

fn make_localhost_cert() -> CertifiedKey {
    generate_self_signed_ed25519(&["localhost"]).expect("cert gen")
}

// ---------------------------------------------------------------------------
// Test 1: client-side key logging
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_client_key_log_file_written() {
    let ck = make_localhost_cert();

    // Build server without key logging (plain TLS).
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

    // Choose a temp file path for the key log.
    let key_log_path = std::env::temp_dir().join(format!(
        "oxihttp_client_keylog_{}.log",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    // Clean up any prior run.
    let _ = fs::remove_file(&key_log_path);

    // Build a TLS-capable client that trusts the server cert and logs keys.
    let client = oxihttp::Client::builder()
        .with_trusted_cert_der(ck.cert_der.clone())
        .with_key_log_file(key_log_path.clone())
        .build_https()
        .expect("build_https");

    let url = format!("https://localhost:{}/ping", addr.port());
    let resp = client.get(&url).expect("GET").send().await.expect("send");
    assert_eq!(resp.status(), oxihttp::StatusCode::OK);

    // Give the key-logger a moment to flush.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Verify the key-log file was created and contains at least one CLIENT_RANDOM line.
    let contents = fs::read_to_string(&key_log_path)
        .expect("key-log file should have been created by the TLS handshake");

    // TLS 1.3 uses traffic-secret labels; TLS 1.2 uses CLIENT_RANDOM.
    // Accept either so the test is robust across protocol versions.
    let has_tls13_labels = contents.contains("CLIENT_HANDSHAKE_TRAFFIC_SECRET")
        || contents.contains("CLIENT_TRAFFIC_SECRET_0");
    let has_tls12_label = contents.contains("CLIENT_RANDOM");
    assert!(
        has_tls13_labels || has_tls12_label,
        "key-log file should contain NSS key-log entries (CLIENT_HANDSHAKE_TRAFFIC_SECRET \
         or CLIENT_RANDOM), got:\n{contents}"
    );

    // Verify the format: each non-empty line should be `<LABEL> <hex> <hex>`.
    for line in contents.lines().filter(|l| !l.is_empty()) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(
            parts.len(),
            3,
            "expected 3 whitespace-separated fields per key-log line, got: {line:?}"
        );
    }

    // Clean up.
    let _ = fs::remove_file(&key_log_path);
    server_handle.abort();
}

// ---------------------------------------------------------------------------
// Test 2: server-side key logging
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_server_key_log_file_written() {
    let ck = make_localhost_cert();

    // Choose a temp file path for the server key log.
    let key_log_path = std::env::temp_dir().join(format!(
        "oxihttp_server_keylog_{}.log",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_file(&key_log_path);

    // Build server with key logging enabled.
    let tls_cfg = TlsConfig::from_pem_with_key_log(
        ck.cert_pem.as_bytes(),
        ck.key_pem().as_bytes(),
        key_log_path.clone(),
    )
    .expect("server TlsConfig with key log");

    let router = Router::new().get("/ping", |_req| async {
        oxihttp::response::text_response("pong")
    });

    let (addr, server_handle) = Server::bind("127.0.0.1:0")
        .with_tls(tls_cfg)
        .serve_with_addr(router)
        .await
        .expect("server bind");

    // Build a plain TLS-capable client (no key logging on client side).
    let client = oxihttp::Client::builder()
        .with_trusted_cert_der(ck.cert_der.clone())
        .build_https()
        .expect("build_https");

    let url = format!("https://localhost:{}/ping", addr.port());
    let resp = client.get(&url).expect("GET").send().await.expect("send");
    assert_eq!(resp.status(), oxihttp::StatusCode::OK);

    // Give the key-logger a moment to flush.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Verify the server-side key-log file was written.
    let contents = fs::read_to_string(&key_log_path)
        .expect("server key-log file should have been created by the TLS handshake");

    // TLS 1.3 uses traffic-secret labels; TLS 1.2 uses CLIENT_RANDOM.
    let has_tls13_labels = contents.contains("CLIENT_HANDSHAKE_TRAFFIC_SECRET")
        || contents.contains("CLIENT_TRAFFIC_SECRET_0");
    let has_tls12_label = contents.contains("CLIENT_RANDOM");
    assert!(
        has_tls13_labels || has_tls12_label,
        "server key-log file should contain NSS key-log entries, got:\n{contents}"
    );

    // Clean up.
    let _ = fs::remove_file(&key_log_path);
    server_handle.abort();
}
