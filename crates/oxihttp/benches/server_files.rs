//! Benchmark: static file serving throughput (M6 Block C).
//!
//! Group — `static_file_serving`:
//!   Parameterised by file size (1 KB, 100 KB, 1 MB).
//!   Reports Throughput::Bytes so criterion shows MB/s.
//!
//! The whole benchmark is feature-gated with `static-files`.

#[cfg(feature = "static-files")]
mod bench_impl {
    use criterion::{BenchmarkId, Criterion, Throughput};
    use http::HeaderMap;
    use oxihttp_server::ServeDir;
    use std::hint::black_box;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;

    // -----------------------------------------------------------------------
    // File creation helper
    // -----------------------------------------------------------------------

    fn create_test_file(dir: &std::path::Path, name: &str, size: usize) {
        let content = vec![b'x'; size];
        std::fs::write(dir.join(name), &content).expect("write bench file");
    }

    // -----------------------------------------------------------------------
    // Server spawn helper — matches static_files_test.rs pattern.
    // Returns the bound SocketAddr; the server task runs for process lifetime.
    // -----------------------------------------------------------------------

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
                            let headers: HeaderMap = req.headers().clone();

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

    // -----------------------------------------------------------------------
    // Benchmark group
    // -----------------------------------------------------------------------

    pub fn bench_static_file_serving(c: &mut Criterion) {
        let rt = tokio::runtime::Runtime::new().expect("tokio rt");

        // Build unique temp dir for this bench run (PID + nanos).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        let tmp_dir = std::env::temp_dir().join(format!(
            "oxihttp_bench_files_{}_{}",
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&tmp_dir).expect("create bench temp dir");

        // File sizes to benchmark.
        let sizes: &[(usize, &str)] = &[
            (1024, "file_1k.bin"),
            (102_400, "file_100k.bin"),
            (1_048_576, "file_1m.bin"),
        ];

        for &(size, name) in sizes {
            create_test_file(&tmp_dir, name, size);
        }

        // Spawn a single server (shared across all file-size benchmarks).
        let addr = rt.block_on(spawn_static_server(ServeDir::new(&tmp_dir)));

        let client = oxihttp_client::ClientBuilder::new()
            .build()
            .expect("client");

        let mut group = c.benchmark_group("static_file_serving");
        group.measurement_time(Duration::from_secs(10));

        for &(size, name) in sizes {
            group.throughput(Throughput::Bytes(size as u64));
            let url = format!("http://{addr}/{name}");

            group.bench_with_input(BenchmarkId::from_parameter(size), &url, |b, url| {
                b.to_async(&rt).iter(|| async {
                    let resp = client
                        .get(black_box(url.as_str()))
                        .expect("GET builder")
                        .send()
                        .await
                        .expect("send");
                    // Drain the body to measure full transfer cost.
                    let body = resp.body_bytes().await.expect("body bytes");
                    black_box(body.len());
                });
            });
        }

        group.finish();

        // Clean up temp directory (best-effort).
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
}

// ---------------------------------------------------------------------------
// Criterion entry point (only when the feature is active)
// ---------------------------------------------------------------------------

#[cfg(feature = "static-files")]
use criterion::{criterion_group, criterion_main};

#[cfg(feature = "static-files")]
criterion_group! {
    name = benches;
    config = criterion::Criterion::default()
        .warm_up_time(std::time::Duration::from_secs(1))
        .measurement_time(std::time::Duration::from_secs(5));
    targets = bench_impl::bench_static_file_serving
}

#[cfg(feature = "static-files")]
criterion_main!(benches);

// Fallback main when the `static-files` feature is not enabled.
#[cfg(not(feature = "static-files"))]
fn main() {}
