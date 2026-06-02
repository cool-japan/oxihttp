//! Lightweight middleware/interceptor chain for the HTTP client.
//!
//! Provides a `ClientMiddleware` trait that lets callers observe and instrument
//! each request/response round-trip without the complex type-erasure required
//! by a full `tower::Service` composition.
//!
//! # Example
//!
//! ```rust,no_run
//! use oxihttp_client::{Client, middleware::{ClientMiddleware, LoggingMiddleware}};
//!
//! let client = Client::builder()
//!     .with_middleware(LoggingMiddleware::new("my-client"))
//!     .build()
//!     .expect("client build");
//! ```

use std::sync::Arc;
use std::time::Duration;

use http::{HeaderMap, Method, Uri};

// ---------------------------------------------------------------------------
// Context types
// ---------------------------------------------------------------------------

/// Request context passed to `ClientMiddleware::before_request`.
///
/// Contains read-only references to the parts of the outgoing request that
/// are cheaply observable without buffering the body.
pub struct RequestContext<'a> {
    /// HTTP method of the outgoing request.
    pub method: &'a Method,
    /// Target URI of the outgoing request.
    pub uri: &'a Uri,
    /// Headers of the outgoing request.
    pub headers: &'a HeaderMap,
}

/// Response context passed to `ClientMiddleware::after_response`.
pub struct ResponseContext {
    /// Final HTTP status code of the response (after any redirects/retries).
    pub status: http::StatusCode,
    /// Wall-clock duration of the entire send operation.
    pub elapsed: Duration,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// A request/response interceptor that can observe (but not modify) each
/// HTTP round-trip.
///
/// Both callbacks receive a context struct describing the relevant phase.
/// The `before_request` hook fires once per `send()` call, immediately before
/// the first attempt.  The `after_response` hook fires once after the final
/// successful response (or after the last failed attempt returns an error,
/// though in error cases `after_response` is *not* called — see
/// `ClientMiddleware::after_error` for that).
pub trait ClientMiddleware: Send + Sync {
    /// Called immediately before the first network attempt.
    fn before_request(&self, ctx: &RequestContext<'_>);

    /// Called after a successful response is obtained (including after retries).
    fn after_response(&self, ctx: &ResponseContext);
}

// ---------------------------------------------------------------------------
// Concrete middleware
// ---------------------------------------------------------------------------

/// Logging middleware: emits one line to `stderr` before each request and one
/// line after each response.
///
/// # Example
///
/// ```rust,no_run
/// use oxihttp_client::middleware::LoggingMiddleware;
///
/// let mw = LoggingMiddleware::new("api-client");
/// ```
#[derive(Debug)]
pub struct LoggingMiddleware {
    name: String,
}

impl LoggingMiddleware {
    /// Create a new `LoggingMiddleware` with the given label.
    ///
    /// The label is included in every log line so that multiple clients can
    /// be distinguished in the same log stream.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl ClientMiddleware for LoggingMiddleware {
    fn before_request(&self, ctx: &RequestContext<'_>) {
        eprintln!("[{}] → {} {}", self.name, ctx.method, ctx.uri);
    }

    fn after_response(&self, ctx: &ResponseContext) {
        eprintln!(
            "[{}] ← {} ({:.1}ms)",
            self.name,
            ctx.status,
            ctx.elapsed.as_secs_f64() * 1000.0
        );
    }
}

/// Timing middleware: invokes a user-supplied callback with the elapsed
/// duration after each successful response.
///
/// Useful for recording latency metrics without coupling to any particular
/// metrics framework.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use std::sync::Mutex;
/// use oxihttp_client::middleware::TimingMiddleware;
///
/// let recorded = Arc::new(Mutex::new(Vec::new()));
/// let r = Arc::clone(&recorded);
/// let mw = TimingMiddleware::new(move |d| r.lock().expect("lock").push(d));
/// ```
pub struct TimingMiddleware {
    callback: Arc<dyn Fn(Duration) + Send + Sync>,
}

impl TimingMiddleware {
    /// Create a new `TimingMiddleware` with the given callback.
    ///
    /// The callback receives the wall-clock elapsed time of the full
    /// `send()` call (including redirect/retry overhead).
    pub fn new<F: Fn(Duration) + Send + Sync + 'static>(f: F) -> Self {
        Self {
            callback: Arc::new(f),
        }
    }
}

impl std::fmt::Debug for TimingMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimingMiddleware").finish_non_exhaustive()
    }
}

impl ClientMiddleware for TimingMiddleware {
    fn before_request(&self, _ctx: &RequestContext<'_>) {}

    fn after_response(&self, ctx: &ResponseContext) {
        (self.callback)(ctx.elapsed);
    }
}
