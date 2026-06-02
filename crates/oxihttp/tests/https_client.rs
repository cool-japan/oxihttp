#![cfg(feature = "tls")]
//! Integration tests for the HTTPS client (TLS M2).
//!
//! Uses oxitls-rcgen to generate self-signed certificates for localhost and
//! spins up a minimal TLS server backed by hyper + tokio-rustls.

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::Request as HyperRequest;
use hyper::Response as HyperResponse;
use oxitls::rcgen_bridge::{generate_self_signed_ed25519, CertifiedKey};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

// ---------------------------------------------------------------------------
// Test TLS server helpers
// ---------------------------------------------------------------------------

/// Generate a self-signed Ed25519 certificate/key pair for `localhost`.
fn make_localhost_cert() -> CertifiedKey {
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

/// Handle a single TLS-over-HTTP/1.1 connection with a simple dispatcher.
async fn handle_request(
    req: HyperRequest<hyper::body::Incoming>,
) -> Result<HyperResponse<Full<Bytes>>, Infallible> {
    use http_body_util::BodyExt;
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    match (method, path.as_str()) {
        (hyper::Method::GET, "/hello") => {
            Ok(HyperResponse::new(Full::new(Bytes::from("hello tls"))))
        }
        (hyper::Method::POST, "/echo") => {
            let body = req.into_body().collect().await.expect("collect").to_bytes();
            Ok(HyperResponse::new(Full::new(body)))
        }
        (hyper::Method::GET, "/alpn") => {
            // Just confirm we got here; real ALPN negotiation is tested via connector.
            Ok(HyperResponse::new(Full::new(Bytes::from("alpn-ok"))))
        }
        _ => {
            let mut resp = HyperResponse::new(Full::new(Bytes::from("not found")));
            *resp.status_mut() = hyper::StatusCode::NOT_FOUND;
            Ok(resp)
        }
    }
}

/// Spawn a TLS server that accepts connections, serves them, then exits.
///
/// Returns the bound `SocketAddr`.
async fn spawn_tls_server(acceptor: TlsAcceptor) -> SocketAddr {
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
                    .serve_connection(io, service_fn(handle_request))
                    .await;
            });
        }
    });
    addr
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_https_get() {
    let ck = make_localhost_cert();
    let acceptor = make_acceptor(&ck);
    let addr = spawn_tls_server(acceptor).await;

    let client = oxihttp::Client::builder()
        .with_trusted_cert_der(ck.cert_der.clone())
        .build_https()
        .expect("build_https");

    let url = format!("https://localhost:{}/hello", addr.port());
    let resp = client.get(&url).expect("GET").send().await.expect("send");
    assert_eq!(resp.status(), oxihttp::StatusCode::OK);
    let body = resp.body_text().await.expect("body text");
    assert_eq!(body, "hello tls");
}

#[tokio::test]
async fn test_https_post_echo() {
    let ck = make_localhost_cert();
    let acceptor = make_acceptor(&ck);
    let addr = spawn_tls_server(acceptor).await;

    let client = oxihttp::Client::builder()
        .with_trusted_cert_der(ck.cert_der.clone())
        .build_https()
        .expect("build_https");

    let url = format!("https://localhost:{}/echo", addr.port());
    let payload = b"ping over tls";
    let resp = client
        .post(&url)
        .expect("POST")
        .body(Bytes::from_static(payload))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), oxihttp::StatusCode::OK);
    let body = resp.body_bytes().await.expect("body bytes");
    assert_eq!(body.as_ref(), payload);
}

#[tokio::test]
async fn test_https_invalid_cert_rejected() {
    let ck = make_localhost_cert();
    let acceptor = make_acceptor(&ck);
    let addr = spawn_tls_server(acceptor).await;

    // Build a client that trusts the Mozilla CA bundle (via webpki-roots) but
    // does NOT add the self-signed server cert.  The builder succeeds (roots are
    // configured), but the TLS handshake must fail because the self-signed cert
    // is not in any trusted root.
    let client = oxihttp::Client::builder()
        .with_webpki_roots()
        .build_https()
        .expect("build_https should succeed when webpki roots are configured");

    let url = format!("https://localhost:{}/hello", addr.port());
    let result = client.get(&url).expect("GET").send().await;
    assert!(result.is_err(), "untrusted self-signed cert should fail");
    let err = result.expect_err("tls/connect error");
    // The TLS error surfaces either as OxiHttpError::Tls (from the connector
    // future) or as OxiHttpError::Hyper("client error (Connect)") when the
    // hyper legacy client wraps the connector error.  Both indicate that the
    // connection was rejected due to cert verification failure.
    let is_tls_like = matches!(err, oxihttp::OxiHttpError::Tls(_))
        || matches!(&err, oxihttp::OxiHttpError::Hyper(msg) if msg.contains("Connect"));
    assert!(is_tls_like, "expected TLS-related error, got: {err:?}");
}

#[tokio::test]
async fn test_https_danger_accept_invalid() {
    let ck = make_localhost_cert();
    let acceptor = make_acceptor(&ck);
    let addr = spawn_tls_server(acceptor).await;

    // accept_invalid_certs=true → succeed even without trusting the self-signed cert
    let client = oxihttp::Client::builder()
        .with_danger_accept_invalid_certs()
        .build_https()
        .expect("build_https");

    let url = format!("https://localhost:{}/hello", addr.port());
    let resp = client.get(&url).expect("GET").send().await.expect("send");
    assert_eq!(resp.status(), oxihttp::StatusCode::OK);
    let body = resp.body_text().await.expect("body text");
    assert_eq!(body, "hello tls");
}

#[tokio::test]
async fn test_https_webpki_roots_builds() {
    // Just verify that building a webpki-rooted client succeeds without error.
    // No network call is made.
    let result = oxihttp::Client::builder().with_webpki_roots().build_https();
    assert!(
        result.is_ok(),
        "webpki roots client should build: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_http_uri_with_tls_client() {
    // A TLS-capable client should also serve plain http:// URIs via the Http variant.
    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper::Request as HyperRequest;
    use hyper::Response as HyperResponse;
    use std::convert::Infallible;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
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
                                "plain ok",
                            ))))
                        }),
                    )
                    .await;
            });
        }
    });

    // Use a TLS-capable client to make a plain http:// request
    let client = oxihttp::Client::builder()
        .with_danger_accept_invalid_certs()
        .build_https()
        .expect("build_https");

    let url = format!("http://{addr}/");
    let resp = client.get(&url).expect("GET").send().await.expect("send");
    assert_eq!(resp.status(), oxihttp::StatusCode::OK);
    let body = resp.body_text().await.expect("body text");
    assert_eq!(body, "plain ok");
}
