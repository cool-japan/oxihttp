//! Gzip and deflate response compression middleware for OxiHTTP server.
//!
//! Compresses response bodies based on the client's `Accept-Encoding` header,
//! using the `oxiarc-deflate` crate (pure Rust, no C/C++ dependencies).
//!
//! # Usage
//!
//! ```rust,no_run
//! use oxihttp_server::Compression;
//!
//! # async fn example() {
//! let compression = Compression::new();
//! // Use in your handler:
//! // let resp = compression.apply(accept_encoding, response).await.unwrap();
//! # }
//! ```

#![forbid(unsafe_code)]

use bytes::Bytes;
use http::StatusCode;
use http_body_util::{BodyExt, Full};
use oxihttp_core::OxiHttpError;

/// Supported compression algorithms (in preference order if negotiated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompressionAlgorithm {
    /// Gzip compression (RFC 1952).
    Gzip,
    /// Deflate/zlib compression (RFC 7230: zlib-wrapped DEFLATE).
    Deflate,
}

impl CompressionAlgorithm {
    /// Returns the `Content-Encoding` token for this algorithm.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Gzip => "gzip",
            Self::Deflate => "deflate",
        }
    }
}

/// Configuration for the compression middleware.
#[derive(Debug, Clone)]
pub struct CompressionConfig {
    /// Minimum body size in bytes to compress.
    ///
    /// Bodies smaller than this threshold are sent uncompressed.
    pub min_size: usize,
    /// Preference-ordered list of algorithms to offer.
    pub algorithms: Vec<CompressionAlgorithm>,
    /// Compression level 1–9 (1 = fastest, 9 = best compression).
    pub level: u8,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            min_size: 1024,
            algorithms: vec![CompressionAlgorithm::Gzip, CompressionAlgorithm::Deflate],
            level: 6,
        }
    }
}

/// Middleware that compresses response bodies based on `Accept-Encoding`.
///
/// # Example
///
/// ```rust,no_run
/// use oxihttp_server::Compression;
/// use bytes::Bytes;
/// use http_body_util::Full;
///
/// # async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
/// let compression = Compression::new().min_size(512).level(6);
/// let resp = hyper::Response::builder()
///     .status(200)
///     .body(Full::new(Bytes::from("hello world")))
///     .unwrap();
/// let compressed = compression.apply(Some("gzip"), resp).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct Compression {
    config: CompressionConfig,
}

impl Compression {
    /// Create a new `Compression` middleware with default configuration.
    pub fn new() -> Self {
        Self {
            config: CompressionConfig::default(),
        }
    }

    /// Set the minimum body size (in bytes) to compress.
    ///
    /// Bodies smaller than this will be sent uncompressed.
    pub fn min_size(mut self, n: usize) -> Self {
        self.config.min_size = n;
        self
    }

    /// Set the compression level (clamped to 1–9).
    pub fn level(mut self, n: u8) -> Self {
        self.config.level = n.clamp(1, 9);
        self
    }

    /// Set the preference-ordered list of algorithms to negotiate.
    pub fn algorithms(mut self, algos: Vec<CompressionAlgorithm>) -> Self {
        self.config.algorithms = algos;
        self
    }

    /// Apply compression to a response, given the request's `Accept-Encoding` header value.
    ///
    /// - If the response is not a 2xx or 206 status, it is returned unchanged.
    /// - If the response already has a `Content-Encoding` header, it is returned unchanged.
    /// - If no compatible algorithm is found in `Accept-Encoding`, the response is returned unchanged.
    /// - If the body is below `min_size`, the response is returned unchanged.
    pub async fn apply(
        &self,
        accept_encoding: Option<&str>,
        resp: hyper::Response<Full<Bytes>>,
    ) -> Result<hyper::Response<Full<Bytes>>, OxiHttpError> {
        let status = resp.status();

        // Only compress 2xx and 206 Partial Content
        let is_compressible_status = status.is_success() || status == StatusCode::PARTIAL_CONTENT;
        if !is_compressible_status {
            return Ok(resp);
        }

        // Don't double-encode
        if resp.headers().contains_key("content-encoding") {
            return Ok(resp);
        }

        // Negotiate algorithm from Accept-Encoding
        let chosen = match accept_encoding {
            None => return Ok(resp),
            Some(ae) => match negotiate(ae, &self.config.algorithms) {
                None => return Ok(resp),
                Some(algo) => algo,
            },
        };

        // Collect body bytes
        let (parts, body) = resp.into_parts();
        let collected = body
            .collect()
            .await
            .map_err(|e| OxiHttpError::Body(e.to_string()))?;
        let raw_bytes = collected.to_bytes();

        // Below minimum size? Return unchanged.
        if raw_bytes.len() < self.config.min_size {
            return Ok(hyper::Response::from_parts(parts, Full::new(raw_bytes)));
        }

        // Compress
        let compressed_vec = match chosen {
            CompressionAlgorithm::Gzip => {
                oxiarc_deflate::gzip_compress(&raw_bytes, self.config.level)
                    .map_err(|e| OxiHttpError::Body(e.to_string()))?
            }
            CompressionAlgorithm::Deflate => {
                // RFC 7230: "deflate" means zlib-wrapped DEFLATE (not raw DEFLATE)
                oxiarc_deflate::zlib_compress(&raw_bytes, self.config.level)
                    .map_err(|e| OxiHttpError::Body(e.to_string()))?
            }
        };

        let compressed_bytes = Bytes::from(compressed_vec);
        let content_length = compressed_bytes.len().to_string();

        let mut new_parts = parts;
        new_parts.headers.remove("content-length");
        new_parts.headers.insert(
            http::header::CONTENT_ENCODING,
            http::HeaderValue::from_str(chosen.as_str())
                .map_err(|e| OxiHttpError::InvalidHeader(e.to_string()))?,
        );
        new_parts.headers.insert(
            http::header::CONTENT_LENGTH,
            http::HeaderValue::from_str(&content_length)
                .map_err(|e| OxiHttpError::InvalidHeader(e.to_string()))?,
        );
        new_parts.headers.insert(
            http::header::VARY,
            http::HeaderValue::from_static("Accept-Encoding"),
        );

        Ok(hyper::Response::from_parts(
            new_parts,
            Full::new(compressed_bytes),
        ))
    }
}

impl Default for Compression {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse `Accept-Encoding` and choose the best matching algorithm from `preferred`.
///
/// Algorithms are checked in the order given by `preferred` (first match wins),
/// so the caller can express preference priority.
fn negotiate(
    accept_encoding: &str,
    preferred: &[CompressionAlgorithm],
) -> Option<CompressionAlgorithm> {
    /// A parsed Accept-Encoding entry.
    struct Entry {
        name: String,
        q: f32,
    }

    let mut entries: Vec<Entry> = accept_encoding
        .split(',')
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            let mut iter = part.splitn(2, ';');
            let name = iter.next()?.trim().to_lowercase();
            let q = iter
                .next()
                .and_then(|q_part| q_part.trim().strip_prefix("q="))
                .and_then(|v| v.parse::<f32>().ok())
                .unwrap_or(1.0_f32);
            Some(Entry { name, q })
        })
        .collect();

    // Sort by descending q-value so highest preference is checked first
    entries.sort_by(|a, b| b.q.partial_cmp(&a.q).unwrap_or(std::cmp::Ordering::Equal));

    // Walk preferred algorithms in order; pick first one the client accepts
    for algo in preferred {
        let name = algo.as_str();
        let allowed = entries
            .iter()
            .any(|e| (e.name == name || e.name == "*") && e.q > 0.0);
        if allowed {
            return Some(algo.clone());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preferred() -> Vec<CompressionAlgorithm> {
        vec![CompressionAlgorithm::Gzip, CompressionAlgorithm::Deflate]
    }

    #[test]
    fn test_negotiate_gzip_preferred() {
        assert_eq!(
            negotiate("gzip, deflate", &preferred()),
            Some(CompressionAlgorithm::Gzip)
        );
    }

    #[test]
    fn test_negotiate_deflate_only() {
        assert_eq!(
            negotiate("deflate", &preferred()),
            Some(CompressionAlgorithm::Deflate)
        );
    }

    #[test]
    fn test_negotiate_none() {
        let gzip_only = vec![CompressionAlgorithm::Gzip];
        assert_eq!(negotiate("br, zstd", &gzip_only), None);
    }

    #[test]
    fn test_negotiate_gzip_q0_deflate_fallback() {
        // gzip explicitly refused; deflate should be selected
        assert_eq!(
            negotiate("gzip;q=0, deflate", &preferred()),
            Some(CompressionAlgorithm::Deflate)
        );
    }

    #[test]
    fn test_negotiate_wildcard() {
        let gzip_only = vec![CompressionAlgorithm::Gzip];
        assert_eq!(negotiate("*", &gzip_only), Some(CompressionAlgorithm::Gzip));
    }

    #[test]
    fn test_negotiate_empty_accept() {
        assert_eq!(negotiate("", &preferred()), None);
    }

    #[test]
    fn test_negotiate_q_ordering() {
        // deflate has higher q than gzip — but our preferred list puts Gzip first.
        // We choose from preferred order, not client order, so Gzip wins.
        assert_eq!(
            negotiate("deflate;q=0.9, gzip;q=0.8", &preferred()),
            Some(CompressionAlgorithm::Gzip)
        );
    }
}
