//! Error types for the OxiHTTP stack.
//!
//! # `BoxError` bounds policy
//!
//! [`OxiHttpError`] is the *only* error type that crosses a public OxiHTTP
//! API boundary — every fallible `pub fn` in `oxihttp-core`, `oxihttp-client`,
//! and `oxihttp-server` returns `Result<_, OxiHttpError>`. Callers should
//! never need to downcast a `Box<dyn Error>` to find out what went wrong.
//!
//! That said, `Box<dyn std::error::Error + Send + Sync>` (informally
//! `BoxError` in this codebase) does appear in a handful of internal
//! signatures, strictly at the seams where OxiHTTP has to interoperate with
//! external traits that are not ours to redefine:
//!
//! - `oxihttp_client::resolver::BoxResolver`'s `tower_service::Service<Name>`
//!   implementation — `hyper-util`'s connector plumbing (the `Resolve` bound
//!   used by `HttpConnector::with_resolver`) requires a `Service` whose
//!   `Error` type is convertible to `Box<dyn Error + Send + Sync>`. There is
//!   no way to plug a custom [`OxiHttpError`]-typed resolver into
//!   `hyper-util` without going through this bound.
//! - `oxihttp_client::connector::OxiHttpsConnector`'s generic `H: Service<Uri>`
//!   bound (`H::Error: Into<Box<dyn Error + Send + Sync>>`) — the same
//!   `hyper-util` `Connect` convention, needed so `OxiHttpsConnector` can wrap
//!   *any* inner connector (plain TCP, a proxy tunnel, a test double, …)
//!   supplied by a caller, not just types this crate controls.
//!
//! In both cases the boxed error is a strictly **internal, transient**
//! representation: it is immediately mapped into a typed [`OxiHttpError`]
//! variant (typically [`OxiHttpError::Hyper`] or [`OxiHttpError::Dns`]) before
//! the value leaves the function that produced it, and it never appears in
//! any type signature reachable from `oxihttp::Client` or `oxihttp::Server`.
//! Server-side tower integration (`oxihttp_server::tower_compat`,
//! `tower_middleware`) does not need this escape hatch at all — every
//! `tower_service::Service` implemented there uses `OxiHttpError` directly as
//! `Service::Error`, since the router itself defines that trait's
//! implementation rather than adapting someone else's.
//!
//! Rule of thumb when adding new code: if you are implementing a foreign
//! trait (`tower_service::Service`, `hyper_util`'s `Connect`/`Resolve`, …)
//! whose `Error` associated type is bounded by `Into<Box<dyn Error + Send +
//! Sync>>`, it is fine to satisfy that bound locally — but convert to
//! [`OxiHttpError`] at the first opportunity and never let the boxed form
//! escape into a `pub fn` return type.

use std::sync::Arc;

use thiserror::Error;

/// Top-level error type for the OxiHTTP stack.
///
/// Every fallible public function across `oxihttp-core`, `oxihttp-client`,
/// and `oxihttp-server` returns `Result<_, OxiHttpError>`. See the module
/// documentation above for the policy on `Box<dyn Error>` at internal
/// interop boundaries.
///
/// # Example
///
/// ```rust
/// use oxihttp_core::OxiHttpError;
/// use http::StatusCode;
///
/// let err = OxiHttpError::RouteNotFound {
///     method: "GET".to_string(),
///     path: "/missing".to_string(),
/// };
///
/// assert_eq!(err.status_code(), Some(StatusCode::NOT_FOUND));
/// assert!(err.to_string().contains("route not found"));
/// assert!(!err.is_timeout());
/// ```
#[derive(Debug, Clone, Error)]
pub enum OxiHttpError {
    /// An invalid URI was provided.
    #[error("invalid URI: {0}")]
    InvalidUri(Arc<http::uri::InvalidUri>),

    /// An HTTP protocol error.
    #[error("HTTP error: {0}")]
    Http(Arc<http::Error>),

    /// A hyper transport error, captured as a string to avoid exposing hyper's types.
    #[error("hyper error: {0}")]
    Hyper(String),

    /// An I/O error.
    #[error("I/O error: {0}")]
    Io(Arc<std::io::Error>),

    /// An error reading or processing the response body.
    #[error("body error: {0}")]
    Body(String),

    /// A request or connect timeout expired.
    #[error("timeout: {0}")]
    Timeout(String),

    /// A redirect loop or limit was reached.
    #[error("redirect error: {0}")]
    Redirect(String),

    /// A TLS-specific error from oxitls.
    #[error("TLS error: {0}")]
    Tls(String),

    /// A DNS resolution failure.
    #[error("DNS error: {0}")]
    Dns(String),

    /// Connection pool exhaustion.
    #[error("connection pool error: {0}")]
    ConnectionPool(String),

    /// JSON serialization/deserialization error.
    #[error("JSON error: {0}")]
    Json(String),

    /// URL-encoded form error.
    #[error("form encoding error: {0}")]
    FormEncoding(String),

    /// An invalid header name or value.
    #[error("invalid header: {0}")]
    InvalidHeader(String),

    /// A server-specific error.
    #[error("server error: {0}")]
    Server(String),

    /// Route not found (404).
    #[error("route not found: {method} {path}")]
    RouteNotFound {
        /// The HTTP method of the request.
        method: String,
        /// The path that was not found.
        path: String,
    },

    /// Method not allowed (405).
    #[error("method not allowed: {method} {path}")]
    MethodNotAllowed {
        /// The HTTP method that is not allowed.
        method: String,
        /// The path where the method is not allowed.
        path: String,
    },

    /// An HTTP/3 / QUIC transport error (oxiquic-h3).
    #[error("HTTP/3 error: {0}")]
    H3(String),
}

impl From<http::uri::InvalidUri> for OxiHttpError {
    fn from(e: http::uri::InvalidUri) -> Self {
        OxiHttpError::InvalidUri(Arc::new(e))
    }
}

impl From<std::io::Error> for OxiHttpError {
    fn from(e: std::io::Error) -> Self {
        OxiHttpError::Io(Arc::new(e))
    }
}

impl From<http::Error> for OxiHttpError {
    fn from(e: http::Error) -> Self {
        OxiHttpError::Http(Arc::new(e))
    }
}

#[cfg(feature = "tls")]
impl From<oxitls_core::TlsError> for OxiHttpError {
    fn from(e: oxitls_core::TlsError) -> Self {
        OxiHttpError::Tls(e.to_string())
    }
}

impl OxiHttpError {
    /// Returns the HTTP status code associated with this error, if any.
    pub fn status_code(&self) -> Option<http::StatusCode> {
        match self {
            Self::RouteNotFound { .. } => Some(http::StatusCode::NOT_FOUND),
            Self::MethodNotAllowed { .. } => Some(http::StatusCode::METHOD_NOT_ALLOWED),
            Self::Timeout(_) => Some(http::StatusCode::REQUEST_TIMEOUT),
            _ => None,
        }
    }

    /// Returns `true` if this is a timeout error.
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout(_))
    }

    /// Returns `true` if this is a connection-related error.
    pub fn is_connect(&self) -> bool {
        matches!(self, Self::Dns(_) | Self::ConnectionPool(_) | Self::Tls(_))
    }

    /// Returns `true` if this is a body reading error.
    pub fn is_body(&self) -> bool {
        matches!(self, Self::Body(_))
    }

    /// Returns `true` if this is a redirect error.
    pub fn is_redirect(&self) -> bool {
        matches!(self, Self::Redirect(_))
    }
}

#[cfg(test)]
mod clone_tests {
    use super::*;

    #[test]
    fn test_oxi_http_error_is_clone() {
        let io_err = OxiHttpError::from(std::io::Error::new(std::io::ErrorKind::NotFound, "test"));
        let cloned = io_err.clone();
        assert_eq!(io_err.to_string(), cloned.to_string());

        let str_err = OxiHttpError::Body("test".to_string());
        let _ = str_err.clone();
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;

    // -------------------------------------------------------------------------
    // Display formatting tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_display_invalid_uri() {
        let raw_err: http::uri::InvalidUri = "not a valid uri!!!"
            .parse::<http::Uri>()
            .expect_err("should fail to parse");
        let err = OxiHttpError::from(raw_err);
        let msg = err.to_string();
        assert!(
            msg.contains("invalid URI"),
            "expected 'invalid URI' in '{msg}'"
        );
    }

    #[test]
    fn test_display_http_error() {
        let raw_err = http::Request::builder()
            .header("\n", "x")
            .body(())
            .expect_err("should fail with invalid header name");
        let err = OxiHttpError::from(raw_err);
        let msg = err.to_string();
        assert!(
            msg.contains("HTTP error"),
            "expected 'HTTP error' in '{msg}'"
        );
    }

    #[test]
    fn test_display_hyper_error() {
        let err = OxiHttpError::Hyper("connection reset".to_string());
        let msg = err.to_string();
        assert!(
            msg.contains("hyper error"),
            "expected 'hyper error' in '{msg}'"
        );
    }

    #[test]
    fn test_display_io_error() {
        let raw_err = std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "connection refused test",
        );
        let err = OxiHttpError::from(raw_err);
        let msg = err.to_string();
        assert!(msg.contains("I/O error"), "expected 'I/O error' in '{msg}'");
    }

    #[test]
    fn test_display_body_error() {
        let err = OxiHttpError::Body("chunk too large".to_string());
        let msg = err.to_string();
        assert!(
            msg.contains("body error"),
            "expected 'body error' in '{msg}'"
        );
    }

    #[test]
    fn test_display_timeout() {
        let err = OxiHttpError::Timeout("request timed out".to_string());
        let msg = err.to_string();
        assert!(msg.contains("timeout"), "expected 'timeout' in '{msg}'");
    }

    #[test]
    fn test_display_redirect() {
        let err = OxiHttpError::Redirect("too many redirects".to_string());
        let msg = err.to_string();
        assert!(
            msg.contains("redirect error"),
            "expected 'redirect error' in '{msg}'"
        );
    }

    #[test]
    fn test_display_tls() {
        let err = OxiHttpError::Tls("certificate invalid".to_string());
        let msg = err.to_string();
        assert!(msg.contains("TLS error"), "expected 'TLS error' in '{msg}'");
    }

    #[test]
    fn test_display_dns() {
        let err = OxiHttpError::Dns("no such host".to_string());
        let msg = err.to_string();
        assert!(msg.contains("DNS error"), "expected 'DNS error' in '{msg}'");
    }

    #[test]
    fn test_display_connection_pool() {
        let err = OxiHttpError::ConnectionPool("pool exhausted".to_string());
        let msg = err.to_string();
        assert!(
            msg.contains("connection pool error"),
            "expected 'connection pool error' in '{msg}'"
        );
    }

    #[test]
    fn test_display_json() {
        let err = OxiHttpError::Json("unexpected token".to_string());
        let msg = err.to_string();
        assert!(
            msg.contains("JSON error"),
            "expected 'JSON error' in '{msg}'"
        );
    }

    #[test]
    fn test_display_route_not_found() {
        let err = OxiHttpError::RouteNotFound {
            method: "GET".to_string(),
            path: "/foo".to_string(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("route not found"),
            "expected 'route not found' in '{msg}'"
        );
        assert!(msg.contains("GET"), "expected 'GET' in '{msg}'");
        assert!(msg.contains("/foo"), "expected '/foo' in '{msg}'");
    }

    #[test]
    fn test_display_method_not_allowed() {
        let err = OxiHttpError::MethodNotAllowed {
            method: "DELETE".to_string(),
            path: "/bar".to_string(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("method not allowed"),
            "expected 'method not allowed' in '{msg}'"
        );
        assert!(msg.contains("DELETE"), "expected 'DELETE' in '{msg}'");
        assert!(msg.contains("/bar"), "expected '/bar' in '{msg}'");
    }

    // -------------------------------------------------------------------------
    // From conversion tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_from_invalid_uri() {
        let raw: http::uri::InvalidUri = "not a valid uri!!!"
            .parse::<http::Uri>()
            .expect_err("should fail");
        let result = OxiHttpError::from(raw);
        assert!(
            matches!(result, OxiHttpError::InvalidUri(_)),
            "expected InvalidUri variant"
        );
    }

    #[test]
    fn test_from_http_error() {
        let raw = http::Request::builder()
            .header("\n", "x")
            .body(())
            .expect_err("should fail with invalid header name");
        let result = OxiHttpError::from(raw);
        assert!(
            matches!(result, OxiHttpError::Http(_)),
            "expected Http variant"
        );
    }

    #[test]
    fn test_from_io_error() {
        let raw = std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "test io error message",
        );
        let result = OxiHttpError::from(raw);
        assert!(matches!(result, OxiHttpError::Io(_)), "expected Io variant");
        assert!(
            result.to_string().contains("test io error message"),
            "Display should include the original io message"
        );
    }

    // -------------------------------------------------------------------------
    // status_code() tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_status_code_route_not_found() {
        let err = OxiHttpError::RouteNotFound {
            method: "GET".to_string(),
            path: "/missing".to_string(),
        };
        assert_eq!(err.status_code(), Some(http::StatusCode::NOT_FOUND));
    }

    #[test]
    fn test_status_code_method_not_allowed() {
        let err = OxiHttpError::MethodNotAllowed {
            method: "PUT".to_string(),
            path: "/resource".to_string(),
        };
        assert_eq!(
            err.status_code(),
            Some(http::StatusCode::METHOD_NOT_ALLOWED)
        );
    }

    #[test]
    fn test_status_code_timeout() {
        let err = OxiHttpError::Timeout("waited too long".to_string());
        assert_eq!(err.status_code(), Some(http::StatusCode::REQUEST_TIMEOUT));
    }

    #[test]
    fn test_status_code_body_is_none() {
        let err = OxiHttpError::Body("incomplete body".to_string());
        assert_eq!(err.status_code(), None);
    }

    // -------------------------------------------------------------------------
    // Predicate tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_is_timeout_true() {
        let err = OxiHttpError::Timeout("timed out".to_string());
        assert!(err.is_timeout());
    }

    #[test]
    fn test_is_timeout_false() {
        let err = OxiHttpError::Body("body error".to_string());
        assert!(!err.is_timeout());
    }

    #[test]
    fn test_is_connect_dns() {
        let err = OxiHttpError::Dns("nxdomain".to_string());
        assert!(err.is_connect());
    }

    #[test]
    fn test_is_connect_pool() {
        let err = OxiHttpError::ConnectionPool("exhausted".to_string());
        assert!(err.is_connect());
    }

    #[test]
    fn test_is_connect_tls() {
        let err = OxiHttpError::Tls("bad cert".to_string());
        assert!(err.is_connect());
    }

    #[test]
    fn test_is_connect_false() {
        let err = OxiHttpError::Timeout("timed out".to_string());
        assert!(!err.is_connect());
    }

    #[test]
    fn test_is_body_true() {
        let err = OxiHttpError::Body("truncated".to_string());
        assert!(err.is_body());
    }

    #[test]
    fn test_is_body_false() {
        let err = OxiHttpError::Json("bad json".to_string());
        assert!(!err.is_body());
    }

    #[test]
    fn test_is_redirect_true() {
        let err = OxiHttpError::Redirect("loop detected".to_string());
        assert!(err.is_redirect());
    }

    #[test]
    fn test_is_redirect_false() {
        let err = OxiHttpError::Timeout("timed out".to_string());
        assert!(!err.is_redirect());
    }
}
