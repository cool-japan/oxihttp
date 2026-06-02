//! OxiHTTP - Pure-Rust HTTP facade for the COOLJAPAN ecosystem.
//!
//! This crate provides a unified API for HTTP client and server functionality,
//! re-exporting types from `oxihttp-core`, `oxihttp-client`, and `oxihttp-server`.
//!
//! # Features
//!
//! - `client` (default): HTTP client with connection pooling, redirects, retries.
//! - `server`: HTTP server with routing, middleware, graceful shutdown.
//! - `tls`: TLS support via oxitls.
//!
//! ## Feature flags
//!
//! | Feature | Enables | Default |
//! |---|---|---|
//! | `client` | HTTP client (connection pooling, redirects, retries) | ✓ |
//! | `server` | HTTP server (routing, middleware, graceful shutdown) | ✓ |
//! | `tls` | HTTPS client + server TLS via rustls/oxitls | ✗ |
//! | `websocket` | RFC 6455 WebSocket (server-side) | ✗ |
//! | `compression` | Response compression via oxiarc-deflate | ✗ |
//! | `decompression` | Client auto-decompression via oxiarc-deflate | ✗ |
//! | `static-files` | ServeDir/ServeFile static file serving | ✗ |
//! | `sse` | Server-Sent Events | ✗ |
//! | `tower` | Tower middleware integration | ✗ |
//! | `socks` | SOCKS5 proxy support | ✗ |
//! | `all` | All of the above | ✗ |
//!
//! # Quick Start
//!
//! ```rust,no_run
//! # async fn example() -> oxihttp::Result<()> {
//! // Simple GET request
//! let client = oxihttp::Client::builder().build()?;
//! let resp = client.get("http://example.com")?.send().await?;
//! println!("Status: {}", resp.status());
//! let body = resp.body_text().await?;
//! println!("Body: {body}");
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

// Re-export core types
pub use oxihttp_core::{
    // Body types
    Body,
    // Bytes re-exports
    Bytes,
    BytesMut,
    // Extension traits
    ContentType,
    // Cookie support
    Cookie,
    CookieJar,
    // Form builder
    FormBody,
    // http crate re-exports
    HeaderMap,
    HeaderMapExt,
    HeaderName,
    HeaderValue,
    Method,
    // Multipart builder
    MultipartBuilder,
    MultipartPart,
    // Error and result
    OxiHttpError,
    PinnedBody,
    SameSite,
    StatusCode,
    Uri,
    UriExt,
    Version,
};

/// Type alias for `Result<T, OxiHttpError>`.
pub type Result<T> = std::result::Result<T, OxiHttpError>;

/// Convenience re-export of `http::Request`.
pub use http::Request;

/// Convenience re-export of `http::Response` (the raw http crate type).
pub use http::Response as RawResponse;

// ---------------------------------------------------------------------------
// Client feature
// ---------------------------------------------------------------------------

#[cfg(feature = "client")]
pub use oxihttp_client::{
    BodyStream, Client, ClientBuilder, RedirectPolicy, RequestBuilder, Response, RetryPolicy,
};

#[cfg(feature = "tls")]
pub use oxihttp_client::{
    request_config::RequestTlsConfig, HttpsClient, MaybeHttpsStream, OxiHttpsConnector,
};

/// One-shot GET request using a default client.
///
/// This creates a temporary client for each call. For multiple requests,
/// prefer creating a `Client` and reusing it.
#[cfg(feature = "client")]
pub async fn get(url: &str) -> Result<Response> {
    let client = Client::builder().build()?;
    client.get(url)?.send().await
}

/// One-shot POST request using a default client.
///
/// This creates a temporary client for each call.
#[cfg(feature = "client")]
pub async fn post(url: &str, body: impl Into<Bytes>) -> Result<Response> {
    let client = Client::builder().build()?;
    client.post(url)?.body(body).send().await
}

/// One-shot PUT request using a default client.
#[cfg(feature = "client")]
pub async fn put(url: &str, body: impl Into<Bytes>) -> Result<Response> {
    let client = Client::builder().build()?;
    client.put(url)?.body(body).send().await
}

/// One-shot DELETE request using a default client.
#[cfg(feature = "client")]
pub async fn delete(url: &str) -> Result<Response> {
    let client = Client::builder().build()?;
    client.delete(url)?.send().await
}

// ---------------------------------------------------------------------------
// Server feature
// ---------------------------------------------------------------------------

#[cfg(feature = "server")]
pub use oxihttp_server::{
    // Response helpers
    response,
    // Middleware
    CorsConfig,
    RateLimiter,
    // Router
    Router,
    // Server types
    Server,
    ServerBuilder,
};

#[cfg(all(feature = "tls", feature = "server"))]
pub use oxihttp_server::TlsConfig;

#[cfg(feature = "server")]
pub use oxihttp_server::Request as ServerRequest;

// ---------------------------------------------------------------------------
// TLS module (feature: tls + server)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "tls", feature = "server"))]
pub mod tls {
    //! TLS configuration types for HTTPS client and server.
    //!
    //! # Example
    //!
    //! ```rust,no_run
    //! # #[cfg(all(feature = "tls", feature = "server"))] {
    //! use oxihttp::tls::TlsConfig;
    //! # fn main() -> oxihttp::Result<()> {
    //! // Build from PEM files
    //! let cert_pem = b"-----BEGIN CERTIFICATE-----\n...\n-----END CERTIFICATE-----\n";
    //! let key_pem  = b"-----BEGIN PRIVATE KEY-----\n...\n-----END PRIVATE KEY-----\n";
    //! let _config = TlsConfig::from_pem(cert_pem, key_pem)?;
    //! # Ok(())
    //! # }
    //! # }
    //! ```
    pub use oxihttp_server::PeerCertInfo;
    pub use oxihttp_server::TlsConfig;
}

// ---------------------------------------------------------------------------
// WebSocket module (feature: websocket)
// ---------------------------------------------------------------------------

#[cfg(feature = "websocket")]
pub mod ws {
    //! WebSocket support (RFC 6455).
    //!
    //! # Example
    //!
    //! ```rust,no_run
    //! # #[cfg(feature = "websocket")] {
    //! use oxihttp::ws;
    //! use oxihttp::ServerRequest;
    //!
    //! async fn ws_handler(req: ServerRequest) -> Result<http::Response<http_body_util::Full<bytes::Bytes>>, oxihttp::OxiHttpError> {
    //!     let (upgrade, resp) = ws::upgrade(req)?;
    //!     tokio::spawn(async move {
    //!         if let Ok(mut socket) = upgrade.accept().await {
    //!             // echo loop
    //!             while let Ok(Some(msg)) = socket.recv().await {
    //!                 if socket.send(msg).await.is_err() { break; }
    //!             }
    //!         }
    //!     });
    //!     Ok(resp)
    //! }
    //! # }
    //! ```
    pub use oxihttp_server::ws::{upgrade, CloseFrame, Message, WebSocket, WebSocketUpgrade};
}

// ---------------------------------------------------------------------------
// Middleware module (feature: tower)
// ---------------------------------------------------------------------------

#[cfg(feature = "tower")]
pub mod middleware {
    //! Tower middleware for OxiHTTP client and server.
    //!
    //! Server middleware (applied via `ServerBuilder::with_layer()`):
    //!
    //! - `LoggingLayer`: logs each request method + path with status and elapsed time
    //! - `RequestIdLayer`: injects `X-Request-Id` header into requests and responses
    //!
    //! Client middleware (applied via `ClientBuilder::with_middleware()`):
    //!
    //! - `ClientMiddleware`: trait for request/response interceptors
    //! - `LoggingMiddleware`: logs each outgoing request and response
    //! - `TimingMiddleware`: records elapsed time via a user-supplied callback
    pub use oxihttp_client::middleware::{ClientMiddleware, LoggingMiddleware, TimingMiddleware};
    pub use oxihttp_server::{LoggingLayer, RequestIdLayer};
}

// ---------------------------------------------------------------------------
// HTTP/3 module (feature: h3)
// ---------------------------------------------------------------------------

/// HTTP/3 client and server via oxiquic-h3.
///
/// Enable with `features = ["h3"]`.
///
/// # Example — client
///
/// ```rust,no_run
/// # #[cfg(feature = "h3")]
/// # async fn example() -> oxihttp::Result<()> {
/// use bytes::Bytes;
/// use oxihttp::h3::{H3ConnectionBuilder, H3Response};
///
/// // Build a rustls::ClientConfig with TLS 1.3 first
/// # let client_tls: rustls::ClientConfig = todo!();
/// let mut conn = H3ConnectionBuilder::new("example.com")
///     .with_tls_config(client_tls)
///     .connect("93.184.216.34:443".parse().unwrap())
///     .await?;
/// let resp = conn.get("https://example.com/").await?;
/// assert!(resp.is_success());
/// let _ = conn.close().await;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "h3")]
pub mod h3 {
    pub use oxihttp_client::h3::{H3Connection, H3ConnectionBuilder};
    pub use oxihttp_server::h3::H3Server;
    // H3Request and H3Response come from the same underlying type; use one source.
    pub use oxihttp_client::h3::{H3Request, H3Response};
}

// ---------------------------------------------------------------------------
// Migration guide
// ---------------------------------------------------------------------------

pub mod migration;

// ---------------------------------------------------------------------------
// Prelude
// ---------------------------------------------------------------------------

/// A prelude module re-exporting the most commonly used types.
pub mod prelude {
    pub use crate::{Bytes, HeaderMap, HeaderValue, Method, OxiHttpError, StatusCode, Uri};

    pub use crate::Result;

    #[cfg(feature = "client")]
    pub use crate::{Client, Response};

    #[cfg(feature = "server")]
    pub use crate::{Router, Server};
}

/// Return the crate version at runtime.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Type alias for `OxiHttpError` (shorter name).
pub type Error = OxiHttpError;
