//! Benchmark: facade overhead vs direct construction (M9 Block E).
//!
//! Groups:
//!   - `facade_overhead`      — facade `oxihttp::get()` vs explicit `Client::builder()…send()`
//!   - `client_construction`  — default `Client::builder().build()` in-memory cost

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, Criterion};
use http_body_util::Full;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpListener;

// ---------------------------------------------------------------------------
// Minimal plain-HTTP echo server (per-group, not per-iteration)
// ---------------------------------------------------------------------------

async fn spawn_echo_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("facade bench bind");
    let addr = listener.local_addr().expect("facade bench local addr");
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
// facade_overhead group
//
// Measures that the `oxihttp::get()` convenience wrapper adds no observable
// overhead compared to the equivalent explicit builder chain.  Both code
// paths compile to the same sequence of calls; this bench acts as a
// regression guard rather than expecting a meaningful delta.
// ---------------------------------------------------------------------------

fn bench_facade_get_overhead(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("facade overhead rt");
    let addr = rt.block_on(spawn_echo_server());
    let url = format!("http://{addr}/");

    let mut group = c.benchmark_group("facade_overhead");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(5));

    // Bench A: facade convenience — `oxihttp::get(url)` creates a fresh
    // one-shot client internally (see lib.rs lines 89-93).
    {
        let u = url.clone();
        group.bench_function("facade_get", |b| {
            b.to_async(&rt).iter(|| {
                let u = u.as_str();
                async move {
                    let resp = oxihttp::get(u).await.expect("facade GET");
                    std::hint::black_box(resp);
                }
            });
        });
    }

    // Bench B: explicit builder chain — identical code path, no extra wrapping.
    {
        let u = url.clone();
        group.bench_function("direct_client_get", |b| {
            b.to_async(&rt).iter(|| {
                let u = u.as_str();
                async move {
                    let client = oxihttp::Client::builder()
                        .build()
                        .expect("direct client build");
                    let resp = client
                        .get(u)
                        .expect("GET")
                        .send()
                        .await
                        .expect("direct GET");
                    std::hint::black_box(resp);
                }
            });
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// client_construction group
//
// Purely in-memory: measures the cost of `Client::builder().build()` including
// TLS config initialisation and connection-pool setup — no network involved.
// ---------------------------------------------------------------------------

fn bench_client_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("client_construction");
    // Default sample size (100) is fine; this is a cheap in-memory operation.

    group.bench_function("builder_build", |b| {
        b.iter(|| std::hint::black_box(oxihttp::Client::builder().build().expect("client build")));
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

criterion_group!(
    facade_benches,
    bench_facade_get_overhead,
    bench_client_construction,
);
criterion_main!(facade_benches);
