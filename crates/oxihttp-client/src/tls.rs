//! TLS configuration helper for the oxihttp client.
//!
//! Gated under `#[cfg(feature = "tls")]` via the file-level attribute.

#![cfg(feature = "tls")]

use std::path::PathBuf;
use std::sync::Arc;

use oxihttp_core::OxiHttpError;
use oxitls::tls13::ClientBuilder;
use tokio_rustls::TlsConnector;

/// Build a `TlsConnector` from the given parameters.
///
/// - `trusted_certs_der` — additional DER-encoded CA certificates to trust.
/// - `alpn` — ALPN protocols to announce (e.g. `["h2", "http/1.1"]`).
/// - `accept_invalid` — if `true`, skip certificate verification (testing only).
/// - `webpki_roots` — trust the Mozilla CA bundle.
/// - `key_log_path` — if `Some(path)`, write session secrets to that file in
///   NSS key-log format (SSLKEYLOGFILE); for debugging only.
/// - `early_data` — if `true`, enable TLS 1.3 0-RTT early data; see RFC 8446 §8.
pub(crate) fn build_tls_connector(
    trusted_certs_der: &[Vec<u8>],
    alpn: &[String],
    accept_invalid: bool,
    webpki_roots: bool,
    key_log_path: Option<PathBuf>,
    early_data: bool,
) -> Result<TlsConnector, OxiHttpError> {
    let mut builder = ClientBuilder::new().with_tls12_fallback();

    if webpki_roots {
        builder = builder.with_webpki_roots();
    }

    for cert_der in trusted_certs_der {
        builder = builder
            .with_trusted_cert_der(cert_der.clone())
            .map_err(|e| OxiHttpError::Tls(e.to_string()))?;
    }

    if !alpn.is_empty() {
        let alpn_refs: Vec<&str> = alpn.iter().map(String::as_str).collect();
        builder = builder.with_alpn_protocols(alpn_refs.iter().copied());
    }

    if accept_invalid {
        builder = builder.with_danger_accept_invalid_certs();
    }

    if let Some(path) = key_log_path {
        builder = builder.with_key_log_file(path);
    }

    if early_data {
        builder = builder.with_early_data();
    }

    let cfg = builder
        .build()
        .map_err(|e| OxiHttpError::Tls(e.to_string()))?;
    Ok(TlsConnector::from(Arc::new(cfg)))
}
