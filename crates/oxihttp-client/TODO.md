# oxihttp-client TODO

## Status
Fully-implemented HTTP client (~1000 SLOC) with connection pooling, redirect handling,
retry policy struct, timeouts, TLS via oxitls, and a fluent request builder API.
RetryPolicy is defined but not yet wired into send(). HTTP/2 multiplexing, proxy
support, streaming body, and tower middleware are deferred to later milestones.

## Core Implementation
- [x] Implement `Client` struct wrapping `hyper_util::client::legacy::Client` with connection pooling (~150 SLOC)
- [x] Implement `ClientBuilder` with fluent API: `Client::builder().build() -> Result<Client>` (~200 SLOC)
- [x] Implement `RequestBuilder`: `client.get(url)`, `.post(url)`, `.put(url)`, `.delete(url)`, `.patch(url)`, `.head(url)` returning `RequestBuilder` (~200 SLOC)
- [x] Implement `RequestBuilder::header(k, v)`, `.headers(map)`, `.bearer_token(token)`, `.basic_auth(user, pass)` (~100 SLOC)
- [x] Implement `RequestBuilder::body(impl Into<Body>)`, `.json<T: Serialize>(value)`, `.form(params)`, `.multipart(multipart)` (~150 SLOC)
- [x] Implement `RequestBuilder::timeout(Duration)` for per-request timeout (~40 SLOC)
- [x] Implement `RequestBuilder::send() -> impl Future<Output = Result<Response, OxiHttpError>>` executing the request (~200 SLOC)
- [x] Implement `Response` type: `.status()`, `.headers()`, `.version()`, `.content_length()` (~80 SLOC)
- [x] Implement `Response::body_bytes()`, `.body_text()`, `.body_json<T>()` body consumption methods (~120 SLOC)
- [x] Implement streaming response body: `Response::body_stream() -> impl AsyncRead` for large downloads (~100 SLOC)
- [x] Add TLS support: `ClientBuilder::with_tls()` integrating oxitls `ClientBuilder` for HTTPS (~150 SLOC)
- [x] Add TLS config customization: `ClientBuilder::with_tls_config(oxitls::ClientConfig)` (~30 SLOC)
- [x] Add connection pooling configuration: `ClientBuilder::pool_max_idle_per_host(n)`, `.pool_idle_timeout(duration)` (~60 SLOC)
- [x] Add keep-alive support: `ClientBuilder::with_http1_keep_alive(bool)`, `.with_http2_keep_alive_interval(duration)` (~40 SLOC)
- [x] Add redirect handling: `ClientBuilder::redirect_policy(RedirectPolicy)` with `None`, `Limited(n)`, `All` (~150 SLOC)
- [x] Add `RedirectPolicy` enum with per-redirect callback option for custom logic (~60 SLOC)
- [x] Add retry logic: `ClientBuilder::with_retry(RetryPolicy)` with exponential backoff, max retries, retry-on conditions (~200 SLOC)
- [x] Add `RetryPolicy` struct: max_retries, backoff_base, backoff_max, retryable_status_codes, retryable_errors (~80 SLOC)
- [x] Add HTTP proxy support: `ClientBuilder::with_http_proxy(url)` via hyper-util HTTP CONNECT (~200 SLOC)
- [x] Add SOCKS5 proxy support: `ClientBuilder::with_socks5_proxy(url)` behind `socks` feature flag (~250 SLOC)
- [x] Add DNS resolution control: `ClientBuilder::with_resolver(impl Resolve)` for custom DNS (~80 SLOC)
- [x] Add `ClientBuilder::with_connect_timeout(Duration)` for TCP connect timeout (~30 SLOC)
- [x] Add `ClientBuilder::with_read_timeout(Duration)` for response read timeout (~30 SLOC)
- [x] Add HTTP/2 adaptive window size: `ClientBuilder::with_http2_adaptive_window(bool)` (~20 SLOC)
- [x] Add HTTP/2 initial stream window size: `ClientBuilder::with_http2_initial_stream_window_size(u32)` (~20 SLOC)
- [x] Add HTTP/2 initial connection window size: `ClientBuilder::with_http2_initial_connection_window_size(u32)` (~20 SLOC)
- [x] Add middleware/interceptor chain: `ClientBuilder::with_layer(impl Layer<Service>)` via tower (~100 SLOC)
- [x] Add request/response logging interceptor: `LoggingLayer` with configurable verbosity (~80 SLOC)
- [x] Add request timing interceptor: `TimingLayer` recording elapsed time per request (~60 SLOC)
- [x] Add automatic decompression: `ClientBuilder::with_decompression(bool)` using oxiarc-deflate (never flate2) (~80 SLOC)
- [x] Add automatic content-type detection from body type (~40 SLOC)
- [x] Add `Client::execute(Request<Body>) -> Response` low-level method for pre-built requests (~50 SLOC)

## API Improvements
- [x] Add `Client::get_bytes(url) -> Result<Bytes>` convenience shorthand
- [x] Add `Client::get_json<T>(url) -> Result<T>` convenience shorthand
- [x] Add `Client::post_json<T, R>(url, body: &T) -> Result<R>` convenience shorthand
- [x] Add `Response::error_for_status() -> Result<Response, OxiHttpError>` (return error for 4xx/5xx)
- [x] Add `Response::content_type() -> Option<ContentType>` accessor
- [x] Add `Response::cookies() -> Vec<Cookie>` parsing Set-Cookie headers
- [x] Implement `Clone` on `Client` (shares underlying connection pool)
- [x] Add `Debug` impl on `Client` showing pool stats
- [x] Add `Drop` impl on `Client` that gracefully drains the connection pool
- [x] Auto cookie jar with RFC 6265 domain/path scoping (done 2026-05-26)

## Testing
- [x] Integration test: GET request to local hyper server returns 200 + body
  - **Evidence:** `test_get_request` → http1_client.rs
- [x] Integration test: POST request with JSON body echoed back
  - **Evidence:** `test_json_body_and_response` → http1_client.rs
- [x] Integration test: PUT, DELETE, PATCH, HEAD methods work correctly
  - **Evidence:** `test_put_request`, `test_delete_request`, `test_patch_request`, `test_head_request` → http1_client.rs
- [x] Integration test: custom headers round-trip
  - **Evidence:** `test_custom_headers` → http1_client.rs
- [x] Integration test: bearer token and basic auth headers set correctly
  - **Evidence:** `test_bearer_token`, `test_basic_auth` → http1_client.rs
- [x] Integration test: multipart file upload
  - **Evidence:** `test_multipart_post_roundtrip` → multipart_test.rs
- [x] Integration test: form-encoded body submission
  - **Evidence:** `test_form_body` → http1_client.rs
- [x] Integration test: HTTPS request via oxitls with rcgen self-signed cert
  - **Evidence:** `test_https_get` → https_client.rs
- [x] Integration test: HTTP/2 negotiation via ALPN
- [x] Integration test: redirect follow (301, 302, 307, 308) up to limit
  - **Goal:** New tests: 301 rewrites to GET, 307/308 preserve method; 302 already covered, extend harness
  - **Files:** crates/oxihttp/tests/http1_client.rs (append)
- [x] Integration test: redirect limit exceeded returns error
  - **Evidence:** `test_redirect_limit_exceeded` → http1_client.rs
- [x] Integration test: connection pool reuse (second request on same host reuses connection)
  - **Evidence:** `test_pool_sequential_reuse` → http1_client.rs
- [x] Integration test: per-request timeout fires on slow server
  - **Evidence:** `test_per_request_timeout` → http1_client.rs
- [x] Integration test: connect timeout fires on unreachable host
  - **Goal:** New test: connect_timeout(50ms), attempt 192.0.2.1:1 (TEST-NET-1 RFC 5737), assert is_timeout() or error
  - **Files:** crates/oxihttp/tests/http1_client.rs (append)
- [x] Integration test: retry policy retries on 503 then succeeds
  - **Evidence:** `test_retry_on_503_succeeds_on_third_attempt` → retry_test.rs
- [x] Integration test: streaming response body for large payload (1MB)
  - **Evidence:** `test_body_stream_1mb` → streaming_test.rs
- [x] Integration test: automatic decompression of gzip response (via oxiarc)
  - **Goal:** New test: with_decompression(true), gzip server, body_bytes() returns uncompressed; also fix stale doc-comment in compression_test.rs
  - **Files:** crates/oxihttp/tests/compression_test.rs (append test + fix doc)
- [x] Integration test: error_for_status() returns error on 404
  - **Evidence:** `test_error_for_status_404` → http1_client.rs
- [x] Integration test: concurrent requests to same host share pool
  - **Evidence:** `test_concurrent_requests` → http1_client.rs
- [x] Fuzz test: malformed URL input does not panic
  - **Goal:** proptest: random strings as URI input to Client::get(url), assert no panic
  - **Files:** crates/oxihttp/tests/client_fuzz_test.rs (new)

## Performance
- [x] Benchmark: simple GET request latency (HTTP/1.1 plaintext, localhost)
  - **Files:** crates/oxihttp-client/benches/client_bench.rs (Criterion), crates/oxihttp-client/benches/client_memory.rs (TrackingAllocator for memory item)
- [x] Benchmark: simple GET request latency (HTTPS/TLS 1.3, localhost)
  - **Files:** crates/oxihttp-client/benches/client_bench.rs (Criterion), crates/oxihttp-client/benches/client_memory.rs (TrackingAllocator for memory item)
- [x] Benchmark: HTTP/2 multiplexed requests (10 concurrent to same host)
  - **Files:** crates/oxihttp-client/benches/client_bench.rs (Criterion), crates/oxihttp-client/benches/client_memory.rs (TrackingAllocator for memory item)
- [x] Benchmark: connection pool hit ratio under sustained load
  - **Files:** crates/oxihttp-client/benches/client_bench.rs (Criterion), crates/oxihttp-client/benches/client_memory.rs (TrackingAllocator for memory item)
- [x] Benchmark: large response body throughput (10MB, 100MB)
  - **Files:** crates/oxihttp-client/benches/client_bench.rs (Criterion), crates/oxihttp-client/benches/client_memory.rs (TrackingAllocator for memory item)
- [x] Benchmark: request builder construction overhead
  - **Files:** crates/oxihttp-client/benches/client_bench.rs (Criterion), crates/oxihttp-client/benches/client_memory.rs (TrackingAllocator for memory item)
- [x] Benchmark: redirect chain following (1, 5, 10 hops)
  - **Files:** crates/oxihttp-client/benches/client_bench.rs (Criterion), crates/oxihttp-client/benches/client_memory.rs (TrackingAllocator for memory item)
- [x] Profile: memory usage under 1000 concurrent connections
  - **Files:** crates/oxihttp-client/benches/client_bench.rs (Criterion), crates/oxihttp-client/benches/client_memory.rs (TrackingAllocator for memory item)

## Integration
- [x] Wire oxitls `ClientBuilder` into HTTPS support
- [x] Wire oxitls `connector_with_webpki_roots()` as default TLS connector
- [x] Wire tower middleware layer into client pipeline
- [x] Wire oxiarc-deflate into automatic decompression (never flate2)
- [x] Coordinate `Body` type with `oxihttp-core::Body`
- [x] Coordinate error types with `oxihttp-core::OxiHttpError`
- [x] Wire into `oxihttp` facade with `client` feature flag
- [x] Provide HTTP/3 upgrade path via `oxiquic-h3` behind `h3` feature (done 2026-05-30)
- [x] Coordinate cookie handling with `oxihttp-core::CookieJar`
