# oxihttp-server TODO

## Status
Fully-implemented HTTP/1.1 server (~1600 SLOC) with routing, middleware pipeline,
graceful shutdown, static file serving with ETag/range support, compression (oxiarc),
and SSE. WebSocket, virtual host, TLS server-side, and HTTP/2 server deferred.

## Core Implementation
- [x] Implement `Server` struct wrapping hyper server with TCP listener (~150 SLOC)
- [x] Implement `Server::bind(addr: &str) -> ServerBuilder` binding to a socket address (~40 SLOC)
- [x] Implement `ServerBuilder::serve(handler) -> impl Future` starting the server event loop (~200 SLOC)
- [x] Implement handler trait: `async fn(Request<Incoming>) -> Result<Response<Body>, Infallible>` pattern (~30 SLOC)
- [x] Implement `ServerBuilder::with_tls(oxitls_server_config)` for HTTPS support via oxitls (~150 SLOC)
- [x] Implement `ServerBuilder::with_tls_from_pem(cert_pem, key_pem)` convenience for PEM cert loading (~40 SLOC)
- [x] Implement `ServerBuilder::with_alpn(protocols)` for ALPN negotiation (h2, http/1.1) (~30 SLOC)
- [x] Implement HTTP/2 support: automatic protocol upgrade when ALPN negotiates h2 (~100 SLOC)
- [x] Implement graceful shutdown: `ServerBuilder::with_graceful_shutdown(signal)` draining active connections (~120 SLOC)
- [x] Implement `ctrl_c` shutdown signal: `Server::shutdown_on_ctrl_c()` convenience (~30 SLOC)
- [x] Implement request routing: `Router::new().route(path, handler).route(path, handler)` (~300 SLOC)
- [x] Implement path parameter extraction: `/users/:id` pattern with `PathParams` extractor (~150 SLOC)
- [x] Implement query parameter extraction: `QueryParams` from URI query string (~80 SLOC)
- [x] Implement method-based routing: `Router::get(path, handler)`, `.post()`, `.put()`, `.delete()` (~60 SLOC)
- [x] Implement route grouping: `Router::nest(prefix, sub_router)` for modular route trees (~80 SLOC)
- [x] Implement wildcard routes: `/static/*path` matching any suffix (~50 SLOC)
- [x] Implement middleware pipeline: `ServerBuilder::with_layer(impl Layer)` via tower (~100 SLOC)
- [x] Implement CORS middleware: `CorsLayer` with configurable origins, methods, headers, max-age (~200 SLOC)
- [x] Implement request body size limit: `BodyLimitLayer(max_bytes)` rejecting oversized requests with 413 (~80 SLOC)
- [x] Implement rate limiting: `RateLimitLayer` with token bucket algorithm per IP or per route (~250 SLOC)
- [x] Implement request logging: `LoggingLayer` with method, path, status, duration (~80 SLOC)
- [x] Implement static file serving: `ServeDir::new(path)` handler for directory listings and file downloads (~200 SLOC)
- [x] Implement `ServeFile::new(path)` for single-file serving with Content-Type detection (~60 SLOC)
- [x] Implement ETag/If-None-Match support for static files: 304 Not Modified responses (~80 SLOC)
- [x] Implement compression middleware: `CompressionLayer` using oxiarc-deflate (never flate2/tower-http compression) (~150 SLOC)
- [x] Implement WebSocket upgrade: `ws::upgrade(request) -> (WebSocketUpgrade, Response)` (~300 SLOC) (done 2026-05-25)
- [x] Implement WebSocket message types: Text, Binary, Ping, Pong, Close (~80 SLOC) (done 2026-05-25)
- [x] Implement WebSocket frame reading/writing: RFC 6455 codec with masking, fragmentation, control frames (~200 SLOC) (done 2026-05-25)
- [x] Implement virtual host support: `ServerBuilder::with_virtual_host(hostname, router)` (~100 SLOC)
- [x] Implement request timeout: `TimeoutLayer(duration)` returning 408 on timeout (~60 SLOC)
- [x] Implement health check endpoint: `Router::health(path)` returning 200 OK with configurable body (~30 SLOC)
- [x] Implement `ServerBuilder::with_max_connections(n)` limiting concurrent connections (~40 SLOC)
- [x] Implement `ServerBuilder::with_tcp_keepalive(duration)` for TCP-level keepalive (~20 SLOC)
- [x] Implement `ServerBuilder::with_http2_settings(H2Settings)` for server-side HTTP/2 tuning (~30 SLOC)
- [x] Implement JSON response helper: `Json(value)` returning application/json with serialization (~40 SLOC)
- [x] Implement form response helper: `Form(value)` for URL-encoded responses (~30 SLOC)
- [x] Implement SSE (Server-Sent Events): `Sse::new(stream)` producing text/event-stream responses (~150 SLOC)

## API Improvements
- [x] Add `Router::fallback(handler)` for custom 404 responses
- [x] Add `Router::method_not_allowed(handler)` for custom 405 responses
- [x] Add typed header extraction via `TypedHeader<T>` extractor
  - **Goal:** Full extractor framework: `Header` trait in core + `FromRequestParts` + `TypedHeader<H>` + `Request::extract::<T>()` method
  - **Files:** oxihttp-core/src/header_types.rs (new), oxihttp-server/src/extractor.rs (new), router.rs, both lib.rs files
- [x] Add `Extension<T>` extractor for sharing state across handlers
- [x] Add `State<T>` extractor for application state injection
- [x] Add `Server::local_addr() -> SocketAddr` for retrieving the bound address
- [x] Add `Server::into_make_service()` for advanced tower Service integration
- [x] Add request ID middleware: `RequestIdLayer` generating unique IDs per request
- [x] Implement `Display` for `Router` showing registered routes

## Testing
- [x] Integration test: GET handler returns 200 + JSON body
  - **Goal:** New test: GET handler returns json_response(...), client asserts 200 + valid JSON + correct Content-Type
  - **Files:** crates/oxihttp/tests/server_test.rs (append)
- [x] Integration test: POST handler receives body and returns processed response
  - **Evidence:** `test_server_post_json_handler` → server_test.rs
- [x] Integration test: path parameter extraction from `/users/:id`
  - **Evidence:** `test_server_path_params` → server_test.rs
- [x] Integration test: query parameter parsing from `?key=value`
  - **Evidence:** `test_server_query_params` → server_test.rs
- [x] Integration test: nested router with prefix routing
  - **Evidence:** `test_server_nested_router` → server_test.rs
- [x] Integration test: HTTPS server with rcgen self-signed cert via oxitls
  - **Evidence:** `test_https_server_http1` → http2_test.rs
- [x] Integration test: HTTP/2 negotiation via ALPN h2
- [x] Integration test: graceful shutdown drains active requests
  - **Goal:** New test: with_graceful_shutdown(), slow handler, fire signal mid-flight, assert in-flight completes 200, subsequent refused
  - **Files:** crates/oxihttp/tests/server_test.rs (append)
- [x] Integration test: CORS preflight OPTIONS request returns correct headers
  - **Evidence:** `test_server_cors_preflight` → server_test.rs, `test_cors_preflight_options` → middleware_test.rs
- [x] Integration test: body size limit rejects oversized POST with 413
  - **Evidence:** `test_server_body_limit` → server_test.rs
- [x] Integration test: rate limiting returns 429 after threshold
  - **Evidence:** `test_rate_limiting_returns_429` → server_test.rs
- [x] Integration test: static file serving with correct Content-Type
  - **Evidence:** `test_serve_file_200` → static_files_test.rs
- [x] Integration test: ETag/304 response for unchanged static files
  - **Evidence:** `test_if_none_match_304` → static_files_test.rs
- [x] Integration test: WebSocket upgrade + echo round-trip (5 tests: text echo, binary echo, ping/pong, fragment reassembly, invalid upgrade) (done 2026-05-25)
- [x] Integration test: SSE event stream delivers 3 events then closes
  - **Evidence:** `test_sse_stream_three_events` → sse_test.rs
- [x] Integration test: compression middleware compresses response body
  - **Evidence:** `test_gzip_response` → compression_test.rs
- [x] Integration test: virtual host routing dispatches to correct router
  - **Evidence:** `test_virtual_host_dispatch` → server_test.rs
- [x] Integration test: request timeout returns 408
  - **Evidence:** `test_timeout_interrupts_slow_handler` → middleware_test.rs
- [x] Integration test: max_connections limit rejects excess connections
  - **Goal:** New test: with_max_connections(1), occupy one connection, second attempt returns 503 or refused
  - **Files:** crates/oxihttp/tests/server_test.rs (append)
- [x] Integration test: full client-server roundtrip with oxihttp-client
  - **Evidence:** `test_full_client_server_roundtrip` → server_test.rs
- [x] Fuzz test: malformed HTTP/1.1 requests do not panic
  - **Goal:** proptest: random byte sequences sent as raw TCP, assert server does not panic
  - **Files:** crates/oxihttp/tests/server_fuzz_test.rs (new)

## Performance
- [x] Benchmark: request routing latency (10, 100, 1000 routes)
  - **Files:** crates/oxihttp-server/benches/server_bench.rs (Criterion), crates/oxihttp-server/benches/server_memory.rs (TrackingAllocator for memory/accept items)
- [x] Benchmark: request throughput (requests/second, HTTP/1.1 plaintext)
  - **Files:** crates/oxihttp-server/benches/server_bench.rs (Criterion), crates/oxihttp-server/benches/server_memory.rs (TrackingAllocator for memory/accept items)
- [x] Benchmark: request throughput (requests/second, HTTPS/TLS 1.3)
  - **Files:** crates/oxihttp-server/benches/server_bench.rs (Criterion), crates/oxihttp-server/benches/server_memory.rs (TrackingAllocator for memory/accept items)
- [x] Benchmark: HTTP/2 concurrent stream throughput
  - **Files:** crates/oxihttp-server/benches/server_bench.rs (Criterion), crates/oxihttp-server/benches/server_memory.rs (TrackingAllocator for memory/accept items)
- [x] Benchmark: static file serving throughput (1KB, 100KB, 10MB files)
  - **Files:** crates/oxihttp-server/benches/server_bench.rs (Criterion), crates/oxihttp-server/benches/server_memory.rs (TrackingAllocator for memory/accept items)
- [x] Benchmark: middleware pipeline overhead (0, 1, 5, 10 layers)
  - **Files:** crates/oxihttp-server/benches/server_bench.rs (Criterion), crates/oxihttp-server/benches/server_memory.rs (TrackingAllocator for memory/accept items)
- [x] Benchmark: WebSocket message throughput (messages/second)
  - **Files:** crates/oxihttp-server/benches/server_bench.rs (Criterion), crates/oxihttp-server/benches/server_memory.rs (TrackingAllocator for memory/accept items)
- [x] Benchmark: JSON serialization/deserialization in handlers
  - **Files:** crates/oxihttp-server/benches/server_bench.rs (Criterion), crates/oxihttp-server/benches/server_memory.rs (TrackingAllocator for memory/accept items)
- [x] Profile: memory usage under 10000 concurrent connections
  - **Files:** crates/oxihttp-server/benches/server_bench.rs (Criterion), crates/oxihttp-server/benches/server_memory.rs (TrackingAllocator for memory/accept items)
- [x] Profile: connection accept latency under load
  - **Files:** crates/oxihttp-server/benches/server_bench.rs (Criterion), crates/oxihttp-server/benches/server_memory.rs (TrackingAllocator for memory/accept items)

## Integration
- [x] Wire oxitls `ServerBuilder` into HTTPS support (done 2026-05-25)
- [x] Wire oxitls ALPN configuration for h2 protocol negotiation (done 2026-05-25)
- [x] Wire oxitls mTLS into per-route client certificate verification (done 2026-05-25)
  - `TlsConfig::with_client_auth(cert_pem, key_pem, client_ca_pem)` — server requires client certs
  - `PeerCertInfo` struct extracted in accept_loop after TLS handshake
  - `Request::tls_info()` / `Request::peer_certificates()` accessors (feature-gated `tls`)
- [x] Wire tower middleware layers into server pipeline (done 2026-05-25)
- [x] Wire oxiarc-deflate into compression middleware (never flate2)
- [x] Coordinate `Body` type with `oxihttp-core::Body`
- [x] Coordinate error types with `oxihttp-core::OxiHttpError`
- [x] Wire into `oxihttp` facade with `server` feature flag
- [x] Provide HTTP/3 server path via `oxiquic-h3` behind `h3` feature (done 2026-05-30)
- [x] Coordinate WebSocket with `oxitls` for WSS (WebSocket over TLS)
  - **Goal:** WSS smoke test: TLS server + WS upgrade handler; raw tokio-rustls client runs WS handshake + text echo over TLS stream
  - **Files:** crates/oxihttp/tests/wss_test.rs (new, feature-gated tls+server)
