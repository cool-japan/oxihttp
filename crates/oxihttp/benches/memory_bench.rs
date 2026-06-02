// Memory allocation benchmark — 1000 concurrent connections (M6 Block D).
//
// Custom harness (not criterion). Reports peak allocation and bytes-per-connection.
// Run: cargo bench --bench memory_bench --all-features

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
// Main
// ---------------------------------------------------------------------------

const CONNECTIONS: usize = 1000;

fn reset_peak() {
    PEAK.store(0, Ordering::SeqCst);
    // Give the allocator a moment to settle.
}

fn current_alloc() -> usize {
    CURRENT.load(Ordering::SeqCst)
}

fn peak_alloc() -> usize {
    PEAK.load(Ordering::SeqCst)
}

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("tokio runtime");

    // -----------------------------------------------------------------------
    // 1. Baseline measurement (no connections open, just the server running).
    // -----------------------------------------------------------------------
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

        // Allow server to fully initialize.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let baseline = current_alloc();
        let _ = tx.send(());
        baseline
    });

    // -----------------------------------------------------------------------
    // 2. Connection benchmark — open CONNECTIONS TCP connections concurrently.
    // -----------------------------------------------------------------------
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

        // Record allocation before opening connections.
        let before = current_alloc();
        reset_peak();

        // Open CONNECTIONS TCP connections concurrently using a barrier to
        // ensure they are all in-flight at the same time.
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(CONNECTIONS + 1));
        let mut handles = Vec::with_capacity(CONNECTIONS);

        for _ in 0..CONNECTIONS {
            let barrier_clone = std::sync::Arc::clone(&barrier);
            let h = tokio::spawn(async move {
                let stream = tokio::net::TcpStream::connect(addr)
                    .await
                    .expect("TCP connect");
                // All connections wait at the barrier together.
                barrier_clone.wait().await;
                // Keep the stream alive until we've measured peak.
                stream
            });
            handles.push(h);
        }

        // Orchestrator arrives at barrier to release all tasks simultaneously.
        barrier.wait().await;

        // Allow the server to accept and process all connections.
        tokio::time::sleep(Duration::from_millis(100)).await;

        let peak = peak_alloc();
        let after = current_alloc();

        // Drop all connections.
        drop(handles);
        let _ = tx.send(());

        let connections_alloc = after.saturating_sub(before);
        (peak, connections_alloc)
    });

    // -----------------------------------------------------------------------
    // 3. Report
    // -----------------------------------------------------------------------
    let peak_mb = peak as f64 / 1_048_576.0;
    let per_conn = connections_alloc.checked_div(CONNECTIONS).unwrap_or(0);

    println!(
        "Baseline allocation  : {:.2} MB ({} bytes)",
        baseline as f64 / 1_048_576.0,
        baseline
    );
    println!("Peak allocation      : {peak_mb:.2} MB ({peak} bytes)");
    println!("Connections in use   : {CONNECTIONS}");
    println!(
        "Bytes per connection : {per_conn} bytes ({:.2} KB)",
        per_conn as f64 / 1024.0
    );
    println!("Peak allocation: {peak_mb:.2} MB ({per_conn} bytes per connection)");
}
