//! [`ClientBuilder`] and related HTTP/2 configuration types.
//!
//! Extracted from `lib.rs` to keep individual source files under the 2 000-line
//! policy limit.  All public items are re-exported from the crate root so the
//! external API is unchanged.

use http::{HeaderMap, HeaderValue, Uri};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::rt::TokioExecutor;
use oxihttp_core::OxiHttpError;
use std::sync::Arc;
use std::time::Duration;

use crate::middleware::ClientMiddleware;
use crate::proxy::{ProxyConnector, ProxyKind};
use crate::redirect::RedirectPolicy;
use crate::resolver::BoxResolver;
use crate::retry::RetryPolicy;

#[cfg(feature = "socks")]
use crate::proxy::Socks5Connector;

#[cfg(feature = "tls")]
use crate::connector::OxiHttpsConnector;
#[cfg(feature = "tls")]
use crate::tls;

// Re-export the Client struct and type aliases so build methods can reference them.
use super::Client;
#[cfg(feature = "tls")]
use super::TlsRebuildConfig;

// ---------------------------------------------------------------------------
// Http2Settings
// ---------------------------------------------------------------------------

/// HTTP/2 connection settings for the client.
#[derive(Debug, Clone, Default)]
pub struct Http2Settings {
    /// Initial window size for stream-level flow control (bytes).
    pub initial_stream_window_size: Option<u32>,
    /// Initial window size for connection-level flow control (bytes).
    pub initial_connection_window_size: Option<u32>,
    /// Enable adaptive flow control (overrides window size settings when set).
    pub adaptive_window: Option<bool>,
    /// Interval for HTTP/2 PING keep-alive frames.
    pub keep_alive_interval: Option<std::time::Duration>,
    /// Timeout for acknowledgement of keep-alive PING before closing.
    pub keep_alive_timeout: Option<std::time::Duration>,
    /// Maximum HTTP/2 frame size (bytes).
    pub max_frame_size: Option<u32>,
    /// Maximum number of concurrent locally-reset streams.
    pub max_concurrent_reset_streams: Option<usize>,
    /// Maximum write buffer size per HTTP/2 stream (bytes).
    pub max_send_buf_size: Option<usize>,
}

// ---------------------------------------------------------------------------
// apply_http2_settings — shared helper for all client build paths
// ---------------------------------------------------------------------------

pub(crate) fn apply_http2_settings(
    builder: &mut hyper_util::client::legacy::Builder,
    settings: &Http2Settings,
) {
    if let Some(sz) = settings.initial_stream_window_size {
        builder.http2_initial_stream_window_size(sz);
    }
    if let Some(sz) = settings.initial_connection_window_size {
        builder.http2_initial_connection_window_size(sz);
    }
    if let Some(adaptive) = settings.adaptive_window {
        builder.http2_adaptive_window(adaptive);
    }
    if let Some(interval) = settings.keep_alive_interval {
        builder.http2_keep_alive_interval(interval);
    }
    if let Some(timeout) = settings.keep_alive_timeout {
        builder.http2_keep_alive_timeout(timeout);
    }
    if let Some(sz) = settings.max_frame_size {
        builder.http2_max_frame_size(sz);
    }
    if let Some(n) = settings.max_concurrent_reset_streams {
        builder.http2_max_concurrent_reset_streams(n);
    }
    if let Some(sz) = settings.max_send_buf_size {
        builder.http2_max_send_buf_size(sz);
    }
}

// ---------------------------------------------------------------------------
// ClientBuilder
// ---------------------------------------------------------------------------

/// A builder for constructing a `Client` with custom configuration.
pub struct ClientBuilder {
    pub(super) pool_max_idle_per_host: Option<usize>,
    pub(super) pool_idle_timeout: Option<Duration>,
    pub(super) connect_timeout: Option<Duration>,
    pub(super) read_timeout: Option<Duration>,
    pub(super) redirect_policy: RedirectPolicy,
    pub(super) retry_policy: Option<RetryPolicy>,
    pub(super) default_headers: HeaderMap,
    pub(super) user_agent: Option<String>,
    pub(super) decompression: bool,
    /// Middleware interceptors applied to every request/response.
    pub(super) middleware: Vec<Arc<dyn ClientMiddleware>>,
    /// Optional proxy configuration.
    pub(super) proxy: Option<ProxyKind>,
    /// Optional shared cookie jar for automatic cookie management.
    pub(super) cookie_jar: Option<Arc<std::sync::Mutex<oxihttp_core::CookieJar>>>,
    /// HTTP/2 tuning settings (applied to all build paths that support H2).
    pub(super) http2_settings: Option<Http2Settings>,
    /// TCP_NODELAY setting for all outgoing connections.
    pub(super) tcp_nodelay: Option<bool>,
    /// TCP keepalive idle time for all outgoing connections.
    pub(super) tcp_keepalive: Option<Duration>,
    /// Custom DNS resolver (used with build_with_resolver / build_https_with_resolver).
    pub(super) resolver: Option<Arc<dyn crate::resolver::DnsResolver>>,
    // TLS options (only active when `tls` feature is enabled)
    #[cfg(feature = "tls")]
    pub(super) trusted_certs_der: Vec<Vec<u8>>,
    #[cfg(feature = "tls")]
    pub(super) alpn: Vec<String>,
    #[cfg(feature = "tls")]
    pub(super) accept_invalid_certs: bool,
    #[cfg(feature = "tls")]
    pub(super) use_webpki_roots: bool,
    /// Optional path for SSLKEYLOGFILE-style key logging (development/debugging only).
    #[cfg(feature = "tls")]
    pub(super) key_log_path: Option<std::path::PathBuf>,
    /// Enable TLS 1.3 0-RTT early data (HTTP fast-open) for subsequent requests.
    #[cfg(feature = "tls")]
    pub(super) early_data: bool,
    /// Optional custom server-certificate verifier.  When `Some`, takes
    /// precedence over `accept_invalid_certs` and all trust-store settings.
    #[cfg(feature = "tls")]
    pub(super) custom_cert_verifier:
        Option<std::sync::Arc<dyn rustls::client::danger::ServerCertVerifier>>,
}

impl ClientBuilder {
    /// Create a new `ClientBuilder` with default settings.
    pub fn new() -> Self {
        Self {
            pool_max_idle_per_host: None,
            pool_idle_timeout: None,
            connect_timeout: None,
            read_timeout: None,
            redirect_policy: RedirectPolicy::default(),
            retry_policy: None,
            default_headers: HeaderMap::new(),
            user_agent: None,
            decompression: false,
            middleware: Vec::new(),
            proxy: None,
            cookie_jar: None,
            http2_settings: None,
            tcp_nodelay: None,
            tcp_keepalive: None,
            resolver: None,
            #[cfg(feature = "tls")]
            trusted_certs_der: Vec::new(),
            #[cfg(feature = "tls")]
            alpn: Vec::new(),
            #[cfg(feature = "tls")]
            accept_invalid_certs: false,
            #[cfg(feature = "tls")]
            use_webpki_roots: false,
            #[cfg(feature = "tls")]
            key_log_path: None,
            #[cfg(feature = "tls")]
            early_data: false,
            #[cfg(feature = "tls")]
            custom_cert_verifier: None,
        }
    }

    /// Set the maximum number of idle connections per host in the pool.
    pub fn pool_max_idle_per_host(mut self, n: usize) -> Self {
        self.pool_max_idle_per_host = Some(n);
        self
    }

    /// Set the idle timeout for pooled connections.
    pub fn pool_idle_timeout(mut self, duration: Duration) -> Self {
        self.pool_idle_timeout = Some(duration);
        self
    }

    /// Set the TCP connect timeout.
    pub fn connect_timeout(mut self, duration: Duration) -> Self {
        self.connect_timeout = Some(duration);
        self
    }

    /// Set the response read timeout.
    pub fn read_timeout(mut self, duration: Duration) -> Self {
        self.read_timeout = Some(duration);
        self
    }

    /// Set the redirect policy.
    pub fn redirect_policy(mut self, policy: RedirectPolicy) -> Self {
        self.redirect_policy = policy;
        self
    }

    /// Set the retry policy.
    pub fn retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = Some(policy);
        self
    }

    /// Set default headers to include on every request.
    pub fn default_headers(mut self, headers: HeaderMap) -> Self {
        self.default_headers = headers;
        self
    }

    /// Set the User-Agent header for all requests.
    pub fn user_agent(mut self, agent: impl Into<String>) -> Self {
        self.user_agent = Some(agent.into());
        self
    }

    /// Enable or disable automatic response decompression.
    ///
    /// When enabled the client adds `Accept-Encoding: gzip, deflate` to
    /// outgoing requests and transparently decompresses the response body
    /// based on the `Content-Encoding` header (requires `decompression`
    /// feature).
    pub fn with_decompression(mut self, enabled: bool) -> Self {
        self.decompression = enabled;
        self
    }

    // --- middleware --------------------------------------------------------

    /// Register a request/response middleware interceptor.
    ///
    /// Middleware is invoked in registration order: `before_request` fires
    /// in FIFO order before the first network attempt; `after_response` fires
    /// in FIFO order after a successful final response.
    ///
    /// Multiple calls to this method append to the middleware stack.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use oxihttp_client::{Client, middleware::LoggingMiddleware};
    ///
    /// let client = Client::builder()
    ///     .with_middleware(LoggingMiddleware::new("api"))
    ///     .build()
    ///     .expect("client build");
    /// ```
    pub fn with_middleware<M: ClientMiddleware + 'static>(mut self, m: M) -> Self {
        self.middleware.push(Arc::new(m));
        self
    }

    /// Alias for [`ClientBuilder::with_middleware`].
    ///
    /// Provided so that callers familiar with the `tower::Layer` terminology
    /// can use the same name.  This does **not** accept a `tower::Layer`; for
    /// a full tower `Service` composition see the `tower_compat` module docs.
    pub fn with_layer<M: ClientMiddleware + 'static>(self, m: M) -> Self {
        self.with_middleware(m)
    }

    // --- cookie jar builder methods ------------------------------------------

    /// Configure the client to use the given shared cookie jar for automatic cookie management.
    pub fn with_cookie_jar(mut self, jar: Arc<std::sync::Mutex<oxihttp_core::CookieJar>>) -> Self {
        self.cookie_jar = Some(jar);
        self
    }

    /// Configure the client to create and use a fresh cookie jar automatically.
    pub fn with_new_cookie_jar(mut self) -> Self {
        self.cookie_jar = Some(Arc::new(std::sync::Mutex::new(
            oxihttp_core::CookieJar::new(),
        )));
        self
    }

    // --- HTTP/2 and TCP tuning builder methods --------------------------------

    /// Set HTTP/2 connection tuning parameters.
    pub fn with_http2_settings(mut self, settings: Http2Settings) -> Self {
        self.http2_settings = Some(settings);
        self
    }

    /// Enable or disable `TCP_NODELAY` on all outgoing connections.
    pub fn with_tcp_nodelay(mut self, nodelay: bool) -> Self {
        self.tcp_nodelay = Some(nodelay);
        self
    }

    /// Set the TCP keepalive idle time for all outgoing connections.
    pub fn with_tcp_keepalive(mut self, duration: Duration) -> Self {
        self.tcp_keepalive = Some(duration);
        self
    }

    // --- custom DNS resolver builder methods ---------------------------------

    /// Set a custom DNS resolver for all connections made by this client.
    ///
    /// After calling this, use [`ClientBuilder::build_with_resolver`] (plain HTTP)
    /// or [`ClientBuilder::build_https_with_resolver`] (TLS) to construct the client.
    pub fn with_resolver<R: crate::resolver::DnsResolver>(mut self, r: R) -> Self {
        self.resolver = Some(Arc::new(r));
        self
    }

    // --- proxy builder methods -----------------------------------------------

    /// Route all requests through an HTTP CONNECT proxy.
    ///
    /// Call `build_proxy()` (plain HTTP target) or `build_proxy_https()` (HTTPS
    /// target, requires `tls` feature) after setting this.
    pub fn with_http_proxy(mut self, uri: Uri) -> Self {
        self.proxy = Some(ProxyKind::HttpConnect(uri));
        self
    }

    /// Route all requests through a SOCKS5 proxy.
    ///
    /// Call `build_socks5_proxy()` (plain HTTP) or `build_socks5_proxy_https()`
    /// (HTTPS, requires `tls` feature) after setting this.
    #[cfg(feature = "socks")]
    pub fn with_socks5_proxy(mut self, uri: Uri) -> Self {
        self.proxy = Some(ProxyKind::Socks5(uri));
        self
    }

    /// Build a plain-HTTP `Client` routed through an HTTP CONNECT proxy.
    ///
    /// `with_http_proxy()` must have been called before this.
    pub fn build_proxy(self) -> Result<Client<ProxyConnector>, OxiHttpError> {
        let proxy_uri = match self.proxy.as_ref() {
            Some(ProxyKind::HttpConnect(u)) => u.clone(),
            #[cfg(feature = "socks")]
            Some(ProxyKind::Socks5(_)) => {
                return Err(OxiHttpError::ConnectionPool(
                    "SOCKS5 proxy configured; use build_socks5_proxy() instead".into(),
                ))
            }
            None => {
                return Err(OxiHttpError::ConnectionPool(
                    "no proxy configured; call with_http_proxy() first".into(),
                ))
            }
        };
        let connector = ProxyConnector::new(proxy_uri, self.connect_timeout);
        let mut builder = HyperClient::builder(TokioExecutor::new());
        if let Some(n) = self.pool_max_idle_per_host {
            builder.pool_max_idle_per_host(n);
        }
        if let Some(dur) = self.pool_idle_timeout {
            builder.pool_idle_timeout(dur);
        }
        if let Some(ref h2) = self.http2_settings {
            apply_http2_settings(&mut builder, h2);
        }
        let inner = builder.build(connector);
        let mut default_headers = self.default_headers;
        if let Some(agent) = &self.user_agent {
            if let Ok(val) = HeaderValue::from_str(agent) {
                default_headers.insert(http::header::USER_AGENT, val);
            }
        }
        Ok(Client {
            inner,
            redirect_policy: self.redirect_policy,
            retry_policy: self.retry_policy,
            default_headers,
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
            decompression: self.decompression,
            middleware: self.middleware,
            cookie_jar: self.cookie_jar.clone(),
            #[cfg(feature = "tls")]
            tls_rebuild: None,
        })
    }

    /// Build a plain-HTTP `Client` routed through a SOCKS5 proxy.
    ///
    /// `with_socks5_proxy()` must have been called before this.
    #[cfg(feature = "socks")]
    pub fn build_socks5_proxy(self) -> Result<Client<Socks5Connector>, OxiHttpError> {
        let proxy_uri = match self.proxy.as_ref() {
            Some(ProxyKind::Socks5(u)) => u.clone(),
            Some(ProxyKind::HttpConnect(_)) => {
                return Err(OxiHttpError::ConnectionPool(
                    "HTTP CONNECT proxy configured; use build_proxy() instead".into(),
                ))
            }
            None => {
                return Err(OxiHttpError::ConnectionPool(
                    "no proxy configured; call with_socks5_proxy() first".into(),
                ))
            }
        };
        let connector = Socks5Connector::new(proxy_uri, self.connect_timeout);
        let mut builder = HyperClient::builder(TokioExecutor::new());
        if let Some(n) = self.pool_max_idle_per_host {
            builder.pool_max_idle_per_host(n);
        }
        if let Some(dur) = self.pool_idle_timeout {
            builder.pool_idle_timeout(dur);
        }
        if let Some(ref h2) = self.http2_settings {
            apply_http2_settings(&mut builder, h2);
        }
        let inner = builder.build(connector);
        let mut default_headers = self.default_headers;
        if let Some(agent) = &self.user_agent {
            if let Ok(val) = HeaderValue::from_str(agent) {
                default_headers.insert(http::header::USER_AGENT, val);
            }
        }
        Ok(Client {
            inner,
            redirect_policy: self.redirect_policy,
            retry_policy: self.retry_policy,
            default_headers,
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
            decompression: self.decompression,
            middleware: self.middleware,
            cookie_jar: self.cookie_jar.clone(),
            #[cfg(feature = "tls")]
            tls_rebuild: None,
        })
    }

    /// Build a TLS-capable `Client` that tunnels HTTPS through an HTTP CONNECT proxy.
    ///
    /// `with_http_proxy()` must have been called before this.
    ///
    /// The resulting client handles both `http://` and `https://` URIs.
    #[cfg(feature = "tls")]
    pub fn build_proxy_https(
        self,
    ) -> Result<Client<OxiHttpsConnector<ProxyConnector>>, OxiHttpError> {
        let proxy_uri = match self.proxy.as_ref() {
            Some(ProxyKind::HttpConnect(u)) => u.clone(),
            #[cfg(feature = "socks")]
            Some(ProxyKind::Socks5(_)) => {
                return Err(OxiHttpError::ConnectionPool(
                    "SOCKS5 proxy configured; use build_socks5_proxy_https() instead".into(),
                ))
            }
            None => {
                return Err(OxiHttpError::ConnectionPool(
                    "no proxy configured; call with_http_proxy() first".into(),
                ))
            }
        };

        let tls_connector = self.build_tls_connector_inner()?;

        let http_connector = ProxyConnector::new(proxy_uri, self.connect_timeout);
        let https_connector = OxiHttpsConnector::new(http_connector, tls_connector);

        let mut builder = HyperClient::builder(TokioExecutor::new());
        if let Some(n) = self.pool_max_idle_per_host {
            builder.pool_max_idle_per_host(n);
        }
        if let Some(dur) = self.pool_idle_timeout {
            builder.pool_idle_timeout(dur);
        }
        if let Some(ref h2) = self.http2_settings {
            apply_http2_settings(&mut builder, h2);
        }
        let inner = builder.build(https_connector);
        let mut default_headers = self.default_headers;
        if let Some(agent) = &self.user_agent {
            if let Ok(val) = HeaderValue::from_str(agent) {
                default_headers.insert(http::header::USER_AGENT, val);
            }
        }
        Ok(Client {
            inner,
            redirect_policy: self.redirect_policy,
            retry_policy: self.retry_policy,
            default_headers,
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
            decompression: self.decompression,
            middleware: self.middleware,
            cookie_jar: self.cookie_jar.clone(),
            #[cfg(feature = "tls")]
            tls_rebuild: None,
        })
    }

    /// Build a TLS-capable `Client` that tunnels HTTPS through a SOCKS5 proxy.
    ///
    /// `with_socks5_proxy()` must have been called before this.
    ///
    /// The resulting client handles both `http://` and `https://` URIs.
    #[cfg(all(feature = "tls", feature = "socks"))]
    pub fn build_socks5_proxy_https(
        self,
    ) -> Result<Client<OxiHttpsConnector<Socks5Connector>>, OxiHttpError> {
        let proxy_uri = match self.proxy.as_ref() {
            Some(ProxyKind::Socks5(u)) => u.clone(),
            Some(ProxyKind::HttpConnect(_)) => {
                return Err(OxiHttpError::ConnectionPool(
                    "HTTP CONNECT proxy configured; use build_proxy_https() instead".into(),
                ))
            }
            None => {
                return Err(OxiHttpError::ConnectionPool(
                    "no proxy configured; call with_socks5_proxy() first".into(),
                ))
            }
        };

        let tls_connector = self.build_tls_connector_inner()?;

        let socks_connector = Socks5Connector::new(proxy_uri, self.connect_timeout);
        let https_connector = OxiHttpsConnector::new(socks_connector, tls_connector);

        let mut builder = HyperClient::builder(TokioExecutor::new());
        if let Some(n) = self.pool_max_idle_per_host {
            builder.pool_max_idle_per_host(n);
        }
        if let Some(dur) = self.pool_idle_timeout {
            builder.pool_idle_timeout(dur);
        }
        if let Some(ref h2) = self.http2_settings {
            apply_http2_settings(&mut builder, h2);
        }
        let inner = builder.build(https_connector);
        let mut default_headers = self.default_headers;
        if let Some(agent) = &self.user_agent {
            if let Ok(val) = HeaderValue::from_str(agent) {
                default_headers.insert(http::header::USER_AGENT, val);
            }
        }
        Ok(Client {
            inner,
            redirect_policy: self.redirect_policy,
            retry_policy: self.retry_policy,
            default_headers,
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
            decompression: self.decompression,
            middleware: self.middleware,
            cookie_jar: self.cookie_jar.clone(),
            #[cfg(feature = "tls")]
            tls_rebuild: None,
        })
    }

    // --- TLS-specific builder methods (feature-gated) -----------------------

    /// Enable TLS with the Mozilla CA bundle (webpki-roots) as the trust store.
    ///
    /// Required to be called (or `with_webpki_roots` / `with_trusted_cert_der`)
    /// before `build_https()` returns a usable client for real HTTPS endpoints.
    #[cfg(feature = "tls")]
    pub fn with_tls(mut self) -> Self {
        self.use_webpki_roots = true;
        self
    }

    /// Trust the Mozilla CA bundle (webpki-roots).
    #[cfg(feature = "tls")]
    pub fn with_webpki_roots(mut self) -> Self {
        self.use_webpki_roots = true;
        self
    }

    /// Trust an additional DER-encoded CA certificate.
    ///
    /// Can be called multiple times to add several trusted roots.
    #[cfg(feature = "tls")]
    pub fn with_trusted_cert_der(mut self, der: Vec<u8>) -> Self {
        self.trusted_certs_der.push(der);
        self
    }

    /// Set ALPN protocols to advertise (e.g. `&["h2", "http/1.1"]`).
    #[cfg(feature = "tls")]
    pub fn with_alpn(mut self, protocols: &[&str]) -> Self {
        self.alpn = protocols.iter().map(|s| s.to_string()).collect();
        self
    }

    /// **DANGER**: Accept any server certificate, including self-signed ones.
    ///
    /// For testing only — disables all certificate verification.
    #[cfg(feature = "tls")]
    pub fn with_danger_accept_invalid_certs(mut self) -> Self {
        self.accept_invalid_certs = true;
        self
    }

    /// Write TLS session secrets to `path` in NSS key-log format (SSLKEYLOGFILE).
    ///
    /// The file is created/appended to on every TLS handshake. Use this for
    /// decrypting captured HTTPS traffic in Wireshark or mitmproxy during
    /// development. **Do not enable in production.**
    #[cfg(feature = "tls")]
    pub fn with_key_log_file(mut self, path: std::path::PathBuf) -> Self {
        self.key_log_path = Some(path);
        self
    }

    /// Enable TLS 1.3 0-RTT early data (HTTP fast-open) for subsequent requests.
    ///
    /// Only effective if a prior connection stored a session ticket and the server
    /// indicated `max_early_data_size > 0`. Safe to call on any builder; ignored
    /// if 0-RTT is not available.
    ///
    /// # Security
    /// Early data is NOT protected against replay attacks — see RFC 8446 §8.
    /// Only enable for idempotent requests (GET, HEAD, etc.).
    #[cfg(feature = "tls")]
    pub fn with_early_data(mut self) -> Self {
        self.early_data = true;
        self
    }

    /// **DANGER**: Enable or disable certificate verification via a boolean flag.
    ///
    /// This is an alias for [`with_danger_accept_invalid_certs`](Self::with_danger_accept_invalid_certs)
    /// that accepts an explicit `bool` parameter, matching the API style of
    /// reqwest's `danger_accept_invalid_certs`.
    ///
    /// # Security
    ///
    /// Passing `true` disables **all** TLS certificate verification, making HTTPS
    /// connections trivially vulnerable to man-in-the-middle attacks.  Only use
    /// in tests or isolated local environments.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # fn example() -> Result<(), oxihttp_core::OxiHttpError> {
    /// use oxihttp_client::Client;
    ///
    /// // Mirror reqwest's API: danger_accept_invalid_certs(true)
    /// let client = Client::builder()
    ///     .danger_accept_invalid_certs(true)
    ///     .build_https()?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "tls")]
    pub fn danger_accept_invalid_certs(mut self, accept: bool) -> Self {
        self.accept_invalid_certs = accept;
        self
    }

    /// Inject a custom server-certificate verifier.
    ///
    /// The supplied `verifier` replaces the default trust-store verification
    /// for all TLS connections made by the built client.  When a custom verifier
    /// is present it takes precedence over:
    /// - `with_trusted_cert_der` / `with_webpki_roots`
    /// - `danger_accept_invalid_certs` / `with_danger_accept_invalid_certs`
    ///
    /// This enables certificate pinning, custom CA hierarchies, or completely
    /// bespoke verification logic without forking the library.
    ///
    /// # Security
    ///
    /// The security of the resulting client depends entirely on the supplied
    /// verifier.  Injecting a verifier that accepts any certificate (e.g.
    /// [`crate::tls::DangerousNoVerification`]) disables authentication;
    /// see that type's documentation for details.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # fn example() -> Result<(), oxihttp_core::OxiHttpError> {
    /// use std::sync::Arc;
    /// use oxihttp_client::{Client, tls::DangerousNoVerification};
    ///
    /// // Inject the "accept-everything" verifier (for tests only).
    /// let provider = oxitls::pure_provider();
    /// let verifier = Arc::new(DangerousNoVerification::new(provider));
    ///
    /// let client = Client::builder()
    ///     .with_custom_cert_verifier(verifier)
    ///     .build_https()?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "tls")]
    pub fn with_custom_cert_verifier(
        mut self,
        verifier: std::sync::Arc<dyn rustls::client::danger::ServerCertVerifier>,
    ) -> Self {
        self.custom_cert_verifier = Some(verifier);
        self
    }

    // --- internal TLS connector helper (feature-gated) ----------------------

    /// Choose the appropriate TLS connector builder depending on whether a
    /// custom verifier has been injected.  This keeps the dispatch in one place
    /// so all `build_*` variants stay consistent.
    #[cfg(feature = "tls")]
    fn build_tls_connector_inner(&self) -> Result<tokio_rustls::TlsConnector, OxiHttpError> {
        if let Some(ref verifier) = self.custom_cert_verifier {
            tls::build_tls_connector_with_verifier(
                std::sync::Arc::clone(verifier),
                &self.alpn,
                self.early_data,
            )
        } else {
            tls::build_tls_connector(
                &self.trusted_certs_der,
                &self.alpn,
                self.accept_invalid_certs,
                self.use_webpki_roots,
                self.key_log_path.clone(),
                self.early_data,
            )
        }
    }

    // --- build() — plain HTTP -----------------------------------------------

    /// Build a plain HTTP `Client` (no TLS).
    pub fn build(self) -> Result<Client, OxiHttpError> {
        let mut builder = HyperClient::builder(TokioExecutor::new());

        if let Some(n) = self.pool_max_idle_per_host {
            builder.pool_max_idle_per_host(n);
        }
        if let Some(dur) = self.pool_idle_timeout {
            builder.pool_idle_timeout(dur);
        }
        if let Some(ref h2) = self.http2_settings {
            apply_http2_settings(&mut builder, h2);
        }

        // Use an explicit HttpConnector so TCP options can be applied.
        let mut http = HttpConnector::new();
        http.enforce_http(false);
        if let Some(dur) = self.connect_timeout {
            http.set_connect_timeout(Some(dur));
        }
        if let Some(nodelay) = self.tcp_nodelay {
            http.set_nodelay(nodelay);
        }
        if let Some(ka) = self.tcp_keepalive {
            http.set_keepalive(Some(ka));
        }

        let inner = builder.build(http);

        let mut default_headers = self.default_headers;
        if let Some(agent) = &self.user_agent {
            if let Ok(val) = HeaderValue::from_str(agent) {
                default_headers.insert(http::header::USER_AGENT, val);
            }
        }

        Ok(Client {
            inner,
            redirect_policy: self.redirect_policy,
            retry_policy: self.retry_policy,
            default_headers,
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
            decompression: self.decompression,
            middleware: self.middleware,
            cookie_jar: self.cookie_jar.clone(),
            #[cfg(feature = "tls")]
            tls_rebuild: None,
        })
    }

    // --- build_https() — TLS-capable client ---------------------------------

    /// Build a TLS-capable `HttpsClient` (alias for `Client<OxiHttpsConnector<HttpConnector>>`).
    ///
    /// The resulting client handles both `http://` and `https://` URIs.
    #[cfg(feature = "tls")]
    pub fn build_https(self) -> Result<super::HttpsClient, OxiHttpError> {
        let connector = self.build_tls_connector_inner()?;

        let mut http = HttpConnector::new();
        // Allow the inner connector to accept https:// URIs so that the
        // OxiHttpsConnector can extract the host/port and then upgrade the TCP
        // stream to TLS.  Without this flag, HttpConnector rejects any URI
        // whose scheme is not "http".
        http.enforce_http(false);
        if let Some(dur) = self.connect_timeout {
            http.set_connect_timeout(Some(dur));
        }
        if let Some(nodelay) = self.tcp_nodelay {
            http.set_nodelay(nodelay);
        }
        if let Some(ka) = self.tcp_keepalive {
            http.set_keepalive(Some(ka));
        }
        let https_connector = OxiHttpsConnector::new(http, connector);

        let mut builder = HyperClient::builder(TokioExecutor::new());

        if let Some(n) = self.pool_max_idle_per_host {
            builder.pool_max_idle_per_host(n);
        }
        if let Some(dur) = self.pool_idle_timeout {
            builder.pool_idle_timeout(dur);
        }
        if let Some(ref h2) = self.http2_settings {
            apply_http2_settings(&mut builder, h2);
        }

        let inner = builder.build(https_connector);

        let mut default_headers = self.default_headers;
        if let Some(agent) = &self.user_agent {
            if let Ok(val) = HeaderValue::from_str(agent) {
                default_headers.insert(http::header::USER_AGENT, val);
            }
        }

        let tls_rebuild = Arc::new(TlsRebuildConfig {
            trusted_certs_der: self.trusted_certs_der.clone(),
            alpn: self.alpn.clone(),
            accept_invalid_certs: self.accept_invalid_certs,
            use_webpki_roots: self.use_webpki_roots,
            key_log_path: self.key_log_path.clone(),
            early_data: self.early_data,
            connect_timeout: self.connect_timeout,
            tcp_nodelay: self.tcp_nodelay,
            tcp_keepalive: self.tcp_keepalive,
            http2_settings: self.http2_settings.clone(),
            pool_max_idle_per_host: self.pool_max_idle_per_host,
            pool_idle_timeout: self.pool_idle_timeout,
            custom_cert_verifier: self.custom_cert_verifier,
        });

        Ok(Client {
            inner,
            redirect_policy: self.redirect_policy,
            retry_policy: self.retry_policy,
            default_headers,
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
            decompression: self.decompression,
            middleware: self.middleware,
            cookie_jar: self.cookie_jar.clone(),
            tls_rebuild: Some(tls_rebuild),
        })
    }

    // --- build_with_resolver / build_https_with_resolver ---------------------

    /// Build a plain HTTP `Client` using a custom DNS resolver.
    ///
    /// [`ClientBuilder::with_resolver`] must be called before this.
    pub fn build_with_resolver(self) -> Result<super::ResolverClient, OxiHttpError> {
        let resolver = self.resolver.ok_or_else(|| {
            OxiHttpError::Dns("with_resolver must be called before build_with_resolver".into())
        })?;
        let mut http = HttpConnector::new_with_resolver(BoxResolver(resolver));
        http.enforce_http(false);
        if let Some(dur) = self.connect_timeout {
            http.set_connect_timeout(Some(dur));
        }
        if let Some(nodelay) = self.tcp_nodelay {
            http.set_nodelay(nodelay);
        }
        if let Some(ka) = self.tcp_keepalive {
            http.set_keepalive(Some(ka));
        }

        let mut builder = HyperClient::builder(TokioExecutor::new());
        if let Some(n) = self.pool_max_idle_per_host {
            builder.pool_max_idle_per_host(n);
        }
        if let Some(d) = self.pool_idle_timeout {
            builder.pool_idle_timeout(d);
        }
        if let Some(ref h2) = self.http2_settings {
            apply_http2_settings(&mut builder, h2);
        }

        let mut default_headers = self.default_headers;
        if let Some(agent) = &self.user_agent {
            if let Ok(val) = HeaderValue::from_str(agent) {
                default_headers.insert(http::header::USER_AGENT, val);
            }
        }

        Ok(Client {
            inner: builder.build(http),
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
            redirect_policy: self.redirect_policy,
            retry_policy: self.retry_policy,
            default_headers,
            decompression: self.decompression,
            middleware: self.middleware,
            cookie_jar: self.cookie_jar,
            #[cfg(feature = "tls")]
            tls_rebuild: None,
        })
    }

    /// Build a TLS-capable `Client` using a custom DNS resolver.
    ///
    /// [`ClientBuilder::with_resolver`] must be called before this.
    /// The resulting client handles both `http://` and `https://` URIs.
    #[cfg(feature = "tls")]
    pub fn build_https_with_resolver(self) -> Result<super::ResolverHttpsClient, OxiHttpError> {
        // Build the TLS connector first (while `self` is still whole), then
        // extract the resolver field.  Reversing the order would cause a
        // partial-move conflict because `ok_or_else` consumes `self.resolver`.
        let tls_connector = self.build_tls_connector_inner()?;

        let resolver = self.resolver.ok_or_else(|| {
            OxiHttpError::Dns(
                "with_resolver must be called before build_https_with_resolver".into(),
            )
        })?;

        let mut http = HttpConnector::new_with_resolver(BoxResolver(resolver));
        http.enforce_http(false);
        if let Some(dur) = self.connect_timeout {
            http.set_connect_timeout(Some(dur));
        }
        if let Some(nodelay) = self.tcp_nodelay {
            http.set_nodelay(nodelay);
        }
        if let Some(ka) = self.tcp_keepalive {
            http.set_keepalive(Some(ka));
        }

        let connector = crate::connector::OxiHttpsConnector::new(http, tls_connector);

        let mut builder = HyperClient::builder(TokioExecutor::new());
        if let Some(n) = self.pool_max_idle_per_host {
            builder.pool_max_idle_per_host(n);
        }
        if let Some(d) = self.pool_idle_timeout {
            builder.pool_idle_timeout(d);
        }
        if let Some(ref h2) = self.http2_settings {
            apply_http2_settings(&mut builder, h2);
        }

        let mut default_headers = self.default_headers;
        if let Some(agent) = &self.user_agent {
            if let Ok(val) = HeaderValue::from_str(agent) {
                default_headers.insert(http::header::USER_AGENT, val);
            }
        }

        Ok(Client {
            inner: builder.build(connector),
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
            redirect_policy: self.redirect_policy,
            retry_policy: self.retry_policy,
            default_headers,
            decompression: self.decompression,
            middleware: self.middleware,
            cookie_jar: self.cookie_jar,
            #[cfg(feature = "tls")]
            tls_rebuild: None,
        })
    }
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ClientBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientBuilder")
            .field("pool_max_idle_per_host", &self.pool_max_idle_per_host)
            .field("pool_idle_timeout", &self.pool_idle_timeout)
            .field("connect_timeout", &self.connect_timeout)
            .field("read_timeout", &self.read_timeout)
            .field("redirect_policy", &self.redirect_policy)
            .field("retry_policy", &self.retry_policy)
            .field("decompression", &self.decompression)
            .field("user_agent", &self.user_agent)
            .field("tcp_nodelay", &self.tcp_nodelay)
            .field("tcp_keepalive", &self.tcp_keepalive)
            .finish_non_exhaustive()
    }
}
