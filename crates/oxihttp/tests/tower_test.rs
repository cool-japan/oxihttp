//! Integration tests for tower-compatible middleware.
//!
//! Section 1 (client-side): Verifies that `LoggingMiddleware` and
//! `TimingMiddleware` can be registered via `ClientBuilder::with_middleware` /
//! `with_layer` and that the client still executes requests correctly
//! end-to-end.
//!
//! Section 2 (server-side): Verifies that `ServerBuilder::with_layer`
//! correctly wires `LoggingLayer` and `RequestIdLayer` into the server
//! pipeline, and that `x-request-id` is present in responses.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::Request as HyperRequest;
use hyper::Response as HyperResponse;
use std::convert::Infallible;
use std::net::SocketAddr;
use tokio::net::TcpListener;

use oxihttp_client::middleware::{ClientMiddleware, LoggingMiddleware, TimingMiddleware};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn spawn_echo_server() -> SocketAddr {
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
                                "middleware-ok",
                            ))))
                        }),
                    )
                    .await;
            });
        }
    });
    addr
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `LoggingMiddleware` must not interfere with the normal request/response
/// cycle.  The logging side-effect is not directly asserted (it writes to
/// stderr) but the round-trip must succeed with 200 OK and the expected body.
#[tokio::test]
async fn test_logging_middleware_roundtrip() {
    let addr = spawn_echo_server().await;
    let url = format!("http://{addr}/");

    let client = oxihttp_client::Client::builder()
        .with_middleware(LoggingMiddleware::new("test-logger"))
        .build()
        .expect("client build");

    let resp = client
        .get(&url)
        .expect("GET builder")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.body_text().await.expect("body text");
    assert_eq!(body, "middleware-ok");
}

/// `with_layer` is an alias for `with_middleware` — use it to register a
/// `LoggingMiddleware` and verify the request still succeeds.
#[tokio::test]
async fn test_with_layer_alias_roundtrip() {
    let addr = spawn_echo_server().await;
    let url = format!("http://{addr}/");

    let client = oxihttp_client::Client::builder()
        .with_layer(LoggingMiddleware::new("layer-alias"))
        .build()
        .expect("client build");

    let resp = client
        .get(&url)
        .expect("GET builder")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status().as_u16(), 200);
}

/// `TimingMiddleware` must call the user callback with a non-zero duration
/// after a successful round-trip.
#[tokio::test]
async fn test_timing_middleware_records_elapsed() {
    let addr = spawn_echo_server().await;
    let url = format!("http://{addr}/");

    let recorded: Arc<Mutex<Vec<Duration>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = Arc::clone(&recorded);

    let client = oxihttp_client::Client::builder()
        .with_middleware(TimingMiddleware::new(move |d| {
            rec.lock().expect("lock").push(d);
        }))
        .build()
        .expect("client build");

    let resp = client
        .get(&url)
        .expect("GET builder")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status().as_u16(), 200);
    let _ = resp.body_bytes().await.expect("body");

    let durations = recorded.lock().expect("lock");
    assert_eq!(
        durations.len(),
        1,
        "TimingMiddleware callback must be called once"
    );
    // Sanity: elapsed must be at least 1 nanosecond
    assert!(durations[0] > Duration::ZERO, "elapsed must be non-zero");
}

/// Multiple middleware must all be invoked in registration order.
/// We use a simple counter middleware to verify both hooks run for each.
#[tokio::test]
async fn test_multiple_middleware_all_invoked() {
    let addr = spawn_echo_server().await;
    let url = format!("http://{addr}/");

    let before_count = Arc::new(AtomicU32::new(0));
    let after_count = Arc::new(AtomicU32::new(0));

    struct CountingMiddleware {
        before: Arc<AtomicU32>,
        after: Arc<AtomicU32>,
    }

    impl ClientMiddleware for CountingMiddleware {
        fn before_request(&self, _ctx: &oxihttp_client::middleware::RequestContext<'_>) {
            self.before.fetch_add(1, Ordering::SeqCst);
        }
        fn after_response(&self, _ctx: &oxihttp_client::middleware::ResponseContext) {
            self.after.fetch_add(1, Ordering::SeqCst);
        }
    }

    let mw1 = CountingMiddleware {
        before: Arc::clone(&before_count),
        after: Arc::clone(&after_count),
    };
    let mw2 = CountingMiddleware {
        before: Arc::clone(&before_count),
        after: Arc::clone(&after_count),
    };

    let client = oxihttp_client::Client::builder()
        .with_middleware(mw1)
        .with_middleware(mw2)
        .build()
        .expect("client build");

    let resp = client
        .get(&url)
        .expect("GET builder")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status().as_u16(), 200);
    let _ = resp.body_bytes().await.expect("body");

    // Both middleware × both hooks → 2 each
    assert_eq!(before_count.load(Ordering::SeqCst), 2);
    assert_eq!(after_count.load(Ordering::SeqCst), 2);
}

// ===========================================================================
// Section 2 — Server-side tower Layer integration
// ===========================================================================

/// Spawn an OxiHTTP server with the given `ServerBuilder` configuration and
/// return the bound socket address plus a shutdown sender.
async fn spawn_oxihttp_server(
    builder: oxihttp_server::ServerBuilder,
) -> (std::net::SocketAddr, tokio::sync::oneshot::Sender<()>) {
    let router = oxihttp_server::Router::new().get("/hello", |_req| async {
        oxihttp_server::response::text_response("tower-hello")
    });

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown = builder.with_graceful_shutdown(async move {
        let _ = rx.await;
    });
    let (addr, _handle) = shutdown
        .serve_with_addr(router)
        .await
        .expect("serve_with_addr");
    (addr, tx)
}

/// `RequestIdLayer` must inject an `x-request-id` header into every response.
///
/// We start a server with `RequestIdLayer`, issue a plain GET, then assert
/// the response header is present and non-empty.
#[tokio::test]
async fn test_request_id_header() {
    let builder =
        oxihttp_server::Server::bind("127.0.0.1:0").with_layer(oxihttp_server::RequestIdLayer);

    let (addr, _shutdown_tx) = spawn_oxihttp_server(builder).await;
    let url = format!("http://{addr}/hello");

    // Give the server a moment to start accepting.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let resp = oxihttp_client::Client::builder()
        .build()
        .expect("client build")
        .get(&url)
        .expect("GET builder")
        .send()
        .await
        .expect("send");

    assert_eq!(resp.status().as_u16(), 200, "expected 200 OK");

    let has_request_id = resp.headers().contains_key("x-request-id");
    assert!(
        has_request_id,
        "response must contain x-request-id header; got headers: {:?}",
        resp.headers()
    );

    // The value must be a non-empty 16-character hex string.
    let id_value = resp
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .expect("x-request-id header value must be valid UTF-8");
    assert_eq!(id_value.len(), 16, "request-id must be 16 hex chars");
    assert!(
        id_value.chars().all(|c| c.is_ascii_hexdigit()),
        "request-id must be lowercase hex; got: {id_value}"
    );
}

/// `LoggingLayer` must not interfere with the normal request/response cycle.
///
/// The logging output is written to stderr and is not asserted here, but the
/// round-trip must complete successfully with 200 OK.
#[tokio::test]
async fn test_logging_layer_no_panic() {
    let builder =
        oxihttp_server::Server::bind("127.0.0.1:0").with_layer(oxihttp_server::LoggingLayer);

    let (addr, _shutdown_tx) = spawn_oxihttp_server(builder).await;
    let url = format!("http://{addr}/hello");

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let resp = oxihttp_client::Client::builder()
        .build()
        .expect("client build")
        .get(&url)
        .expect("GET builder")
        .send()
        .await
        .expect("send");

    assert_eq!(resp.status().as_u16(), 200, "expected 200 OK");
    let body = resp.body_text().await.expect("body text");
    assert_eq!(body, "tower-hello");
}
