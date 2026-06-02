//! Integration tests for retry logic.

use bytes::Bytes;
use http_body_util::Full;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Spawn a server that returns 503 for the first `fail_count` requests,
/// then returns 200.  Returns (port, Arc<AtomicUsize> call counter).
async fn spawn_flaky_server(fail_count: usize) -> (u16, Arc<AtomicUsize>) {
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let call_count = Arc::new(AtomicUsize::new(0));
    let cc = Arc::clone(&call_count);

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let cc = Arc::clone(&cc);
            tokio::spawn(async move {
                let io = hyper_util::rt::TokioIo::new(stream);
                let svc = service_fn(move |_req: hyper::Request<hyper::body::Incoming>| {
                    let cc = Arc::clone(&cc);
                    async move {
                        let n = cc.fetch_add(1, Ordering::SeqCst);
                        let status = if n < fail_count { 503u16 } else { 200u16 };
                        let body = if n < fail_count {
                            Bytes::from("unavailable")
                        } else {
                            Bytes::from("ok")
                        };
                        Ok::<_, std::convert::Infallible>(
                            hyper::Response::builder()
                                .status(status)
                                .body(Full::new(body))
                                .expect("response build"),
                        )
                    }
                });
                let _ = http1::Builder::new().serve_connection(io, svc).await;
            });
        }
    });

    // Give the server a moment to start accepting
    tokio::time::sleep(Duration::from_millis(5)).await;

    (port, call_count)
}

#[tokio::test]
async fn test_retry_on_503_succeeds_on_third_attempt() {
    let (port, call_count) = spawn_flaky_server(2).await;

    let policy = oxihttp_client::RetryPolicy::new(3).with_backoff_base(Duration::from_millis(10));

    let client = oxihttp_client::Client::builder()
        .retry_policy(policy)
        .build()
        .expect("client build");

    let url = format!("http://127.0.0.1:{port}/retry");
    let resp = client
        .get(&url)
        .expect("builder")
        .send()
        .await
        .expect("send");

    assert_eq!(resp.status().as_u16(), 200);
    // Two failures + one success = 3 total calls
    assert_eq!(call_count.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn test_no_retry_without_policy() {
    // Without a retry policy, a 503 is returned as-is
    let (port, call_count) = spawn_flaky_server(1).await;

    let client = oxihttp_client::Client::builder()
        .build()
        .expect("client build");

    let url = format!("http://127.0.0.1:{port}/no-retry");
    let resp = client
        .get(&url)
        .expect("builder")
        .send()
        .await
        .expect("send");

    assert_eq!(resp.status().as_u16(), 503);
    // Only one call — no retries
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_retry_exhausted_returns_last_response() {
    // Server always returns 503 — policy max_retries=2 means 3 total attempts
    let (port, call_count) = spawn_flaky_server(100).await;

    let policy = oxihttp_client::RetryPolicy::new(2).with_backoff_base(Duration::from_millis(5));

    let client = oxihttp_client::Client::builder()
        .retry_policy(policy)
        .build()
        .expect("client build");

    let url = format!("http://127.0.0.1:{port}/always-503");
    let resp = client
        .get(&url)
        .expect("builder")
        .send()
        .await
        .expect("send");

    // Last 503 is returned (not an error — status-based retry returns the response)
    assert_eq!(resp.status().as_u16(), 503);
    // max_retries=2 means 1 initial + 2 retries = 3 total
    assert_eq!(call_count.load(Ordering::SeqCst), 3);
}
