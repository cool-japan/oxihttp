//! Static file serving with ETag support.
//!
//! Provides [`ServeDir`] for serving files from a directory with:
//! - ETag-based conditional GET (If-None-Match)
//! - If-Modified-Since conditional GET (best-effort)
//! - Byte-range requests (single range)
//! - MIME type detection via `mime_guess`
//! - Path traversal protection

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode};
use sha2::{Digest, Sha256};

use oxihttp_core::{Body, OxiHttpError};

/// Serve static files from a directory with ETag and range support.
pub struct ServeDir {
    root: PathBuf,
    index: Option<String>,
    fallback: Option<PathBuf>,
    cache_control: Option<String>,
    mime_overrides: HashMap<String, String>,
}

impl ServeDir {
    /// Create a new `ServeDir` rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            index: None,
            fallback: None,
            cache_control: None,
            mime_overrides: HashMap::new(),
        }
    }

    /// Set the name of the index file served for directory roots (e.g. `"index.html"`).
    pub fn with_index(mut self, name: &str) -> Self {
        self.index = Some(name.to_owned());
        self
    }

    /// Set a fallback file path (relative to root) served when a file is not found.
    pub fn with_fallback(mut self, path: impl Into<PathBuf>) -> Self {
        self.fallback = Some(path.into());
        self
    }

    /// Set the `Cache-Control` header value for all responses.
    pub fn with_cache_control(mut self, value: &str) -> Self {
        self.cache_control = Some(value.to_owned());
        self
    }

    /// Override the MIME type for the given file extension (without the leading dot).
    pub fn add_mime_override(mut self, ext: &str, mime: &str) -> Self {
        self.mime_overrides.insert(ext.to_owned(), mime.to_owned());
        self
    }

    /// Serve a file request.
    ///
    /// Returns an `http::Response<Body>` with the appropriate status, headers, and body.
    /// Only `GET` and `HEAD` methods are accepted; others yield `405 Method Not Allowed`.
    pub async fn serve(
        &self,
        method: &Method,
        path: &str,
        req_headers: &HeaderMap,
    ) -> Result<http::Response<Body>, OxiHttpError> {
        // Only GET and HEAD are allowed.
        if method != Method::GET && method != Method::HEAD {
            return Ok(http::Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .body(Body::empty())?);
        }

        // Resolve the filesystem path relative to root.
        let rel_path = path.trim_start_matches('/');
        let rel_path = if rel_path.is_empty() {
            // Root request — try the index file if configured.
            match &self.index {
                Some(index) => index.as_str(),
                None => {
                    return Ok(http::Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body(Body::empty())?)
                }
            }
        } else {
            rel_path
        };

        // Security: canonicalize the root (must exist), then normalize the
        // candidate path manually to detect traversal without requiring the
        // file to exist yet.
        let abs_root = self.root.canonicalize()?;
        let joined = abs_root.join(rel_path);

        if !is_path_safe(&abs_root, &joined) {
            return Ok(http::Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Body::empty())?);
        }

        // Determine the actual file path (may fall back).
        let file_path = if joined.is_file() {
            joined
        } else if let Some(fallback) = &self.fallback {
            let fb = abs_root.join(fallback);
            if fb.is_file() {
                fb
            } else {
                return Ok(http::Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::empty())?);
            }
        } else {
            return Ok(http::Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())?);
        };

        // Read file contents via spawn_blocking (avoids requiring the tokio `fs` feature).
        let file_path_for_read = file_path.clone();
        let file_bytes: Vec<u8> =
            tokio::task::spawn_blocking(move || std::fs::read(&file_path_for_read))
                .await
                .map_err(|e| OxiHttpError::Server(format!("read task panicked: {e}")))??;

        // Compute ETag (first 16 bytes of SHA-256, hex-encoded, wrapped in quotes).
        let etag = compute_etag(&file_bytes);

        // Conditional GET: If-None-Match
        if let Some(inm) = req_headers.get("if-none-match") {
            if let Ok(v) = inm.to_str() {
                if etag_matches(v, &etag) {
                    return Ok(http::Response::builder()
                        .status(StatusCode::NOT_MODIFIED)
                        .header("ETag", &etag)
                        .body(Body::empty())?);
                }
            }
        }

        // Conditional GET: If-Modified-Since (best-effort, mtime-based).
        let mtime = file_path.metadata().ok().and_then(|m| m.modified().ok());
        if let (Some(mt), Some(ims_hdr)) = (mtime, req_headers.get("if-modified-since")) {
            if let Ok(ims_str) = ims_hdr.to_str() {
                if !is_modified_since(mt, ims_str) {
                    return Ok(http::Response::builder()
                        .status(StatusCode::NOT_MODIFIED)
                        .header("ETag", &etag)
                        .body(Body::empty())?);
                }
            }
        }

        // MIME type detection.
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let mime = self.mime_overrides.get(ext).cloned().unwrap_or_else(|| {
            mime_guess::from_path(&file_path)
                .first_or_octet_stream()
                .to_string()
        });

        // Range request handling.
        if let Some(range_hdr) = req_headers.get("range") {
            if let Ok(range_str) = range_hdr.to_str() {
                match parse_single_range(range_str, file_bytes.len()) {
                    Ok((start, end)) => {
                        let slice = Bytes::copy_from_slice(&file_bytes[start..=end]);
                        let content_range = format!("bytes {start}-{end}/{}", file_bytes.len());
                        let content_length = slice.len().to_string();
                        let body = if method == Method::HEAD {
                            Body::empty()
                        } else {
                            Body::full(slice)
                        };
                        let mut resp = http::Response::builder()
                            .status(StatusCode::PARTIAL_CONTENT)
                            .header("Content-Type", &mime)
                            .header("Content-Range", content_range)
                            .header("Content-Length", &content_length)
                            .header("ETag", &etag);
                        if let Some(cc) = &self.cache_control {
                            resp = resp.header("Cache-Control", cc);
                        }
                        return Ok(resp.body(body)?);
                    }
                    Err(RangeError::MultiRange | RangeError::Invalid) => {
                        return Ok(http::Response::builder()
                            .status(StatusCode::RANGE_NOT_SATISFIABLE)
                            .header("Content-Range", format!("bytes */{}", file_bytes.len()))
                            .body(Body::empty())?);
                    }
                }
            }
        }

        // Normal (full) response.
        let content_length = file_bytes.len().to_string();
        let body = if method == Method::HEAD {
            Body::empty()
        } else {
            Body::full(Bytes::from(file_bytes))
        };
        let mut resp_builder = http::Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", &mime)
            .header("Content-Length", &content_length)
            .header("ETag", &etag);
        if let Some(cc) = &self.cache_control {
            resp_builder = resp_builder.header("Cache-Control", cc);
        }
        if let Some(mt) = mtime {
            if let Ok(d) = mt.duration_since(SystemTime::UNIX_EPOCH) {
                resp_builder = resp_builder.header("Last-Modified", format_http_date(d.as_secs()));
            }
        }
        Ok(resp_builder.body(body)?)
    }
}

// ---------------------------------------------------------------------------
// ETag helpers
// ---------------------------------------------------------------------------

fn compute_etag(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let hash = hasher.finalize();
    let hex: String = hash.iter().take(16).map(|b| format!("{b:02x}")).collect();
    format!("\"{hex}\"")
}

fn etag_matches(if_none_match: &str, etag: &str) -> bool {
    let inm = if_none_match.trim();
    if inm == "*" {
        return true;
    }
    inm.split(',').map(str::trim).any(|e| e == etag)
}

// ---------------------------------------------------------------------------
// Conditional GET helpers
// ---------------------------------------------------------------------------

/// Returns `false` if the file has NOT been modified since the given HTTP-date.
///
/// Parsing is best-effort; on failure the conservative default (assume modified)
/// is returned so the full file is served.
fn is_modified_since(mtime: SystemTime, ims: &str) -> bool {
    parse_http_date(ims)
        .map(|ims_secs| {
            let mtime_secs = mtime
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            mtime_secs > ims_secs
        })
        .unwrap_or(true)
}

/// Parse an HTTP-date string (RFC 7231 §7.1.1.1) to a Unix timestamp (seconds).
///
/// Supports all three formats defined in RFC 7231:
/// - IMF-fixdate (preferred): `Sun, 06 Nov 1994 08:49:37 GMT`
/// - RFC 850 (obsolete):      `Sunday, 06-Nov-94 08:49:37 GMT`
/// - ANSI C asctime (obsolete): `Sun Nov  6 08:49:37 1994`
///
/// Returns `None` when parsing fails (conservative: caller assumes modified).
fn parse_http_date(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(unix) = parse_imf_fixdate(s) {
        return Some(unix);
    }
    if let Some(unix) = parse_rfc850_date(s) {
        return Some(unix);
    }
    if let Some(unix) = parse_asctime(s) {
        return Some(unix);
    }
    None
}

/// Parse IMF-fixdate: `Sun, 06 Nov 1994 08:49:37 GMT`
fn parse_imf_fixdate(s: &str) -> Option<u64> {
    // Skip weekday + ", "
    let rest = s.split_once(", ")?.1;
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() != 5 || parts[4] != "GMT" {
        return None;
    }
    let day: u32 = parts[0].parse().ok()?;
    let month = parse_month(parts[1])?;
    let year: u32 = parts[2].parse().ok()?;
    let time_parts: Vec<&str> = parts[3].split(':').collect();
    if time_parts.len() != 3 {
        return None;
    }
    let hour: u32 = time_parts[0].parse().ok()?;
    let min: u32 = time_parts[1].parse().ok()?;
    let sec: u32 = time_parts[2].parse().ok()?;
    date_to_unix(year, month, day, hour, min, sec)
}

/// Parse RFC 850 date: `Sunday, 06-Nov-94 08:49:37 GMT`
fn parse_rfc850_date(s: &str) -> Option<u64> {
    let rest = s.split_once(", ")?.1;
    let (date_part, time_tz) = rest.split_once(' ')?;
    let date_fields: Vec<&str> = date_part.split('-').collect();
    if date_fields.len() != 3 {
        return None;
    }
    let day: u32 = date_fields[0].parse().ok()?;
    let month = parse_month(date_fields[1])?;
    let yy: u32 = date_fields[2].parse().ok()?;
    // 2-digit year: 00-69 -> 2000-2069, 70-99 -> 1970-1999
    let year = if yy < 70 { 2000 + yy } else { 1900 + yy };
    let (time_part, tz) = time_tz.rsplit_once(' ')?;
    if tz != "GMT" {
        return None;
    }
    let t: Vec<&str> = time_part.split(':').collect();
    if t.len() != 3 {
        return None;
    }
    let hour: u32 = t[0].parse().ok()?;
    let min: u32 = t[1].parse().ok()?;
    let sec: u32 = t[2].parse().ok()?;
    date_to_unix(year, month, day, hour, min, sec)
}

/// Parse ANSI C asctime: `Sun Nov  6 08:49:37 1994`
fn parse_asctime(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 5 {
        return None;
    }
    let month = parse_month(parts[1])?;
    let day: u32 = parts[2].parse().ok()?;
    let t: Vec<&str> = parts[3].split(':').collect();
    if t.len() != 3 {
        return None;
    }
    let hour: u32 = t[0].parse().ok()?;
    let min: u32 = t[1].parse().ok()?;
    let sec: u32 = t[2].parse().ok()?;
    let year: u32 = parts[4].parse().ok()?;
    date_to_unix(year, month, day, hour, min, sec)
}

/// Parse abbreviated month name to number (1-12).
fn parse_month(s: &str) -> Option<u32> {
    match s {
        "Jan" => Some(1),
        "Feb" => Some(2),
        "Mar" => Some(3),
        "Apr" => Some(4),
        "May" => Some(5),
        "Jun" => Some(6),
        "Jul" => Some(7),
        "Aug" => Some(8),
        "Sep" => Some(9),
        "Oct" => Some(10),
        "Nov" => Some(11),
        "Dec" => Some(12),
        _ => None,
    }
}

/// Convert a date/time to Unix timestamp (seconds since 1970-01-01 00:00:00 UTC).
fn date_to_unix(year: u32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> Option<u64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || year < 1970 {
        return None;
    }
    let days = days_since_epoch(year as i64, month as i64, day as i64)?;
    let total_secs = days as u64 * 86400 + hour as u64 * 3600 + min as u64 * 60 + sec as u64;
    Some(total_secs)
}

/// Days from 1970-01-01 to the given date (proleptic Gregorian calendar).
fn days_since_epoch(year: i64, month: i64, day: i64) -> Option<i64> {
    // Shift so March = month 0 to simplify leap-year calculation.
    let m = if month <= 2 { month + 9 } else { month - 3 };
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    // 719468 = days from 0000-03-01 to 1970-01-01 in proleptic Gregorian
    let result = era * 146097 + doe - 719468;
    if result < 0 {
        None
    } else {
        Some(result)
    }
}

/// Format a Unix timestamp (seconds since epoch) as an IMF-fixdate string.
///
/// Example output: `Sun, 06 Nov 1994 08:49:37 GMT`
fn format_http_date(secs: u64) -> String {
    let secs = secs as i64;
    let days = secs.div_euclid(86400);
    let time_of_day = secs.rem_euclid(86400);

    let hour = time_of_day / 3600;
    let min = (time_of_day % 3600) / 60;
    let sec = time_of_day % 60;

    // Day-of-week: 1970-01-01 was a Thursday (index 4 in Sun=0 scheme).
    let dow = ((days + 4).rem_euclid(7)) as usize;
    let dow_names = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

    let (year, month, day) = unix_days_to_civil(days);
    let month_names = [
        "", "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        dow_names[dow], day, month_names[month as usize], year, hour, min, sec,
    )
}

/// Convert days-since-Unix-epoch to (year, month, day) in the proleptic Gregorian calendar.
fn unix_days_to_civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ---------------------------------------------------------------------------
// Range parsing
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum RangeError {
    MultiRange,
    Invalid,
}

/// Parse a `Range: bytes=N-M` header (single range only).
///
/// Returns `(start, end)` as inclusive byte offsets.
fn parse_single_range(range: &str, file_len: usize) -> Result<(usize, usize), RangeError> {
    let range = range.trim();
    if !range.starts_with("bytes=") {
        return Err(RangeError::Invalid);
    }
    let spec = &range["bytes=".len()..];

    // Reject multi-range.
    if spec.contains(',') {
        return Err(RangeError::MultiRange);
    }

    let dash_pos = spec.find('-').ok_or(RangeError::Invalid)?;
    let start_str = &spec[..dash_pos];
    let end_str = &spec[dash_pos + 1..];

    let (start, end) = if start_str.is_empty() {
        // Suffix range: `bytes=-N` → last N bytes.
        let suffix: usize = end_str.parse().map_err(|_| RangeError::Invalid)?;
        if suffix == 0 {
            return Err(RangeError::Invalid);
        }
        let start = file_len.saturating_sub(suffix);
        (start, file_len.saturating_sub(1))
    } else {
        let start: usize = start_str.parse().map_err(|_| RangeError::Invalid)?;
        let end = if end_str.is_empty() {
            file_len.saturating_sub(1)
        } else {
            end_str.parse::<usize>().map_err(|_| RangeError::Invalid)?
        };
        (start, end)
    };

    if file_len == 0 || start >= file_len || end >= file_len || start > end {
        return Err(RangeError::Invalid);
    }
    Ok((start, end))
}

// ---------------------------------------------------------------------------
// Path safety
// ---------------------------------------------------------------------------

/// Returns `true` if `candidate` is safely inside `root` (no path traversal).
///
/// This normalises both paths component-by-component rather than calling
/// `canonicalize`, which requires the path to already exist.
fn is_path_safe(root: &Path, candidate: &Path) -> bool {
    let root_components: Vec<_> = root.components().collect();
    let mut cand_components: Vec<Component<'_>> = Vec::new();
    for c in candidate.components() {
        match c {
            Component::ParentDir => {
                cand_components.pop();
            }
            Component::CurDir => {}
            other => cand_components.push(other),
        }
    }
    if cand_components.len() < root_components.len() {
        return false;
    }
    root_components == cand_components[..root_components.len()]
}

// ---------------------------------------------------------------------------
// ServeFile — serve a single, pre-configured file
// ---------------------------------------------------------------------------

/// Serve a single specific file (fixed path) with ETag, conditional GET, and byte-range support.
///
/// Unlike [`ServeDir`], this serves one pre-configured file and performs no path resolution.
pub struct ServeFile {
    path: PathBuf,
    cache_control: Option<String>,
    mime_override: Option<String>,
}

impl ServeFile {
    /// Create a new `ServeFile` serving the file at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            cache_control: None,
            mime_override: None,
        }
    }

    /// Set the `Cache-Control` header value for all responses.
    pub fn with_cache_control(mut self, value: &str) -> Self {
        self.cache_control = Some(value.to_owned());
        self
    }

    /// Override the MIME type detected from the file extension.
    pub fn with_mime(mut self, mime: &str) -> Self {
        self.mime_override = Some(mime.to_owned());
        self
    }

    /// Serve the file for the given request method and headers.
    ///
    /// Handles GET/HEAD; returns 405 for other methods.
    /// Uses the same ETag, conditional-GET, and byte-range logic as [`ServeDir`].
    pub async fn serve(
        &self,
        method: &Method,
        req_headers: &HeaderMap,
    ) -> Result<http::Response<Body>, OxiHttpError> {
        // Only GET and HEAD are allowed.
        if method != Method::GET && method != Method::HEAD {
            return Ok(http::Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .body(Body::empty())?);
        }

        // Read the file.
        let data = tokio::fs::read(&self.path)
            .await
            .map_err(|e| OxiHttpError::Io(std::sync::Arc::new(e)))?;

        // MIME type detection.
        let mime = self.mime_override.clone().unwrap_or_else(|| {
            mime_guess::from_path(&self.path)
                .first_or_octet_stream()
                .to_string()
        });

        // Compute ETag.
        let etag = compute_etag(&data);

        // Conditional GET: If-None-Match
        if let Some(inm) = req_headers.get("if-none-match") {
            if let Ok(v) = inm.to_str() {
                if etag_matches(v, &etag) {
                    return Ok(http::Response::builder()
                        .status(StatusCode::NOT_MODIFIED)
                        .header("ETag", &etag)
                        .body(Body::empty())?);
                }
            }
        }

        // Conditional GET: If-Modified-Since (best-effort, mtime-based).
        let mtime = self.path.metadata().ok().and_then(|m| m.modified().ok());
        if let (Some(mt), Some(ims_hdr)) = (mtime, req_headers.get("if-modified-since")) {
            if let Ok(ims_str) = ims_hdr.to_str() {
                if !is_modified_since(mt, ims_str) {
                    return Ok(http::Response::builder()
                        .status(StatusCode::NOT_MODIFIED)
                        .header("ETag", &etag)
                        .body(Body::empty())?);
                }
            }
        }

        // Range request handling.
        if let Some(range_hdr) = req_headers.get("range") {
            if let Ok(range_str) = range_hdr.to_str() {
                match parse_single_range(range_str, data.len()) {
                    Ok((start, end)) => {
                        let slice = Bytes::copy_from_slice(&data[start..=end]);
                        let content_range = format!("bytes {start}-{end}/{}", data.len());
                        let content_length = slice.len().to_string();
                        let body = if method == Method::HEAD {
                            Body::empty()
                        } else {
                            Body::full(slice)
                        };
                        let mut resp = http::Response::builder()
                            .status(StatusCode::PARTIAL_CONTENT)
                            .header("Content-Type", &mime)
                            .header("Content-Range", content_range)
                            .header("Content-Length", &content_length)
                            .header("ETag", &etag);
                        if let Some(cc) = &self.cache_control {
                            resp = resp.header("Cache-Control", cc);
                        }
                        return Ok(resp.body(body)?);
                    }
                    Err(RangeError::MultiRange | RangeError::Invalid) => {
                        return Ok(http::Response::builder()
                            .status(StatusCode::RANGE_NOT_SATISFIABLE)
                            .header("Content-Range", format!("bytes */{}", data.len()))
                            .body(Body::empty())?);
                    }
                }
            }
        }

        // Normal (full) response.
        let content_length = data.len().to_string();
        let body = if method == Method::HEAD {
            Body::empty()
        } else {
            Body::full(Bytes::from(data))
        };
        let mut resp_builder = http::Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", &mime)
            .header("Content-Length", &content_length)
            .header("ETag", &etag);
        if let Some(cc) = &self.cache_control {
            resp_builder = resp_builder.header("Cache-Control", cc);
        }
        if let Some(mt) = mtime {
            if let Ok(d) = mt.duration_since(SystemTime::UNIX_EPOCH) {
                resp_builder = resp_builder.header("Last-Modified", format_http_date(d.as_secs()));
            }
        }
        Ok(resp_builder.body(body)?)
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_etag_stable() {
        let e1 = compute_etag(b"hello");
        let e2 = compute_etag(b"hello");
        assert_eq!(e1, e2);
        assert!(e1.starts_with('"') && e1.ends_with('"'));
        // 16 bytes × 2 hex chars + 2 quote chars = 34
        assert_eq!(e1.len(), 34);
    }

    #[test]
    fn test_etag_different() {
        assert_ne!(compute_etag(b"hello"), compute_etag(b"world"));
    }

    #[test]
    fn test_etag_matches_wildcard() {
        assert!(etag_matches("*", "\"abc\""));
    }

    #[test]
    fn test_etag_matches_exact() {
        assert!(etag_matches("\"abc\"", "\"abc\""));
        assert!(!etag_matches("\"abc\"", "\"xyz\""));
    }

    #[test]
    fn test_parse_range_simple() {
        assert_eq!(parse_single_range("bytes=0-9", 100).unwrap(), (0, 9));
    }

    #[test]
    fn test_parse_range_open_end() {
        assert_eq!(parse_single_range("bytes=5-", 20).unwrap(), (5, 19));
    }

    #[test]
    fn test_parse_range_suffix() {
        // `bytes=-5` on a 20-byte file: last 5 bytes → [15, 19]
        assert_eq!(parse_single_range("bytes=-5", 20).unwrap(), (15, 19));
    }

    #[test]
    fn test_parse_range_multirange_rejected() {
        assert!(matches!(
            parse_single_range("bytes=0-9,20-29", 100),
            Err(RangeError::MultiRange)
        ));
    }

    #[test]
    fn test_path_safe_normal() {
        let root = Path::new("/srv/www");
        assert!(is_path_safe(root, Path::new("/srv/www/index.html")));
    }

    #[test]
    fn test_path_traversal_rejected() {
        let root = Path::new("/srv/www");
        assert!(!is_path_safe(root, Path::new("/srv/www/../etc/passwd")));
    }

    #[test]
    fn test_path_safe_root_equal() {
        let root = Path::new("/srv/www");
        // Exact root — no extra segments, so len < root len check triggers.
        // The root itself is NOT a file, but the safety check allows it.
        // (Serving root returns 404 because it's a dir, not a file.)
        assert!(is_path_safe(root, Path::new("/srv/www")));
    }
}

// ---------------------------------------------------------------------------
// HTTP date parsing unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod http_date_tests {
    use super::*;

    #[test]
    fn test_parse_imf_fixdate() {
        // 1994-11-06T08:49:37Z = 784111777
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784111777)
        );
    }

    #[test]
    fn test_parse_rfc850_date() {
        // Same instant via RFC 850 format
        assert_eq!(
            parse_http_date("Sunday, 06-Nov-94 08:49:37 GMT"),
            Some(784111777)
        );
    }

    #[test]
    fn test_parse_asctime() {
        // Same instant via ANSI C asctime format
        assert_eq!(parse_http_date("Sun Nov  6 08:49:37 1994"), Some(784111777));
    }

    #[test]
    fn test_format_http_date_roundtrip() {
        let ts = 784111777u64;
        let formatted = format_http_date(ts);
        assert_eq!(formatted, "Sun, 06 Nov 1994 08:49:37 GMT");
        let parsed = parse_http_date(&formatted).expect("parse formatted");
        assert_eq!(parsed, ts);
    }

    #[test]
    fn test_format_epoch() {
        // Unix epoch: 1970-01-01 00:00:00 UTC = Thursday
        assert_eq!(format_http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
    }

    #[test]
    fn test_parse_invalid() {
        assert_eq!(parse_http_date("not a date"), None);
        assert_eq!(parse_http_date(""), None);
        assert_eq!(parse_http_date("Sun, 06 Nov 1994 08:49:37 EST"), None);
    }

    #[test]
    fn test_parse_year_before_epoch_returns_none() {
        // 1969 is before Unix epoch
        assert_eq!(parse_http_date("Wed, 01 Jan 1969 00:00:00 GMT"), None);
    }
}
