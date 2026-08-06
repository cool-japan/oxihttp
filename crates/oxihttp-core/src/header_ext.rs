//! Typed header extension traits for `HeaderMap`.

use http::{HeaderMap, HeaderValue};

use crate::content_type::ContentType;
use crate::OxiHttpError;

/// Extension trait providing typed accessors for common HTTP headers.
///
/// Implemented for `http::HeaderMap`; import the trait to call these methods
/// directly on any `HeaderMap` value (request headers, response headers, or
/// a freshly-built map).
///
/// # Example
///
/// ```rust
/// use http::HeaderMap;
/// use oxihttp_core::{ContentType, HeaderMapExt};
///
/// let mut headers = HeaderMap::new();
/// headers.set_content_type(&ContentType::Json).expect("valid header value");
/// headers.set_bearer_auth("secret-token").expect("valid header value");
///
/// assert_eq!(headers.content_type(), Some(ContentType::Json));
/// assert_eq!(headers.authorization(), Some("Bearer secret-token"));
/// assert_eq!(headers.host(), None);
/// ```
pub trait HeaderMapExt {
    /// Get the `Content-Type` header as a parsed `ContentType`.
    fn content_type(&self) -> Option<ContentType>;

    /// Get the `Content-Length` header as a `u64`.
    fn content_length(&self) -> Option<u64>;

    /// Get the `Authorization` header value.
    fn authorization(&self) -> Option<&str>;

    /// Get the `Accept` header value.
    fn accept(&self) -> Option<&str>;

    /// Get the `Host` header value.
    fn host(&self) -> Option<&str>;

    /// Get the `User-Agent` header value.
    fn user_agent(&self) -> Option<&str>;

    /// Get the `Cache-Control` header value.
    fn cache_control(&self) -> Option<&str>;

    /// Get the `ETag` header value.
    fn etag(&self) -> Option<&str>;

    /// Get the `If-None-Match` header value.
    fn if_none_match(&self) -> Option<&str>;

    /// Get the `If-Modified-Since` header value.
    fn if_modified_since(&self) -> Option<&str>;

    /// Get the raw `Cookie` header value (not parsed).
    fn cookie_header(&self) -> Option<&str>;

    /// Get the `Location` header value.
    fn location(&self) -> Option<&str>;

    /// Get the `Referer` header value.
    fn referer(&self) -> Option<&str>;

    /// Set a typed `Content-Type` header.
    fn set_content_type(&mut self, ct: &ContentType) -> Result<(), OxiHttpError>;

    /// Set a `Content-Length` header.
    fn set_content_length(&mut self, len: u64) -> Result<(), OxiHttpError>;

    /// Set a `Bearer` authorization header.
    fn set_bearer_auth(&mut self, token: &str) -> Result<(), OxiHttpError>;

    /// Set a `Basic` authorization header from username and password.
    fn set_basic_auth(
        &mut self,
        username: &str,
        password: Option<&str>,
    ) -> Result<(), OxiHttpError>;

    /// Set the `Cache-Control` header.
    fn set_cache_control(&mut self, value: &str) -> Result<(), OxiHttpError>;

    /// Set the `ETag` header.
    fn set_etag(&mut self, value: &str) -> Result<(), OxiHttpError>;

    /// Set the `Location` header.
    fn set_location(&mut self, value: &str) -> Result<(), OxiHttpError>;

    /// Append a `Set-Cookie` header value (uses `append` per RFC 6265).
    fn set_cookie_header(&mut self, value: &str) -> Result<(), OxiHttpError>;
}

impl HeaderMapExt for HeaderMap {
    fn content_type(&self) -> Option<ContentType> {
        let val = self.get(http::header::CONTENT_TYPE)?;
        val.to_str().ok()?.parse().ok()
    }

    fn content_length(&self) -> Option<u64> {
        let val = self.get(http::header::CONTENT_LENGTH)?;
        val.to_str().ok()?.parse().ok()
    }

    fn authorization(&self) -> Option<&str> {
        self.get(http::header::AUTHORIZATION)?.to_str().ok()
    }

    fn accept(&self) -> Option<&str> {
        self.get(http::header::ACCEPT)?.to_str().ok()
    }

    fn host(&self) -> Option<&str> {
        self.get(http::header::HOST)?.to_str().ok()
    }

    fn user_agent(&self) -> Option<&str> {
        self.get(http::header::USER_AGENT)?.to_str().ok()
    }

    fn cache_control(&self) -> Option<&str> {
        self.get(http::header::CACHE_CONTROL)?.to_str().ok()
    }

    fn etag(&self) -> Option<&str> {
        self.get(http::header::ETAG)?.to_str().ok()
    }

    fn if_none_match(&self) -> Option<&str> {
        self.get(http::header::IF_NONE_MATCH)?.to_str().ok()
    }

    fn if_modified_since(&self) -> Option<&str> {
        self.get(http::header::IF_MODIFIED_SINCE)?.to_str().ok()
    }

    fn cookie_header(&self) -> Option<&str> {
        self.get(http::header::COOKIE)?.to_str().ok()
    }

    fn location(&self) -> Option<&str> {
        self.get(http::header::LOCATION)?.to_str().ok()
    }

    fn referer(&self) -> Option<&str> {
        self.get(http::header::REFERER)?.to_str().ok()
    }

    fn set_content_type(&mut self, ct: &ContentType) -> Result<(), OxiHttpError> {
        let val = HeaderValue::from_str(&ct.to_string())
            .map_err(|e| OxiHttpError::InvalidHeader(e.to_string()))?;
        self.insert(http::header::CONTENT_TYPE, val);
        Ok(())
    }

    fn set_content_length(&mut self, len: u64) -> Result<(), OxiHttpError> {
        let val = HeaderValue::from_str(&len.to_string())
            .map_err(|e| OxiHttpError::InvalidHeader(e.to_string()))?;
        self.insert(http::header::CONTENT_LENGTH, val);
        Ok(())
    }

    fn set_bearer_auth(&mut self, token: &str) -> Result<(), OxiHttpError> {
        let val = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| OxiHttpError::InvalidHeader(e.to_string()))?;
        self.insert(http::header::AUTHORIZATION, val);
        Ok(())
    }

    fn set_basic_auth(
        &mut self,
        username: &str,
        password: Option<&str>,
    ) -> Result<(), OxiHttpError> {
        use std::io::Write;
        let mut buf = Vec::new();
        let _ = write!(buf, "{username}:");
        if let Some(pw) = password {
            let _ = write!(buf, "{pw}");
        }
        // Base64 encode without external dependency - simple implementation
        let encoded = base64_encode(&buf);
        let val = HeaderValue::from_str(&format!("Basic {encoded}"))
            .map_err(|e| OxiHttpError::InvalidHeader(e.to_string()))?;
        self.insert(http::header::AUTHORIZATION, val);
        Ok(())
    }

    fn set_cache_control(&mut self, value: &str) -> Result<(), OxiHttpError> {
        let hv = HeaderValue::from_str(value).map_err(|_| {
            OxiHttpError::InvalidHeader(format!("invalid Cache-Control value: {value}"))
        })?;
        self.insert(http::header::CACHE_CONTROL, hv);
        Ok(())
    }

    fn set_etag(&mut self, value: &str) -> Result<(), OxiHttpError> {
        let hv = HeaderValue::from_str(value)
            .map_err(|_| OxiHttpError::InvalidHeader(format!("invalid ETag value: {value}")))?;
        self.insert(http::header::ETAG, hv);
        Ok(())
    }

    fn set_location(&mut self, value: &str) -> Result<(), OxiHttpError> {
        let hv = HeaderValue::from_str(value)
            .map_err(|_| OxiHttpError::InvalidHeader(format!("invalid Location value: {value}")))?;
        self.insert(http::header::LOCATION, hv);
        Ok(())
    }

    fn set_cookie_header(&mut self, value: &str) -> Result<(), OxiHttpError> {
        let hv = HeaderValue::from_str(value).map_err(|_| {
            OxiHttpError::InvalidHeader(format!("invalid Set-Cookie value: {value}"))
        })?;
        self.append(http::header::SET_COOKIE, hv);
        Ok(())
    }
}

/// Simple base64 encoding (RFC 4648) without external dependency.
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_type_accessor() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        assert_eq!(headers.content_type(), Some(ContentType::Json));
    }

    #[test]
    fn test_content_length_accessor() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::CONTENT_LENGTH, HeaderValue::from_static("42"));
        assert_eq!(headers.content_length(), Some(42));
    }

    #[test]
    fn test_set_bearer_auth() {
        let mut headers = HeaderMap::new();
        headers.set_bearer_auth("mytoken123").expect("set bearer");
        assert_eq!(headers.authorization(), Some("Bearer mytoken123"));
    }

    #[test]
    fn test_set_basic_auth() {
        let mut headers = HeaderMap::new();
        headers
            .set_basic_auth("user", Some("pass"))
            .expect("set basic");
        let auth = headers.authorization().expect("auth present");
        assert!(auth.starts_with("Basic "));
        // "user:pass" base64 = "dXNlcjpwYXNz"
        assert_eq!(auth, "Basic dXNlcjpwYXNz");
    }

    #[test]
    fn test_set_content_type() {
        let mut headers = HeaderMap::new();
        headers
            .set_content_type(&ContentType::Json)
            .expect("set ct");
        assert_eq!(headers.content_type(), Some(ContentType::Json));
    }

    #[test]
    fn test_host_accessor() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::HOST, HeaderValue::from_static("example.com"));
        assert_eq!(headers.host(), Some("example.com"));
    }

    #[test]
    fn test_base64_encode() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn test_cache_control_getter_setter() {
        let mut headers = HeaderMap::new();
        headers
            .set_cache_control("no-store, max-age=0")
            .expect("set cache-control");
        assert_eq!(headers.cache_control(), Some("no-store, max-age=0"));
    }

    #[test]
    fn test_etag_getter_setter() {
        let mut headers = HeaderMap::new();
        headers
            .set_etag("\"33a64df551425fcc55e4d42a148795d9f25f89d4\"")
            .expect("set etag");
        assert_eq!(
            headers.etag(),
            Some("\"33a64df551425fcc55e4d42a148795d9f25f89d4\"")
        );
    }

    #[test]
    fn test_if_none_match_getter() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::IF_NONE_MATCH,
            HeaderValue::from_static("\"abc123\""),
        );
        assert_eq!(headers.if_none_match(), Some("\"abc123\""));
    }

    #[test]
    fn test_if_modified_since_getter() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::IF_MODIFIED_SINCE,
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(
            headers.if_modified_since(),
            Some("Wed, 21 Oct 2015 07:28:00 GMT")
        );
    }

    #[test]
    fn test_cookie_header_getter() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::COOKIE,
            HeaderValue::from_static("session=abc"),
        );
        assert_eq!(headers.cookie_header(), Some("session=abc"));
    }

    #[test]
    fn test_location_getter_setter() {
        let mut headers = HeaderMap::new();
        headers
            .set_location("https://example.com/new-path")
            .expect("set location");
        assert_eq!(headers.location(), Some("https://example.com/new-path"));
    }

    #[test]
    fn test_referer_getter() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::REFERER,
            HeaderValue::from_static("https://example.com/page"),
        );
        assert_eq!(headers.referer(), Some("https://example.com/page"));
    }

    #[test]
    fn test_set_cookie_header_appends() {
        let mut headers = HeaderMap::new();
        headers
            .set_cookie_header("session=abc; Path=/; HttpOnly")
            .expect("first set-cookie");
        headers
            .set_cookie_header("theme=dark; Path=/; Max-Age=31536000")
            .expect("second set-cookie");
        let values: Vec<&str> = headers
            .get_all(http::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        assert_eq!(values.len(), 2);
        assert!(values.contains(&"session=abc; Path=/; HttpOnly"));
        assert!(values.contains(&"theme=dark; Path=/; Max-Age=31536000"));
    }
}
