//! Criterion benchmarks for the oxihttp-server crate (M9 Block E).
//!
//! Groups:
//!   1. `router_dispatch`      — pure in-memory Router::resolve() latency at 10/100/1000 routes
//!   2. `h1_throughput`        — HTTP/1.1 requests-per-second over loopback
//!   3. `static_file_serving`  — throughput at 1KB/100KB/1MB (gated on `static-files`)
//!   4. `middleware_overhead`  — per-layer latency cost (0/1/3 layer configurations)
//!   5. `json_handler`         — JSON POST round-trip latency
//!   6. `ws_throughput`        — WebSocket echo message throughput (gated on `websocket`)
//!   7. `connection_accept`    — TCP connection-to-first-byte latency

#[cfg(any(feature = "static-files", feature = "websocket"))]
use criterion::Throughput;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use http::Method;
use std::hint::black_box;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Shared handler helper
// ---------------------------------------------------------------------------

async fn dummy_handler(
    _req: oxihttp_server::router::Request,
) -> Result<hyper::Response<http_body_util::Full<bytes::Bytes>>, oxihttp_core::OxiHttpError> {
    Ok(hyper::Response::new(http_body_util::Full::new(
        bytes::Bytes::from_static(b""),
    )))
}

// ---------------------------------------------------------------------------
// Router construction helper
// ---------------------------------------------------------------------------

fn build_router(n: usize) -> oxihttp_server::Router {
    let mut router = oxihttp_server::Router::new();
    for i in 0..n {
        let path = format!("/route_{i}");
        router = router.get(Box::leak(path.into_boxed_str()), dummy_handler);
    }
    router
}

// ---------------------------------------------------------------------------
// Server spawn helper — returns (addr, _shutdown_tx) where _shutdown_tx must
// be kept alive for the server to stay alive.
// ---------------------------------------------------------------------------

async fn spawn_test_server(
    router: oxihttp_server::Router,
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

// ---------------------------------------------------------------------------
// Hyper HTTP/1.1 client helpers (no oxihttp-client dep to avoid cycles)
// ---------------------------------------------------------------------------

/// Execute a single GET request to addr/path and return the status code.
/// Creates a new TCP connection per call.
async fn raw_http_get(addr: std::net::SocketAddr, path: &str) -> u16 {
    use http_body_util::{BodyExt, Empty};
    use hyper::body::Bytes;
    use hyper_util::rt::TokioIo;

    let stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("TCP connect");
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(io)
        .await
        .expect("HTTP/1.1 handshake");

    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .method(Method::GET)
        .uri(path)
        .header("host", addr.to_string())
        .body(Empty::<Bytes>::new())
        .expect("build request");

    let resp = sender.send_request(req).await.expect("send request");
    let status = resp.status().as_u16();
    let _ = resp.into_body().collect().await;
    status
}

/// Execute a single POST request with a JSON body, return the response body bytes.
async fn raw_http_post_json(addr: std::net::SocketAddr, path: &str, body: Vec<u8>) -> Vec<u8> {
    use http_body_util::{BodyExt, Full};
    use hyper::body::Bytes;
    use hyper_util::rt::TokioIo;

    let stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("TCP connect");
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
        .await
        .expect("HTTP/1.1 handshake");

    tokio::spawn(async move {
        let _ = conn.await;
    });

    let body_bytes = Bytes::from(body);
    let content_len = body_bytes.len().to_string();
    let req = hyper::Request::builder()
        .method(Method::POST)
        .uri(path)
        .header("host", addr.to_string())
        .header("content-type", "application/json")
        .header("content-length", content_len)
        .body(Full::new(body_bytes))
        .expect("build POST request");

    let resp = sender.send_request(req).await.expect("send request");
    let collected = resp.into_body().collect().await.expect("collect body");
    collected.to_bytes().to_vec()
}

// ---------------------------------------------------------------------------
// Group 1: router_dispatch (pure in-memory, no I/O)
// ---------------------------------------------------------------------------

fn bench_router_dispatch(c: &mut Criterion) {
    let mut group = c.benchmark_group("router_dispatch");

    for &n in &[10usize, 100, 1000] {
        let router = build_router(n);
        let method = Method::GET;

        let best = "/route_0".to_string();
        group.bench_with_input(BenchmarkId::new("best_case", n), &n, |b, _| {
            b.iter(|| black_box(router.resolve(black_box(&method), black_box(best.as_str()))))
        });

        let worst = format!("/route_{}", n - 1);
        group.bench_with_input(BenchmarkId::new("worst_case", n), &n, |b, _| {
            b.iter(|| black_box(router.resolve(black_box(&method), black_box(worst.as_str()))))
        });

        let miss = "/nonexistent".to_string();
        group.bench_with_input(BenchmarkId::new("miss", n), &n, |b, _| {
            b.iter(|| black_box(router.resolve(black_box(&method), black_box(miss.as_str()))))
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Group 2: h1_throughput (TCP round-trip, new connection per iteration)
// ---------------------------------------------------------------------------

fn bench_h1_throughput(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("tokio rt");

    let router = oxihttp_server::Router::new().get("/bench", |_req| async {
        oxihttp_server::response::text_response("ok")
    });
    let (addr, _shutdown_tx) = rt.block_on(spawn_test_server(router));

    let mut group = c.benchmark_group("h1_throughput");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("single_get", |b| {
        b.to_async(&rt).iter(|| async {
            let status = raw_http_get(addr, "/bench").await;
            black_box(status);
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Group 3: static_file_serving (gated on `static-files`)
// ---------------------------------------------------------------------------

#[cfg(feature = "static-files")]
fn bench_static_file_serving(c: &mut Criterion) {
    use oxihttp_server::ServeDir;
    use std::net::SocketAddr;
    use std::sync::Arc;

    async fn spawn_static_server(serve_dir: ServeDir) -> SocketAddr {
        use hyper::server::conn::http1;
        use hyper::service::service_fn;
        use hyper_util::rt::TokioIo;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind static bench server");
        let addr = listener.local_addr().expect("local addr");
        let serve_dir = Arc::new(serve_dir);

        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let sd = Arc::clone(&serve_dir);
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let svc = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                        let sd = Arc::clone(&sd);
                        async move {
                            let method = req.method().clone();
                            let path = req.uri().path().to_string();
                            let headers: http::HeaderMap = req.headers().clone();

                            let body_resp = sd
                                .serve(&method, &path, &headers)
                                .await
                                .unwrap_or_else(|_| {
                                    http::Response::builder()
                                        .status(http::StatusCode::INTERNAL_SERVER_ERROR)
                                        .body(oxihttp_core::Body::empty())
                                        .expect("build 500")
                                });

                            let (parts, body) = body_resp.into_parts();
                            let pinned: oxihttp_core::PinnedBody = body.into_pinned();
                            Ok::<_, std::convert::Infallible>(http::Response::from_parts(
                                parts, pinned,
                            ))
                        }
                    });
                    let _ = http1::Builder::new().serve_connection(io, svc).await;
                });
            }
        });

        tokio::time::sleep(Duration::from_millis(10)).await;
        addr
    }

    let rt = tokio::runtime::Runtime::new().expect("tokio rt");

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let tmp_dir = std::env::temp_dir().join(format!(
        "oxihttp_server_bench_files_{}_{}",
        std::process::id(),
        nanos
    ));
    std::fs::create_dir_all(&tmp_dir).expect("create bench temp dir");

    let sizes: &[(usize, &str)] = &[
        (1024, "file_1k.bin"),
        (102_400, "file_100k.bin"),
        (1_048_576, "file_1m.bin"),
    ];

    for &(size, name) in sizes {
        std::fs::write(tmp_dir.join(name), vec![b'x'; size]).expect("write bench file");
    }

    let addr = rt.block_on(spawn_static_server(ServeDir::new(&tmp_dir)));

    let mut group = c.benchmark_group("static_file_serving");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(10);

    for &(size, name) in sizes {
        group.throughput(Throughput::Bytes(size as u64));
        let path = format!("/{name}");

        group.bench_with_input(BenchmarkId::from_parameter(size), &path, |b, path| {
            b.to_async(&rt).iter(|| async {
                use http_body_util::{BodyExt, Empty};
                use hyper::body::Bytes;
                use hyper_util::rt::TokioIo;

                let stream = tokio::net::TcpStream::connect(addr)
                    .await
                    .expect("TCP connect");
                let io = TokioIo::new(stream);
                let (mut sender, conn) =
                    hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(io)
                        .await
                        .expect("handshake");
                tokio::spawn(async move {
                    let _ = conn.await;
                });
                let req = hyper::Request::builder()
                    .method(Method::GET)
                    .uri(path.as_str())
                    .header("host", addr.to_string())
                    .body(Empty::<Bytes>::new())
                    .expect("build request");
                let resp = sender.send_request(req).await.expect("send request");
                let body = resp.into_body().collect().await.expect("collect body");
                black_box(body.to_bytes().len());
            });
        });
    }

    group.finish();
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// ---------------------------------------------------------------------------
// Group 4: middleware_overhead (TCP round-trip with varying layer counts)
// ---------------------------------------------------------------------------

fn bench_middleware_overhead(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("tokio rt");

    let mut group = c.benchmark_group("middleware_overhead");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    // 0 layers
    {
        let router = oxihttp_server::Router::new().get("/bench", |_req| async {
            oxihttp_server::response::text_response("ok")
        });
        let (addr, _shutdown_tx) = rt.block_on(spawn_test_server(router));

        group.bench_function("no_middleware", |b| {
            b.to_async(&rt).iter(|| async {
                let status = raw_http_get(addr, "/bench").await;
                black_box(status);
            });
        });
    }

    // 1 layer: CORS
    {
        let router = oxihttp_server::Router::new().get("/bench", |_req| async {
            oxihttp_server::response::text_response("ok")
        });
        let (addr, _shutdown_tx) = rt.block_on(async {
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            let (addr, _handle) = oxihttp_server::Server::bind("127.0.0.1:0")
                .with_cors(oxihttp_server::CorsConfig::permissive())
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .serve_with_addr(router)
                .await
                .expect("server bind");
            tokio::time::sleep(Duration::from_millis(10)).await;
            (addr, tx)
        });

        group.bench_function("cors_only", |b| {
            b.to_async(&rt).iter(|| async {
                let status = raw_http_get(addr, "/bench").await;
                black_box(status);
            });
        });
    }

    // 3 layers: CORS + body_limit + rate_limiter
    {
        let router = oxihttp_server::Router::new().get("/bench", |_req| async {
            oxihttp_server::response::text_response("ok")
        });
        let (addr, _shutdown_tx) = rt.block_on(async {
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            let (addr, _handle) = oxihttp_server::Server::bind("127.0.0.1:0")
                .with_cors(oxihttp_server::CorsConfig::permissive())
                .with_body_limit(1024 * 1024)
                .with_rate_limiter(oxihttp_server::RateLimiter::new(100_000, 100_000.0))
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .serve_with_addr(router)
                .await
                .expect("server bind");
            tokio::time::sleep(Duration::from_millis(10)).await;
            (addr, tx)
        });

        group.bench_function("cors_body_limit_rate", |b| {
            b.to_async(&rt).iter(|| async {
                let status = raw_http_get(addr, "/bench").await;
                black_box(status);
            });
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Group 5: json_handler (JSON POST round-trip)
// ---------------------------------------------------------------------------

fn bench_json_handler(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("tokio rt");

    let router = oxihttp_server::Router::new().post("/json", |req| async move {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct Payload {
            value: u64,
        }

        let payload: Payload = req.body_json().await.unwrap_or(Payload { value: 0 });
        let resp_payload = Payload {
            value: payload.value + 1,
        };
        let body = serde_json::to_vec(&resp_payload).expect("serialize");
        let response = hyper::Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(http_body_util::Full::new(bytes::Bytes::from(body)))
            .expect("build response");
        Ok(response)
    });

    let (addr, _shutdown_tx) = rt.block_on(spawn_test_server(router));
    let request_body = serde_json::to_vec(&serde_json::json!({"value": 42})).expect("serialize");

    let mut group = c.benchmark_group("json_handler");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("post_json", |b| {
        b.to_async(&rt).iter(|| async {
            let body = raw_http_post_json(addr, "/json", request_body.clone()).await;
            black_box(body.len());
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Group 6: ws_throughput (gated on `websocket`)
// ---------------------------------------------------------------------------

#[cfg(feature = "websocket")]
fn bench_ws_throughput(c: &mut Criterion) {
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::sync::Mutex;

    let rt = tokio::runtime::Runtime::new().expect("tokio rt");

    let addr = rt.block_on(async {
        let router = oxihttp_server::Router::new().get("/ws", |req| async move {
            let (upgrade, resp) = oxihttp_server::ws::upgrade(req)?;
            tokio::spawn(async move {
                if let Ok(mut socket) = upgrade.accept().await {
                    while let Ok(Some(msg)) = socket.recv().await {
                        match msg {
                            oxihttp_server::Message::Close(_) => break,
                            other => {
                                if socket.send(other).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
            });
            Ok(resp)
        });

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let (addr, _handle) = oxihttp_server::Server::bind("127.0.0.1:0")
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .serve_with_addr(router)
            .await
            .expect("server bind");

        std::mem::forget(tx);
        tokio::time::sleep(Duration::from_millis(20)).await;
        addr
    });

    async fn ws_connect(addr: std::net::SocketAddr) -> TcpStream {
        let mut stream = TcpStream::connect(addr).await.expect("TCP connect");
        let ws_key = "dGhlIHNhbXBsZSBub25jZQ==";
        let request = format!(
            "GET /ws HTTP/1.1\r\n\
             Host: {addr}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: {ws_key}\r\n\
             Sec-WebSocket-Version: 13\r\n\
             \r\n"
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write upgrade request");
        let mut buf = Vec::with_capacity(512);
        loop {
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte).await.expect("read byte");
            buf.push(byte[0]);
            if buf.ends_with(b"\r\n\r\n") {
                break;
            }
            if buf.len() > 8192 {
                panic!("response headers too large");
            }
        }
        let resp = String::from_utf8_lossy(&buf);
        assert!(
            resp.starts_with("HTTP/1.1 101"),
            "expected 101 Switching Protocols, got: {resp}"
        );
        stream
    }

    async fn write_masked_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) {
        let mut frame = Vec::with_capacity(payload.len() + 10);
        frame.push(0x80 | opcode);
        let len = payload.len();
        if len <= 125 {
            frame.push(0x80 | len as u8);
        } else if len <= 0xFFFF {
            frame.push(0x80 | 126_u8);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127_u8);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
        let mask: [u8; 4] = [0x37, 0xfa, 0x21, 0x3d];
        frame.extend_from_slice(&mask);
        for (i, &b) in payload.iter().enumerate() {
            frame.push(b ^ mask[i % 4]);
        }
        stream.write_all(&frame).await.expect("write frame");
        stream.flush().await.expect("flush");
    }

    async fn read_server_frame(stream: &mut TcpStream) -> Vec<u8> {
        let mut header = [0u8; 2];
        stream.read_exact(&mut header).await.expect("read header");
        let len_byte = (header[1] & 0x7F) as usize;
        let payload_len: usize = match len_byte {
            0..=125 => len_byte,
            126 => {
                let mut b = [0u8; 2];
                stream.read_exact(&mut b).await.expect("read ext len16");
                u16::from_be_bytes(b) as usize
            }
            127 => {
                let mut b = [0u8; 8];
                stream.read_exact(&mut b).await.expect("read ext len64");
                u64::from_be_bytes(b) as usize
            }
            _ => unreachable!(),
        };
        let mut payload = vec![0u8; payload_len];
        stream.read_exact(&mut payload).await.expect("read payload");
        payload
    }

    let mut group = c.benchmark_group("ws_throughput");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    group.throughput(Throughput::Elements(1));
    group.bench_function("text_64b", |b| {
        let payload = b"A".repeat(64);
        let stream = Arc::new(Mutex::new(rt.block_on(ws_connect(addr))));
        b.to_async(&rt).iter(|| {
            let stream = Arc::clone(&stream);
            let payload = payload.clone();
            async move {
                let mut s = stream.lock().await;
                write_masked_frame(&mut s, 0x1, &payload).await;
                let _ = read_server_frame(&mut s).await;
            }
        });
    });

    group.throughput(Throughput::Bytes(1024));
    group.bench_function("binary_1kb", |b| {
        let payload = vec![0xABu8; 1024];
        let stream = Arc::new(Mutex::new(rt.block_on(ws_connect(addr))));
        b.to_async(&rt).iter(|| {
            let stream = Arc::clone(&stream);
            let payload = payload.clone();
            async move {
                let mut s = stream.lock().await;
                write_masked_frame(&mut s, 0x2, &payload).await;
                let _ = read_server_frame(&mut s).await;
            }
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Group 7: connection_accept_latency
// ---------------------------------------------------------------------------

fn bench_connection_accept_latency(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("tokio rt");

    let router = oxihttp_server::Router::new().get("/ping", |_req| async {
        oxihttp_server::response::text_response("pong")
    });
    let (addr, _shutdown_tx) = rt.block_on(spawn_test_server(router));

    let mut group = c.benchmark_group("connection_accept_latency");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("tcp_connect_to_first_byte", |b| {
        b.to_async(&rt).iter(|| async {
            let status = raw_http_get(addr, "/ping").await;
            black_box(status);
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Criterion entry point
// ---------------------------------------------------------------------------

#[cfg(feature = "static-files")]
criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(5));
    targets =
        bench_router_dispatch,
        bench_h1_throughput,
        bench_static_file_serving,
        bench_middleware_overhead,
        bench_json_handler,
        bench_connection_accept_latency
}

#[cfg(not(feature = "static-files"))]
criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(5));
    targets =
        bench_router_dispatch,
        bench_h1_throughput,
        bench_middleware_overhead,
        bench_json_handler,
        bench_connection_accept_latency
}

#[cfg(feature = "websocket")]
criterion_group! {
    name = ws_benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(5));
    targets = bench_ws_throughput
}

#[cfg(feature = "websocket")]
criterion_main!(benches, ws_benches);

#[cfg(not(feature = "websocket"))]
criterion_main!(benches);
