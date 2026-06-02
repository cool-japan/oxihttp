//! Typed header extraction framework for the OxiHTTP stack.
//!
//! This module provides the [`Header`] trait and concrete typed header structs
//! that wrap common HTTP header values with proper Rust types.

use crate::content_type::ContentType;
use crate::header_ext::HeaderMapExt;
use crate::OxiHttpError;
use http::HeaderMap;

/// Trait for typed header extraction from a [`HeaderMap`].
///
/// Implement this trait to define how a specific HTTP header is read and
/// decoded from a [`HeaderMap`] into a typed Rust value.
pub trait Header: Sized {
    /// Returns the canonical [`http::header::HeaderName`] for this header.
    fn header_name() -> http::header::HeaderName;

    /// Decode the header from a [`HeaderMap`], returning an error if the
    /// header is absent or its value is not valid for this type.
    fn decode(headers: &HeaderMap) -> Result<Self, OxiHttpError>;
}

// ---------------------------------------------------------------------------
// ContentType implements Header
// ---------------------------------------------------------------------------

impl Header for ContentType {
    fn header_name() -> http::header::HeaderName {
        http::header::CONTENT_TYPE
    }

    fn decode(headers: &HeaderMap) -> Result<Self, OxiHttpError> {
        headers.content_type().ok_or_else(|| {
            OxiHttpError::InvalidHeader("missing or invalid Content-Type header".to_string())
        })
    }
}

// ---------------------------------------------------------------------------
// ContentLength
// ---------------------------------------------------------------------------

/// Typed wrapper for the `Content-Length` header (a non-negative integer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentLength(pub u64);

impl Header for ContentLength {
    fn header_name() -> http::header::HeaderName {
        http::header::CONTENT_LENGTH
    }

    fn decode(headers: &HeaderMap) -> Result<Self, OxiHttpError> {
        let val = headers.get(http::header::CONTENT_LENGTH).ok_or_else(|| {
            OxiHttpError::InvalidHeader("missing Content-Length header".to_string())
        })?;
        let s = val.to_str().map_err(|e| {
            OxiHttpError::InvalidHeader(format!("invalid Content-Length value: {e}"))
        })?;
        s.trim()
            .parse::<u64>()
            .map(ContentLength)
            .map_err(|e| OxiHttpError::InvalidHeader(format!("cannot parse Content-Length: {e}")))
    }
}

impl std::fmt::Display for ContentLength {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Macro for simple string-value typed headers
// ---------------------------------------------------------------------------

macro_rules! string_header {
    (
        $(#[$meta:meta])*
        $name:ident, $header_const:expr, $display_name:literal
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $name(pub String);

        impl Header for $name {
            fn header_name() -> http::header::HeaderName {
                $header_const
            }

            fn decode(headers: &HeaderMap) -> Result<Self, OxiHttpError> {
                let val = headers
                    .get($header_const)
                    .ok_or_else(|| OxiHttpError::InvalidHeader(
                        format!("missing {} header", $display_name)
                    ))?;
                let s = val
                    .to_str()
                    .map_err(|e| OxiHttpError::InvalidHeader(
                        format!("invalid {} header value: {}", $display_name, e)
                    ))?;
                Ok($name(s.to_string()))
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_header!(
    /// Typed wrapper for the `Host` header.
    Host,
    http::header::HOST,
    "Host"
);

string_header!(
    /// Typed wrapper for the `ETag` header.
    ETag,
    http::header::ETAG,
    "ETag"
);

string_header!(
    /// Typed wrapper for the `Authorization` header.
    Authorization,
    http::header::AUTHORIZATION,
    "Authorization"
);

string_header!(
    /// Typed wrapper for the `Cache-Control` header.
    CacheControl,
    http::header::CACHE_CONTROL,
    "Cache-Control"
);

string_header!(
    /// Typed wrapper for the `Referer` header.
    Referer,
    http::header::REFERER,
    "Referer"
);

string_header!(
    /// Typed wrapper for the `Location` header.
    Location,
    http::header::LOCATION,
    "Location"
);

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    // ---- ContentLength -------------------------------------------------------

    #[test]
    fn test_content_length_decode_ok() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::CONTENT_LENGTH, HeaderValue::from_static("42"));
        let result = ContentLength::decode(&headers).expect("should decode");
        assert_eq!(result, ContentLength(42));
        assert_eq!(result.to_string(), "42");
    }

    #[test]
    fn test_content_length_decode_missing() {
        let headers = HeaderMap::new();
        assert!(ContentLength::decode(&headers).is_err());
    }

    // ---- Host ---------------------------------------------------------------

    #[test]
    fn test_host_decode_ok() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::HOST, HeaderValue::from_static("example.com"));
        let result = Host::decode(&headers).expect("should decode");
        assert_eq!(result, Host("example.com".to_string()));
        assert_eq!(result.to_string(), "example.com");
    }

    #[test]
    fn test_host_decode_missing() {
        let headers = HeaderMap::new();
        assert!(Host::decode(&headers).is_err());
    }

    // ---- ETag ---------------------------------------------------------------

    #[test]
    fn test_etag_decode_ok() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::ETAG, HeaderValue::from_static("\"abc123\""));
        let result = ETag::decode(&headers).expect("should decode");
        assert_eq!(result, ETag("\"abc123\"".to_string()));
        assert_eq!(result.to_string(), "\"abc123\"");
    }

    #[test]
    fn test_etag_decode_missing() {
        let headers = HeaderMap::new();
        assert!(ETag::decode(&headers).is_err());
    }

    // ---- Authorization ------------------------------------------------------

    #[test]
    fn test_authorization_decode_ok() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer token123"),
        );
        let result = Authorization::decode(&headers).expect("should decode");
        assert_eq!(result, Authorization("Bearer token123".to_string()));
        assert_eq!(result.to_string(), "Bearer token123");
    }

    #[test]
    fn test_authorization_decode_missing() {
        let headers = HeaderMap::new();
        assert!(Authorization::decode(&headers).is_err());
    }

    // ---- CacheControl -------------------------------------------------------

    #[test]
    fn test_cache_control_decode_ok() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, max-age=0"),
        );
        let result = CacheControl::decode(&headers).expect("should decode");
        assert_eq!(result, CacheControl("no-store, max-age=0".to_string()));
        assert_eq!(result.to_string(), "no-store, max-age=0");
    }

    #[test]
    fn test_cache_control_decode_missing() {
        let headers = HeaderMap::new();
        assert!(CacheControl::decode(&headers).is_err());
    }

    // ---- Referer ------------------------------------------------------------

    #[test]
    fn test_referer_decode_ok() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::REFERER,
            HeaderValue::from_static("https://example.com/page"),
        );
        let result = Referer::decode(&headers).expect("should decode");
        assert_eq!(result, Referer("https://example.com/page".to_string()));
        assert_eq!(result.to_string(), "https://example.com/page");
    }

    #[test]
    fn test_referer_decode_missing() {
        let headers = HeaderMap::new();
        assert!(Referer::decode(&headers).is_err());
    }

    // ---- Location -----------------------------------------------------------

    #[test]
    fn test_location_decode_ok() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::LOCATION,
            HeaderValue::from_static("https://example.com/new-path"),
        );
        let result = Location::decode(&headers).expect("should decode");
        assert_eq!(result, Location("https://example.com/new-path".to_string()));
        assert_eq!(result.to_string(), "https://example.com/new-path");
    }

    #[test]
    fn test_location_decode_missing() {
        let headers = HeaderMap::new();
        assert!(Location::decode(&headers).is_err());
    }

    // ---- ContentType Header impl -------------------------------------------

    #[test]
    fn test_content_type_header_impl_ok() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        let result = ContentType::decode(&headers).expect("should decode");
        assert_eq!(result, ContentType::Json);
    }

    #[test]
    fn test_content_type_header_impl_missing() {
        let headers = HeaderMap::new();
        assert!(ContentType::decode(&headers).is_err());
    }
}
