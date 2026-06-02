//! TLS configuration for the OxiHTTP server via oxitls/tokio-rustls.

use oxihttp_core::OxiHttpError;
use rustls::ServerConfig;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use std::path::PathBuf;
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;

/// Information extracted from a TLS connection after a successful handshake.
///
/// Populated at accept time from `oxitls::TlsConnectionExt::tls_connection_info()`.
/// Retrieve in a handler via `req.tls_info()`.
#[derive(Debug, Clone)]
pub struct PeerCertInfo {
    /// DER-encoded peer certificate chain (leaf first), if client auth succeeded.
    /// Empty when the connection used plain TLS without client authentication.
    pub peer_certificates: Vec<CertificateDer<'static>>,
    /// Negotiated ALPN protocol (e.g., `b"h2"` or `b"http/1.1"`).
    /// `None` when no ALPN negotiation occurred.
    pub alpn_protocol: Option<Vec<u8>>,
    /// TLS protocol version as a human-readable string ("TLSv1.3", "TLSv1.2", etc.).
    /// `None` when the version could not be determined.
    pub protocol_version: Option<String>,
    /// Negotiated TLS version as a typed enum value.
    ///
    /// `None` when the version could not be mapped to a known variant.
    pub version: Option<oxitls::TlsVersion>,
    /// Negotiated cipher suite as a typed enum value.
    ///
    /// `None` when the cipher suite could not be mapped to a known variant.
    pub cipher_suite: Option<oxitls::CipherSuite>,
    /// Server Name Indication (SNI) sent by the client during the handshake.
    ///
    /// `None` when no SNI extension was present.
    pub sni: Option<String>,
}

/// TLS configuration for the HTTP server.
#[derive(Clone)]
pub struct TlsConfig {
    pub(crate) acceptor: TlsAcceptor,
}

impl TlsConfig {
    /// Build from a pre-configured rustls `ServerConfig`.
    pub fn new(config: ServerConfig) -> Self {
        Self {
            acceptor: TlsAcceptor::from(Arc::new(config)),
        }
    }

    /// Build from DER-encoded certificate chain and private key.
    pub fn from_der(
        certs: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> Result<Self, OxiHttpError> {
        Self::from_der_with_alpn(certs, key, &[])
    }

    /// Build from DER-encoded certificate chain and private key, with explicit ALPN protocols.
    ///
    /// Pass `alpn_protocols` as a slice of byte-string protocol names (e.g.
    /// `&[b"h2", b"http/1.1"]`). An empty slice disables ALPN.
    pub fn from_der_with_alpn(
        certs: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        alpn_protocols: &[Vec<u8>],
    ) -> Result<Self, OxiHttpError> {
        let mut builder = oxitls::tls13::ServerBuilder::new().with_der_cert_and_key(certs, key);
        if !alpn_protocols.is_empty() {
            builder = builder.with_alpn_protocols(alpn_protocols.iter().map(Vec::as_slice));
        }
        let server_cfg = builder
            .build()
            .map_err(|e| OxiHttpError::Tls(e.to_string()))?;
        Ok(Self::new(server_cfg))
    }

    /// Build from PEM-encoded certificate chain and private key.
    pub fn from_pem(cert_pem: &[u8], key_pem: &[u8]) -> Result<Self, OxiHttpError> {
        Self::from_pem_with_alpn(cert_pem, key_pem, &[])
    }

    /// Build from PEM-encoded certificate chain and private key, with explicit ALPN protocols.
    ///
    /// Pass `alpn_protocols` as a slice of byte-string protocol names (e.g.
    /// `&[b"h2".to_vec(), b"http/1.1".to_vec()]`). An empty slice disables ALPN.
    pub fn from_pem_with_alpn(
        cert_pem: &[u8],
        key_pem: &[u8],
        alpn_protocols: &[Vec<u8>],
    ) -> Result<Self, OxiHttpError> {
        let certs: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut std::io::Cursor::new(cert_pem))
                .collect::<Result<_, _>>()
                .map_err(|e| OxiHttpError::Tls(format!("cert PEM parse error: {e}")))?;
        if certs.is_empty() {
            return Err(OxiHttpError::Tls(
                "no certificates found in PEM".to_string(),
            ));
        }

        let key = rustls_pemfile::private_key(&mut std::io::Cursor::new(key_pem))
            .map_err(|e| OxiHttpError::Tls(format!("key PEM parse error: {e}")))?
            .ok_or_else(|| OxiHttpError::Tls("no private key found in PEM".to_string()))?;

        Self::from_der_with_alpn(certs, key, alpn_protocols)
    }

    /// Build a `TlsConfig` that requires mutual TLS (mTLS) — clients must present
    /// a certificate signed by one of the CAs in `client_ca_pem`.
    ///
    /// - `cert_pem` / `key_pem`: server certificate and private key (PEM-encoded).
    /// - `client_ca_pem`: one or more PEM-encoded CA certificates whose signatures
    ///   are trusted when verifying the client certificate chain.
    ///
    /// # Errors
    ///
    /// Returns `OxiHttpError::Tls` when any PEM cannot be parsed, no certificates are
    /// found, or the verifier cannot be constructed.
    pub fn with_client_auth(
        cert_pem: &[u8],
        key_pem: &[u8],
        client_ca_pem: &[u8],
    ) -> Result<Self, OxiHttpError> {
        // --- Parse server certificate chain + key ---------------------------------
        let certs: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut std::io::Cursor::new(cert_pem))
                .collect::<Result<_, _>>()
                .map_err(|e| OxiHttpError::Tls(format!("server cert PEM parse error: {e}")))?;
        if certs.is_empty() {
            return Err(OxiHttpError::Tls(
                "no server certificates found in PEM".to_string(),
            ));
        }

        let key = rustls_pemfile::private_key(&mut std::io::Cursor::new(key_pem))
            .map_err(|e| OxiHttpError::Tls(format!("server key PEM parse error: {e}")))?
            .ok_or_else(|| OxiHttpError::Tls("no private key found in server PEM".to_string()))?;

        // --- Parse CA certificate(s) into a RootCertStore -----------------------
        let ca_certs: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut std::io::Cursor::new(client_ca_pem))
                .collect::<Result<_, _>>()
                .map_err(|e| OxiHttpError::Tls(format!("client CA PEM parse error: {e}")))?;
        if ca_certs.is_empty() {
            return Err(OxiHttpError::Tls(
                "no CA certificates found in client_ca_pem".to_string(),
            ));
        }

        let mut roots = rustls::RootCertStore::empty();
        for ca in ca_certs {
            roots
                .add(ca)
                .map_err(|e| OxiHttpError::Tls(format!("failed to add CA to root store: {e}")))?;
        }

        // --- Build ServerConfig with WebPkiClientVerifier via oxitls builder ----
        // Note: with_client_cert_verifier is infallible; errors are deferred to build().
        let server_cfg = oxitls::tls13::ServerBuilder::new()
            .with_der_cert_and_key(certs, key)
            .with_client_cert_verifier(roots)
            .build()
            .map_err(|e| OxiHttpError::Tls(format!("mTLS ServerConfig build error: {e}")))?;

        Ok(Self::new(server_cfg))
    }

    /// Build an HTTP/2-optimised `TlsConfig` from PEM-encoded certificate and key.
    ///
    /// Sets the ALPN protocols to `["h2", "http/1.1"]` in that order
    /// (HTTP/2 preferred, HTTP/1.1 as fallback) and is otherwise equivalent
    /// to [`from_pem`](Self::from_pem).
    ///
    /// Use this constructor when deploying an HTTP/2-capable server so that
    /// both HTTP/2 and HTTP/1.1 clients can negotiate the right protocol.
    pub fn http2_defaults(cert_pem: &[u8], key_pem: &[u8]) -> Result<Self, OxiHttpError> {
        let alpn: Vec<Vec<u8>> = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Self::from_pem_with_alpn(cert_pem, key_pem, &alpn)
    }

    /// Build an HTTP/2-optimised `TlsConfig` from DER-encoded certificate chain and key.
    ///
    /// Sets the ALPN protocols to `["h2", "http/1.1"]` in that order
    /// (HTTP/2 preferred, HTTP/1.1 as fallback) and is otherwise equivalent
    /// to [`from_der`](Self::from_der).
    pub fn http2_defaults_from_der(
        certs: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> Result<Self, OxiHttpError> {
        let alpn: Vec<Vec<u8>> = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Self::from_der_with_alpn(certs, key, &alpn)
    }

    /// Build a `TlsConfig` from PEM with SSLKEYLOGFILE-style key logging enabled.
    ///
    /// Session secrets are written in NSS key-log format to `key_log_path`.
    /// This is intended for development and debugging with Wireshark; do **not**
    /// enable in production.
    pub fn from_pem_with_key_log(
        cert_pem: &[u8],
        key_pem: &[u8],
        key_log_path: PathBuf,
    ) -> Result<Self, OxiHttpError> {
        let certs: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut std::io::Cursor::new(cert_pem))
                .collect::<Result<_, _>>()
                .map_err(|e| OxiHttpError::Tls(format!("cert PEM parse error: {e}")))?;
        if certs.is_empty() {
            return Err(OxiHttpError::Tls(
                "no certificates found in PEM".to_string(),
            ));
        }
        let key = rustls_pemfile::private_key(&mut std::io::Cursor::new(key_pem))
            .map_err(|e| OxiHttpError::Tls(format!("key PEM parse error: {e}")))?
            .ok_or_else(|| OxiHttpError::Tls("no private key found in PEM".to_string()))?;

        let server_cfg = oxitls::tls13::ServerBuilder::new()
            .with_der_cert_and_key(certs, key)
            .with_key_log_file(key_log_path)
            .build()
            .map_err(|e| OxiHttpError::Tls(e.to_string()))?;
        Ok(Self::new(server_cfg))
    }

    /// Build a `TlsConfig` from DER with SSLKEYLOGFILE-style key logging enabled.
    ///
    /// Session secrets are written in NSS key-log format to `key_log_path`.
    /// This is intended for development and debugging with Wireshark; do **not**
    /// enable in production.
    pub fn from_der_with_key_log(
        certs: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        key_log_path: PathBuf,
    ) -> Result<Self, OxiHttpError> {
        let server_cfg = oxitls::tls13::ServerBuilder::new()
            .with_der_cert_and_key(certs, key)
            .with_key_log_file(key_log_path)
            .build()
            .map_err(|e| OxiHttpError::Tls(e.to_string()))?;
        Ok(Self::new(server_cfg))
    }
}

impl std::fmt::Debug for TlsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsConfig").finish_non_exhaustive()
    }
}
