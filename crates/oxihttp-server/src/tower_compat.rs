//! Tower compatibility layer for the OxiHTTP server.
//!
//! Wraps `Arc<Router>` as a `tower_service::Service` and provides type-erasure
//! utilities so that arbitrary tower `Layer`s can be stacked on top of the
//! router in a type-erased way.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use tower::util::BoxCloneService;
use tower_layer::Layer;
use tower_service::Service;

use oxihttp_core::OxiHttpError;

use crate::router::Router;

// ---------------------------------------------------------------------------
// Type alias for our erased, cloneable service type
// ---------------------------------------------------------------------------

/// A type-erased, `Clone`-able tower service that handles incoming HTTP
/// requests and produces fully-buffered responses.
pub type BoxedRouterService =
    BoxCloneService<http::Request<Incoming>, http::Response<Full<Bytes>>, OxiHttpError>;

// ---------------------------------------------------------------------------
// RouterService
// ---------------------------------------------------------------------------

/// Wraps `Arc<Router>` as a `tower_service::Service`.
///
/// This allows the router to be placed at the bottom of a tower `Layer` stack.
#[derive(Clone)]
pub struct RouterService {
    router: Arc<Router>,
}

impl RouterService {
    /// Create a new `RouterService` wrapping the given router.
    pub fn new(router: Arc<Router>) -> Self {
        Self { router }
    }
}

impl Service<http::Request<Incoming>> for RouterService {
    type Response = http::Response<Full<Bytes>>;
    type Error = OxiHttpError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<Incoming>) -> Self::Future {
        let router = Arc::clone(&self.router);
        Box::pin(async move { router.dispatch(req).await })
    }
}

// ---------------------------------------------------------------------------
// ErasedLayer — type-erased Layer stored in Vec<Arc<dyn ErasedLayer>>
// ---------------------------------------------------------------------------

/// Object-safe trait that allows a tower `Layer` to be stored in a
/// `Vec<Arc<dyn ErasedLayer>>` with full type erasure.
///
/// Each implementation wraps a concrete `Layer<BoxedRouterService>` and
/// applies it to produce a new `BoxedRouterService`.
pub trait ErasedLayer: Send + Sync {
    /// Apply this layer to `svc`, returning a new type-erased service.
    fn layer_boxed(&self, svc: BoxedRouterService) -> BoxedRouterService;
}

// ---------------------------------------------------------------------------
// OwnedLayer — concrete wrapper that implements ErasedLayer
// ---------------------------------------------------------------------------

/// Newtype that implements `ErasedLayer` for any concrete `Layer` whose
/// produced `Service` satisfies the required trait bounds.
pub struct OwnedLayer<L>(pub L);

impl<L> ErasedLayer for OwnedLayer<L>
where
    L: Layer<BoxedRouterService> + Send + Sync + Clone + 'static,
    L::Service: Service<
            http::Request<Incoming>,
            Response = http::Response<Full<Bytes>>,
            Error = OxiHttpError,
        > + Clone
        + Send
        + 'static,
    <L::Service as Service<http::Request<Incoming>>>::Future: Send + 'static,
{
    fn layer_boxed(&self, svc: BoxedRouterService) -> BoxedRouterService {
        BoxCloneService::new(self.0.clone().layer(svc))
    }
}

// ---------------------------------------------------------------------------
// RouterMakeService — factory for per-connection RouterService instances
// ---------------------------------------------------------------------------

/// A factory for per-connection [`RouterService`] instances.
///
/// Created by `Router::into_make_service()`.  Each call to [`make`](RouterMakeService::make)
/// returns a cloned `RouterService` suitable for serving one HTTP connection.
#[derive(Clone)]
pub struct RouterMakeService(pub Arc<Router>);

impl RouterMakeService {
    /// Create a `RouterService` for one connection.
    pub fn make(&self) -> RouterService {
        RouterService::new(Arc::clone(&self.0))
    }
}

// ---------------------------------------------------------------------------
// build_layered_service helper
// ---------------------------------------------------------------------------

/// Build a fully-layered `BoxedRouterService` from a router and an ordered
/// list of erased layers.
///
/// Layers are applied from last to first (outermost layer last in the slice
/// ends up as the first to see requests).
pub fn build_layered_service(
    router: Arc<Router>,
    layers: &[Arc<dyn ErasedLayer>],
) -> BoxedRouterService {
    let base: BoxedRouterService = BoxCloneService::new(RouterService::new(router));
    // Fold: first layer in the vec is closest to the router (innermost),
    // last layer is outermost (first to see requests). We reverse so that
    // the first `with_layer` call ends up outermost.
    layers
        .iter()
        .rev()
        .fold(base, |svc, layer| layer.layer_boxed(svc))
}
