//! HTTP/3 server via oxiquic-h3.
//!
//! This module provides an ergonomic wrapper around [`oxiquic_h3::H3ServerEndpoint`]
//! and [`oxiquic_h3::H3ServerBuilder`], mapping errors into [`OxiHttpError`].
//!
//! # Example
//!
//! ```rust,no_run
//! # #[cfg(feature = "h3")]
//! # async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
//! use bytes::Bytes;
//! use oxihttp_server::h3::{H3Request, H3Response, H3Server};
//!
//! // server_tls: a rustls::ServerConfig with TLS 1.3 (h3 ALPN auto-injected)
//! # let server_tls: rustls::ServerConfig = todo!();
//! let server = H3Server::bind("127.0.0.1:8443".parse().unwrap(), server_tls)
//!     .await?;
//! let addr = server.local_addr()?;
//!
//! server
//!     .serve(|_req: H3Request, _body: Bytes| async move {
//!         H3Response::new(200).with_body(b"hello h3".to_vec())
//!     })
//!     .await?;
//! # Ok(())
//! # }
//! ```

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use oxihttp_core::OxiHttpError;
use oxiquic_h3::{H3Connection, H3ServerBuilder, H3ServerEndpoint};

pub use oxiquic_h3::{H3Request, H3Response};

// ─────────────────────────────────────────────────────────────────────────────
// H3Server
// ─────────────────────────────────────────────────────────────────────────────

/// An HTTP/3 server bound to a local UDP address.
///
/// Wraps [`H3ServerEndpoint`] and maps errors into [`OxiHttpError`].
/// Constructed via [`H3Server::bind`].
pub struct H3Server {
    endpoint: H3ServerEndpoint,
}

impl H3Server {
    /// Bind to `addr` and prepare to accept HTTP/3 connections.
    ///
    /// The `h3` ALPN token is injected automatically into `tls_config`.
    ///
    /// # Errors
    ///
    /// Returns [`OxiHttpError::H3`] if no TLS config was set or the socket
    /// cannot be bound.
    pub async fn bind(
        addr: SocketAddr,
        tls_config: rustls::ServerConfig,
    ) -> Result<Self, OxiHttpError> {
        let endpoint = H3ServerBuilder::new(addr)
            .with_tls_config(tls_config)
            .build()
            .await
            .map_err(|e| OxiHttpError::H3(e.to_string()))?;
        Ok(Self { endpoint })
    }

    /// The local address this server is listening on.
    ///
    /// # Errors
    ///
    /// Returns [`OxiHttpError::H3`] if the socket address cannot be retrieved.
    pub fn local_addr(&self) -> Result<SocketAddr, OxiHttpError> {
        self.endpoint
            .local_addr()
            .map_err(|e| OxiHttpError::H3(e.to_string()))
    }

    /// Run the server, dispatching each request to `handler`.
    ///
    /// Accepts connections in a loop; for each connection, spawns a Tokio task
    /// to process requests concurrently.  Returns when the endpoint is closed.
    ///
    /// # Handler signature
    ///
    /// ```text
    /// async fn handler(req: H3Request, body: Bytes) -> H3Response
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `Ok(())` when the accept loop ends (endpoint closed). Individual
    /// per-request errors are logged and discarded rather than propagated, so the
    /// server continues running on partial failures.
    pub async fn serve<F, Fut>(self, handler: F) -> Result<(), OxiHttpError>
    where
        F: Fn(H3Request, Bytes) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = H3Response> + Send + 'static,
    {
        let handler = Arc::new(handler);
        let incoming = self.endpoint.incoming();
        while let Some(conn) = incoming.next().await {
            let handler = Arc::clone(&handler);
            tokio::spawn(handle_h3_connection(conn, handler));
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// handle_h3_connection — per-connection task
// ─────────────────────────────────────────────────────────────────────────────

/// Drive a single HTTP/3 connection to completion, calling `handler` for each
/// incoming request.
async fn handle_h3_connection<F, Fut>(mut conn: H3Connection, handler: Arc<F>)
where
    F: Fn(H3Request, Bytes) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = H3Response> + Send + 'static,
{
    while let Ok(Some(mut req_ctx)) = conn.accept().await {
        let handler = Arc::clone(&handler);
        tokio::spawn(async move {
            let req = req_ctx.request().clone();
            // Read body (mutable borrow ends before we consume req_ctx).
            let body = match req_ctx.body().await {
                Ok(b) => b,
                Err(_) => return,
            };
            let resp = handler(req, body).await;
            // Consume req_ctx to send the response.
            let _ = req_ctx.respond(resp).await;
        });
    }
}
