//! Concrete tower middleware for the OxiHTTP server.
//!
//! Provides two ready-to-use tower layers:
//!
//! * [`LoggingLayer`] — logs method, path, status code, and elapsed time for
//!   every request to stderr.
//! * [`RequestIdLayer`] — assigns a monotonically-incrementing hex request ID
//!   to each request/response via the `x-request-id` header.
//!
//! Both layers implement `tower_layer::Layer` and can be registered with
//! [`ServerBuilder::with_layer`](crate::ServerBuilder::with_layer).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Instant;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use tower_layer::Layer;
use tower_service::Service;

use oxihttp_core::OxiHttpError;

// ─── LoggingLayer ─────────────────────────────────────────────────────────────

/// A tower `Layer` that logs every request/response to stderr.
///
/// Each log line includes the HTTP method, path, response status code, and
/// elapsed time in milliseconds.
#[derive(Clone, Debug, Default)]
pub struct LoggingLayer;

impl<S> Layer<S> for LoggingLayer {
    type Service = LoggingService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        LoggingService { inner }
    }
}

/// The `Service` produced by [`LoggingLayer`].
#[derive(Clone)]
pub struct LoggingService<S> {
    inner: S,
}

impl<S> Service<http::Request<Incoming>> for LoggingService<S>
where
    S: Service<
            http::Request<Incoming>,
            Response = http::Response<Full<Bytes>>,
            Error = OxiHttpError,
        > + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = http::Response<Full<Bytes>>;
    type Error = OxiHttpError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<Incoming>) -> Self::Future {
        let method = req.method().clone();
        let path = req.uri().path().to_owned();
        let start = Instant::now();
        // Clone inner so the async block owns it (required for `Send` bounds).
        let mut inner = self.inner.clone();
        Box::pin(async move {
            let result = inner.call(req).await;
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            match &result {
                Ok(resp) => eprintln!(
                    "[oxihttp] {} {} {} {:.1}ms",
                    method,
                    path,
                    resp.status().as_u16(),
                    elapsed_ms,
                ),
                Err(e) => eprintln!(
                    "[oxihttp] {} {} ERROR {:.1}ms: {}",
                    method, path, elapsed_ms, e,
                ),
            }
            result
        })
    }
}

// ─── RequestIdLayer ───────────────────────────────────────────────────────────

/// Global monotonic counter for request IDs.
static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);

/// A tower `Layer` that assigns a unique `x-request-id` header to every
/// request and propagates it to the response.
///
/// The ID is a zero-padded 16-character lowercase hex string derived from a
/// process-global atomic counter.
#[derive(Clone, Debug, Default)]
pub struct RequestIdLayer;

impl<S> Layer<S> for RequestIdLayer {
    type Service = RequestIdService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RequestIdService { inner }
    }
}

/// The `Service` produced by [`RequestIdLayer`].
#[derive(Clone)]
pub struct RequestIdService<S> {
    inner: S,
}

impl<S> Service<http::Request<Incoming>> for RequestIdService<S>
where
    S: Service<
            http::Request<Incoming>,
            Response = http::Response<Full<Bytes>>,
            Error = OxiHttpError,
        > + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = http::Response<Full<Bytes>>;
    type Error = OxiHttpError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: http::Request<Incoming>) -> Self::Future {
        let id = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let id_str = format!("{id:016x}");

        // Insert the request-ID into the incoming request headers so that
        // downstream handlers can read it via `req.headers()`.
        if let Ok(val) = http::HeaderValue::from_str(&id_str) {
            req.headers_mut().insert("x-request-id", val);
        }

        // Pre-compute a HeaderValue for the response (can't borrow id_str
        // across the await point, so build it now).
        let resp_header = http::HeaderValue::from_str(&id_str).ok();

        let mut inner = self.inner.clone();
        Box::pin(async move {
            let mut resp = inner.call(req).await?;
            if let Some(v) = resp_header {
                resp.headers_mut().insert("x-request-id", v);
            }
            Ok(resp)
        })
    }
}
