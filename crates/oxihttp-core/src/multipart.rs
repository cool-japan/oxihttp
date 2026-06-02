//! Multipart form-data body builder (RFC 7578).
//!
//! Provides [`MultipartBuilder`] for constructing `multipart/form-data` bodies
//! and [`Part`] for individual MIME parts.

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::{BufMut, Bytes, BytesMut};

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
    pub fn text(name: &str, value: impl Into<String>) -> Self {
        Self {
            headers: vec![(
                "Content-Disposition".into(),
                format!("form-data; name=\"{name}\""),
            )],
            body: Bytes::from(value.into()),
        }
    }

    /// Create a file/binary part with `Content-Disposition` and `Content-Type` headers.
    pub fn file(name: &str, filename: &str, content_type: &str, body: impl Into<Bytes>) -> Self {
        Self {
            headers: vec![
                (
                    "Content-Disposition".into(),
                    format!("form-data; name=\"{name}\"; filename=\"{filename}\""),
                ),
                ("Content-Type".into(), content_type.to_owned()),
            ],
            body: body.into(),
        }
    }

    /// Create a part with fully custom headers and body.
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
    pub fn add_text(mut self, name: &str, value: impl Into<String>) -> Self {
        self.parts.push(Part::text(name, value));
        self
    }

    /// Add a file/binary part.
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
}
