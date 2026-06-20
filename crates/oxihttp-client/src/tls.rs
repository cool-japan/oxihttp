//! TLS configuration helper for the oxihttp client.
//!
//! Gated under `#[cfg(feature = "tls")]` via the file-level attribute.

#![cfg(feature = "tls")]

use std::path::PathBuf;
use std::sync::Arc;

use oxihttp_core::OxiHttpError;
use oxitls::tls13::ClientBuilder;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as RustlsError, SignatureScheme};
use tokio_rustls::TlsConnector;

// ---------------------------------------------------------------------------
// DangerousNoVerification — accepts any certificate without verification.
//
// IMPORTANT: This verifier MUST NOT be used in production. It disables all
// TLS certificate verification, making HTTPS connections susceptible to
// man-in-the-middle attacks. Use only for testing with self-signed certificates
// or in isolated local environments.
// ---------------------------------------------------------------------------

/// A server certificate verifier that accepts **any** certificate.
///
/// # Security
///
/// **WARNING — DO NOT USE IN PRODUCTION.**
/// This verifier disables all TLS certificate verification. Any server can
/// present any certificate and the client will accept it unconditionally. This
/// means the connection provides encryption but **no authentication**, making
/// it trivially vulnerable to man-in-the-middle attacks.
///
/// Intended only for:
/// - Local development with self-signed certificates
/// - Integration tests that spin up throwaway TLS servers
/// - Internal tooling where the network path is fully trusted
///
/// Inject via [`crate::client_builder::ClientBuilder::with_custom_cert_verifier`]
/// or enable via [`crate::client_builder::ClientBuilder::danger_accept_invalid_certs`].
#[derive(Debug)]
pub struct DangerousNoVerification {
    /// The crypto provider is kept so we can forward
    /// `supported_verify_schemes()` to get the full scheme list from the
    /// provider rather than hard-coding a subset.
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl DangerousNoVerification {
    /// Create a new `DangerousNoVerification` verifier backed by the given
    /// crypto provider (used only to query supported signature schemes).
    pub fn new(provider: Arc<rustls::crypto::CryptoProvider>) -> Self {
        Self { provider }
    }
}

impl ServerCertVerifier for DangerousNoVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// ---------------------------------------------------------------------------
// build_tls_connector — standard path through oxitls ClientBuilder
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// build_tls_connector_with_verifier — custom verifier path (bypasses oxitls)
// ---------------------------------------------------------------------------

/// Build a `TlsConnector` that uses a caller-supplied `ServerCertVerifier`.
///
/// This path bypasses the oxitls `ClientBuilder` and builds the
/// `rustls::ClientConfig` directly so that an arbitrary verifier can be
/// injected.  All other parameters (ALPN, early-data) are applied the same
/// way as in `build_tls_connector`.
///
/// # Security
///
/// The security guarantee of the resulting `TlsConnector` depends entirely on
/// the supplied `verifier`.  When `verifier` is a [`DangerousNoVerification`]
/// instance the connection is **not authenticated** — see that type's doc for
/// details.
pub(crate) fn build_tls_connector_with_verifier(
    verifier: Arc<dyn ServerCertVerifier>,
    alpn: &[String],
    early_data: bool,
) -> Result<TlsConnector, OxiHttpError> {
    let provider = oxitls::pure_provider();

    let cfg_builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|e| OxiHttpError::Tls(e.to_string()))?;

    let with_verifier = cfg_builder
        .dangerous()
        .with_custom_certificate_verifier(verifier);
    let mut cfg = with_verifier.with_no_client_auth();

    if !alpn.is_empty() {
        cfg.alpn_protocols = alpn.iter().map(|s| s.as_bytes().to_vec()).collect();
    }

    if early_data {
        cfg.enable_early_data = true;
    }

    Ok(TlsConnector::from(Arc::new(cfg)))
}
