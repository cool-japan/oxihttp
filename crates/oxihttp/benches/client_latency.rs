//! Benchmark: client round-trip latency (M6 Block B).
//!
//! Groups:
//!   - `get_latency`       — single GET RTT for H1/HTTPS/H2
//!   - `request_throughput`— H1 sequential request rate
//!   - `pool_cost`         — cold-start vs warm-pool comparison

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
// Plain-HTTP echo server
// ---------------------------------------------------------------------------

async fn spawn_echo_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bench bind");
    let addr = listener.local_addr().expect("bench local addr");
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
// TLS echo server (HTTPS / H2)
// ---------------------------------------------------------------------------

#[cfg(feature = "tls")]
struct TlsServer {
    addr: SocketAddr,
    cert_der: Vec<u8>,
}

#[cfg(feature = "tls")]
async fn spawn_tls_echo_server(alpn: &[&str]) -> TlsServer {
    use oxitls::rcgen_bridge::generate_self_signed_ed25519;
    use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use tokio_rustls::TlsAcceptor;

    let ck = generate_self_signed_ed25519(&["localhost"]).expect("bench cert gen");
    let cert_der = ck.cert_der.clone();

    let cert = CertificateDer::from(ck.cert_der.clone());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.pkcs8_der.clone()));

    let mut server_cfg = oxitls::tls13::ServerBuilder::new()
        .with_der_cert_and_key(vec![cert], key)
        .build()
        .expect("bench TLS server config");

    // Set ALPN protocols on the ServerConfig.
    server_cfg.alpn_protocols = alpn.iter().map(|s| s.as_bytes().to_vec()).collect();

    let acceptor = TlsAcceptor::from(std::sync::Arc::new(server_cfg));

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bench TLS bind");
    let addr = listener.local_addr().expect("bench TLS local addr");

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(stream).await else {
                    return;
                };
                // Check negotiated ALPN
                let (_, server_conn) = tls.get_ref();
                let is_h2 = server_conn
                    .alpn_protocol()
                    .map(|p| p == b"h2")
                    .unwrap_or(false);

                let io = TokioIo::new(tls);
                if is_h2 {
                    let _ = hyper::server::conn::http2::Builder::new(
                        hyper_util::rt::TokioExecutor::new(),
                    )
                    .serve_connection(
                        io,
                        service_fn(|_req: hyper::Request<hyper::body::Incoming>| async {
                            Ok::<_, Infallible>(hyper::Response::new(Full::new(
                                Bytes::from_static(b"OK"),
                            )))
                        }),
                    )
                    .await;
                } else {
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(
                            io,
                            service_fn(|_req: hyper::Request<hyper::body::Incoming>| async {
                                Ok::<_, Infallible>(hyper::Response::new(Full::new(
                                    Bytes::from_static(b"OK"),
                                )))
                            }),
                        )
                        .await;
                }
            });
        }
    });

    TlsServer { addr, cert_der }
}

// ---------------------------------------------------------------------------
// get_latency group
// ---------------------------------------------------------------------------

fn bench_get_latency(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("latency rt");

    // H1 server — spawn once, reuse across iterations.
    let h1_addr = rt.block_on(spawn_echo_server());
    let h1_url = format!("http://{h1_addr}/");

    let mut group = c.benchmark_group("get_latency");
    group.measurement_time(Duration::from_secs(5));

    // --- h1_plaintext ---
    {
        let client = oxihttp_client::Client::builder()
            .build()
            .expect("h1 client build");
        let url = h1_url.clone();
        group.bench_function("h1_plaintext", |b| {
            b.to_async(&rt).iter(|| {
                let c = &client;
                let u = url.as_str();
                async move {
                    let resp = c.get(u).expect("GET").send().await.expect("GET send");
                    std::hint::black_box(resp);
                }
            });
        });
    }

    // --- https_tls13 ---
    #[cfg(feature = "tls")]
    {
        let tls_srv = rt.block_on(spawn_tls_echo_server(&["http/1.1"]));
        let tls_url = format!("https://localhost:{}/", tls_srv.addr.port());
        let https_client = oxihttp_client::Client::builder()
            .with_trusted_cert_der(tls_srv.cert_der.clone())
            .build_https()
            .expect("https client build");

        group.bench_function("https_tls13", |b| {
            b.to_async(&rt).iter(|| {
                let c = &https_client;
                let u = tls_url.as_str();
                async move {
                    let resp = c.get(u).expect("GET").send().await.expect("https GET");
                    std::hint::black_box(resp);
                }
            });
        });
    }

    // --- http2 ---
    #[cfg(feature = "tls")]
    {
        let h2_srv = rt.block_on(spawn_tls_echo_server(&["h2", "http/1.1"]));
        let h2_url = format!("https://localhost:{}/", h2_srv.addr.port());
        let h2_client = oxihttp_client::Client::builder()
            .with_trusted_cert_der(h2_srv.cert_der.clone())
            .with_alpn(&["h2", "http/1.1"])
            .build_https()
            .expect("h2 client build");

        group.bench_function("http2", |b| {
            b.to_async(&rt).iter(|| {
                let c = &h2_client;
                let u = h2_url.as_str();
                async move {
                    let resp = c.get(u).expect("GET").send().await.expect("h2 GET");
                    std::hint::black_box(resp);
                }
            });
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// request_throughput group
// ---------------------------------------------------------------------------

fn bench_request_throughput(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("throughput rt");
    let addr = rt.block_on(spawn_echo_server());
    let url = format!("http://{addr}/");

    let client = oxihttp_client::Client::builder()
        .pool_max_idle_per_host(8)
        .build()
        .expect("throughput client build");

    let mut group = c.benchmark_group("request_throughput");
    group.throughput(Throughput::Elements(1));
    group.measurement_time(Duration::from_secs(5));

    group.bench_function("h1_sustained", |b| {
        b.to_async(&rt).iter_custom(|iters| {
            let c = &client;
            let u = url.as_str();
            async move {
                let start = Instant::now();
                for _ in 0..iters {
                    let resp = c.get(u).expect("GET").send().await.expect("sustained GET");
                    std::hint::black_box(resp);
                }
                start.elapsed()
            }
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// pool_cost group
// ---------------------------------------------------------------------------

fn bench_pool_cost(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("pool rt");
    let addr = rt.block_on(spawn_echo_server());
    let url = format!("http://{addr}/");

    let mut group = c.benchmark_group("pool_cost");
    group.measurement_time(Duration::from_secs(5));

    // cold_start: new Client created and used per iteration.
    {
        let u = url.clone();
        group.bench_function("cold_start", |b| {
            b.to_async(&rt).iter(|| {
                let u = u.as_str();
                async move {
                    let client = oxihttp_client::Client::builder()
                        .build()
                        .expect("cold client build");
                    let resp = client.get(u).expect("GET").send().await.expect("cold GET");
                    std::hint::black_box(resp);
                }
            });
        });
    }

    // warm_pool: one shared Client, sequential GETs.
    {
        let warm_client = oxihttp_client::Client::builder()
            .pool_max_idle_per_host(4)
            .build()
            .expect("warm client build");
        let u = url.clone();
        group.bench_function("warm_pool", |b| {
            b.to_async(&rt).iter(|| {
                let c = &warm_client;
                let u = u.as_str();
                async move {
                    let resp = c.get(u).expect("GET").send().await.expect("warm GET");
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
    latency_benches,
    bench_get_latency,
    bench_request_throughput,
    bench_pool_cost,
);
criterion_main!(latency_benches);
