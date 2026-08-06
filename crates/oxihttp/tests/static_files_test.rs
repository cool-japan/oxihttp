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

    // -----------------------------------------------------------------------
    // Streaming-body regression coverage: "ServeDir/ServeFile read the whole
    // file into memory even for Range requests".
    //
    // These use a multi-megabyte file specifically so a regression back to
    // "read the whole file, then slice a Vec in memory" would still pass
    // *correctness*-only tests like the small-file ones above — the point
    // here is exercising the seek + bounded-chunk-read path across many
    // internal `FileRangeStream::CHUNK_SIZE` (64 KiB) boundaries, and the
    // "tiny Range request against a large file" shape the finding named
    // directly.
    // -----------------------------------------------------------------------

    /// Deterministic pseudo-random-looking byte pattern, so a byte at
    /// position `i` has a distinctive, checkable value (unlike an
    /// all-zeros or all-repeating file, which wouldn't catch an off-by-one
    /// or misaligned seek).
    fn pattern_byte(i: usize) -> u8 {
        (i.wrapping_mul(2654435761).wrapping_add(i / 7) % 256) as u8
    }

    fn make_pattern_file(len: usize) -> Vec<u8> {
        (0..len).map(pattern_byte).collect()
    }

    /// A full GET of a multi-megabyte file — spanning many internal
    /// `FileRangeStream` chunks — must reproduce the file exactly.
    #[tokio::test]
    async fn test_large_file_full_get_streams_correctly() {
        let dir = make_temp_dir();
        const LEN: usize = 5 * 1024 * 1024 + 1234; // not a multiple of the 64 KiB chunk size
        let content = make_pattern_file(LEN);
        fs::write(dir.join("large.bin"), &content).unwrap();

        let addr = spawn_static_server(ServeDir::new(&dir)).await;
        let client = make_client();

        let resp = client
            .get(&format!("http://{addr}/large.bin"))
            .expect("GET")
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.content_length(), Some(LEN as u64));
        let body = resp.body_bytes().await.expect("body");
        assert_eq!(body.len(), LEN);
        assert_eq!(body.as_ref(), content.as_slice());
    }

    /// A range spanning multiple internal 64 KiB stream chunks (not
    /// aligned to a chunk boundary) must be sliced exactly, proving the
    /// seek + bounded-read path (not just "happens to work for a range
    /// smaller than one chunk").
    #[tokio::test]
    async fn test_large_file_multi_chunk_range_is_exact() {
        let dir = make_temp_dir();
        const LEN: usize = 1024 * 1024;
        let content = make_pattern_file(LEN);
        fs::write(dir.join("ranged.bin"), &content).unwrap();

        let addr = spawn_static_server(ServeDir::new(&dir)).await;
        let client = make_client();

        // 200,000 bytes starting at an offset that is itself not aligned to
        // the 64 KiB internal chunk size, spanning several chunk boundaries.
        let start = 12_345usize;
        let end = start + 200_000 - 1;
        let resp = client
            .get(&format!("http://{addr}/ranged.bin"))
            .expect("GET")
            .header("range", &format!("bytes={start}-{end}"))
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
            .expect("str")
            .to_owned();
        assert_eq!(cr, format!("bytes {start}-{end}/{LEN}"));
        let body = resp.body_bytes().await.expect("body");
        assert_eq!(body.as_ref(), &content[start..=end]);
    }

    /// The specific scenario named by the audit finding: a 1-byte `Range`
    /// request against a several-megabyte file must return exactly that one
    /// byte, correctly identified regardless of where in the file it falls.
    #[tokio::test]
    async fn test_one_byte_range_on_large_file() {
        let dir = make_temp_dir();
        const LEN: usize = 4 * 1024 * 1024;
        let content = make_pattern_file(LEN);
        fs::write(dir.join("onebyte.bin"), &content).unwrap();

        let addr = spawn_static_server(ServeDir::new(&dir)).await;
        let client = make_client();

        // Roughly the middle of the file — well past the first chunk, so a
        // correct result depends on the seek actually taking effect.
        let offset = LEN / 2 + 17;
        let resp = client
            .get(&format!("http://{addr}/onebyte.bin"))
            .expect("GET")
            .header("range", &format!("bytes={offset}-{offset}"))
            .expect("header")
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), http::StatusCode::PARTIAL_CONTENT);
        assert_eq!(resp.content_length(), Some(1));
        let body = resp.body_bytes().await.expect("body");
        assert_eq!(body.len(), 1);
        assert_eq!(body[0], pattern_byte(offset));
    }

    /// Many concurrent range requests against the same large file must each
    /// get their own correct slice — this is only possible if each request
    /// opens (and seeks) an independent file handle rather than sharing
    /// mutable state, which is exactly what per-request streaming (as
    /// opposed to a single shared in-memory buffer) requires anyway.
    #[tokio::test]
    async fn test_concurrent_ranges_on_large_file_are_independent() {
        let dir = make_temp_dir();
        const LEN: usize = 2 * 1024 * 1024;
        let content = make_pattern_file(LEN);
        fs::write(dir.join("concurrent.bin"), &content).unwrap();

        let addr = spawn_static_server(ServeDir::new(&dir)).await;

        let ranges: Vec<(usize, usize)> = vec![
            (0, 99),
            (500_000, 500_099),
            (1_000_000, 1_050_000),
            (LEN - 100, LEN - 1),
        ];

        let mut handles = Vec::new();
        for (start, end) in ranges.clone() {
            handles.push(tokio::spawn(async move {
                let client = make_client();
                let resp = client
                    .get(&format!("http://{addr}/concurrent.bin"))
                    .expect("GET")
                    .header("range", &format!("bytes={start}-{end}"))
                    .expect("header")
                    .send()
                    .await
                    .expect("send");
                assert_eq!(resp.status(), http::StatusCode::PARTIAL_CONTENT);
                resp.body_bytes().await.expect("body")
            }));
        }

        for (handle, (start, end)) in handles.into_iter().zip(ranges) {
            let body = handle.await.expect("task");
            assert_eq!(body.as_ref(), &content[start..=end], "range {start}-{end}");
        }
    }

    /// Two different files with identical content but different
    /// modification times must now get *different* ETags — pinning the
    /// documented switch from a content hash to metadata (mtime + length),
    /// which is what makes an O(1), no-content-read ETag possible.
    ///
    /// Note: `static_files::tests::test_etag_differs_for_different_mtime`
    /// (in `oxihttp-server`) already covers this precise behavior
    /// deterministically, against synthetic `SystemTime` values that don't
    /// depend on real filesystem timing at all. This test additionally
    /// exercises the real wiring (an actual `serve()` call reading real
    /// file metadata end-to-end), so — unlike a fixed sleep-and-hope — it
    /// actively waits for the filesystem's mtime clock to actually advance
    /// (bounded retries) before asserting on ETags, rather than gambling
    /// against an unknown mtime resolution (some filesystems only tick
    /// once per second).
    #[tokio::test]
    async fn test_etag_differs_across_files_with_same_content_different_mtime() {
        let dir = make_temp_dir();
        let path_a = dir.join("a.txt");
        let path_b = dir.join("b.txt");
        fs::write(&path_a, b"identical content").unwrap();
        let mtime_a = fs::metadata(&path_a)
            .expect("metadata a")
            .modified()
            .expect("mtime a");

        // Actively wait for the filesystem clock to visibly advance instead
        // of trusting a fixed sleep duration to outrun an unknown mtime
        // resolution.
        let mut advanced = false;
        for _ in 0..50 {
            fs::write(&path_b, b"identical content").unwrap();
            let mtime_b = fs::metadata(&path_b)
                .expect("metadata b")
                .modified()
                .expect("mtime b");
            if mtime_b != mtime_a {
                advanced = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            advanced,
            "filesystem mtime did not advance after retrying for ~2.5s; \
             cannot exercise mtime-based ETag differentiation on this filesystem"
        );

        let addr = spawn_static_server(ServeDir::new(&dir)).await;
        let client = make_client();

        let etag_of = |name: &'static str, addr: SocketAddr, client: Client| async move {
            let resp = client
                .get(&format!("http://{addr}/{name}"))
                .expect("GET")
                .send()
                .await
                .expect("send");
            resp.headers()
                .get("etag")
                .expect("ETag")
                .to_str()
                .expect("str")
                .to_owned()
        };

        let etag_a = etag_of("a.txt", addr, client.clone()).await;
        let etag_b = etag_of("b.txt", addr, client).await;
        // The precondition above already guarantees `mtime_a != mtime_b`,
        // so a failure here is a real ETag-computation regression, not
        // filesystem-timing flakiness.
        assert_ne!(etag_a, etag_b);
    }

    // -----------------------------------------------------------------------
    // Symlink handling — `ServeDir::with_symlink_protection`
    // -----------------------------------------------------------------------
    //
    // `is_path_safe`'s traversal check is purely lexical (the request path
    // itself never contains `..`), so by default a symlink placed *inside*
    // the served root that points *outside* it is followed and served. See
    // the module-level security note on `static_files`. These tests pin
    // both the documented default behavior and the opt-in that closes it,
    // using real symlinks so a regression in either direction is caught
    // end-to-end rather than only at the lexical-check unit-test level.

    /// Documents the default: a symlink inside the root that resolves
    /// outside it is followed and served. This is not the fix — it is the
    /// baseline the next two tests contrast against, so a future change to
    /// this default is a deliberate, visible decision rather than a silent
    /// regression rediscovered as a security report.
    #[tokio::test]
    #[cfg(unix)]
    async fn symlink_escaping_root_is_served_by_default() {
        let root = make_temp_dir();
        let secret_dir = make_temp_dir();
        fs::write(secret_dir.join("secret.txt"), b"top secret").unwrap();
        std::os::unix::fs::symlink(secret_dir.join("secret.txt"), root.join("escape.txt"))
            .expect("create symlink");

        let addr = spawn_static_server(ServeDir::new(&root)).await;
        let client = make_client();

        let resp = client
            .get(&format!("http://{addr}/escape.txt"))
            .expect("GET")
            .send()
            .await
            .expect("send");

        assert_eq!(
            resp.status(),
            http::StatusCode::OK,
            "documented default: symlinks inside the root are followed"
        );
        let body = resp.body_text().await.expect("body");
        assert_eq!(body, "top secret");
    }

    /// `with_symlink_protection(true)` closes the gap the previous test
    /// documents: a symlink resolving outside the served root must be
    /// rejected with `403 Forbidden` instead of served.
    #[tokio::test]
    #[cfg(unix)]
    async fn symlink_escaping_root_is_rejected_with_protection_enabled() {
        let root = make_temp_dir();
        let secret_dir = make_temp_dir();
        fs::write(secret_dir.join("secret.txt"), b"top secret").unwrap();
        std::os::unix::fs::symlink(secret_dir.join("secret.txt"), root.join("escape.txt"))
            .expect("create symlink");

        let addr = spawn_static_server(ServeDir::new(&root).with_symlink_protection(true)).await;
        let client = make_client();

        let resp = client
            .get(&format!("http://{addr}/escape.txt"))
            .expect("GET")
            .send()
            .await
            .expect("send");

        assert_eq!(resp.status(), http::StatusCode::FORBIDDEN);
    }

    /// A symlink that stays *inside* the served root must still be served
    /// normally with protection enabled — only escapes are rejected, not
    /// symlinks in general.
    #[tokio::test]
    #[cfg(unix)]
    async fn symlink_staying_inside_root_is_served_with_protection_enabled() {
        let root = make_temp_dir();
        fs::write(root.join("real.txt"), b"real content").unwrap();
        std::os::unix::fs::symlink(root.join("real.txt"), root.join("link.txt"))
            .expect("create symlink");

        let addr = spawn_static_server(ServeDir::new(&root).with_symlink_protection(true)).await;
        let client = make_client();

        let resp = client
            .get(&format!("http://{addr}/link.txt"))
            .expect("GET")
            .send()
            .await
            .expect("send");

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.body_text().await.expect("body");
        assert_eq!(body, "real content");
    }
}
