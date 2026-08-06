//! Multipart form-data body builder (RFC 7578).
//!
//! Provides [`MultipartBuilder`] for constructing `multipart/form-data` bodies
//! and [`Part`] for individual MIME parts.

#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::{BufMut, Bytes, BytesMut};
use futures_core::Stream;

use crate::{Body, OxiHttpError};

static BOUNDARY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique boundary string using nanosecond timestamp + atomic counter.
fn generate_boundary() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let counter = BOUNDARY_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("----OxiHTTPBoundary{nanos:08x}{counter:04x}")
}

/// Neutralize characters that would let a caller-supplied form-data
/// parameter (a field `name`, `filename`, or `content_type`) break out of
/// its position in a serialized multipart header line.
///
/// Both hazards addressed here are realistic for `filename` and
/// `content_type` in particular: both are commonly sourced from
/// attacker-influenced input (e.g. a browser-reported upload filename or
/// MIME type forwarded verbatim by a calling application).
///
/// - A literal `"` would terminate the quoted-string parameter early
///   (relevant to `name`/`filename`, which are embedded in
///   `Content-Disposition` as a `quoted-string`), letting the remainder of
///   the caller's value be interpreted as additional `Content-Disposition`
///   parameters. Backslash-escaped per the `quoted-string` grammar (RFC
///   9110 §5.6.4) — and a literal `\` is escaped for the same reason (it
///   would otherwise be read as escaping whatever follows it). `escape_quotes`
///   selects whether this rule applies; it does not for the unquoted
///   `Content-Type` value.
/// - A literal CR or LF has no valid representation inside an HTTP header
///   field value at all (RFC 9110 §5.5 forbids raw CR/LF in a field value).
///   Left unescaped, either would let a caller-controlled value inject an
///   arbitrary extra header line — or, choosing the bytes carefully, a
///   forged part boundary — into the multipart body. Stripped rather than
///   escaped: there is no valid escape for a raw control character here,
///   and the surrounding quoted-string / header line stays well-formed
///   with the stripped text collapsed into inert inline content.
fn sanitize_header_param(value: &str, escape_quotes: bool) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '"' if escape_quotes => out.push_str("\\\""),
            '\\' if escape_quotes => out.push_str("\\\\"),
            '\r' | '\n' => {} // no valid escape for a raw control char in a header value
            _ => out.push(c),
        }
    }
    out
}

/// A single MIME part in a multipart body.
///
/// Parts consist of headers (name-value pairs) and a binary body.
#[derive(Debug, Clone)]
pub struct Part {
    headers: Vec<(String, String)>,
    body: Bytes,
}

impl Part {
    /// Create a text part with a `Content-Disposition: form-data; name=...` header.
    ///
    /// Per RFC 7578, text fields do not need an explicit `Content-Type`; the
    /// receiver treats them as `text/plain`.
    ///
    /// # Header safety
    ///
    /// `name` is sanitized before being embedded in the `Content-Disposition`
    /// header: a literal `"` or `\` is backslash-escaped, and a literal CR or
    /// LF is stripped (there is no valid escape for a raw control character
    /// in a header value). This matters because `name` — and, for
    /// [`file`](Self::file), `filename` and `content_type` — routinely
    /// originates from caller/user-controlled data (e.g. an uploaded file's
    /// original filename); left unsanitized, it could otherwise break out of
    /// the header line and inject an extra header or a forged part boundary.
    /// See [`custom`](Self::custom) for a raw, unsanitized escape hatch.
    pub fn text(name: &str, value: impl Into<String>) -> Self {
        Self {
            headers: vec![(
                "Content-Disposition".into(),
                format!("form-data; name=\"{}\"", sanitize_header_param(name, true)),
            )],
            body: Bytes::from(value.into()),
        }
    }

    /// Create a file/binary part with `Content-Disposition` and `Content-Type` headers.
    ///
    /// # Header safety
    ///
    /// `name`, `filename`, and `content_type` are all sanitized before being
    /// embedded in their respective headers — see [`text`](Self::text)'s
    /// "Header safety" section for what that means and why.
    pub fn file(name: &str, filename: &str, content_type: &str, body: impl Into<Bytes>) -> Self {
        Self {
            headers: vec![
                (
                    "Content-Disposition".into(),
                    format!(
                        "form-data; name=\"{}\"; filename=\"{}\"",
                        sanitize_header_param(name, true),
                        sanitize_header_param(filename, true)
                    ),
                ),
                (
                    "Content-Type".into(),
                    sanitize_header_param(content_type, false),
                ),
            ],
            body: body.into(),
        }
    }

    /// Create a part with fully custom headers and body.
    ///
    /// Unlike [`text`](Self::text) and [`file`](Self::file), the supplied
    /// `headers` are used verbatim — **not** sanitized. This is the
    /// escape hatch for callers who need full control over the header
    /// lines (including, deliberately, ones [`text`]/[`file`] would refuse
    /// to produce); it is the caller's responsibility to ensure `headers`
    /// contains no unintended CR/LF or unbalanced quoting if any part of it
    /// is not fully trusted.
    ///
    /// [`text`]: Self::text
    /// [`file`]: Self::file
    pub fn custom(headers: Vec<(String, String)>, body: impl Into<Bytes>) -> Self {
        Self {
            headers,
            body: body.into(),
        }
    }
}

/// Builder for `multipart/form-data` bodies per RFC 7578.
///
/// # Example
///
/// ```rust
/// use oxihttp_core::multipart::MultipartBuilder;
///
/// let builder = MultipartBuilder::new()
///     .add_text("username", "alice")
///     .add_file("avatar", "pic.png", "image/png", b"PNG\r\n".as_ref());
///
/// let content_type = builder.content_type();
/// let body_bytes = builder.build();
/// ```
#[derive(Debug, Clone)]
pub struct MultipartBuilder {
    boundary: String,
    parts: Vec<Part>,
}

impl MultipartBuilder {
    /// Create a new builder with an auto-generated boundary.
    pub fn new() -> Self {
        Self {
            boundary: generate_boundary(),
            parts: Vec::new(),
        }
    }

    /// Return the boundary string (without leading `--`).
    pub fn boundary(&self) -> &str {
        &self.boundary
    }

    /// Return the full `Content-Type` header value including the boundary parameter.
    ///
    /// Set this as the `Content-Type` header when sending the body.
    pub fn content_type(&self) -> String {
        format!("multipart/form-data; boundary={}", self.boundary)
    }

    /// Add a text field part.
    ///
    /// `name` is sanitized for header safety — see [`Part::text`]'s "Header
    /// safety" section.
    pub fn add_text(mut self, name: &str, value: impl Into<String>) -> Self {
        self.parts.push(Part::text(name, value));
        self
    }

    /// Add a file/binary part.
    ///
    /// `name`, `filename`, and `content_type` are sanitized for header
    /// safety — see [`Part::file`]'s "Header safety" section.
    pub fn add_file(
        mut self,
        name: &str,
        filename: &str,
        content_type: &str,
        body: impl Into<Bytes>,
    ) -> Self {
        self.parts
            .push(Part::file(name, filename, content_type, body));
        self
    }

    /// Add a pre-constructed [`Part`].
    pub fn add_part(mut self, part: Part) -> Self {
        self.parts.push(part);
        self
    }

    /// Add a part whose body is produced lazily by an async byte stream,
    /// rather than a `Bytes` buffer that must already be fully resident in
    /// memory — the "zero-copy large upload" path (e.g. streaming a file
    /// straight from disk or network without ever holding the whole thing
    /// in memory at once).
    ///
    /// Because a streamed part cannot be serialized synchronously (see
    /// [`StreamingMultipart`]), calling this method **transitions** the
    /// builder to a [`StreamingMultipart`], which offers
    /// [`build_stream`](StreamingMultipart::build_stream) in place of
    /// [`build`](Self::build). This makes "tried to synchronously `build()`
    /// a builder holding a streamed part" a compile-time impossibility
    /// rather than a silent gap or a runtime error. All parts already added
    /// to `self` (and any added afterward via
    /// [`StreamingMultipart::add_text`] / `add_file` / `add_stream_part`)
    /// are preserved in order.
    ///
    /// # Example
    ///
    /// `file_chunks()` below stands in for any `Stream<Item =
    /// Result<Bytes, OxiHttpError>> + Send + 'static` — e.g. one built by
    /// reading a file in fixed-size chunks — that never materializes the
    /// whole part body in memory at once.
    ///
    /// ```rust,ignore
    /// use oxihttp_core::MultipartBuilder;
    ///
    /// let streaming = MultipartBuilder::new()
    ///     .add_text("title", "vacation photo")
    ///     .add_stream_part(
    ///         vec![
    ///             ("Content-Disposition".into(), "form-data; name=\"file\"; filename=\"a.bin\"".into()),
    ///             ("Content-Type".into(), "application/octet-stream".into()),
    ///         ],
    ///         file_chunks(),
    ///     );
    /// let content_type = streaming.content_type();
    /// let body = streaming.build_stream();
    /// ```
    pub fn add_stream_part(
        self,
        headers: Vec<(String, String)>,
        body: impl Stream<Item = std::result::Result<Bytes, OxiHttpError>> + Send + 'static,
    ) -> StreamingMultipart {
        StreamingMultipart {
            boundary: self.boundary,
            parts: self
                .parts
                .into_iter()
                .map(PartOrStream::Buffered)
                .chain(std::iter::once(PartOrStream::Streamed(
                    headers,
                    Box::pin(body),
                )))
                .collect(),
        }
    }

    /// Add a file/binary part whose body is produced lazily by an async
    /// byte stream. Convenience wrapper over
    /// [`add_stream_part`](Self::add_stream_part) that builds the same
    /// `Content-Disposition` / `Content-Type` headers as
    /// [`add_file`](Self::add_file), **including the same sanitization** of
    /// `name`, `filename`, and `content_type` — see [`Part::file`]'s
    /// "Header safety" section. See `add_stream_part` for why this returns a
    /// [`StreamingMultipart`] rather than `Self`.
    pub fn add_file_stream(
        self,
        name: &str,
        filename: &str,
        content_type: &str,
        body: impl Stream<Item = std::result::Result<Bytes, OxiHttpError>> + Send + 'static,
    ) -> StreamingMultipart {
        let headers = vec![
            (
                "Content-Disposition".into(),
                format!(
                    "form-data; name=\"{}\"; filename=\"{}\"",
                    sanitize_header_param(name, true),
                    sanitize_header_param(filename, true)
                ),
            ),
            (
                "Content-Type".into(),
                sanitize_header_param(content_type, false),
            ),
        ];
        self.add_stream_part(headers, body)
    }

    /// Serialise to a [`Bytes`] buffer containing the complete multipart wire format.
    ///
    /// Automatically handles boundary collision: if the boundary string occurs literally
    /// inside any part body, a numeric suffix is appended and the check repeats until
    /// the boundary is guaranteed unique across all part bodies.
    ///
    /// Note: the uniqueness check searches for the bare boundary string (conservative —
    /// no false negatives). The actual wire delimiter is `--<boundary>`, but matching
    /// the bare string is safe because any occurrence of the bare string would also
    /// produce a collision in the delimiter form.
    pub fn build(self) -> Bytes {
        let boundary = self.find_unique_boundary();
        let dash_boundary = format!("--{boundary}");
        let final_boundary = format!("--{boundary}--\r\n");

        let mut buf = BytesMut::new();

        for part in &self.parts {
            buf.put_slice(dash_boundary.as_bytes());
            buf.put_slice(b"\r\n");
            for (k, v) in &part.headers {
                buf.put_slice(k.as_bytes());
                buf.put_slice(b": ");
                buf.put_slice(v.as_bytes());
                buf.put_slice(b"\r\n");
            }
            buf.put_slice(b"\r\n");
            buf.put_slice(&part.body);
            buf.put_slice(b"\r\n");
        }
        buf.put_slice(final_boundary.as_bytes());
        buf.freeze()
    }

    /// Find a boundary string guaranteed not to occur in any part body.
    fn find_unique_boundary(&self) -> String {
        let mut boundary = self.boundary.clone();
        let mut suffix = 0u32;
        loop {
            let has_collision = self.parts.iter().any(|p| {
                p.body
                    .windows(boundary.len())
                    .any(|w| w == boundary.as_bytes())
            });
            if !has_collision {
                return boundary;
            }
            suffix += 1;
            boundary = format!("{}{suffix:04x}", self.boundary);
        }
    }
}

impl Default for MultipartBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// StreamingMultipart — zero-copy large-upload path
// ---------------------------------------------------------------------------

/// One part of a [`StreamingMultipart`] body: either a pre-built, in-memory
/// [`Part`] or a "streamed" part whose body is produced lazily by an async
/// byte stream.
enum PartOrStream {
    /// An in-memory part, identical to what [`MultipartBuilder`] holds.
    Buffered(Part),
    /// A streamed part: headers are known up front, but the body is only
    /// read (and emitted) chunk-by-chunk as the returned [`Body`] is
    /// polled — see [`MultipartBuilder::add_stream_part`].
    Streamed(
        Vec<(String, String)>,
        Pin<Box<dyn Stream<Item = std::result::Result<Bytes, OxiHttpError>> + Send>>,
    ),
}

/// A [`MultipartBuilder`] that has gained at least one *streamed* part (via
/// [`MultipartBuilder::add_stream_part`] / `add_file_stream`).
///
/// `MultipartBuilder::build()` is synchronous and returns a single `Bytes`
/// buffer, which requires every part's body to already be fully resident in
/// memory — fundamentally incompatible with a lazily-produced (streamed)
/// part. Rather than make `build()` fallible (a breaking change rippling
/// through every existing caller) or silently drop/mis-serialize streamed
/// parts, adding one changes the builder's *type* to `StreamingMultipart`,
/// whose only serialization method is
/// [`build_stream`](Self::build_stream). This makes "synchronously
/// `build()`-ing a builder that holds a streamed part" a compile-time
/// impossibility instead of a silent runtime gap.
///
/// # Wired into `oxihttp-client` via `multipart_stream`
///
/// [`build_stream`](Self::build_stream) produces an [`Body::Stream`],
/// consumable by anything that accepts an `http_body::Body` (see that
/// method's doc comment for how to drive it directly). `oxihttp-client`'s
/// `RequestBuilder::multipart_stream()` accepts a `StreamingMultipart`
/// directly and sends it to the wire without buffering — unlike
/// `RequestBuilder::multipart()`, which only ever calls the plain
/// [`MultipartBuilder::build`] and buffers the full request body. Because
/// the underlying stream is one-shot (cannot be cloned or replayed), a
/// `multipart_stream` request bypasses the client's retry policy and
/// cannot follow a body-preserving (307/308) redirect — see
/// `RequestBuilder::multipart_stream`'s doc comment in `oxihttp-client`
/// for the specifics.
pub struct StreamingMultipart {
    boundary: String,
    parts: Vec<PartOrStream>,
}

impl std::fmt::Debug for StreamingMultipart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamingMultipart")
            .field("boundary", &self.boundary)
            .field("part_count", &self.parts.len())
            .finish()
    }
}

impl StreamingMultipart {
    /// Return the boundary string (without leading `--`). See
    /// [`MultipartBuilder::boundary`].
    pub fn boundary(&self) -> &str {
        &self.boundary
    }

    /// Return the full `Content-Type` header value including the boundary
    /// parameter. See [`MultipartBuilder::content_type`].
    pub fn content_type(&self) -> String {
        format!("multipart/form-data; boundary={}", self.boundary)
    }

    /// Add a text field part (in-memory). See
    /// [`MultipartBuilder::add_text`].
    pub fn add_text(mut self, name: &str, value: impl Into<String>) -> Self {
        self.parts
            .push(PartOrStream::Buffered(Part::text(name, value)));
        self
    }

    /// Add a file/binary part (in-memory). See
    /// [`MultipartBuilder::add_file`].
    pub fn add_file(
        mut self,
        name: &str,
        filename: &str,
        content_type: &str,
        body: impl Into<Bytes>,
    ) -> Self {
        self.parts.push(PartOrStream::Buffered(Part::file(
            name,
            filename,
            content_type,
            body,
        )));
        self
    }

    /// Add a pre-constructed in-memory [`Part`]. See
    /// [`MultipartBuilder::add_part`].
    pub fn add_part(mut self, part: Part) -> Self {
        self.parts.push(PartOrStream::Buffered(part));
        self
    }

    /// Add another part whose body is produced lazily by an async byte
    /// stream. See [`MultipartBuilder::add_stream_part`].
    ///
    /// # Boundary-collision detection
    ///
    /// [`build`](MultipartBuilder::build) (and the in-memory parts of
    /// [`build_stream`](Self::build_stream)) scan every in-memory part body
    /// for an accidental literal occurrence of the boundary string and pick
    /// a new boundary if found. A streamed part's body is opaque — reading
    /// it to scan it would defeat the purpose of streaming — so it is
    /// **not** included in that check; this is a documented, accepted
    /// trade-off (the boundary already embeds a nanosecond timestamp and a
    /// monotonic counter, and other multipart implementations such as
    /// `reqwest` make the same trade-off for streamed bodies).
    pub fn add_stream_part(
        mut self,
        headers: Vec<(String, String)>,
        body: impl Stream<Item = std::result::Result<Bytes, OxiHttpError>> + Send + 'static,
    ) -> Self {
        self.parts
            .push(PartOrStream::Streamed(headers, Box::pin(body)));
        self
    }

    /// Add a file/binary part whose body is produced lazily by an async
    /// byte stream. See [`MultipartBuilder::add_file_stream`].
    pub fn add_file_stream(
        self,
        name: &str,
        filename: &str,
        content_type: &str,
        body: impl Stream<Item = std::result::Result<Bytes, OxiHttpError>> + Send + 'static,
    ) -> Self {
        let headers = vec![
            (
                "Content-Disposition".into(),
                format!(
                    "form-data; name=\"{}\"; filename=\"{}\"",
                    sanitize_header_param(name, true),
                    sanitize_header_param(filename, true)
                ),
            ),
            (
                "Content-Type".into(),
                sanitize_header_param(content_type, false),
            ),
        ];
        self.add_stream_part(headers, body)
    }

    /// Find a boundary string guaranteed not to occur in any *in-memory*
    /// part body. Mirrors [`MultipartBuilder::find_unique_boundary`]; see
    /// [`add_stream_part`](Self::add_stream_part) for why streamed part
    /// bodies are excluded from the scan.
    fn find_unique_boundary(&self) -> String {
        let mut boundary = self.boundary.clone();
        let mut suffix = 0u32;
        loop {
            let has_collision = self.parts.iter().any(|p| match p {
                PartOrStream::Buffered(part) => part
                    .body
                    .windows(boundary.len())
                    .any(|w| w == boundary.as_bytes()),
                PartOrStream::Streamed(..) => false,
            });
            if !has_collision {
                return boundary;
            }
            suffix += 1;
            boundary = format!("{}{suffix:04x}", self.boundary);
        }
    }

    /// Serialize this builder into a lazily-produced [`Body`]
    /// (`Body::Stream`) instead of one concatenated `Bytes` buffer.
    ///
    /// Each part's boundary marker, headers, and body are emitted as
    /// separate stream chunks — never concatenated into a single buffer —
    /// so peak memory is bounded by the largest single in-memory part (or,
    /// for a streamed part, by whatever chunk size that stream itself
    /// yields) rather than the sum of every part's size.
    ///
    /// The returned `Body` is consumed like any other `Body::Stream`: call
    /// [`Body::into_pinned`] to get a [`PinnedBody`](crate::PinnedBody),
    /// which implements `http_body::Body` (`poll_frame`, driven by a real
    /// HTTP/1 or HTTP/2 connection, or manually via
    /// `std::future::poll_fn` — see this crate's
    /// `multipart::streaming_tests::collect_body_frames` test helper for a
    /// minimal example). `oxihttp-client`'s `RequestBuilder::multipart_stream()`
    /// accepts a whole [`StreamingMultipart`] directly (calling
    /// `build_stream` internally) and drives it against the wire without
    /// buffering — see that method's doc comment in `oxihttp-client`.
    pub fn build_stream(self) -> Body {
        let boundary = self.find_unique_boundary();
        let dash_boundary = format!("--{boundary}");
        let final_boundary = format!("--{boundary}--\r\n");

        let mut queue: VecDeque<PendingChunk> = VecDeque::new();
        for part in self.parts {
            match part {
                PartOrStream::Buffered(p) => {
                    queue.push_back(PendingChunk::Ready(part_preamble(
                        &dash_boundary,
                        &p.headers,
                    )));
                    queue.push_back(PendingChunk::Ready(p.body));
                    queue.push_back(PendingChunk::Ready(Bytes::from_static(b"\r\n")));
                }
                PartOrStream::Streamed(headers, stream) => {
                    queue.push_back(PendingChunk::Ready(part_preamble(&dash_boundary, &headers)));
                    queue.push_back(PendingChunk::Nested(stream));
                    queue.push_back(PendingChunk::Ready(Bytes::from_static(b"\r\n")));
                }
            }
        }
        queue.push_back(PendingChunk::Ready(Bytes::from(final_boundary)));

        Body::stream(Box::pin(MultipartStream { queue }))
    }
}

/// Render `--boundary\r\n<headers>\r\n\r\n` (the fixed preamble preceding
/// every part's body) as a single `Bytes` chunk.
fn part_preamble(dash_boundary: &str, headers: &[(String, String)]) -> Bytes {
    let mut buf = BytesMut::new();
    buf.put_slice(dash_boundary.as_bytes());
    buf.put_slice(b"\r\n");
    for (k, v) in headers {
        buf.put_slice(k.as_bytes());
        buf.put_slice(b": ");
        buf.put_slice(v.as_bytes());
        buf.put_slice(b"\r\n");
    }
    buf.put_slice(b"\r\n");
    buf.freeze()
}

/// One item still waiting to be emitted by a [`MultipartStream`]: either an
/// already-available `Bytes` chunk, or a nested (streamed-part) source that
/// must itself be polled to produce more chunks.
enum PendingChunk {
    Ready(Bytes),
    Nested(Pin<Box<dyn Stream<Item = std::result::Result<Bytes, OxiHttpError>> + Send>>),
}

/// The [`Stream`] backing [`StreamingMultipart::build_stream`]'s [`Body`].
///
/// Drains a queue of [`PendingChunk`]s in order. `Ready` chunks are
/// returned immediately (an empty one is skipped rather than yielding a
/// spurious zero-byte frame); a `Nested` streamed part is polled
/// repeatedly, forwarding each of *its* chunks in turn, until it is
/// exhausted — at no point are two adjacent chunks concatenated into one
/// buffer.
struct MultipartStream {
    queue: VecDeque<PendingChunk>,
}

impl Stream for MultipartStream {
    type Item = std::result::Result<Bytes, OxiHttpError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // `MultipartStream` has no self-referential fields (the pinning
        // guarantee each nested stream needs is already carried by its own
        // `Pin<Box<..>>`), so it is `Unpin` and `get_mut` is safe — no
        // `unsafe` needed despite this crate's `#![forbid(unsafe_code)]`.
        let this = self.get_mut();
        loop {
            let front = match this.queue.pop_front() {
                None => return Poll::Ready(None),
                Some(f) => f,
            };
            match front {
                PendingChunk::Ready(b) => {
                    if b.is_empty() {
                        continue;
                    }
                    return Poll::Ready(Some(Ok(b)));
                }
                PendingChunk::Nested(mut s) => match s.as_mut().poll_next(cx) {
                    Poll::Ready(Some(Ok(chunk))) => {
                        // Not yet exhausted — put it back at the front so
                        // the next poll resumes exactly where this left off.
                        this.queue.push_front(PendingChunk::Nested(s));
                        if chunk.is_empty() {
                            continue;
                        }
                        return Poll::Ready(Some(Ok(chunk)));
                    }
                    Poll::Ready(Some(Err(e))) => {
                        // Propagate the error; do not put `s` back — a
                        // stream that has yielded an error is not polled
                        // again.
                        return Poll::Ready(Some(Err(e)));
                    }
                    Poll::Ready(None) => continue, // exhausted — advance to the next chunk
                    Poll::Pending => {
                        this.queue.push_front(PendingChunk::Nested(s));
                        return Poll::Pending;
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_builder() {
        let builder = MultipartBuilder::new();
        let ct = builder.content_type();
        assert!(ct.starts_with("multipart/form-data; boundary="));
        let bytes = builder.build();
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(s.contains("----OxiHTTPBoundary"));
        assert!(s.ends_with("--\r\n"));
    }

    #[test]
    fn test_text_field() {
        let bytes = MultipartBuilder::new()
            .add_text("field1", "hello world")
            .build();
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(s.contains("name=\"field1\""));
        assert!(s.contains("hello world"));
    }

    #[test]
    fn test_file_part() {
        let bytes = MultipartBuilder::new()
            .add_file("upload", "test.txt", "text/plain", "file contents")
            .build();
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(s.contains("filename=\"test.txt\""));
        assert!(s.contains("Content-Type: text/plain"));
        assert!(s.contains("file contents"));
    }

    #[test]
    fn test_mixed_parts() {
        let bytes = MultipartBuilder::new()
            .add_text("name", "Alice")
            .add_file("avatar", "pic.png", "image/png", b"\x89PNG\r\n".as_ref())
            .build();
        // The raw bytes may not be valid UTF-8 (PNG magic bytes), so search byte-by-byte.
        let bytes_vec = bytes.to_vec();
        let header_section = &bytes_vec[..];
        // The headers and text fields are valid ASCII; convert the header region for inspection.
        // We search for the known ASCII patterns in the byte slice directly.
        assert!(
            bytes_vec
                .windows(b"name=\"name\"".len())
                .any(|w| w == b"name=\"name\""),
            "missing name field"
        );
        assert!(
            bytes_vec.windows(b"Alice".len()).any(|w| w == b"Alice"),
            "missing Alice"
        );
        assert!(
            header_section
                .windows(b"filename=\"pic.png\"".len())
                .any(|w| w == b"filename=\"pic.png\""),
            "missing filename"
        );
    }

    #[test]
    fn test_boundary_collision_resolved() {
        let mut builder = MultipartBuilder::new();
        // Inject the boundary string literally into a part body.
        let boundary_clone = builder.boundary().to_owned();
        builder = builder.add_text("field", boundary_clone.as_str());
        // build() must resolve the collision and produce valid output.
        let bytes = builder.build();
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        // Final boundary marker must be present.
        assert!(s.ends_with("--\r\n"));
    }

    #[test]
    fn test_content_type_header() {
        let b = MultipartBuilder::new();
        let ct = b.content_type();
        let bnd = b.boundary().to_owned();
        assert_eq!(ct, format!("multipart/form-data; boundary={bnd}"));
    }

    #[test]
    fn test_crlf_format() {
        let bytes = MultipartBuilder::new().add_text("x", "y").build();
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        // Headers end in CRLF; blank line (CRLF) before body.
        assert!(s.contains("\r\n\r\n"));
        // Part body ends in CRLF before next boundary.
        assert!(s.contains("y\r\n"));
    }

    #[test]
    fn test_boundary_collision_unique() {
        let mut b = MultipartBuilder::new();
        let bnd = b.boundary().to_owned();
        let body_with_boundary = format!("some text {bnd} more text");
        b = b.add_text("collision_field", &body_with_boundary);
        let bytes = b.build();
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(s.ends_with("--\r\n"));
    }

    #[test]
    fn test_custom_part() {
        let bytes = MultipartBuilder::new()
            .add_part(Part::custom(
                vec![
                    (
                        "Content-Disposition".into(),
                        "form-data; name=\"raw\"".into(),
                    ),
                    ("X-Custom".into(), "header-value".into()),
                ],
                Bytes::from("raw body"),
            ))
            .build();
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(s.contains("X-Custom: header-value"));
        assert!(s.contains("raw body"));
    }

    // -----------------------------------------------------------------------
    // Content-Disposition / Content-Type parameter injection hardening
    // -----------------------------------------------------------------------

    /// A literal `"` in a field `name` must be backslash-escaped, not break
    /// out of the `quoted-string` and let the rest of the value be read as
    /// extra `Content-Disposition` parameters.
    #[test]
    fn test_name_with_quote_is_escaped() {
        let bytes = MultipartBuilder::new().add_text("na\"me", "value").build();
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            s.contains("name=\"na\\\"me\""),
            "a literal quote in the field name must be backslash-escaped: {s:?}"
        );
    }

    /// A literal `\` in a `filename` must itself be escaped (`\\`) so it is
    /// not read as escaping the character that follows it.
    #[test]
    fn test_filename_with_backslash_is_escaped() {
        let bytes = MultipartBuilder::new()
            .add_file("f", "C:\\evil.txt", "text/plain", b"data".as_ref())
            .build();
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            s.contains("filename=\"C:\\\\evil.txt\""),
            "a literal backslash in filename must be escaped: {s:?}"
        );
    }

    /// A `filename` carrying embedded CRLF sequences (e.g. attempting to
    /// inject an extra header line, or even a forged boundary) must not
    /// change the physical line structure of the serialized part: the CR/LF
    /// bytes are stripped, collapsing the attempted injection into inert
    /// inline text within the existing `filename` parameter rather than a
    /// new header line or boundary marker.
    #[test]
    fn test_filename_crlf_injection_does_not_add_header_lines() {
        let benign = MultipartBuilder::new()
            .add_file("f", "benign.txt", "text/plain", b"data".as_ref())
            .build();
        let benign_line_count = String::from_utf8(benign.to_vec())
            .unwrap()
            .matches("\r\n")
            .count();

        let malicious_filename = "evil.txt\r\nX-Injected: header-injection\r\n--fakeboundary--";
        let malicious = MultipartBuilder::new()
            .add_file("f", malicious_filename, "text/plain", b"data".as_ref())
            .build();
        let s = String::from_utf8(malicious.to_vec()).unwrap();
        let malicious_line_count = s.matches("\r\n").count();

        assert_eq!(
            benign_line_count, malicious_line_count,
            "a CRLF-laced filename must not change the number of physical lines in the \
             serialized body — a different count means it injected an extra header or \
             boundary line: {s:?}"
        );
        assert!(
            !s.lines().any(|l| l.starts_with("X-Injected")),
            "CRLF injection must not produce a standalone header line: {s:?}"
        );
        assert!(
            s.contains("X-Injected: header-injection--fakeboundary--"),
            "the stripped text must still be present, inertly, inside the filename \
             parameter (stripping is not silent data loss): {s:?}"
        );
    }

    /// The same CRLF-injection hazard applies to `content_type` (unquoted,
    /// so only the CR/LF-stripping rule applies — there is no quoting to
    /// escape).
    #[test]
    fn test_content_type_crlf_is_stripped() {
        let malicious_ct = "text/plain\r\nX-Injected: evil";
        let bytes = MultipartBuilder::new()
            .add_file("f", "file.txt", malicious_ct, b"data".as_ref())
            .build();
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            !s.lines().any(|l| l.starts_with("X-Injected")),
            "CRLF in content_type must not create a new header line: {s:?}"
        );
        assert!(
            s.contains("Content-Type: text/plainX-Injected: evil"),
            "the stripped text must still be present, inertly, on the Content-Type line: {s:?}"
        );
    }

    /// `Part::custom` is the documented escape hatch for fully custom,
    /// caller-controlled headers and must remain unsanitized — the
    /// sanitizer only applies to the ergonomic `text`/`file` constructors.
    #[test]
    fn test_custom_part_headers_remain_unsanitized() {
        let bytes = MultipartBuilder::new()
            .add_part(Part::custom(
                vec![("X-Raw".into(), "no\"escaping\"here".into())],
                Bytes::from("body"),
            ))
            .build();
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(s.contains("X-Raw: no\"escaping\"here"));
    }
}

// ---------------------------------------------------------------------------
// StreamingMultipart tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod streaming_tests {
    use super::*;
    use http_body::Body as HttpBody;

    /// A trivial test-only `Stream` that yields pre-built chunks in order,
    /// then ends — stands in for a real chunked byte source (e.g. a file
    /// read in fixed-size pieces) without pulling in `futures-util`.
    struct VecStream(VecDeque<std::result::Result<Bytes, OxiHttpError>>);

    impl VecStream {
        fn of(chunks: impl IntoIterator<Item = &'static [u8]>) -> Self {
            Self(
                chunks
                    .into_iter()
                    .map(|c| Ok(Bytes::from_static(c)))
                    .collect(),
            )
        }
    }

    impl Stream for VecStream {
        type Item = std::result::Result<Bytes, OxiHttpError>;
        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Ready(self.get_mut().0.pop_front())
        }
    }

    /// Drive any [`Body`] (`Empty`/`Full`/`Stream`) to completion via the
    /// public [`http_body::Body`] interface — the same interface hyper
    /// itself uses — concatenating every data frame. Also returns the raw,
    /// per-frame chunk boundaries (before concatenation) so tests can
    /// assert chunks were forwarded individually rather than merged.
    async fn collect_body_frames(body: Body) -> std::result::Result<Vec<Bytes>, OxiHttpError> {
        let mut pinned = Box::pin(body.into_pinned());
        let mut frames = Vec::new();
        loop {
            let frame = std::future::poll_fn(|cx| pinned.as_mut().poll_frame(cx)).await;
            match frame {
                None => return Ok(frames),
                Some(Err(e)) => return Err(e),
                Some(Ok(f)) => {
                    if let Ok(data) = f.into_data() {
                        frames.push(data);
                    }
                }
            }
        }
    }

    async fn collect_body(body: Body) -> std::result::Result<Vec<u8>, OxiHttpError> {
        let frames = collect_body_frames(body).await?;
        let mut out = Vec::new();
        for f in frames {
            out.extend_from_slice(&f);
        }
        Ok(out)
    }

    /// `build_stream()` on a builder mixing an in-memory field, a streamed
    /// file part, and a second in-memory field must produce a well-formed
    /// multipart body with every part present, correctly delimited, and in
    /// insertion order.
    #[tokio::test]
    async fn build_stream_produces_correct_wire_format_with_mixed_parts() {
        let streaming = MultipartBuilder::new()
            .add_text("title", "vacation photo")
            .add_stream_part(
                vec![
                    (
                        "Content-Disposition".into(),
                        "form-data; name=\"file\"; filename=\"a.bin\"".into(),
                    ),
                    ("Content-Type".into(), "application/octet-stream".into()),
                ],
                VecStream::of([b"chunk-one-".as_slice(), b"chunk-two".as_slice()]),
            )
            .add_text("caption", "sunset");

        let content_type = streaming.content_type();
        assert!(content_type.starts_with("multipart/form-data; boundary="));

        let body = streaming.build_stream();
        let bytes = collect_body(body).await.expect("collect");
        let s = String::from_utf8(bytes).expect("utf8");

        // All three parts present, in order.
        let title_pos = s.find("name=\"title\"").expect("title header");
        let file_pos = s.find("filename=\"a.bin\"").expect("file header");
        let caption_pos = s.find("name=\"caption\"").expect("caption header");
        assert!(
            title_pos < file_pos && file_pos < caption_pos,
            "parts must appear in insertion order"
        );

        assert!(s.contains("vacation photo"));
        // The two streamed chunks must appear, concatenated in the body
        // text (the multipart *wire format* concatenates a part's bytes;
        // what must NOT happen is the *serializer* pre-concatenating them
        // before the stream is polled — see the next test).
        assert!(s.contains("chunk-one-chunk-two"));
        assert!(s.contains("sunset"));
        assert!(s.ends_with("--\r\n"));
    }

    /// The defining property of `build_stream`: a streamed part's chunks
    /// must be forwarded to the output `Body` as *separate* frames, never
    /// pre-concatenated by the serializer into one buffer. This is what
    /// makes "zero-copy large upload" true rather than aspirational.
    #[tokio::test]
    async fn build_stream_forwards_streamed_chunks_without_concatenating() {
        let streaming = MultipartBuilder::new().add_stream_part(
            vec![("Content-Disposition".into(), "form-data; name=\"f\"".into())],
            VecStream::of([b"AAAA".as_slice(), b"BBBB".as_slice(), b"CCCC".as_slice()]),
        );
        let frames = collect_body_frames(streaming.build_stream())
            .await
            .expect("collect frames");

        let as_strings: Vec<String> = frames
            .iter()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        assert!(
            as_strings.iter().any(|f| f == "AAAA"),
            "expected a standalone 'AAAA' frame, got: {as_strings:?}"
        );
        assert!(
            as_strings.iter().any(|f| f == "BBBB"),
            "expected a standalone 'BBBB' frame, got: {as_strings:?}"
        );
        assert!(
            as_strings.iter().any(|f| f == "CCCC"),
            "expected a standalone 'CCCC' frame, got: {as_strings:?}"
        );
        // In particular, no single frame may already contain more than one
        // of the source chunks pre-joined.
        assert!(
            as_strings
                .iter()
                .all(|f| !f.contains("AAAABBBB") && !f.contains("BBBBCCCC")),
            "streamed chunks must not be pre-concatenated by the serializer: {as_strings:?}"
        );
    }

    /// A streamed part's body legitimately containing the literal boundary
    /// string is a documented, accepted limitation (see
    /// `add_stream_part`'s docs): unlike in-memory parts, it is not
    /// scanned, so it must not perturb the chosen boundary. This test
    /// pins that documented behavior rather than merely hoping for it.
    #[tokio::test]
    async fn find_unique_boundary_does_not_scan_streamed_part_bodies() {
        let streaming = MultipartBuilder::new();
        let original_boundary = streaming.boundary().to_owned();

        // The streamed chunk literally contains the boundary string, which
        // *would* trigger a collision-avoidance suffix if this were an
        // in-memory part (see `test_boundary_collision_resolved`).
        let boundary_bytes = original_boundary.clone().into_bytes();
        let streaming = streaming.add_stream_part(
            vec![("Content-Disposition".into(), "form-data; name=\"f\"".into())],
            VecStream::of([Box::leak(boundary_bytes.into_boxed_slice()) as &'static [u8]]),
        );

        let bytes = collect_body(streaming.build_stream())
            .await
            .expect("collect");
        let s = String::from_utf8_lossy(&bytes);
        // The *unmodified* boundary (no numeric suffix) must delimit parts.
        assert!(s.contains(&format!("--{original_boundary}\r\n")));
        assert!(s.ends_with(&format!("--{original_boundary}--\r\n")));
    }

    /// An in-memory part added *around* a streamed part still participates
    /// in boundary-collision detection — only streamed bodies are exempt.
    #[tokio::test]
    async fn find_unique_boundary_still_scans_buffered_parts_around_a_stream() {
        let builder = MultipartBuilder::new();
        let original_boundary = builder.boundary().to_owned();

        let streaming = builder
            .add_text("colliding", &original_boundary)
            .add_stream_part(
                vec![("Content-Disposition".into(), "form-data; name=\"f\"".into())],
                VecStream::of([b"harmless".as_slice()]),
            );

        let bytes = collect_body(streaming.build_stream())
            .await
            .expect("collect");
        let s = String::from_utf8_lossy(&bytes);
        // A numeric suffix must have been appended, exactly as
        // `test_boundary_collision_resolved` asserts for `build()`.
        assert!(s.ends_with("--\r\n"));
        assert!(!s.contains(&format!("--{original_boundary}\r\n")));
    }

    /// If the streamed part's source itself errors mid-stream, that error
    /// must propagate out of the collected `Body` rather than being
    /// silently swallowed or truncating the body without signal.
    #[tokio::test]
    async fn build_stream_propagates_streamed_part_errors() {
        let mut chunks: VecDeque<std::result::Result<Bytes, OxiHttpError>> = VecDeque::new();
        chunks.push_back(Ok(Bytes::from_static(b"partial-data")));
        chunks.push_back(Err(OxiHttpError::Body("simulated read failure".into())));
        let failing_stream = VecStream(chunks);

        let streaming = MultipartBuilder::new().add_stream_part(
            vec![("Content-Disposition".into(), "form-data; name=\"f\"".into())],
            failing_stream,
        );

        let err = collect_body(streaming.build_stream())
            .await
            .expect_err("a mid-stream error must propagate");
        assert!(err.to_string().contains("simulated read failure"));
    }

    /// `add_file_stream` must build the same `Content-Disposition` /
    /// `Content-Type` headers as the in-memory `add_file`.
    #[tokio::test]
    async fn add_file_stream_builds_expected_headers() {
        let streaming = MultipartBuilder::new().add_file_stream(
            "avatar",
            "pic.png",
            "image/png",
            VecStream::of([b"\x89PNG\r\n".as_slice()]),
        );
        let bytes = collect_body(streaming.build_stream())
            .await
            .expect("collect");
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("name=\"avatar\""));
        assert!(s.contains("filename=\"pic.png\""));
        assert!(s.contains("Content-Type: image/png"));
    }

    /// The streaming path's `add_file_stream` (a separate header-building
    /// call site from the in-memory `Part::file`) must apply the same
    /// Content-Disposition sanitization — a CRLF-laced filename must not
    /// inject an extra header line here either.
    #[tokio::test]
    async fn add_file_stream_sanitizes_crlf_in_filename() {
        let malicious_filename = "evil.bin\r\nX-Injected: header-injection";
        let streaming = MultipartBuilder::new().add_file_stream(
            "payload",
            malicious_filename,
            "application/octet-stream",
            VecStream::of([b"data".as_slice()]),
        );
        let bytes = collect_body(streaming.build_stream())
            .await
            .expect("collect");
        let s = String::from_utf8_lossy(&bytes);
        assert!(
            !s.lines().any(|l| l.starts_with("X-Injected")),
            "CRLF in a streamed file part's filename must not create a new header line: {s:?}"
        );
        assert!(s.contains("evil.binX-Injected: header-injection"));
    }
}
