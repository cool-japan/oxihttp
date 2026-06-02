#![cfg(feature = "static-files")]
//! Integration tests for `ServeFile`.

use http::{HeaderMap, Method};
use http_body_util::BodyExt as _;
use oxihttp_server::ServeFile;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Write `content` to a uniquely-named file in `temp_dir()` and return the path.
fn write_temp_file(suffix: &str, content: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("oxihttp_serve_file_test_{suffix}"));
    std::fs::write(&path, content).expect("write temp file");
    path
}

/// Collect all body bytes from a `Body`.
async fn collect_body(body: oxihttp_core::Body) -> bytes::Bytes {
    body.into_pinned()
        .collect()
        .await
        .expect("collect body")
        .to_bytes()
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_serve_file_get_200() {
    let path = write_temp_file("get.txt", b"Hello, ServeFile!");

    let sf = ServeFile::new(&path);
    let headers = HeaderMap::new();
    let resp = sf.serve(&Method::GET, &headers).await.expect("serve");

    assert_eq!(resp.status(), http::StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("text"), "expected text MIME, got: {ct}");

    let body_bytes = collect_body(resp.into_body()).await;
    assert_eq!(&body_bytes[..], b"Hello, ServeFile!");

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn test_serve_file_etag_304() {
    let path = write_temp_file("etag.bin", b"ETag test content");

    let sf = ServeFile::new(&path);

    // First request to get the ETag.
    let resp = sf
        .serve(&Method::GET, &HeaderMap::new())
        .await
        .expect("first serve");
    let etag = resp
        .headers()
        .get("etag")
        .cloned()
        .expect("etag header present");

    // Second request with matching If-None-Match.
    let mut headers = HeaderMap::new();
    headers.insert("if-none-match", etag);
    let resp2 = sf
        .serve(&Method::GET, &headers)
        .await
        .expect("second serve");
    assert_eq!(
        resp2.status(),
        http::StatusCode::NOT_MODIFIED,
        "matching ETag should return 304"
    );

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn test_serve_file_range_206() {
    let path = write_temp_file("range.bin", b"0123456789abcdef");

    let sf = ServeFile::new(&path);

    let mut headers = HeaderMap::new();
    headers.insert("range", http::HeaderValue::from_static("bytes=0-4"));
    let resp = sf.serve(&Method::GET, &headers).await.expect("range serve");

    assert_eq!(
        resp.status(),
        http::StatusCode::PARTIAL_CONTENT,
        "range request should return 206"
    );

    let body_bytes = collect_body(resp.into_body()).await;
    assert_eq!(&body_bytes[..], b"01234", "expected first 5 bytes");

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn test_serve_file_405_for_post() {
    let path = write_temp_file("method.bin", b"some content");

    let sf = ServeFile::new(&path);
    let resp = sf
        .serve(&Method::POST, &HeaderMap::new())
        .await
        .expect("serve");
    assert_eq!(resp.status(), http::StatusCode::METHOD_NOT_ALLOWED);

    let _ = std::fs::remove_file(&path);
}
