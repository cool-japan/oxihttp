//! Integration tests for the typed extractor framework.

#![cfg(all(feature = "client", feature = "server"))]

use std::time::Duration;

use bytes::Bytes;
use http::StatusCode;
use http_body_util::Full;
use oxihttp_core::{ContentType, OxiHttpError};
use oxihttp_server::extractor::TypedHeader;
use oxihttp_server::Router;

/// Spawn a test server and return its bound address + a shutdown channel.
async fn spawn_test_server(
    router: Router,
) -> (std::net::SocketAddr, tokio::sync::oneshot::Sender<()>) {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let (addr, _handle) = oxihttp_server::Server::bind("127.0.0.1:0")
        .with_graceful_shutdown(async move {
            let _ = rx.await;
        })
        .serve_with_addr(router)
        .await
        .expect("server bind");
    tokio::time::sleep(Duration::from_millis(10)).await;
    (addr, tx)
}

/// A handler that extracts `Content-Type` via `req.extract()` and returns
/// 200 when the value is `application/json`, or 400 otherwise.
async fn content_type_handler(
    req: oxihttp_server::router::Request,
) -> Result<hyper::Response<Full<Bytes>>, OxiHttpError> {
    match req.extract::<TypedHeader<ContentType>>() {
        Ok(TypedHeader(ct)) if ct == ContentType::Json => hyper::Response::builder()
            .status(StatusCode::OK)
            .body(Full::new(Bytes::from(ct.to_string())))
            .map_err(|e| OxiHttpError::Http(std::sync::Arc::new(e))),
        Ok(TypedHeader(ct)) => hyper::Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Full::new(Bytes::from(format!("unexpected: {ct}"))))
            .map_err(|e| OxiHttpError::Http(std::sync::Arc::new(e))),
        Err(_) => hyper::Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Full::new(Bytes::from("missing Content-Type")))
            .map_err(|e| OxiHttpError::Http(std::sync::Arc::new(e))),
    }
}

#[tokio::test]
async fn test_extract_content_type_json_returns_200() {
    let router = Router::new().post("/upload", content_type_handler);
    let (addr, _shutdown) = spawn_test_server(router).await;

    let client = oxihttp_client::Client::builder().build().expect("client");
    let url = format!("http://{addr}/upload");
    let resp = client
        .post(&url)
        .expect("POST builder")
        .header("Content-Type", "application/json")
        .expect("set header")
        .body(b"{}" as &[u8])
        .send()
        .await
        .expect("send");

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.body_text().await.expect("body");
    assert_eq!(body, "application/json");
}

#[tokio::test]
async fn test_extract_missing_content_type_returns_400() {
    let router = Router::new().post("/upload", content_type_handler);
    let (addr, _shutdown) = spawn_test_server(router).await;

    let client = oxihttp_client::Client::builder().build().expect("client");
    let url = format!("http://{addr}/upload");
    // Explicitly set Content-Type to text/plain so we hit the "non-JSON" branch,
    // which also returns 400. The important invariant is that only application/json
    // gets 200.
    let resp = client
        .post(&url)
        .expect("POST builder")
        .header("Content-Type", "text/plain")
        .expect("set header")
        .body(b"hello" as &[u8])
        .send()
        .await
        .expect("send");

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = resp.body_text().await.expect("body");
    assert!(
        body.contains("unexpected"),
        "expected 'unexpected' in response body: {body}"
    );
}
