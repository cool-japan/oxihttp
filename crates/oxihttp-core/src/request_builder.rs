//! Execution-free HTTP request builder for constructing `http::Request<Body>` values.
//!
//! Unlike the client's request builder, this builder has no network I/O — the terminal method
//! is `.build()` which returns an `OxiRequest<Body>`. Useful for tests, server-side request
//! forging, and as a reusable base for building requests without a live connection.

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Uri};

use crate::body::Body;
use crate::error::OxiHttpError;
use crate::OxiRequest;

/// Execution-free HTTP request builder.
///
/// Constructs an `http::Request<Body>` without performing any network I/O.
/// The terminal method is [`RequestBuilder::build`] which returns an
/// `OxiRequest<Body>`. This is distinct from `oxihttp_client::RequestBuilder`
/// which owns a network connection and executes requests.
///
/// # Example
///
/// ```rust
/// use oxihttp_core::CoreRequestBuilder;
///
/// let req = CoreRequestBuilder::get("http://example.com/api")
///     .expect("valid URI")
///     .header("x-api-key", "secret")
///     .expect("valid header")
///     .build()
///     .expect("valid request");
///
/// assert_eq!(req.method(), http::Method::GET);
/// ```
#[derive(Debug)]
pub struct RequestBuilder {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
}

impl RequestBuilder {
    /// Create a new builder with the given method and URI.
    pub fn new(method: Method, uri: Uri) -> Self {
        Self {
            method,
            uri,
            headers: HeaderMap::new(),
            body: Body::empty(),
        }
    }

    /// Create a GET request builder.
    pub fn get(uri: impl AsRef<str>) -> Result<Self, OxiHttpError> {
        let uri = Uri::try_from(uri.as_ref())?;
        Ok(Self::new(Method::GET, uri))
    }

    /// Create a POST request builder.
    pub fn post(uri: impl AsRef<str>) -> Result<Self, OxiHttpError> {
        let uri = Uri::try_from(uri.as_ref())?;
        Ok(Self::new(Method::POST, uri))
    }

    /// Create a PUT request builder.
    pub fn put(uri: impl AsRef<str>) -> Result<Self, OxiHttpError> {
        let uri = Uri::try_from(uri.as_ref())?;
        Ok(Self::new(Method::PUT, uri))
    }

    /// Create a DELETE request builder.
    pub fn delete(uri: impl AsRef<str>) -> Result<Self, OxiHttpError> {
        let uri = Uri::try_from(uri.as_ref())?;
        Ok(Self::new(Method::DELETE, uri))
    }

    /// Create a PATCH request builder.
    pub fn patch(uri: impl AsRef<str>) -> Result<Self, OxiHttpError> {
        let uri = Uri::try_from(uri.as_ref())?;
        Ok(Self::new(Method::PATCH, uri))
    }

    /// Create a HEAD request builder.
    pub fn head(uri: impl AsRef<str>) -> Result<Self, OxiHttpError> {
        let uri = Uri::try_from(uri.as_ref())?;
        Ok(Self::new(Method::HEAD, uri))
    }

    /// Add a single header, replacing any existing header with the same name.
    pub fn header(
        mut self,
        name: impl AsRef<str>,
        value: impl AsRef<str>,
    ) -> Result<Self, OxiHttpError> {
        let name = HeaderName::try_from(name.as_ref())
            .map_err(|e| OxiHttpError::InvalidHeader(e.to_string()))?;
        let value = HeaderValue::try_from(value.as_ref())
            .map_err(|e| OxiHttpError::InvalidHeader(e.to_string()))?;
        self.headers.insert(name, value);
        Ok(self)
    }

    /// Merge a `HeaderMap` into the request headers, replacing duplicates.
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        for (k, v) in &headers {
            self.headers.insert(k.clone(), v.clone());
        }
        self
    }

    /// Set a raw body (no `Content-Type` set automatically).
    pub fn body(mut self, body: impl Into<Body>) -> Self {
        self.body = body.into();
        self
    }

    /// Set a JSON body, serialising `value` and setting `Content-Type: application/json`.
    pub fn json<T: serde::Serialize>(mut self, value: &T) -> Result<Self, OxiHttpError> {
        let bytes = serde_json::to_vec(value).map_err(|e| OxiHttpError::Json(e.to_string()))?;
        self.headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        self.body = Body::full(Bytes::from(bytes));
        Ok(self)
    }

    /// Set a URL-encoded form body, setting
    /// `Content-Type: application/x-www-form-urlencoded`.
    ///
    /// Takes a [`crate::FormBody`] whose `build()` is infallible, so this
    /// method returns `Self` directly.
    pub fn form(mut self, body: crate::FormBody) -> Self {
        self.headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        self.body = Body::full(body.build());
        self
    }

    /// Build the request.
    ///
    /// Returns `Err` only when the accumulated headers, method, or URI are
    /// invalid at the `http::Request` layer (which should be rare given that
    /// the builder already validated each piece).
    pub fn build(self) -> Result<OxiRequest<Body>, OxiHttpError> {
        let mut builder = http::Request::builder().method(self.method).uri(self.uri);
        for (k, v) in &self.headers {
            builder = builder.header(k, v);
        }
        builder
            .body(self.body)
            .map_err(|e| OxiHttpError::Http(std::sync::Arc::new(e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FormBody;

    #[test]
    fn test_get_builder() {
        let req = RequestBuilder::get("http://example.com/path")
            .expect("valid URI")
            .build()
            .expect("build succeeds");
        assert_eq!(req.method(), &Method::GET);
        assert_eq!(req.uri().to_string(), "http://example.com/path");
    }

    #[test]
    fn test_post_with_json() {
        #[derive(serde::Serialize)]
        struct Payload {
            key: &'static str,
        }

        let req = RequestBuilder::post("http://example.com/api")
            .expect("valid URI")
            .json(&Payload { key: "value" })
            .expect("serialises")
            .build()
            .expect("build succeeds");

        assert_eq!(req.method(), &Method::POST);
        assert_eq!(
            req.headers()
                .get(http::header::CONTENT_TYPE)
                .map(|v| v.as_bytes()),
            Some(b"application/json".as_ref()),
        );
    }

    #[test]
    fn test_headers_merged() {
        let mut extra = HeaderMap::new();
        extra.insert(
            HeaderName::from_static("x-custom"),
            HeaderValue::from_static("abc"),
        );

        let req = RequestBuilder::get("http://example.com")
            .expect("valid URI")
            .headers(extra)
            .build()
            .expect("build succeeds");

        assert_eq!(
            req.headers().get("x-custom").map(|v| v.as_bytes()),
            Some(b"abc".as_ref()),
        );
    }

    #[test]
    fn test_form_sets_content_type() {
        let form = FormBody::new().field("foo", "bar");
        let req = RequestBuilder::post("http://example.com/form")
            .expect("valid URI")
            .form(form)
            .build()
            .expect("build succeeds");

        assert_eq!(
            req.headers()
                .get(http::header::CONTENT_TYPE)
                .map(|v| v.as_bytes()),
            Some(b"application/x-www-form-urlencoded".as_ref()),
        );
    }

    #[test]
    fn test_invalid_uri_returns_error() {
        let result = RequestBuilder::get("not a valid uri!!!!");
        assert!(result.is_err());
    }

    #[test]
    fn test_all_methods() {
        let cases: &[(&str, Method)] = &[
            ("http://a.com", Method::POST),
            ("http://a.com", Method::PUT),
            ("http://a.com", Method::DELETE),
            ("http://a.com", Method::PATCH),
            ("http://a.com", Method::HEAD),
        ];
        for (uri, method) in cases {
            let req = match method.as_str() {
                "POST" => RequestBuilder::post(uri),
                "PUT" => RequestBuilder::put(uri),
                "DELETE" => RequestBuilder::delete(uri),
                "PATCH" => RequestBuilder::patch(uri),
                "HEAD" => RequestBuilder::head(uri),
                _ => unreachable!(),
            }
            .expect("valid uri")
            .build()
            .expect("build succeeds");
            assert_eq!(req.method(), method);
        }
    }
}
