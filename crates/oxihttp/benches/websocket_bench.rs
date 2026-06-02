// WebSocket frame throughput benchmarks (M6 Block D).
//
// Feature-gated: only compiled when the `websocket` feature is enabled.
// Build: cargo bench --bench websocket_bench --features websocket
//
// When the `websocket` feature is absent the file reduces to `fn main() {}`.

#![cfg_attr(not(feature = "websocket"), allow(dead_code, unused_imports))]

#[cfg(feature = "websocket")]
use std::sync::Arc;
#[cfg(feature = "websocket")]
use std::time::Duration;
#[cfg(feature = "websocket")]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(feature = "websocket")]
use tokio::net::TcpStream;
#[cfg(feature = "websocket")]
use tokio::runtime::Runtime;
#[cfg(feature = "websocket")]
use tokio::sync::Mutex;

#[cfg(feature = "websocket")]
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

// ---------------------------------------------------------------------------
// Server helper
// ---------------------------------------------------------------------------

#[cfg(feature = "websocket")]
fn spawn_ws_server_sync(rt: &Runtime) -> std::net::SocketAddr {
    rt.block_on(async {
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

        // Leak the shutdown sender so the server lives for the entire bench run.
        std::mem::forget(tx);

        tokio::time::sleep(Duration::from_millis(20)).await;
        addr
    })
}

// ---------------------------------------------------------------------------
// Frame helpers (mirrors websocket_test.rs — not importable from test modules)
// ---------------------------------------------------------------------------

#[cfg(feature = "websocket")]
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

#[cfg(feature = "websocket")]
async fn write_masked_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) {
    let mut frame = Vec::with_capacity(payload.len() + 10);
    // FIN + opcode.
    frame.push(0x80 | opcode);
    // Mask-bit + length.
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
    // Deterministic masking key.
    let mask: [u8; 4] = [0x37, 0xfa, 0x21, 0x3d];
    frame.extend_from_slice(&mask);
    for (i, &b) in payload.iter().enumerate() {
        frame.push(b ^ mask[i % 4]);
    }
    stream.write_all(&frame).await.expect("write frame");
    stream.flush().await.expect("flush");
}

#[cfg(feature = "websocket")]
async fn read_server_frame(stream: &mut TcpStream) -> Vec<u8> {
    let mut header = [0u8; 2];
    stream
        .read_exact(&mut header)
        .await
        .expect("read frame header");
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

// ---------------------------------------------------------------------------
// Benchmark group
// ---------------------------------------------------------------------------

#[cfg(feature = "websocket")]
fn websocket_throughput(c: &mut Criterion) {
    let rt = Runtime::new().expect("bench runtime");
    let addr = spawn_ws_server_sync(&rt);

    let mut group = c.benchmark_group("websocket_throughput");

    // ── text_64b ──────────────────────────────────────────────────────────────
    group.throughput(Throughput::Elements(1));
    group.bench_function(BenchmarkId::new("text_64b", ""), |b| {
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

    // ── binary_1kb ────────────────────────────────────────────────────────────
    group.throughput(Throughput::Bytes(1024));
    group.bench_function(BenchmarkId::new("binary_1kb", ""), |b| {
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

    // ── binary_64kb ───────────────────────────────────────────────────────────
    group.throughput(Throughput::Bytes(65536));
    group.bench_function(BenchmarkId::new("binary_64kb", ""), |b| {
        let payload = vec![0xCDu8; 65536];
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
// Entrypoint: criterion or stub depending on feature flag
// ---------------------------------------------------------------------------

#[cfg(feature = "websocket")]
criterion_group!(benches, websocket_throughput);

#[cfg(feature = "websocket")]
criterion_main!(benches);

#[cfg(not(feature = "websocket"))]
fn main() {
    eprintln!("websocket_bench requires --features websocket");
}
