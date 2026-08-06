//! Regression tests for the client-side response-body cap and bounded
//! decompression fix (S-severity: uncapped response body + decompression
//! bomb).
//!
//! Before the fix, `Response::body_bytes()` collected the wire body with no
//! size limit and, when `with_decompression(true)` was enabled, fed it to
//! one-shot decompressors (`oxiarc_deflate::gzip_decompress` /
//! `zlib_decompress`) that decode into an unbounded, internally growing
//! buffer. A malicious or compromised server could therefore force the
//! client to allocate an arbitrary amount of memory either directly (an
//! oversized response) or via a small, highly-compressible payload (a
//! "decompression bomb").
//!
//! Run with:
//!   cargo test -p oxihttp --features decompression --test client_decompression_bomb_test

#![cfg(feature = "decompression")]

use bytes::Bytes;
use http::StatusCode;
use http_body_util::Full;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

/// Spawn a raw hyper/tokio HTTP server that replies to every request with a
/// fixed, pre-built response: the given body bytes plus an optional
/// `Content-Encoding` header. Unlike `compression_test.rs`'s
/// `spawn_compression_server`, this does not run the server-side
/// `Compression` middleware — the caller supplies already-compressed (or
/// deliberately adversarial) bytes directly, so these tests can construct
/// payloads a legitimate compressor would never produce.
async fn spawn_fixed_response_server(
    body: Vec<u8>,
    content_encoding: Option<&'static str>,
) -> (SocketAddr, tokio::sync::oneshot::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let body = Arc::new(body);

    tokio::spawn(async move {
        tokio::select! {
            _ = rx => {}
            _ = async {
                loop {
                    let Ok((stream, _)) = listener.accept().await else { break };
                    let body = Arc::clone(&body);

                    tokio::spawn(async move {
                        let io = hyper_util::rt::TokioIo::new(stream);
                        let svc = hyper::service::service_fn(move |_req: hyper::Request<hyper::body::Incoming>| {
                            let body = Arc::clone(&body);
                            async move {
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

    (addr, tx)
}

/// A response with no `Content-Encoding` larger than the configured cap
/// must be rejected with a typed error, not buffered without bound.
#[tokio::test]
async fn oversized_uncompressed_body_is_rejected() {
    let big_body = vec![b'x'; 200_000];
    let (addr, _shutdown) = spawn_fixed_response_server(big_body, None).await;

    let client = oxihttp::Client::builder()
        .with_max_response_body(1024)
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
        .expect_err("oversized body must be rejected, not buffered");
    assert!(
        err.to_string().contains("too large"),
        "unexpected error: {err}"
    );
}

/// A response within the configured cap must still round-trip correctly —
/// the cap must not corrupt or truncate legitimate small responses.
#[tokio::test]
async fn body_within_cap_is_returned_unchanged() {
    let body = b"hello, bounded world".to_vec();
    let (addr, _shutdown) = spawn_fixed_response_server(body.clone(), None).await;

    let client = oxihttp::Client::builder()
        .with_max_response_body(1024)
        .build()
        .expect("client build");

    let resp = client
        .get(&format!("http://{addr}/"))
        .expect("GET builder")
        .send()
        .await
        .expect("GET send");

    let got = resp.body_bytes().await.expect("body within cap");
    assert_eq!(got.as_ref(), body.as_slice());
}

/// A gzip "decompression bomb" — a small, highly-compressible payload whose
/// decompressed size exceeds the configured cap — must be rejected with a
/// typed error rather than handed back to the caller as an oversized
/// buffer.
#[tokio::test]
async fn gzip_decompression_bomb_is_rejected() {
    let plaintext = "A".repeat(200_000);
    let compressed = oxiarc_deflate::gzip_compress(plaintext.as_bytes(), 9).expect("gzip_compress");
    // Sanity: this really is a "bomb" shape — tiny on the wire, huge decoded.
    assert!(
        compressed.len() < 2_000,
        "fixture is not actually highly compressed: {} bytes",
        compressed.len()
    );

    let (addr, _shutdown) = spawn_fixed_response_server(compressed, Some("gzip")).await;

    let client = oxihttp::Client::builder()
        .with_decompression(true)
        .with_max_response_body(4096)
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
        .expect_err("decompression bomb must be rejected, not returned as a huge buffer");
    // The cap is tight enough here that even the first (heuristic-sized)
    // `bounded_inflate` attempt already equals `cap`, so `inflate_into`
    // itself reports the failure (its own "buffer too small" error,
    // surfaced through the "gzip decompression error: " wrapper) rather
    // than the separate multi-member/ISIZE mismatch message. Either way
    // the key property holds: a typed `Err`, never a >cap buffer.
    assert!(
        err.to_string().contains("gzip decompression error"),
        "unexpected error: {err}"
    );
}

/// The gzip path above bounds decompression via `inflate_into` writing
/// directly into a capped buffer; when the payload genuinely fits within
/// the cap it must still decode to the exact original plaintext.
#[tokio::test]
async fn gzip_decompression_within_cap_round_trips() {
    let plaintext = "Hello, bounded gzip decompression! ".repeat(50);
    let compressed = oxiarc_deflate::gzip_compress(plaintext.as_bytes(), 6).expect("gzip_compress");

    let (addr, _shutdown) = spawn_fixed_response_server(compressed, Some("gzip")).await;

    let client = oxihttp::Client::builder()
        .with_decompression(true)
        .with_max_response_body(1024 * 1024)
        .build()
        .expect("client build");

    let resp = client
        .get(&format!("http://{addr}/"))
        .expect("GET builder")
        .send()
        .await
        .expect("GET send");

    let text = resp.body_text().await.expect("body_text");
    assert_eq!(text, plaintext);
}

/// A concatenated (multi-member) gzip stream must be rejected rather than
/// silently decoded as if only the first member were the whole body: the
/// bounded gzip path cannot tell how many compressed bytes the first
/// member consumed, so it verifies the decoded length against the
/// trailing ISIZE field and refuses a mismatch instead of returning
/// truncated data.
#[tokio::test]
async fn multi_member_gzip_is_rejected_not_truncated() {
    let member_a = oxiarc_deflate::gzip_compress(b"first-member", 6).expect("gzip_compress a");
    let member_b = oxiarc_deflate::gzip_compress(b"second-member", 6).expect("gzip_compress b");
    let mut concatenated = member_a;
    concatenated.extend_from_slice(&member_b);

    let (addr, _shutdown) = spawn_fixed_response_server(concatenated, Some("gzip")).await;

    let client = oxihttp::Client::builder()
        .with_decompression(true)
        .with_max_response_body(1024 * 1024)
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
        .expect_err("multi-member gzip must be rejected, not silently truncated");
    assert!(
        err.to_string().contains("multi-member"),
        "unexpected error: {err}"
    );
}

/// The zlib/deflate counterpart of `gzip_decompression_bomb_is_rejected`:
/// `zlib_decompress_into` writes directly into a capped buffer and must
/// refuse rather than over-allocate.
#[tokio::test]
async fn deflate_decompression_bomb_is_rejected() {
    let plaintext = "B".repeat(200_000);
    let compressed = oxiarc_deflate::zlib_compress(plaintext.as_bytes(), 9).expect("zlib_compress");
    assert!(
        compressed.len() < 2_000,
        "fixture is not actually highly compressed: {} bytes",
        compressed.len()
    );

    let (addr, _shutdown) = spawn_fixed_response_server(compressed, Some("deflate")).await;

    let client = oxihttp::Client::builder()
        .with_decompression(true)
        .with_max_response_body(4096)
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
        .expect_err("decompression bomb must be rejected, not returned as a huge buffer");
    // `bounded_deflate_decompress` tries `zlib_decompress_into` first, then
    // falls back to `inflate_into` on *any* failure (mirroring the
    // pre-existing unbounded fallback semantics, which also retried
    // unconditionally) — so a too-small buffer can surface either as
    // `inflate_into`'s own "buffer too small" error or, if the fallback
    // misinterprets the still-zlib-wrapped bytes as headerless raw
    // DEFLATE, as a stream-corruption error from that second attempt.
    // Both are wrapped under the same prefix; either way the key property
    // holds: a typed `Err`, never a >cap buffer handed back to the caller.
    assert!(
        err.to_string().contains("deflate decompression error"),
        "unexpected error: {err}"
    );
}

/// Deflate counterpart of `gzip_decompression_within_cap_round_trips`.
#[tokio::test]
async fn deflate_decompression_within_cap_round_trips() {
    let plaintext = "Hello, bounded deflate decompression! ".repeat(50);
    let compressed = oxiarc_deflate::zlib_compress(plaintext.as_bytes(), 6).expect("zlib_compress");

    let (addr, _shutdown) = spawn_fixed_response_server(compressed, Some("deflate")).await;

    let client = oxihttp::Client::builder()
        .with_decompression(true)
        .with_max_response_body(1024 * 1024)
        .build()
        .expect("client build");

    let resp = client
        .get(&format!("http://{addr}/"))
        .expect("GET builder")
        .send()
        .await
        .expect("GET send");

    let text = resp.body_text().await.expect("body_text");
    assert_eq!(text, plaintext);
}

/// Per-request override: `RequestBuilder::max_response_body` takes effect
/// even when the client-level default would have allowed the body through.
#[tokio::test]
async fn per_request_max_response_body_override_is_honored() {
    let big_body = vec![b'y'; 10_000];
    let (addr, _shutdown) = spawn_fixed_response_server(big_body, None).await;

    // Client default cap is generous...
    let client = oxihttp::Client::builder()
        .with_max_response_body(1024 * 1024)
        .build()
        .expect("client build");

    // ...but this request tightens it below the actual body size.
    let resp = client
        .get(&format!("http://{addr}/"))
        .expect("GET builder")
        .max_response_body(100)
        .send()
        .await
        .expect("GET send");

    let err = resp
        .body_bytes()
        .await
        .expect_err("per-request cap must be honored");
    assert!(
        err.to_string().contains("too large"),
        "unexpected error: {err}"
    );
}
