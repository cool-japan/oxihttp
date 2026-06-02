//! Criterion benchmarks for oxihttp-client (M9 Block E).
//!
//! Groups:
//!   - `get_h1_latency`              — single GET round-trip over plain HTTP/1.1
//!   - `pool_cold_vs_warm`           — cold-start vs warm connection pool
//!   - `large_body_throughput`       — 10 MB body download throughput
//!   - `request_builder_construction`— in-memory builder overhead (no network)
//!   - `redirect_chain`              — 1-, 5-, and 10-hop redirect follow time

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use http_body_util::Full;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

// ---------------------------------------------------------------------------
// Plain-HTTP echo server — spawned once, reused across all iterations.
// ---------------------------------------------------------------------------

async fn spawn_echo_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bench bind echo server");
    let addr = listener.local_addr().expect("bench echo local addr");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(stream),
                        service_fn(|_req: hyper::Request<hyper::body::Incoming>| async {
                            Ok::<_, Infallible>(hyper::Response::new(Full::new(
                                Bytes::from_static(b"OK"),
                            )))
                        }),
                    )
                    .await;
            });
        }
    });
    addr
}

// ---------------------------------------------------------------------------
// Large-body server — returns 10 MB of zeroed bytes on every request.
// ---------------------------------------------------------------------------

async fn spawn_large_body_server() -> SocketAddr {
    const BODY_SIZE: usize = 10 * 1024 * 1024; // 10 MB
    let body_data = Arc::new(vec![0u8; BODY_SIZE]);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bench bind large body server");
    let addr = listener.local_addr().expect("bench large body local addr");

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let data = Arc::clone(&body_data);
            tokio::spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(stream),
                        service_fn(move |_req: hyper::Request<hyper::body::Incoming>| {
                            let d = Arc::clone(&data);
                            async move {
                                let body = Bytes::copy_from_slice(&d);
                                Ok::<_, Infallible>(hyper::Response::new(Full::new(body)))
                            }
                        }),
                    )
                    .await;
            });
        }
    });
    addr
}

// ---------------------------------------------------------------------------
// Redirect chain server.
//
// Requests to `/N` (N > 0) return `302 Location: /<N-1>`.
// Requests to `/0` return `200 OK`.
// ---------------------------------------------------------------------------

async fn spawn_redirect_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bench bind redirect server");
    let addr = listener.local_addr().expect("bench redirect local addr");

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(stream),
                        service_fn(|req: hyper::Request<hyper::body::Incoming>| async move {
                            let path = req.uri().path();
                            // Parse the hop count from the path, e.g. "/5" -> 5
                            let hops: u32 = path.trim_start_matches('/').parse().unwrap_or(0);

                            if hops == 0 {
                                Ok::<_, Infallible>(hyper::Response::new(Full::new(
                                    Bytes::from_static(b"done"),
                                )))
                            } else {
                                let next = hops - 1;
                                let location = format!("/{next}");
                                let resp = hyper::Response::builder()
                                    .status(302)
                                    .header("location", location)
                                    .body(Full::new(Bytes::new()))
                                    .expect("redirect response build");
                                Ok::<_, Infallible>(resp)
                            }
                        }),
                    )
                    .await;
            });
        }
    });
    addr
}

// ---------------------------------------------------------------------------
// Bench 1 — GET H1 latency
// ---------------------------------------------------------------------------

fn bench_get_h1_latency(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("h1 latency rt");
    let addr = rt.block_on(spawn_echo_server());
    let url = format!("http://{addr}/");

    let client = oxihttp_client::Client::builder()
        .build()
        .expect("h1 latency client build");

    let mut group = c.benchmark_group("get_h1_latency");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(5));

    group.bench_function("h1_plaintext", |b| {
        b.to_async(&rt).iter(|| {
            let c = &client;
            let u = url.as_str();
            async move {
                let resp = c
                    .get(u)
                    .expect("GET url parse")
                    .send()
                    .await
                    .expect("GET send");
                std::hint::black_box(resp);
            }
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Bench 2 — Connection pool: cold-start vs warm-pool
// ---------------------------------------------------------------------------

fn bench_pool_cold_vs_warm(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("pool rt");
    let addr = rt.block_on(spawn_echo_server());
    let url = format!("http://{addr}/");

    let mut group = c.benchmark_group("pool_cold_vs_warm");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(5));

    // cold: build a fresh client each iteration, fire a single GET.
    {
        let u = url.clone();
        group.bench_function("cold_new_client", |b| {
            b.to_async(&rt).iter(|| {
                let u = u.as_str();
                async move {
                    let client = oxihttp_client::Client::builder()
                        .build()
                        .expect("cold client build");
                    let resp = client
                        .get(u)
                        .expect("GET url parse")
                        .send()
                        .await
                        .expect("cold GET send");
                    std::hint::black_box(resp);
                }
            });
        });
    }

    // warm: one shared client, 10 sequential GETs — amortised cost per request.
    {
        let warm_client = oxihttp_client::Client::builder()
            .pool_max_idle_per_host(4)
            .build()
            .expect("warm client build");
        let u = url.clone();
        group.bench_function("warm_pool_10_seq", |b| {
            b.to_async(&rt).iter(|| {
                let c = &warm_client;
                let u = u.as_str();
                async move {
                    for _ in 0..10 {
                        let resp = c
                            .get(u)
                            .expect("GET url parse")
                            .send()
                            .await
                            .expect("warm GET send");
                        std::hint::black_box(resp);
                    }
                }
            });
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Bench 3 — Large body throughput (10 MB download)
// ---------------------------------------------------------------------------

fn bench_large_body_throughput(c: &mut Criterion) {
    const BODY_BYTES: u64 = 10 * 1024 * 1024;

    let rt = tokio::runtime::Runtime::new().expect("large body rt");
    let addr = rt.block_on(spawn_large_body_server());
    let url = format!("http://{addr}/");

    let client = oxihttp_client::Client::builder()
        .build()
        .expect("large body client build");

    let mut group = c.benchmark_group("large_body_throughput");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));
    group.throughput(Throughput::Bytes(BODY_BYTES));

    group.bench_function("10mb_body_bytes", |b| {
        b.to_async(&rt).iter(|| {
            let c = &client;
            let u = url.as_str();
            async move {
                let resp = c
                    .get(u)
                    .expect("GET url parse")
                    .send()
                    .await
                    .expect("large body GET send");
                let bytes = resp.body_bytes().await.expect("body_bytes collect");
                std::hint::black_box(bytes);
            }
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Bench 4 — RequestBuilder construction (in-memory, no network)
// ---------------------------------------------------------------------------

fn bench_request_builder_construction(c: &mut Criterion) {
    let client = oxihttp_client::Client::builder()
        .build()
        .expect("builder bench client build");

    let mut group = c.benchmark_group("request_builder_construction");
    group.sample_size(100);

    #[derive(serde::Serialize)]
    struct Payload<'a> {
        key: &'a str,
        value: u32,
    }

    group.bench_function("post_with_json_headers", |b| {
        b.iter(|| {
            let rb = client
                .post("http://127.0.0.1:1/api/v1/resource")
                .expect("POST url parse")
                .header("x-request-id", "bench-run-id")
                .expect("header x-request-id")
                .header("x-trace-id", "trace-abc123")
                .expect("header x-trace-id")
                .json(&Payload {
                    key: "benchkey",
                    value: 42,
                })
                .expect("json body serialize");
            std::hint::black_box(rb);
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Bench 5 — Redirect chain (1, 5, 10 hops)
// ---------------------------------------------------------------------------

fn bench_redirect_chain(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("redirect chain rt");
    let addr = rt.block_on(spawn_redirect_server());

    let client = oxihttp_client::Client::builder()
        .build()
        .expect("redirect chain client build");

    let mut group = c.benchmark_group("redirect_chain");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(5));

    for hops in [1u32, 5, 10] {
        let url = format!("http://{addr}/{hops}");
        group.bench_with_input(BenchmarkId::from_parameter(hops), &url, |b, url| {
            b.to_async(&rt).iter(|| {
                let c = &client;
                let u = url.as_str();
                async move {
                    let resp = c
                        .get(u)
                        .expect("GET url parse")
                        .send()
                        .await
                        .expect("redirect GET send");
                    std::hint::black_box(resp);
                }
            });
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

criterion_group!(
    client_benches,
    bench_get_h1_latency,
    bench_pool_cold_vs_warm,
    bench_large_body_throughput,
    bench_request_builder_construction,
    bench_redirect_chain,
);
criterion_main!(client_benches);
