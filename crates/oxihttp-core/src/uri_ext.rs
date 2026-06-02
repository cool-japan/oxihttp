//! URI extension methods.

use http::Uri;

/// Extension trait providing convenience methods for `http::Uri`.
pub trait UriExt {
    /// Extract the host from the URI.
    fn host_str(&self) -> Option<&str>;

    /// Return the port, or the default port for the scheme (80 for HTTP, 443 for HTTPS).
    fn port_or_default(&self) -> Option<u16>;

    /// Returns `true` if the URI scheme is `https`.
    fn is_https(&self) -> bool;

    /// Returns `true` if the URI scheme is `http`.
    fn is_http(&self) -> bool;

    /// Return the origin portion of the URI (scheme + authority).
    fn origin(&self) -> Option<String>;
}

impl UriExt for Uri {
    fn host_str(&self) -> Option<&str> {
        self.host()
    }

    fn port_or_default(&self) -> Option<u16> {
        if let Some(port) = self.port_u16() {
            return Some(port);
        }
        match self.scheme_str() {
            Some("https") => Some(443),
            Some("http") => Some(80),
            _ => None,
        }
    }

    fn is_https(&self) -> bool {
        self.scheme_str() == Some("https")
    }

    fn is_http(&self) -> bool {
        self.scheme_str() == Some("http")
    }

    fn origin(&self) -> Option<String> {
        let scheme = self.scheme_str()?;
        let authority = self.authority()?.as_str();
        Some(format!("{scheme}://{authority}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_host() {
        let uri = Uri::from_str("http://example.com/path").expect("parse");
        assert_eq!(uri.host_str(), Some("example.com"));
    }

    #[test]
    fn test_port_or_default_http() {
        let uri = Uri::from_str("http://example.com/path").expect("parse");
        assert_eq!(uri.port_or_default(), Some(80));
    }

    #[test]
    fn test_port_or_default_https() {
        let uri = Uri::from_str("https://example.com/path").expect("parse");
        assert_eq!(uri.port_or_default(), Some(443));
    }

    #[test]
    fn test_port_or_default_explicit() {
        let uri = Uri::from_str("http://example.com:8080/path").expect("parse");
        assert_eq!(uri.port_or_default(), Some(8080));
    }

    #[test]
    fn test_is_https() {
        let uri = Uri::from_str("https://example.com").expect("parse");
        assert!(uri.is_https());
        assert!(!uri.is_http());
    }

    #[test]
    fn test_is_http() {
        let uri = Uri::from_str("http://example.com").expect("parse");
        assert!(uri.is_http());
        assert!(!uri.is_https());
    }

    #[test]
    fn test_origin() {
        let uri = Uri::from_str("https://example.com:8443/path?q=1").expect("parse");
        assert_eq!(uri.origin(), Some("https://example.com:8443".to_string()));
    }

    #[test]
    fn test_no_scheme() {
        let uri = Uri::from_str("/path/only").expect("parse");
        assert_eq!(uri.port_or_default(), None);
        assert!(!uri.is_http());
        assert!(!uri.is_https());
        assert!(uri.origin().is_none());
    }
}
