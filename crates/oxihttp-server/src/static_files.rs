//! Static file serving with ETag support.
//!
//! Provides [`ServeDir`] for serving files from a directory with:
//! - ETag-based conditional GET (If-None-Match)
//! - If-Modified-Since conditional GET (best-effort)
//! - Byte-range requests (single range)
//! - MIME type detection via `mime_guess`
//! - Path traversal protection
//!
//! File bodies (full responses and byte ranges alike) are streamed from
//! disk in bounded chunks rather than read into memory up front (see the
//! internal `FileRangeStream`), so serving a multi-gigabyte file, or many
//! concurrent range requests against one, does not allocate the file's
//! full size per request.
//!
//! # Security: symlinks are not resolved by default
//!
//! [`ServeDir`]'s traversal check (the internal `is_path_safe` helper) is purely lexical: it
//! rejects `..` segments in the *requested* path, but never calls
//! `canonicalize` on the resolved file (the candidate need not exist yet
//! when the check runs). Consequently, a symlink placed *inside* the
//! served root that points *outside* it is followed and served — the
//! request path itself never contains a traversal sequence, so the lexical
//! check has nothing to reject. This matches the default, documented
//! behavior of nginx's `root`/`alias` directives and tower-http's
//! `ServeDir`. Operators serving a directory that untrusted users can
//! write to (upload directories, extracted archives, etc.) should either
//! avoid allowing symlink creation in that directory, or opt into
//! [`ServeDir::with_symlink_protection`], which re-validates the
//! *resolved* (canonicalized) file path against the served root before
//! sending a response.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures_core::Stream;
use http::{HeaderMap, Method, StatusCode};
use tokio::io::{AsyncRead, AsyncSeekExt, ReadBuf};

use oxihttp_core::{Body, OxiHttpError};

/// Serve static files from a directory with ETag and range support.
///
/// See the [module-level security note](self#security-symlinks-are-not-resolved-by-default)
/// about symlinks before serving a directory untrusted users can write to.
pub struct ServeDir {
    root: PathBuf,
    index: Option<String>,
    fallback: Option<PathBuf>,
    cache_control: Option<String>,
    mime_overrides: HashMap<String, String>,
    deny_symlink_escapes: bool,
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
            deny_symlink_escapes: false,
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

    /// Reject requests that resolve (after following symlinks) to a path
    /// outside the served root.
    ///
    /// Off by default — see the
    /// [module-level security note](self#security-symlinks-are-not-resolved-by-default).
    /// When enabled, every resolved file path is additionally
    /// `canonicalize`d (which follows symlinks) and re-checked for
    /// containment within the served root; a symlink that escapes the root
    /// gets a `403 Forbidden` response instead of being served. This adds
    /// one extra `stat`-class syscall per request and is therefore opt-in
    /// rather than the default.
    pub fn with_symlink_protection(mut self, deny_escapes: bool) -> Self {
        self.deny_symlink_escapes = deny_escapes;
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

        // Opt-in symlink-escape protection (see `with_symlink_protection`
        // and the module-level security note): `is_path_safe` above is
        // purely lexical and does not follow symlinks, so a symlink inside
        // `abs_root` pointing outside it passes that check. Here the file
        // is known to exist, so `canonicalize` (which *does* follow
        // symlinks) can run and the resolved path is re-checked for
        // containment.
        if self.deny_symlink_escapes {
            let resolved = file_path.canonicalize()?;
            if !resolved.starts_with(&abs_root) {
                return Ok(http::Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .body(Body::empty())?);
            }
        }

        // MIME type detection (path/extension only — no file content read).
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let mime = self.mime_overrides.get(ext).cloned().unwrap_or_else(|| {
            mime_guess::from_path(&file_path)
                .first_or_octet_stream()
                .to_string()
        });

        // Conditional-GET, Range, and body construction are shared with
        // `ServeFile` — see `respond_with_file` for why they stream rather
        // than buffer the file.
        respond_with_file(
            &file_path,
            method,
            req_headers,
            &mime,
            self.cache_control.as_deref(),
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// ETag helpers
// ---------------------------------------------------------------------------

/// Compute an ETag from file metadata (mtime + length) rather than file
/// content — the nginx/tower-http convention, and the reason serving a file
/// body can be streamed at all: an ETag that required hashing the full
/// content would force a full read on every request no matter how the body
/// itself is served, defeating the point of not buffering it.
///
/// Format: `"<mtime-nanos-as-hex>-<length-as-hex>"`, mirroring nginx's
/// `<mtime>-<size>` default. When `mtime` is unavailable (uncommon, but not
/// guaranteed by every filesystem/platform), only the length is used —
/// still stable across requests for an unchanged file, just unable to
/// detect a same-size edit.
fn compute_etag_from_metadata(len: u64, mtime: Option<SystemTime>) -> String {
    match mtime.and_then(|mt| mt.duration_since(UNIX_EPOCH).ok()) {
        Some(d) => format!("\"{:x}-{len:x}\"", d.as_nanos()),
        None => format!("\"{len:x}\""),
    }
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

        // MIME type detection (path/extension only — no file content read).
        let mime = self.mime_override.clone().unwrap_or_else(|| {
            mime_guess::from_path(&self.path)
                .first_or_octet_stream()
                .to_string()
        });

        // Conditional-GET, Range, and body construction are shared with
        // `ServeDir` — see `respond_with_file` for why they stream rather
        // than buffer the file.
        respond_with_file(
            &self.path,
            method,
            req_headers,
            &mime,
            self.cache_control.as_deref(),
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// Shared conditional-GET / Range / streaming-body response builder
// ---------------------------------------------------------------------------

/// Build the response for a resolved, existing file — shared by
/// [`ServeDir::serve`] and [`ServeFile::serve`] (which differ only in how
/// they resolve `file_path` and `mime`).
///
/// Neither the ETag nor the response body requires reading the file's
/// content:
/// - The ETag is derived from metadata (`mtime` + length — the
///   nginx/tower-http convention; see [`compute_etag_from_metadata`]),
///   which is why it can be computed in O(1) regardless of file size and
///   without itself defeating the point of streaming the body.
/// - The body (full or a single byte range) is streamed from disk in
///   bounded chunks via [`FileRangeStream`] rather than buffered — a 1-byte
///   `Range` request against a multi-gigabyte file allocates a chunk-sized
///   buffer, not the whole file, and N concurrent requests do not each pay
///   for a full copy of the file in memory.
async fn respond_with_file(
    file_path: &Path,
    method: &Method,
    req_headers: &HeaderMap,
    mime: &str,
    cache_control: Option<&str>,
) -> Result<http::Response<Body>, OxiHttpError> {
    let metadata = tokio::fs::metadata(file_path)
        .await
        .map_err(|e| OxiHttpError::Io(std::sync::Arc::new(e)))?;
    let file_len = metadata.len();
    let mtime = metadata.modified().ok();
    let etag = compute_etag_from_metadata(file_len, mtime);

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
            // `parse_single_range` works in `usize`; file lengths are
            // tracked in `u64` (matching `Metadata::len()`) everywhere else
            // in this function so a file larger than `usize::MAX` on a
            // 32-bit target degrades to "range not satisfiable" rather than
            // silently truncating, but still serves the *full* body
            // correctly via the non-Range path below (which stays in `u64`
            // throughout).
            let file_len_for_range = usize::try_from(file_len).unwrap_or(usize::MAX);
            match parse_single_range(range_str, file_len_for_range) {
                Ok((start, end)) => {
                    let range_len = (end - start + 1) as u64;
                    let content_range = format!("bytes {start}-{end}/{file_len}");
                    let body = if method == Method::HEAD {
                        Body::empty()
                    } else {
                        stream_file_range(file_path.to_path_buf(), start as u64, range_len).await?
                    };
                    let mut resp = http::Response::builder()
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header("Content-Type", mime)
                        .header("Content-Range", content_range)
                        .header("Content-Length", range_len.to_string())
                        .header("ETag", &etag);
                    if let Some(cc) = cache_control {
                        resp = resp.header("Cache-Control", cc);
                    }
                    return Ok(resp.body(body)?);
                }
                Err(RangeError::MultiRange | RangeError::Invalid) => {
                    return Ok(http::Response::builder()
                        .status(StatusCode::RANGE_NOT_SATISFIABLE)
                        .header("Content-Range", format!("bytes */{file_len}"))
                        .body(Body::empty())?);
                }
            }
        }
    }

    // Normal (full) response.
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        stream_file_range(file_path.to_path_buf(), 0, file_len).await?
    };
    let mut resp_builder = http::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", mime)
        .header("Content-Length", file_len.to_string())
        .header("ETag", &etag);
    if let Some(cc) = cache_control {
        resp_builder = resp_builder.header("Cache-Control", cc);
    }
    if let Some(mt) = mtime {
        if let Ok(d) = mt.duration_since(UNIX_EPOCH) {
            resp_builder = resp_builder.header("Last-Modified", format_http_date(d.as_secs()));
        }
    }
    Ok(resp_builder.body(body)?)
}

/// Open `path`, seek to `start`, and return a [`Body::Stream`] that yields
/// exactly `len` bytes read in bounded chunks (see [`FileRangeStream`]) —
/// never the whole file, or even the whole requested range, buffered in
/// memory at once.
async fn stream_file_range(path: PathBuf, start: u64, len: u64) -> Result<Body, OxiHttpError> {
    let mut file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| OxiHttpError::Io(std::sync::Arc::new(e)))?;
    if start > 0 {
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|e| OxiHttpError::Io(std::sync::Arc::new(e)))?;
    }
    Ok(Body::stream(Box::pin(FileRangeStream {
        file,
        remaining: len,
    })))
}

/// A [`Stream`] of `Bytes` chunks read from a bounded byte range of an open
/// file. Backs [`stream_file_range`] (and, through it, both `ServeDir` and
/// `ServeFile`'s response bodies): each chunk is read, yielded, and dropped
/// before the next is read, so peak memory for serving a file body is
/// bounded by [`CHUNK_SIZE`](Self::CHUNK_SIZE), not the file's (or the
/// requested range's) size.
struct FileRangeStream {
    file: tokio::fs::File,
    /// Bytes remaining to be read and yielded.
    remaining: u64,
}

impl FileRangeStream {
    /// Upper bound on a single read/yield.
    const CHUNK_SIZE: usize = 64 * 1024;
}

impl Stream for FileRangeStream {
    type Item = Result<Bytes, OxiHttpError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // No self-referential fields (`tokio::fs::File` is itself `Unpin`),
        // so `FileRangeStream` is `Unpin` too and `get_mut` needs no
        // `unsafe`, despite this crate's `#![forbid(unsafe_code)]`.
        let this = self.get_mut();
        if this.remaining == 0 {
            return Poll::Ready(None);
        }
        let want = this.remaining.min(Self::CHUNK_SIZE as u64) as usize;
        let mut buf = vec![0u8; want];
        let mut read_buf = ReadBuf::new(&mut buf);
        match Pin::new(&mut this.file).poll_read(cx, &mut read_buf) {
            Poll::Ready(Ok(())) => {
                let n = read_buf.filled().len();
                if n == 0 {
                    // EOF before `remaining` bytes were all read (e.g. the
                    // file was truncated concurrently) — stop instead of
                    // spinning forever re-polling a source with nothing
                    // left to give.
                    this.remaining = 0;
                    return Poll::Ready(None);
                }
                buf.truncate(n);
                this.remaining -= n as u64;
                Poll::Ready(Some(Ok(Bytes::from(buf))))
            }
            Poll::Ready(Err(e)) => {
                this.remaining = 0;
                Poll::Ready(Some(Err(OxiHttpError::Io(std::sync::Arc::new(e)))))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_etag_stable() {
        let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let e1 = compute_etag_from_metadata(12345, Some(mtime));
        let e2 = compute_etag_from_metadata(12345, Some(mtime));
        assert_eq!(e1, e2);
        assert!(e1.starts_with('"') && e1.ends_with('"'));
    }

    #[test]
    fn test_etag_differs_for_different_length() {
        assert_ne!(
            compute_etag_from_metadata(100, None),
            compute_etag_from_metadata(200, None)
        );
    }

    #[test]
    fn test_etag_differs_for_different_mtime() {
        let mt1 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let mt2 = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000);
        assert_ne!(
            compute_etag_from_metadata(100, Some(mt1)),
            compute_etag_from_metadata(100, Some(mt2))
        );
    }

    #[test]
    fn test_etag_same_length_and_mtime_is_identical() {
        let mt = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        assert_eq!(
            compute_etag_from_metadata(100, Some(mt)),
            compute_etag_from_metadata(100, Some(mt))
        );
    }

    /// Without an `mtime` (e.g. a filesystem that doesn't report one), the
    /// ETag still round-trips (quoted, stable) using only the length.
    #[test]
    fn test_etag_without_mtime_is_quoted_and_stable() {
        let e1 = compute_etag_from_metadata(42, None);
        let e2 = compute_etag_from_metadata(42, None);
        assert_eq!(e1, e2);
        assert!(e1.starts_with('"') && e1.ends_with('"'));
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

    // -------------------------------------------------------------------------
    // Adversarial fuzz: `Range` request-header parsing must never panic
    // -------------------------------------------------------------------------
    //
    // `parse_single_range` parses the client-controlled `Range` request
    // header. It must always resolve to `Ok`/`Err(RangeError)`, never panic —
    // in particular the suffix/prefix numeric parsing and `file_len`
    // arithmetic must not overflow or index out of bounds on hostile input.

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 512,
            max_shrink_iters: 32,
            ..ProptestConfig::default()
        })]

        /// Any arbitrary UTF-8 string as a `Range` value, against any file
        /// length, must not panic.
        #[test]
        fn fuzz_parse_single_range_never_panics(range in ".*", file_len in any::<usize>()) {
            let _ = parse_single_range(&range, file_len);
        }

        /// Structured adversarial input built from the `bytes=`/digit/`-`/`,`
        /// vocabulary the parser actually branches on, against small and
        /// large file lengths (including the `0` edge case).
        #[test]
        fn fuzz_parse_single_range_never_panics_structured(
            prefix in prop::bool::ANY,
            start in prop::option::of("[0-9]{0,20}"),
            end in prop::option::of("[0-9]{0,20}"),
            extra_commas in 0usize..3,
            file_len in prop_oneof![Just(0usize), any::<usize>()],
        ) {
            let mut spec = String::new();
            if prefix {
                spec.push_str("bytes=");
            }
            if let Some(s) = &start {
                spec.push_str(s);
            }
            spec.push('-');
            if let Some(e) = &end {
                spec.push_str(e);
            }
            for _ in 0..extra_commas {
                spec.push_str(",0-1");
            }
            let _ = parse_single_range(&spec, file_len);
        }
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
