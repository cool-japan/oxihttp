//! HTTP protocol version enum.

use std::fmt;
use std::str::FromStr;

/// HTTP protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HttpVersion {
    /// HTTP/1.0
    Http10,
    /// HTTP/1.1
    Http11,
    /// HTTP/2
    H2,
    /// HTTP/3
    H3,
}

impl HttpVersion {
    /// Returns the canonical string representation of this HTTP version.
    ///
    /// # Examples
    ///
    /// ```
    /// use oxihttp_core::HttpVersion;
    ///
    /// assert_eq!(HttpVersion::Http11.as_str(), "HTTP/1.1");
    /// assert_eq!(HttpVersion::H2.as_str(), "HTTP/2");
    /// ```
    pub fn as_str(&self) -> &'static str {
        match self {
            HttpVersion::Http10 => "HTTP/1.0",
            HttpVersion::Http11 => "HTTP/1.1",
            HttpVersion::H2 => "HTTP/2",
            HttpVersion::H3 => "HTTP/3",
        }
    }

    /// Returns `true` if this is HTTP/1.x.
    pub fn is_http1(&self) -> bool {
        matches!(self, HttpVersion::Http10 | HttpVersion::Http11)
    }

    /// Returns `true` if this is HTTP/2.
    pub fn is_http2(&self) -> bool {
        matches!(self, HttpVersion::H2)
    }

    /// Returns `true` if this is HTTP/3.
    pub fn is_http3(&self) -> bool {
        matches!(self, HttpVersion::H3)
    }
}

impl fmt::Display for HttpVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HttpVersion {
    type Err = crate::OxiHttpError;

    /// Parses an HTTP version string, case-insensitively.
    ///
    /// Accepts: `"HTTP/1.0"`, `"HTTP/1.1"`, `"HTTP/2"`, `"HTTP/3"` and
    /// lowercase variants thereof.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_uppercase().as_str() {
            "HTTP/1.0" => Ok(HttpVersion::Http10),
            "HTTP/1.1" => Ok(HttpVersion::Http11),
            "HTTP/2" | "H2" => Ok(HttpVersion::H2),
            "HTTP/3" | "H3" => Ok(HttpVersion::H3),
            _ => Err(crate::OxiHttpError::InvalidHeader(format!(
                "unknown HTTP version: {s}"
            ))),
        }
    }
}

impl From<http::Version> for HttpVersion {
    fn from(v: http::Version) -> Self {
        match v {
            http::Version::HTTP_10 => HttpVersion::Http10,
            http::Version::HTTP_11 => HttpVersion::Http11,
            http::Version::HTTP_2 => HttpVersion::H2,
            http::Version::HTTP_3 => HttpVersion::H3,
            // Unknown / future versions — use HTTP/1.1 as a safe default.
            _ => HttpVersion::Http11,
        }
    }
}

impl From<HttpVersion> for http::Version {
    fn from(v: HttpVersion) -> http::Version {
        match v {
            HttpVersion::Http10 => http::Version::HTTP_10,
            HttpVersion::Http11 => http::Version::HTTP_11,
            HttpVersion::H2 => http::Version::HTTP_2,
            HttpVersion::H3 => http::Version::HTTP_3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_as_str() {
        assert_eq!(HttpVersion::Http10.as_str(), "HTTP/1.0");
        assert_eq!(HttpVersion::Http11.as_str(), "HTTP/1.1");
        assert_eq!(HttpVersion::H2.as_str(), "HTTP/2");
        assert_eq!(HttpVersion::H3.as_str(), "HTTP/3");
    }

    #[test]
    fn test_display() {
        assert_eq!(HttpVersion::Http11.to_string(), "HTTP/1.1");
        assert_eq!(HttpVersion::H2.to_string(), "HTTP/2");
    }

    #[test]
    fn test_from_str_canonical() {
        assert_eq!(
            "HTTP/1.0".parse::<HttpVersion>().unwrap(),
            HttpVersion::Http10
        );
        assert_eq!(
            "HTTP/1.1".parse::<HttpVersion>().unwrap(),
            HttpVersion::Http11
        );
        assert_eq!("HTTP/2".parse::<HttpVersion>().unwrap(), HttpVersion::H2);
        assert_eq!("HTTP/3".parse::<HttpVersion>().unwrap(), HttpVersion::H3);
    }

    #[test]
    fn test_from_str_case_insensitive() {
        assert_eq!(
            "http/1.0".parse::<HttpVersion>().unwrap(),
            HttpVersion::Http10
        );
        assert_eq!(
            "http/1.1".parse::<HttpVersion>().unwrap(),
            HttpVersion::Http11
        );
        assert_eq!("http/2".parse::<HttpVersion>().unwrap(), HttpVersion::H2);
        assert_eq!("http/3".parse::<HttpVersion>().unwrap(), HttpVersion::H3);
        assert_eq!("h2".parse::<HttpVersion>().unwrap(), HttpVersion::H2);
        assert_eq!("h3".parse::<HttpVersion>().unwrap(), HttpVersion::H3);
    }

    #[test]
    fn test_from_str_invalid() {
        assert!("HTTP/4".parse::<HttpVersion>().is_err());
        assert!("".parse::<HttpVersion>().is_err());
        assert!("1.1".parse::<HttpVersion>().is_err());
    }

    #[test]
    fn test_from_http_version() {
        assert_eq!(
            HttpVersion::from(http::Version::HTTP_10),
            HttpVersion::Http10
        );
        assert_eq!(
            HttpVersion::from(http::Version::HTTP_11),
            HttpVersion::Http11
        );
        assert_eq!(HttpVersion::from(http::Version::HTTP_2), HttpVersion::H2);
        assert_eq!(HttpVersion::from(http::Version::HTTP_3), HttpVersion::H3);
    }

    #[test]
    fn test_into_http_version() {
        assert_eq!(
            http::Version::from(HttpVersion::Http10),
            http::Version::HTTP_10
        );
        assert_eq!(
            http::Version::from(HttpVersion::Http11),
            http::Version::HTTP_11
        );
        assert_eq!(http::Version::from(HttpVersion::H2), http::Version::HTTP_2);
        assert_eq!(http::Version::from(HttpVersion::H3), http::Version::HTTP_3);
    }

    #[test]
    fn test_round_trip() {
        for v in [
            HttpVersion::Http10,
            HttpVersion::Http11,
            HttpVersion::H2,
            HttpVersion::H3,
        ] {
            let http_v: http::Version = v.into();
            let back: HttpVersion = http_v.into();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn test_predicates() {
        assert!(HttpVersion::Http10.is_http1());
        assert!(HttpVersion::Http11.is_http1());
        assert!(!HttpVersion::H2.is_http1());
        assert!(HttpVersion::H2.is_http2());
        assert!(!HttpVersion::H2.is_http3());
        assert!(HttpVersion::H3.is_http3());
    }
}
