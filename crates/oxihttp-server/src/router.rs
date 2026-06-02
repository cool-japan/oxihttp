//! HTTP request router with path parameters and method-based routing.

use bytes::Bytes;
use http::{Method, StatusCode};
use http_body_util::Full;
use hyper::body::Incoming;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use oxihttp_core::OxiHttpError;

/// Type alias for the state injection function stored in `Router`.
///
/// The closure receives mutable access to `http::Extensions` and inserts the
/// typed `Arc<T>` so that `Request::state::<T>()` can retrieve it later.
type StateFn = Box<dyn Fn(&mut http::Extensions) + Send + Sync>;

/// Type alias for the handler function signature.
pub type HandlerFn = Arc<
    dyn Fn(
            Request,
        ) -> Pin<
            Box<dyn Future<Output = Result<hyper::Response<Full<Bytes>>, OxiHttpError>> + Send>,
        > + Send
        + Sync,
>;

/// A request with parsed path parameters and query string.
#[derive(Debug)]
pub struct Request {
    inner: hyper::Request<Incoming>,
    path_params: HashMap<String, String>,
}

impl Request {
    /// Create a new `Request` wrapping a hyper request.
    pub fn new(inner: hyper::Request<Incoming>, path_params: HashMap<String, String>) -> Self {
        Self { inner, path_params }
    }

    /// The HTTP method.
    pub fn method(&self) -> &Method {
        self.inner.method()
    }

    /// The request URI.
    pub fn uri(&self) -> &http::Uri {
        self.inner.uri()
    }

    /// The request headers.
    pub fn headers(&self) -> &http::HeaderMap {
        self.inner.headers()
    }

    /// The path portion of the URI.
    pub fn path(&self) -> &str {
        self.inner.uri().path()
    }

    /// Get a path parameter by name (e.g. from `/users/:id`).
    pub fn param(&self, name: &str) -> Option<&str> {
        self.path_params.get(name).map(|s| s.as_str())
    }

    /// Get all path parameters.
    pub fn params(&self) -> &HashMap<String, String> {
        &self.path_params
    }

    /// Parse query parameters from the URI.
    pub fn query_params(&self) -> HashMap<String, String> {
        self.inner
            .uri()
            .query()
            .map(|q| {
                q.split('&')
                    .filter_map(|pair| {
                        let (k, v) = pair.split_once('=')?;
                        Some((percent_decode(k), percent_decode(v)))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get a single query parameter by name.
    pub fn query(&self, name: &str) -> Option<String> {
        self.query_params().remove(name)
    }

    /// Consume the request and return the inner hyper request.
    pub fn into_inner(self) -> hyper::Request<Incoming> {
        self.inner
    }

    /// Consume the body and return raw bytes.
    pub async fn body_bytes(self) -> Result<Bytes, OxiHttpError> {
        use http_body_util::BodyExt;
        self.inner
            .into_body()
            .collect()
            .await
            .map(|c| c.to_bytes())
            .map_err(|e| OxiHttpError::Body(e.to_string()))
    }

    /// Consume the body and return it as a UTF-8 string.
    pub async fn body_text(self) -> Result<String, OxiHttpError> {
        let bytes = self.body_bytes().await?;
        String::from_utf8(bytes.to_vec())
            .map_err(|e| OxiHttpError::Body(format!("invalid UTF-8: {e}")))
    }

    /// Consume the body and deserialize from JSON.
    pub async fn body_json<T: serde::de::DeserializeOwned>(self) -> Result<T, OxiHttpError> {
        let bytes = self.body_bytes().await?;
        serde_json::from_slice(&bytes).map_err(|e| OxiHttpError::Json(e.to_string()))
    }

    /// Retrieve the shared application state of type `T`.
    ///
    /// Returns `Some(Arc<T>)` when `Router::with_state::<T>()` was used to
    /// register a value of type `T` before starting the server.  Returns `None`
    /// when no state of that type was injected.
    pub fn state<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.inner.extensions().get::<Arc<T>>().cloned()
    }

    /// Retrieve a per-request extension of type `T`.
    ///
    /// Handlers and middleware can store arbitrary values in request extensions
    /// via `req.extensions_mut().insert(value)`.  This accessor clones the
    /// stored value and returns it.
    pub fn extension<T: Clone + Send + Sync + 'static>(&self) -> Option<T> {
        self.inner.extensions().get::<T>().cloned()
    }

    /// Access the raw request extensions map (read-only).
    pub fn extensions(&self) -> &http::Extensions {
        self.inner.extensions()
    }

    /// Access the raw request extensions map (mutable).
    ///
    /// Useful in middleware or handlers that need to attach data for downstream
    /// consumers.
    pub fn extensions_mut(&mut self) -> &mut http::Extensions {
        self.inner.extensions_mut()
    }

    /// Borrow the non-body request parts as a [`RequestParts`][crate::extractor::RequestParts].
    ///
    /// Used internally by [`Request::extract`] and available for manual
    /// extraction via [`FromRequestParts`][crate::extractor::FromRequestParts].
    pub fn parts(&self) -> crate::extractor::RequestParts<'_> {
        crate::extractor::RequestParts {
            method: self.inner.method(),
            uri: self.inner.uri(),
            headers: self.inner.headers(),
            path_params: &self.path_params,
        }
    }

    /// Extract a value implementing [`FromRequestParts`][crate::extractor::FromRequestParts]
    /// from this request.
    ///
    /// # Errors
    ///
    /// Returns the extractor's `Rejection` type (which converts to [`OxiHttpError`])
    /// when extraction fails.
    pub fn extract<T: crate::extractor::FromRequestParts>(&self) -> Result<T, T::Rejection> {
        T::from_request_parts(&self.parts())
    }

    /// Negotiate the best [`ContentType`][oxihttp_core::ContentType] from the
    /// request's `Accept` header and the supplied list of supported types.
    ///
    /// Returns `None` when no supported type satisfies the client's `Accept`
    /// header.  Falls back to `*/*` matching when the header is absent.
    pub fn negotiate(
        &self,
        supported: &[oxihttp_core::ContentType],
    ) -> Option<oxihttp_core::ContentType> {
        negotiate_from_headers(self.headers(), supported)
    }

    /// Get TLS peer certificate information for the current connection.
    ///
    /// Returns `Some(Arc<PeerCertInfo>)` when the request arrived over a TLS
    /// connection.  For plain-TLS connections (no client auth) the returned
    /// struct will have an empty `peer_certificates` vec.  Returns `None` on
    /// non-TLS connections.
    #[cfg(feature = "tls")]
    pub fn tls_info(&self) -> Option<Arc<crate::tls::PeerCertInfo>> {
        self.inner
            .extensions()
            .get::<Arc<crate::tls::PeerCertInfo>>()
            .cloned()
    }

    /// Get the peer certificate chain (DER-encoded, leaf first) from the mTLS handshake.
    ///
    /// Returns `Some(Vec<CertificateDer>)` only when the server required client
    /// authentication (`TlsConfig::with_client_auth`) and the client presented a
    /// valid certificate chain.  Returns `None` for non-TLS connections or when
    /// no client certificate was presented.
    #[cfg(feature = "tls")]
    pub fn peer_certificates(&self) -> Option<Vec<rustls_pki_types::CertificateDer<'static>>> {
        self.tls_info().and_then(|info| {
            if info.peer_certificates.is_empty() {
                None
            } else {
                Some(info.peer_certificates.clone())
            }
        })
    }

    /// Get typed TLS connection information (version, cipher suite, SNI) for the current request.
    ///
    /// Returns `Some` when the request arrived over a TLS connection, `None` otherwise.
    ///
    /// The returned [`oxitls::ConnectionInfo`] contains:
    /// - `version` — negotiated TLS version (`TlsVersion::Tls13`, etc.)
    /// - `cipher_suite` — negotiated cipher suite
    /// - `alpn_protocol` — negotiated ALPN protocol bytes
    /// - `sni` — SNI hostname sent by the client
    /// - `peer_certificates` — DER-encoded client certificate chain (mTLS only)
    #[cfg(feature = "tls")]
    pub fn tls_connection_info(&self) -> Option<oxitls::ConnectionInfo> {
        self.tls_info().map(|info| {
            let mut ci = oxitls::ConnectionInfo::new();
            if let Some(v) = info.version {
                ci = ci.with_version(v);
            }
            if let Some(cs) = info.cipher_suite {
                ci = ci.with_cipher_suite(cs);
            }
            if let Some(ref alpn) = info.alpn_protocol {
                ci = ci.with_alpn_protocol(alpn.clone());
            }
            if let Some(ref sni) = info.sni {
                ci = ci.with_sni(sni.clone());
            }
            if !info.peer_certificates.is_empty() {
                let der_vecs: Vec<Vec<u8>> = info
                    .peer_certificates
                    .iter()
                    .map(|c| c.as_ref().to_vec())
                    .collect();
                ci = ci.with_peer_certificates(der_vecs);
            }
            ci
        })
    }
}

/// Negotiate the best content type from the request's `Accept` header.
///
/// Extracted as a free function so unit tests can call it without constructing
/// a full `hyper::Request` (which requires a live hyper body).
fn negotiate_from_headers(
    headers: &http::HeaderMap,
    supported: &[oxihttp_core::ContentType],
) -> Option<oxihttp_core::ContentType> {
    let accept = headers
        .get(http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("*/*");
    oxihttp_core::content_type::negotiate_content_type(accept, supported)
}

/// Simple percent-decoding for query parameters.
fn percent_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next();
            let lo = chars.next();
            if let (Some(h), Some(l)) = (hi, lo) {
                let hex = [h, l];
                if let Ok(decoded) = u8::from_str_radix(std::str::from_utf8(&hex).unwrap_or(""), 16)
                {
                    result.push(decoded as char);
                    continue;
                }
            }
            result.push('%');
        } else if b == b'+' {
            result.push(' ');
        } else {
            result.push(b as char);
        }
    }
    result
}

/// A single route definition: pattern + method + handler.
struct Route {
    method: Method,
    segments: Vec<Segment>,
    handler: HandlerFn,
}

/// A segment in a route pattern.
#[derive(Debug, Clone)]
enum Segment {
    /// A literal path segment (e.g. "users").
    Literal(String),
    /// A parameter segment (e.g. ":id").
    Param(String),
    /// A wildcard segment (e.g. "*path") that matches the rest.
    Wildcard(String),
}

/// Future type returned by `Router::dispatch`.
pub type DispatchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<hyper::Response<Full<Bytes>>, OxiHttpError>> + Send + 'a>>;

/// HTTP request router with path-parameter extraction and method-based dispatch.
pub struct Router {
    routes: Vec<Route>,
    nested: Vec<(String, Router)>,
    vhosts: Vec<(String, Router)>,
    fallback: Option<HandlerFn>,
    method_not_allowed_handler: Option<HandlerFn>,
    /// Optional state injection function.  When `Some`, it is called with the
    /// request's `Extensions` map immediately before dispatching to a handler,
    /// inserting the typed `Arc<T>` for later retrieval via `Request::state::<T>()`.
    state: Option<StateFn>,
}

impl Router {
    /// Create a new empty router.
    pub fn new() -> Self {
        Self {
            routes: Vec::new(),
            nested: Vec::new(),
            vhosts: Vec::new(),
            fallback: None,
            method_not_allowed_handler: None,
            state: None,
        }
    }

    /// Attach application state of type `T` to this router.
    ///
    /// The state is wrapped in an `Arc<T>` and injected into every request's
    /// extensions map just before the handler is invoked.  Handlers retrieve
    /// it with `req.state::<T>()`.
    ///
    /// Nested routers that do not have their own state automatically inherit
    /// this router's state during dispatch.
    ///
    /// ```rust,no_run
    /// # use oxihttp_server::{Router, router::Request};
    /// # use std::sync::Arc;
    /// #[derive(Clone)]
    /// struct AppState { db_url: String }
    ///
    /// let state = AppState { db_url: "postgres://localhost/mydb".into() };
    /// let router = Router::new()
    ///     .with_state(state)
    ///     .get("/", |req: Request| async move {
    ///         let s = req.state::<AppState>().expect("state present");
    ///         oxihttp_server::response::text_response(&s.db_url)
    ///     });
    /// ```
    pub fn with_state<T: Clone + Send + Sync + 'static>(mut self, state: T) -> Self {
        let arc = Arc::new(state);
        self.state = Some(Box::new(move |ext: &mut http::Extensions| {
            ext.insert(Arc::clone(&arc));
        }));
        self
    }

    /// Register a route for the given method and path pattern.
    ///
    /// Path patterns support:
    /// - Literal segments: `/users/list`
    /// - Parameters: `/users/:id`
    /// - Wildcards: `/static/*path`
    pub fn route<F, Fut>(mut self, method: Method, path: &str, handler: F) -> Self
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<hyper::Response<Full<Bytes>>, OxiHttpError>> + Send + 'static,
    {
        let segments = parse_pattern(path);
        let handler: HandlerFn = Arc::new(move |req| Box::pin(handler(req)));
        self.routes.push(Route {
            method,
            segments,
            handler,
        });
        self
    }

    /// Register a GET route.
    pub fn get<F, Fut>(self, path: &str, handler: F) -> Self
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<hyper::Response<Full<Bytes>>, OxiHttpError>> + Send + 'static,
    {
        self.route(Method::GET, path, handler)
    }

    /// Register a POST route.
    pub fn post<F, Fut>(self, path: &str, handler: F) -> Self
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<hyper::Response<Full<Bytes>>, OxiHttpError>> + Send + 'static,
    {
        self.route(Method::POST, path, handler)
    }

    /// Register a PUT route.
    pub fn put<F, Fut>(self, path: &str, handler: F) -> Self
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<hyper::Response<Full<Bytes>>, OxiHttpError>> + Send + 'static,
    {
        self.route(Method::PUT, path, handler)
    }

    /// Register a DELETE route.
    pub fn delete<F, Fut>(self, path: &str, handler: F) -> Self
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<hyper::Response<Full<Bytes>>, OxiHttpError>> + Send + 'static,
    {
        self.route(Method::DELETE, path, handler)
    }

    /// Register a PATCH route.
    pub fn patch<F, Fut>(self, path: &str, handler: F) -> Self
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<hyper::Response<Full<Bytes>>, OxiHttpError>> + Send + 'static,
    {
        self.route(Method::PATCH, path, handler)
    }

    /// Register a HEAD route.
    pub fn head<F, Fut>(self, path: &str, handler: F) -> Self
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<hyper::Response<Full<Bytes>>, OxiHttpError>> + Send + 'static,
    {
        self.route(Method::HEAD, path, handler)
    }

    /// Nest a sub-router under the given prefix.
    pub fn nest(mut self, prefix: &str, router: Router) -> Self {
        let prefix = prefix.trim_end_matches('/').to_string();
        self.nested.push((prefix, router));
        self
    }

    /// Route requests with the given `Host` header value to `router`.
    ///
    /// The `host` value is matched case-insensitively against the bare hostname
    /// (port suffix stripped).  When a match is found the request is forwarded
    /// to `router` without any path rewriting.  Virtual-host dispatch happens
    /// before nested-prefix dispatch.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use oxihttp_server::Router;
    /// let api = Router::new().get("/v1", |_req| async {
    ///     oxihttp_server::response::text_response("api")
    /// });
    /// let web = Router::new().get("/", |_req| async {
    ///     oxihttp_server::response::text_response("web")
    /// });
    /// let router = Router::new()
    ///     .host("api.example.com", api)
    ///     .host("example.com", web);
    /// ```
    pub fn host(mut self, host: &str, router: Router) -> Self {
        self.vhosts.push((host.to_owned(), router));
        self
    }

    /// Set a fallback handler for routes that don't match (custom 404).
    pub fn fallback<F, Fut>(mut self, handler: F) -> Self
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<hyper::Response<Full<Bytes>>, OxiHttpError>> + Send + 'static,
    {
        self.fallback = Some(Arc::new(move |req| Box::pin(handler(req))));
        self
    }

    /// Set a handler for method-not-allowed (405) responses.
    pub fn method_not_allowed<F, Fut>(mut self, handler: F) -> Self
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<hyper::Response<Full<Bytes>>, OxiHttpError>> + Send + 'static,
    {
        self.method_not_allowed_handler = Some(Arc::new(move |req| Box::pin(handler(req))));
        self
    }

    /// A simple health-check route returning 200 OK.
    pub fn health(self, path: &str) -> Self {
        self.get(path, |_req| async {
            hyper::Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(Bytes::from("OK")))
                .map_err(|e| OxiHttpError::Http(Arc::new(e)))
        })
    }

    /// Match a request path against registered routes without dispatching.
    ///
    /// Replicates the O(n) dispatch scan for use in benchmarks and introspection.
    /// Returns extracted path parameters on a successful match, `None` on no match.
    ///
    /// When the path is found but the method is not registered the method returns
    /// `Some(HashMap::new())` — an empty map — to signal a 405 situation without
    /// actually dispatching.
    pub fn resolve(&self, method: &Method, path: &str) -> Option<HashMap<String, String>> {
        // Check nested prefixes first (delegate to sub-router if matched).
        for (prefix, sub_router) in &self.nested {
            if let Some(stripped) = path.strip_prefix(prefix.as_str()) {
                let sub_path = if stripped.is_empty() { "/" } else { stripped };
                return sub_router.resolve(method, sub_path);
            }
        }

        // Scan routes O(n).
        let mut path_matched = false;
        for route in &self.routes {
            if let Some(params) = match_pattern(&route.segments, path) {
                path_matched = true;
                if route.method == *method {
                    return Some(params);
                }
            }
        }

        // Path existed but method not allowed: return empty params to signal 405.
        if path_matched {
            return Some(HashMap::new());
        }

        None
    }

    /// Dispatch an incoming request through the router.
    pub fn dispatch(&self, req: hyper::Request<Incoming>) -> DispatchFuture<'_> {
        Box::pin(self.dispatch_inner(req))
    }

    async fn dispatch_inner(
        &self,
        mut req: hyper::Request<Incoming>,
    ) -> Result<hyper::Response<Full<Bytes>>, OxiHttpError> {
        let method = req.method().clone();
        let path = req.uri().path().to_string();

        // Virtual host dispatch (runs before nested-prefix dispatch).
        if let Some(host_hdr) = req.headers().get(http::header::HOST) {
            if let Ok(host_str) = host_hdr.to_str() {
                let host_bare = host_str.split(':').next().unwrap_or(host_str);
                for (vhost_name, sub_router) in &self.vhosts {
                    if vhost_name.eq_ignore_ascii_case(host_bare) {
                        // Inherit parent state when the vhost sub-router has none.
                        if sub_router.state.is_none() {
                            if let Some(ref inject_fn) = self.state {
                                inject_fn(req.extensions_mut());
                            }
                        }
                        return sub_router.dispatch(req).await;
                    }
                }
            }
        }

        // Try nested routers.
        // If the nested router has no state of its own, inject the parent's
        // state before forwarding the request so handlers in the sub-router
        // can still call `req.state::<T>()`.
        for (prefix, sub_router) in &self.nested {
            if path.starts_with(prefix.as_str()) {
                let sub_path = &path[prefix.len()..];
                let sub_path = if sub_path.is_empty() { "/" } else { sub_path };

                // Rebuild URI with sub-path.
                let new_uri = http::Uri::builder()
                    .path_and_query(sub_path)
                    .build()
                    .map_err(|e| OxiHttpError::Http(Arc::new(e)))?;

                let (mut parts, body) = req.into_parts();
                parts.uri = new_uri;
                let mut new_req = hyper::Request::from_parts(parts, body);

                // Inherit parent state only when the nested router does not
                // define its own state (the nested router's state takes
                // precedence when set).
                if sub_router.state.is_none() {
                    if let Some(ref inject_fn) = self.state {
                        inject_fn(new_req.extensions_mut());
                    }
                }

                return sub_router.dispatch(new_req).await;
            }
        }

        // Try matching routes.
        let mut path_matched = false;
        for route in &self.routes {
            if let Some(params) = match_pattern(&route.segments, &path) {
                path_matched = true;
                if route.method == method {
                    let mut inner = req;
                    if let Some(ref inject_fn) = self.state {
                        inject_fn(inner.extensions_mut());
                    }
                    let request = Request::new(inner, params);
                    return (route.handler)(request).await;
                }
            }
        }

        // Path matched but method didn't -> 405.
        if path_matched {
            if let Some(ref handler) = self.method_not_allowed_handler {
                let mut inner = req;
                if let Some(ref inject_fn) = self.state {
                    inject_fn(inner.extensions_mut());
                }
                let request = Request::new(inner, HashMap::new());
                return (handler)(request).await;
            }
            return hyper::Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .body(Full::new(Bytes::from("Method Not Allowed")))
                .map_err(|e| OxiHttpError::Http(Arc::new(e)));
        }

        // No match at all -> fallback or 404.
        if let Some(ref handler) = self.fallback {
            let mut inner = req;
            if let Some(ref inject_fn) = self.state {
                inject_fn(inner.extensions_mut());
            }
            let request = Request::new(inner, HashMap::new());
            return (handler)(request).await;
        }

        hyper::Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::from("Not Found")))
            .map_err(|e| OxiHttpError::Http(Arc::new(e)))
    }

    /// Return the number of registered routes (not including nested).
    pub fn route_count(&self) -> usize {
        self.routes.len()
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Router {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Router")
            .field("routes", &self.routes.len())
            .field("nested", &self.nested.len())
            .field("vhosts", &self.vhosts.len())
            .field("has_state", &self.state.is_some())
            .finish()
    }
}

impl std::fmt::Display for Router {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // List virtual hosts and their routes.
        for (host, sub) in &self.vhosts {
            writeln!(f, "vhost: {host}")?;
            for route in &sub.routes {
                writeln!(f, "  {} /<vhost-path>", route.method)?;
            }
        }
        // List top-level routes.
        for route in &self.routes {
            let pattern = route
                .segments
                .iter()
                .map(|s| match s {
                    Segment::Literal(l) => format!("/{l}"),
                    Segment::Param(p) => format!("/:{p}"),
                    Segment::Wildcard(w) => format!("/*{w}"),
                })
                .collect::<String>();
            writeln!(f, "{} {pattern}", route.method)?;
        }
        // List nested prefixes.
        for (prefix, sub) in &self.nested {
            writeln!(f, "nested: {prefix}")?;
            for route in &sub.routes {
                writeln!(f, "  {} {prefix}<path>", route.method)?;
            }
        }
        Ok(())
    }
}

#[cfg(feature = "tower")]
impl Router {
    /// Wrap this router in a `RouterMakeService` factory for use with
    /// tower-compatible runtimes or test harnesses.
    pub fn into_make_service(self) -> crate::tower_compat::RouterMakeService {
        crate::tower_compat::RouterMakeService(std::sync::Arc::new(self))
    }
}

/// Parse a route pattern string into segments.
fn parse_pattern(pattern: &str) -> Vec<Segment> {
    pattern
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if let Some(param) = s.strip_prefix(':') {
                Segment::Param(param.to_string())
            } else if let Some(wildcard) = s.strip_prefix('*') {
                Segment::Wildcard(wildcard.to_string())
            } else {
                Segment::Literal(s.to_string())
            }
        })
        .collect()
}

/// Try to match a path against a route pattern, extracting parameters.
fn match_pattern(segments: &[Segment], path: &str) -> Option<HashMap<String, String>> {
    let path_segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut params = HashMap::new();
    let mut path_idx = 0;

    for seg in segments {
        match seg {
            Segment::Literal(expected) => {
                if path_idx >= path_segments.len() || path_segments[path_idx] != expected.as_str() {
                    return None;
                }
                path_idx += 1;
            }
            Segment::Param(name) => {
                if path_idx >= path_segments.len() {
                    return None;
                }
                params.insert(name.clone(), path_segments[path_idx].to_string());
                path_idx += 1;
            }
            Segment::Wildcard(name) => {
                if path_idx >= path_segments.len() {
                    return None;
                }
                let rest = path_segments[path_idx..].join("/");
                params.insert(name.clone(), rest);
                return Some(params);
            }
        }
    }

    // All segments consumed; check that path is also fully consumed
    if path_idx == path_segments.len() {
        Some(params)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- negotiate_from_headers tests ----------------------------------------

    #[test]
    fn test_negotiate_returns_json_for_json_accept() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::ACCEPT,
            http::HeaderValue::from_static("application/json"),
        );
        let supported = vec![
            oxihttp_core::ContentType::Json,
            oxihttp_core::ContentType::Html(None),
        ];
        let result = negotiate_from_headers(&headers, &supported);
        assert_eq!(result, Some(oxihttp_core::ContentType::Json));
    }

    #[test]
    fn test_negotiate_returns_none_for_unsupported() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::ACCEPT,
            http::HeaderValue::from_static("image/png"),
        );
        let supported = vec![
            oxihttp_core::ContentType::Json,
            oxihttp_core::ContentType::Html(None),
        ];
        let result = negotiate_from_headers(&headers, &supported);
        assert_eq!(result, None);
    }

    // ---- Pattern parse tests -------------------------------------------------

    #[test]
    fn test_parse_literal_pattern() {
        let segments = parse_pattern("/users/list");
        assert_eq!(segments.len(), 2);
        assert!(matches!(&segments[0], Segment::Literal(s) if s == "users"));
        assert!(matches!(&segments[1], Segment::Literal(s) if s == "list"));
    }

    #[test]
    fn test_parse_param_pattern() {
        let segments = parse_pattern("/users/:id");
        assert_eq!(segments.len(), 2);
        assert!(matches!(&segments[0], Segment::Literal(s) if s == "users"));
        assert!(matches!(&segments[1], Segment::Param(s) if s == "id"));
    }

    #[test]
    fn test_parse_wildcard_pattern() {
        let segments = parse_pattern("/static/*path");
        assert_eq!(segments.len(), 2);
        assert!(matches!(&segments[0], Segment::Literal(s) if s == "static"));
        assert!(matches!(&segments[1], Segment::Wildcard(s) if s == "path"));
    }

    #[test]
    fn test_match_literal() {
        let segments = parse_pattern("/users/list");
        let result = match_pattern(&segments, "/users/list");
        assert!(result.is_some());
        assert!(result.as_ref().is_some_and(|p| p.is_empty()));
    }

    #[test]
    fn test_match_literal_no_match() {
        let segments = parse_pattern("/users/list");
        assert!(match_pattern(&segments, "/users/other").is_none());
        assert!(match_pattern(&segments, "/users").is_none());
    }

    #[test]
    fn test_match_param() {
        let segments = parse_pattern("/users/:id");
        let result = match_pattern(&segments, "/users/42");
        assert!(result.is_some());
        let params = result.expect("should match");
        assert_eq!(params.get("id"), Some(&"42".to_string()));
    }

    #[test]
    fn test_match_wildcard() {
        let segments = parse_pattern("/static/*path");
        let result = match_pattern(&segments, "/static/css/style.css");
        assert!(result.is_some());
        let params = result.expect("should match");
        assert_eq!(params.get("path"), Some(&"css/style.css".to_string()));
    }

    #[test]
    fn test_no_match_extra_segments() {
        let segments = parse_pattern("/users");
        assert!(match_pattern(&segments, "/users/extra").is_none());
    }

    #[test]
    fn test_percent_decode() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("plain"), "plain");
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    #[tokio::test]
    async fn test_resolve_match_and_miss() {
        use oxihttp_core::OxiHttpError;
        async fn dummy(
            _req: Request,
        ) -> Result<hyper::Response<http_body_util::Full<bytes::Bytes>>, OxiHttpError> {
            Ok(hyper::Response::new(http_body_util::Full::new(
                bytes::Bytes::new(),
            )))
        }
        let router = Router::new().get("/hello", dummy).get("/users/:id", dummy);

        let method = http::Method::GET;
        // Exact match
        assert!(router.resolve(&method, "/hello").is_some());
        // Param match
        let params = router.resolve(&method, "/users/42").expect("should match");
        assert_eq!(params.get("id").map(|s| s.as_str()), Some("42"));
        // Miss
        assert!(router.resolve(&method, "/nonexistent").is_none());
        // Wrong method: POST on GET route returns Some(empty) to signal 405
        let post = http::Method::POST;
        let result = router.resolve(&post, "/hello");
        assert!(result.is_some());
        assert!(result.unwrap().is_empty());
    }
}
