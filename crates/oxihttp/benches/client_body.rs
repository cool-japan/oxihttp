//! Benchmark: client large-body throughput (M6 Block B).
//!
//! Group `body_throughput` — measures how fast the client can consume
//! pre-allocated response bodies of 10 MiB and 100 MiB.

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use http_body_util::Full;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;

// ---------------------------------------------------------------------------
// Body server
// ---------------------------------------------------------------------------

/// Spawn a hyper server that always returns a body of exactly `size` bytes.
/// The body is pre-allocated once; each response clones the `Bytes` handle
/// (refcount bump, no data copy).
async fn spawn_body_server(size: usize) -> SocketAddr {
    let body = Bytes::from(vec![0u8; size]);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("body bench bind");
    let addr = listener.local_addr().expect("body bench local addr");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let body = body.clone();
            tokio::spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(stream),
                        service_fn(move |_req: hyper::Request<hyper::body::Incoming>| {
                            let b = body.clone();
                            async move { Ok::<_, Infallible>(hyper::Response::new(Full::new(b))) }
                        }),
                    )
                    .await;
            });
        }
    });
    addr
}

// ---------------------------------------------------------------------------
// body_throughput group
// ---------------------------------------------------------------------------

fn bench_body_throughput(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("body rt");

    const MB_10: usize = 10 * 1024 * 1024;
    const MB_100: usize = 100 * 1024 * 1024;

    let addr_10mb = rt.block_on(spawn_body_server(MB_10));
    let addr_100mb = rt.block_on(spawn_body_server(MB_100));

    let client = oxihttp_client::Client::builder()
        .pool_max_idle_per_host(2)
        .build()
        .expect("body client build");

    let mut group = c.benchmark_group("body_throughput");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));

    // ---- 10 MiB ----
    {
        let url = format!("http://{addr_10mb}/");
        group.throughput(Throughput::Bytes(MB_10 as u64));
        group.bench_function("10mb", |b| {
            b.to_async(&rt).iter_custom(|iters| {
                let c = &client;
                let u = url.as_str();
                async move {
                    let start = Instant::now();
                    for _ in 0..iters {
                        let resp = c.get(u).expect("GET").send().await.expect("10mb GET");
                        let bytes = resp.body_bytes().await.expect("10mb body");
                        std::hint::black_box(bytes);
                    }
                    start.elapsed()
                }
            });
        });
    }

    // ---- 100 MiB ----
    {
        let url = format!("http://{addr_100mb}/");
        group.throughput(Throughput::Bytes(MB_100 as u64));
        group.bench_function("100mb", |b| {
            b.to_async(&rt).iter_custom(|iters| {
                let c = &client;
                let u = url.as_str();
                async move {
                    let start = Instant::now();
                    for _ in 0..iters {
                        let resp = c.get(u).expect("GET").send().await.expect("100mb GET");
                        let bytes = resp.body_bytes().await.expect("100mb body");
                        std::hint::black_box(bytes);
                    }
                    start.elapsed()
                }
            });
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

criterion_group!(body_benches, bench_body_throughput);
criterion_main!(body_benches);
