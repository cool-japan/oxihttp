//! OxiHTTP Client - Pure-Rust HTTP client for the OxiHTTP stack.
//!
//! Provides a high-level HTTP client with connection pooling, redirect handling,
//! retry logic, timeouts, and a fluent request builder API.
//!
//! # Example
//!
//! ```rust,no_run
//! # async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
//! use oxihttp_client::Client;
//!
//! let client = Client::builder().build()?;
//! let resp = client.get("http://example.com")?.send().await?;
//! assert_eq!(resp.status(), http::StatusCode::OK);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod client_builder;
pub mod middleware;
pub mod proxy;
pub mod redirect;
pub mod resolver;
pub mod retry;

#[cfg(feature = "tls")]
pub mod connector;
#[cfg(feature = "tls")]
pub mod request_config;
#[cfg(feature = "tls")]
pub mod tls;

#[cfg(feature = "h3")]
pub mod h3;

#[cfg(feature = "tls")]
pub use connector::{MaybeHttpsStream, OxiHttpsConnector};
#[cfg(feature = "tls")]
pub use request_config::RequestTlsConfig;
#[cfg(feature = "tls")]
pub use tls::DangerousNoVerification;

#[cfg(feature = "socks")]
pub use proxy::Socks5Connector;
pub use proxy::{ProxyConnector, ProxyKind};

use bytes::Bytes;
use futures_core::Stream;
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::client::legacy::connect::{Connect, HttpConnector};
use hyper_util::client::legacy::Client as HyperClient;
#[cfg(feature = "tls")]
use hyper_util::rt::TokioExecutor;
use resolver::BoxResolver;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

#[cfg(feature = "tls")]
pub(crate) use client_builder::apply_http2_settings;
pub use client_builder::{ClientBuilder, Http2Settings};
pub use middleware::{ClientMiddleware, LoggingMiddleware, TimingMiddleware};
use oxihttp_core::OxiHttpError;
pub use redirect::RedirectPolicy;
pub use retry::RetryPolicy;

// ---------------------------------------------------------------------------
// BodyStream — streaming response body
// ---------------------------------------------------------------------------

/// An async stream of response body chunks produced by `Response::body_stream()`.
pub struct BodyStream {
    inner: http_body_util::BodyStream<Incoming>,
}

impl Stream for BodyStream {
    type Item = Result<Bytes, OxiHttpError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        return Poll::Ready(Some(Ok(data)));
                    }
                    // Trailers or other non-data frames — skip and poll again
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(OxiHttpError::Body(e.to_string()))));
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------

/// HTTP response wrapper providing convenience methods for body consumption.
pub struct Response {
    inner: http::Response<Incoming>,
    /// Whether to auto-decompress the response body using Content-Encoding.
    decompress: bool,
}

impl Response {
    /// HTTP status code.
    pub fn status(&self) -> StatusCode {
        self.inner.status()
    }

    /// Response headers.
    pub fn headers(&self) -> &HeaderMap {
        self.inner.headers()
    }

    /// Return the first value of the named response header as a UTF-8 string,
    /// or `None` if the header is absent or its value is not valid UTF-8.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
    /// use oxihttp_client::Client;
    ///
    /// let client = Client::builder().build()?;
    /// let resp = client.get("http://example.com/new-resource")?.send().await?;
    /// if let Some(location) = resp.header("location") {
    ///     println!("redirected to: {location}");
    /// }
    /// if let Some(nonce) = resp.header("replay-nonce") {
    ///     println!("ACME nonce: {nonce}");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn header(&self, name: &str) -> Option<&str> {
        self.inner.headers().get(name).and_then(|v| v.to_str().ok())
    }

    /// HTTP version used for this response.
    pub fn version(&self) -> http::Version {
        self.inner.version()
    }

    /// Content-Length header as u64 if present and valid.
    pub fn content_length(&self) -> Option<u64> {
        self.inner
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
    }

    /// Consume the body and return raw bytes, auto-decompressing if enabled.
    pub async fn body_bytes(self) -> Result<Bytes, OxiHttpError> {
        let decompress = self.decompress;
        let ce = self
            .inner
            .headers()
            .get(http::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_ascii_lowercase());

        let raw = self
            .inner
            .into_body()
            .collect()
            .await
            .map(|c| c.to_bytes())
            .map_err(|e| OxiHttpError::Body(e.to_string()))?;

        if decompress {
            match ce.as_deref() {
                Some("gzip") => {
                    #[cfg(feature = "decompression")]
                    {
                        let decompressed = oxiarc_deflate::gzip_decompress(&raw).map_err(|e| {
                            OxiHttpError::Body(format!("gzip decompression error: {e}"))
                        })?;
                        return Ok(Bytes::from(decompressed));
                    }
                    #[cfg(not(feature = "decompression"))]
                    {
                        // Feature not enabled; return raw bytes
                    }
                }
                Some("deflate") => {
                    #[cfg(feature = "decompression")]
                    {
                        let decompressed = oxiarc_deflate::zlib_decompress(&raw)
                            .or_else(|_| {
                                // Some servers send raw DEFLATE without the zlib wrapper
                                oxiarc_deflate::inflate(&raw).map_err(|e| {
                                    OxiHttpError::Body(format!("deflate decompression error: {e}"))
                                })
                            })
                            .map_err(|e| {
                                OxiHttpError::Body(format!("deflate decompression error: {e}"))
                            })?;
                        return Ok(Bytes::from(decompressed));
                    }
                    #[cfg(not(feature = "decompression"))]
                    {
                        // Feature not enabled; return raw bytes
                    }
                }
                _ => {}
            }
        }

        Ok(raw)
    }

    /// Consume the body and return it as a UTF-8 string.
    pub async fn body_text(self) -> Result<String, OxiHttpError> {
        let bytes = self.body_bytes().await?;
        String::from_utf8(bytes.to_vec())
            .map_err(|e| OxiHttpError::Body(format!("invalid UTF-8: {e}")))
    }

    /// Consume the body and deserialize it as JSON.
    pub async fn body_json<T: serde::de::DeserializeOwned>(self) -> Result<T, OxiHttpError> {
        let bytes = self.body_bytes().await?;
        serde_json::from_slice(&bytes).map_err(|e| OxiHttpError::Json(e.to_string()))
    }

    /// Return an error if the response status is a client (4xx) or server (5xx) error.
    ///
    /// Returns `Ok(self)` for success and redirect status codes.
    pub fn error_for_status(self) -> Result<Self, OxiHttpError> {
        let status = self.inner.status();
        if status.is_client_error() || status.is_server_error() {
            Err(OxiHttpError::Body(format!(
                "HTTP error: {} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("Unknown")
            )))
        } else {
            Ok(self)
        }
    }

    /// Returns the `Content-Type` header value as a string, if present.
    pub fn content_type(&self) -> Option<&str> {
        self.inner
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
    }

    /// Parse all `Set-Cookie` response headers using `oxihttp_core::Cookie::parse_set_cookie`.
    ///
    /// Returns an empty `Vec` when there are no `Set-Cookie` headers or none parse
    /// successfully.
    pub fn cookies(&self) -> Vec<oxihttp_core::Cookie> {
        self.inner
            .headers()
            .get_all(http::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(oxihttp_core::Cookie::parse_set_cookie)
            .collect()
    }

    /// Consume the response and return the body as an async stream of chunks.
    pub fn body_stream(self) -> BodyStream {
        BodyStream {
            inner: http_body_util::BodyStream::new(self.inner.into_body()),
        }
    }
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.inner.status())
            .field("version", &self.inner.version())
            .field("headers", self.inner.headers())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// RequestBuilder
// ---------------------------------------------------------------------------

/// Builder for a single HTTP request.
///
/// Created via `Client::get()`, `Client::post()`, etc.
pub struct RequestBuilder<C = HttpConnector> {
    client: HyperClient<C, Full<Bytes>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    timeout: Option<Duration>,
    redirect_policy: RedirectPolicy,
    retry_policy: Option<RetryPolicy>,
    decompression: bool,
    middleware: Vec<Arc<dyn ClientMiddleware>>,
    cookie_jar: Option<Arc<std::sync::Mutex<oxihttp_core::CookieJar>>>,
}

impl<C> RequestBuilder<C>
where
    C: Connect + Clone + Send + Sync + 'static,
{
    #[allow(clippy::too_many_arguments)]
    fn new(
        client: HyperClient<C, Full<Bytes>>,
        method: Method,
        uri: Uri,
        redirect_policy: RedirectPolicy,
        retry_policy: Option<RetryPolicy>,
        decompression: bool,
        middleware: Vec<Arc<dyn ClientMiddleware>>,
        cookie_jar: Option<Arc<std::sync::Mutex<oxihttp_core::CookieJar>>>,
    ) -> Self {
        Self {
            client,
            method,
            uri,
            headers: HeaderMap::new(),
            body: Bytes::new(),
            timeout: None,
            redirect_policy,
            retry_policy,
            decompression,
            middleware,
            cookie_jar,
        }
    }

    /// Add a request header.
    pub fn header(mut self, key: &str, value: &str) -> Result<Self, OxiHttpError> {
        let k =
            HeaderName::from_str(key).map_err(|e| OxiHttpError::InvalidHeader(e.to_string()))?;
        let v =
            HeaderValue::from_str(value).map_err(|e| OxiHttpError::InvalidHeader(e.to_string()))?;
        self.headers.insert(k, v);
        Ok(self)
    }

    /// Add multiple headers from a `HeaderMap`.
    pub fn headers(mut self, map: HeaderMap) -> Self {
        self.headers.extend(map);
        self
    }

    /// Set a Bearer token for the Authorization header.
    pub fn bearer_token(mut self, token: &str) -> Result<Self, OxiHttpError> {
        let v = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| OxiHttpError::InvalidHeader(e.to_string()))?;
        self.headers.insert(http::header::AUTHORIZATION, v);
        Ok(self)
    }

    /// Set Basic authentication for the Authorization header.
    pub fn basic_auth(
        mut self,
        username: &str,
        password: Option<&str>,
    ) -> Result<Self, OxiHttpError> {
        let credentials = match password {
            Some(pw) => format!("{username}:{pw}"),
            None => format!("{username}:"),
        };
        let encoded = base64_encode(credentials.as_bytes());
        let v = HeaderValue::from_str(&format!("Basic {encoded}"))
            .map_err(|e| OxiHttpError::InvalidHeader(e.to_string()))?;
        self.headers.insert(http::header::AUTHORIZATION, v);
        Ok(self)
    }

    /// Set the request body as raw bytes.
    pub fn body(mut self, b: impl Into<Bytes>) -> Self {
        self.body = b.into();
        self
    }

    /// Set the request body as JSON, automatically setting the Content-Type header.
    pub fn json<T: serde::Serialize>(mut self, value: &T) -> Result<Self, OxiHttpError> {
        let json_bytes =
            serde_json::to_vec(value).map_err(|e| OxiHttpError::Json(e.to_string()))?;
        self.body = Bytes::from(json_bytes);
        let ct = HeaderValue::from_static("application/json");
        self.headers.insert(http::header::CONTENT_TYPE, ct);
        Ok(self)
    }

    /// Set the request body as URL-encoded form data.
    pub fn form(mut self, form_body: &oxihttp_core::FormBody) -> Self {
        self.body = form_body.clone().build();
        if let Ok(ct) = HeaderValue::from_str("application/x-www-form-urlencoded") {
            self.headers.insert(http::header::CONTENT_TYPE, ct);
        }
        self
    }

    /// Set the request body from a [`MultipartBuilder`], automatically setting
    /// the `Content-Type: multipart/form-data; boundary=…` header.
    ///
    /// The Content-Type is only set if the caller has not already provided one.
    /// This allows overriding the header with an explicit `.header()` call made
    /// *before* `.multipart()`.
    ///
    /// [`MultipartBuilder`]: oxihttp_core::MultipartBuilder
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
    /// use oxihttp_client::Client;
    /// use oxihttp_core::MultipartBuilder;
    ///
    /// let client = Client::builder().build()?;
    /// let builder = MultipartBuilder::new().add_text("field", "value");
    /// let resp = client.post("http://example.com/upload")?
    ///     .multipart(builder)
    ///     .send()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn multipart(mut self, builder: oxihttp_core::MultipartBuilder) -> Self {
        // Retrieve content_type BEFORE build() because build() consumes the builder.
        let ct_str = builder.content_type();
        self.body = builder.build();
        // Only set Content-Type when the caller has not already provided one.
        if !self.headers.contains_key(http::header::CONTENT_TYPE) {
            if let Ok(ct) = HeaderValue::from_str(&ct_str) {
                self.headers.insert(http::header::CONTENT_TYPE, ct);
            }
        }
        self
    }

    /// Set a per-request timeout.
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(duration);
        self
    }

    /// Send the request and return the response.
    ///
    /// Respects retry policy and per-request timeout.
    /// Before the first attempt the `before_request` hook is called on each
    /// registered middleware; after a successful response `after_response` is
    /// called with the final status and elapsed wall-clock time.
    pub async fn send(self) -> Result<Response, OxiHttpError> {
        let RequestBuilder {
            client,
            method,
            uri,
            headers,
            body,
            timeout,
            redirect_policy,
            retry_policy,
            decompression,
            middleware,
            cookie_jar,
        } = self;

        // --- middleware: before_request -----------------------------------
        {
            let ctx = middleware::RequestContext {
                method: &method,
                uri: &uri,
                headers: &headers,
            };
            for mw in &middleware {
                mw.before_request(&ctx);
            }
        }

        let start = Instant::now();

        let max_attempts = retry_policy
            .as_ref()
            .map(|p| p.max_retries + 1)
            .unwrap_or(1);

        for attempt in 0..max_attempts {
            let result = {
                let fut = send_inner(
                    &client,
                    method.clone(),
                    uri.clone(),
                    body.clone(),
                    headers.clone(),
                    &redirect_policy,
                    decompression,
                    cookie_jar.clone(),
                );
                if let Some(dur) = timeout {
                    match tokio::time::timeout(dur, fut).await {
                        Ok(r) => r,
                        Err(_) => Err(OxiHttpError::Timeout(format!(
                            "request timed out after {}ms",
                            dur.as_millis()
                        ))),
                    }
                } else {
                    fut.await
                }
            };

            match result {
                Ok(resp) => {
                    if let Some(ref policy) = retry_policy {
                        if attempt < max_attempts - 1
                            && policy.should_retry_status(resp.status().as_u16())
                        {
                            let delay = policy.backoff_delay(attempt);
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                    }
                    // --- middleware: after_response ----------------------
                    let elapsed = start.elapsed();
                    let resp_ctx = middleware::ResponseContext {
                        status: resp.status(),
                        elapsed,
                    };
                    for mw in &middleware {
                        mw.after_response(&resp_ctx);
                    }
                    return Ok(resp);
                }
                Err(e) => {
                    if let Some(ref policy) = retry_policy {
                        let should_retry = match &e {
                            OxiHttpError::Hyper(_) => policy.retry_on_connection_error,
                            OxiHttpError::Timeout(_) => policy.retry_on_timeout,
                            OxiHttpError::Io(_) => policy.retry_on_connection_error,
                            _ => false,
                        };
                        if should_retry && attempt < max_attempts - 1 {
                            let delay = policy.backoff_delay(attempt);
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                    }
                    return Err(e);
                }
            }
        }

        // This is unreachable when max_attempts >= 1, but needed for the type checker.
        Err(OxiHttpError::Hyper("max retries exceeded".to_string()))
    }
}

/// Inner request executor: handles redirect loop and returns a `Response`.
///
/// All clone-able fields are passed by value so the outer retry loop can
/// re-invoke this function on each attempt.
#[allow(clippy::too_many_arguments)]
async fn send_inner<C>(
    client: &HyperClient<C, Full<Bytes>>,
    mut method: Method,
    mut uri: Uri,
    mut body: Bytes,
    headers: HeaderMap,
    redirect_policy: &RedirectPolicy,
    decompression: bool,
    cookie_jar: Option<Arc<std::sync::Mutex<oxihttp_core::CookieJar>>>,
) -> Result<Response, OxiHttpError>
where
    C: Connect + Clone + Send + Sync + 'static,
{
    let max_redirects = redirect_policy.max_redirects();
    let mut redirect_count: usize = 0;

    loop {
        let mut req_builder = http::Request::builder()
            .method(method.clone())
            .uri(uri.clone());
        for (k, v) in &headers {
            req_builder = req_builder.header(k, v);
        }

        // Inject Accept-Encoding when decompression is enabled and the user
        // hasn't already set the header.
        if decompression && !headers.contains_key(http::header::ACCEPT_ENCODING) {
            req_builder = req_builder.header(
                http::header::ACCEPT_ENCODING,
                HeaderValue::from_static("gzip, deflate"),
            );
        }

        let mut req = req_builder
            .body(Full::new(body.clone()))
            .map_err(|e| OxiHttpError::Http(Arc::new(e)))?;

        // Inject cookies from jar for this URL
        if let Some(ref jar) = cookie_jar {
            if let Ok(guard) = jar.lock() {
                if let Some(cookie_header) = guard.to_cookie_header_for_url(&uri) {
                    if let Ok(hv) = HeaderValue::from_str(&cookie_header) {
                        req.headers_mut().insert(http::header::COOKIE, hv);
                    }
                }
            }
        }

        let resp = client
            .request(req)
            .await
            .map_err(|e| OxiHttpError::Hyper(e.to_string()))?;

        // Persist Set-Cookie headers into jar
        if let Some(ref jar) = cookie_jar {
            if let Ok(mut guard) = jar.lock() {
                guard.add_from_response_headers(resp.headers(), &uri);
            }
        }

        // Check for redirect
        let status = resp.status();
        if redirect::is_redirect_status(status) {
            if let Some(max) = max_redirects {
                if max == 0 || redirect_count >= max {
                    // Return the redirect response as-is when not following
                    if max == 0 {
                        return Ok(Response {
                            inner: resp,
                            decompress: decompression,
                        });
                    }
                    return Err(OxiHttpError::Redirect(format!(
                        "too many redirects (max: {max})"
                    )));
                }
            }
            redirect_count += 1;

            // Extract the Location header
            let location = resp
                .headers()
                .get(http::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    OxiHttpError::Redirect("redirect response missing Location header".to_string())
                })?;

            // Resolve relative URIs
            let new_uri = resolve_redirect_uri(&uri, location)?;

            // Update method (POST -> GET for 301/302/303)
            let new_method = redirect::redirect_method(status, &method);

            // Clear body if method changed away from body-carrying
            if !redirect::should_preserve_body(status) {
                body = Bytes::new();
            }

            method = new_method;
            uri = new_uri;
            continue;
        }

        return Ok(Response {
            inner: resp,
            decompress: decompression,
        });
    }
}

/// Resolve a redirect URI, handling both absolute and relative URIs.
fn resolve_redirect_uri(base: &Uri, location: &str) -> Result<Uri, OxiHttpError> {
    // Try parsing as absolute URI first
    if let Ok(uri) = Uri::from_str(location) {
        if uri.scheme().is_some() {
            return Ok(uri);
        }
    }

    // Relative URI: combine with base
    let scheme = base.scheme_str().unwrap_or("http");
    let authority = base.authority().map(|a| a.as_str()).unwrap_or("localhost");
    let full = format!("{scheme}://{authority}{location}");
    Uri::from_str(&full).map_err(|e| OxiHttpError::InvalidUri(Arc::new(e)))
}

// TlsRebuildConfig — stores all TLS + pool params needed to re-create an
// HttpsClient with modified trust settings (used by with_request_tls_config).
#[cfg(feature = "tls")]
#[derive(Clone)]
pub(crate) struct TlsRebuildConfig {
    pub trusted_certs_der: Vec<Vec<u8>>,
    pub alpn: Vec<String>,
    pub accept_invalid_certs: bool,
    pub use_webpki_roots: bool,
    pub key_log_path: Option<std::path::PathBuf>,
    pub early_data: bool,
    pub connect_timeout: Option<Duration>,
    pub tcp_nodelay: Option<bool>,
    pub tcp_keepalive: Option<Duration>,
    pub http2_settings: Option<Http2Settings>,
    pub pool_max_idle_per_host: Option<usize>,
    pub pool_idle_timeout: Option<Duration>,
    /// Optional custom certificate verifier injected via
    /// [`ClientBuilder::with_custom_cert_verifier`].  When `Some`, this verifier
    /// takes precedence over all other trust-store settings.
    pub custom_cert_verifier: Option<Arc<dyn rustls::client::danger::ServerCertVerifier>>,
}

#[cfg(feature = "tls")]
impl std::fmt::Debug for TlsRebuildConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsRebuildConfig")
            .field("trusted_certs_der_count", &self.trusted_certs_der.len())
            .field("alpn", &self.alpn)
            .field("accept_invalid_certs", &self.accept_invalid_certs)
            .field("use_webpki_roots", &self.use_webpki_roots)
            .field("early_data", &self.early_data)
            .field("connect_timeout", &self.connect_timeout)
            .field("tcp_nodelay", &self.tcp_nodelay)
            .field("tcp_keepalive", &self.tcp_keepalive)
            .field(
                "custom_cert_verifier",
                &self
                    .custom_cert_verifier
                    .as_ref()
                    .map(|_| "<dyn ServerCertVerifier>"),
            )
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Client<C>
// ---------------------------------------------------------------------------

/// HTTP client with connection pooling, redirect handling, and retry support.
///
/// The default type parameter `C = HttpConnector` gives a plain HTTP-only
/// client. Use `HttpsClient` (feature `tls`) for a TLS-capable client.
///
/// Created via `Client::builder().build()` or `Client::builder().build_https()`.
#[derive(Clone)]
pub struct Client<C = HttpConnector> {
    pub(crate) inner: HyperClient<C, Full<Bytes>>,
    pub(crate) redirect_policy: RedirectPolicy,
    pub(crate) retry_policy: Option<RetryPolicy>,
    pub(crate) default_headers: HeaderMap,
    pub(crate) connect_timeout: Option<Duration>,
    pub(crate) read_timeout: Option<Duration>,
    pub(crate) decompression: bool,
    /// Ordered list of middleware interceptors applied to every request.
    pub(crate) middleware: Vec<Arc<dyn ClientMiddleware>>,
    /// Optional shared cookie jar for automatic RFC 6265 cookie management.
    pub(crate) cookie_jar: Option<Arc<std::sync::Mutex<oxihttp_core::CookieJar>>>,
    /// TLS rebuild parameters, populated only for [`HttpsClient`] instances.
    ///
    /// Used by [`HttpsClient::with_request_tls_config`] to construct a fresh
    /// client with modified TLS trust settings.
    #[cfg(feature = "tls")]
    pub(crate) tls_rebuild: Option<Arc<TlsRebuildConfig>>,
}

/// A TLS-capable client that supports both `http://` and `https://` URIs.
///
/// Created via `Client::builder().build_https()`.
#[cfg(feature = "tls")]
pub type HttpsClient = Client<OxiHttpsConnector<HttpConnector>>;

/// An HTTP client using a custom DNS resolver (plain HTTP).
///
/// Created via `Client::builder().with_resolver(r).build_with_resolver()`.
pub type ResolverClient = Client<HttpConnector<BoxResolver>>;

/// An HTTP client using a custom DNS resolver with TLS support.
///
/// Created via `Client::builder().with_resolver(r).build_https_with_resolver()`.
#[cfg(feature = "tls")]
pub type ResolverHttpsClient =
    Client<crate::connector::OxiHttpsConnector<HttpConnector<BoxResolver>>>;

/// Provide `builder()` only on the default `Client<HttpConnector>` variant so
/// that type-inference works without annotation at call sites.
impl Client<HttpConnector> {
    /// Return a `ClientBuilder` for configuring a new client.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::new()
    }
}

// ---------------------------------------------------------------------------
// HttpsClient — per-request TLS config override
// ---------------------------------------------------------------------------

/// Per-request TLS overrides for [`HttpsClient`].
///
/// These methods are only available on clients built via
/// [`ClientBuilder::build_https`].
#[cfg(feature = "tls")]
impl Client<OxiHttpsConnector<HttpConnector>> {
    /// Return a new `HttpsClient` that shares all settings with `self` except
    /// for the TLS trust configuration, which is replaced by `override_cfg`.
    ///
    /// The returned client has its **own independent connection pool**.  Use it
    /// to make requests that require different TLS trust than the original
    /// client (e.g., certificate pinning to a different CA).
    ///
    /// # Errors
    ///
    /// Returns an error if the TLS connector cannot be built from the merged
    /// configuration (e.g., a supplied DER-encoded certificate is malformed).
    ///
    /// # Notes on connection pooling
    ///
    /// Because the returned client uses a separate pool, it will always open a
    /// fresh connection even if the original client already has an idle
    /// connection to the same host.  This guarantees that the override TLS
    /// config is applied.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use oxihttp_client::{Client, request_config::RequestTlsConfig};
    /// # async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
    /// let global_client = Client::builder()
    ///     .with_trusted_cert_der(vec![/* CA cert A DER … */])
    ///     .build_https()?;
    ///
    /// // Override: trust CA cert B instead of CA cert A for a single request.
    /// let pinned = global_client.with_request_tls_config(
    ///     RequestTlsConfig::new().with_trusted_cert(vec![/* CA cert B DER … */]),
    /// )?;
    /// let resp = pinned.get("https://pinned-endpoint.example.com")?.send().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_request_tls_config(
        &self,
        override_cfg: RequestTlsConfig,
    ) -> Result<Self, OxiHttpError> {
        use crate::connector::OxiHttpsConnector;

        let base = self.tls_rebuild.as_ref().ok_or_else(|| {
            OxiHttpError::Tls(
                "client has no TLS rebuild config (was it built with build_https()?)".to_string(),
            )
        })?;

        // Merge: per-request overrides win over global config.
        let effective_certs = if override_cfg.trusted_cert_ders.is_empty() {
            base.trusted_certs_der.as_slice()
        } else {
            override_cfg.trusted_cert_ders.as_slice()
        };
        let accept_invalid = base.accept_invalid_certs || override_cfg.accept_invalid_certs;

        // If a custom verifier is installed on the base config, use the
        // verifier-path builder so the custom verifier is preserved.
        let new_tls = if let Some(ref verifier) = base.custom_cert_verifier {
            tls::build_tls_connector_with_verifier(
                Arc::clone(verifier),
                &base.alpn,
                base.early_data,
            )?
        } else {
            tls::build_tls_connector(
                effective_certs,
                &base.alpn,
                accept_invalid,
                base.use_webpki_roots,
                base.key_log_path.clone(),
                base.early_data,
            )?
        };

        let mut http = HttpConnector::new();
        http.enforce_http(false);
        if let Some(dur) = base.connect_timeout {
            http.set_connect_timeout(Some(dur));
        }
        if let Some(nodelay) = base.tcp_nodelay {
            http.set_nodelay(nodelay);
        }
        if let Some(ka) = base.tcp_keepalive {
            http.set_keepalive(Some(ka));
        }
        let https_connector = OxiHttpsConnector::new(http, new_tls);

        let mut hb = HyperClient::builder(TokioExecutor::new());
        if let Some(n) = base.pool_max_idle_per_host {
            hb.pool_max_idle_per_host(n);
        }
        if let Some(dur) = base.pool_idle_timeout {
            hb.pool_idle_timeout(dur);
        }
        if let Some(ref h2) = base.http2_settings {
            apply_http2_settings(&mut hb, h2);
        }

        // Build a new TlsRebuildConfig reflecting the merged settings so that
        // further calls to `with_request_tls_config` on the returned client
        // start from a consistent state.
        let new_rebuild = Arc::new(TlsRebuildConfig {
            trusted_certs_der: effective_certs.to_vec(),
            alpn: base.alpn.clone(),
            accept_invalid_certs: accept_invalid,
            use_webpki_roots: base.use_webpki_roots,
            key_log_path: base.key_log_path.clone(),
            early_data: base.early_data,
            connect_timeout: base.connect_timeout,
            tcp_nodelay: base.tcp_nodelay,
            tcp_keepalive: base.tcp_keepalive,
            http2_settings: base.http2_settings.clone(),
            pool_max_idle_per_host: base.pool_max_idle_per_host,
            pool_idle_timeout: base.pool_idle_timeout,
            custom_cert_verifier: base.custom_cert_verifier.clone(),
        });

        Ok(Client {
            inner: hb.build(https_connector),
            redirect_policy: self.redirect_policy.clone(),
            retry_policy: self.retry_policy.clone(),
            default_headers: self.default_headers.clone(),
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
            decompression: self.decompression,
            middleware: self.middleware.clone(),
            cookie_jar: self.cookie_jar.clone(),
            tls_rebuild: Some(new_rebuild),
        })
    }
}

impl<C> Client<C>
where
    C: Connect + Clone + Send + Sync + 'static,
{
    /// Create a request builder for the given method and URL.
    fn request_builder(
        &self,
        method: Method,
        url: &str,
    ) -> Result<RequestBuilder<C>, OxiHttpError> {
        let uri = Uri::from_str(url)?;
        let mut rb = RequestBuilder::new(
            self.inner.clone(),
            method,
            uri,
            self.redirect_policy.clone(),
            self.retry_policy.clone(),
            self.decompression,
            self.middleware.clone(),
            self.cookie_jar.clone(),
        );
        // Apply default headers
        for (k, v) in &self.default_headers {
            rb.headers.insert(k.clone(), v.clone());
        }
        Ok(rb)
    }

    /// Build a GET request for the given URL.
    pub fn get(&self, url: &str) -> Result<RequestBuilder<C>, OxiHttpError> {
        self.request_builder(Method::GET, url)
    }

    /// Build a POST request for the given URL.
    pub fn post(&self, url: &str) -> Result<RequestBuilder<C>, OxiHttpError> {
        self.request_builder(Method::POST, url)
    }

    /// Build a PUT request for the given URL.
    pub fn put(&self, url: &str) -> Result<RequestBuilder<C>, OxiHttpError> {
        self.request_builder(Method::PUT, url)
    }

    /// Build a DELETE request for the given URL.
    pub fn delete(&self, url: &str) -> Result<RequestBuilder<C>, OxiHttpError> {
        self.request_builder(Method::DELETE, url)
    }

    /// Build a PATCH request for the given URL.
    pub fn patch(&self, url: &str) -> Result<RequestBuilder<C>, OxiHttpError> {
        self.request_builder(Method::PATCH, url)
    }

    /// Build a HEAD request for the given URL.
    pub fn head(&self, url: &str) -> Result<RequestBuilder<C>, OxiHttpError> {
        self.request_builder(Method::HEAD, url)
    }

    /// Execute a pre-built `http::Request`.
    pub async fn execute(&self, req: http::Request<Full<Bytes>>) -> Result<Response, OxiHttpError> {
        let resp = self
            .inner
            .request(req)
            .await
            .map_err(|e| OxiHttpError::Hyper(e.to_string()))?;
        Ok(Response {
            inner: resp,
            decompress: self.decompression,
        })
    }

    /// Convenience: GET the URL and return the response body as bytes.
    pub async fn get_bytes(&self, url: &str) -> Result<Bytes, OxiHttpError> {
        let resp = self.get(url)?.send().await?;
        resp.error_for_status()?.body_bytes().await
    }

    /// Convenience: GET the URL and deserialize the JSON response body.
    pub async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<T, OxiHttpError> {
        let resp = self.get(url)?.send().await?;
        resp.error_for_status()?.body_json().await
    }

    /// Convenience: POST JSON and deserialize the response.
    pub async fn post_json<T: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &T,
    ) -> Result<R, OxiHttpError> {
        let resp = self.post(url)?.json(body)?.send().await?;
        resp.error_for_status()?.body_json().await
    }

    /// Returns a reference to the retry policy, if configured.
    pub fn retry_policy(&self) -> Option<&RetryPolicy> {
        self.retry_policy.as_ref()
    }

    /// Returns a reference to the connect timeout, if set.
    pub fn connect_timeout(&self) -> Option<Duration> {
        self.connect_timeout
    }

    /// Returns a reference to the read timeout, if set.
    pub fn read_timeout(&self) -> Option<Duration> {
        self.read_timeout
    }
}

impl<C> std::fmt::Debug for Client<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("redirect_policy", &self.redirect_policy)
            .field("retry_policy", &self.retry_policy)
            .field("default_headers_count", &self.default_headers.len())
            .finish()
    }
}

/// Simple base64 encoding (RFC 4648) without external dependency.
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use oxihttp_core::MultipartBuilder;

    /// Helper: build a plain-HTTP client and a POST RequestBuilder targeting a
    /// dummy URL. The builder is never actually sent, so the URL doesn't need to
    /// resolve — we only inspect the headers that would be set.
    fn post_builder() -> RequestBuilder {
        let client = Client::builder().build().expect("client build");
        client
            .post("http://127.0.0.1:0/test")
            .expect("request builder")
    }

    /// `.multipart()` without a prior Content-Type must auto-set
    /// `multipart/form-data; boundary=…` including the exact boundary value.
    #[test]
    fn multipart_sets_content_type_automatically() {
        let mp = MultipartBuilder::new().add_text("field", "value");
        // Capture boundary before the builder is consumed by .multipart().
        let expected_boundary = mp.boundary().to_owned();

        let rb = post_builder().multipart(mp);

        let ct = rb
            .headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .expect("Content-Type header must be set after .multipart()");

        assert!(
            ct.starts_with("multipart/form-data; boundary="),
            "Content-Type must start with multipart/form-data; boundary= but got: {ct}"
        );
        assert!(
            ct.contains(&expected_boundary),
            "Content-Type must contain the boundary '{expected_boundary}' but got: {ct}"
        );
    }

    /// If the caller sets Content-Type *before* `.multipart()`, the explicit
    /// header must be preserved (not overridden by the auto-detection).
    #[test]
    fn multipart_does_not_override_explicit_content_type() {
        let mp = MultipartBuilder::new().add_text("x", "y");

        let rb = post_builder()
            .header("content-type", "application/octet-stream")
            .expect("header set")
            .multipart(mp);

        let ct = rb
            .headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .expect("Content-Type header must be present");

        assert_eq!(
            ct, "application/octet-stream",
            "explicit Content-Type must not be overridden by .multipart()"
        );
    }
}
