//! Per-request TLS configuration override for the oxihttp client.
//!
//! Gated under `#[cfg(feature = "tls")]` via the file-level attribute.

#![cfg(feature = "tls")]

/// Per-request TLS configuration override.
///
/// Pass to `Client::with_request_tls_config` to derive a new `HttpsClient`
/// that shares all settings with the original but uses different TLS trust for
/// a specific endpoint — enabling per-endpoint certificate pinning without
/// rebuilding the entire client.
///
/// # Under the hood
///
/// A new `Client` is constructed with an independent connection pool and a
/// freshly-built `TlsConnector`.  All non-TLS state (redirect policy, retry
/// policy, headers, middleware …) is cloned from the original.
///
/// # Connection-pool caveat
///
/// Because the returned client has its own pool, it always opens a fresh
/// connection even when the original client already has an idle connection to
/// the same host.  Use separate `Client` instances from the start when
/// guaranteed per-endpoint TLS isolation is required.
///
/// # Example
///
/// ```no_run
/// # use oxihttp_client::request_config::RequestTlsConfig;
/// let override_tls = RequestTlsConfig::new()
///     .with_trusted_cert(b"der bytes here".to_vec());
/// ```
#[derive(Debug, Clone, Default)]
pub struct RequestTlsConfig {
    /// DER-encoded certificates to trust for this request.
    ///
    /// When non-empty this list **replaces** the global client's trusted cert
    /// list.  When empty the global list is used unchanged.
    pub trusted_cert_ders: Vec<Vec<u8>>,
    /// If `true`, all certificate verification is disabled for this request.
    ///
    /// ORed with the global `accept_invalid_certs` flag — if either is `true`,
    /// verification is skipped.
    ///
    /// # Security
    /// Never enable in production.
    pub accept_invalid_certs: bool,
}

impl RequestTlsConfig {
    /// Create an empty override (all fields at their defaults).
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a DER-encoded certificate to trust for this request.
    ///
    /// Calling this more than once appends to the list.
    pub fn with_trusted_cert(mut self, der: Vec<u8>) -> Self {
        self.trusted_cert_ders.push(der);
        self
    }

    /// Disable certificate verification for this request.
    ///
    /// # Security
    /// Use only for development / testing.
    pub fn with_accept_invalid_certs(mut self) -> Self {
        self.accept_invalid_certs = true;
        self
    }
}
