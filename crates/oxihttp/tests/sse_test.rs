//! Integration tests for SSE support.
//!
//! Run with: cargo test -p oxihttp --features sse --test sse_test

#![cfg(feature = "sse")]

use oxihttp_server::sse::{SseEvent, SseResponse};
use std::net::SocketAddr;

// ---------------------------------------------------------------------------
// Test server helper
// ---------------------------------------------------------------------------

/// Spawn a hyper HTTP/1.1 server that accepts one connection, handles the
/// request with SSE, and sends the given events before closing.
async fn spawn_sse_server(events: Vec<SseEvent>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local addr");

    tokio::spawn(async move {
        // Accept one connection only (sufficient for these unit tests)
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let io = hyper_util::rt::TokioIo::new(stream);
        let events_clone = events.clone();

        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(
                io,
                hyper::service::service_fn(move |_req| {
                    let evts = events_clone.clone();
                    async move {
                        let (sender, sse_resp) = SseResponse::channel(16);
                        tokio::spawn(async move {
                            for evt in evts {
                                sender.send(evt).await.ok();
                            }
                            // sender drops here, closing the stream
                        });
                        let resp = sse_resp.into_response();
                        // Convert oxihttp_core::Body into PinnedBody for hyper
                        Ok::<_, std::convert::Infallible>(resp.map(|b| b.into_pinned()))
                    }
                }),
            )
            .await;
    });

    addr
}

// ---------------------------------------------------------------------------
// Unit tests: SseEvent encoding (no network needed)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_sse_event_encoding() {
    let evt = SseEvent::data("hello world");
    let encoded = evt.encode();
    let s = std::str::from_utf8(&encoded).expect("utf8");
    assert!(s.starts_with("data: hello world\n"), "got: {s:?}");
    assert!(s.ends_with("\n\n"), "got: {s:?}");
}

#[tokio::test]
async fn test_sse_event_with_id_and_type() {
    let evt = SseEvent::data("payload")
        .with_id("42")
        .with_event("update")
        .with_retry(5000);
    let encoded = String::from_utf8(evt.encode().to_vec()).expect("utf8");
    assert!(encoded.contains("id: 42\n"), "got: {encoded:?}");
    assert!(encoded.contains("event: update\n"), "got: {encoded:?}");
    assert!(encoded.contains("retry: 5000\n"), "got: {encoded:?}");
    assert!(encoded.contains("data: payload\n"), "got: {encoded:?}");
}

#[tokio::test]
async fn test_sse_multiline_data() {
    let evt = SseEvent::data("line1\nline2\nline3");
    let encoded = String::from_utf8(evt.encode().to_vec()).expect("utf8");
    assert!(encoded.contains("data: line1\n"), "got: {encoded:?}");
    assert!(encoded.contains("data: line2\n"), "got: {encoded:?}");
    assert!(encoded.contains("data: line3\n"), "got: {encoded:?}");
    assert_eq!(
        encoded.matches("data: ").count(),
        3,
        "expected 3 data: lines in: {encoded:?}"
    );
}

// ---------------------------------------------------------------------------
// Integration tests: real HTTP connection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_sse_stream_three_events() {
    let events = vec![
        SseEvent::data("event1"),
        SseEvent::data("event2"),
        SseEvent::data("event3"),
    ];
    let addr = spawn_sse_server(events).await;

    let client = oxihttp::Client::builder().build().expect("client");
    let resp = client
        .get(&format!("http://{addr}/sse"))
        .expect("GET")
        .send()
        .await
        .expect("send");

    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.body_bytes().await.expect("body bytes");
    let s = String::from_utf8_lossy(&body);
    assert!(s.contains("data: event1"), "body: {s:?}");
    assert!(s.contains("data: event2"), "body: {s:?}");
    assert!(s.contains("data: event3"), "body: {s:?}");
}

#[tokio::test]
async fn test_sse_response_headers() {
    let addr = spawn_sse_server(vec![SseEvent::data("x")]).await;

    let client = oxihttp::Client::builder().build().expect("client");
    let resp = client
        .get(&format!("http://{addr}/sse"))
        .expect("GET")
        .send()
        .await
        .expect("send");

    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("text/event-stream"), "content-type was: {ct:?}");
}

#[tokio::test]
async fn test_sse_empty_data() {
    let evt = SseEvent::data("");
    let encoded = String::from_utf8(evt.encode().to_vec()).expect("utf8");
    // Must have a data: line even for empty payload
    assert!(encoded.contains("data: \n"), "got: {encoded:?}");
    assert!(encoded.ends_with("\n\n"), "got: {encoded:?}");
}
