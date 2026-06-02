//! Facade integration tests for CORS and timeout middleware.
//!
//! facade:37 — CORS + timeout middleware via the oxihttp facade API.

use std::time::Duration;

// ---------------------------------------------------------------------------
// CORS middleware facade test
// ---------------------------------------------------------------------------

/// Test that CORS middleware injects `Access-Control-Allow-Origin` header.
#[tokio::test]
async fn test_cors_header_present_on_get() {
    let cors = oxihttp::CorsConfig::permissive();

    let router = oxihttp::Router::new().get("/api/data", |_req| async {
        oxihttp::response::text_response("data")
    });

    let (addr, _handle) = oxihttp::Server::bind("127.0.0.1:0")
        .with_cors(cors)
        .serve_with_addr(router)
        .await
        .expect("server bind");

    tokio::time::sleep(Duration::from_millis(5)).await;

    let client = oxihttp_client::Client::builder().build().expect("client");
    let url = format!("http://{addr}/api/data");

    let req = http::Request::builder()
        .method(http::Method::GET)
        .uri(&url)
        .header("origin", "https://example.com")
        .body(http_body_util::Full::new(bytes::Bytes::new()))
        .expect("request build");

    let resp = client.execute(req).await.expect("execute");

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert!(
        resp.headers()
            .contains_key(http::header::ACCESS_CONTROL_ALLOW_ORIGIN),
        "CORS header ACCESS_CONTROL_ALLOW_ORIGIN should be present"
    );

    let body = resp.body_text().await.expect("body text");
    assert_eq!(body, "data");
}

/// Test that CORS OPTIONS preflight returns 204 No Content with CORS headers.
#[tokio::test]
async fn test_cors_preflight_options() {
    let cors = oxihttp::CorsConfig::permissive();

    let router = oxihttp::Router::new().post("/submit", |_req| async {
        oxihttp::response::text_response("submitted")
    });

    let (addr, _handle) = oxihttp::Server::bind("127.0.0.1:0")
        .with_cors(cors)
        .serve_with_addr(router)
        .await
        .expect("server bind");

    tokio::time::sleep(Duration::from_millis(5)).await;

    let client = oxihttp_client::Client::builder().build().expect("client");
    let url = format!("http://{addr}/submit");

    let req = http::Request::builder()
        .method(http::Method::OPTIONS)
        .uri(&url)
        .header("origin", "https://example.com")
        .header("access-control-request-method", "POST")
        .body(http_body_util::Full::new(bytes::Bytes::new()))
        .expect("request build");

    let resp = client.execute(req).await.expect("execute");

    assert_eq!(resp.status(), http::StatusCode::NO_CONTENT);
    assert!(
        resp.headers()
            .contains_key(http::header::ACCESS_CONTROL_ALLOW_ORIGIN),
        "CORS header should be present on preflight"
    );
}

// ---------------------------------------------------------------------------
// Timeout middleware facade test
// ---------------------------------------------------------------------------

/// Test that `with_timeout` does NOT interrupt a fast handler.
#[tokio::test]
async fn test_timeout_does_not_interrupt_fast_handler() {
    let router = oxihttp::Router::new().get("/fast", |_req| async {
        // This handler completes immediately — well within the timeout.
        oxihttp::response::text_response("fast-response")
    });

    let (addr, _handle) = oxihttp::Server::bind("127.0.0.1:0")
        .with_timeout(Duration::from_secs(5))
        .serve_with_addr(router)
        .await
        .expect("server bind");

    tokio::time::sleep(Duration::from_millis(5)).await;

    let client = oxihttp_client::Client::builder().build().expect("client");
    let url = format!("http://{addr}/fast");
    let resp = client.get(&url).expect("GET").send().await.expect("send");

    assert_eq!(
        resp.status(),
        http::StatusCode::OK,
        "fast handler should succeed within timeout"
    );
    let body = resp.body_text().await.expect("body text");
    assert_eq!(body, "fast-response");
}

/// Test that `with_timeout` returns 408 Request Timeout when the handler is slow.
#[tokio::test]
async fn test_timeout_interrupts_slow_handler() {
    let router = oxihttp::Router::new().get("/slow", |_req| async {
        // Sleep longer than the configured timeout.
        tokio::time::sleep(Duration::from_secs(10)).await;
        oxihttp::response::text_response("too-late")
    });

    let (addr, _handle) = oxihttp::Server::bind("127.0.0.1:0")
        // Very short timeout so the test is fast.
        .with_timeout(Duration::from_millis(50))
        .serve_with_addr(router)
        .await
        .expect("server bind");

    tokio::time::sleep(Duration::from_millis(5)).await;

    let client = oxihttp_client::Client::builder().build().expect("client");
    let url = format!("http://{addr}/slow");
    let resp = client.get(&url).expect("GET").send().await.expect("send");

    assert_eq!(
        resp.status(),
        http::StatusCode::REQUEST_TIMEOUT,
        "slow handler should return 408 Request Timeout after timeout exceeded"
    );
}

// ---------------------------------------------------------------------------
// Combined CORS + timeout test
// ---------------------------------------------------------------------------

/// Test that CORS headers are still present even when timeout is configured.
#[tokio::test]
async fn test_cors_with_timeout_combined() {
    let cors = oxihttp::CorsConfig::permissive();

    let router = oxihttp::Router::new().get("/combined", |_req| async {
        oxihttp::response::text_response("ok")
    });

    let (addr, _handle) = oxihttp::Server::bind("127.0.0.1:0")
        .with_cors(cors)
        .with_timeout(Duration::from_secs(5))
        .serve_with_addr(router)
        .await
        .expect("server bind");

    tokio::time::sleep(Duration::from_millis(5)).await;

    let client = oxihttp_client::Client::builder().build().expect("client");
    let url = format!("http://{addr}/combined");

    let req = http::Request::builder()
        .method(http::Method::GET)
        .uri(&url)
        .header("origin", "https://example.com")
        .body(http_body_util::Full::new(bytes::Bytes::new()))
        .expect("request build");

    let resp = client.execute(req).await.expect("execute");

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert!(
        resp.headers()
            .contains_key(http::header::ACCESS_CONTROL_ALLOW_ORIGIN),
        "CORS headers should be present when both CORS and timeout middleware are active"
    );
    let body = resp.body_text().await.expect("body text");
    assert_eq!(body, "ok");
}
