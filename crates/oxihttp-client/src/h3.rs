//! HTTP/3 client via oxiquic-h3.
//!
//! This module provides a thin ergonomic wrapper around [`oxiquic_h3::H3Client`]
//! and [`oxiquic_h3::H3ClientBuilder`], mapping errors into [`OxiHttpError`].
//!
//! # Example
//!
//! ```rust,no_run
//! # #[cfg(feature = "h3")]
//! # async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
//! use oxihttp_client::h3::H3ConnectionBuilder;
//!
//! // tls_cfg: a rustls::ClientConfig with TLS 1.3 and h3 ALPN (auto-injected)
//! # let tls_cfg: rustls::ClientConfig = todo!();
//! let mut conn = H3ConnectionBuilder::new("example.com")
//!     .with_tls_config(tls_cfg)
//!     .connect("93.184.216.34:443".parse().unwrap())
//!     .await?;
//!
//! let resp = conn.get("https://example.com/").await?;
//! assert!(resp.is_success());
//! let _ = conn.close().await;
//! # Ok(())
//! # }
//! ```

use std::net::SocketAddr;

use bytes::Bytes;
use oxihttp_core::OxiHttpError;
use oxiquic_h3::{H3Client as RawH3Client, H3ClientBuilder};

pub use oxiquic_h3::{H3Request, H3Response};

// ─────────────────────────────────────────────────────────────────────────────
// H3ConnectionBuilder
// ─────────────────────────────────────────────────────────────────────────────

/// Builder for an HTTP/3 client connection.
///
/// Set the server name (required), optionally supply a [`rustls::ClientConfig`],
/// then call [`connect`][H3ConnectionBuilder::connect].  The `h3` ALPN token is
/// injected automatically by the underlying [`H3ClientBuilder`].
pub struct H3ConnectionBuilder {
    server_name: String,
    tls_config: Option<rustls::ClientConfig>,
}

impl H3ConnectionBuilder {
    /// Create a builder targeting `server_name` (used for TLS SNI).
    #[must_use]
    pub fn new(server_name: impl Into<String>) -> Self {
        Self {
            server_name: server_name.into(),
            tls_config: None,
        }
    }

    /// Supply the [`rustls::ClientConfig`] used for the QUIC/TLS handshake.
    ///
    /// This field is **required** before calling [`connect`][Self::connect].
    #[must_use]
    pub fn with_tls_config(mut self, cfg: rustls::ClientConfig) -> Self {
        self.tls_config = Some(cfg);
        self
    }

    /// Connect to the HTTP/3 server at `addr`.
    ///
    /// Performs the QUIC handshake and HTTP/3 SETTINGS exchange.
    ///
    /// # Errors
    ///
    /// Returns [`OxiHttpError::H3`] if no TLS config was set, or if the
    /// QUIC/HTTP/3 handshake fails.
    pub async fn connect(self, addr: SocketAddr) -> Result<H3Connection, OxiHttpError> {
        let tls_config = self
            .tls_config
            .ok_or_else(|| OxiHttpError::H3("H3Connection requires a TLS config".into()))?;
        let inner = H3ClientBuilder::new()
            .with_server_name(&self.server_name)
            .with_tls_config(tls_config)
            .connect(addr)
            .await
            .map_err(|e| OxiHttpError::H3(e.to_string()))?;
        Ok(H3Connection { inner })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// H3Connection
// ─────────────────────────────────────────────────────────────────────────────

/// An established HTTP/3 client connection.
///
/// Wraps [`oxiquic_h3::H3Client`] and maps errors into [`OxiHttpError`].
/// Construct via [`H3ConnectionBuilder`].
pub struct H3Connection {
    inner: RawH3Client,
}

impl H3Connection {
    /// Send a `GET` request to `uri`.
    ///
    /// # Errors
    ///
    /// Returns [`OxiHttpError::H3`] on protocol or transport failure.
    pub async fn get(&mut self, uri: &str) -> Result<H3Response, OxiHttpError> {
        self.inner
            .get(uri)
            .await
            .map_err(|e| OxiHttpError::H3(e.to_string()))
    }

    /// Send a `POST` request to `uri` with `body`.
    ///
    /// # Errors
    ///
    /// Returns [`OxiHttpError::H3`] on protocol or transport failure.
    pub async fn post(&mut self, uri: &str, body: Bytes) -> Result<H3Response, OxiHttpError> {
        self.inner
            .post(uri, body)
            .await
            .map_err(|e| OxiHttpError::H3(e.to_string()))
    }

    /// Send a `HEAD` request to `uri`.
    ///
    /// # Errors
    ///
    /// Returns [`OxiHttpError::H3`] on failure.
    pub async fn head(&mut self, uri: &str) -> Result<H3Response, OxiHttpError> {
        self.inner
            .head(uri)
            .await
            .map_err(|e| OxiHttpError::H3(e.to_string()))
    }

    /// Send a `PUT` request to `uri` with `body`.
    ///
    /// # Errors
    ///
    /// Returns [`OxiHttpError::H3`] on failure.
    pub async fn put(&mut self, uri: &str, body: Bytes) -> Result<H3Response, OxiHttpError> {
        self.inner
            .put(uri, body)
            .await
            .map_err(|e| OxiHttpError::H3(e.to_string()))
    }

    /// Send a `DELETE` request to `uri`.
    ///
    /// # Errors
    ///
    /// Returns [`OxiHttpError::H3`] on failure.
    pub async fn delete(&mut self, uri: &str) -> Result<H3Response, OxiHttpError> {
        self.inner
            .delete(uri)
            .await
            .map_err(|e| OxiHttpError::H3(e.to_string()))
    }

    /// Send an arbitrary [`H3Request`] with an optional body.
    ///
    /// # Errors
    ///
    /// Returns [`OxiHttpError::H3`] on failure.
    pub async fn request(
        &mut self,
        req: H3Request,
        body: Option<Bytes>,
    ) -> Result<H3Response, OxiHttpError> {
        self.inner
            .request(req, body)
            .await
            .map_err(|e| OxiHttpError::H3(e.to_string()))
    }

    /// Gracefully close the HTTP/3 connection (sends GOAWAY).
    ///
    /// # Errors
    ///
    /// Returns [`OxiHttpError::H3`] if the shutdown fails.
    pub async fn close(self) -> Result<(), OxiHttpError> {
        self.inner
            .close()
            .await
            .map_err(|e| OxiHttpError::H3(e.to_string()))
    }
}
