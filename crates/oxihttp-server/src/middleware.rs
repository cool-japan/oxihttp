//! Middleware types for the OxiHTTP server.
//!
//! Provides CORS, body size limits, rate limiting, timeouts, and logging
//! as composable middleware layers.

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use http_body_util::Full;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use oxihttp_core::OxiHttpError;

// ---------------------------------------------------------------------------
// CORS Middleware
// ---------------------------------------------------------------------------

/// Configuration for Cross-Origin Resource Sharing (CORS).
///
/// # Example
///
/// ```rust
/// use http::HeaderMap;
/// use oxihttp_server::middleware::CorsConfig;
///
/// let cors = CorsConfig::with_origins(vec!["https://app.example.com".to_string()]);
///
/// let mut headers = HeaderMap::new();
/// cors.apply_headers(&mut headers, Some("https://app.example.com"));
/// assert_eq!(
///     headers.get(http::header::ACCESS_CONTROL_ALLOW_ORIGIN).and_then(|v| v.to_str().ok()),
///     Some("https://app.example.com")
/// );
/// ```
#[derive(Debug, Clone)]
pub struct CorsConfig {
    /// Allowed origins. Use `["*"]` to allow all.
    pub allowed_origins: Vec<String>,
    /// Allowed HTTP methods.
    pub allowed_methods: Vec<Method>,
    /// Allowed request headers.
    pub allowed_headers: Vec<String>,
    /// Headers exposed to the client.
    pub exposed_headers: Vec<String>,
    /// Whether to allow credentials (cookies, auth headers).
    pub allow_credentials: bool,
    /// Max age for preflight cache (in seconds).
    pub max_age: Option<u64>,
}

impl CorsConfig {
    /// Create a permissive CORS config (allow all origins, common methods).
    pub fn permissive() -> Self {
        Self {
            allowed_origins: vec!["*".to_string()],
            allowed_methods: vec![
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::PATCH,
                Method::HEAD,
                Method::OPTIONS,
            ],
            allowed_headers: vec!["*".to_string()],
            exposed_headers: Vec::new(),
            allow_credentials: false,
            max_age: Some(86400),
        }
    }

    /// Create a CORS config that allows specific origins.
    pub fn with_origins(origins: Vec<String>) -> Self {
        Self {
            allowed_origins: origins,
            ..Self::permissive()
        }
    }

    /// Apply CORS headers to a response.
    ///
    /// # Caching (`Vary: Origin`)
    ///
    /// Whenever the allowed-origin set is not the bare wildcard (`["*"]`),
    /// the `Access-Control-Allow-Origin` value returned for one `Origin`
    /// differs from what would be returned for another (including the
    /// "no header at all" case when the origin is not on the allowlist), so
    /// a `Vary: Origin` response header is always added first — before any
    /// of the allow/deny branches below, including the "origin not
    /// allowed" early return. Without it, a shared cache or CDN sitting in
    /// front of the server could serve one origin's `Access-Control-Allow-*`
    /// headers (or lack thereof) to a different origin.
    ///
    /// This *adds* an `Origin` token to `Vary` rather than overwriting the
    /// header outright (see the internal `add_vary_origin` helper), so a `Vary: Accept-Encoding`
    /// set by another layer — e.g. the [`crate::Compression`] middleware,
    /// when both are applied to the same response — is preserved rather
    /// than silently dropped.
    pub fn apply_headers(&self, headers: &mut HeaderMap, origin: Option<&str>) {
        let is_wildcard_only = self.allowed_origins.contains(&"*".to_string());
        if !is_wildcard_only {
            add_vary_origin(headers);
        }

        let origin_value = if is_wildcard_only {
            "*"
        } else if let Some(o) = origin {
            if self.allowed_origins.iter().any(|a| a == o) {
                o
            } else {
                return; // Origin not allowed, don't set headers
            }
        } else {
            return;
        };

        // A wildcard `Access-Control-Allow-Origin: *` combined with
        // `Access-Control-Allow-Credentials: true` is rejected outright by
        // every browser (Fetch §3.2.3): credentialed requests require an
        // explicit, non-wildcard origin. This is a `debug_assert!` (rather
        // than a runtime error) so misconfiguration is caught during
        // development/tests without changing production response behavior.
        debug_assert!(
            !(origin_value == "*" && self.allow_credentials),
            "CORS misconfiguration: allow_credentials=true combined with a wildcard \
             allowed_origins([\"*\"]) is rejected by browsers (Fetch \u{a7}3.2.3); use an \
             explicit origin allowlist (CorsConfig::with_origins) instead of \"*\" when \
             allow_credentials is true"
        );

        if let Ok(val) = HeaderValue::from_str(origin_value) {
            headers.insert(http::header::ACCESS_CONTROL_ALLOW_ORIGIN, val);
        }

        if self.allow_credentials {
            headers.insert(
                http::header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                HeaderValue::from_static("true"),
            );
        }

        if !self.allowed_methods.is_empty() {
            let methods: String = self
                .allowed_methods
                .iter()
                .map(|m| m.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            if let Ok(val) = HeaderValue::from_str(&methods) {
                headers.insert(http::header::ACCESS_CONTROL_ALLOW_METHODS, val);
            }
        }

        if !self.allowed_headers.is_empty() {
            let hdrs = self.allowed_headers.join(", ");
            if let Ok(val) = HeaderValue::from_str(&hdrs) {
                headers.insert(http::header::ACCESS_CONTROL_ALLOW_HEADERS, val);
            }
        }

        if !self.exposed_headers.is_empty() {
            let hdrs = self.exposed_headers.join(", ");
            if let Ok(val) = HeaderValue::from_str(&hdrs) {
                headers.insert(http::header::ACCESS_CONTROL_EXPOSE_HEADERS, val);
            }
        }

        if let Some(max_age) = self.max_age {
            if let Ok(val) = HeaderValue::from_str(&max_age.to_string()) {
                headers.insert(http::header::ACCESS_CONTROL_MAX_AGE, val);
            }
        }
    }

    /// Handle a preflight (OPTIONS) request, returning a 204 No Content response
    /// with appropriate CORS headers.
    pub fn preflight_response(
        &self,
        origin: Option<&str>,
    ) -> Result<hyper::Response<Full<Bytes>>, OxiHttpError> {
        let mut resp = hyper::Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Full::new(Bytes::new()))
            .map_err(|e| OxiHttpError::Http(Arc::new(e)))?;
        self.apply_headers(resp.headers_mut(), origin);
        Ok(resp)
    }
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self::permissive()
    }
}

/// Ensure the response's `Vary` header includes an `Origin` token, without
/// discarding any token(s) another layer already put there.
///
/// `HeaderMap::insert` *replaces* every existing value for a header name —
/// using it directly here would silently destroy a `Vary: Accept-Encoding`
/// set by e.g. [`crate::Compression`]'s middleware if both are applied to
/// the same response (compression as an opt-in step inside a handler, CORS
/// automatically in `MiddlewarePipeline::post_handle`).
///
/// `Vary` is a list-based field (RFC 9110 §5.2 / §12.5.5): a HeaderMap may
/// legally represent it either as one comma-joined line or as multiple
/// same-name lines (if some other layer used `HeaderMap::append` rather
/// than `insert`). This reads *every* existing line via `get_all` — not
/// just the first, which is all a plain `get` would see — so a token
/// hiding in a second line is not missed. All lines are then normalized
/// into a single comma-joined line (semantically identical per RFC 9110,
/// and avoids ever needing to reason about "which of N lines do I append
/// to"), with `Origin` added only if no existing line already carries it.
fn add_vary_origin(headers: &mut HeaderMap) {
    let mut tokens: Vec<String> = Vec::new();
    let mut has_origin = false;
    for value in headers.get_all(http::header::VARY).iter() {
        let Ok(s) = value.to_str() else {
            // Not valid UTF-8 (exceedingly unlikely for a header this crate
            // itself would have set) — do not risk corrupting it; leave the
            // whole header untouched rather than partially normalizing it.
            return;
        };
        for tok in s.split(',') {
            let tok = tok.trim();
            if tok.is_empty() {
                continue;
            }
            if tok.eq_ignore_ascii_case("origin") {
                has_origin = true;
            }
            tokens.push(tok.to_string());
        }
    }

    if has_origin {
        return; // Every existing line already accounted for; nothing to do.
    }
    tokens.push("Origin".to_string());

    let combined = tokens.join(", ");
    if let Ok(val) = HeaderValue::from_str(&combined) {
        headers.insert(http::header::VARY, val);
    }
}

// ---------------------------------------------------------------------------
// Body Size Limit
// ---------------------------------------------------------------------------

/// Configuration for request body size limits.
#[derive(Debug, Clone, Copy)]
pub struct BodyLimitConfig {
    /// Maximum body size in bytes.
    pub max_bytes: u64,
}

impl BodyLimitConfig {
    /// Create a body limit config with the given maximum size.
    pub fn new(max_bytes: u64) -> Self {
        Self { max_bytes }
    }

    /// Check if a content-length exceeds the limit.
    /// Returns `Ok(())` if within limits, `Err` with a 413 status otherwise.
    pub fn check_content_length(&self, content_length: Option<u64>) -> Result<(), OxiHttpError> {
        if let Some(len) = content_length {
            if len > self.max_bytes {
                return Err(OxiHttpError::Body(format!(
                    "request body too large: {} bytes exceeds limit of {} bytes",
                    len, self.max_bytes
                )));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Rate Limiting (Token Bucket)
// ---------------------------------------------------------------------------

/// Buckets idle longer than this are dropped by the periodic sweep.
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Hard ceiling on the number of distinct buckets tracked at once, bounding
/// worst-case memory even when keys are attacker-chosen and rotate faster
/// than the idle sweep can reclaim them.
const DEFAULT_MAX_BUCKETS: usize = 100_000;

/// Run the idle-bucket sweep every this many `check()` calls, so the common
/// case (few distinct keys) never pays for a full-map scan on every request.
const SWEEP_INTERVAL: u32 = 256;

/// Rate limiter using the token bucket algorithm.
#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<RateLimiterInner>>,
}

struct RateLimiterInner {
    /// Buckets keyed by client identifier (by default the TCP peer IP; see
    /// [`MiddlewarePipeline::with_trusted_proxy_headers`]).
    buckets: HashMap<String, TokenBucket>,
    /// Maximum tokens per bucket.
    max_tokens: u32,
    /// Token refill rate (tokens per second).
    refill_rate: f64,
    /// Buckets idle longer than this are reclaimed by the periodic sweep.
    idle_timeout: Duration,
    /// Hard cap on the number of tracked buckets.
    max_buckets: usize,
    /// Calls since the last idle sweep.
    checks_since_sweep: u32,
}

struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
}

impl RateLimiter {
    /// Create a new rate limiter.
    ///
    /// - `max_tokens`: Maximum number of tokens (burst capacity).
    /// - `refill_rate`: Tokens added per second.
    ///
    /// Buckets that have not been touched for 5 minutes are reclaimed
    /// automatically and the total number of tracked buckets is capped at
    /// 100,000, so an attacker who rotates their rate-limit key cannot grow
    /// the limiter's memory without bound. Use
    /// [`RateLimiter::with_limits`] to override either threshold.
    ///
    /// # Example
    ///
    /// ```rust
    /// use oxihttp_server::middleware::RateLimiter;
    ///
    /// #[tokio::main(flavor = "current_thread")]
    /// async fn main() {
    ///     // Burst capacity of 2, refilling extremely slowly so the 3rd call
    ///     // in quick succession is rejected within the same token bucket.
    ///     let limiter = RateLimiter::new(2, 0.0);
    ///
    ///     assert!(limiter.check("client-a").await, "1st request consumes a token");
    ///     assert!(limiter.check("client-a").await, "2nd request consumes the last token");
    ///     assert!(!limiter.check("client-a").await, "3rd request is rate-limited");
    ///
    ///     // A different key has its own independent bucket.
    ///     assert!(limiter.check("client-b").await);
    /// }
    /// ```
    pub fn new(max_tokens: u32, refill_rate: f64) -> Self {
        Self::with_limits(
            max_tokens,
            refill_rate,
            DEFAULT_IDLE_TIMEOUT,
            DEFAULT_MAX_BUCKETS,
        )
    }

    /// Create a new rate limiter with explicit bucket-eviction thresholds.
    ///
    /// - `idle_timeout`: buckets untouched for longer than this are dropped
    ///   by the periodic sweep (runs every `SWEEP_INTERVAL` calls).
    /// - `max_buckets`: hard cap on the number of distinct buckets tracked
    ///   at once. When a *new* key arrives at capacity, one arbitrary
    ///   existing bucket is evicted to make room — this is O(1) rather than
    ///   a full LRU scan, so a client sharing the evicted bucket's key
    ///   simply starts a fresh bucket on its next request. This is an
    ///   accepted trade-off at the default 100,000-bucket cap: the goal is
    ///   a hard memory ceiling, not perfect fairness under sustained
    ///   attack-level key churn.
    pub fn with_limits(
        max_tokens: u32,
        refill_rate: f64,
        idle_timeout: Duration,
        max_buckets: usize,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RateLimiterInner {
                buckets: HashMap::new(),
                max_tokens,
                refill_rate,
                idle_timeout,
                max_buckets: max_buckets.max(1),
                checks_since_sweep: 0,
            })),
        }
    }

    /// Check if a request from the given key is allowed.
    ///
    /// Returns `true` if the request is allowed (token consumed),
    /// `false` if rate-limited (429 should be returned).
    pub async fn check(&self, key: &str) -> bool {
        let mut inner = self.inner.lock().await;
        let now = Instant::now();
        let max_tokens = inner.max_tokens;
        let refill_rate = inner.refill_rate;
        let idle_timeout = inner.idle_timeout;

        // Periodically reclaim buckets that have been idle long enough that
        // their owner is unlikely to still be sending requests. Bounded to
        // once every `SWEEP_INTERVAL` calls so the common case (a small,
        // stable set of clients) never pays for a full-map scan per request.
        inner.checks_since_sweep += 1;
        if inner.checks_since_sweep >= SWEEP_INTERVAL {
            inner.checks_since_sweep = 0;
            inner
                .buckets
                .retain(|_, b| now.duration_since(b.last_refill) < idle_timeout);
        }

        // Hard cap: if this is a new key and we are already at capacity,
        // evict one arbitrary existing bucket first. This bounds worst-case
        // memory even when a client rotates keys faster than the idle sweep
        // (above) can reclaim them.
        if !inner.buckets.contains_key(key) && inner.buckets.len() >= inner.max_buckets {
            if let Some(evict) = inner.buckets.keys().next().cloned() {
                inner.buckets.remove(&evict);
            }
        }

        let bucket = inner.buckets.entry(key.to_string()).or_insert(TokenBucket {
            tokens: max_tokens as f64,
            last_refill: now,
        });

        // Refill tokens based on elapsed time
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * refill_rate).min(max_tokens as f64);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Number of buckets currently tracked. Exposed for tests and metrics;
    /// bounded at all times by the `max_buckets` threshold.
    pub async fn bucket_count(&self) -> usize {
        self.inner.lock().await.buckets.len()
    }

    /// Build a 429 Too Many Requests response.
    pub fn too_many_requests() -> Result<hyper::Response<Full<Bytes>>, OxiHttpError> {
        hyper::Response::builder()
            .status(StatusCode::TOO_MANY_REQUESTS)
            .body(Full::new(Bytes::from("Too Many Requests")))
            .map_err(|e| OxiHttpError::Http(Arc::new(e)))
    }
}

impl std::fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimiter").finish()
    }
}

// ---------------------------------------------------------------------------
// Request Timeout
// ---------------------------------------------------------------------------

/// Configuration for request processing timeout.
#[derive(Debug, Clone, Copy)]
pub struct TimeoutConfig {
    /// Maximum time to process a request.
    pub duration: Duration,
}

impl TimeoutConfig {
    /// Create a timeout config.
    pub fn new(duration: Duration) -> Self {
        Self { duration }
    }

    /// Build a 408 Request Timeout response.
    pub fn timeout_response() -> Result<hyper::Response<Full<Bytes>>, OxiHttpError> {
        hyper::Response::builder()
            .status(StatusCode::REQUEST_TIMEOUT)
            .body(Full::new(Bytes::from("Request Timeout")))
            .map_err(|e| OxiHttpError::Http(Arc::new(e)))
    }
}

// ---------------------------------------------------------------------------
// Middleware Pipeline
// ---------------------------------------------------------------------------

/// The middleware pipeline configuration for a server.
#[derive(Clone)]
pub struct MiddlewarePipeline {
    /// CORS configuration (applied to all responses).
    pub cors: Option<CorsConfig>,
    /// Body size limit (checked before handler).
    pub body_limit: Option<BodyLimitConfig>,
    /// Rate limiter (checked before handler).
    pub rate_limiter: Option<RateLimiter>,
    /// Request timeout.
    pub timeout: Option<TimeoutConfig>,
    /// Whether to trust the `X-Forwarded-For` header for the rate-limiter
    /// key. See [`MiddlewarePipeline::with_trusted_proxy_headers`].
    pub trust_forwarded_for: bool,
    /// Allowed methods for CORS preflight (derived from CORS config).
    allowed_methods: HashSet<Method>,
}

impl MiddlewarePipeline {
    /// Create an empty middleware pipeline.
    pub fn new() -> Self {
        Self {
            cors: None,
            body_limit: None,
            rate_limiter: None,
            timeout: None,
            trust_forwarded_for: false,
            allowed_methods: HashSet::new(),
        }
    }

    /// Add CORS middleware.
    pub fn with_cors(mut self, config: CorsConfig) -> Self {
        self.allowed_methods = config.allowed_methods.iter().cloned().collect();
        self.cors = Some(config);
        self
    }

    /// Add body size limit middleware.
    pub fn with_body_limit(mut self, max_bytes: u64) -> Self {
        self.body_limit = Some(BodyLimitConfig::new(max_bytes));
        self
    }

    /// Add rate limiting middleware.
    pub fn with_rate_limiter(mut self, limiter: RateLimiter) -> Self {
        self.rate_limiter = Some(limiter);
        self
    }

    /// Add request timeout middleware.
    pub fn with_timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(TimeoutConfig::new(duration));
        self
    }

    /// Trust the `X-Forwarded-For` header for the rate-limiter key.
    ///
    /// Only enable this when the server sits behind a **single trusted
    /// reverse proxy** (nginx, an ALB, ...) that itself appends the real
    /// client address to `X-Forwarded-For` on every request. When enabled,
    /// the *last* comma-separated entry — the hop closest to this server,
    /// i.e. the one the trusted proxy appended — is used as the rate-limit
    /// key.
    ///
    /// Defaults to `false`: the rate limiter keys on the actual TCP peer
    /// address instead. Do **not** enable this for a server that accepts
    /// connections directly from the internet — `X-Forwarded-For` is fully
    /// attacker-controlled in that deployment and trusting it lets a client
    /// bypass the rate limit by sending a different value on every request,
    /// or collapse every other client onto a single shared bucket.
    pub fn with_trusted_proxy_headers(mut self, trust: bool) -> Self {
        self.trust_forwarded_for = trust;
        self
    }

    /// Run pre-handler middleware checks.
    ///
    /// `remote_addr` is the TCP peer address accepted for this connection;
    /// it is the default rate-limiter key (see
    /// [`MiddlewarePipeline::with_trusted_proxy_headers`] to key on
    /// `X-Forwarded-For` instead, only when a trusted reverse proxy is
    /// actually in front of this server).
    ///
    /// Returns `Some(response)` if middleware short-circuits (e.g. CORS preflight,
    /// rate limit exceeded, body too large). Returns `None` if the request should
    /// proceed to the handler.
    pub async fn pre_handle(
        &self,
        req: &hyper::Request<hyper::body::Incoming>,
        remote_addr: SocketAddr,
    ) -> Option<Result<hyper::Response<Full<Bytes>>, OxiHttpError>> {
        // CORS preflight
        if req.method() == Method::OPTIONS {
            if let Some(ref cors) = self.cors {
                let origin = req
                    .headers()
                    .get(http::header::ORIGIN)
                    .and_then(|v| v.to_str().ok());
                return Some(cors.preflight_response(origin));
            }
        }

        // Rate limiting
        if let Some(ref limiter) = self.rate_limiter {
            let key = rate_limit_key(req.headers(), remote_addr, self.trust_forwarded_for);
            if !limiter.check(&key).await {
                return Some(RateLimiter::too_many_requests());
            }
        }

        // Body size limit
        if let Some(ref body_limit) = self.body_limit {
            let content_length = req
                .headers()
                .get(http::header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            if let Err(e) = body_limit.check_content_length(content_length) {
                return Some(
                    hyper::Response::builder()
                        .status(StatusCode::PAYLOAD_TOO_LARGE)
                        .body(Full::new(Bytes::from(e.to_string())))
                        .map_err(|e| OxiHttpError::Http(Arc::new(e))),
                );
            }
        }

        None
    }

    /// Attach the configured body limit to the request extensions so that body
    /// readers (`Request::body_bytes` and friends) can enforce it on the decoded
    /// byte stream.
    ///
    /// The pre-handler [`Content-Length`](http::header::CONTENT_LENGTH) check
    /// only rejects bodies that *declare* an oversized length; a chunked /
    /// `Transfer-Encoding` body omits that header and would otherwise be
    /// buffered without bound. Carrying the limit through to the body reader
    /// closes that gap.
    pub fn inject_body_limit(&self, req: &mut hyper::Request<hyper::body::Incoming>) {
        if let Some(limit) = self.body_limit {
            req.extensions_mut().insert(limit);
        }
    }

    /// Apply post-handler middleware (e.g. CORS headers) to a response.
    pub fn post_handle(&self, resp: &mut hyper::Response<Full<Bytes>>, origin: Option<&str>) {
        if let Some(ref cors) = self.cors {
            cors.apply_headers(resp.headers_mut(), origin);
        }
    }
}

/// Derive the rate-limiter bucket key for a request.
///
/// By default (`trust_forwarded_for == false`) this is always the TCP peer
/// IP address, which cannot be spoofed by the client. Only when
/// `trust_forwarded_for` is `true` — meaning the operator has confirmed a
/// trusted reverse proxy sits directly in front of this server and
/// overwrites/appends `X-Forwarded-For` itself — is the header consulted,
/// and even then only its last (rightmost) comma-separated entry is used,
/// since that is the hop the trusted proxy itself appended; everything to
/// its left may have been supplied by the original client and cannot be
/// trusted. An empty or missing header falls back to the peer address.
fn rate_limit_key(
    headers: &HeaderMap,
    remote_addr: SocketAddr,
    trust_forwarded_for: bool,
) -> String {
    if trust_forwarded_for {
        if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(last) = xff.rsplit(',').next() {
                let trimmed = last.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }
    remote_addr.ip().to_string()
}

impl Default for MiddlewarePipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for MiddlewarePipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MiddlewarePipeline")
            .field("cors", &self.cors.is_some())
            .field("body_limit", &self.body_limit)
            .field("rate_limiter", &self.rate_limiter.is_some())
            .field("trust_forwarded_for", &self.trust_forwarded_for)
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn addr(ip: [u8; 4], port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), port)
    }

    // -----------------------------------------------------------------
    // rate_limit_key — regression coverage for the spoofable-XFF /
    // shared-"unknown"-bucket bug.
    // -----------------------------------------------------------------

    #[test]
    fn rate_limit_key_defaults_to_peer_ip_ignoring_xff() {
        // Before the fix, a missing X-Forwarded-For header collapsed every
        // client onto a single shared "unknown" bucket. Two different
        // peers must now get two different keys even with no XFF header.
        let headers = HeaderMap::new();
        let a = rate_limit_key(&headers, addr([203, 0, 113, 5], 51000), false);
        let b = rate_limit_key(&headers, addr([198, 51, 100, 9], 6000), false);
        assert_ne!(a, b, "distinct peers must not share a rate-limit bucket");
        assert_eq!(a, "203.0.113.5");
        assert_eq!(b, "198.51.100.9");
    }

    #[test]
    fn rate_limit_key_ignores_spoofed_xff_by_default() {
        // The header is fully attacker-controlled; without an explicit
        // trusted-proxy opt-in it must be ignored, not trusted.
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("1.2.3.4"));
        let key = rate_limit_key(&headers, addr([203, 0, 113, 5], 51000), false);
        assert_eq!(
            key, "203.0.113.5",
            "XFF must be ignored unless trust_forwarded_for is enabled"
        );
    }

    #[test]
    fn rate_limit_key_honors_last_hop_when_trusted() {
        // Convention: each proxy *appends* the address it observed, so the
        // right-most (last) entry is the one the trusted proxy directly in
        // front of us added — everything to its left may be attacker-supplied.
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("9.9.9.9, 203.0.113.5"),
        );
        let key = rate_limit_key(&headers, addr([10, 0, 0, 1], 51000), true);
        assert_eq!(key, "203.0.113.5");
    }

    #[test]
    fn rate_limit_key_falls_back_to_peer_ip_on_empty_or_garbage_xff() {
        let peer = addr([203, 0, 113, 5], 51000);

        let mut empty_headers = HeaderMap::new();
        empty_headers.insert("x-forwarded-for", HeaderValue::from_static(""));
        assert_eq!(
            rate_limit_key(&empty_headers, peer, true),
            "203.0.113.5",
            "empty XFF value must fall back to the peer address"
        );

        let mut trailing_comma = HeaderMap::new();
        trailing_comma.insert("x-forwarded-for", HeaderValue::from_static("1.2.3.4, "));
        assert_eq!(
            rate_limit_key(&trailing_comma, peer, true),
            "203.0.113.5",
            "trailing empty segment must fall back to the peer address"
        );

        // No header at all, trust enabled.
        let no_header = HeaderMap::new();
        assert_eq!(rate_limit_key(&no_header, peer, true), "203.0.113.5");
    }

    // -----------------------------------------------------------------
    // RateLimiter — bucket eviction bounds memory.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn rate_limiter_evicts_buckets_at_capacity() {
        let limiter = RateLimiter::with_limits(10, 1.0, Duration::from_secs(300), 8);

        for i in 0..64 {
            limiter.check(&format!("client-{i}")).await;
            assert!(
                limiter.bucket_count().await <= 8,
                "bucket count must never exceed max_buckets, even under key churn"
            );
        }
        assert_eq!(limiter.bucket_count().await, 8);
    }

    #[tokio::test]
    async fn rate_limiter_idle_sweep_reclaims_stale_buckets() {
        // idle_timeout of 0 means every bucket looks stale on the very next
        // sweep; SWEEP_INTERVAL (256) forces a sweep by then regardless of
        // how many distinct keys were seen.
        let limiter = RateLimiter::with_limits(10, 1.0, Duration::from_secs(0), 100_000);
        for i in 0..300 {
            limiter.check(&format!("client-{i}")).await;
        }
        // A sweep must have run at least once (300 > SWEEP_INTERVAL) and
        // reclaimed buckets whose idle_timeout of 0 makes them immediately
        // stale, so the tracked count must be far below the 300 distinct
        // keys presented.
        assert!(
            limiter.bucket_count().await < 300,
            "idle sweep must reclaim buckets older than idle_timeout"
        );
    }

    #[tokio::test]
    async fn rate_limiter_distinct_keys_have_independent_buckets() {
        // Direct regression test for the "one client exhausts the shared
        // bucket and 429s everyone else" failure mode: two different keys
        // (as would now be derived from two different peer IPs) must not
        // interfere with each other.
        let limiter = RateLimiter::new(1, 0.0);
        assert!(limiter.check("peer-a").await);
        assert!(
            !limiter.check("peer-a").await,
            "peer-a's single token is spent"
        );
        assert!(
            limiter.check("peer-b").await,
            "peer-b must have its own independent bucket"
        );
    }

    #[test]
    fn middleware_pipeline_trusted_proxy_headers_defaults_to_false() {
        // Secure-by-default: XFF must not be trusted unless explicitly
        // opted into via `with_trusted_proxy_headers(true)`.
        let pipeline = MiddlewarePipeline::new();
        assert!(!pipeline.trust_forwarded_for);
        let pipeline = pipeline.with_trusted_proxy_headers(true);
        assert!(pipeline.trust_forwarded_for);
    }

    // -----------------------------------------------------------------
    // CorsConfig::apply_headers — `Vary: Origin` caching correctness, and
    // the wildcard-origin + credentials debug guard.
    // -----------------------------------------------------------------

    #[test]
    fn cors_vary_origin_present_when_allowlist_matches() {
        let cors = CorsConfig::with_origins(vec!["https://app.example.com".to_string()]);
        let mut headers = HeaderMap::new();
        cors.apply_headers(&mut headers, Some("https://app.example.com"));
        assert_eq!(
            headers
                .get(http::header::VARY)
                .and_then(|v| v.to_str().ok()),
            Some("Origin"),
            "a response whose ACAO value depends on the request Origin must vary on it"
        );
        assert_eq!(
            headers
                .get(http::header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok()),
            Some("https://app.example.com")
        );
    }

    #[test]
    fn cors_vary_origin_present_even_when_origin_rejected() {
        // The early-return "origin not on the allowlist" path must still
        // carry `Vary: Origin` — a cache must not conflate "origin X got no
        // CORS headers" with "origin Y got no CORS headers" either.
        let cors = CorsConfig::with_origins(vec!["https://app.example.com".to_string()]);
        let mut headers = HeaderMap::new();
        cors.apply_headers(&mut headers, Some("https://evil.example.com"));
        assert_eq!(
            headers
                .get(http::header::VARY)
                .and_then(|v| v.to_str().ok()),
            Some("Origin")
        );
        assert!(
            !headers.contains_key(http::header::ACCESS_CONTROL_ALLOW_ORIGIN),
            "a disallowed origin must not receive an ACAO header"
        );
    }

    #[test]
    fn cors_vary_origin_omitted_for_pure_wildcard_config() {
        // A `["*"]`-only config's ACAO value never depends on the request
        // Origin, so no `Vary: Origin` is needed (matching e.g. tower-http's
        // permissive CORS layer).
        let cors = CorsConfig::permissive();
        let mut headers = HeaderMap::new();
        cors.apply_headers(&mut headers, Some("https://anything.example.com"));
        assert!(!headers.contains_key(http::header::VARY));
        assert_eq!(
            headers
                .get(http::header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok()),
            Some("*")
        );
    }

    /// Regression test: `apply_headers` must not clobber a `Vary` value
    /// already set by another layer applied to the same response — e.g.
    /// `Compression`'s middleware, which sets `Vary: Accept-Encoding` (see
    /// `compression.rs`). Both `Compression::apply` (opt-in, called inside a
    /// handler) and `CorsConfig::apply_headers` (automatic, called from
    /// `MiddlewarePipeline::post_handle`) can run against the same response,
    /// so a plain `headers.insert(VARY, "Origin")` here would silently
    /// discard the compression signal — the very caching-correctness bug
    /// this feature exists to fix, just relocated to a different header.
    #[test]
    fn cors_vary_origin_appends_to_existing_vary_without_clobbering_it() {
        let cors = CorsConfig::with_origins(vec!["https://app.example.com".to_string()]);
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::VARY,
            HeaderValue::from_static("Accept-Encoding"),
        );
        cors.apply_headers(&mut headers, Some("https://app.example.com"));
        let vary = headers
            .get(http::header::VARY)
            .and_then(|v| v.to_str().ok())
            .expect("Vary header must be present");
        let tokens: Vec<&str> = vary.split(',').map(str::trim).collect();
        assert!(
            tokens
                .iter()
                .any(|t| t.eq_ignore_ascii_case("Accept-Encoding")),
            "pre-existing Vary token must survive: {vary:?}"
        );
        assert!(
            tokens.iter().any(|t| t.eq_ignore_ascii_case("Origin")),
            "Origin token must be added: {vary:?}"
        );
    }

    /// Calling `apply_headers` twice (e.g. once for a preflight helper path
    /// and again from `post_handle`, or simply two CORS layers stacked)
    /// must not accumulate duplicate `Origin` tokens.
    #[test]
    fn cors_vary_origin_is_idempotent_across_repeated_calls() {
        let cors = CorsConfig::with_origins(vec!["https://app.example.com".to_string()]);
        let mut headers = HeaderMap::new();
        cors.apply_headers(&mut headers, Some("https://app.example.com"));
        cors.apply_headers(&mut headers, Some("https://app.example.com"));
        let vary = headers
            .get(http::header::VARY)
            .and_then(|v| v.to_str().ok())
            .expect("Vary header must be present");
        assert_eq!(
            vary, "Origin",
            "a second apply_headers call must not duplicate the Origin token"
        );
    }

    /// `Vary` is a list-based field (RFC 9110 §5.2): a spec-conformant
    /// HeaderMap may represent multiple tokens either as one comma-joined
    /// line or as several same-name lines added via `HeaderMap::append`.
    /// `add_vary_origin` must inspect *every* line (not just the first, as
    /// a plain `headers.get()` would) when deciding whether `Origin` is
    /// already present — otherwise a token hiding in a second line would be
    /// missed and (in the pre-fix `insert`-based implementation) that
    /// second line would additionally have been silently discarded.
    #[test]
    fn cors_vary_origin_detects_token_hiding_in_a_second_vary_line() {
        let cors = CorsConfig::with_origins(vec!["https://app.example.com".to_string()]);
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::VARY,
            HeaderValue::from_static("Accept-Encoding"),
        );
        // A second, independent `Vary` line — legal per RFC 9110 §5.2 for a
        // list-based field — already carrying `Origin`.
        headers.append(http::header::VARY, HeaderValue::from_static("Origin"));

        cors.apply_headers(&mut headers, Some("https://app.example.com"));

        // All `Vary` lines, tokenized: must contain each token exactly
        // once — `Origin` was already present (in the second line) and
        // must not be duplicated, and `Accept-Encoding` (in the first
        // line) must survive the normalization into a single line.
        let all_tokens: Vec<String> = headers
            .get_all(http::header::VARY)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|s| s.split(',').map(|t| t.trim().to_ascii_lowercase()))
            .filter(|t| !t.is_empty())
            .collect();
        let origin_count = all_tokens.iter().filter(|t| *t == "origin").count();
        let accept_encoding_count = all_tokens
            .iter()
            .filter(|t| *t == "accept-encoding")
            .count();
        assert_eq!(
            origin_count, 1,
            "Origin must appear exactly once across all Vary lines: {all_tokens:?}"
        );
        assert_eq!(
            accept_encoding_count, 1,
            "Accept-Encoding from the first line must survive: {all_tokens:?}"
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "CORS misconfiguration")]
    fn cors_wildcard_plus_credentials_trips_debug_assert() {
        // Browsers reject `Access-Control-Allow-Origin: *` combined with
        // `Access-Control-Allow-Credentials: true` outright (Fetch
        // §3.2.3) — this configuration is always a mistake, so a debug
        // build must catch it loudly rather than silently emitting headers
        // no client can actually use.
        let cors = CorsConfig {
            allow_credentials: true,
            ..CorsConfig::permissive()
        };
        let mut headers = HeaderMap::new();
        cors.apply_headers(&mut headers, None);
    }
}
