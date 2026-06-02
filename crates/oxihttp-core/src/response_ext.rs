//! Extension trait providing ergonomic body-consuming methods on HTTP responses.
//!
//! Mirrors the reqwest `Response` API for any `http::Response<B>` where
//! `B: http_body::Body`. All methods are async and consume the response.

use bytes::{Buf, Bytes};
use http::Response;
use http_body_util::BodyExt as _;

use crate::error::OxiHttpError;

/// Extension methods for `http::Response<B>` where `B` implements
/// [`http_body::Body`].
///
/// Provided for any response type where the body can be buffered
/// asynchronously. The trait methods consume `self` (the response).
///
/// # Example
///
/// ```rust,ignore
/// use oxihttp_core::ResponseExt;
///
/// let text = response.body_text().await?;
/// let value: MyStruct = response.body_json().await?;
/// ```
#[allow(async_fn_in_trait)]
pub trait ResponseExt: Sized {
    /// Consume the response and collect all body bytes into a single [`Bytes`].
    async fn body_bytes(self) -> Result<Bytes, OxiHttpError>;

    /// Consume the response and decode the body as a UTF-8 string.
    async fn body_text(self) -> Result<String, OxiHttpError>;

    /// Consume the response and deserialize the body as JSON.
    async fn body_json<T: serde::de::DeserializeOwned>(self) -> Result<T, OxiHttpError>;
}

impl<B> ResponseExt for Response<B>
where
    B: http_body::Body + Send,
    B::Data: Buf,
    B::Error: std::fmt::Display,
{
    async fn body_bytes(self) -> Result<Bytes, OxiHttpError> {
        let body = self.into_body();
        let collected = body
            .collect()
            .await
            .map_err(|e| OxiHttpError::Body(e.to_string()))?;
        Ok(collected.to_bytes())
    }

    async fn body_text(self) -> Result<String, OxiHttpError> {
        let bytes = self.body_bytes().await?;
        String::from_utf8(bytes.to_vec())
            .map_err(|e| OxiHttpError::Body(format!("invalid UTF-8: {e}")))
    }

    async fn body_json<T: serde::de::DeserializeOwned>(self) -> Result<T, OxiHttpError> {
        let bytes = self.body_bytes().await?;
        serde_json::from_slice(&bytes).map_err(|e| OxiHttpError::Json(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http::Response;
    use http_body_util::Full;

    #[tokio::test]
    async fn test_body_bytes() {
        let resp: Response<Full<Bytes>> = Response::new(Full::new(Bytes::from("hello")));
        let bytes = resp.body_bytes().await.expect("collect succeeds");
        assert_eq!(bytes.as_ref(), b"hello");
    }

    #[tokio::test]
    async fn test_body_text() {
        let resp: Response<Full<Bytes>> = Response::new(Full::new(Bytes::from("hello text")));
        let text = resp.body_text().await.expect("decode succeeds");
        assert_eq!(text, "hello text");
    }

    #[tokio::test]
    async fn test_body_json() {
        #[derive(serde::Deserialize, PartialEq, Debug)]
        struct Msg {
            value: u32,
        }

        let json = br#"{"value":42}"#;
        let resp: Response<Full<Bytes>> = Response::new(Full::new(Bytes::from(json.as_ref())));
        let msg: Msg = resp.body_json().await.expect("deserialise succeeds");
        assert_eq!(msg, Msg { value: 42 });
    }

    #[tokio::test]
    async fn test_body_text_invalid_utf8() {
        let resp: Response<Full<Bytes>> = Response::new(Full::new(Bytes::from(vec![0xFF, 0xFE])));
        let result = resp.body_text().await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("invalid UTF-8"), "got: {err_msg}");
    }

    #[tokio::test]
    async fn test_body_json_invalid() {
        let resp: Response<Full<Bytes>> = Response::new(Full::new(Bytes::from("not json")));
        let result = resp.body_json::<serde_json::Value>().await;
        assert!(result.is_err());
    }
}
