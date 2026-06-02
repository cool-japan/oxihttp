//! Cookie parsing and management for the OxiHTTP stack.
//!
//! Provides a `Cookie` struct for individual cookie values and a `CookieJar`
//! for managing cookies across multiple requests and responses.

use std::fmt;
use std::time::Duration;

/// Represents a single HTTP cookie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: Option<String>,
    pub path: Option<String>,
    /// Max-Age attribute stored as a Duration.
    pub max_age: Option<Duration>,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: Option<SameSite>,
    /// Computed absolute expiry time from max_age, set at insertion.
    pub expires_at: Option<std::time::Instant>,
}

/// The SameSite cookie attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameSite {
    /// Cookies are sent with same-site requests only.
    Strict,
    /// Cookies are sent with same-site requests and top-level navigations.
    Lax,
    /// Cookies are sent with all requests (requires Secure).
    None,
}

impl Cookie {
    /// Create a new cookie with the given name and value.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            domain: None,
            path: None,
            max_age: None,
            secure: false,
            http_only: false,
            same_site: None,
            expires_at: None,
        }
    }

    /// The cookie name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The cookie value.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// The domain attribute.
    pub fn domain(&self) -> Option<&str> {
        self.domain.as_deref()
    }

    /// The path attribute.
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// The max-age attribute as a `Duration`.
    pub fn max_age(&self) -> Option<Duration> {
        self.max_age
    }

    /// Whether the Secure flag is set.
    pub fn is_secure(&self) -> bool {
        self.secure
    }

    /// Whether the HttpOnly flag is set.
    pub fn is_http_only(&self) -> bool {
        self.http_only
    }

    /// The SameSite attribute.
    pub fn same_site(&self) -> Option<SameSite> {
        self.same_site
    }

    /// Set the domain attribute.
    pub fn set_domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }

    /// Set the path attribute.
    pub fn set_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Set the max-age attribute in seconds.
    pub fn set_max_age(mut self, seconds: u64) -> Self {
        self.max_age = Some(Duration::from_secs(seconds));
        self
    }

    /// Set the Secure flag.
    pub fn set_secure(mut self, secure: bool) -> Self {
        self.secure = secure;
        self
    }

    /// Set the HttpOnly flag.
    pub fn set_http_only(mut self, http_only: bool) -> Self {
        self.http_only = http_only;
        self
    }

    /// Set the SameSite attribute.
    pub fn set_same_site(mut self, same_site: SameSite) -> Self {
        self.same_site = Some(same_site);
        self
    }

    /// Parse a cookie from a `Set-Cookie` header value (RFC 6265).
    /// `expires_at` is left as `None` here; it is computed at insertion time by `CookieJar`.
    pub fn parse_set_cookie(header: &str) -> Option<Self> {
        let mut parts = header.split(';');
        let first = parts.next()?.trim();
        let (name, value) = first.split_once('=')?;
        let name = name.trim();
        let value = value.trim();

        if name.is_empty() {
            return None;
        }

        let mut cookie = Cookie::new(name, value);

        for attr in parts {
            let attr = attr.trim();
            if attr.is_empty() {
                continue;
            }
            if let Some((key, val)) = attr.split_once('=') {
                let key = key.trim().to_lowercase();
                let val = val.trim();
                match key.as_str() {
                    "domain" => cookie.domain = Some(val.to_string()),
                    "path" => cookie.path = Some(val.to_string()),
                    "max-age" => {
                        cookie.max_age = val.parse::<u64>().ok().map(Duration::from_secs);
                    }
                    "samesite" => {
                        cookie.same_site = match val.to_lowercase().as_str() {
                            "strict" => Some(SameSite::Strict),
                            "lax" => Some(SameSite::Lax),
                            "none" => Some(SameSite::None),
                            _ => None,
                        };
                    }
                    _ => {}
                }
            } else {
                match attr.to_lowercase().as_str() {
                    "secure" => cookie.secure = true,
                    "httponly" => cookie.http_only = true,
                    _ => {}
                }
            }
        }

        Some(cookie)
    }

    /// Serialize the cookie for use in a `Cookie` request header.
    pub fn to_cookie_header(&self) -> String {
        format!("{}={}", self.name, self.value)
    }
}

impl fmt::Display for Cookie {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}={}", self.name, self.value)?;
        if let Some(ref domain) = self.domain {
            write!(f, "; Domain={domain}")?;
        }
        if let Some(ref path) = self.path {
            write!(f, "; Path={path}")?;
        }
        if let Some(max_age) = self.max_age {
            write!(f, "; Max-Age={}", max_age.as_secs())?;
        }
        if self.secure {
            write!(f, "; Secure")?;
        }
        if self.http_only {
            write!(f, "; HttpOnly")?;
        }
        if let Some(same_site) = self.same_site {
            match same_site {
                SameSite::Strict => write!(f, "; SameSite=Strict")?,
                SameSite::Lax => write!(f, "; SameSite=Lax")?,
                SameSite::None => write!(f, "; SameSite=None")?,
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// RFC 6265 helpers
// ---------------------------------------------------------------------------

/// RFC 6265 §5.1.3 domain-match
fn domain_match(cookie_domain: &str, request_host: &str) -> bool {
    let cd = cookie_domain.to_lowercase();
    let rh_full = request_host.to_lowercase();
    // Strip port from request_host if present
    let rh = rh_full.split(':').next().unwrap_or(&rh_full);
    // Remove leading dot from cookie domain
    let cd = cd.strip_prefix('.').unwrap_or(&cd);
    // Exact match
    if rh == cd {
        return true;
    }
    // Suffix match: host ends with "." + cookie_domain, and not an IP
    if rh.ends_with(&format!(".{cd}")) {
        // Ensure cookie_domain is not an IP literal
        return cd.parse::<std::net::IpAddr>().is_err();
    }
    false
}

/// RFC 6265 §5.1.4 path-match
fn path_match(cookie_path: &str, request_path: &str) -> bool {
    if request_path == cookie_path {
        return true;
    }
    if let Some(remaining) = request_path.strip_prefix(cookie_path) {
        // Next char must be '/' or cookie_path ends with '/'
        return remaining.starts_with('/') || cookie_path.ends_with('/');
    }
    false
}

// ---------------------------------------------------------------------------
// CookieJar
// ---------------------------------------------------------------------------

/// A jar for collecting and managing cookies across requests and responses.
#[derive(Debug, Clone, Default)]
pub struct CookieJar {
    pub cookies: Vec<Cookie>,
}

impl CookieJar {
    /// Create an empty cookie jar.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or replace a cookie in the jar (keyed by name only, for backward compat).
    pub fn insert(&mut self, cookie: Cookie) {
        self.cookies.retain(|c| c.name != cookie.name);
        self.cookies.push(cookie);
    }

    /// Get a cookie by name (first match).
    pub fn get(&self, name: &str) -> Option<&Cookie> {
        self.cookies.iter().find(|c| c.name == name)
    }

    /// Remove a cookie by name. Returns the removed cookie if it existed.
    pub fn remove(&mut self, name: &str) -> Option<Cookie> {
        if let Some(pos) = self.cookies.iter().position(|c| c.name == name) {
            Some(self.cookies.remove(pos))
        } else {
            None
        }
    }

    /// Iterate over all cookies.
    pub fn iter(&self) -> impl Iterator<Item = &Cookie> {
        self.cookies.iter()
    }

    /// The number of cookies in the jar.
    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    /// Returns `true` if the jar is empty.
    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    /// Build a `Cookie` header value from all cookies in the jar.
    pub fn to_cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|c| c.to_cookie_header())
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Parse cookies from multiple `Set-Cookie` header values and add them to the jar.
    pub fn add_from_set_cookie_headers<'a, I: IntoIterator<Item = &'a str>>(&mut self, headers: I) {
        for header in headers {
            if let Some(cookie) = Cookie::parse_set_cookie(header) {
                self.insert(cookie);
            }
        }
    }

    /// Insert a cookie with URL context for default domain/path assignment and expiry computation.
    pub fn insert_for_url(&mut self, mut cookie: Cookie, request_url: &http::Uri) {
        // Default domain = request host when cookie has no Domain attr
        if cookie.domain.is_none() {
            if let Some(host) = request_url.host() {
                cookie.domain = Some(host.split(':').next().unwrap_or(host).to_lowercase());
            }
        } else {
            // Normalize: remove leading dot
            if let Some(d) = &cookie.domain {
                cookie.domain = Some(d.trim_start_matches('.').to_lowercase());
            }
        }
        // Default path = up to last '/' in request path
        if cookie.path.is_none() {
            let req_path = request_url.path();
            let default_path = if let Some(pos) = req_path.rfind('/') {
                if pos == 0 {
                    "/"
                } else {
                    &req_path[..pos]
                }
            } else {
                "/"
            };
            cookie.path = Some(default_path.to_string());
        }
        // Compute expires_at from max_age
        cookie.expires_at = cookie.max_age.map(|dur| std::time::Instant::now() + dur);

        // Dedup by (name, domain, path)
        let name = cookie.name.clone();
        let domain = cookie.domain.clone();
        let path = cookie.path.clone();
        self.cookies
            .retain(|c| !(c.name == name && c.domain == domain && c.path == path));
        self.cookies.push(cookie);
    }

    /// Returns cookies matching the given URL per RFC 6265 §5.4.
    /// Filters by domain-match, path-match, secure, and expiry.
    /// Result is sorted by path length descending.
    pub fn cookies_for_url(&self, url: &http::Uri) -> Vec<&Cookie> {
        let now = std::time::Instant::now();
        let host = url.host().unwrap_or("");
        let path = url.path();
        let is_secure = url.scheme_str() == Some("https");

        let mut matched: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| {
                // Expiry check
                if let Some(exp) = c.expires_at {
                    if exp <= now {
                        return false;
                    }
                }
                // Secure flag: secure cookies only for https
                if c.secure && !is_secure {
                    return false;
                }
                // Domain match
                let cookie_domain = c.domain.as_deref().unwrap_or("");
                if !cookie_domain.is_empty() && !domain_match(cookie_domain, host) {
                    return false;
                }
                // Path match
                let cookie_path = c.path.as_deref().unwrap_or("/");
                if !path_match(cookie_path, path) {
                    return false;
                }
                true
            })
            .collect();

        // Sort by path length descending (longer path = more specific = first)
        matched.sort_by(|a, b| {
            let a_len = a.path.as_deref().unwrap_or("/").len();
            let b_len = b.path.as_deref().unwrap_or("/").len();
            b_len.cmp(&a_len)
        });
        matched
    }

    /// Build a `Cookie` header value for the given URL, or None if no cookies match.
    pub fn to_cookie_header_for_url(&self, url: &http::Uri) -> Option<String> {
        let matched = self.cookies_for_url(url);
        if matched.is_empty() {
            return None;
        }
        Some(
            matched
                .iter()
                .map(|c| format!("{}={}", c.name, c.value))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    /// Parse all `Set-Cookie` headers from an `http::HeaderMap` and insert them
    /// with URL context for default domain/path and expiry computation.
    pub fn add_from_response_headers(&mut self, headers: &http::HeaderMap, url: &http::Uri) {
        for value in headers.get_all(http::header::SET_COOKIE) {
            if let Ok(s) = value.to_str() {
                if let Some(cookie) = Cookie::parse_set_cookie(s) {
                    self.insert_for_url(cookie, url);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_cookie() {
        let cookie = Cookie::parse_set_cookie("session=abc123").expect("parse cookie");
        assert_eq!(cookie.name(), "session");
        assert_eq!(cookie.value(), "abc123");
    }

    #[test]
    fn test_parse_full_cookie() {
        let cookie = Cookie::parse_set_cookie(
            "id=a3fWa; Domain=.example.com; Path=/; Max-Age=3600; Secure; HttpOnly; SameSite=Lax",
        )
        .expect("parse full cookie");
        assert_eq!(cookie.name(), "id");
        assert_eq!(cookie.value(), "a3fWa");
        assert_eq!(cookie.domain(), Some(".example.com"));
        assert_eq!(cookie.path(), Some("/"));
        assert_eq!(cookie.max_age(), Some(Duration::from_secs(3600)));
        assert!(cookie.is_secure());
        assert!(cookie.is_http_only());
        assert_eq!(cookie.same_site(), Some(SameSite::Lax));
    }

    #[test]
    fn test_cookie_display() {
        let cookie = Cookie::new("session", "abc")
            .set_domain(".example.com")
            .set_path("/")
            .set_secure(true)
            .set_http_only(true);
        let s = cookie.to_string();
        assert!(s.contains("session=abc"));
        assert!(s.contains("Domain=.example.com"));
        assert!(s.contains("Secure"));
        assert!(s.contains("HttpOnly"));
    }

    #[test]
    fn test_cookie_jar_operations() {
        let mut jar = CookieJar::new();
        assert!(jar.is_empty());

        jar.insert(Cookie::new("a", "1"));
        jar.insert(Cookie::new("b", "2"));
        assert_eq!(jar.len(), 2);

        assert_eq!(jar.get("a").map(|c| c.value()), Some("1"));
        jar.remove("a");
        assert_eq!(jar.len(), 1);
        assert!(jar.get("a").is_none());
    }

    #[test]
    fn test_cookie_jar_header() {
        let mut jar = CookieJar::new();
        jar.insert(Cookie::new("a", "1"));
        jar.insert(Cookie::new("b", "2"));
        let header = jar.to_cookie_header();
        // Order not guaranteed, but both must be present
        assert!(header.contains("a=1"));
        assert!(header.contains("b=2"));
    }

    #[test]
    fn test_add_from_set_cookie_headers() {
        let mut jar = CookieJar::new();
        jar.add_from_set_cookie_headers(vec!["session=abc; HttpOnly", "lang=en; Path=/"]);
        assert_eq!(jar.len(), 2);
        assert!(jar.get("session").expect("session").is_http_only());
        assert_eq!(jar.get("lang").expect("lang").path(), Some("/"));
    }

    #[test]
    fn test_empty_name_rejected() {
        let result = Cookie::parse_set_cookie("=value");
        assert!(result.is_none());
    }

    #[test]
    fn test_domain_match_exact() {
        assert!(domain_match("example.com", "example.com"));
        assert!(domain_match(".example.com", "example.com")); // leading dot stripped
        assert!(!domain_match("example.com", "other.com"));
    }

    #[test]
    fn test_domain_match_suffix() {
        assert!(domain_match("example.com", "sub.example.com"));
        assert!(domain_match(".example.com", "sub.example.com"));
        assert!(!domain_match("example.com", "notexample.com"));
    }

    #[test]
    fn test_path_match() {
        assert!(path_match("/", "/foo/bar"));
        assert!(path_match("/foo", "/foo/bar"));
        assert!(path_match("/foo/", "/foo/bar"));
        assert!(!path_match("/foo", "/foobar")); // must be boundary
        assert!(path_match("/foo", "/foo"));
    }

    #[test]
    fn test_insert_for_url_defaults() {
        use http::Uri;
        let url: Uri = "http://example.com/api/v1/items".parse().expect("uri");
        let cookie = Cookie::new("session", "abc");
        let mut jar = CookieJar::new();
        jar.insert_for_url(cookie, &url);
        assert_eq!(jar.cookies[0].domain.as_deref(), Some("example.com"));
        assert_eq!(jar.cookies[0].path.as_deref(), Some("/api/v1"));
    }

    #[test]
    fn test_cookies_for_url() {
        use http::Uri;
        let url: Uri = "http://example.com/api/items".parse().expect("uri");
        let mut jar = CookieJar::new();
        let c = Cookie::new("session", "abc");
        jar.insert_for_url(c, &url);

        let matches = jar.cookies_for_url(&url);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "session");

        // Different domain — no match
        let other_url: Uri = "http://other.com/api/items".parse().expect("uri");
        let no_matches = jar.cookies_for_url(&other_url);
        assert!(no_matches.is_empty());
    }

    #[test]
    fn test_expired_cookie_not_returned() {
        use http::Uri;
        let url: Uri = "http://example.com/".parse().expect("uri");
        let mut c = Cookie::new("old", "val");
        c.max_age = Some(std::time::Duration::from_secs(0)); // immediately expired
        let mut jar = CookieJar::new();
        jar.insert_for_url(c, &url);
        // Manually set expires_at to the past
        if let Some(cookie) = jar.cookies.first_mut() {
            cookie.expires_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        }
        assert!(jar.cookies_for_url(&url).is_empty());
    }

    #[test]
    fn test_secure_cookie_https_only() {
        use http::Uri;
        let https_url: Uri = "https://example.com/".parse().expect("uri");
        let http_url: Uri = "http://example.com/".parse().expect("uri");
        let mut c = Cookie::new("secure_token", "xyz");
        c.secure = true;
        let mut jar = CookieJar::new();
        jar.insert_for_url(c, &https_url);
        // Only returned for https
        assert_eq!(jar.cookies_for_url(&https_url).len(), 1);
        assert!(jar.cookies_for_url(&http_url).is_empty());
    }

    #[test]
    fn test_same_name_different_domain_coexist() {
        use http::Uri;
        let url_a: Uri = "http://a.example.com/".parse().expect("uri");
        let url_b: Uri = "http://b.example.com/".parse().expect("uri");
        let mut jar = CookieJar::new();
        jar.insert_for_url(Cookie::new("token", "aaa"), &url_a);
        jar.insert_for_url(Cookie::new("token", "bbb"), &url_b);
        assert_eq!(jar.cookies.len(), 2);
        assert_eq!(jar.cookies_for_url(&url_a)[0].value, "aaa");
        assert_eq!(jar.cookies_for_url(&url_b)[0].value, "bbb");
    }

    // -------------------------------------------------------------------------
    // Proptest: Cookie name/value round-trip stability
    // -------------------------------------------------------------------------

    use proptest::prelude::*;

    /// Generate strings that are safe cookie name tokens:
    /// non-empty, alphanumeric only (strict subset of RFC 7230 token chars).
    fn cookie_name_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[a-zA-Z][a-zA-Z0-9]{0,31}").expect("valid regex")
    }

    /// Generate strings that are safe cookie values:
    /// alphanumeric only, may be empty, no `;` or `=` or whitespace.
    fn cookie_value_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[a-zA-Z0-9]{0,64}").expect("valid regex")
    }

    proptest! {
        #[test]
        fn prop_cookie_round_trip(
            name in cookie_name_strategy(),
            value in cookie_value_strategy(),
        ) {
            let original = Cookie::new(name.clone(), value.clone());
            let serialized = original.to_string();
            let parsed = Cookie::parse_set_cookie(&serialized)
                .expect("round-trip parse must succeed for valid name/value");
            prop_assert_eq!(&parsed.name, &name);
            prop_assert_eq!(&parsed.value, &value);
        }
    }

    // Known special-case round trips (no whitespace, no control chars)
    #[test]
    fn test_cookie_round_trip_known_cases() {
        let cases = [
            ("session", "abc123"),
            ("LANG", "en"),
            ("x", ""),
            ("Token", "abcdefghijklmnopqrstuvwxyz"),
            ("A1B2", "Z9Y8X7"),
        ];
        for (name, value) in cases {
            let original = Cookie::new(name, value);
            let serialized = original.to_string();
            let parsed = Cookie::parse_set_cookie(&serialized)
                .unwrap_or_else(|| panic!("failed to parse round-trip for {name}={value}"));
            assert_eq!(parsed.name, name, "name mismatch for {name}");
            assert_eq!(parsed.value, value, "value mismatch for {name}={value}");
        }
    }
}
