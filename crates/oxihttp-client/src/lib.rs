//! OxiHTTP Client - Pure-Rust HTTP client for the OxiHTTP stack.
//!
//! Provides a high-level HTTP client with connection pooling, redirect handling,
//! retry logic, timeouts, and a fluent request builder API.
//!
//! # Example
//!
//! ```rust,no_run
//! # async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
//! use oxihttp_client::Client;
//!
//! let client = Client::builder().build()?;
//! let resp = client.get("http://example.com")?.send().await?;
//! assert_eq!(resp.status(), http::StatusCode::OK);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod client_builder;
pub mod middleware;
pub mod proxy;
pub mod redirect;
pub mod resolver;
pub mod retry;

#[cfg(feature = "tls")]
pub mod connector;
#[cfg(feature = "tls")]
pub mod request_config;
#[cfg(feature = "tls")]
pub mod tls;

#[cfg(feature = "h3")]
pub mod h3;

#[cfg(feature = "tls")]
pub use connector::{MaybeHttpsStream, OxiHttpsConnector};
#[cfg(feature = "tls")]
pub use request_config::RequestTlsConfig;
#[cfg(feature = "tls")]
pub use tls::DangerousNoVerification;

#[cfg(feature = "socks")]
pub use proxy::Socks5Connector;
pub use proxy::{ProxyConnector, ProxyKind};

use bytes::Bytes;
use futures_core::Stream;
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::client::legacy::connect::{Connect, HttpConnector};
use hyper_util::client::legacy::Client as HyperClient;
#[cfg(feature = "tls")]
use hyper_util::rt::TokioExecutor;
use resolver::BoxResolver;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

#[cfg(feature = "tls")]
pub(crate) use client_builder::apply_http2_settings;
pub use client_builder::{ClientBuilder, Http2Settings};
pub use middleware::{ClientMiddleware, LoggingMiddleware, TimingMiddleware};
use oxihttp_core::{Body, OxiHttpError, PinnedBody};
pub use redirect::RedirectPolicy;
pub use retry::RetryPolicy;

/// Default cap, in bytes, on the response body a [`Client`] will buffer.
///
/// Applies both to the raw (wire) bytes collected by `body_bytes()` /
/// `body_text()` / `body_json()` and — when
/// [`ClientBuilder::with_decompression`] is enabled — to the *decompressed*
/// output, so a malicious or compromised server cannot force the client to
/// allocate an unbounded amount of memory, whether by sending an oversized
/// response directly or by sending a small, highly-compressible payload
/// (a "decompression bomb"). Override with
/// [`ClientBuilder::with_max_response_body`].
pub const DEFAULT_MAX_RESPONSE_BODY: usize = 64 * 1024 * 1024;

// ---------------------------------------------------------------------------
// BodyStream — streaming response body
// ---------------------------------------------------------------------------

/// An async stream of response body chunks produced by `Response::body_stream()`.
pub struct BodyStream {
    inner: http_body_util::BodyStream<Incoming>,
}

impl Stream for BodyStream {
    type Item = Result<Bytes, OxiHttpError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        return Poll::Ready(Some(Ok(data)));
                    }
                    // Trailers or other non-data frames — skip and poll again
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(OxiHttpError::Body(e.to_string()))));
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------

/// HTTP response wrapper providing convenience methods for body consumption.
pub struct Response {
    inner: http::Response<Incoming>,
    /// Whether to auto-decompress the response body using Content-Encoding.
    decompress: bool,
    /// Maximum number of bytes `body_bytes()` will buffer, applied to both
    /// the raw wire body and (if `decompress` is set) the decompressed
    /// output. See [`DEFAULT_MAX_RESPONSE_BODY`].
    max_body_bytes: usize,
}

impl Response {
    /// HTTP status code.
    pub fn status(&self) -> StatusCode {
        self.inner.status()
    }

    /// Response headers.
    pub fn headers(&self) -> &HeaderMap {
        self.inner.headers()
    }

    /// Return the first value of the named response header as a UTF-8 string,
    /// or `None` if the header is absent or its value is not valid UTF-8.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
    /// use oxihttp_client::Client;
    ///
    /// let client = Client::builder().build()?;
    /// let resp = client.get("http://example.com/new-resource")?.send().await?;
    /// if let Some(location) = resp.header("location") {
    ///     println!("redirected to: {location}");
    /// }
    /// if let Some(nonce) = resp.header("replay-nonce") {
    ///     println!("ACME nonce: {nonce}");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn header(&self, name: &str) -> Option<&str> {
        self.inner.headers().get(name).and_then(|v| v.to_str().ok())
    }

    /// HTTP version used for this response.
    pub fn version(&self) -> http::Version {
        self.inner.version()
    }

    /// Content-Length header as u64 if present and valid.
    pub fn content_length(&self) -> Option<u64> {
        self.inner
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
    }

    /// Consume the body and return raw bytes, auto-decompressing if enabled.
    ///
    /// The wire body is never buffered past `max_body_bytes` (see
    /// [`ClientBuilder::with_max_response_body`]): a server that sends (or
    /// lies about, via chunked `Transfer-Encoding`) a larger body gets a
    /// typed [`OxiHttpError::Body`] instead of unbounded client-side
    /// allocation. When decompression is enabled, the *decompressed*
    /// output is bounded by the same cap.
    ///
    /// # Errors when the `decompression` feature is not compiled in
    ///
    /// [`ClientBuilder::with_decompression`] remains usable with default
    /// features (so callers don't have to feature-gate their own code), but
    /// without the `decompression` feature this crate has no gzip/deflate
    /// decoder to call. Rather than silently handing back the still-encoded
    /// wire bytes as if they were plaintext — which previously surfaced
    /// downstream as a confusing UTF-8 or JSON parse failure, or worse, raw
    /// compressed bytes written to disk — a response whose
    /// `Content-Encoding` is `gzip` or `deflate` is rejected here with a
    /// typed [`OxiHttpError::Body`]. A `Content-Encoding` of `identity`, or
    /// no `Content-Encoding` at all, is unaffected: there is nothing to
    /// decode either way.
    pub async fn body_bytes(self) -> Result<Bytes, OxiHttpError> {
        let decompress = self.decompress;
        let cap = self.max_body_bytes;
        let ce = self
            .inner
            .headers()
            .get(http::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_ascii_lowercase());

        let raw = collect_body_limited(self.inner.into_body(), cap).await?;

        if decompress {
            match ce.as_deref() {
                Some("gzip") => {
                    #[cfg(feature = "decompression")]
                    {
                        return bounded_gzip_decompress(&raw, cap);
                    }
                    #[cfg(not(feature = "decompression"))]
                    {
                        return Err(decompression_feature_missing_error("gzip"));
                    }
                }
                Some("deflate") => {
                    #[cfg(feature = "decompression")]
                    {
                        return bounded_deflate_decompress(&raw, cap);
                    }
                    #[cfg(not(feature = "decompression"))]
                    {
                        return Err(decompression_feature_missing_error("deflate"));
                    }
                }
                // `identity` (RFC 9110 §8.4.1) and the absent-header case both
                // mean "not encoded" — nothing to decode, raw bytes are correct.
                _ => {}
            }
        }

        Ok(raw)
    }

    /// Consume the body and return it as a UTF-8 string.
    pub async fn body_text(self) -> Result<String, OxiHttpError> {
        let bytes = self.body_bytes().await?;
        String::from_utf8(bytes.to_vec())
            .map_err(|e| OxiHttpError::Body(format!("invalid UTF-8: {e}")))
    }

    /// Consume the body and deserialize it as JSON.
    pub async fn body_json<T: serde::de::DeserializeOwned>(self) -> Result<T, OxiHttpError> {
        let bytes = self.body_bytes().await?;
        serde_json::from_slice(&bytes).map_err(|e| OxiHttpError::Json(e.to_string()))
    }

    /// Return an error if the response status is a client (4xx) or server (5xx) error.
    ///
    /// Returns `Ok(self)` for success and redirect status codes.
    pub fn error_for_status(self) -> Result<Self, OxiHttpError> {
        let status = self.inner.status();
        if status.is_client_error() || status.is_server_error() {
            Err(OxiHttpError::Body(format!(
                "HTTP error: {} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("Unknown")
            )))
        } else {
            Ok(self)
        }
    }

    /// Returns the `Content-Type` header value as a string, if present.
    pub fn content_type(&self) -> Option<&str> {
        self.inner
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
    }

    /// Parse all `Set-Cookie` response headers using `oxihttp_core::Cookie::parse_set_cookie`.
    ///
    /// Returns an empty `Vec` when there are no `Set-Cookie` headers or none parse
    /// successfully.
    pub fn cookies(&self) -> Vec<oxihttp_core::Cookie> {
        self.inner
            .headers()
            .get_all(http::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(oxihttp_core::Cookie::parse_set_cookie)
            .collect()
    }

    /// Consume the response and return the body as an async stream of chunks.
    ///
    /// Unlike [`body_bytes`](Self::body_bytes), this is **not** bounded by
    /// `max_response_body` — streaming is itself the opt-out for large
    /// bodies, since the caller controls how much of the stream to consume
    /// and never has the whole body buffered in memory at once. No
    /// decompression is applied here either; `Content-Encoding` handling is
    /// only wired into `body_bytes()` / `body_text()` / `body_json()`.
    pub fn body_stream(self) -> BodyStream {
        BodyStream {
            inner: http_body_util::BodyStream::new(self.inner.into_body()),
        }
    }
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.inner.status())
            .field("version", &self.inner.version())
            .field("headers", self.inner.headers())
            .finish()
    }
}

/// Build the typed error returned by [`Response::body_bytes`] when a
/// response arrives with `Content-Encoding: {encoding}` and
/// [`ClientBuilder::with_decompression`] is enabled, but this build has no
/// decoder for it because the `decompression` Cargo feature is not
/// compiled in. Returning raw compressed bytes silently here would be a
/// worse failure mode than a clear, typed error.
///
/// [`ClientBuilder::with_decompression`]: crate::ClientBuilder::with_decompression
#[cfg(not(feature = "decompression"))]
fn decompression_feature_missing_error(encoding: &str) -> OxiHttpError {
    OxiHttpError::Body(format!(
        "response Content-Encoding is '{encoding}' but this build of oxihttp-client does not \
         have the 'decompression' feature enabled, so it cannot decode the body; rebuild with \
         the 'decompression' feature, or call ClientBuilder::with_decompression(false) so \
         Accept-Encoding is not advertised to the server"
    ))
}

/// Collect a response body into `Bytes`, rejecting it as soon as the
/// accumulated size would exceed `max` bytes.
///
/// Mirrors the server-side body-limit enforcement
/// (`oxihttp_server::router`'s internal `collect_body_limited`): the body
/// is read frame-by-frame and the running total checked *before* each
/// chunk is appended, so a server that lies about (or omits, via chunked
/// `Transfer-Encoding`) its `Content-Length` cannot force the client to
/// buffer an unbounded amount of memory before the limit has a chance to
/// bite.
async fn collect_body_limited(mut body: Incoming, max: usize) -> Result<Bytes, OxiHttpError> {
    use bytes::Buf;

    let mut collected: Vec<u8> = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| OxiHttpError::Body(e.to_string()))?;
        if let Ok(mut data) = frame.into_data() {
            let incoming = data.remaining();
            if collected.len().saturating_add(incoming) > max {
                return Err(OxiHttpError::Body(format!(
                    "response body too large: exceeds limit of {max} bytes"
                )));
            }
            while data.has_remaining() {
                let chunk = data.chunk();
                collected.extend_from_slice(chunk);
                let consumed = chunk.len();
                data.advance(consumed);
            }
        }
    }
    Ok(Bytes::from(collected))
}

/// Run a bounded DEFLATE-family decoder against `src`, writing into a
/// buffer sized `min(cap, max(src.len() * 8, 8192))` on the first attempt
/// and retrying once at the full `cap` if that undershoots.
///
/// This keeps the common case (a small response with a typical compression
/// ratio) from unconditionally paying for a `cap`-sized allocation, while
/// still guaranteeing no attempt ever allocates more than `cap` bytes.
/// `decode` receives `(src, dst)` and must behave like
/// `oxiarc_deflate::inflate_into`: write only into `dst`, never allocate
/// internally, and return the number of bytes written or a typed error
/// (including when `dst` is too small).
#[cfg(feature = "decompression")]
fn bounded_inflate<F>(src: &[u8], cap: usize, decode: F) -> Result<Vec<u8>, OxiHttpError>
where
    F: Fn(&[u8], &mut [u8]) -> Result<usize, OxiHttpError>,
{
    let first_try = src.len().saturating_mul(8).max(8192).min(cap);
    let mut buf = vec![0u8; first_try];
    match decode(src, &mut buf) {
        Ok(written) => {
            buf.truncate(written);
            Ok(buf)
        }
        Err(first_err) => {
            if first_try >= cap {
                return Err(first_err);
            }
            let mut buf = vec![0u8; cap];
            let written = decode(src, &mut buf)?;
            buf.truncate(written);
            Ok(buf)
        }
    }
}

/// Decompress a zlib-wrapped or raw-DEFLATE response body, bounded to `cap`
/// bytes of output.
///
/// Unlike the one-shot `oxiarc_deflate::zlib_decompress` /
/// `oxiarc_deflate::inflate` (which decode into an unbounded, internally
/// growing `Vec`), this uses `oxiarc_deflate::zlib_decompress_into` /
/// `inflate_into`: both write directly into the caller-supplied buffer and
/// allocate nothing internally, so decompression is genuinely bounded
/// *during* decoding — a hostile stream cannot force a transient
/// allocation larger than `cap`, even mid-decode. The zlib-then-raw
/// fallback mirrors the pre-existing (unbounded) behavior: some servers
/// send raw DEFLATE without the 2-byte zlib wrapper.
#[cfg(feature = "decompression")]
fn bounded_deflate_decompress(raw: &[u8], cap: usize) -> Result<Bytes, OxiHttpError> {
    let decoded = bounded_inflate(raw, cap, |src, dst| {
        oxiarc_deflate::zlib_decompress_into(src, dst)
            .or_else(|_| oxiarc_deflate::inflate_into(src, dst))
            .map_err(|e| OxiHttpError::Body(format!("deflate decompression error: {e}")))
    })?;
    Ok(Bytes::from(decoded))
}

/// Locate the start of the raw DEFLATE payload inside a gzip container
/// (RFC 1952 §2.3), bounds-checking every length-prefixed optional field
/// against `data.len()` so a truncated or malformed header is a typed
/// error rather than an out-of-bounds panic.
#[cfg(feature = "decompression")]
fn gzip_deflate_payload_start(data: &[u8]) -> Result<usize, OxiHttpError> {
    const GZIP_ID1: u8 = 0x1f;
    const GZIP_ID2: u8 = 0x8b;
    const GZIP_CM_DEFLATE: u8 = 8;

    if data.len() < 10 {
        return Err(OxiHttpError::Body("gzip: stream too short".to_string()));
    }
    if data[0] != GZIP_ID1 || data[1] != GZIP_ID2 {
        return Err(OxiHttpError::Body("gzip: bad magic bytes".to_string()));
    }
    if data[2] != GZIP_CM_DEFLATE {
        return Err(OxiHttpError::Body(format!(
            "gzip: unsupported compression method {}",
            data[2]
        )));
    }
    let flg = data[3];
    let mut pos: usize = 10;

    // FEXTRA (FLG bit 2): 2-byte little-endian length, then that many bytes.
    if flg & 0x04 != 0 {
        if pos + 2 > data.len() {
            return Err(OxiHttpError::Body(
                "gzip: truncated FEXTRA length".to_string(),
            ));
        }
        let xlen = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2;
        pos = pos
            .checked_add(xlen)
            .ok_or_else(|| OxiHttpError::Body("gzip: FEXTRA length overflow".to_string()))?;
        if pos > data.len() {
            return Err(OxiHttpError::Body(
                "gzip: truncated FEXTRA data".to_string(),
            ));
        }
    }

    // FNAME (FLG bit 3): NUL-terminated original file name.
    if flg & 0x08 != 0 {
        while pos < data.len() && data[pos] != 0 {
            pos += 1;
        }
        if pos >= data.len() {
            return Err(OxiHttpError::Body("gzip: truncated FNAME".to_string()));
        }
        pos += 1;
    }

    // FCOMMENT (FLG bit 4): NUL-terminated comment.
    if flg & 0x10 != 0 {
        while pos < data.len() && data[pos] != 0 {
            pos += 1;
        }
        if pos >= data.len() {
            return Err(OxiHttpError::Body("gzip: truncated FCOMMENT".to_string()));
        }
        pos += 1;
    }

    // FHCRC (FLG bit 1): 2-byte header CRC16.
    if flg & 0x02 != 0 {
        if pos + 2 > data.len() {
            return Err(OxiHttpError::Body("gzip: truncated FHCRC".to_string()));
        }
        pos += 2;
    }

    if pos > data.len() {
        return Err(OxiHttpError::Body(
            "gzip: header extends past end of data".to_string(),
        ));
    }

    Ok(pos)
}

/// Decompress a single-member gzip response body, bounded to `cap` bytes of
/// output.
///
/// `oxiarc_deflate::gzip_decompress` (the one-shot API) decodes eagerly
/// into an unbounded, internally growing buffer with no bounded/streaming
/// entry point comparable to `zlib_decompress_into`. To get the same
/// allocate-nothing-until-bounded guarantee this crate has for zlib/raw
/// DEFLATE, this function instead locates the raw DEFLATE payload after
/// the gzip header itself (RFC 1952 §2.3 — magic bytes, compression
/// method, then bounds-checked skips for the optional FEXTRA / FNAME /
/// FCOMMENT / FHCRC fields) and decodes *that* with
/// `oxiarc_deflate::inflate_into`, which allocates nothing internally and
/// stops as soon as the output would exceed the supplied buffer.
///
/// `inflate_into` has no way to report how many compressed bytes it
/// consumed, so this function cannot tell a single-member stream from the
/// first member of a concatenated multi-member one (as produced by e.g.
/// `gzip -c a b`) purely from a successful decode. Rather than silently
/// returning only the first member as if it were the whole body, the
/// decompressed length is checked against the trailing ISIZE field (RFC
/// 1952: the length of the final member modulo 2^32) — for the
/// overwhelmingly common single-member case (every mainstream HTTP server)
/// these match; a mismatch (truncation, corruption, or concatenation) is
/// rejected with a typed error rather than mis-decoded.
///
/// The trailing CRC-32 is also verified against the decoded bytes (using
/// `oxiarc_core::Crc32`, the same check `oxiarc_deflate::gzip_decompress`
/// itself performs), so a body that is corrupted in a way that happens to
/// preserve the exact decoded length is still caught rather than silently
/// accepted.
#[cfg(feature = "decompression")]
fn bounded_gzip_decompress(raw: &[u8], cap: usize) -> Result<Bytes, OxiHttpError> {
    let deflate_start = gzip_deflate_payload_start(raw)?;
    if raw.len() < deflate_start + 8 {
        return Err(OxiHttpError::Body("gzip: missing trailer".to_string()));
    }
    let trailer = &raw[raw.len() - 8..];
    let expected_crc = u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    let expected_isize = u32::from_le_bytes([trailer[4], trailer[5], trailer[6], trailer[7]]);

    let deflate_payload = &raw[deflate_start..];
    let decoded = bounded_inflate(deflate_payload, cap, |src, dst| {
        oxiarc_deflate::inflate_into(src, dst)
            .map_err(|e| OxiHttpError::Body(format!("gzip decompression error: {e}")))
    })?;

    let actual_isize = (decoded.len() as u64 & 0xFFFF_FFFF) as u32;
    if actual_isize != expected_isize {
        return Err(OxiHttpError::Body(
            "gzip: decoded length does not match the trailing ISIZE field (truncated body, \
             corrupted data, or an unsupported concatenated multi-member stream)"
                .to_string(),
        ));
    }

    let actual_crc = oxiarc_core::Crc32::compute(&decoded);
    if actual_crc != expected_crc {
        return Err(OxiHttpError::Body(format!(
            "gzip: CRC-32 mismatch (stored {expected_crc:#010x}, computed {actual_crc:#010x}): \
             response body is corrupted"
        )));
    }

    Ok(Bytes::from(decoded))
}

// ---------------------------------------------------------------------------
// RequestBuilder
// ---------------------------------------------------------------------------

/// Builder for a single HTTP request.
///
/// Created via `Client::get()`, `Client::post()`, etc.
pub struct RequestBuilder<C = HttpConnector> {
    client: HyperClient<C, PinnedBody>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    /// Set by [`multipart_stream`](Self::multipart_stream): when present,
    /// this streaming body is sent instead of `body`. Kept as a separate
    /// field (rather than folding `body` into an enum) so the common
    /// `.body()`/`.json()`/`.form()`/`.multipart()` path is untouched — see
    /// [`OutgoingBody`] for how the two are reconciled at send time.
    stream_body: Option<Body>,
    timeout: Option<Duration>,
    redirect_policy: RedirectPolicy,
    retry_policy: Option<RetryPolicy>,
    decompression: bool,
    max_response_body: usize,
    middleware: Vec<Arc<dyn ClientMiddleware>>,
    cookie_jar: Option<Arc<std::sync::Mutex<oxihttp_core::CookieJar>>>,
}

impl<C> RequestBuilder<C>
where
    C: Connect + Clone + Send + Sync + 'static,
{
    #[allow(clippy::too_many_arguments)]
    fn new(
        client: HyperClient<C, PinnedBody>,
        method: Method,
        uri: Uri,
        redirect_policy: RedirectPolicy,
        retry_policy: Option<RetryPolicy>,
        decompression: bool,
        max_response_body: usize,
        middleware: Vec<Arc<dyn ClientMiddleware>>,
        cookie_jar: Option<Arc<std::sync::Mutex<oxihttp_core::CookieJar>>>,
    ) -> Self {
        Self {
            client,
            method,
            uri,
            headers: HeaderMap::new(),
            body: Bytes::new(),
            stream_body: None,
            timeout: None,
            redirect_policy,
            retry_policy,
            decompression,
            max_response_body,
            middleware,
            cookie_jar,
        }
    }

    /// Add a request header.
    pub fn header(mut self, key: &str, value: &str) -> Result<Self, OxiHttpError> {
        let k =
            HeaderName::from_str(key).map_err(|e| OxiHttpError::InvalidHeader(e.to_string()))?;
        let v =
            HeaderValue::from_str(value).map_err(|e| OxiHttpError::InvalidHeader(e.to_string()))?;
        self.headers.insert(k, v);
        Ok(self)
    }

    /// Add multiple headers from a `HeaderMap`.
    pub fn headers(mut self, map: HeaderMap) -> Self {
        self.headers.extend(map);
        self
    }

    /// Set a Bearer token for the Authorization header.
    pub fn bearer_token(mut self, token: &str) -> Result<Self, OxiHttpError> {
        let v = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| OxiHttpError::InvalidHeader(e.to_string()))?;
        self.headers.insert(http::header::AUTHORIZATION, v);
        Ok(self)
    }

    /// Set Basic authentication for the Authorization header.
    pub fn basic_auth(
        mut self,
        username: &str,
        password: Option<&str>,
    ) -> Result<Self, OxiHttpError> {
        let credentials = match password {
            Some(pw) => format!("{username}:{pw}"),
            None => format!("{username}:"),
        };
        let encoded = base64_encode(credentials.as_bytes());
        let v = HeaderValue::from_str(&format!("Basic {encoded}"))
            .map_err(|e| OxiHttpError::InvalidHeader(e.to_string()))?;
        self.headers.insert(http::header::AUTHORIZATION, v);
        Ok(self)
    }

    /// Set the request body as raw bytes.
    ///
    /// Supersedes any streaming body previously set via
    /// [`.multipart_stream()`](Self::multipart_stream).
    pub fn body(mut self, b: impl Into<Bytes>) -> Self {
        self.body = b.into();
        self.stream_body = None;
        self
    }

    /// Set the request body as JSON, automatically setting the Content-Type header.
    ///
    /// Supersedes any streaming body previously set via
    /// [`.multipart_stream()`](Self::multipart_stream).
    pub fn json<T: serde::Serialize>(mut self, value: &T) -> Result<Self, OxiHttpError> {
        let json_bytes =
            serde_json::to_vec(value).map_err(|e| OxiHttpError::Json(e.to_string()))?;
        self.body = Bytes::from(json_bytes);
        self.stream_body = None;
        let ct = HeaderValue::from_static("application/json");
        self.headers.insert(http::header::CONTENT_TYPE, ct);
        Ok(self)
    }

    /// Set the request body as URL-encoded form data.
    ///
    /// Supersedes any streaming body previously set via
    /// [`.multipart_stream()`](Self::multipart_stream).
    pub fn form(mut self, form_body: &oxihttp_core::FormBody) -> Self {
        self.body = form_body.clone().build();
        self.stream_body = None;
        if let Ok(ct) = HeaderValue::from_str("application/x-www-form-urlencoded") {
            self.headers.insert(http::header::CONTENT_TYPE, ct);
        }
        self
    }

    /// Set the request body from a [`MultipartBuilder`], automatically setting
    /// the `Content-Type: multipart/form-data; boundary=…` header.
    ///
    /// The Content-Type is only set if the caller has not already provided one.
    /// This allows overriding the header with an explicit `.header()` call made
    /// *before* `.multipart()`.
    ///
    /// # This buffers the whole body
    ///
    /// This method calls
    /// [`MultipartBuilder::build`](oxihttp_core::MultipartBuilder::build),
    /// which concatenates every part into one in-memory `Bytes` buffer,
    /// exactly like [`.body()`](Self::body). For a large file part, prefer
    /// [`.multipart_stream()`](Self::multipart_stream), which streams each
    /// part to the wire instead of buffering the whole body first.
    ///
    /// [`MultipartBuilder`]: oxihttp_core::MultipartBuilder
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
    /// use oxihttp_client::Client;
    /// use oxihttp_core::MultipartBuilder;
    ///
    /// let client = Client::builder().build()?;
    /// let builder = MultipartBuilder::new().add_text("field", "value");
    /// let resp = client.post("http://example.com/upload")?
    ///     .multipart(builder)
    ///     .send()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn multipart(mut self, builder: oxihttp_core::MultipartBuilder) -> Self {
        // Retrieve content_type BEFORE build() because build() consumes the builder.
        let ct_str = builder.content_type();
        self.body = builder.build();
        self.stream_body = None;
        // Only set Content-Type when the caller has not already provided one.
        if !self.headers.contains_key(http::header::CONTENT_TYPE) {
            if let Ok(ct) = HeaderValue::from_str(&ct_str) {
                self.headers.insert(http::header::CONTENT_TYPE, ct);
            }
        }
        self
    }

    /// Set the request body from a [`StreamingMultipart`], streaming each
    /// part — including a large file part added via
    /// [`add_file_stream`](oxihttp_core::MultipartBuilder::add_file_stream)
    /// — directly to the wire as the request sends, instead of
    /// concatenating every part into one in-memory buffer first (that's
    /// what [`.multipart()`](Self::multipart) does). Peak client-side
    /// memory for the request body is therefore bounded by the largest
    /// single in-memory part (or, for a streamed part, by whatever chunk
    /// size the source stream itself yields), not by the sum of every
    /// part's size.
    ///
    /// The Content-Type is only set if the caller has not already provided
    /// one, exactly like [`.multipart()`](Self::multipart).
    ///
    /// # Trade-offs of a one-shot body
    ///
    /// A [`StreamingMultipart`]'s streamed parts are backed by a one-shot
    /// `Stream` that cannot be cloned or replayed. As a result, a request
    /// built with `multipart_stream`:
    ///
    /// - **Has no `Content-Length`**: the body's total size isn't known
    ///   up front (a streamed part's length is unknowable before it has
    ///   been fully read), so the request is sent with
    ///   `Transfer-Encoding: chunked` instead. Servers that require an
    ///   explicit `Content-Length` on uploads cannot accept a
    ///   `multipart_stream` request.
    /// - **Bypasses the client's [`RetryPolicy`]**: since the body cannot
    ///   be resent, this request is always sent exactly once, regardless
    ///   of any retry policy configured on the [`Client`]/[`ClientBuilder`].
    ///   Whatever the first attempt returns (success, an HTTP error status,
    ///   or a connection error) is returned as-is.
    /// - **Cannot follow a body-preserving redirect** (307/308): the body
    ///   has already been fully consumed by the first attempt, so it
    ///   cannot be resent to the redirect target. This returns
    ///   [`OxiHttpError::Body`] rather than silently sending an empty or
    ///   truncated body. A redirect that *drops* the body (301/302/303,
    ///   converted to GET) is followed normally, since no body needs to be
    ///   resent in that case.
    ///
    /// [`StreamingMultipart`]: oxihttp_core::StreamingMultipart
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
    /// use oxihttp_client::Client;
    /// use oxihttp_core::MultipartBuilder;
    ///
    /// let client = Client::builder().build()?;
    /// let file = tokio::fs::File::open("large-upload.bin").await?;
    /// // ... wrap `file` in a chunked `Stream<Item = Result<Bytes, OxiHttpError>>`, e.g. by
    /// // reading it in fixed-size pieces with `futures_util::stream::unfold` ...
    /// # struct NoChunksStream;
    /// # impl futures_core::Stream for NoChunksStream {
    /// #     type Item = Result<bytes::Bytes, oxihttp_core::OxiHttpError>;
    /// #     fn poll_next(
    /// #         self: std::pin::Pin<&mut Self>,
    /// #         _cx: &mut std::task::Context<'_>,
    /// #     ) -> std::task::Poll<Option<Self::Item>> {
    /// #         std::task::Poll::Ready(None)
    /// #     }
    /// # }
    /// # drop(file);
    /// # let chunk_stream = NoChunksStream;
    /// let streaming = MultipartBuilder::new()
    ///     .add_text("title", "large upload")
    ///     .add_file_stream("payload", "large-upload.bin", "application/octet-stream", chunk_stream);
    ///
    /// let resp = client.post("http://example.com/upload")?
    ///     .multipart_stream(streaming)
    ///     .send()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn multipart_stream(mut self, streaming: oxihttp_core::StreamingMultipart) -> Self {
        let ct_str = streaming.content_type();
        self.body = Bytes::new();
        self.stream_body = Some(streaming.build_stream());
        if !self.headers.contains_key(http::header::CONTENT_TYPE) {
            if let Ok(ct) = HeaderValue::from_str(&ct_str) {
                self.headers.insert(http::header::CONTENT_TYPE, ct);
            }
        }
        self
    }

    /// Set a per-request timeout.
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(duration);
        self
    }

    /// Override the maximum response body size (see
    /// [`ClientBuilder::with_max_response_body`]) for this request only.
    pub fn max_response_body(mut self, max_bytes: usize) -> Self {
        self.max_response_body = max_bytes;
        self
    }

    /// Send the request and return the response.
    ///
    /// Respects retry policy and per-request timeout.
    /// Before the first attempt the `before_request` hook is called on each
    /// registered middleware; after a successful response `after_response` is
    /// called with the final status and elapsed wall-clock time.
    pub async fn send(self) -> Result<Response, OxiHttpError> {
        let RequestBuilder {
            client,
            method,
            uri,
            headers,
            body,
            stream_body,
            timeout,
            redirect_policy,
            retry_policy,
            decompression,
            max_response_body,
            middleware,
            cookie_jar,
        } = self;

        // --- middleware: before_request -----------------------------------
        {
            let ctx = middleware::RequestContext {
                method: &method,
                uri: &uri,
                headers: &headers,
            };
            for mw in &middleware {
                mw.before_request(&ctx);
            }
        }

        let start = Instant::now();

        // A streaming body (see `multipart_stream`) is backed by a one-shot
        // `Stream` that cannot be cloned or replayed, so it can only ever be
        // sent once: no retry-policy-driven re-attempts. (A body-preserving
        // redirect on a one-shot body is separately rejected with a typed
        // error inside `OutgoingBody::take_for_hop`, invoked from
        // `send_inner`.) Capping `max_attempts` at 1 here enforces the
        // no-retry half of that contract.
        let is_streaming = stream_body.is_some();
        let mut stream_body_slot = stream_body;
        let max_attempts = if is_streaming {
            1
        } else {
            retry_policy
                .as_ref()
                .map(|p| p.max_retries + 1)
                .unwrap_or(1)
        };

        for attempt in 0..max_attempts {
            let outgoing = if is_streaming {
                OutgoingBody::OneShot(stream_body_slot.take())
            } else {
                OutgoingBody::Reusable(body.clone())
            };
            let result = {
                let fut = send_inner(
                    &client,
                    method.clone(),
                    uri.clone(),
                    outgoing,
                    headers.clone(),
                    &redirect_policy,
                    decompression,
                    max_response_body,
                    cookie_jar.clone(),
                );
                if let Some(dur) = timeout {
                    match tokio::time::timeout(dur, fut).await {
                        Ok(r) => r,
                        Err(_) => Err(OxiHttpError::Timeout(format!(
                            "request timed out after {}ms",
                            dur.as_millis()
                        ))),
                    }
                } else {
                    fut.await
                }
            };

            match result {
                Ok(resp) => {
                    if let Some(ref policy) = retry_policy {
                        if attempt < max_attempts - 1
                            && policy.should_retry_status(resp.status().as_u16())
                        {
                            let delay = policy.backoff_delay(attempt);
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                    }
                    // --- middleware: after_response ----------------------
                    let elapsed = start.elapsed();
                    let resp_ctx = middleware::ResponseContext {
                        status: resp.status(),
                        elapsed,
                    };
                    for mw in &middleware {
                        mw.after_response(&resp_ctx);
                    }
                    return Ok(resp);
                }
                Err(e) => {
                    if let Some(ref policy) = retry_policy {
                        let should_retry = match &e {
                            OxiHttpError::Hyper(_) => policy.retry_on_connection_error,
                            OxiHttpError::Timeout(_) => policy.retry_on_timeout,
                            OxiHttpError::Io(_) => policy.retry_on_connection_error,
                            _ => false,
                        };
                        if should_retry && attempt < max_attempts - 1 {
                            let delay = policy.backoff_delay(attempt);
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                    }
                    return Err(e);
                }
            }
        }

        // This is unreachable when max_attempts >= 1, but needed for the type checker.
        Err(OxiHttpError::Hyper("max retries exceeded".to_string()))
    }
}

/// The request body carried through [`send_inner`]'s per-hop send.
///
/// `Reusable` is the common case — everything built via `.body()`,
/// `.json()`, `.form()`, or `.multipart()` — where the underlying `Bytes`
/// buffer can be cheaply re-cloned (an `O(1)` refcount bump) for every
/// redirect hop and retry attempt, exactly as before this type existed.
///
/// `OneShot` backs [`RequestBuilder::multipart_stream`]: a body that
/// streams to the wire as it sends and, once taken, cannot be produced
/// again. Calling [`take_for_hop`](Self::take_for_hop) a second time (a
/// retry, or a body-preserving 307/308 redirect) returns a typed error
/// instead of silently sending an empty or corrupted body.
enum OutgoingBody {
    Reusable(Bytes),
    OneShot(Option<Body>),
}

impl OutgoingBody {
    /// Produce the [`PinnedBody`] to send for the next hop, taking a
    /// `OneShot` body's inner stream on its single permitted use.
    fn take_for_hop(&mut self) -> Result<PinnedBody, OxiHttpError> {
        match self {
            OutgoingBody::Reusable(bytes) => Ok(Body::from(bytes.clone()).into_pinned()),
            OutgoingBody::OneShot(slot) => match slot.take() {
                Some(body) => Ok(body.into_pinned()),
                None => Err(OxiHttpError::Body(
                    "streaming request body (multipart_stream) was already sent once and \
                     cannot be sent again — it cannot be retried and cannot follow a \
                     body-preserving (307/308) redirect"
                        .to_string(),
                )),
            },
        }
    }

    /// Called when a redirect drops the body (301/302/303, converted to
    /// GET): from this point on in the redirect chain there is nothing
    /// left to (re)send, whether the original body was reusable or
    /// one-shot.
    fn clear(&mut self) {
        *self = OutgoingBody::Reusable(Bytes::new());
    }
}

/// Inner request executor: handles redirect loop and returns a `Response`.
///
/// All clone-able fields are passed by value so the outer retry loop can
/// re-invoke this function on each attempt. `body` is the exception: a
/// [`OutgoingBody::OneShot`] can only be taken once across every hop of
/// every attempt combined — see [`OutgoingBody`].
#[allow(clippy::too_many_arguments)]
async fn send_inner<C>(
    client: &HyperClient<C, PinnedBody>,
    mut method: Method,
    mut uri: Uri,
    mut body: OutgoingBody,
    mut headers: HeaderMap,
    redirect_policy: &RedirectPolicy,
    decompression: bool,
    max_response_body: usize,
    cookie_jar: Option<Arc<std::sync::Mutex<oxihttp_core::CookieJar>>>,
) -> Result<Response, OxiHttpError>
where
    C: Connect + Clone + Send + Sync + 'static,
{
    let max_redirects = redirect_policy.max_redirects();
    let mut redirect_count: usize = 0;

    // Origin of the very first request. Credential-bearing headers
    // (`Authorization`, `Cookie`) must not be forwarded once a redirect leaves
    // this origin, mirroring browser / RFC 9110 §15.4 behavior.
    let original_uri = uri.clone();

    loop {
        let mut req_builder = http::Request::builder()
            .method(method.clone())
            .uri(uri.clone());
        for (k, v) in &headers {
            req_builder = req_builder.header(k, v);
        }

        // Inject Accept-Encoding when decompression is enabled and the user
        // hasn't already set the header. Only advertise gzip/deflate support
        // when this build can actually decode them (the `decompression`
        // feature): otherwise `body_bytes()` would reject the very response
        // this header invited (see `decompression_feature_missing_error`),
        // and a server has no way to know this client cannot follow through.
        #[cfg(feature = "decompression")]
        if decompression && !headers.contains_key(http::header::ACCEPT_ENCODING) {
            req_builder = req_builder.header(
                http::header::ACCEPT_ENCODING,
                HeaderValue::from_static("gzip, deflate"),
            );
        }

        let pinned_body = body.take_for_hop()?;
        let mut req = req_builder
            .body(pinned_body)
            .map_err(|e| OxiHttpError::Http(Arc::new(e)))?;

        // Inject cookies from jar for this URL
        if let Some(ref jar) = cookie_jar {
            if let Ok(guard) = jar.lock() {
                if let Some(cookie_header) = guard.to_cookie_header_for_url(&uri) {
                    if let Ok(hv) = HeaderValue::from_str(&cookie_header) {
                        req.headers_mut().insert(http::header::COOKIE, hv);
                    }
                }
            }
        }

        let resp = client
            .request(req)
            .await
            .map_err(|e| OxiHttpError::Hyper(e.to_string()))?;

        // Persist Set-Cookie headers into jar
        if let Some(ref jar) = cookie_jar {
            if let Ok(mut guard) = jar.lock() {
                guard.add_from_response_headers(resp.headers(), &uri);
            }
        }

        // Check for redirect
        let status = resp.status();
        if redirect::is_redirect_status(status) {
            if let Some(max) = max_redirects {
                if max == 0 || redirect_count >= max {
                    // Return the redirect response as-is when not following
                    if max == 0 {
                        return Ok(Response {
                            inner: resp,
                            decompress: decompression,
                            max_body_bytes: max_response_body,
                        });
                    }
                    return Err(OxiHttpError::Redirect(format!(
                        "too many redirects (max: {max})"
                    )));
                }
            }
            redirect_count += 1;

            // Extract the Location header
            let location = resp
                .headers()
                .get(http::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    OxiHttpError::Redirect("redirect response missing Location header".to_string())
                })?;

            // Resolve relative URIs
            let new_uri = resolve_redirect_uri(&uri, location)?;

            // Strip credential-bearing headers when the redirect target leaves
            // the original origin (host or scheme differs) to avoid leaking
            // `Authorization` / `Cookie` to a third party.
            if !same_origin(&original_uri, &new_uri) {
                headers.remove(http::header::AUTHORIZATION);
                headers.remove(http::header::COOKIE);
            }

            // Update method (POST -> GET for 301/302/303)
            let new_method = redirect::redirect_method(status, &method);

            // Clear body if method changed away from body-carrying
            if !redirect::should_preserve_body(status) {
                body.clear();
            }

            method = new_method;
            uri = new_uri;
            continue;
        }

        return Ok(Response {
            inner: resp,
            decompress: decompression,
            max_body_bytes: max_response_body,
        });
    }
}

/// Resolve a redirect URI, handling both absolute and relative URIs.
fn resolve_redirect_uri(base: &Uri, location: &str) -> Result<Uri, OxiHttpError> {
    // Try parsing as absolute URI first
    if let Ok(uri) = Uri::from_str(location) {
        if uri.scheme().is_some() {
            return Ok(uri);
        }
    }

    // Relative URI: combine with base
    let scheme = base.scheme_str().unwrap_or("http");
    let authority = base.authority().map(|a| a.as_str()).unwrap_or("localhost");
    let full = format!("{scheme}://{authority}{location}");
    Uri::from_str(&full).map_err(|e| OxiHttpError::InvalidUri(Arc::new(e)))
}

/// Return `true` when two URIs share the same origin for the purpose of
/// forwarding credentials across a redirect.
///
/// Two URIs are same-origin when their scheme and host match (both compared
/// case-insensitively). A differing host *or* scheme is treated as a distinct
/// origin, matching browser behavior for `Authorization` / `Cookie` stripping.
fn same_origin(a: &Uri, b: &Uri) -> bool {
    let scheme_a = a.scheme_str().unwrap_or("http");
    let scheme_b = b.scheme_str().unwrap_or("http");
    if !scheme_a.eq_ignore_ascii_case(scheme_b) {
        return false;
    }
    match (a.host(), b.host()) {
        (Some(x), Some(y)) => x.eq_ignore_ascii_case(y),
        (None, None) => true,
        _ => false,
    }
}

// TlsRebuildConfig — stores all TLS + pool params needed to re-create an
// HttpsClient with modified trust settings (used by with_request_tls_config).
#[cfg(feature = "tls")]
#[derive(Clone)]
pub(crate) struct TlsRebuildConfig {
    pub trusted_certs_der: Vec<Vec<u8>>,
    pub alpn: Vec<String>,
    pub accept_invalid_certs: bool,
    pub use_webpki_roots: bool,
    pub key_log_path: Option<std::path::PathBuf>,
    pub early_data: bool,
    pub connect_timeout: Option<Duration>,
    pub tcp_nodelay: Option<bool>,
    pub tcp_keepalive: Option<Duration>,
    pub http2_settings: Option<Http2Settings>,
    pub pool_max_idle_per_host: Option<usize>,
    pub pool_idle_timeout: Option<Duration>,
    /// Optional custom certificate verifier injected via
    /// [`ClientBuilder::with_custom_cert_verifier`].  When `Some`, this verifier
    /// takes precedence over all other trust-store settings.
    pub custom_cert_verifier: Option<Arc<dyn rustls::client::danger::ServerCertVerifier>>,
}

#[cfg(feature = "tls")]
impl std::fmt::Debug for TlsRebuildConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsRebuildConfig")
            .field("trusted_certs_der_count", &self.trusted_certs_der.len())
            .field("alpn", &self.alpn)
            .field("accept_invalid_certs", &self.accept_invalid_certs)
            .field("use_webpki_roots", &self.use_webpki_roots)
            .field("early_data", &self.early_data)
            .field("connect_timeout", &self.connect_timeout)
            .field("tcp_nodelay", &self.tcp_nodelay)
            .field("tcp_keepalive", &self.tcp_keepalive)
            .field(
                "custom_cert_verifier",
                &self
                    .custom_cert_verifier
                    .as_ref()
                    .map(|_| "<dyn ServerCertVerifier>"),
            )
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Client<C>
// ---------------------------------------------------------------------------

/// HTTP client with connection pooling, redirect handling, and retry support.
///
/// The default type parameter `C = HttpConnector` gives a plain HTTP-only
/// client. Use `HttpsClient` (feature `tls`) for a TLS-capable client.
///
/// Created via `Client::builder().build()` or `Client::builder().build_https()`.
#[derive(Clone)]
pub struct Client<C = HttpConnector> {
    pub(crate) inner: HyperClient<C, PinnedBody>,
    pub(crate) redirect_policy: RedirectPolicy,
    pub(crate) retry_policy: Option<RetryPolicy>,
    pub(crate) default_headers: HeaderMap,
    pub(crate) connect_timeout: Option<Duration>,
    pub(crate) read_timeout: Option<Duration>,
    pub(crate) decompression: bool,
    /// Maximum response body size in bytes (see [`DEFAULT_MAX_RESPONSE_BODY`]
    /// and [`ClientBuilder::with_max_response_body`]).
    pub(crate) max_response_body: usize,
    /// Ordered list of middleware interceptors applied to every request.
    pub(crate) middleware: Vec<Arc<dyn ClientMiddleware>>,
    /// Optional shared cookie jar for automatic RFC 6265 cookie management.
    pub(crate) cookie_jar: Option<Arc<std::sync::Mutex<oxihttp_core::CookieJar>>>,
    /// TLS rebuild parameters, populated only for [`HttpsClient`] instances.
    ///
    /// Used by [`HttpsClient::with_request_tls_config`] to construct a fresh
    /// client with modified TLS trust settings.
    #[cfg(feature = "tls")]
    pub(crate) tls_rebuild: Option<Arc<TlsRebuildConfig>>,
}

/// A TLS-capable client that supports both `http://` and `https://` URIs.
///
/// Created via `Client::builder().build_https()`.
#[cfg(feature = "tls")]
pub type HttpsClient = Client<OxiHttpsConnector<HttpConnector>>;

/// An HTTP client using a custom DNS resolver (plain HTTP).
///
/// Created via `Client::builder().with_resolver(r).build_with_resolver()`.
pub type ResolverClient = Client<HttpConnector<BoxResolver>>;

/// An HTTP client using a custom DNS resolver with TLS support.
///
/// Created via `Client::builder().with_resolver(r).build_https_with_resolver()`.
#[cfg(feature = "tls")]
pub type ResolverHttpsClient =
    Client<crate::connector::OxiHttpsConnector<HttpConnector<BoxResolver>>>;

/// Provide `builder()` only on the default `Client<HttpConnector>` variant so
/// that type-inference works without annotation at call sites.
impl Client<HttpConnector> {
    /// Return a `ClientBuilder` for configuring a new client.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::new()
    }
}

// ---------------------------------------------------------------------------
// HttpsClient — per-request TLS config override
// ---------------------------------------------------------------------------

/// Per-request TLS overrides for [`HttpsClient`].
///
/// These methods are only available on clients built via
/// [`ClientBuilder::build_https`].
#[cfg(feature = "tls")]
impl Client<OxiHttpsConnector<HttpConnector>> {
    /// Return a new `HttpsClient` that shares all settings with `self` except
    /// for the TLS trust configuration, which is replaced by `override_cfg`.
    ///
    /// The returned client has its **own independent connection pool**.  Use it
    /// to make requests that require different TLS trust than the original
    /// client (e.g., certificate pinning to a different CA).
    ///
    /// # Errors
    ///
    /// Returns an error if the TLS connector cannot be built from the merged
    /// configuration (e.g., a supplied DER-encoded certificate is malformed).
    ///
    /// # Notes on connection pooling
    ///
    /// Because the returned client uses a separate pool, it will always open a
    /// fresh connection even if the original client already has an idle
    /// connection to the same host.  This guarantees that the override TLS
    /// config is applied.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use oxihttp_client::{Client, request_config::RequestTlsConfig};
    /// # async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
    /// let global_client = Client::builder()
    ///     .with_trusted_cert_der(vec![/* CA cert A DER … */])
    ///     .build_https()?;
    ///
    /// // Override: trust CA cert B instead of CA cert A for a single request.
    /// let pinned = global_client.with_request_tls_config(
    ///     RequestTlsConfig::new().with_trusted_cert(vec![/* CA cert B DER … */]),
    /// )?;
    /// let resp = pinned.get("https://pinned-endpoint.example.com")?.send().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_request_tls_config(
        &self,
        override_cfg: RequestTlsConfig,
    ) -> Result<Self, OxiHttpError> {
        use crate::connector::OxiHttpsConnector;

        let base = self.tls_rebuild.as_ref().ok_or_else(|| {
            OxiHttpError::Tls(
                "client has no TLS rebuild config (was it built with build_https()?)".to_string(),
            )
        })?;

        // Merge: per-request overrides win over global config.
        let effective_certs = if override_cfg.trusted_cert_ders.is_empty() {
            base.trusted_certs_der.as_slice()
        } else {
            override_cfg.trusted_cert_ders.as_slice()
        };
        let accept_invalid = base.accept_invalid_certs || override_cfg.accept_invalid_certs;

        // If a custom verifier is installed on the base config, use the
        // verifier-path builder so the custom verifier is preserved.
        let new_tls = if let Some(ref verifier) = base.custom_cert_verifier {
            tls::build_tls_connector_with_verifier(
                Arc::clone(verifier),
                &base.alpn,
                base.early_data,
            )?
        } else {
            tls::build_tls_connector(
                effective_certs,
                &base.alpn,
                accept_invalid,
                base.use_webpki_roots,
                base.key_log_path.clone(),
                base.early_data,
            )?
        };

        let mut http = HttpConnector::new();
        http.enforce_http(false);
        if let Some(dur) = base.connect_timeout {
            http.set_connect_timeout(Some(dur));
        }
        if let Some(nodelay) = base.tcp_nodelay {
            http.set_nodelay(nodelay);
        }
        if let Some(ka) = base.tcp_keepalive {
            http.set_keepalive(Some(ka));
        }
        let https_connector = OxiHttpsConnector::new(http, new_tls);

        let mut hb = HyperClient::builder(TokioExecutor::new());
        if let Some(n) = base.pool_max_idle_per_host {
            hb.pool_max_idle_per_host(n);
        }
        if let Some(dur) = base.pool_idle_timeout {
            hb.pool_idle_timeout(dur);
        }
        if let Some(ref h2) = base.http2_settings {
            apply_http2_settings(&mut hb, h2);
        }

        // Build a new TlsRebuildConfig reflecting the merged settings so that
        // further calls to `with_request_tls_config` on the returned client
        // start from a consistent state.
        let new_rebuild = Arc::new(TlsRebuildConfig {
            trusted_certs_der: effective_certs.to_vec(),
            alpn: base.alpn.clone(),
            accept_invalid_certs: accept_invalid,
            use_webpki_roots: base.use_webpki_roots,
            key_log_path: base.key_log_path.clone(),
            early_data: base.early_data,
            connect_timeout: base.connect_timeout,
            tcp_nodelay: base.tcp_nodelay,
            tcp_keepalive: base.tcp_keepalive,
            http2_settings: base.http2_settings.clone(),
            pool_max_idle_per_host: base.pool_max_idle_per_host,
            pool_idle_timeout: base.pool_idle_timeout,
            custom_cert_verifier: base.custom_cert_verifier.clone(),
        });

        Ok(Client {
            inner: hb.build(https_connector),
            redirect_policy: self.redirect_policy.clone(),
            retry_policy: self.retry_policy.clone(),
            default_headers: self.default_headers.clone(),
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
            decompression: self.decompression,
            max_response_body: self.max_response_body,
            middleware: self.middleware.clone(),
            cookie_jar: self.cookie_jar.clone(),
            tls_rebuild: Some(new_rebuild),
        })
    }
}

impl<C> Client<C>
where
    C: Connect + Clone + Send + Sync + 'static,
{
    /// Create a request builder for the given method and URL.
    fn request_builder(
        &self,
        method: Method,
        url: &str,
    ) -> Result<RequestBuilder<C>, OxiHttpError> {
        let uri = Uri::from_str(url)?;
        let mut rb = RequestBuilder::new(
            self.inner.clone(),
            method,
            uri,
            self.redirect_policy.clone(),
            self.retry_policy.clone(),
            self.decompression,
            self.max_response_body,
            self.middleware.clone(),
            self.cookie_jar.clone(),
        );
        // Apply default headers
        for (k, v) in &self.default_headers {
            rb.headers.insert(k.clone(), v.clone());
        }
        Ok(rb)
    }

    /// Build a GET request for the given URL.
    pub fn get(&self, url: &str) -> Result<RequestBuilder<C>, OxiHttpError> {
        self.request_builder(Method::GET, url)
    }

    /// Build a POST request for the given URL.
    pub fn post(&self, url: &str) -> Result<RequestBuilder<C>, OxiHttpError> {
        self.request_builder(Method::POST, url)
    }

    /// Build a PUT request for the given URL.
    pub fn put(&self, url: &str) -> Result<RequestBuilder<C>, OxiHttpError> {
        self.request_builder(Method::PUT, url)
    }

    /// Build a DELETE request for the given URL.
    pub fn delete(&self, url: &str) -> Result<RequestBuilder<C>, OxiHttpError> {
        self.request_builder(Method::DELETE, url)
    }

    /// Build a PATCH request for the given URL.
    pub fn patch(&self, url: &str) -> Result<RequestBuilder<C>, OxiHttpError> {
        self.request_builder(Method::PATCH, url)
    }

    /// Build a HEAD request for the given URL.
    pub fn head(&self, url: &str) -> Result<RequestBuilder<C>, OxiHttpError> {
        self.request_builder(Method::HEAD, url)
    }

    /// Execute a pre-built `http::Request`.
    ///
    /// For a request whose body must stream to the wire instead of being
    /// pre-buffered (for example one built via
    /// [`StreamingMultipart::build_stream`](oxihttp_core::StreamingMultipart::build_stream)),
    /// use [`execute_body`](Self::execute_body) instead.
    pub async fn execute(&self, req: http::Request<Full<Bytes>>) -> Result<Response, OxiHttpError> {
        let (parts, full_body) = req.into_parts();
        // `Full<Bytes>::Error = Infallible`: this body is already completely
        // in memory, so `.collect()` cannot actually fail or await real I/O —
        // it just hands the bytes back out. The `match ... {}` on the `Err`
        // arm proves that at compile time instead of `.expect()`-ing it away.
        let bytes = match full_body.collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(never) => match never {},
        };
        let req = http::Request::from_parts(parts, Body::from(bytes).into_pinned());
        let resp = self
            .inner
            .request(req)
            .await
            .map_err(|e| OxiHttpError::Hyper(e.to_string()))?;
        Ok(Response {
            inner: resp,
            decompress: self.decompression,
            max_body_bytes: self.max_response_body,
        })
    }

    /// Execute a pre-built `http::Request` carrying an [`oxihttp_core::Body`]
    /// — the streaming-capable counterpart of [`execute`](Self::execute).
    ///
    /// Accepts any `Body` variant: [`Body::empty`](oxihttp_core::Body::empty),
    /// [`Body::full`](oxihttp_core::Body::full), or a
    /// [`Body::stream`](oxihttp_core::Body::stream) built directly or via
    /// [`StreamingMultipart::build_stream`](oxihttp_core::StreamingMultipart::build_stream)
    /// — so a caller who needs full control over the request (rather than
    /// going through [`RequestBuilder`]) can still send a streaming body
    /// without buffering it first. Like `execute`, this bypasses
    /// redirect-following and the retry policy entirely: it is a single,
    /// direct request/response round trip.
    pub async fn execute_body(&self, req: http::Request<Body>) -> Result<Response, OxiHttpError> {
        let (parts, body) = req.into_parts();
        let req = http::Request::from_parts(parts, body.into_pinned());
        let resp = self
            .inner
            .request(req)
            .await
            .map_err(|e| OxiHttpError::Hyper(e.to_string()))?;
        Ok(Response {
            inner: resp,
            decompress: self.decompression,
            max_body_bytes: self.max_response_body,
        })
    }

    /// Convenience: GET the URL and return the response body as bytes.
    pub async fn get_bytes(&self, url: &str) -> Result<Bytes, OxiHttpError> {
        let resp = self.get(url)?.send().await?;
        resp.error_for_status()?.body_bytes().await
    }

    /// Convenience: GET the URL and deserialize the JSON response body.
    pub async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<T, OxiHttpError> {
        let resp = self.get(url)?.send().await?;
        resp.error_for_status()?.body_json().await
    }

    /// Convenience: POST JSON and deserialize the response.
    pub async fn post_json<T: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &T,
    ) -> Result<R, OxiHttpError> {
        let resp = self.post(url)?.json(body)?.send().await?;
        resp.error_for_status()?.body_json().await
    }

    /// Returns a reference to the retry policy, if configured.
    pub fn retry_policy(&self) -> Option<&RetryPolicy> {
        self.retry_policy.as_ref()
    }

    /// Returns a reference to the connect timeout, if set.
    pub fn connect_timeout(&self) -> Option<Duration> {
        self.connect_timeout
    }

    /// Returns a reference to the read timeout, if set.
    pub fn read_timeout(&self) -> Option<Duration> {
        self.read_timeout
    }
}

impl<C> std::fmt::Debug for Client<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("redirect_policy", &self.redirect_policy)
            .field("retry_policy", &self.retry_policy)
            .field("default_headers_count", &self.default_headers.len())
            .finish()
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

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use oxihttp_core::MultipartBuilder;

    /// Helper: build a plain-HTTP client and a POST RequestBuilder targeting a
    /// dummy URL. The builder is never actually sent, so the URL doesn't need to
    /// resolve — we only inspect the headers that would be set.
    fn post_builder() -> RequestBuilder {
        let client = Client::builder().build().expect("client build");
        client
            .post("http://127.0.0.1:0/test")
            .expect("request builder")
    }

    /// `.multipart()` without a prior Content-Type must auto-set
    /// `multipart/form-data; boundary=…` including the exact boundary value.
    #[test]
    fn multipart_sets_content_type_automatically() {
        let mp = MultipartBuilder::new().add_text("field", "value");
        // Capture boundary before the builder is consumed by .multipart().
        let expected_boundary = mp.boundary().to_owned();

        let rb = post_builder().multipart(mp);

        let ct = rb
            .headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .expect("Content-Type header must be set after .multipart()");

        assert!(
            ct.starts_with("multipart/form-data; boundary="),
            "Content-Type must start with multipart/form-data; boundary= but got: {ct}"
        );
        assert!(
            ct.contains(&expected_boundary),
            "Content-Type must contain the boundary '{expected_boundary}' but got: {ct}"
        );
    }

    /// If the caller sets Content-Type *before* `.multipart()`, the explicit
    /// header must be preserved (not overridden by the auto-detection).
    #[test]
    fn multipart_does_not_override_explicit_content_type() {
        let mp = MultipartBuilder::new().add_text("x", "y");

        let rb = post_builder()
            .header("content-type", "application/octet-stream")
            .expect("header set")
            .multipart(mp);

        let ct = rb
            .headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .expect("Content-Type header must be present");

        assert_eq!(
            ct, "application/octet-stream",
            "explicit Content-Type must not be overridden by .multipart()"
        );
    }

    /// Helper: build a HeaderMap carrying credential-bearing headers, then apply
    /// the same cross-origin stripping logic the redirect loop performs.
    fn strip_for_redirect(from: &str, to: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret-token"),
        );
        headers.insert(
            http::header::COOKIE,
            HeaderValue::from_static("session=abc"),
        );
        let from_uri = Uri::from_str(from).expect("from uri");
        let to_uri = Uri::from_str(to).expect("to uri");
        if !same_origin(&from_uri, &to_uri) {
            headers.remove(http::header::AUTHORIZATION);
            headers.remove(http::header::COOKIE);
        }
        headers
    }

    /// A redirect to a *different host* must drop `Authorization` and `Cookie`
    /// so credentials are not leaked to a third party.
    #[test]
    fn redirect_cross_host_strips_credentials() {
        let headers = strip_for_redirect("http://example.com/a", "http://evil.example.net/b");
        assert!(
            !headers.contains_key(http::header::AUTHORIZATION),
            "Authorization must be stripped on cross-host redirect"
        );
        assert!(
            !headers.contains_key(http::header::COOKIE),
            "Cookie must be stripped on cross-host redirect"
        );
    }

    /// A redirect that only changes the *scheme* (https -> http) is also a
    /// different origin and must drop credentials.
    #[test]
    fn redirect_cross_scheme_strips_credentials() {
        let headers = strip_for_redirect("https://example.com/a", "http://example.com/b");
        assert!(
            !headers.contains_key(http::header::AUTHORIZATION),
            "Authorization must be stripped on scheme change"
        );
        assert!(
            !headers.contains_key(http::header::COOKIE),
            "Cookie must be stripped on scheme change"
        );
    }

    /// A same-origin redirect (host + scheme unchanged) must preserve the
    /// credential-bearing headers.
    #[test]
    fn redirect_same_origin_keeps_credentials() {
        let headers = strip_for_redirect("http://example.com/a", "http://example.com/b");
        assert_eq!(
            headers
                .get(http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok()),
            Some("Bearer secret-token"),
            "Authorization must survive a same-origin redirect"
        );
        assert_eq!(
            headers
                .get(http::header::COOKIE)
                .and_then(|v| v.to_str().ok()),
            Some("session=abc"),
            "Cookie must survive a same-origin redirect"
        );
    }

    /// Host comparison is case-insensitive: differing case is still same-origin.
    #[test]
    fn same_origin_is_case_insensitive() {
        let a = Uri::from_str("http://Example.COM/x").expect("a");
        let b = Uri::from_str("http://example.com/y").expect("b");
        assert!(same_origin(&a, &b));
    }

    // -----------------------------------------------------------------
    // collect_body_limited / bounded decompression — S-severity
    // regression coverage (uncapped response body + decompression bomb).
    //
    // See crates/oxihttp/tests/client_decompression_bomb_test.rs for the
    // end-to-end (real server + real client) versions of these; the tests
    // below exercise the pure helper functions directly for fast,
    // deterministic edge-case coverage (truncated / malformed headers)
    // that would be awkward to provoke through a real HTTP round-trip.
    // -----------------------------------------------------------------

    #[cfg(feature = "decompression")]
    mod bounded_decompression_tests {
        use super::*;

        #[test]
        fn gzip_deflate_payload_start_rejects_truncated_header() {
            let too_short = [0x1fu8, 0x8b, 0x08, 0x00, 0x00];
            let err = gzip_deflate_payload_start(&too_short).expect_err("must reject");
            assert!(err.to_string().contains("too short"));
        }

        #[test]
        fn gzip_deflate_payload_start_rejects_bad_magic() {
            let bad_magic = [0u8; 10];
            let err = gzip_deflate_payload_start(&bad_magic).expect_err("must reject");
            assert!(err.to_string().contains("magic"));
        }

        #[test]
        fn gzip_deflate_payload_start_rejects_unsupported_method() {
            let mut header = [0u8; 10];
            header[0] = 0x1f;
            header[1] = 0x8b;
            header[2] = 99; // not CM=8 (deflate)
            let err = gzip_deflate_payload_start(&header).expect_err("must reject");
            assert!(err.to_string().contains("compression method"));
        }

        #[test]
        fn gzip_deflate_payload_start_skips_fname_and_fextra() {
            // FLG = FEXTRA (0x04) | FNAME (0x08)
            let mut data = vec![0x1f, 0x8b, 0x08, 0x0C, 0, 0, 0, 0, 0, 0xff];
            // FEXTRA: 2-byte XLEN=3, then 3 bytes of extra data.
            data.extend_from_slice(&3u16.to_le_bytes());
            data.extend_from_slice(b"abc");
            // FNAME: NUL-terminated "f.txt".
            data.extend_from_slice(b"f.txt\0");
            // Trailing byte stands in for the start of the DEFLATE payload.
            data.push(0xAA);

            let start = gzip_deflate_payload_start(&data).expect("valid header");
            assert_eq!(start, data.len() - 1, "must point at the trailing 0xAA");
        }

        #[test]
        fn gzip_deflate_payload_start_rejects_truncated_fextra() {
            let mut data = vec![0x1f, 0x8b, 0x08, 0x04, 0, 0, 0, 0, 0, 0xff];
            // Claims 100 bytes of extra field but supplies none.
            data.extend_from_slice(&100u16.to_le_bytes());
            let err = gzip_deflate_payload_start(&data).expect_err("must reject");
            assert!(err.to_string().contains("FEXTRA"));
        }

        #[test]
        fn bounded_inflate_never_allocates_past_cap_on_repeated_failure() {
            // Garbage input that is not valid DEFLATE at any buffer size:
            // both the heuristic-sized first attempt and the cap-sized
            // retry must fail cleanly (typed Err), never panic.
            let garbage = vec![0xFFu8; 32];
            let result = bounded_inflate(&garbage, 64, |src, dst| {
                oxiarc_deflate::inflate_into(src, dst)
                    .map_err(|e| OxiHttpError::Body(format!("test decode error: {e}")))
            });
            assert!(result.is_err(), "corrupt input must error, not panic");
        }

        #[test]
        fn bounded_inflate_zero_cap_errors_without_panicking() {
            let garbage = vec![0x01u8, 0x02, 0x03];
            let result = bounded_inflate(&garbage, 0, |src, dst| {
                oxiarc_deflate::inflate_into(src, dst)
                    .map_err(|e| OxiHttpError::Body(format!("test decode error: {e}")))
            });
            assert!(result.is_err(), "a zero-byte cap must error, not panic");
        }

        #[test]
        fn bounded_gzip_decompress_round_trips_small_payload() {
            let plaintext = b"round trip me";
            let compressed = oxiarc_deflate::gzip_compress(plaintext, 6).expect("gzip_compress");
            let out = bounded_gzip_decompress(&compressed, 1024).expect("decompress");
            assert_eq!(out.as_ref(), plaintext);
        }

        /// Regression test: a gzip body corrupted in a way that preserves
        /// the exact decoded length (so the ISIZE check alone would pass)
        /// must still be rejected via the CRC-32 check.
        #[test]
        fn bounded_gzip_decompress_rejects_crc_mismatch_with_correct_length() {
            let plaintext = b"integrity check me";
            let mut compressed =
                oxiarc_deflate::gzip_compress(plaintext, 6).expect("gzip_compress");
            // Corrupt only the stored CRC-32 (the 4 bytes immediately before
            // the trailing ISIZE) — the DEFLATE payload and ISIZE are left
            // untouched, so decompression still succeeds and produces the
            // *correct* length. A length-only check would miss this.
            let crc_start = compressed.len() - 8;
            compressed[crc_start] ^= 0xFF;

            let err = bounded_gzip_decompress(&compressed, 1024)
                .expect_err("a CRC-32 mismatch must be rejected even when the length matches");
            assert!(
                err.to_string().contains("CRC-32 mismatch"),
                "unexpected error: {err}"
            );
        }

        #[test]
        fn bounded_deflate_decompress_round_trips_small_payload() {
            let plaintext = b"round trip me too";
            let compressed = oxiarc_deflate::zlib_compress(plaintext, 6).expect("zlib_compress");
            let out = bounded_deflate_decompress(&compressed, 1024).expect("decompress");
            assert_eq!(out.as_ref(), plaintext);
        }
    }
}
