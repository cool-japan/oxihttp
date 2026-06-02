//! Integration tests for M7 Block B/C: DNS resolver injection, HTTP/2 tuning, TCP knobs.

use oxihttp_client::{ClientBuilder, Http2Settings};
use oxihttp_server::{Router, Server, ServerHttp2Settings};
use std::time::Duration;

#[tokio::test]
async fn test_client_with_http2_settings() {
    // Build a client with H2 settings — verify no error on construction
    let _client = ClientBuilder::new()
        .with_http2_settings(Http2Settings {
            initial_stream_window_size: Some(1024 * 1024),
            adaptive_window: Some(true),
            ..Default::default()
        })
        .build()
        .expect("build with http2 settings");
    // If we get here without panic, settings were accepted
}

#[tokio::test]
async fn test_client_with_tcp_settings() {
    let _client = ClientBuilder::new()
        .with_tcp_nodelay(true)
        .with_tcp_keepalive(Duration::from_secs(30))
        .build()
        .expect("build with tcp settings");
}

#[tokio::test]
async fn test_client_with_all_h2_settings() {
    let _client = ClientBuilder::new()
        .with_http2_settings(Http2Settings {
            initial_stream_window_size: Some(512 * 1024),
            initial_connection_window_size: Some(2 * 1024 * 1024),
            adaptive_window: Some(false),
            keep_alive_interval: Some(Duration::from_secs(10)),
            keep_alive_timeout: Some(Duration::from_secs(5)),
            max_frame_size: Some(16_384),
            max_concurrent_reset_streams: Some(10),
            max_send_buf_size: Some(1024 * 1024),
        })
        .build()
        .expect("build with all h2 settings");
}

#[tokio::test]
async fn test_server_with_tcp_nodelay() {
    async fn handler(
        _req: oxihttp_server::Request,
    ) -> Result<hyper::Response<http_body_util::Full<bytes::Bytes>>, oxihttp_core::OxiHttpError>
    {
        Ok(hyper::Response::new(http_body_util::Full::new(
            bytes::Bytes::from("ok"),
        )))
    }

    let router = Router::new().get("/", handler);
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let (addr, _handle) = Server::bind("127.0.0.1:0")
        .with_tcp_nodelay(true)
        .with_http2_settings(ServerHttp2Settings {
            max_concurrent_streams: Some(100),
            ..Default::default()
        })
        .with_graceful_shutdown(async move {
            let _ = rx.await;
        })
        .serve_with_addr(router)
        .await
        .expect("bind");

    // Give the server a moment to start.
    tokio::time::sleep(Duration::from_millis(10)).await;

    let client = ClientBuilder::new().build().expect("client");
    let resp = client
        .get(&format!("http://{addr}/"))
        .expect("GET")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), http::StatusCode::OK);

    let _ = tx.send(());
}

#[tokio::test]
async fn test_server_with_tcp_keepalive() {
    async fn handler(
        _req: oxihttp_server::Request,
    ) -> Result<hyper::Response<http_body_util::Full<bytes::Bytes>>, oxihttp_core::OxiHttpError>
    {
        Ok(hyper::Response::new(http_body_util::Full::new(
            bytes::Bytes::from("keepalive ok"),
        )))
    }

    let router = Router::new().get("/ping", handler);
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let (addr, _handle) = Server::bind("127.0.0.1:0")
        .with_tcp_nodelay(true)
        .with_tcp_keepalive(Duration::from_secs(60))
        .with_graceful_shutdown(async move {
            let _ = rx.await;
        })
        .serve_with_addr(router)
        .await
        .expect("bind");

    tokio::time::sleep(Duration::from_millis(10)).await;

    let client = ClientBuilder::new().build().expect("client");
    let resp = client
        .get(&format!("http://{addr}/ping"))
        .expect("GET")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.body_text().await.expect("body");
    assert_eq!(body, "keepalive ok");

    let _ = tx.send(());
}

#[tokio::test]
async fn test_resolver_build_with_resolver_no_resolver_error() {
    // Calling build_with_resolver without with_resolver should return Dns error
    let result = ClientBuilder::new().build_with_resolver();
    assert!(result.is_err());
    let err = result.expect_err("should fail");
    assert!(matches!(err, oxihttp_core::OxiHttpError::Dns(_)));
}
