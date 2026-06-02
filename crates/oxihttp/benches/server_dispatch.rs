//! Benchmark: server router dispatch latency and middleware overhead (M6 Block C).
//!
//! Group 1 — `router_dispatch`: pure CPU, no I/O.
//!   Parameterised by route count (10, 100, 1000).
//!   Sub-benchmarks: worst_case, best_case, miss.
//!
//! Group 2 — `middleware_overhead`: full TCP round-trip.
//!   Sub-benchmarks: no_middleware, cors_only, cors_body_limit_rate,
//!   tower_5_layers (behind `#[cfg(feature = "tower")]`).

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use http::Method;
use http_body_util::Full;
use oxihttp_core::OxiHttpError;
use std::hint::black_box;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Shared dummy handler for router construction.
// ---------------------------------------------------------------------------

async fn dummy_handler(
    _req: oxihttp_server::router::Request,
) -> Result<hyper::Response<Full<Bytes>>, OxiHttpError> {
    Ok(hyper::Response::new(Full::new(Bytes::from_static(b""))))
}

// ---------------------------------------------------------------------------
// Helper: build a router with `n` GET routes.
// Routes are named /route_0 … /route_{n-1}.
// ---------------------------------------------------------------------------

fn build_router(n: usize) -> oxihttp_server::Router {
    let mut router = oxihttp_server::Router::new();
    for i in 0..n {
        let path = format!("/route_{i}");
        // Box the static string so we can move into the closure.
        router = router.get(Box::leak(path.into_boxed_str()), dummy_handler);
    }
    router
}

// ---------------------------------------------------------------------------
// Helper: spawn a test server and return (addr, shutdown_tx).
// Must be called inside an async context / block_on.
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
// Group 1: router_dispatch (sync — uses Router::resolve, no I/O)
// ---------------------------------------------------------------------------

fn bench_router_dispatch(c: &mut Criterion) {
    let mut group = c.benchmark_group("router_dispatch");

    for &n in &[10usize, 100, 1000] {
        let router = build_router(n);
        let method = Method::GET;

        // best_case: match /route_0 (first route in the table)
        let best_target = "/route_0".to_string();
        group.bench_with_input(BenchmarkId::new("best_case", n), &n, |b, _| {
            b.iter(|| {
                black_box(router.resolve(black_box(&method), black_box(best_target.as_str())))
            });
        });

        // worst_case: match /route_{n-1} (last route — full scan)
        let worst_target = format!("/route_{}", n - 1);
        group.bench_with_input(BenchmarkId::new("worst_case", n), &n, |b, _| {
            b.iter(|| {
                black_box(router.resolve(black_box(&method), black_box(worst_target.as_str())))
            });
        });

        // miss: no route matches (full scan, returns None)
        let miss_target = "/nonexistent".to_string();
        group.bench_with_input(BenchmarkId::new("miss", n), &n, |b, _| {
            b.iter(|| {
                black_box(router.resolve(black_box(&method), black_box(miss_target.as_str())))
            });
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Group 2: middleware_overhead (async TCP round-trip)
// ---------------------------------------------------------------------------

fn bench_middleware_overhead(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("tokio rt");

    let mut group = c.benchmark_group("middleware_overhead");
    // One iteration per benchmark is enough for round-trip cost measurement;
    // criterion will run many iterations automatically.
    group.measurement_time(Duration::from_secs(10));

    // --- no_middleware ---
    {
        let router = oxihttp_server::Router::new().get("/bench", |_req| async {
            oxihttp_server::response::text_response("ok")
        });
        let (addr, _shutdown_tx) = rt.block_on(spawn_test_server(router));
        let client = oxihttp_client::ClientBuilder::new()
            .build()
            .expect("client");
        let url = format!("http://{addr}/bench");

        group.bench_function("no_middleware", |b| {
            b.to_async(&rt).iter(|| async {
                let resp = client
                    .get(black_box(url.as_str()))
                    .expect("GET builder")
                    .send()
                    .await
                    .expect("send");
                black_box(resp.status());
            });
        });
    }

    // --- cors_only ---
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
        let client = oxihttp_client::ClientBuilder::new()
            .build()
            .expect("client");
        let url = format!("http://{addr}/bench");

        group.bench_function("cors_only", |b| {
            b.to_async(&rt).iter(|| async {
                let resp = client
                    .get(black_box(url.as_str()))
                    .expect("GET builder")
                    .send()
                    .await
                    .expect("send");
                black_box(resp.status());
            });
        });
    }

    // --- cors_body_limit_rate (3 layers) ---
    {
        let router = oxihttp_server::Router::new().get("/bench", |_req| async {
            oxihttp_server::response::text_response("ok")
        });
        let (addr, _shutdown_tx) = rt.block_on(async {
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            let (addr, _handle) = oxihttp_server::Server::bind("127.0.0.1:0")
                .with_cors(oxihttp_server::CorsConfig::permissive())
                .with_body_limit(1024 * 1024) // 1 MB
                .with_rate_limiter(oxihttp_server::RateLimiter::new(10_000, 10_000.0))
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .serve_with_addr(router)
                .await
                .expect("server bind");
            tokio::time::sleep(Duration::from_millis(10)).await;
            (addr, tx)
        });
        let client = oxihttp_client::ClientBuilder::new()
            .build()
            .expect("client");
        let url = format!("http://{addr}/bench");

        group.bench_function("cors_body_limit_rate", |b| {
            b.to_async(&rt).iter(|| async {
                let resp = client
                    .get(black_box(url.as_str()))
                    .expect("GET builder")
                    .send()
                    .await
                    .expect("send");
                black_box(resp.status());
            });
        });
    }

    // --- tower_5_layers (feature-gated) ---
    #[cfg(feature = "tower")]
    {
        use oxihttp_server::RequestIdLayer;

        let router = oxihttp_server::Router::new().get("/bench", |_req| async {
            oxihttp_server::response::text_response("ok")
        });
        let (addr, _shutdown_tx) = rt.block_on(async {
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            let (addr, _handle) = oxihttp_server::Server::bind("127.0.0.1:0")
                .with_layer(RequestIdLayer)
                .with_layer(RequestIdLayer)
                .with_layer(RequestIdLayer)
                .with_layer(RequestIdLayer)
                .with_layer(RequestIdLayer)
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .serve_with_addr(router)
                .await
                .expect("server bind");
            tokio::time::sleep(Duration::from_millis(10)).await;
            (addr, tx)
        });
        let client = oxihttp_client::ClientBuilder::new()
            .build()
            .expect("client");
        let url = format!("http://{addr}/bench");

        group.bench_function("tower_5_layers", |b| {
            b.to_async(&rt).iter(|| async {
                let resp = client
                    .get(black_box(url.as_str()))
                    .expect("GET builder")
                    .send()
                    .await
                    .expect("send");
                black_box(resp.status());
            });
        });
    }

    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(5));
    targets = bench_router_dispatch, bench_middleware_overhead
}
criterion_main!(benches);
