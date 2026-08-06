//! Response helpers for the OxiHTTP server.

use bytes::Bytes;
use http::StatusCode;
use http_body_util::Full;

use oxihttp_core::OxiHttpError;

/// Build a JSON response from a serializable value.
///
/// Sets the `Content-Type` header to `application/json`.
///
/// # Example
///
/// ```rust
/// use oxihttp_server::response::json_response;
/// use serde::Serialize;
///
/// #[derive(Serialize)]
/// struct Health { status: &'static str }
///
/// let resp = json_response(&Health { status: "ok" }).expect("valid json");
/// assert_eq!(resp.status(), http::StatusCode::OK);
/// assert_eq!(
///     resp.headers().get(http::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()),
///     Some("application/json")
/// );
/// ```
pub fn json_response<T: serde::Serialize>(
    value: &T,
) -> Result<hyper::Response<Full<Bytes>>, OxiHttpError> {
    let body = serde_json::to_vec(value).map_err(|e| OxiHttpError::Json(e.to_string()))?;
    hyper::Response::builder()
        .status(StatusCode::OK)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .map_err(|e| OxiHttpError::Http(std::sync::Arc::new(e)))
}

/// Build a JSON response with a custom status code.
pub fn json_response_with_status<T: serde::Serialize>(
    status: StatusCode,
    value: &T,
) -> Result<hyper::Response<Full<Bytes>>, OxiHttpError> {
    let body = serde_json::to_vec(value).map_err(|e| OxiHttpError::Json(e.to_string()))?;
    hyper::Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .map_err(|e| OxiHttpError::Http(std::sync::Arc::new(e)))
}

/// Build a plain text response.
pub fn text_response(
    body: impl Into<String>,
) -> Result<hyper::Response<Full<Bytes>>, OxiHttpError> {
    hyper::Response::builder()
        .status(StatusCode::OK)
        .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from(body.into())))
        .map_err(|e| OxiHttpError::Http(std::sync::Arc::new(e)))
}

/// Build an HTML response.
pub fn html_response(
    body: impl Into<String>,
) -> Result<hyper::Response<Full<Bytes>>, OxiHttpError> {
    hyper::Response::builder()
        .status(StatusCode::OK)
        .header(http::header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Full::new(Bytes::from(body.into())))
        .map_err(|e| OxiHttpError::Http(std::sync::Arc::new(e)))
}

/// Build a redirect response (302 Found).
pub fn redirect_response(location: &str) -> Result<hyper::Response<Full<Bytes>>, OxiHttpError> {
    hyper::Response::builder()
        .status(StatusCode::FOUND)
        .header(http::header::LOCATION, location)
        .body(Full::new(Bytes::new()))
        .map_err(|e| OxiHttpError::Http(std::sync::Arc::new(e)))
}

/// Build an empty response with a status code.
pub fn empty_response(status: StatusCode) -> Result<hyper::Response<Full<Bytes>>, OxiHttpError> {
    hyper::Response::builder()
        .status(status)
        .body(Full::new(Bytes::new()))
        .map_err(|e| OxiHttpError::Http(std::sync::Arc::new(e)))
}

/// Build a 204 No Content response.
pub fn no_content() -> Result<hyper::Response<Full<Bytes>>, OxiHttpError> {
    empty_response(StatusCode::NO_CONTENT)
}

/// Build a 400 Bad Request response with a message.
pub fn bad_request(msg: impl Into<String>) -> Result<hyper::Response<Full<Bytes>>, OxiHttpError> {
    hyper::Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from(msg.into())))
        .map_err(|e| OxiHttpError::Http(std::sync::Arc::new(e)))
}

/// Build a 500 Internal Server Error response with a message.
pub fn internal_error(
    msg: impl Into<String>,
) -> Result<hyper::Response<Full<Bytes>>, OxiHttpError> {
    hyper::Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from(msg.into())))
        .map_err(|e| OxiHttpError::Http(std::sync::Arc::new(e)))
}

/// Build an `application/x-www-form-urlencoded` response from a [`oxihttp_core::FormBody`].
///
/// The form body is serialized to bytes and returned with status 200 OK and
/// `Content-Type: application/x-www-form-urlencoded`.
pub fn form_response(
    body: oxihttp_core::FormBody,
) -> Result<hyper::Response<Full<Bytes>>, OxiHttpError> {
    let bytes = body.build();
    hyper::Response::builder()
        .status(StatusCode::OK)
        .header(
            http::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(Full::new(bytes))
        .map_err(|e| OxiHttpError::Http(std::sync::Arc::new(e)))
}
