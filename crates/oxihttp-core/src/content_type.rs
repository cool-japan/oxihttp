//! Content-Type parsing, detection, and Accept negotiation.

use std::fmt;
use std::str::FromStr;

use crate::OxiHttpError;

/// Well-known content types for HTTP request and response bodies.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContentType {
    /// `application/json`
    Json,
    /// `application/x-www-form-urlencoded`
    Form,
    /// `multipart/form-data` with an optional boundary.
    Multipart(Option<String>),
    /// `application/octet-stream`
    OctetStream,
    /// `text/plain` with an optional charset (e.g. `utf-8`).
    Text(Option<String>),
    /// `text/html` with an optional charset.
    Html(Option<String>),
    /// `application/xml` or `text/xml`.
    Xml,
    /// Any other content type as a raw string.
    Other(String),
}

impl ContentType {
    /// Return the MIME type string (without parameters).
    pub fn mime_type(&self) -> &str {
        match self {
            Self::Json => "application/json",
            Self::Form => "application/x-www-form-urlencoded",
            Self::Multipart(_) => "multipart/form-data",
            Self::OctetStream => "application/octet-stream",
            Self::Text(_) => "text/plain",
            Self::Html(_) => "text/html",
            Self::Xml => "application/xml",
            Self::Other(s) => s.as_str(),
        }
    }

    /// Return the charset parameter if present.
    pub fn charset(&self) -> Option<&str> {
        match self {
            Self::Text(c) | Self::Html(c) => c.as_deref(),
            _ => None,
        }
    }

    /// Returns `true` if this content type is textual.
    pub fn is_text(&self) -> bool {
        matches!(self, Self::Text(_) | Self::Html(_) | Self::Json | Self::Xml)
    }

    /// Detect content type from a file extension.
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "json" => Self::Json,
            "html" | "htm" => Self::Html(Some("utf-8".to_string())),
            "txt" | "text" => Self::Text(Some("utf-8".to_string())),
            "xml" => Self::Xml,
            "css" => Self::Other("text/css".to_string()),
            "js" | "mjs" => Self::Other("application/javascript".to_string()),
            "png" => Self::Other("image/png".to_string()),
            "jpg" | "jpeg" => Self::Other("image/jpeg".to_string()),
            "gif" => Self::Other("image/gif".to_string()),
            "svg" => Self::Other("image/svg+xml".to_string()),
            "ico" => Self::Other("image/x-icon".to_string()),
            "woff" => Self::Other("font/woff".to_string()),
            "woff2" => Self::Other("font/woff2".to_string()),
            "pdf" => Self::Other("application/pdf".to_string()),
            "wasm" => Self::Other("application/wasm".to_string()),
            _ => Self::OctetStream,
        }
    }
}

impl fmt::Display for ContentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Multipart(Some(boundary)) => {
                write!(f, "multipart/form-data; boundary={boundary}")
            }
            Self::Text(Some(charset)) => write!(f, "text/plain; charset={charset}"),
            Self::Html(Some(charset)) => write!(f, "text/html; charset={charset}"),
            _ => f.write_str(self.mime_type()),
        }
    }
}

impl FromStr for ContentType {
    type Err = OxiHttpError;

    /// Parse a `Content-Type` header value.
    ///
    /// This never returns `Err`: unrecognised MIME types are preserved as
    /// [`ContentType::Other`] so callers can still inspect the raw value.
    /// The `Result` return type exists to satisfy the [`FromStr`] contract
    /// and to leave room for stricter validation in the future.
    ///
    /// # Example
    ///
    /// ```rust
    /// use oxihttp_core::ContentType;
    ///
    /// let ct: ContentType = "text/plain; charset=utf-8".parse().expect("infallible");
    /// assert_eq!(ct, ContentType::Text(Some("utf-8".to_string())));
    ///
    /// let unknown: ContentType = "application/vnd.custom+json".parse().expect("infallible");
    /// assert_eq!(unknown, ContentType::Other("application/vnd.custom+json".to_string()));
    /// ```
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Parse "type/subtype; param=value" format
        // MIME type is case-insensitive, but parameter values preserve case.
        let trimmed = s.trim();
        let (mime_raw, params_raw) = match trimmed.split_once(';') {
            Some((m, p)) => (m.trim(), Some(p.trim())),
            None => (trimmed, None),
        };
        let mime = mime_raw.to_lowercase();

        let charset = params_raw.and_then(|p| {
            p.split(';').map(str::trim).find_map(|param| {
                let (key, val) = param.split_once('=')?;
                if key.trim().eq_ignore_ascii_case("charset") {
                    Some(val.trim().trim_matches('"').to_string())
                } else {
                    None
                }
            })
        });

        let boundary = params_raw.and_then(|p| {
            p.split(';').map(str::trim).find_map(|param| {
                let (key, val) = param.split_once('=')?;
                if key.trim().eq_ignore_ascii_case("boundary") {
                    Some(val.trim().trim_matches('"').to_string())
                } else {
                    None
                }
            })
        });

        match mime.as_str() {
            "application/json" => Ok(Self::Json),
            "application/x-www-form-urlencoded" => Ok(Self::Form),
            "multipart/form-data" => Ok(Self::Multipart(boundary)),
            "application/octet-stream" => Ok(Self::OctetStream),
            "text/plain" => Ok(Self::Text(charset)),
            "text/html" => Ok(Self::Html(charset)),
            "application/xml" | "text/xml" => Ok(Self::Xml),
            other => Ok(Self::Other(other.to_string())),
        }
    }
}

/// A parsed `Accept` header entry with quality value.
#[derive(Debug, Clone)]
pub struct AcceptEntry {
    /// The content type.
    pub content_type: ContentType,
    /// The quality value (0.0 to 1.0). Defaults to 1.0.
    pub quality: f32,
}

/// Parse an `Accept` header value into a list of entries sorted by quality
/// (highest first).
///
/// # Example
///
/// ```rust
/// use oxihttp_core::content_type::parse_accept;
///
/// let entries = parse_accept("text/html, application/json;q=0.9, */*;q=0.1");
/// assert_eq!(entries.len(), 3);
/// // Highest quality (implicit 1.0) sorts first.
/// assert_eq!(entries[0].content_type.mime_type(), "text/html");
/// assert!((entries[1].quality - 0.9).abs() < f32::EPSILON);
/// ```
pub fn parse_accept(header: &str) -> Vec<AcceptEntry> {
    let mut entries: Vec<AcceptEntry> = header
        .split(',')
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            let (mime_part, quality) = extract_quality(part);
            let content_type = ContentType::from_str(mime_part).ok()?;
            Some(AcceptEntry {
                content_type,
                quality,
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.quality
            .partial_cmp(&a.quality)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    entries
}

/// Negotiate the best content type from an Accept header and a list of supported types.
///
/// Returns the first supported type that matches the highest-quality accept entry.
pub fn negotiate_content_type(
    accept_header: &str,
    supported: &[ContentType],
) -> Option<ContentType> {
    let entries = parse_accept(accept_header);
    for entry in &entries {
        for sup in supported {
            if entry.content_type.mime_type() == sup.mime_type() {
                return Some(sup.clone());
            }
            // Handle wildcard matching
            if entry.content_type.mime_type() == "*/*" {
                return Some(sup.clone());
            }
            // Handle type/* wildcard
            let entry_type = entry.content_type.mime_type().split('/').next();
            let sup_type = sup.mime_type().split('/').next();
            if let (Some(et), Some(st)) = (entry_type, sup_type) {
                if entry.content_type.mime_type().ends_with("/*") && et == st {
                    return Some(sup.clone());
                }
            }
        }
    }
    None
}

/// Extract quality value from a media type string.
/// e.g. "text/html;q=0.9" -> ("text/html", 0.9)
fn extract_quality(s: &str) -> (&str, f32) {
    // Look for ";q=" parameter
    if let Some(idx) = s.to_lowercase().find(";q=") {
        let (mime, rest) = s.split_at(idx);
        let q_str = &rest[3..]; // skip ";q="
                                // The q value may be followed by more params
        let q_val = q_str
            .split(';')
            .next()
            .and_then(|v| v.trim().parse::<f32>().ok())
            .unwrap_or(1.0);
        (mime.trim(), q_val)
    } else {
        (s, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_json() {
        let ct: ContentType = "application/json".parse().expect("parse json");
        assert_eq!(ct, ContentType::Json);
        assert_eq!(ct.mime_type(), "application/json");
    }

    #[test]
    fn test_parse_text_with_charset() {
        let ct: ContentType = "text/plain; charset=utf-8".parse().expect("parse text");
        assert_eq!(ct, ContentType::Text(Some("utf-8".to_string())));
        assert_eq!(ct.charset(), Some("utf-8"));
    }

    #[test]
    fn test_parse_multipart_with_boundary() {
        let ct: ContentType = "multipart/form-data; boundary=----WebKitFormBoundary"
            .parse()
            .expect("parse multipart");
        assert_eq!(
            ct,
            ContentType::Multipart(Some("----WebKitFormBoundary".to_string()))
        );
    }

    #[test]
    fn test_display() {
        assert_eq!(ContentType::Json.to_string(), "application/json");
        assert_eq!(
            ContentType::Text(Some("utf-8".to_string())).to_string(),
            "text/plain; charset=utf-8"
        );
    }

    #[test]
    fn test_from_extension() {
        assert_eq!(ContentType::from_extension("json"), ContentType::Json);
        assert_eq!(
            ContentType::from_extension("html"),
            ContentType::Html(Some("utf-8".to_string()))
        );
        assert_eq!(
            ContentType::from_extension("unknown"),
            ContentType::OctetStream
        );
    }

    #[test]
    fn test_is_text() {
        assert!(ContentType::Json.is_text());
        assert!(ContentType::Text(None).is_text());
        assert!(ContentType::Html(None).is_text());
        assert!(!ContentType::OctetStream.is_text());
    }

    #[test]
    fn test_accept_negotiation() {
        let supported = vec![ContentType::Json, ContentType::Html(None)];
        let result = negotiate_content_type("text/html, application/json;q=0.9", &supported);
        assert_eq!(result, Some(ContentType::Html(None)));
    }

    #[test]
    fn test_accept_wildcard() {
        let supported = vec![ContentType::Json];
        let result = negotiate_content_type("*/*", &supported);
        assert_eq!(result, Some(ContentType::Json));
    }

    // -------------------------------------------------------------------------
    // Adversarial fuzz: header parsers must never panic on untrusted input
    // -------------------------------------------------------------------------
    //
    // `ContentType::from_str` parses the `Content-Type` request/response
    // header and `parse_accept` parses the `Accept` request header — both are
    // fed directly with attacker-controlled bytes over the wire. Neither may
    // panic; `ContentType::from_str` must resolve to `Ok`/`Err` and
    // `parse_accept` must always return (possibly empty) without panicking.

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 512,
            max_shrink_iters: 32,
            ..ProptestConfig::default()
        })]

        /// Any arbitrary UTF-8 string fed as a `Content-Type` value must not panic.
        #[test]
        fn fuzz_content_type_from_str_never_panics(header in ".*") {
            let _ = ContentType::from_str(&header);
        }

        /// Any arbitrary UTF-8 string fed as an `Accept` header must not panic,
        /// and `negotiate_content_type` built on top of it must not panic either.
        #[test]
        fn fuzz_parse_accept_never_panics(header in ".*") {
            let entries = parse_accept(&header);
            let supported = [ContentType::Json, ContentType::Html(None), ContentType::Xml];
            let _ = negotiate_content_type(&header, &supported);
            // Every parsed entry must carry a finite, well-formed quality value
            // (the parser must never smuggle NaN/garbage through from `f32::parse`).
            for entry in &entries {
                prop_assert!(entry.quality.is_finite());
            }
        }

        /// Structured adversarial input: semicolon/quote-delimited fragments,
        /// which exercise the `;`-splitting and `charset=`/`boundary=`
        /// parameter-extraction branches far more often than free-form text.
        #[test]
        fn fuzz_content_type_from_str_never_panics_structured(
            parts in prop::collection::vec(
                prop::string::string_regex("[a-zA-Z0-9/;= \"-]{0,16}").expect("valid regex"),
                0..8,
            )
        ) {
            let header = parts.join(";");
            let _ = ContentType::from_str(&header);
        }
    }
}
