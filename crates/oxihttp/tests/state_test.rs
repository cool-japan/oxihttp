//! Tests for State<T> and Extension<T> extractors.
//!
//! Verifies that:
//! - `Router::with_state::<T>()` injects `Arc<T>` into each request's extensions.
//! - `Request::state::<T>()` retrieves the injected `Arc<T>`.
//! - `Request::extension::<T>()` retrieves arbitrary per-request values.
//! - `Request::extensions_mut()` allows handlers to attach and read values.
//! - Nested routers without their own state inherit the parent's state.
//! - Nested routers with their own state use that instead of the parent's.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use oxihttp_core::OxiHttpError;
use oxihttp_server::router::Request;
use oxihttp_server::{Router, Server};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Spawn an ephemeral test server and return its bound address together with a
/// one-shot shutdown sender.  The server stops when the sender is dropped.
async fn spawn_test_server(
    router: Router,
) -> (std::net::SocketAddr, tokio::sync::oneshot::Sender<()>) {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let (addr, _handle) = Server::bind("127.0.0.1:0")
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
// Application state types used across tests
// ---------------------------------------------------------------------------

/// Shared counter state — Clone is required by `with_state`, but we wrap in
/// Arc so the "real" counter survives the clone.
#[derive(Clone)]
struct CounterState {
    counter: Arc<AtomicU32>,
}

/// Simple string config state.
#[derive(Clone)]
struct AppConfig {
    greeting: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Handler can read `Arc<CounterState>` injected via `Router::with_state`.
/// Two sequential requests both see the same counter and increment it, proving
/// that the *same* `Arc` (not a clone of the inner value) is injected.
#[tokio::test]
async fn test_state_injection() {
    let shared_counter = Arc::new(AtomicU32::new(0));
    let state = CounterState {
        counter: Arc::clone(&shared_counter),
    };

    let router = Router::new()
        .with_state(state)
        .get("/count", |req: Request| async move {
            let s = req
                .state::<CounterState>()
                .ok_or_else(|| OxiHttpError::Server("missing state".into()))?;
            let prev = s.counter.fetch_add(1, Ordering::SeqCst);
            oxihttp_server::response::text_response(format!("{prev}"))
        });

    let (addr, _shutdown) = spawn_test_server(router).await;

    let client = oxihttp_client::Client::builder().build().expect("client");

    // First request: counter was 0, returns "0", then increments to 1.
    let body1 = client
        .get(&format!("http://{addr}/count"))
        .expect("builder")
        .send()
        .await
        .expect("request 1")
        .body_text()
        .await
        .expect("body 1");
    assert_eq!(body1, "0");

    // Second request: counter is now 1, returns "1", then increments to 2.
    let body2 = client
        .get(&format!("http://{addr}/count"))
        .expect("builder")
        .send()
        .await
        .expect("request 2")
        .body_text()
        .await
        .expect("body 2");
    assert_eq!(body2, "1");

    // The canonical counter was incremented twice.
    assert_eq!(shared_counter.load(Ordering::SeqCst), 2);
}

/// Handler can set a per-request extension and read it back in the same request.
#[tokio::test]
async fn test_extension_read() {
    #[derive(Clone)]
    struct RequestTag(String);

    let router = Router::new().get("/tag", |mut req: Request| async move {
        // Insert a per-request value.
        req.extensions_mut().insert(RequestTag("hello-ext".into()));

        // Read it back immediately.
        let tag = req
            .extension::<RequestTag>()
            .ok_or_else(|| OxiHttpError::Server("extension missing".into()))?;
        oxihttp_server::response::text_response(tag.0)
    });

    let (addr, _shutdown) = spawn_test_server(router).await;

    let client = oxihttp_client::Client::builder().build().expect("client");
    let body = client
        .get(&format!("http://{addr}/tag"))
        .expect("builder")
        .send()
        .await
        .expect("request")
        .body_text()
        .await
        .expect("body");
    assert_eq!(body, "hello-ext");
}

/// `extensions()` (read-only accessor) returns the same map as `extensions_mut()`.
#[tokio::test]
async fn test_extensions_accessor() {
    #[derive(Clone, Debug, PartialEq)]
    struct Marker(u32);

    let router = Router::new().get("/ext", |mut req: Request| async move {
        req.extensions_mut().insert(Marker(42));
        // Read via the read-only accessor.
        let v = req
            .extensions()
            .get::<Marker>()
            .cloned()
            .ok_or_else(|| OxiHttpError::Server("no marker".into()))?;
        oxihttp_server::response::text_response(format!("{}", v.0))
    });

    let (addr, _shutdown) = spawn_test_server(router).await;

    let client = oxihttp_client::Client::builder().build().expect("client");
    let body = client
        .get(&format!("http://{addr}/ext"))
        .expect("builder")
        .send()
        .await
        .expect("request")
        .body_text()
        .await
        .expect("body");
    assert_eq!(body, "42");
}

/// A nested router without its own state inherits the parent's state.
#[tokio::test]
async fn test_nested_router_inherits_state() {
    let config = AppConfig {
        greeting: "hello-nested".into(),
    };

    // Sub-router has no state of its own.
    let api = Router::new().get("/greet", |req: Request| async move {
        let cfg = req
            .state::<AppConfig>()
            .ok_or_else(|| OxiHttpError::Server("no config".into()))?;
        oxihttp_server::response::text_response(cfg.greeting.clone())
    });

    // Parent router holds the state and nests the sub-router under /api.
    let router = Router::new().with_state(config).nest("/api", api);

    let (addr, _shutdown) = spawn_test_server(router).await;

    let client = oxihttp_client::Client::builder().build().expect("client");
    let body = client
        .get(&format!("http://{addr}/api/greet"))
        .expect("builder")
        .send()
        .await
        .expect("request")
        .body_text()
        .await
        .expect("body");
    assert_eq!(body, "hello-nested");
}

/// A nested router with its own state uses its own state, not the parent's.
#[tokio::test]
async fn test_nested_router_own_state_wins() {
    let parent_config = AppConfig {
        greeting: "parent".into(),
    };
    let child_config = AppConfig {
        greeting: "child".into(),
    };

    // Sub-router has its own state.
    let api = Router::new()
        .with_state(child_config)
        .get("/greet", |req: Request| async move {
            let cfg = req
                .state::<AppConfig>()
                .ok_or_else(|| OxiHttpError::Server("no config".into()))?;
            oxihttp_server::response::text_response(cfg.greeting.clone())
        });

    // Parent router nests the sub-router.
    let router = Router::new().with_state(parent_config).nest("/api", api);

    let (addr, _shutdown) = spawn_test_server(router).await;

    let client = oxihttp_client::Client::builder().build().expect("client");
    let body = client
        .get(&format!("http://{addr}/api/greet"))
        .expect("builder")
        .send()
        .await
        .expect("request")
        .body_text()
        .await
        .expect("body");
    // Child state takes precedence.
    assert_eq!(body, "child");
}

/// `state::<T>()` returns `None` when no state of type `T` was registered.
#[tokio::test]
async fn test_state_missing_returns_none() {
    let router = Router::new().get("/nostate", |req: Request| async move {
        let present = req.state::<AppConfig>().is_some();
        oxihttp_server::response::text_response(if present { "present" } else { "absent" })
    });

    let (addr, _shutdown) = spawn_test_server(router).await;

    let client = oxihttp_client::Client::builder().build().expect("client");
    let body = client
        .get(&format!("http://{addr}/nostate"))
        .expect("builder")
        .send()
        .await
        .expect("request")
        .body_text()
        .await
        .expect("body");
    assert_eq!(body, "absent");
}

/// Verify that `Router::with_state` state injection works with the fallback handler.
#[tokio::test]
async fn test_state_in_fallback_handler() {
    let config = AppConfig {
        greeting: "fallback-state".into(),
    };

    let router = Router::new()
        .with_state(config)
        .fallback(|req: Request| async move {
            let cfg = req
                .state::<AppConfig>()
                .ok_or_else(|| OxiHttpError::Server("no config".into()))?;
            let body = Bytes::from(cfg.greeting.clone().into_bytes());
            hyper::Response::builder()
                .status(http::StatusCode::NOT_FOUND)
                .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .body(http_body_util::Full::new(body))
                .map_err(|e| OxiHttpError::Http(std::sync::Arc::new(e)))
        });

    let (addr, _shutdown) = spawn_test_server(router).await;

    let client = oxihttp_client::Client::builder()
        .redirect_policy(oxihttp_client::RedirectPolicy::None)
        .build()
        .expect("client");
    let resp = client
        .get(&format!("http://{addr}/unknown"))
        .expect("builder")
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
    let body = resp.body_text().await.expect("body");
    assert_eq!(body, "fallback-state");
}
