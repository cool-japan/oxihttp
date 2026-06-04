# oxihttp (facade) TODO

## Status
Fully-implemented facade crate (~150 SLOC) re-exporting all public types from
oxihttp-core, oxihttp-client (feature `client`), and oxihttp-server (feature `server`).
Includes prelude module, one-shot convenience functions (get/post/put/delete),
Result/Error type aliases, and version() fn. TLS/h3/ws/middleware sub-modules
and reqwest migration guide are deferred.

## Core Implementation
- [x] Add prelude module: `pub mod prelude` re-exporting the most commonly used types (Client, Response, StatusCode, Method, HeaderMap, Body) (~30 SLOC)
- [x] Add `get(url) -> Result<Response>` free function for simple one-off GET requests using a default client (~40 SLOC)
- [x] Add `post(url, body) -> Result<Response>` free function for simple one-off POST requests (~40 SLOC)
- [x] Add `Client` re-export from oxihttp-client with TLS pre-configured (oxitls + webpki roots) (~20 SLOC)
- [x] Add `Server` re-export from oxihttp-server with sensible defaults (~20 SLOC)
- [x] Add `tls` module re-exporting oxitls types needed for custom TLS config (~30 SLOC)
- [x] Add `middleware` module re-exporting common tower-http layers (Timeout, CORS, Compression, Logging) (~40 SLOC)
- [x] Add `h3` module behind `h3` feature gate for HTTP/3 client and server via oxiquic (~30 SLOC)
- [x] Add `ws` module re-exporting WebSocket types from oxihttp-server (~20 SLOC)
- [x] Add default feature flags documentation in lib.rs doc comments (~40 SLOC doc)
- [x] Add migration guide from reqwest: doc module `pub mod migrate_from_reqwest` with API comparison (~200 SLOC doc)

## API Improvements
- [x] Add `oxihttp::Error` type alias for `OxiHttpError` (shorter name for common use)
- [x] Add `oxihttp::Result<T>` type alias for `Result<T, OxiHttpError>`
- [x] Re-export `bytes::Bytes` at crate root for body construction
- [x] Re-export `http::Request` and `http::Response` at crate root
- [x] Add `oxihttp::version() -> &str` returning the crate version at runtime
- [x] Ensure all public types implement `Debug`, `Clone` where applicable

## Testing
- [x] Integration test: `oxihttp::get(url)` returns 200 from local server
- [x] Integration test: `oxihttp::post(url, body)` sends body and returns response
- [x] Integration test: full client-server roundtrip using only facade API
- [x] Integration test: HTTPS client-server roundtrip via facade TLS defaults
- [x] Integration test: HTTP/2 client-server roundtrip via facade ALPN
- [x] Integration test: middleware (timeout, CORS) applied via facade API
- [x] Compile test: `cargo build --no-default-features` produces no errors
- [x] Compile test: `cargo build --features client` produces no errors
- [x] Compile test: `cargo build --features server` produces no errors
- [x] Compile test: `cargo build --all-features` produces no errors

## Performance
- [x] Benchmark: facade `get()` overhead vs direct client usage
- [x] Benchmark: default client construction time (includes TLS + connection pool init)

## Integration
- [x] Wire oxitls as default TLS provider for the facade client
- [x] Wire oxitls webpki roots as default trust store
- [x] Wire oxihttp-client as `client` feature dependency
- [x] Wire oxihttp-server as `server` feature dependency
- [x] Wire oxiquic-h3 as `h3` feature dependency (HTTP/3 support)
- [x] Coordinate feature flags: `client`, `server`, `tls`, `h2`, `h3`, `ws`, `middleware`
- [x] Ensure `cargo tree` shows no ring/aws-lc-rs/openssl in default feature closure
- [x] Add deny.toml verification in CI for banned crate enforcement
