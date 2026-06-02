//! Redirect handling policies for the HTTP client.

/// Policy controlling how the client handles HTTP redirects.
#[derive(Debug, Clone)]
pub enum RedirectPolicy {
    /// Never follow redirects. The redirect response is returned as-is.
    None,
    /// Follow up to `n` redirects before returning an error.
    Limited(usize),
    /// Follow all redirects (use with caution).
    All,
}

impl RedirectPolicy {
    /// Returns the maximum number of redirects allowed, or `None` for unlimited.
    pub fn max_redirects(&self) -> Option<usize> {
        match self {
            Self::None => Some(0),
            Self::Limited(n) => Some(*n),
            Self::All => None,
        }
    }

    /// Returns `true` if redirects are disabled.
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

impl Default for RedirectPolicy {
    /// Default: follow up to 10 redirects.
    fn default() -> Self {
        Self::Limited(10)
    }
}

/// Returns `true` if the given status code is a redirect that should be followed.
pub fn is_redirect_status(status: http::StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

/// Determine the method to use after a redirect.
///
/// For 301/302/303, POST is changed to GET (as per browser behavior).
/// For 307/308, the method is preserved.
pub fn redirect_method(status: http::StatusCode, original: &http::Method) -> http::Method {
    match status.as_u16() {
        301..=303 => {
            if *original == http::Method::HEAD {
                http::Method::HEAD
            } else {
                http::Method::GET
            }
        }
        // 307 and 308 preserve the method
        _ => original.clone(),
    }
}

/// Determine whether to preserve the body after a redirect.
///
/// For 307/308, the body should be resent. For 301/302/303, the body is dropped.
pub fn should_preserve_body(status: http::StatusCode) -> bool {
    matches!(status.as_u16(), 307 | 308)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_policy() {
        let policy = RedirectPolicy::default();
        assert_eq!(policy.max_redirects(), Some(10));
    }

    #[test]
    fn test_none_policy() {
        let policy = RedirectPolicy::None;
        assert!(policy.is_none());
        assert_eq!(policy.max_redirects(), Some(0));
    }

    #[test]
    fn test_is_redirect_status() {
        assert!(is_redirect_status(http::StatusCode::MOVED_PERMANENTLY));
        assert!(is_redirect_status(http::StatusCode::FOUND));
        assert!(is_redirect_status(http::StatusCode::TEMPORARY_REDIRECT));
        assert!(is_redirect_status(http::StatusCode::PERMANENT_REDIRECT));
        assert!(!is_redirect_status(http::StatusCode::OK));
        assert!(!is_redirect_status(http::StatusCode::NOT_FOUND));
    }

    #[test]
    fn test_redirect_method_303() {
        let method = redirect_method(http::StatusCode::SEE_OTHER, &http::Method::POST);
        assert_eq!(method, http::Method::GET);
    }

    #[test]
    fn test_redirect_method_307_preserves() {
        let method = redirect_method(http::StatusCode::TEMPORARY_REDIRECT, &http::Method::POST);
        assert_eq!(method, http::Method::POST);
    }

    #[test]
    fn test_redirect_method_head_preserved() {
        let method = redirect_method(http::StatusCode::FOUND, &http::Method::HEAD);
        assert_eq!(method, http::Method::HEAD);
    }

    #[test]
    fn test_should_preserve_body() {
        assert!(!should_preserve_body(http::StatusCode::MOVED_PERMANENTLY));
        assert!(!should_preserve_body(http::StatusCode::FOUND));
        assert!(should_preserve_body(http::StatusCode::TEMPORARY_REDIRECT));
        assert!(should_preserve_body(http::StatusCode::PERMANENT_REDIRECT));
    }
}
