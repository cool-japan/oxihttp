# Changelog

All notable changes to OxiHTTP are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/) and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.4] - 2026-06-19

### Added

- `ClientBuilder::danger_accept_invalid_certs(bool)` — boolean-parameter alias for
  `with_danger_accept_invalid_certs()`, mirroring the reqwest API style (`tls` feature,
  `oxihttp-client`).
- `ClientBuilder::with_custom_cert_verifier(Arc<dyn ServerCertVerifier>)` — inject an
  arbitrary `rustls::client::danger::ServerCertVerifier` into the TLS stack, enabling
  certificate pinning, custom CA hierarchies, and bespoke verification logic without
  forking the library (`tls` feature, `oxihttp-client`).
- `tls::DangerousNoVerification` — a `ServerCertVerifier` implementation that accepts any
  certificate unconditionally (intended for tests / isolated local environments only).
  Re-exported as `oxihttp::DangerousNoVerification` (`tls` feature).
- `tls::build_tls_connector_with_verifier` — internal `TlsConnector` builder that bypasses
  the `oxitls::ClientBuilder` to wire a caller-supplied verifier directly into a
  `rustls::ClientConfig` (`tls` feature, `oxihttp-client`).
- `Response::header(name: &str) -> Option<&str>` — ergonomic single-header accessor
  returning the first UTF-8 value for the named header, or `None` if absent or non-UTF-8
  (`oxihttp-client`).
- `oxihttp-client::tls` module is now `pub` (was `pub(crate)`), making `DangerousNoVerification`
  and related types directly accessible without going through the facade.

### Changed

- `ClientBuilder` internal TLS connector construction refactored into `build_tls_connector_inner()`;
  all `build_*` variants (proxy, SOCKS5, HTTPS, resolver) now dispatch through this helper,
  ensuring consistent behaviour when a custom verifier is present (`tls` feature).
- `TlsRebuildConfig` gains `custom_cert_verifier` field so per-request TLS override
  (`with_request_tls_config`) correctly preserves a custom verifier across re-connections.
- `TlsRebuildConfig`'s `Debug` impl is now manual (was `#[derive(Debug)]`) to avoid
  requiring `ServerCertVerifier: Debug`; the custom verifier field prints as
  `<dyn ServerCertVerifier>` (`tls` feature, `oxihttp-client`).
- Workspace crate versions bumped to 0.1.4 (`oxihttp-core`, `oxihttp-client`,
  `oxihttp-server`, `oxihttp`).

### Security

- `DangerousNoVerification` and `danger_accept_invalid_certs(true)` are clearly documented
  as PRODUCTION-UNSAFE; their doc-comments carry explicit `WARNING` notices and
  recommend TLS-verified alternatives. No production default behaviour has changed.

[0.1.4]: https://github.com/cool-japan/oxihttp/releases/tag/v0.1.4

## [0.1.2] - 2026-06-10

### Changed

- Bumped all workspace crate versions to 0.1.2 (`oxihttp-core`, `oxihttp-client`, `oxihttp-server`, `oxihttp`)
- Updated `oxiarc-deflate` dependency from 0.3.2 → 0.3.3 (latest compression library)
- Aligned workspace-level internal dependency references to 0.1.2

[0.1.2]: https://github.com/cool-japan/oxihttp/compare/v0.1.1...v0.1.2

## [0.1.1] - 2026-06-04

### Changed

- Version bumped to 0.1.1 across all workspace crates (`oxihttp-core`, `oxihttp-client`, `oxihttp-server`, `oxihttp`) and workspace `Cargo.toml`
- README and TODO updated to reflect v0.1.1 release date (2026-06-04)

[0.1.1]: https://github.com/cool-japan/oxihttp/releases/tag/v0.1.1

## [0.1.0] — 2026-06-01

Initial public release of the COOLJAPAN Pure-Rust HTTP stack.

### Crates

| Crate | Description |
|-------|-------------|
| `oxihttp-core` | Core error types, `Body`, `CookieJar`, `ContentType`, multipart/form builders, `HeaderMapExt`, `UriExt`, `RequestBuilder`, `ResponseExt`, `HttpVersion` |
| `oxihttp-client` | Async HTTP/1.1 + HTTPS + HTTP/2 client — redirects, retries, timeouts, proxy (HTTP CONNECT + SOCKS5), decompression, streaming, tower middleware |
| `oxihttp-server` | HTTP/1.1 + HTTPS + HTTP/2 server — `Router` with path/query/vhost routing, CORS, rate limiting, body limits, SSE, WebSocket, static files, tower layers, graceful shutdown |
| `oxihttp` | Facade re-exporting the full public API with `get`/`post`/`put`/`delete` convenience functions |

### Added

**Core (oxihttp-core)**
- `OxiHttpError` with 14 variants (thiserror-derived, Clone + Send + Sync)
- `Body` enum: `Empty`, `Full`, `Stream` with `http_body::Body` impl
- `CookieJar` with RFC 6265-compliant parsing, domain/path matching, expiry
- `ContentType` enum with MIME negotiation and `Accept` header parsing
- `MultipartBuilder` for RFC 2046 multipart/form-data bodies
- `FormBody` for `application/x-www-form-urlencoded` encoding
- `HeaderMapExt` trait with typed accessors for all common HTTP headers
- `UriExt` trait: `host()`, `port_or_default()`, `is_https()`, `origin()`
- `RequestBuilder` (execution-free, for test forging and server-side use)
- `ResponseExt` trait: `body_bytes()`, `body_text()`, `body_json::<T>()`
- `HttpVersion` enum with `FromStr`, `Display`, `From<http::Version>` impls
- Optional `tls` feature gating `oxitls-core` integration

**Client (oxihttp-client)**
- `Client` / `ClientBuilder` backed by `hyper-util` connection pool
- HTTP/1.1 plaintext + HTTPS (oxitls/tokio-rustls, Pure-Rust TLS)
- HTTP/2 via ALPN negotiation
- Per-request TLS config override (`with_tls_config()` on `RequestBuilder`)
- Redirect policy: follow (configurable limit), none, manual
- `RetryPolicy` with exponential backoff wired into request execution
- Per-request and global connect/request timeouts
- HTTP CONNECT proxy (`ProxyConnector`)
- SOCKS5 proxy with user/pass auth (`socks` feature, RFC 1928/1929)
- Automatic decompression via `oxiarc-deflate` (`decompression` feature)
- `Response::body_stream()` returning `Stream<Item = Result<Bytes, _>>`
- `Response::cookies()` — parses all `Set-Cookie` response headers
- `ClientMiddleware` trait + `LoggingMiddleware` + `TimingMiddleware`
- `ClientBuilder::with_layer()` tower Layer composition
- Custom DNS resolver support (`with_resolver()`)
- HTTP/3 client behind `h3` feature flag (via `oxiquic-h3`)
- TLS key logging (`with_key_log_file()` for Wireshark analysis)
- TCP/HTTP/2 socket tuning (`with_tcp_settings()`, `with_http2_settings()`)
- TLS 0-RTT early data support (`with_early_data()`)

**Server (oxihttp-server)**
- `ServerBuilder` / `BoundServer` with TCP listener bind and graceful shutdown
- `Router` with literal, parameterized (`/:param`), and wildcard (`/*`) path matching
- Method-level routing, nested routers (`.nest()`), virtual host dispatch (`.host()`)
- `Router::with_state::<T>()` — request-scoped `Arc<T>` injection
- Extension extractor: `req.extension::<T>()`, `req.state::<T>()`
- CORS middleware (`CorsLayer` with preflight handling)
- Rate limiting: token bucket per IP (`RateLimitLayer`)
- Body size limit middleware (`BodyLimitLayer`)
- Server-side TLS via oxitls/tokio-rustls (`tls` feature)
- mTLS: client certificate verification + `Request::peer_certificates()` accessor
- HTTP/1.1 + HTTP/2 auto-negotiation via `hyper-util::auto::Builder`
- Compression middleware via `oxiarc-deflate` (`compression` feature)
- Static file serving with ETag, conditional GET, byte-range (`static-files` feature)
- `ServeFile` for single-file serving with cache control and MIME detection
- SSE (`Server-Sent Events`) support (`sse` feature)
- WebSocket upgrade (RFC 6455) + frame codec + `Message` types (`websocket` feature)
- WSS (WebSocket over TLS) support
- Tower `Service` impl (`RouterService`) + layer composition (`tower` feature)
- `LoggingLayer`, `RequestIdLayer`, `TimingLayer` built-in middleware
- `fmt::Display` for `Router` (prints route table)
- `Router::resolve()` for O(n) dispatch benchmarking
- `ServerBuilder::local_addr()` for zero-port test setups
- TCP keepalive / nodelay tuning via `socket2`
- Max-connections semaphore (`ServerBuilder::with_max_connections()`)
- HTTP/3 server behind `h3` feature flag (via `oxiquic-h3`)

**Facade (oxihttp)**
- `get()`, `post()`, `put()`, `delete()` top-level convenience functions
- `oxihttp::prelude` module
- `oxihttp::tls` re-export module (TLS config types, `tls` feature)
- `oxihttp::ws` re-export module (WebSocket types, `websocket` feature)
- `oxihttp::middleware` re-export module (`tower` feature)
- `oxihttp::migration` — rustdoc module mapping reqwest idioms to OxiHTTP
- `oxihttp::Result<T>` type alias

### Test Coverage

349 tests across 31 binaries including:
- HTTP/1.1 plaintext GET/POST client-server roundtrip
- HTTPS/TLS 1.3 with rcgen self-signed certs
- HTTP/2 ALPN negotiation
- HTTP/3 roundtrip (h3 feature)
- mTLS (client cert verification, handler reads `peer_certificates()`)
- Redirect chain (301, 302, 307, 308)
- Retry on 503 with exponential backoff
- Streaming 1 MB response body
- HTTP CONNECT proxy + SOCKS5 proxy
- WebSocket text/binary echo, ping/pong, fragmentation
- WSS (WebSocket over TLS) echo
- SSE event stream delivery
- Static file serving with ETag, range, path-traversal guard
- Tower middleware pipeline (logging, request-id injection)
- Compression/decompression via oxiarc-deflate
- Concurrent 10-task client pool reuse
- Per-request timeout enforcement
- State injection and extension extractors
- Virtual host dispatch
- Fuzz harnesses: malformed HTTP requests, malformed client URLs

### Dependencies

All dependencies are Pure-Rust (default features). No C/C++/Fortran code in the
default feature set. TLS is provided by `oxitls` (rustls-based). Compression uses
`oxiarc-deflate`. HTTP/3 uses `oxiquic-h3` (feature-gated).

[0.1.0]: https://github.com/cool-japan/oxihttp/releases/tag/v0.1.0
