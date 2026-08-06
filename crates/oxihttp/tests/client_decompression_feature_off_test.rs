//! Regression tests for the "with_decompression(true) is a silent no-op
//! when the `decompression` feature is off" fix.
//!
//! Before the fix:
//!   - a client built *without* the `decompression` Cargo feature still
//!     unconditionally advertised `Accept-Encoding: gzip, deflate` whenever
//!     `ClientBuilder::with_decompression(true)` was set (the header
//!     injection was gated on the `decompression: bool` field, not on
//!     whether this build actually has a decoder), and
//!   - if a server complied and returned a compressed body anyway,
//!     `Response::body_bytes()` silently handed back the still-compressed
//!     wire bytes as if they were plaintext (the `#[cfg(not(feature =
//!     "decompression"))]` arms were empty, falling through to `Ok(raw)`),
//!     which downstream surfaced as a confusing UTF-8/JSON parse failure or
//!     worse — garbage silently written to disk.
//!
//! This file exercises the fix from the *default-feature* build (no
//! `decompression`), so it is gated to only compile/run in that
//! configuration — see `client_decompression_bomb_test.rs` for the
//! `decompression`-feature-**on** counterpart (bounded decoding, decompression
//! bomb protection).
//!
//! Run with:
//!   cargo test -p oxihttp --test client_decompression_feature_off_test

#![cfg(all(feature = "client", not(feature = "decompression")))]

use bytes::Bytes;
use http::StatusCode;
use http_body_util::Full;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

/// Spawn a raw hyper/tokio HTTP server that always replies with a fixed
/// body plus an optional `Content-Encoding` header, and records (via the
/// returned `AtomicBool`) whether any received request carried an
/// `Accept-Encoding` header.
async fn spawn_recording_server(
    body: Vec<u8>,
    content_encoding: Option<&'static str>,
) -> (
    SocketAddr,
    Arc<AtomicBool>,
    tokio::sync::oneshot::Sender<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let body = Arc::new(body);
    let saw_accept_encoding = Arc::new(AtomicBool::new(false));
    let saw_accept_encoding_for_server = Arc::clone(&saw_accept_encoding);

    tokio::spawn(async move {
        tokio::select! {
            _ = rx => {}
            _ = async {
                loop {
                    let Ok((stream, _)) = listener.accept().await else { break };
                    let body = Arc::clone(&body);
                    let saw_ae = Arc::clone(&saw_accept_encoding_for_server);

                    tokio::spawn(async move {
                        let io = hyper_util::rt::TokioIo::new(stream);
                        let svc = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                            let body = Arc::clone(&body);
                            let saw_ae = Arc::clone(&saw_ae);
                            async move {
                                if req.headers().contains_key(http::header::ACCEPT_ENCODING) {
                                    saw_ae.store(true, Ordering::SeqCst);
                                }
                                let mut builder = hyper::Response::builder().status(StatusCode::OK);
                                if let Some(ce) = content_encoding {
                                    builder = builder.header("content-encoding", ce);
                                }
                                let resp = builder
                                    .body(Full::new(Bytes::from((*body).clone())))
                                    .expect("build response");
                                Ok::<_, Infallible>(resp)
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

    (addr, saw_accept_encoding, tx)
}

/// Without the `decompression` feature, a client built with
/// `with_decompression(true)` must NOT advertise
/// `Accept-Encoding: gzip, deflate` — this build cannot decode a compressed
/// response, so inviting one from the server serves no purpose.
#[tokio::test]
async fn accept_encoding_is_not_sent_without_decompression_feature() {
    let (addr, saw_accept_encoding, _shutdown) =
        spawn_recording_server(b"plain body".to_vec(), None).await;

    let client = oxihttp::Client::builder()
        .with_decompression(true)
        .build()
        .expect("client build");

    let resp = client
        .get(&format!("http://{addr}/"))
        .expect("GET builder")
        .send()
        .await
        .expect("GET send");
    assert_eq!(resp.status(), StatusCode::OK);
    let _ = resp
        .body_bytes()
        .await
        .expect("plain, unencoded body must round-trip");

    assert!(
        !saw_accept_encoding.load(Ordering::SeqCst),
        "client must not advertise Accept-Encoding when it cannot decode the response"
    );
}

/// If a server sends a `Content-Encoding: gzip` response anyway (e.g. a
/// misconfigured or unusual server that compresses regardless of
/// `Accept-Encoding`), a client without the `decompression` feature must
/// reject it with a typed error rather than silently handing back the
/// still-compressed bytes as if they were plaintext.
#[tokio::test]
async fn gzip_content_encoding_is_a_typed_error_without_decompression_feature() {
    // The exact bytes don't matter: the feature-off path rejects based on
    // the Content-Encoding header alone, before ever attempting to decode.
    let garbage_body = vec![0x1fu8, 0x8b, 0x08, 0x00, 0xAA, 0xBB, 0xCC];
    let (addr, _saw_ae, _shutdown) = spawn_recording_server(garbage_body, Some("gzip")).await;

    let client = oxihttp::Client::builder()
        .with_decompression(true)
        .build()
        .expect("client build");

    let resp = client
        .get(&format!("http://{addr}/"))
        .expect("GET builder")
        .send()
        .await
        .expect("GET send");
    assert_eq!(resp.status(), StatusCode::OK);

    let err = resp
        .body_bytes()
        .await
        .expect_err("gzip Content-Encoding without the decompression feature must error");
    assert!(
        err.to_string().contains("decompression"),
        "unexpected error: {err}"
    );
}

/// Same as above but for `deflate`.
#[tokio::test]
async fn deflate_content_encoding_is_a_typed_error_without_decompression_feature() {
    let garbage_body = vec![0x78u8, 0x9c, 0x01, 0x02, 0x03];
    let (addr, _saw_ae, _shutdown) = spawn_recording_server(garbage_body, Some("deflate")).await;

    let client = oxihttp::Client::builder()
        .with_decompression(true)
        .build()
        .expect("client build");

    let resp = client
        .get(&format!("http://{addr}/"))
        .expect("GET builder")
        .send()
        .await
        .expect("GET send");

    let err = resp
        .body_bytes()
        .await
        .expect_err("deflate Content-Encoding without the decompression feature must error");
    assert!(
        err.to_string().contains("decompression"),
        "unexpected error: {err}"
    );
}

/// A response with `Content-Encoding: identity` (explicitly "not encoded",
/// RFC 9110 §8.4.1) must still round-trip normally even without the
/// `decompression` feature — the fix must not over-reject encodings that
/// need no decoding.
#[tokio::test]
async fn identity_content_encoding_round_trips_without_decompression_feature() {
    let body = b"identity is not an encoding to strip".to_vec();
    let (addr, _saw_ae, _shutdown) = spawn_recording_server(body.clone(), Some("identity")).await;

    let client = oxihttp::Client::builder()
        .with_decompression(true)
        .build()
        .expect("client build");

    let resp = client
        .get(&format!("http://{addr}/"))
        .expect("GET builder")
        .send()
        .await
        .expect("GET send");

    let got = resp
        .body_bytes()
        .await
        .expect("identity-encoded body must round-trip");
    assert_eq!(got.as_ref(), body.as_slice());
}

/// A response with no `Content-Encoding` header at all — the overwhelmingly
/// common case — must always round-trip regardless of the `decompression`
/// feature or the `with_decompression` setting.
#[tokio::test]
async fn no_content_encoding_round_trips_without_decompression_feature() {
    let body = b"no encoding here".to_vec();
    let (addr, _saw_ae, _shutdown) = spawn_recording_server(body.clone(), None).await;

    let client = oxihttp::Client::builder()
        .with_decompression(true)
        .build()
        .expect("client build");

    let resp = client
        .get(&format!("http://{addr}/"))
        .expect("GET builder")
        .send()
        .await
        .expect("GET send");

    let got = resp
        .body_bytes()
        .await
        .expect("unencoded body must round-trip");
    assert_eq!(got.as_ref(), body.as_slice());
}
