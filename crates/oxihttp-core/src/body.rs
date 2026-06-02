//! HTTP body abstraction for the OxiHTTP stack.
//!
//! Provides a unified `Body` enum that supports empty bodies, full (in-memory)
//! bodies, and streaming bodies. Implements `http_body::Body` for integration
//! with hyper's transport layer.

use bytes::Bytes;
use futures_core::Stream;
use http_body::Frame;
use pin_project_lite::pin_project;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::OxiHttpError;

/// The body type for requests and responses in the OxiHTTP stack.
///
/// This enum supports three modes:
/// - `Empty`: No body content.
/// - `Full`: A body fully buffered in memory as `Bytes`.
/// - `Stream`: A streaming body backed by an async byte stream.
#[derive(Debug, Default)]
pub enum Body {
    /// An empty body with no content.
    #[default]
    Empty,
    /// A body fully loaded in memory.
    Full(FullBody),
    /// A streaming body. The inner stream is opaque.
    Stream(StreamBody),
}

impl Body {
    /// Create an empty body.
    pub fn empty() -> Self {
        Self::Empty
    }

    /// Create a body from bytes already in memory.
    pub fn full(data: impl Into<Bytes>) -> Self {
        Self::Full(FullBody {
            data: Some(data.into()),
        })
    }

    /// Create a streaming body from a pinned async byte-chunk stream.
    pub fn stream(inner: Pin<Box<dyn Stream<Item = Result<Bytes, OxiHttpError>> + Send>>) -> Self {
        Self::Stream(StreamBody { inner })
    }

    /// Returns the content length if known.
    ///
    /// Returns `Some(0)` for empty bodies, `Some(n)` for full bodies,
    /// and `None` for streams (unknown length).
    pub fn content_length(&self) -> Option<u64> {
        match self {
            Self::Empty => Some(0),
            Self::Full(full) => full.data.as_ref().map(|d| d.len() as u64),
            Self::Stream(_) => None,
        }
    }

    /// Convert this `Body` into a `PinnedBody` suitable for use with `http_body::Body`.
    pub fn into_pinned(self) -> PinnedBody {
        PinnedBody::from(self)
    }
}

impl From<()> for Body {
    fn from(_: ()) -> Self {
        Self::Empty
    }
}

impl From<Bytes> for Body {
    fn from(b: Bytes) -> Self {
        if b.is_empty() {
            Self::Empty
        } else {
            Self::full(b)
        }
    }
}

impl From<Vec<u8>> for Body {
    fn from(v: Vec<u8>) -> Self {
        Self::from(Bytes::from(v))
    }
}

impl From<String> for Body {
    fn from(s: String) -> Self {
        Self::from(Bytes::from(s))
    }
}

impl From<&'static str> for Body {
    fn from(s: &'static str) -> Self {
        Self::from(Bytes::from_static(s.as_bytes()))
    }
}

impl From<&'static [u8]> for Body {
    fn from(s: &'static [u8]) -> Self {
        Self::from(Bytes::from_static(s))
    }
}

/// A full (in-memory) body.
#[derive(Debug)]
pub struct FullBody {
    data: Option<Bytes>,
}

/// A streaming body wrapping an async byte stream.
pub struct StreamBody {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, OxiHttpError>> + Send>>,
}

impl std::fmt::Debug for StreamBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamBody").finish()
    }
}

// ---------------------------------------------------------------------------
// http_body::Body implementation via PinnedBody
// ---------------------------------------------------------------------------

pin_project! {
    /// A pinnable body type that implements `http_body::Body`.
    ///
    /// Created from `Body` via `Body::into_pinned()` or `PinnedBody::from(body)`.
    #[project = PinnedBodyProj]
    pub enum PinnedBody {
        /// Empty body variant.
        Empty,
        /// Full (in-memory) body variant.
        Full { data: Option<Bytes> },
        /// Streaming body variant.
        Stream { #[pin] inner: Pin<Box<dyn Stream<Item = Result<Bytes, OxiHttpError>> + Send>> },
    }
}

impl From<Body> for PinnedBody {
    fn from(body: Body) -> Self {
        match body {
            Body::Empty => PinnedBody::Empty,
            Body::Full(f) => PinnedBody::Full { data: f.data },
            Body::Stream(s) => PinnedBody::Stream { inner: s.inner },
        }
    }
}

impl http_body::Body for PinnedBody {
    type Data = Bytes;
    type Error = OxiHttpError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.project() {
            PinnedBodyProj::Empty => Poll::Ready(None),
            PinnedBodyProj::Full { data } => {
                let chunk = data.take();
                match chunk {
                    Some(d) if !d.is_empty() => Poll::Ready(Some(Ok(Frame::data(d)))),
                    _ => Poll::Ready(None),
                }
            }
            PinnedBodyProj::Stream { mut inner } => match inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => Poll::Ready(Some(Ok(Frame::data(chunk)))),
                Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Pending => Poll::Pending,
            },
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            PinnedBody::Empty => true,
            PinnedBody::Full { data } => data.is_none(),
            PinnedBody::Stream { .. } => false,
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match self {
            PinnedBody::Empty => http_body::SizeHint::with_exact(0),
            PinnedBody::Full { data } => match data {
                Some(d) => http_body::SizeHint::with_exact(d.len() as u64),
                None => http_body::SizeHint::with_exact(0),
            },
            PinnedBody::Stream { .. } => http_body::SizeHint::default(),
        }
    }
}
