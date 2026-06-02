//! Request parts extraction framework for OxiHTTP server handlers.
//!
//! Provides the [`FromRequestParts`] trait and [`TypedHeader`] extractor for
//! typed access to HTTP request components without consuming the request body.

use std::collections::HashMap;

use http::{HeaderMap, Method, Uri};
use oxihttp_core::{Header, OxiHttpError};

/// A view into the non-body parts of a server [`Request`][crate::Request].
///
/// Passed to [`FromRequestParts::from_request_parts`] implementations.
pub struct RequestParts<'a> {
    /// The HTTP method of the request.
    pub method: &'a Method,
    /// The request URI.
    pub uri: &'a Uri,
    /// The request headers.
    pub headers: &'a HeaderMap,
    /// Extracted path parameters (e.g. `:id` in `/users/:id`).
    pub path_params: &'a HashMap<String, String>,
}

/// Trait for types that can be extracted from [`RequestParts`].
///
/// Implement this trait to create custom extractors that can be used with
/// [`Request::extract`][crate::Request::extract].
pub trait FromRequestParts: Sized {
    /// The error type returned when extraction fails.
    type Rejection: Into<OxiHttpError>;

    /// Attempt to extract `Self` from the request parts.
    fn from_request_parts(parts: &RequestParts<'_>) -> Result<Self, Self::Rejection>;
}

/// Extractor for a single typed HTTP header.
///
/// Extracts and decodes the header `H` from the request, returning an error
/// if the header is missing or its value is invalid.
///
/// # Example
///
/// ```rust,no_run
/// use oxihttp_server::extractor::TypedHeader;
/// use oxihttp_core::ContentType;
///
/// // In a handler:
/// // let TypedHeader(ct) = req.extract::<TypedHeader<ContentType>>()?;
/// ```
pub struct TypedHeader<H: Header>(pub H);

impl<H: Header> FromRequestParts for TypedHeader<H> {
    type Rejection = OxiHttpError;

    fn from_request_parts(parts: &RequestParts<'_>) -> Result<Self, Self::Rejection> {
        H::decode(parts.headers).map(TypedHeader)
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;
    use oxihttp_core::{Authorization, ContentType, Host};

    fn make_parts<'a>(
        method: &'a Method,
        uri: &'a Uri,
        headers: &'a HeaderMap,
        path_params: &'a HashMap<String, String>,
    ) -> RequestParts<'a> {
        RequestParts {
            method,
            uri,
            headers,
            path_params,
        }
    }

    #[test]
    fn test_typed_header_extraction_via_request_parts() {
        let method = Method::POST;
        let uri: Uri = "/upload".parse().expect("parse uri");
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        let path_params = HashMap::new();
        let parts = make_parts(&method, &uri, &headers, &path_params);

        let TypedHeader(ct) =
            TypedHeader::<ContentType>::from_request_parts(&parts).expect("should extract");
        assert_eq!(ct, ContentType::Json);
    }

    #[test]
    fn test_typed_header_missing_returns_error() {
        let method = Method::POST;
        let uri: Uri = "/upload".parse().expect("parse uri");
        let headers = HeaderMap::new();
        let path_params = HashMap::new();
        let parts = make_parts(&method, &uri, &headers, &path_params);

        let result = TypedHeader::<ContentType>::from_request_parts(&parts);
        assert!(result.is_err(), "should fail when Content-Type is absent");
    }

    #[test]
    fn test_typed_header_host_ok() {
        let method = Method::GET;
        let uri: Uri = "/".parse().expect("parse uri");
        let mut headers = HeaderMap::new();
        headers.insert(http::header::HOST, HeaderValue::from_static("example.com"));
        let path_params = HashMap::new();
        let parts = make_parts(&method, &uri, &headers, &path_params);

        let TypedHeader(host) =
            TypedHeader::<Host>::from_request_parts(&parts).expect("should extract");
        assert_eq!(host, Host("example.com".to_string()));
    }

    #[test]
    fn test_typed_header_authorization_ok() {
        let method = Method::GET;
        let uri: Uri = "/secure".parse().expect("parse uri");
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret-token"),
        );
        let path_params = HashMap::new();
        let parts = make_parts(&method, &uri, &headers, &path_params);

        let TypedHeader(auth) =
            TypedHeader::<Authorization>::from_request_parts(&parts).expect("should extract");
        assert_eq!(auth, Authorization("Bearer secret-token".to_string()));
    }
}
