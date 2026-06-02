// Memory allocation benchmark under 10000 concurrent connections (M9 Block E).
//
// Custom harness (not criterion). Measures peak allocation while N TCP
// connections are open simultaneously.
//
// Run: cargo bench --bench server_memory -p oxihttp-server --all-features
//
// NOTE: On macOS the default file-descriptor limit is 256. Raise it before
// running the full 10000-connection scenario:
//
//   ulimit -n 65536
//
// The bench degrades automatically when the fd limit is too low: it halves
// the connection target until it fits within available fds.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Tracking allocator
// ---------------------------------------------------------------------------

struct TrackingAllocator;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let current = CURRENT.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            let mut peak = PEAK.load(Ordering::Relaxed);
            while current > peak {
                match PEAK.compare_exchange_weak(
                    peak,
                    current,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(p) => peak = p,
                }
            }
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn reset_peak() {
    PEAK.store(0, Ordering::SeqCst);
}

fn current_alloc() -> usize {
    CURRENT.load(Ordering::SeqCst)
}

fn peak_alloc() -> usize {
    PEAK.load(Ordering::SeqCst)
}

/// Return the target connection count, degraded to fit within available fds.
///
/// On Linux we parse `/proc/self/limits` to discover the soft NOFILE limit
/// without requiring `libc`. On all other platforms we default to a conservative
/// 1000 (safe even with macOS default 256 fd limit).
fn target_connections() -> usize {
    let target = 10_000_usize;

    // Parse /proc/self/limits (Linux only) — no external crate required.
    #[cfg(target_os = "linux")]
    if let Ok(contents) = std::fs::read_to_string("/proc/self/limits") {
        for line in contents.lines() {
            if line.starts_with("Max open files") {
                let cols: Vec<&str> = line.split_whitespace().collect();
                // Format: "Max open files   <soft>   <hard>   files"
                // The soft limit is in column index 3.
                if let Some(soft_str) = cols.get(3) {
                    if let Ok(soft) = soft_str.parse::<usize>() {
                        // Reserve ~256 fds for the process itself.
                        // Each connection uses 2 fds (accepted + client-side).
                        let available = soft.saturating_sub(256);
                        let max_conns = available / 2;
                        return target.min(max_conns).max(10);
                    }
                }
            }
        }
    }

    // macOS, Windows, other Unix, or parse failed: conservative default.
    target.min(1_000)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let connections = target_connections();
    if connections < 10_000 {
        eprintln!(
            "WARNING: fd limit too low to open 10000 connections; \
             running with {connections} connections instead.\n\
             Run 'ulimit -n 65536' to enable the full scenario."
        );
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("tokio runtime");

    // -------------------------------------------------------------------
    // 1. Baseline measurement (server running, no connections).
    // -------------------------------------------------------------------
    let baseline = rt.block_on(async {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let router = oxihttp_server::Router::new().get("/", |_req| async {
            oxihttp_server::response::text_response("OK")
        });

        let (_addr, _handle) = oxihttp_server::Server::bind("127.0.0.1:0")
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .serve_with_addr(router)
            .await
            .expect("server bind");

        tokio::time::sleep(Duration::from_millis(50)).await;
        let b = current_alloc();
        let _ = tx.send(());
        b
    });

    // -------------------------------------------------------------------
    // 2. Connection benchmark — open `connections` TCP connections concurrently.
    // -------------------------------------------------------------------
    reset_peak();

    let (peak, connections_alloc) = rt.block_on(async {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let router = oxihttp_server::Router::new().get("/", |_req| async {
            oxihttp_server::response::text_response("OK")
        });

        let (addr, _handle) = oxihttp_server::Server::bind("127.0.0.1:0")
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .serve_with_addr(router)
            .await
            .expect("server bind");

        tokio::time::sleep(Duration::from_millis(20)).await;
        let before = current_alloc();
        reset_peak();

        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(connections + 1));
        let mut handles = Vec::with_capacity(connections);

        for _ in 0..connections {
            let barrier_clone = std::sync::Arc::clone(&barrier);
            let h = tokio::spawn(async move {
                let stream = tokio::net::TcpStream::connect(addr)
                    .await
                    .expect("TCP connect");
                barrier_clone.wait().await;
                stream
            });
            handles.push(h);
        }

        barrier.wait().await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        let pk = peak_alloc();
        let after = current_alloc();

        drop(handles);
        let _ = tx.send(());

        let delta = after.saturating_sub(before);
        (pk, delta)
    });

    // -------------------------------------------------------------------
    // 3. Report
    // -------------------------------------------------------------------
    let peak_mb = peak as f64 / 1_048_576.0;
    let per_conn = connections_alloc.checked_div(connections).unwrap_or(0);

    println!(
        "Baseline allocation  : {:.2} MB ({} bytes)",
        baseline as f64 / 1_048_576.0,
        baseline
    );
    println!("Peak allocation      : {peak_mb:.2} MB ({peak} bytes)");
    println!("Connections measured : {connections}");
    println!(
        "Bytes per connection : {per_conn} bytes ({:.2} KB)",
        per_conn as f64 / 1024.0
    );
    println!("Peak allocation: {peak_mb:.2} MB ({per_conn} bytes per connection)");
}
