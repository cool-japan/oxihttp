# OxiHTTP TODO

## Status

**v0.2.1 released (2026-08-07)** — Pure-Rust HTTP stack at ~19 100 SLOC across 4 subcrates (85 `.rs`
files incl. tests/examples/benches; 40 under `src/`; via `tokei`, 2026-08-04).
M0–M5 complete. 320 tests with default features, 446 with `--all-features` (plus 56
doctests), 0 failures (`cargo nextest run` / `cargo test --doc`, 2026-08-07). All
milestones shipped. Security: clears L1 PENDING-REPUBLISH (oxitls 0.2.0 pure Mozilla root
store); `oxitls` further bumped to 0.3.0 in this release (clears RUSTSEC-2026-0104, see
CHANGELOG.md).

Next release candidates:
- Performance tuning: H/2 SETTINGS_MAX_CONCURRENT_STREAMS — **partially resolved**: `h2`
  already honors the peer's advertised limit automatically (stream initiation backpressures
  on `poll_ready` until a slot is free — see `h2::client::SendRequest::max_concurrent_send_streams`
  docs), so nothing needs to be *implemented* for adaptation to happen. What remains is purely
  *observability*: the value cannot currently be read back by an oxihttp caller because
  `oxihttp-client::Client` is built on `hyper_util::client::legacy::Client`, whose connection
  pool owns the per-connection `hyper::client::conn::http2::SendRequest` privately and does not
  expose it. Surfacing the value (e.g. a `Client::http2_max_concurrent_streams(&self, uri)`
  metrics accessor) requires bypassing that pooled client and managing HTTP/2 connections
  directly — a larger architectural change, tracked separately rather than folded into this
  bullet.
- Client-side cookie jar (persistent across redirects) — **done**: `ClientBuilder::with_cookie_jar`
  / `with_new_cookie_jar` inject cookies from the jar per-target-URL on every hop of a redirect
  chain and persist `Set-Cookie` responses back into the jar after each hop
  (`oxihttp-client/src/lib.rs`, inside the redirect-following loop).
- Multipart form body streaming (zero-copy for large file uploads) — **done**:
  `MultipartBuilder::add_stream_part` / `add_file_stream` transition to
  `StreamingMultipart::build_stream`, which streams each part (including a caller-supplied
  async byte stream for the file part) without materializing the full body in memory
  (`oxihttp-core/src/multipart.rs`). `oxihttp-client`'s `RequestBuilder::multipart_stream()`
  now accepts a `StreamingMultipart` directly and drives it against the wire (via
  `Client`'s `hyper_util` client, generalized from a fixed `Full<Bytes>` to
  `oxihttp_core::PinnedBody`) without buffering — bounded-memory streaming verified end to
  end against the crate's own server in
  `crates/oxihttp/tests/multipart_test.rs::test_multipart_stream_client_wire_bounded_memory`.
  Trade-off (documented on `multipart_stream`, not a silent gap): the underlying stream is
  one-shot, so a `multipart_stream` request always sends exactly once (bypasses
  `RetryPolicy`) and cannot follow a body-preserving 307/308 redirect (typed
  `OxiHttpError::Body` instead).
- HTTP/3 server-side push (pending oxiquic-h3 API)

## Core Implementation
- [x] Complete M0 workspace skeleton: deny.toml, Dockerfile.ffi-audit, scripts/ffi-audit.sh (~100 SLOC config)
- [x] oxihttp-core: Body enum (Empty, Full, Stream) with content_length and http_body::Body impl (~180 SLOC)
- [x] oxihttp-core: HeaderMapExt typed header accessors (ContentType, ContentLength, Authorization, etc.) (~150 SLOC)
- [x] oxihttp-core: Cookie/CookieJar for cookie management (~300 SLOC)
- [x] oxihttp-core: ContentType enum and Accept negotiation (~230 SLOC)
- [x] oxihttp-core: multipart body builder (~200 SLOC)
- [x] oxihttp-core: form-encoded body builder (~60 SLOC)
- [x] oxihttp-core: UriExt trait (host, port_or_default, is_https, origin) (~60 SLOC)
- [x] oxihttp-core: RequestBuilder (execution-free, terminal .build()) (planned 2026-05-25)
  - **Goal:** A core request builder that constructs an http::Request<Body> with no I/O; distinct from client's network-bound builder
  - **Design:** `RequestBuilder` in request_builder.rs; ctors: `new(Method, Uri)` + `get/post/put/delete/patch/head(uri)`; fluent: `.header()`, `.headers()`, `.body()`, `.json()` (application/json), `.form()` (form content-type); terminal: `.build() -> Result<OxiRequest<Body>>` (NOT .send()); useful for test request forging and server-side usage
  - **Files:** crates/oxihttp-core/src/request_builder.rs (new), lib.rs
  - **Tests:** unit tests: method/uri/headers/body correct; json sets content-type
- [x] oxihttp-core: ResponseExt trait (body_bytes/text/json) (planned 2026-05-25)
  - **Goal:** Extension trait mirroring reqwest ergonomics on core response type
  - **Design:** `ResponseExt` in response_ext.rs; async methods `body_bytes()`, `body_text()`, `body_json::<T>()` on `http::Response<B: http_body::Body>`
  - **Files:** crates/oxihttp-core/src/response_ext.rs (new), lib.rs
  - **Tests:** unit tests: extract bytes/text/json round-trip
- [x] oxihttp-core: Extended OxiHttpError with Timeout, Redirect, Tls, Dns, ConnectionPool variants (~100 SLOC)
- [x] oxihttp-client: Client struct with hyper_util connection pool (~150 SLOC)
- [x] oxihttp-client: ClientBuilder with fluent configuration API (~200 SLOC)
- [x] oxihttp-client: RequestBuilder (get, post, put, delete, patch, head) (~300 SLOC)
- [x] oxihttp-client: TLS support via oxitls integration (~150 SLOC)
- [x] oxihttp-client: redirect handling with configurable policy (~210 SLOC)
- [x] oxihttp-client: retry logic with exponential backoff (~130 SLOC)
- [x] oxihttp-client: wire RetryPolicy into actual request execution (done 2026-05-25)
  - **Goal:** RetryPolicy is stored but not used; wire it into send() for actual retries
  - **Design:** Pass retry_policy to RequestBuilder; retry loop in send() using should_retry_status() and backoff_delay()
  - **Files:** crates/oxihttp-client/src/lib.rs
  - **Tests:** crates/oxihttp/tests/retry_test.rs
- [x] oxihttp-client: HTTP proxy support (HTTP CONNECT) (planned 2026-05-25)
  - **Goal:** Client routes requests through an HTTP CONNECT proxy for both HTTP and HTTPS targets
  - **Design:** `ProxyConnector` implements `tower_service::Service<Uri>` returning `TokioIo<TcpStream>`; dials proxy TCP, sends `CONNECT host:port HTTP/1.1\r\nHost: host:port\r\n\r\n`, reads until `\r\n\r\n`, requires 200 status; Proxy-Authorization Basic header from URI userinfo; works standalone (`Client<ProxyConnector>`) or as inner `H` of `OxiHttpsConnector<ProxyConnector>` for HTTPS-over-proxy; `tower-service` made unconditional dep
  - **Files:** crates/oxihttp-client/src/proxy.rs (new), crates/oxihttp-client/src/lib.rs, crates/oxihttp-client/Cargo.toml
  - **Tests:** crates/oxihttp/tests/proxy_test.rs (mock HTTP-CONNECT proxy harness, roundtrip, Proxy-Authorization)
  - **Risk:** Proxy handshake edge cases; mitigation: mock server asserts exact bytes
- [x] oxihttp-client: SOCKS5 proxy support behind `socks` feature (planned 2026-05-25)
  - **Goal:** Client routes requests through a SOCKS5 proxy with optional username/password auth, DNS resolved server-side
  - **Design:** `Socks5Connector` in proxy.rs (behind `socks` feature); greeting → optional RFC 1929 user/pass auth → CONNECT command; CRITICAL: ATYP=0x03 + len + hostname bytes for DNS names (NO client-side pre-resolution); ATYP=0x01/0x04 only for IP literals; creds from proxy URI userinfo; `with_socks5_proxy(uri)` on ClientBuilder
  - **Files:** crates/oxihttp-client/src/proxy.rs, Cargo.toml
  - **Tests:** crates/oxihttp/tests/proxy_test.rs (mock SOCKS5 server, ATYP=domain roundtrip, user/pass auth)
  - **Risk:** Byte sequence correctness; mitigation: full RFC 1928/1929 spec implementation
- [x] oxihttp-client: Response::cookies() parsing Set-Cookie headers (planned 2026-05-25)
  - **Goal:** Response::cookies() returns Vec<Cookie> by iterating all Set-Cookie response headers
  - **Design:** Iterate headers().get_all(SET_COOKIE), filter_map to_str().ok(), call Cookie::parse_set_cookie() from oxihttp-core; no body consumption
  - **Files:** crates/oxihttp-client/src/lib.rs
  - **Tests:** crates/oxihttp/tests/proxy_test.rs (server sets multiple Set-Cookie headers, client reads them)
- [x] oxihttp-client: automatic decompression via oxiarc-deflate (~80 SLOC) (done 2026-05-25)
  - **Goal:** Client sends Accept-Encoding, auto-decompresses gzip/deflate responses
  - **Design:** with_decompression(bool) flag; Response.decompress field; feature-gate decompression; use gzip_decompress()/zlib_decompress() from oxiarc-deflate
  - **Files:** crates/oxihttp-client/src/lib.rs, Cargo.toml
  - **Tests:** Server compresses response; client reads plaintext
- [x] oxihttp-client: tower middleware/interceptor chain (~100 SLOC) (done 2026-05-25)
  - **Goal:** `ClientBuilder::with_layer()` wraps the hyper client's request path with tower Layers
  - **Design:** `ClientMiddleware` trait with `before_request`/`after_response` hooks; `ClientBuilder::with_middleware()` and `with_layer()` (alias) append to middleware stack; `LoggingMiddleware` and `TimingMiddleware` provided; no feature gate needed (pure-Rust trait, no tower deps)
  - **Files:** crates/oxihttp-client/src/middleware.rs (new), crates/oxihttp-client/src/lib.rs
  - **Tests:** crates/oxihttp/tests/tower_test.rs (4 tests: logging, with_layer alias, timing callback, multiple middleware)
- [x] oxihttp-client: streaming response body (~100 SLOC) (done 2026-05-25)
  - **Goal:** Response::body_stream() for large downloads
  - **Design:** BodyStream newtype over http_body_util::BodyStream<Incoming>; impl Stream<Item = Result<Bytes, OxiHttpError>>; add futures-core dep
  - **Files:** crates/oxihttp-client/src/lib.rs, Cargo.toml
  - **Tests:** Stream 1MB body, verify total size
- [x] oxihttp-server: Server struct with TCP listener and hyper server (~150 SLOC)
- [x] oxihttp-server: ServerBuilder with graceful shutdown (~200 SLOC)
- [x] oxihttp-server: Router with path params, query params, method routing (~400 SLOC)
- [x] oxihttp-server: nested routing and route grouping (~30 SLOC)
- [x] oxihttp-server: CORS middleware (~200 SLOC)
- [x] oxihttp-server: body size limit middleware (~50 SLOC)
- [x] oxihttp-server: rate limiting middleware with token bucket (~120 SLOC)
- [x] oxihttp-server: static file serving with ETag support (~340 SLOC)
- [x] oxihttp-server: compression middleware via oxiarc-deflate (~150 SLOC)
- [x] oxihttp-server: WebSocket upgrade and frame handling (~580 SLOC) (done 2026-05-25)
  - **Goal:** Full RFC 6455 WebSocket: upgrade handshake, frame codec, Message types, WSS support
  - **Design:** `ws::upgrade(req)` validates headers, computes SHA-1 accept hash, returns 101 + `WebSocket<Upgraded>`; frame codec handles all opcodes, masking, fragmentation, control frame interleaving; `WebSocket<S>` generic over AsyncRead+AsyncWrite; feature-gated `websocket`; `serve_connection_with_upgrades` used in all accept loops
  - **Files:** crates/oxihttp-server/src/ws.rs (new), crates/oxihttp-server/src/ws_frame.rs (new), crates/oxihttp-server/src/lib.rs, crates/oxihttp-server/Cargo.toml, Cargo.toml (workspace add sha1 + base64)
  - **Tests:** crates/oxihttp/tests/websocket_test.rs (5 tests: text echo, binary echo, ping/pong, fragment reassembly, invalid upgrade)
- [x] oxihttp-server: SSE (Server-Sent Events) support (~150 SLOC)
- [x] oxihttp-server: ServeFile::new(path) single-file static serving (planned 2026-05-25)
  - **Goal:** Serve a single specific file with ETag, conditional GET, and byte-range support
  - **Design:** `ServeFile` in static_files.rs; `ServeFile::new(path)`, `.with_cache_control()`, `.with_mime()`; reuses private helpers: `compute_etag`, `etag_matches`, `is_modified_since`, `parse_single_range`, `format_http_date` (all in same file); no path-traversal logic (fixed path); re-export from lib.rs under static-files feature
  - **Files:** crates/oxihttp-server/src/static_files.rs, lib.rs
  - **Tests:** crates/oxihttp/tests/serve_file_test.rs (200 + correct content-type, If-None-Match → 304, Range → 206)
- [x] oxihttp-server: virtual host support via Router::host() (planned 2026-05-25)
  - **Goal:** Route requests to different sub-routers based on Host header
  - **Design:** Add `vhosts: Vec<(String, Router)>` to Router struct; `.host(host: &str, router: Router)` builder; in dispatch_inner: check HOST header BEFORE nested prefix loop, strip :port, case-insensitive match, delegate to matching vhost router (full path preserved); reuse nest state-inheritance pattern
  - **Files:** crates/oxihttp-server/src/router.rs
  - **Tests:** crates/oxihttp/tests/server_test.rs (two hosts → two routers; unknown host → fallback)
- [x] oxihttp-server: Display for Router (route listing) (planned 2026-05-25)
  - **Goal:** fmt::Display on Router shows METHOD path per route for debugging
  - **Design:** impl Display for Router listing each route as "METHOD /path", nested prefixes, vhost names
  - **Files:** crates/oxihttp-server/src/router.rs
  - **Tests:** smoke test: format!("{}", router) is non-empty and contains known routes
- [x] oxihttp-server: Server::local_addr() for test ergonomics (planned 2026-05-25)
  - **Goal:** Get the bound socket address before/after server starts (needed for zero-port test setups)
  - **Design:** Add `BoundServer` struct holding `TcpListener + SocketAddr + ServerBuilder`; `ServerBuilder::listen() -> Result<BoundServer>` binds eagerly; `BoundServer::local_addr() -> SocketAddr`; `BoundServer::serve(router) -> Result<()>`; existing serve/serve_with_addr unchanged
  - **Files:** crates/oxihttp-server/src/lib.rs
  - **Tests:** bind to port 0, assert local_addr().port() != 0
- [x] oxihttp-server: Server::into_make_service() for tower compatibility (planned 2026-05-25)
  - **Goal:** Produce a clone-per-connection service factory from Router (tower-gated)
  - **Design:** `Router::into_make_service(self) -> RouterMakeService` (tower feature); `RouterMakeService(Arc<Router>)` implements `Clone` and has `make(&self) -> RouterService` (or tower's MakeService if clean); consistent with tower_compat RouterService
  - **Files:** crates/oxihttp-server/src/tower_compat.rs, router.rs
  - **Tests:** compile test + basic dispatch
- [x] oxihttp-server: form_response() helper in response.rs (planned 2026-05-25)
  - **Goal:** Build an application/x-www-form-urlencoded HTTP response from a FormBody
  - **Design:** `form_response(body: FormBody) -> Result<Response<Full<Bytes>>, OxiHttpError>`; uses FormBody::build() for the body bytes; sets Content-Type: application/x-www-form-urlencoded
  - **Files:** crates/oxihttp-server/src/response.rs
  - **Tests:** roundtrip: server returns FormBody, client reads form-encoded body
- [x] oxihttp facade: prelude, convenience functions (get, post, put, delete), re-exports (~120 SLOC)
- [x] oxihttp facade: reqwest migration guide documentation (planned 2026-05-25)
  - **Goal:** Extensive rustdoc module mapping reqwest idioms → oxihttp equivalents
  - **Design:** `migration.rs` in oxihttp facade with `//!` rustdoc covering: client build, get/post/json, headers, timeouts, redirects, TLS, proxy, streaming, server side; doc-only module; no runtime code
  - **Files:** crates/oxihttp/src/migration.rs (new), crates/oxihttp/src/lib.rs
  - **Tests:** cargo doc --no-deps (no broken links)
- [x] oxihttp facade: tls re-export module (planned 2026-05-25)
  - **Goal:** Re-export TLS config types under oxihttp::tls module (tls feature)
  - **Design:** `pub mod tls` in lib.rs re-exporting TlsConfig, PeerCertInfo; gated on `tls` feature
  - **Files:** crates/oxihttp/src/lib.rs
- [x] oxihttp facade: ws re-export module (planned 2026-05-25)
  - **Goal:** Re-export WebSocket types under oxihttp::ws module (websocket feature)
  - **Design:** `pub mod ws` re-exporting WebSocket, Message, CloseFrame, upgrade, WebSocketUpgrade
  - **Files:** crates/oxihttp/src/lib.rs
- [x] oxihttp facade: middleware re-export module (planned 2026-05-25)
  - **Goal:** Re-export tower middleware types under oxihttp::middleware module (tower feature)
  - **Design:** `pub mod middleware` re-exporting LoggingLayer, RequestIdLayer, TimingLayer, ClientMiddleware, LoggingMiddleware, TimingMiddleware
  - **Files:** crates/oxihttp/src/lib.rs

## API Improvements
- [x] Ergonomic `Client` with pre-configured TLS and sensible defaults
- [x] `Response::error_for_status()` for 4xx/5xx error handling
- [x] Typed header extraction for common headers (planned 2026-05-25)
  - **Goal:** Expand `HeaderMapExt` with 7 new getters + 4 new setters for common HTTP headers
  - **Design:** Add to trait and `impl HeaderMapExt for HeaderMap` in `header_ext.rs`: getters `cache_control()`, `etag()`, `if_none_match()`, `if_modified_since()`, `cookie_header()`, `location()`, `referer()`; setters `set_cache_control()`, `set_etag()`, `set_location()`, `set_cookie_header()` (append for Set-Cookie)
  - **Files:** `crates/oxihttp-core/src/header_ext.rs`
  - **Tests:** Unit tests for each new accessor in existing `#[cfg(test)]` block
- [x] `Router::fallback()` and `.method_not_allowed()` custom error handlers
- [x] State<T> and Extension<T> extractors for handler state injection (done 2026-05-25)
  - **Goal:** Handlers can read application state and per-request extensions from `Request` via `req.state::<T>()` and `req.extension::<T>()`
  - **Design:** `Router::with_state<T>(state)` stores `Arc<T>`, injects into request extensions during dispatch; `Request::state<T>()` returns `Option<Arc<T>>`; `Request::extension<T>()` returns `Option<T>` (cloned); nested routers inherit parent state; closure-based injection avoids Any downcasting
  - **Files:** crates/oxihttp-server/src/router.rs
  - **Tests:** crates/oxihttp/tests/state_test.rs (7 tests: state_injection, extension_read, extensions_accessor, nested_inherits, nested_own_state_wins, state_missing, state_in_fallback)
- [x] `oxihttp::Result<T>` type alias for ergonomic error handling

## Testing
- [x] HTTP/1.1 plaintext GET/POST client-server roundtrip
- [x] HTTPS GET/POST with rcgen self-signed cert via oxitls
- [x] HTTP/2 ALPN negotiation and HTTPS server tests (2026-05-25)
  - **Implemented:** http2_test.rs with TlsConfig from DER/PEM, HTTPS server HTTP/1.1 roundtrip, h2 ALPN configured test, with_tls_from_pem builder
- [x] redirect chain following (301, 302, 307, 308) with limit
- [x] connection pool reuse verification (test_pool_sequential_reuse: 10 sequential requests via pooled client)
- [x] per-request timeout enforcement
- [x] retry on 503 with backoff (done 2026-05-25)
  - **Goal:** Client with RetryPolicy retries 503 and eventually succeeds
  - **Files:** crates/oxihttp/tests/retry_test.rs
- [x] streaming large response body (1MB) (done 2026-05-25)
  - **Goal:** body_stream() streams 1MB response in chunks
  - **Files:** crates/oxihttp/tests/streaming_test.rs
- [x] router path parameter extraction
- [x] router nested routing dispatch
- [x] CORS preflight request handling
- [x] body size limit enforcement (413 response)
- [x] rate limiting (429 response) (test_rate_limiting_returns_429: first request 200, second immediate 429)
- [x] static file serving with Content-Type detection
- [x] WebSocket upgrade and echo roundtrip (5 tests: text, binary, ping/pong, fragments, invalid)
- [x] SSE event stream delivery
- [x] automatic response decompression via oxiarc (done 2026-05-25: with_decompression() API wired; decompression feature-gated via oxiarc-deflate)
- [x] concurrent client requests to same host (test_concurrent_requests: 10 parallel tasks all 200 OK)
- [x] full facade API roundtrip (get, post convenience functions)

## Performance
- [x] GET request latency: HTTP/1.1 plaintext, HTTPS/TLS 1.3, HTTP/2 (planned 2026-05-25)
  - **Goal:** Criterion benchmark measuring single GET round-trip latency for each transport
  - **Files:** `crates/oxihttp/benches/client_latency.rs`
- [x] Request throughput: requests/second under sustained load (planned 2026-05-25)
  - **Goal:** Criterion benchmark counting completed requests in a fixed 1-second window
  - **Files:** `crates/oxihttp/benches/client_latency.rs`
- [x] Connection pool hit ratio and amortized connection cost (planned 2026-05-25)
  - **Goal:** Criterion benchmark comparing cold-start (new client per iter) vs warm-pool (single client N requests) amortized cost
  - **Files:** `crates/oxihttp/benches/client_latency.rs`
- [x] Large response body throughput (10MB, 100MB) (planned 2026-05-25)
  - **Goal:** Criterion benchmark with Throughput::Bytes reporting MB/s for 10MB and 100MB response bodies
  - **Files:** `crates/oxihttp/benches/client_body.rs`
- [x] Router dispatch latency (10, 100, 1000 routes) (planned 2026-05-25)
  - **Goal:** Criterion benchmark using Router::resolve() (no I/O) measuring O(n) scan worst-case/best-case/miss
  - **Files:** `crates/oxihttp/benches/server_dispatch.rs`, `crates/oxihttp-server/src/router.rs` (adds resolve())
- [x] Middleware pipeline overhead (0 to 10 layers) (planned 2026-05-25)
  - **Goal:** Full-stack criterion benchmark comparing round-trip latency with 0, 1, 3, 5 middleware layers
  - **Files:** `crates/oxihttp/benches/server_dispatch.rs`
- [x] Static file serving throughput (planned 2026-05-25)
  - **Goal:** Criterion benchmark with Throughput::Bytes for 1KB/100KB/1MB files via ServeDir
  - **Files:** `crates/oxihttp/benches/server_files.rs`
- [x] WebSocket message throughput (planned 2026-05-25)
  - **Goal:** Criterion benchmark measuring messages/sec (64B text) and data throughput (1KB/64KB binary)
  - **Files:** `crates/oxihttp/benches/websocket_bench.rs`
- [x] Memory usage under 1000+ concurrent connections (planned 2026-05-25)
  - **Goal:** Custom-harness bench with TrackingAllocator measuring peak allocation under 1000 concurrent TCP connections
  - **Files:** `crates/oxihttp/benches/memory_bench.rs`
- [x] Facade convenience overhead (`oxihttp::get()` vs direct `Client::builder().build()`) (2026-05-26)
  - **Goal:** Criterion benchmark comparing facade `get()` vs manual client construction+send
  - **Files:** `crates/oxihttp/benches/facade_bench.rs`
- [x] Default client construction time benchmark (2026-05-26)
  - **Goal:** Criterion benchmark measuring `Client::builder().build()` cost (~944 ns)
  - **Files:** `crates/oxihttp/benches/facade_bench.rs`

## Integration
- [x] Wire oxitls for HTTPS client and server (2026-05-25)
  - **Implemented:** TlsConfig in crates/oxihttp-server/src/tls.rs; ServerBuilder::with_tls() and with_tls_from_pem(); auto::Builder replaces http1::Builder; feature-gate "tls" on oxihttp-server
- [x] Wire oxitls webpki roots as default client trust store
- [x] Wire oxitls ALPN for HTTP/2 protocol negotiation (2026-05-25)
  - **Implemented:** Replaced http1::Builder with auto::Builder in accept_loop; server auto-negotiates h1/h2; TLS accept wraps TCP before auto::Builder; h2 ALPN test in http2_test.rs
- [x] Wire oxitls mTLS for server-side client certificate verification (done 2026-05-25)
  - **Goal:** Server verifies client certs during TLS handshake; handlers access peer cert info via `Request::peer_certificates()`
  - **Design:** `TlsConfig::with_client_auth(cert_pem, key_pem, client_ca_pem)`; accept_loop extracts peer certs from TLS stream before serving; injects `Arc<PeerCertInfo>` into request extensions; `Request::tls_info()` and `Request::peer_certificates()` accessors; both tower and non-tower accept loops updated
  - **Files:** crates/oxihttp-server/src/tls.rs (PeerCertInfo + with_client_auth), crates/oxihttp-server/src/lib.rs (peer cert extraction), crates/oxihttp-server/src/router.rs (tls_info/peer_certificates methods)
  - **Tests:** crates/oxihttp/tests/mtls_test.rs — 4 tests: positive mTLS (handler sees peer_certificates), rejection (no client cert → server closes), bad PEM error, plain HTTP tls_info=None
  - **Note:** Also added `generate_ca_signed_client_cert` to oxitls_rcgen for generating ClientAuth-EKU certs
- [x] Wire tower middleware layers for client and server pipelines (done 2026-05-25)
  - **Goal:** Both client and server use composable tower Layer stacks; `RouterService` implements `tower::Service`; `ServerBuilder::with_layer()` composes Layers; `LoggingLayer` and `RequestIdLayer` ship as concrete middleware
  - **Design:** `RouterService` wraps `Arc<Router>`; `MiddlewarePipeline` retained for backward compat; `with_layer()` on ServerBuilder folds layers via `ErasedLayer`/`BoxCloneService`; `service_fn` in accept_loop delegates to layered service; feature-gated `tower`
  - **Files:** crates/oxihttp-server/src/tower_compat.rs (new), crates/oxihttp-server/src/tower_middleware.rs (new), crates/oxihttp-server/src/lib.rs, crates/oxihttp-server/Cargo.toml
  - **Tests:** crates/oxihttp/tests/tower_test.rs (server with LoggingLayer + RequestIdLayer, verify X-Request-Id header)
- [x] Wire oxiarc-deflate for compression/decompression (never flate2)
- [x] Wire oxiquic-h3 for HTTP/3 behind `h3` feature flag (done 2026-05-30)
- [x] Ensure deny.toml bans ring, aws-lc-rs, openssl, native-tls tree-wide (verified 2026-05-25)
  - All four bans confirmed in deny.toml (plus openssl-sys and aws-lc-sys for completeness)

## Milestones
- [x] M0: Workspace skeleton
- [x] M1: HTTP/1.1 plaintext client (with redirects, retries, timeouts, JSON/form support)
- [x] M2: HTTPS + HTTP/2 server TLS complete (2026-05-25)
- [x] M3: HTTP/1.1 server (routing, middleware, CORS, rate limiting, body limits)
- [x] M4: tower-http middleware + Service impl (done 2026-05-25)
  - **Goal:** Milestone: tower middleware wired into both client and server pipelines
  - **Design:** Block A (server): RouterService + ErasedLayer + BoxCloneService + LoggingLayer/RequestIdLayer; Block B (client): ClientMiddleware trait + LoggingMiddleware/TimingMiddleware; feature-gated `tower` on server
  - **Files:** Multiple (see tower middleware blocks above)
  - **Tests:** crates/oxihttp/tests/tower_test.rs (6 tests: 4 client + 2 server)
- [x] M5: Proxy (HTTP CONNECT + SOCKS5) + server ergonomics + core primitives + facade polish (planned 2026-05-25)
  - HTTP/3 available via `h3` feature flag (done 2026-05-30)


---

<!-- production-readiness-backlog 2026-07-16 -->
## Production-Readiness Backlog — 2026-07-16

_Consolidated from static audit + Opus adversarial bug-hunt (48 verified defects across noffi) + baseline nextest/clippy + design investigation. See `../NOFFI_PRODUCTION_BACKLOG.md` for the full cross-project list and severity/model legend. All four items below shipped in commit `050c754` ("bump oxitls"), an ancestor of the current `0.2.1` branch HEAD — see CHANGELOG.md's `[0.2.1]` entry for the user-facing summary._

**Confirmed bugs — Opus-verified:**
- [x] **S · high** `oxihttp-client/src/lib.rs:590` — redirect loop re-sends Authorization/Cookie to redirect target across different host/scheme without stripping → credential leak. R2/N0 — **fixed** (commit `050c754`): `same_origin()` gates Authorization/Cookie retention on scheme+host match; regression tests `redirect_cross_host_strips_credentials`, `redirect_cross_scheme_strips_credentials`, `same_origin_is_case_insensitive` in `oxihttp-client/src/lib.rs`.
- [x] **S · high** `oxihttp-server/src/ws.rs:146` — WebSocket reassembly accumulates fragments into `frag_buf` with no total-size cap → unbounded memory DoS. R2/N0 — **fixed** (commit `050c754`): `WebSocket::set_max_message_size` + `check_reassembly_size` guard; a subsequent pass additionally closed the single-unfragmented-frame variant of the same gap (`MIN_FRAME_PAYLOAD_CAP` / `check_size_limit` in `oxihttp-server/src/ws.rs`, see CHANGELOG).
- [x] **S · med** `oxihttp-server/src/middleware.rs:164` — BodyLimit only checks Content-Length; `Transfer-Encoding: chunked` bypasses → downstream buffers whole body uncapped. R2/N0 — **fixed** (commit `050c754`): `MiddlewarePipeline::inject_body_limit` + `Request::body_bytes`'s `collect_body_limited` enforce the cap on decoded (not just declared) size.

**Designed / audit:**
- [x] **B/med · W1** doctests/examples expansion + h1 parser fuzz + document BoxError bounds policy. — **fixed** (commit `050c754`): `crates/oxihttp/examples/{client_requests,server_router}.rs`, BoxError policy documented in `oxihttp-core/src/error.rs`, proptest fuzz harnesses in `crates/oxihttp/tests/{server_fuzz_test,client_fuzz_test,client_response_fuzz_test}.rs`. Coverage-guided (`cargo-fuzz`) targets for the same parsers were added separately — see `fuzz/` and CHANGELOG.md.
