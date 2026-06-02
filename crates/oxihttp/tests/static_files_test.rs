//! Integration tests for static file serving with ETag support.
//!
//! Run with:
//!   cargo test -p oxihttp --features static-files --test static_files_test

#[cfg(feature = "static-files")]
mod tests {
    use std::fs;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;

    use bytes::Bytes;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use oxihttp_client::Client;
    use oxihttp_core::PinnedBody;
    use oxihttp_server::ServeDir;
    use tokio::net::TcpListener;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Create a uniquely named temporary directory for a test.
    fn make_temp_dir() -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        let dir = std::env::temp_dir().join(format!(
            "oxihttp_static_test_{nanos}_{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Spawn a minimal hyper HTTP/1.1 server backed by `ServeDir`.
    ///
    /// Returns the bound `SocketAddr`. The server runs until the test process
    /// ends (the spawned task is abandoned on drop).
    async fn spawn_static_server(serve_dir: ServeDir) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let serve_dir = Arc::new(serve_dir);

        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let sd = Arc::clone(&serve_dir);
                tokio::spawn(async move {
                    let io = hyper_util::rt::TokioIo::new(stream);
                    let svc = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                        let sd = Arc::clone(&sd);
                        async move {
                            let method = req.method().clone();
                            let path = req.uri().path().to_string();
                            let headers = req.headers().clone();

                            let body_resp = sd
                                .serve(&method, &path, &headers)
                                .await
                                .unwrap_or_else(|_| {
                                    http::Response::builder()
                                        .status(http::StatusCode::INTERNAL_SERVER_ERROR)
                                        .body(oxihttp_core::Body::empty())
                                        .unwrap()
                                });

                            // Convert oxihttp_core::Body → PinnedBody for hyper.
                            let (parts, body) = body_resp.into_parts();
                            let pinned: PinnedBody = body.into_pinned();
                            Ok::<_, std::convert::Infallible>(http::Response::from_parts(
                                parts, pinned,
                            ))
                        }
                    });
                    let _ = http1::Builder::new().serve_connection(io, svc).await;
                });
            }
        });

        // Give the server a moment to start accepting.
        tokio::time::sleep(Duration::from_millis(10)).await;
        addr
    }

    fn make_client() -> Client {
        Client::builder()
            .redirect_policy(oxihttp_client::RedirectPolicy::None)
            .build()
            .expect("client")
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_serve_file_200() {
        let dir = make_temp_dir();
        fs::write(dir.join("hello.txt"), b"Hello, world!").unwrap();

        let addr = spawn_static_server(ServeDir::new(&dir)).await;
        let client = make_client();

        let resp = client
            .get(&format!("http://{addr}/hello.txt"))
            .expect("GET")
            .send()
            .await
            .expect("send");

        assert_eq!(resp.status(), http::StatusCode::OK);
        // ETag must be present and quoted.
        let etag = resp.headers().get("etag").expect("ETag header");
        let etag_str = etag.to_str().expect("ETag value");
        assert!(etag_str.starts_with('"') && etag_str.ends_with('"'));
        // Content-Type should be text/plain.
        let ct = resp.content_type().unwrap_or("");
        assert!(ct.contains("text/plain"), "unexpected content-type: {ct}");
        let body = resp.body_text().await.expect("body");
        assert_eq!(body, "Hello, world!");
    }

    #[tokio::test]
    async fn test_serve_missing_404() {
        let dir = make_temp_dir();

        let addr = spawn_static_server(ServeDir::new(&dir)).await;
        let client = make_client();

        let resp = client
            .get(&format!("http://{addr}/does_not_exist.txt"))
            .expect("GET")
            .send()
            .await
            .expect("send");

        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_if_none_match_304() {
        let dir = make_temp_dir();
        fs::write(dir.join("cached.txt"), b"cacheable content").unwrap();

        let addr = spawn_static_server(ServeDir::new(&dir)).await;
        let client = make_client();

        // First request — get the ETag.
        let resp = client
            .get(&format!("http://{addr}/cached.txt"))
            .expect("GET")
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), http::StatusCode::OK);
        let etag = resp
            .headers()
            .get("etag")
            .expect("ETag header")
            .to_str()
            .expect("ETag str")
            .to_owned();

        // Second request with matching If-None-Match.
        let resp2 = client
            .get(&format!("http://{addr}/cached.txt"))
            .expect("GET")
            .header("if-none-match", &etag)
            .expect("header")
            .send()
            .await
            .expect("send2");
        assert_eq!(resp2.status(), http::StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn test_etag_mismatch_200() {
        let dir = make_temp_dir();
        fs::write(dir.join("data.bin"), b"binary data here").unwrap();

        let addr = spawn_static_server(ServeDir::new(&dir)).await;
        let client = make_client();

        // Request with a wrong ETag — should get a full 200 response.
        let resp = client
            .get(&format!("http://{addr}/data.bin"))
            .expect("GET")
            .header("if-none-match", "\"wrongetag000000000000000000000000\"")
            .expect("header")
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.body_bytes().await.expect("body");
        assert_eq!(body, Bytes::from_static(b"binary data here"));
    }

    #[tokio::test]
    async fn test_range_request_206() {
        let dir = make_temp_dir();
        // Content: "Hello, world!" (13 bytes)
        fs::write(dir.join("range.txt"), b"Hello, world!").unwrap();

        let addr = spawn_static_server(ServeDir::new(&dir)).await;
        let client = make_client();

        // Request bytes 0-3 → "Hell"
        let resp = client
            .get(&format!("http://{addr}/range.txt"))
            .expect("GET")
            .header("range", "bytes=0-3")
            .expect("header")
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), http::StatusCode::PARTIAL_CONTENT);
        let cr = resp
            .headers()
            .get("content-range")
            .expect("Content-Range")
            .to_str()
            .expect("Content-Range str");
        assert_eq!(cr, "bytes 0-3/13");
        let body = resp.body_bytes().await.expect("body");
        assert_eq!(body.as_ref(), b"Hell");
    }

    #[tokio::test]
    async fn test_range_open_end_206() {
        let dir = make_temp_dir();
        // "0123456789" (10 bytes)
        fs::write(dir.join("nums.txt"), b"0123456789").unwrap();

        let addr = spawn_static_server(ServeDir::new(&dir)).await;
        let client = make_client();

        // Range: bytes=5- → "56789"
        let resp = client
            .get(&format!("http://{addr}/nums.txt"))
            .expect("GET")
            .header("range", "bytes=5-")
            .expect("header")
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), http::StatusCode::PARTIAL_CONTENT);
        let body = resp.body_bytes().await.expect("body");
        assert_eq!(body.as_ref(), b"56789");
    }

    #[tokio::test]
    async fn test_path_traversal_403() {
        let dir = make_temp_dir();
        fs::write(dir.join("secret.txt"), b"secret").unwrap();

        let addr = spawn_static_server(ServeDir::new(&dir)).await;
        let client = make_client();

        // Attempt path traversal via URL.
        let resp = client
            .get(&format!("http://{addr}/../secret.txt"))
            .expect("GET")
            .send()
            .await
            .expect("send");
        // Hyper normalises the URI so "../" collapses to "/" on the server side;
        // if the path collapses to root (no index), we get 404.
        // Either 403 or 404 is acceptable — 200 would mean a security hole.
        let status = resp.status();
        assert!(
            status == http::StatusCode::FORBIDDEN || status == http::StatusCode::NOT_FOUND,
            "expected 403 or 404, got {status}"
        );
    }

    #[tokio::test]
    async fn test_head_request() {
        let dir = make_temp_dir();
        fs::write(dir.join("head.txt"), b"head body content").unwrap();

        let addr = spawn_static_server(ServeDir::new(&dir)).await;
        let client = make_client();

        let resp = client
            .head(&format!("http://{addr}/head.txt"))
            .expect("HEAD")
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), http::StatusCode::OK);
        // Content-Length must be present and correct.
        let cl = resp.content_length().expect("Content-Length");
        assert_eq!(cl, b"head body content".len() as u64);
        // Body must be empty for HEAD.
        let body = resp.body_bytes().await.expect("body");
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn test_index_file() {
        let dir = make_temp_dir();
        fs::write(dir.join("index.html"), b"<h1>Index</h1>").unwrap();

        let addr = spawn_static_server(ServeDir::new(&dir).with_index("index.html")).await;
        let client = make_client();

        // GET / should return the index file.
        let resp = client
            .get(&format!("http://{addr}/"))
            .expect("GET")
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), http::StatusCode::OK);
        let ct = resp.content_type().unwrap_or("");
        assert!(ct.contains("text/html"), "unexpected content-type: {ct}");
        let body = resp.body_text().await.expect("body");
        assert_eq!(body, "<h1>Index</h1>");
    }
}
