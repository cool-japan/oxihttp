//! OxiHTTP Server - Pure-Rust HTTP server for the OxiHTTP stack.
//!
//! Provides an HTTP/1.1 and HTTP/2 server with routing, middleware pipeline,
//! and graceful shutdown support.
//!
//! # Example
//!
//! ```rust,no_run
//! use oxihttp_server::{Server, Router, response};
//! use bytes::Bytes;
//! use http_body_util::Full;
//!
//! # async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
//! let router = Router::new()
//!     .get("/hello", |_req| async {
//!         response::text_response("Hello, World!")
//!     })
//!     .health("/health");
//!
//! Server::bind("127.0.0.1:3000")
//!     .serve(router)
//!     .await?;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

#[cfg(feature = "compression")]
pub mod compression;
pub mod extractor;
pub mod middleware;
pub mod response;
pub mod router;
#[cfg(feature = "sse")]
pub mod sse;
#[cfg(feature = "static-files")]
pub mod static_files;
#[cfg(feature = "tower")]
pub mod tower_compat;
#[cfg(feature = "tower")]
pub mod tower_middleware;
#[cfg(feature = "websocket")]
pub mod ws;
#[cfg(feature = "websocket")]
pub mod ws_frame;

#[cfg(feature = "compression")]
pub use compression::{Compression, CompressionAlgorithm, CompressionConfig};

#[cfg(feature = "sse")]
pub use sse::{SseEvent, SseResponse, SseSender};
#[cfg(feature = "static-files")]
pub use static_files::{ServeDir, ServeFile};

#[cfg(feature = "tls")]
pub mod tls;
#[cfg(feature = "tls")]
pub use tls::{PeerCertInfo, TlsConfig};

#[cfg(feature = "h3")]
pub mod h3;

#[cfg(feature = "tower")]
pub use tower_compat::RouterMakeService;
#[cfg(feature = "tower")]
pub use tower_middleware::{LoggingLayer, RequestIdLayer};

#[cfg(feature = "websocket")]
pub use ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};

use bytes::Bytes;
use http_body_util::Full;
use hyper::service::service_fn;
use hyper_util::rt::TokioExecutor;
use hyper_util::server::conn::auto;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use tokio::net::TcpListener;

use middleware::MiddlewarePipeline;
use oxihttp_core::OxiHttpError;

pub use extractor::{FromRequestParts, RequestParts, TypedHeader};
pub use middleware::{BodyLimitConfig, CorsConfig, RateLimiter, TimeoutConfig};
pub use router::{Request, Router};

// ---------------------------------------------------------------------------
// ServerHttp2Settings
// ---------------------------------------------------------------------------

/// HTTP/2 settings for the server side.
#[derive(Debug, Clone, Default)]
pub struct ServerHttp2Settings {
    /// Initial window size for stream-level flow control (bytes).
    pub initial_stream_window_size: Option<u32>,
    /// Initial window size for connection-level flow control (bytes).
    pub initial_connection_window_size: Option<u32>,
    /// Enable adaptive flow control.
    pub adaptive_window: Option<bool>,
    /// Maximum number of concurrent HTTP/2 streams per connection.
    pub max_concurrent_streams: Option<u32>,
    /// Maximum HTTP/2 frame size (bytes).
    pub max_frame_size: Option<u32>,
    /// Interval for HTTP/2 PING keep-alive frames.
    pub keep_alive_interval: Option<std::time::Duration>,
    /// Timeout for acknowledgement of keep-alive PING.
    pub keep_alive_timeout: Option<std::time::Duration>,
}

// ---------------------------------------------------------------------------
// configure_auto_builder — shared helper for accept loops
// ---------------------------------------------------------------------------

fn configure_auto_builder(builder: &mut auto::Builder<TokioExecutor>, h2: &ServerHttp2Settings) {
    let mut h2b = builder.http2();
    if let Some(sz) = h2.initial_stream_window_size {
        h2b.initial_stream_window_size(sz);
    }
    if let Some(sz) = h2.initial_connection_window_size {
        h2b.initial_connection_window_size(sz);
    }
    if let Some(adaptive) = h2.adaptive_window {
        h2b.adaptive_window(adaptive);
    }
    if let Some(n) = h2.max_concurrent_streams {
        h2b.max_concurrent_streams(n);
    }
    if let Some(sz) = h2.max_frame_size {
        h2b.max_frame_size(sz);
    }
    if let Some(interval) = h2.keep_alive_interval {
        h2b.keep_alive_interval(interval);
    }
    if let Some(timeout) = h2.keep_alive_timeout {
        h2b.keep_alive_timeout(timeout);
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// An HTTP server that listens on a TCP socket and dispatches requests
/// through a `Router` with optional middleware.
pub struct Server;

impl Server {
    /// Bind to the given address, returning a `ServerBuilder` for further configuration.
    pub fn bind(addr: &str) -> ServerBuilder {
        ServerBuilder {
            addr: addr.to_string(),
            middleware: MiddlewarePipeline::new(),
            graceful_shutdown: None,
            max_connections: None,
            tcp_nodelay: None,
            tcp_keepalive: None,
            http2_settings: None,
            #[cfg(feature = "tls")]
            tls: None,
            alpn_protocols: Vec::new(),
            #[cfg(feature = "tower")]
            tower_layers: Vec::new(),
        }
    }
}

/// Builder for configuring and starting an HTTP server.
pub struct ServerBuilder {
    addr: String,
    middleware: MiddlewarePipeline,
    graceful_shutdown: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
    max_connections: Option<usize>,
    /// TCP_NODELAY for accepted connections.
    tcp_nodelay: Option<bool>,
    /// TCP keepalive idle time for accepted connections.
    tcp_keepalive: Option<std::time::Duration>,
    /// HTTP/2 server-side tuning settings.
    http2_settings: Option<ServerHttp2Settings>,
    #[cfg(feature = "tls")]
    tls: Option<tls::TlsConfig>,
    /// ALPN protocols advertised in TLS handshakes.
    ///
    /// Only used when building TLS config via `with_tls_from_pem` or
    /// `with_tls_from_der`. Call `with_alpn` BEFORE `with_tls_from_pem` /
    /// `with_tls_from_der` so the protocols are set at build time.
    /// Has no effect when a pre-built `TlsConfig` is supplied via `with_tls`.
    alpn_protocols: Vec<Vec<u8>>,
    #[cfg(feature = "tower")]
    tower_layers: Vec<Arc<dyn tower_compat::ErasedLayer>>,
}

impl ServerBuilder {
    /// Add CORS middleware with the given configuration.
    pub fn with_cors(mut self, config: CorsConfig) -> Self {
        self.middleware = self.middleware.with_cors(config);
        self
    }

    /// Add a request body size limit (in bytes).
    pub fn with_body_limit(mut self, max_bytes: u64) -> Self {
        self.middleware = self.middleware.with_body_limit(max_bytes);
        self
    }

    /// Add rate limiting middleware.
    pub fn with_rate_limiter(mut self, limiter: RateLimiter) -> Self {
        self.middleware = self.middleware.with_rate_limiter(limiter);
        self
    }

    /// Add a request processing timeout.
    pub fn with_timeout(mut self, duration: std::time::Duration) -> Self {
        self.middleware = self.middleware.with_timeout(duration);
        self
    }

    /// Add a tower `Layer` to the server's middleware pipeline.
    ///
    /// Layers are applied outermost-first: the first `with_layer` call
    /// produces the layer that sees requests first.
    ///
    /// The layer's produced `Service` must satisfy:
    /// - `Service<http::Request<Incoming>, Response = http::Response<Full<Bytes>>, Error = OxiHttpError>`
    /// - `Clone + Send + 'static` (future must be `Send + 'static`)
    #[cfg(feature = "tower")]
    pub fn with_layer<L>(mut self, layer: L) -> Self
    where
        L: tower_layer::Layer<tower_compat::BoxedRouterService> + Send + Sync + Clone + 'static,
        L::Service: tower_service::Service<
                http::Request<hyper::body::Incoming>,
                Response = http::Response<Full<Bytes>>,
                Error = OxiHttpError,
            > + Clone
            + Send
            + 'static,
        <L::Service as tower_service::Service<http::Request<hyper::body::Incoming>>>::Future:
            Send + 'static,
    {
        self.tower_layers
            .push(Arc::new(tower_compat::OwnedLayer(layer)));
        self
    }

    /// Set a maximum number of concurrent connections.
    pub fn with_max_connections(mut self, n: usize) -> Self {
        self.max_connections = Some(n);
        self
    }

    /// Enable or disable `TCP_NODELAY` on accepted connections.
    pub fn with_tcp_nodelay(mut self, nodelay: bool) -> Self {
        self.tcp_nodelay = Some(nodelay);
        self
    }

    /// Set the TCP keepalive idle time for accepted connections.
    pub fn with_tcp_keepalive(mut self, duration: std::time::Duration) -> Self {
        self.tcp_keepalive = Some(duration);
        self
    }

    /// Set HTTP/2 server-side connection tuning parameters.
    pub fn with_http2_settings(mut self, settings: ServerHttp2Settings) -> Self {
        self.http2_settings = Some(settings);
        self
    }

    /// Set a graceful shutdown signal.
    pub fn with_graceful_shutdown<F>(mut self, signal: F) -> Self
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.graceful_shutdown = Some(Box::pin(signal));
        self
    }

    /// Convenience: shut down on Ctrl+C.
    pub fn shutdown_on_ctrl_c(self) -> Self {
        self.with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
    }

    /// Configure TLS with a pre-built `TlsConfig`.
    ///
    /// When using this method, `with_alpn` has no effect — ALPN must be
    /// configured on the `TlsConfig` directly before passing it here.
    #[cfg(feature = "tls")]
    pub fn with_tls(mut self, config: tls::TlsConfig) -> Self {
        self.tls = Some(config);
        self
    }

    /// Configure TLS from PEM-encoded certificate and key bytes.
    ///
    /// To customise ALPN protocols, call [`with_alpn`][Self::with_alpn]
    /// **before** calling this method.
    #[cfg(feature = "tls")]
    pub fn with_tls_from_pem(
        mut self,
        cert_pem: &[u8],
        key_pem: &[u8],
    ) -> Result<Self, OxiHttpError> {
        self.tls = Some(tls::TlsConfig::from_pem_with_alpn(
            cert_pem,
            key_pem,
            &self.alpn_protocols,
        )?);
        Ok(self)
    }

    /// Configure TLS from DER-encoded certificate chain and private key.
    ///
    /// To customise ALPN protocols, call [`with_alpn`][Self::with_alpn]
    /// **before** calling this method.
    #[cfg(feature = "tls")]
    pub fn with_tls_from_der(
        mut self,
        certs: Vec<rustls_pki_types::CertificateDer<'static>>,
        key: rustls_pki_types::PrivateKeyDer<'static>,
    ) -> Result<Self, OxiHttpError> {
        self.tls = Some(tls::TlsConfig::from_der_with_alpn(
            certs,
            key,
            &self.alpn_protocols,
        )?);
        Ok(self)
    }

    /// Set the ALPN protocol list advertised during TLS handshakes.
    ///
    /// Example: advertise HTTP/2 and HTTP/1.1 in preference order:
    ///
    /// ```no_run
    /// # use oxihttp_server::Server;
    /// # async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
    /// Server::bind("127.0.0.1:443")
    ///     .with_alpn(["h2", "http/1.1"])
    ///     .with_tls_from_pem(b"...cert...", b"...key...")?
    ///     .serve(oxihttp_server::Router::new())
    ///     .await
    /// # }
    /// ```
    ///
    /// **Ordering**: call `with_alpn` **before** [`with_tls_from_pem`][Self::with_tls_from_pem]
    /// or [`with_tls_from_der`][Self::with_tls_from_der] — the ALPN list is baked
    /// into the TLS config at that point. When a pre-built `TlsConfig` is
    /// supplied via [`with_tls`][Self::with_tls], this method has no effect.
    #[cfg(feature = "tls")]
    pub fn with_alpn<P: AsRef<[u8]>>(mut self, protocols: impl IntoIterator<Item = P>) -> Self {
        self.alpn_protocols = protocols.into_iter().map(|p| p.as_ref().to_vec()).collect();
        self
    }

    /// Start the server with the given router.
    ///
    /// This function runs until the graceful shutdown signal fires or the
    /// server encounters a fatal error.
    pub async fn serve(self, router: Router) -> Result<(), OxiHttpError> {
        let listener = TcpListener::bind(&self.addr)
            .await
            .map_err(|e| OxiHttpError::Io(Arc::new(e)))?;
        run_server(
            listener,
            router,
            self.middleware,
            self.max_connections,
            self.tcp_nodelay,
            self.tcp_keepalive,
            self.http2_settings,
            self.graceful_shutdown,
            #[cfg(feature = "tls")]
            self.tls,
            #[cfg(feature = "tower")]
            self.tower_layers,
        )
        .await
    }

    /// Start the server and return the bound `SocketAddr` plus a future that
    /// runs the server. Useful for tests where you need the address before
    /// the server starts accepting.
    pub async fn serve_with_addr(
        self,
        router: Router,
    ) -> Result<
        (
            SocketAddr,
            tokio::task::JoinHandle<Result<(), OxiHttpError>>,
        ),
        OxiHttpError,
    > {
        let listener = TcpListener::bind(&self.addr)
            .await
            .map_err(|e| OxiHttpError::Io(Arc::new(e)))?;
        let addr = listener
            .local_addr()
            .map_err(|e| OxiHttpError::Io(Arc::new(e)))?;

        let router = Arc::new(router);
        let middleware = Arc::new(self.middleware);
        let connection_semaphore = self
            .max_connections
            .map(|n| Arc::new(tokio::sync::Semaphore::new(n)));
        let graceful_shutdown = self.graceful_shutdown;
        let tcp_nodelay = self.tcp_nodelay;
        let tcp_keepalive = self.tcp_keepalive;
        let http2_settings = self.http2_settings.map(Arc::new);

        #[cfg(feature = "tls")]
        let tls_acceptor = self.tls.map(|c| Arc::new(c.acceptor));

        #[cfg(feature = "tower")]
        let tower_layers = self.tower_layers;
        #[cfg(not(feature = "tower"))]
        let tower_layers: Vec<()> = Vec::new();

        let handle = tokio::spawn(async move {
            let accept_handle = tokio::spawn(accept_loop(
                listener,
                router,
                middleware,
                connection_semaphore,
                tcp_nodelay,
                tcp_keepalive,
                http2_settings,
                tower_layers,
                #[cfg(feature = "tls")]
                tls_acceptor,
            ));

            if let Some(shutdown) = graceful_shutdown {
                tokio::select! {
                    _ = shutdown => {}
                    result = accept_handle => {
                        if let Err(e) = result {
                            return Err(OxiHttpError::Server(format!(
                                "accept loop panicked: {e}"
                            )));
                        }
                    }
                }
            } else {
                accept_handle
                    .await
                    .map_err(|e| OxiHttpError::Server(format!("accept loop panicked: {e}")))?;
            }

            Ok(())
        });

        Ok((addr, handle))
    }
}

/// A server that has already bound its TCP port but has not yet started accepting connections.
///
/// Created by [`ServerBuilder::listen`].  Use [`BoundServer::local_addr`] to obtain the
/// actual bound address (useful when binding port 0), then call [`BoundServer::serve`]
/// to start accepting connections.
pub struct BoundServer {
    listener: TcpListener,
    addr: SocketAddr,
    inner: ServerBuilder,
}

impl BoundServer {
    /// Return the local socket address the server is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Start accepting connections (same semantics as [`ServerBuilder::serve`]).
    pub async fn serve(self, router: Router) -> Result<(), OxiHttpError> {
        run_server(
            self.listener,
            router,
            self.inner.middleware,
            self.inner.max_connections,
            self.inner.tcp_nodelay,
            self.inner.tcp_keepalive,
            self.inner.http2_settings,
            self.inner.graceful_shutdown,
            #[cfg(feature = "tls")]
            self.inner.tls,
            #[cfg(feature = "tower")]
            self.inner.tower_layers,
        )
        .await
    }
}

impl ServerBuilder {
    /// Bind the TCP port eagerly and return a [`BoundServer`] handle.
    ///
    /// Useful when you need the actual bound address (e.g. when binding port 0)
    /// before the server starts accepting connections.
    ///
    /// ```rust,no_run
    /// # use oxihttp_server::{Server, Router};
    /// # async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
    /// let bound = Server::bind("127.0.0.1:0").listen().await?;
    /// let port = bound.local_addr().port();
    /// println!("Listening on port {port}");
    /// bound.serve(Router::new()).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn listen(self) -> Result<BoundServer, OxiHttpError> {
        let listener = TcpListener::bind(&self.addr)
            .await
            .map_err(|e| OxiHttpError::Io(Arc::new(e)))?;
        let addr = listener
            .local_addr()
            .map_err(|e| OxiHttpError::Io(Arc::new(e)))?;
        Ok(BoundServer {
            listener,
            addr,
            inner: self,
        })
    }
}

impl std::fmt::Debug for ServerBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("ServerBuilder");
        s.field("addr", &self.addr)
            .field("middleware", &self.middleware)
            .field("max_connections", &self.max_connections)
            .field("tcp_nodelay", &self.tcp_nodelay)
            .field("tcp_keepalive", &self.tcp_keepalive)
            .field("http2_settings", &self.http2_settings)
            .field("alpn_protocols_count", &self.alpn_protocols.len());
        #[cfg(feature = "tls")]
        s.field("tls", &self.tls);
        #[cfg(feature = "tower")]
        s.field("tower_layers", &self.tower_layers.len());
        s.finish()
    }
}

// ---------------------------------------------------------------------------
// run_server — shared implementation used by ServerBuilder::serve and
// BoundServer::serve
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn run_server(
    listener: TcpListener,
    router: Router,
    middleware: MiddlewarePipeline,
    max_connections: Option<usize>,
    tcp_nodelay: Option<bool>,
    tcp_keepalive: Option<std::time::Duration>,
    http2_settings: Option<ServerHttp2Settings>,
    graceful_shutdown: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
    #[cfg(feature = "tls")] tls: Option<tls::TlsConfig>,
    #[cfg(feature = "tower")] tower_layers: Vec<Arc<dyn tower_compat::ErasedLayer>>,
) -> Result<(), OxiHttpError> {
    let router = Arc::new(router);
    let middleware = Arc::new(middleware);
    let connection_semaphore = max_connections.map(|n| Arc::new(tokio::sync::Semaphore::new(n)));
    let http2_settings = http2_settings.map(Arc::new);

    #[cfg(feature = "tls")]
    let tls_acceptor = tls.map(|c| Arc::new(c.acceptor));

    #[cfg(feature = "tower")]
    let tower_layers_val = tower_layers;
    #[cfg(not(feature = "tower"))]
    let tower_layers_val: Vec<()> = Vec::new();

    // Spawn the accept loop.
    let accept_handle = tokio::spawn(accept_loop(
        listener,
        router,
        middleware,
        connection_semaphore,
        tcp_nodelay,
        tcp_keepalive,
        http2_settings,
        tower_layers_val,
        #[cfg(feature = "tls")]
        tls_acceptor,
    ));

    // Wait for shutdown signal or accept loop completion.
    if let Some(shutdown) = graceful_shutdown {
        tokio::select! {
            _ = shutdown => {
                // Shutdown signal received — accept_handle will be dropped.
            }
            result = accept_handle => {
                if let Err(e) = result {
                    return Err(OxiHttpError::Server(format!("accept loop panicked: {e}")));
                }
            }
        }
    } else {
        accept_handle
            .await
            .map_err(|e| OxiHttpError::Server(format!("accept loop panicked: {e}")))?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// accept_loop — no tower feature
// ---------------------------------------------------------------------------

#[cfg(not(feature = "tower"))]
#[allow(clippy::too_many_arguments)]
async fn accept_loop(
    listener: TcpListener,
    router: Arc<Router>,
    middleware: Arc<MiddlewarePipeline>,
    semaphore: Option<Arc<tokio::sync::Semaphore>>,
    tcp_nodelay: Option<bool>,
    tcp_keepalive: Option<std::time::Duration>,
    http2_settings: Option<Arc<ServerHttp2Settings>>,
    _tower_layers: Vec<()>,
    #[cfg(feature = "tls")] tls_acceptor: Option<Arc<tokio_rustls::TlsAcceptor>>,
) {
    loop {
        let accept_result = listener.accept().await;
        let (stream, _remote_addr) = match accept_result {
            Ok(conn) => conn,
            Err(_) => continue,
        };

        // Apply TCP socket options immediately after accept.
        if let Some(nodelay) = tcp_nodelay {
            let _ = stream.set_nodelay(nodelay);
        }
        if let Some(ka_dur) = tcp_keepalive {
            let ka = socket2::TcpKeepalive::new().with_time(ka_dur);
            let _ = socket2::SockRef::from(&stream).set_tcp_keepalive(&ka);
        }

        let router = Arc::clone(&router);
        let middleware = Arc::clone(&middleware);
        let permit = if let Some(ref sem) = semaphore {
            match sem.clone().try_acquire_owned() {
                Ok(p) => Some(p),
                Err(_) => continue,
            }
        } else {
            None
        };

        #[cfg(feature = "tls")]
        let tls = tls_acceptor.clone();
        let h2_cfg = http2_settings.clone();

        tokio::spawn(async move {
            #[cfg(feature = "tls")]
            if let Some(acceptor) = tls {
                if let Ok(tls_stream) = acceptor.accept(stream).await {
                    // Extract full ConnectionInfo via TlsConnectionExt BEFORE consuming
                    // the stream. This populates typed version, cipher_suite, sni, ALPN,
                    // and peer certificates in a single call.
                    use oxitls::TlsConnectionExt as _;
                    let conn_info = tls_stream.tls_connection_info();
                    let (_, server_conn) = tls_stream.get_ref();
                    let peer_info: Arc<tls::PeerCertInfo> = Arc::new(tls::PeerCertInfo {
                        peer_certificates: server_conn
                            .peer_certificates()
                            .map(|certs| certs.iter().map(|c| c.clone().into_owned()).collect())
                            .unwrap_or_default(),
                        alpn_protocol: conn_info.alpn_protocol.clone(),
                        // Keep original rustls Debug format for backward compatibility.
                        protocol_version: server_conn.protocol_version().map(|v| format!("{v:?}")),
                        version: conn_info.version,
                        cipher_suite: conn_info.cipher_suite,
                        sni: conn_info.sni.clone(),
                    });

                    let svc = service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
                        let router = Arc::clone(&router);
                        let middleware = Arc::clone(&middleware);
                        let pi = peer_info.clone();
                        async move {
                            req.extensions_mut().insert(pi);
                            dispatch_with_middleware(router, middleware, req).await
                        }
                    });
                    let mut builder = auto::Builder::new(TokioExecutor::new());
                    if let Some(ref h2) = h2_cfg {
                        configure_auto_builder(&mut builder, h2);
                    }
                    let io = hyper_util::rt::TokioIo::new(tls_stream);
                    let _ = builder.serve_connection_with_upgrades(io, svc).await;
                }
                drop(permit);
                return;
            }

            // Plain-HTTP path (only reached when TLS is not configured or the
            // `tls` feature is disabled).
            let mut builder = auto::Builder::new(TokioExecutor::new());
            if let Some(ref h2) = h2_cfg {
                configure_auto_builder(&mut builder, h2);
            }
            let svc = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                let router = Arc::clone(&router);
                let middleware = Arc::clone(&middleware);
                async move { dispatch_with_middleware(router, middleware, req).await }
            });
            let io = hyper_util::rt::TokioIo::new(stream);
            let _ = builder.serve_connection_with_upgrades(io, svc).await;
            drop(permit);
        });
    }
}

// ---------------------------------------------------------------------------
// accept_loop — with tower feature
// ---------------------------------------------------------------------------

#[cfg(feature = "tower")]
#[allow(clippy::too_many_arguments)]
async fn accept_loop(
    listener: TcpListener,
    router: Arc<Router>,
    middleware: Arc<MiddlewarePipeline>,
    semaphore: Option<Arc<tokio::sync::Semaphore>>,
    tcp_nodelay: Option<bool>,
    tcp_keepalive: Option<std::time::Duration>,
    http2_settings: Option<Arc<ServerHttp2Settings>>,
    tower_layers: Vec<Arc<dyn tower_compat::ErasedLayer>>,
    #[cfg(feature = "tls")] tls_acceptor: Option<Arc<tokio_rustls::TlsAcceptor>>,
) {
    use tower_service::Service as _;

    // Build the layered service once; each connection gets a clone.
    let layered_svc = tower_compat::build_layered_service(Arc::clone(&router), &tower_layers);

    loop {
        let accept_result = listener.accept().await;
        let (stream, _remote_addr) = match accept_result {
            Ok(conn) => conn,
            Err(_) => continue,
        };

        // Apply TCP socket options immediately after accept.
        if let Some(nodelay) = tcp_nodelay {
            let _ = stream.set_nodelay(nodelay);
        }
        if let Some(ka_dur) = tcp_keepalive {
            let ka = socket2::TcpKeepalive::new().with_time(ka_dur);
            let _ = socket2::SockRef::from(&stream).set_tcp_keepalive(&ka);
        }

        let middleware = Arc::clone(&middleware);
        let permit = if let Some(ref sem) = semaphore {
            match sem.clone().try_acquire_owned() {
                Ok(p) => Some(p),
                Err(_) => continue,
            }
        } else {
            None
        };

        #[cfg(feature = "tls")]
        let tls = tls_acceptor.clone();
        let h2_cfg = http2_settings.clone();

        // Clone the layered service for this connection.
        let conn_svc = layered_svc.clone();

        tokio::spawn(async move {
            let mut builder = auto::Builder::new(TokioExecutor::new());
            if let Some(ref h2) = h2_cfg {
                configure_auto_builder(&mut builder, h2);
            }

            #[cfg(feature = "tls")]
            if let Some(acceptor) = tls {
                if let Ok(tls_stream) = acceptor.accept(stream).await {
                    // Extract full ConnectionInfo via TlsConnectionExt BEFORE consuming
                    // the stream. This populates typed version, cipher_suite, sni, ALPN,
                    // and peer certificates in a single call.
                    use oxitls::TlsConnectionExt as _;
                    let conn_info = tls_stream.tls_connection_info();
                    let (_, server_conn) = tls_stream.get_ref();
                    let peer_info: Arc<tls::PeerCertInfo> = Arc::new(tls::PeerCertInfo {
                        peer_certificates: server_conn
                            .peer_certificates()
                            .map(|certs| certs.iter().map(|c| c.clone().into_owned()).collect())
                            .unwrap_or_default(),
                        alpn_protocol: conn_info.alpn_protocol.clone(),
                        // Keep original rustls Debug format for backward compatibility.
                        protocol_version: server_conn.protocol_version().map(|v| format!("{v:?}")),
                        version: conn_info.version,
                        cipher_suite: conn_info.cipher_suite,
                        sni: conn_info.sni.clone(),
                    });

                    let svc_tls =
                        service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
                            let middleware = Arc::clone(&middleware);
                            // Clone the service again so the closure is `FnMut` (hyper
                            // may call it multiple times for keep-alive connections).
                            let mut svc = conn_svc.clone();
                            let peer_info = Arc::clone(&peer_info);
                            async move {
                                req.extensions_mut().insert(peer_info);

                                // Pre-handler middleware (CORS, rate limit, body limit)
                                if let Some(result) = middleware.pre_handle(&req).await {
                                    return result.map_err(|e| OxiHttpError::Server(e.to_string()));
                                }

                                let origin = req
                                    .headers()
                                    .get(http::header::ORIGIN)
                                    .and_then(|v| v.to_str().ok())
                                    .map(|s| s.to_string());

                                let handler_result =
                                    if let Some(ref timeout_config) = middleware.timeout {
                                        match tokio::time::timeout(
                                            timeout_config.duration,
                                            svc.call(req),
                                        )
                                        .await
                                        {
                                            Ok(result) => result,
                                            Err(_) => middleware::TimeoutConfig::timeout_response(),
                                        }
                                    } else {
                                        svc.call(req).await
                                    };

                                match handler_result {
                                    Ok(mut resp) => {
                                        middleware.post_handle(&mut resp, origin.as_deref());
                                        Ok(resp)
                                    }
                                    Err(e) => {
                                        let mut resp = hyper::Response::builder()
                                            .status(http::StatusCode::INTERNAL_SERVER_ERROR)
                                            .body(Full::new(Bytes::from(e.to_string())))
                                            .map_err(|e2| OxiHttpError::Server(e2.to_string()))?;
                                        middleware.post_handle(&mut resp, origin.as_deref());
                                        Ok(resp)
                                    }
                                }
                            }
                        });

                    let io = hyper_util::rt::TokioIo::new(tls_stream);
                    let _ = builder.serve_connection_with_upgrades(io, svc_tls).await;
                }
                drop(permit);
                return;
            }

            let svc = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                let middleware = Arc::clone(&middleware);
                // Clone the service again so the closure is `FnMut` (hyper
                // may call it multiple times for keep-alive connections).
                let mut svc = conn_svc.clone();
                async move {
                    // Pre-handler middleware (CORS, rate limit, body limit)
                    if let Some(result) = middleware.pre_handle(&req).await {
                        return result.map_err(|e| OxiHttpError::Server(e.to_string()));
                    }

                    let origin = req
                        .headers()
                        .get(http::header::ORIGIN)
                        .and_then(|v| v.to_str().ok())
                        .map(|s| s.to_string());

                    // Route through the tower layer stack (includes timeout
                    // wrapping when configured, plus LoggingLayer / RequestIdLayer
                    // / user layers).
                    let handler_result = if let Some(ref timeout_config) = middleware.timeout {
                        match tokio::time::timeout(timeout_config.duration, svc.call(req)).await {
                            Ok(result) => result,
                            Err(_) => middleware::TimeoutConfig::timeout_response(),
                        }
                    } else {
                        svc.call(req).await
                    };

                    match handler_result {
                        Ok(mut resp) => {
                            middleware.post_handle(&mut resp, origin.as_deref());
                            Ok(resp)
                        }
                        Err(e) => {
                            let mut resp = hyper::Response::builder()
                                .status(http::StatusCode::INTERNAL_SERVER_ERROR)
                                .body(Full::new(Bytes::from(e.to_string())))
                                .map_err(|e2| OxiHttpError::Server(e2.to_string()))?;
                            middleware.post_handle(&mut resp, origin.as_deref());
                            Ok(resp)
                        }
                    }
                }
            });

            let io = hyper_util::rt::TokioIo::new(stream);
            let _ = builder.serve_connection_with_upgrades(io, svc).await;
            drop(permit);
        });
    }
}

// ---------------------------------------------------------------------------
// Shared dispatch helper (used by the non-tower accept_loop)
// ---------------------------------------------------------------------------

#[cfg(not(feature = "tower"))]
async fn dispatch_with_middleware(
    router: Arc<Router>,
    middleware: Arc<MiddlewarePipeline>,
    req: hyper::Request<hyper::body::Incoming>,
) -> Result<hyper::Response<Full<Bytes>>, OxiHttpError> {
    // Pre-handler middleware
    if let Some(result) = middleware.pre_handle(&req).await {
        return result.map_err(|e| OxiHttpError::Server(e.to_string()));
    }

    let origin = req
        .headers()
        .get(http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Handle timeout if configured
    let handler_result = if let Some(ref timeout_config) = middleware.timeout {
        match tokio::time::timeout(timeout_config.duration, router.dispatch(req)).await {
            Ok(result) => result,
            Err(_) => middleware::TimeoutConfig::timeout_response(),
        }
    } else {
        router.dispatch(req).await
    };

    match handler_result {
        Ok(mut resp) => {
            middleware.post_handle(&mut resp, origin.as_deref());
            Ok(resp)
        }
        Err(e) => {
            let mut resp = hyper::Response::builder()
                .status(http::StatusCode::INTERNAL_SERVER_ERROR)
                .body(Full::new(Bytes::from(e.to_string())))
                .map_err(|e2| OxiHttpError::Server(e2.to_string()))?;
            middleware.post_handle(&mut resp, origin.as_deref());
            Ok(resp)
        }
    }
}
