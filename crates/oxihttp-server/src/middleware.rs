//! Middleware types for the OxiHTTP server.
//!
//! Provides CORS, body size limits, rate limiting, timeouts, and logging
//! as composable middleware layers.

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use http_body_util::Full;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use oxihttp_core::OxiHttpError;

// ---------------------------------------------------------------------------
// CORS Middleware
// ---------------------------------------------------------------------------

/// Configuration for Cross-Origin Resource Sharing (CORS).
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
    pub fn apply_headers(&self, headers: &mut HeaderMap, origin: Option<&str>) {
        let origin_value = if self.allowed_origins.contains(&"*".to_string()) {
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

/// Rate limiter using the token bucket algorithm.
#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<RateLimiterInner>>,
}

struct RateLimiterInner {
    /// Buckets keyed by IP address or route identifier.
    buckets: HashMap<String, TokenBucket>,
    /// Maximum tokens per bucket.
    max_tokens: u32,
    /// Token refill rate (tokens per second).
    refill_rate: f64,
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
    pub fn new(max_tokens: u32, refill_rate: f64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RateLimiterInner {
                buckets: HashMap::new(),
                max_tokens,
                refill_rate,
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

    /// Run pre-handler middleware checks.
    ///
    /// Returns `Some(response)` if middleware short-circuits (e.g. CORS preflight,
    /// rate limit exceeded, body too large). Returns `None` if the request should
    /// proceed to the handler.
    pub async fn pre_handle(
        &self,
        req: &hyper::Request<hyper::body::Incoming>,
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
            let key = req
                .headers()
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown")
                .to_string();
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

    /// Apply post-handler middleware (e.g. CORS headers) to a response.
    pub fn post_handle(&self, resp: &mut hyper::Response<Full<Bytes>>, origin: Option<&str>) {
        if let Some(ref cors) = self.cors {
            cors.apply_headers(resp.headers_mut(), origin);
        }
    }
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
            .field("timeout", &self.timeout)
            .finish()
    }
}
