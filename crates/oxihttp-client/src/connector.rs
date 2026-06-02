//! TLS connector for oxihttp — bridges `OxiHttpsConnector` into the hyper legacy client.
//!
//! The entire module is gated under `#[cfg(feature = "tls")]` via the file-level attribute.

#![cfg(feature = "tls")]

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use http::Uri;
use hyper::rt;
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::rt::TokioIo;
use tokio_rustls::TlsConnector;
use tower_service::Service;

use oxihttp_core::OxiHttpError;

// ---------------------------------------------------------------------------
// OxiHttpsConnector
// ---------------------------------------------------------------------------

/// A connector that upgrades HTTP connections to TLS when the URI scheme is
/// `https`, and passes through plain TCP for `http` URIs.
///
/// Wraps an inner `H` connector (typically `HttpConnector`) and a
/// `TlsConnector` built from a `rustls::ClientConfig`.
#[derive(Clone)]
pub struct OxiHttpsConnector<H> {
    pub(crate) http: H,
    pub(crate) tls: TlsConnector,
    pub(crate) force_https: bool,
}

impl<H> OxiHttpsConnector<H> {
    /// Create a new connector from an inner HTTP connector and a `TlsConnector`.
    pub fn new(http: H, tls: TlsConnector) -> Self {
        Self {
            http,
            tls,
            force_https: false,
        }
    }

    /// If set, reject plain `http://` URIs instead of passing them through.
    pub fn set_force_https(&mut self, force: bool) {
        self.force_https = force;
    }
}

// ---------------------------------------------------------------------------
// MaybeHttpsStream
// ---------------------------------------------------------------------------

/// A stream that is either a plain TCP connection or a TLS-encrypted one.
///
/// Implements `hyper::rt::Read`, `hyper::rt::Write`, and `Connection` so that
/// the hyper legacy client can use it transparently for both HTTP and HTTPS.
#[allow(clippy::large_enum_variant)]
pub enum MaybeHttpsStream<T> {
    /// Plain HTTP stream.
    Http(T),
    /// TLS-wrapped stream.
    Https(TokioIo<tokio_rustls::client::TlsStream<TokioIo<T>>>),
}

impl<T: rt::Read + rt::Write + Connection + Unpin> Connection for MaybeHttpsStream<T> {
    fn connected(&self) -> Connected {
        match self {
            Self::Http(s) => s.connected(),
            Self::Https(s) => {
                let (tcp, tls) = s.inner().get_ref();
                if tls.alpn_protocol() == Some(b"h2") {
                    tcp.inner().connected().negotiated_h2()
                } else {
                    tcp.inner().connected()
                }
            }
        }
    }
}

impl<T: rt::Read + rt::Write + Unpin> rt::Read for MaybeHttpsStream<T> {
    #[inline]
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: rt::ReadBufCursor<'_>,
    ) -> Poll<Result<(), io::Error>> {
        match Pin::get_mut(self) {
            Self::Http(s) => Pin::new(s).poll_read(cx, buf),
            Self::Https(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl<T: rt::Read + rt::Write + Unpin> rt::Write for MaybeHttpsStream<T> {
    #[inline]
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        match Pin::get_mut(self) {
            Self::Http(s) => Pin::new(s).poll_write(cx, buf),
            Self::Https(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    #[inline]
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match Pin::get_mut(self) {
            Self::Http(s) => Pin::new(s).poll_flush(cx),
            Self::Https(s) => Pin::new(s).poll_flush(cx),
        }
    }

    #[inline]
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match Pin::get_mut(self) {
            Self::Http(s) => Pin::new(s).poll_shutdown(cx),
            Self::Https(s) => Pin::new(s).poll_shutdown(cx),
        }
    }

    #[inline]
    fn is_write_vectored(&self) -> bool {
        match self {
            Self::Http(s) => s.is_write_vectored(),
            Self::Https(s) => s.is_write_vectored(),
        }
    }

    #[inline]
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        match Pin::get_mut(self) {
            Self::Http(s) => Pin::new(s).poll_write_vectored(cx, bufs),
            Self::Https(s) => Pin::new(s).poll_write_vectored(cx, bufs),
        }
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for MaybeHttpsStream<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(..) => f.pad("Http(..)"),
            Self::Https(..) => f.pad("Https(..)"),
        }
    }
}

// ---------------------------------------------------------------------------
// Service<Uri> impl for OxiHttpsConnector
// ---------------------------------------------------------------------------

impl<H> Service<Uri> for OxiHttpsConnector<H>
where
    H: Service<Uri> + Clone + Send + Sync + 'static,
    H::Response: rt::Read + rt::Write + Connection + Send + Unpin + 'static,
    H::Future: Send + 'static,
    H::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Response = MaybeHttpsStream<H::Response>;
    type Error = OxiHttpError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.http
            .poll_ready(cx)
            .map_err(|e| OxiHttpError::Hyper(e.into().to_string()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let scheme = uri.scheme_str().unwrap_or("http").to_owned();

        match scheme.as_str() {
            "http" => {
                if self.force_https {
                    return Box::pin(async move {
                        Err(OxiHttpError::Tls(
                            "force_https is set; refusing plain http:// URI".to_string(),
                        ))
                    });
                }
                let fut = self.http.call(uri);
                Box::pin(async move {
                    let stream = fut
                        .await
                        .map_err(|e| OxiHttpError::Hyper(e.into().to_string()))?;
                    Ok(MaybeHttpsStream::Http(stream))
                })
            }
            "https" => {
                let host = uri.host().unwrap_or("").to_owned();
                let tls = self.tls.clone();
                let mut http = self.http.clone();
                Box::pin(async move {
                    let tcp = http
                        .call(uri)
                        .await
                        .map_err(|e| OxiHttpError::Hyper(e.into().to_string()))?;
                    let server_name = rustls_pki_types::ServerName::try_from(host.as_str())
                        .map_err(|e| OxiHttpError::Tls(e.to_string()))?
                        .to_owned();
                    let tls_stream = tls
                        .connect(server_name, TokioIo::new(tcp))
                        .await
                        .map_err(|e| OxiHttpError::Tls(e.to_string()))?;
                    Ok(MaybeHttpsStream::Https(TokioIo::new(tls_stream)))
                })
            }
            other => {
                let msg = format!("unsupported URI scheme: {other}");
                Box::pin(async move { Err(OxiHttpError::Tls(msg)) })
            }
        }
    }
}
