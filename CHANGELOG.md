# Changelog

All notable changes to OxiHTTP are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/) and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.1] - 2026-08-07

### Security

- **Redirect credential leak**: the redirect-following loop no longer re-sends the
  `Authorization` or `Cookie` header to a redirect target on a different host or scheme.
  A new `same_origin()` check (scheme + host, case-insensitive) gates retention of both
  headers across every hop; cookies re-added from the cookie jar are still scoped
  per-target-URL and are unaffected (`oxihttp-client`).
- **WebSocket unbounded-memory DoS (reassembly)**: fragmented message reassembly is now
  bounded by `WebSocket::set_max_message_size` (default 16 MiB) — exceeding it clears the
  reassembly buffer and fails the connection with a typed error instead of growing
  `frag_buf` without limit (`oxihttp-server`, `websocket` feature).
- **WebSocket unbounded-memory DoS (single frame)**: the same budget now also bounds a
  single **unfragmented** data frame, closing the variant where a peer sent one large
  frame (up to the 64 MiB wire-level ceiling) to bypass `set_max_message_size` entirely.
  The wire-level per-frame cap passed to `ws_frame::read_frame` is derived from
  `max_message_size`, floored at 125 bytes (`MIN_FRAME_PAYLOAD_CAP`) so legal
  Ping/Pong/Close control frames (RFC 6455 §5.5, never subject to `max_message_size`)
  stay deliverable even under a very small configured budget.
- **WebSocket protocol conformance**: the server now rejects unmasked client-to-server
  frames per RFC 6455 §5.1 instead of silently accepting them, closing both an Autobahn
  conformance gap and the anti-cache-poisoning property masking exists for.
- **Chunked-encoding `BodyLimit` bypass**: the request body size limit previously only
  checked the declared `Content-Length` header, so `Transfer-Encoding: chunked` requests
  bypassed it entirely. `MiddlewarePipeline::inject_body_limit` now injects the configured
  limit into request extensions on every accept path, and `Request::body_bytes` (via
  `collect_body_limited`) enforces it against the actually-decoded size, not just the
  declared one (`oxihttp-server`).
- **Rate limiter: spoofable key + unbounded bucket growth**: the token-bucket rate
  limiter previously keyed exclusively on the client-controlled `X-Forwarded-For` header
  with a shared `"unknown"` fallback (so, absent a reverse proxy, every client shared one
  bucket and could 429 everyone else) and never evicted buckets. It now defaults to the
  accepted TCP peer address; `X-Forwarded-For` is only honored when a trusted reverse
  proxy is explicitly configured via the new `MiddlewarePipeline::with_trusted_proxy_headers`
  (last-hop entry only). Buckets idle longer than 5 minutes are reclaimed by a periodic
  sweep, and total tracked buckets are hard-capped at 100,000 (`RateLimiter::with_limits`),
  bounding worst-case memory even under attacker-driven key churn (`oxihttp-server`).
- **Client response body / decompression bomb**: `Response::body_bytes()` (and
  `body_text()`/`body_json()`) previously collected the wire body with no size limit and,
  with `with_decompression(true)`, fed it to one-shot decompressors with no output cap.
  Response collection is now bounded by `ClientBuilder::with_max_response_body` (default
  64 MiB, override per-request via `RequestBuilder::max_response_body`), and gzip/zlib
  decoding uses the bounded streaming decoder (CRC-32 verified) instead of the unbounded
  one-shot API, so neither an oversized response nor a small highly-compressible payload
  can force unbounded client-side allocation (`oxihttp-client`, `decompression` feature).
- **Silent compressed-bytes-as-plaintext bug**: building a client with
  `with_decompression(true)` while the `decompression` Cargo feature is off previously
  still advertised `Accept-Encoding: gzip, deflate` and, if a server complied, handed the
  still-compressed wire bytes back from `body_bytes()` as if they were plaintext (a
  confusing UTF-8/JSON-parse failure downstream, or worse). `Accept-Encoding` is now only
  advertised when this build can actually decode the response.
- **`ServeDir`/`ServeFile` memory-per-request**: file bodies (full responses and byte
  ranges alike) are now streamed from disk in bounded chunks (see the internal
  `FileRangeStream`) instead of read into memory up front, so serving a multi-gigabyte
  file — or many concurrent range requests against one — no longer allocates the file's
  full size per request. ETags switched from a content hash (`sha2`, which required
  reading the whole file) to file metadata (mtime + length, the nginx/tower-http
  convention), which is what makes the streaming possible without paying for a hash on
  every request (`oxihttp-server`, `static-files` feature).
- **CORS caching correctness**: `CorsConfig::apply_headers` now always ensures the
  response's `Vary` header includes an `Origin` token when the allowlist is not the bare
  wildcard (`["*"]`) — including on the "origin not on the allowlist" path — so a shared
  cache or CDN in front of the server cannot serve one origin's CORS headers (or lack
  thereof) to a different origin. The token is appended to (not substituted for) any
  existing `Vary` value, so a `Vary: Accept-Encoding` set by e.g. the `Compression`
  middleware on the same response is preserved rather than overwritten, and repeated calls
  do not duplicate the token. A `debug_assert!` also now catches the
  wildcard-origin-plus-credentials misconfiguration that every browser rejects outright
  (Fetch §3.2.3) during development/tests.
- **`ServeDir` symlink escapes (opt-in fix)**: `ServeDir`'s traversal check is lexical
  only (documented, matches nginx/tower-http default behavior) and does not resolve
  symlinks, so a symlink placed inside the served root pointing outside it was — and by
  default still is — followed and served. `ServeDir::with_symlink_protection(true)` now
  re-validates the canonicalized (symlink-resolved) file path against the served root and
  returns `403 Forbidden` on an escape, for operators serving directories untrusted users
  can write to.
- **Multipart `Content-Disposition`/`Content-Type` parameter injection**: `Part::text`,
  `Part::file`, and both `add_file_stream` constructors previously interpolated a
  caller-supplied `name`/`filename`/`content_type` directly into the serialized header
  line with no escaping. A literal `"` could break out of the `Content-Disposition`
  quoted-string parameter, and a literal CR/LF — plausible for either value, since both
  commonly originate from attacker-influenced input such as a browser-reported upload
  filename or MIME type — had no valid representation in a header field value at all and
  could inject an arbitrary extra header line or a forged part boundary into the body.
  `"` and `\` are now backslash-escaped (RFC 9110 §5.6.4 `quoted-string` grammar) and
  CR/LF are stripped rather than escaped (there is no valid escape for a raw control
  character in a header value). `Part::custom` remains the documented, unsanitized escape
  hatch for callers who need fully custom headers (`oxihttp-core`).

### Added

- `WebSocket::set_max_message_size(usize)` — configure the reassembly/single-frame memory
  budget described above (`oxihttp-server`, `websocket` feature).
- `ClientBuilder::with_max_response_body(usize)` and
  `RequestBuilder::max_response_body(usize)` (per-request override), plus the
  `DEFAULT_MAX_RESPONSE_BODY` (64 MiB) constant (`oxihttp-client`).
- `RateLimiter::with_limits(max_tokens, refill_rate, idle_timeout, max_buckets)` for
  explicit bucket-eviction tuning, `RateLimiter::bucket_count()` for tests/metrics, and
  `MiddlewarePipeline::with_trusted_proxy_headers(bool)` to opt into trusting
  `X-Forwarded-For` (`oxihttp-server`).
- `MultipartBuilder::add_stream_part` / `add_file_stream` and the resulting
  `StreamingMultipart` type (`boundary`, `content_type`, `add_text`, `add_file`,
  `add_part`, `add_stream_part`, `add_file_stream`, `build_stream`) — a zero-copy
  multipart body builder whose parts (including a caller-supplied async byte stream for
  the file part) are streamed rather than fully buffered before sending. Re-exported from
  `oxihttp-core` and `oxihttp`.
- `RequestBuilder::multipart_stream(StreamingMultipart)` (`oxihttp-client`, re-exported from
  `oxihttp`) — wires `StreamingMultipart` into the client's wire path: a large file part
  streams directly to a real HTTP/1 or HTTP/2 connection (via `Transfer-Encoding: chunked`,
  since the total size isn't known up front) instead of being concatenated into one
  in-memory buffer first, the way `.multipart()` still does. Because the underlying stream
  is one-shot, a `multipart_stream` request always sends exactly once (bypassing
  `RetryPolicy`) and returns a typed `OxiHttpError::Body` instead of resending if it hits a
  body-preserving (307/308) redirect. Also adds `Client::execute_body(Request<Body>)`, the
  streaming-capable counterpart to `Client::execute` for callers building requests
  directly rather than through `RequestBuilder`. `Client`'s underlying `hyper_util` client
  is now generic over `oxihttp_core::PinnedBody` instead of a fixed `Full<Bytes>`, which is
  what makes the streaming path possible; `Client::execute`'s signature and behavior are
  unchanged.
- `ServeDir::with_symlink_protection(bool)` (`oxihttp-server`, `static-files` feature).
- `SECURITY.md` and `CONTRIBUTING.md` at the repository root.
- `crates/oxihttp/examples/client_requests.rs` and `crates/oxihttp/examples/server_router.rs`
  — runnable end-to-end examples.
- Property-based (proptest) fuzz harnesses for the HTTP/1 parser and client response
  handling: `crates/oxihttp/tests/{server_fuzz_test,client_fuzz_test,client_response_fuzz_test}.rs`.
- Coverage-guided `cargo-fuzz` targets under `fuzz/` (separate, unpublished workspace):
  `ws_frame_read` (WebSocket frame codec), `cookie_parse` (`Set-Cookie` parser),
  `range_header` (HTTP `Range` header, exercised end-to-end through `ServeFile::serve`),
  and `multipart_build` (`MultipartBuilder` — arbitrary field/filename/content-type/body
  bytes must never panic the serializer).
- `rustfmt.toml` and `clippy.toml` at the repository root, pinning formatting and lint
  thresholds (`clippy.toml`'s `msrv` matches the workspace `rust-version`, 1.80).
- README.md MSRV table documenting the per-feature MSRV floors raised by `tls` (1.89),
  `compression`/`decompression` (1.85), and `h3` (1.85, since the `oxiquic` 0.2.1 bump
  below raised it from the 1.80 default floor); mirrored in CONTRIBUTING.md.

### Changed

- Workspace crate versions bumped to 0.2.1 (`oxihttp-core`, `oxihttp-client`,
  `oxihttp-server`, `oxihttp`).
- Shared crate description corrected from "reqwest/hyper-**free** facade" (inaccurate —
  `hyper`/`hyper-util` are unconditional dependencies of `oxihttp-client`/`oxihttp-server`)
  to "a reqwest-**compatible** facade over hyper", matching README.md and the code. The
  Pure-Rust guarantee (no C/C++/Fortran in the default feature set) is unaffected — `hyper`
  is itself Pure Rust.
- `static-files` feature now explicitly requires `tokio/fs` (previously relied on it being
  pulled in transitively by some other enabled feature, which meant the feature could fail
  to build in isolation) and depends on `futures-core` instead of `sha2` (no longer needed
  now that ETags are metadata-based, see Security above).
- `oxiarc-core` added as an explicit workspace/optional dependency of the `decompression`
  feature, backing CRC-32 verification for the bounded streaming gzip decompression path.
- **Sibling COOLJAPAN dependencies bumped** to their latest published releases:
  - `oxitls` / `oxitls-core`: `0.2.0` → `0.3.0` (via an intermediate `0.2.1` hop).
    Informational: `oxitls 0.2.0` (the version this crate was published against) still
    resolved a `rustls-webpki` 0.102.x edge affected by **RUSTSEC-2026-0104** (a
    CRL-parsing panic) through its default RustCrypto provider chain; `oxitls` fixed this
    at `0.2.1` by forking that dependency out into `oxitls-rustcrypto-provider` (see
    oxitls's own `CHANGELOG.md` `[0.2.1]`). Bumping past that floor to `^0.3.0` here
    clears the advisory path for `oxihttp` once this release publishes — no code change
    was required on this side, and no advisory was previously suppressed in this crate's
    own `deny.toml`.
  - `oxiarc-deflate`: `0.3.3` → `0.4.1` (via `0.3.4`, `0.3.5`, `0.3.6`, `0.4.0`); `oxiarc-core`
    (see above) likewise pinned at `0.4.1`.
  - `oxiquic-h3` / `oxiquic-crypto`: `0.2.0` → `0.2.1`.

### Fixed

- **Private intra-doc links in public rustdoc** (`oxihttp-server`): `apply_headers`,
  `RateLimiter::with_limits`, and the `static_files` module-level docs linked to the
  private `add_vary_origin`, `SWEEP_INTERVAL`, and `is_path_safe` items respectively via
  `` [`item`] `` syntax, which `cargo doc --all-features` (`-D warnings`) rejects as
  `rustdoc::private_intra_doc_links`. Reworded as plain `` `item` `` code spans (no link
  resolution attempted) — no behavior or public API change.

[0.2.1]: https://github.com/cool-japan/oxihttp/releases/tag/v0.2.1

## [0.2.0] - 2026-06-22

### Changed

- Updated `oxitls` dependency to `^0.2.0` (was `^0.1.3`); `oxitls-core` likewise updated to
  `^0.2.0`. This tracks the oxitls 0.2.0 release which restricts `webpki-roots` to the
  pure Mozilla-curated root store, removing the previous conditional native-cert path.
- Workspace crate versions bumped to 0.2.0 (`oxihttp-core`, `oxihttp-client`,
  `oxihttp-server`, `oxihttp`).

### Security

- **Clears L1 PENDING-REPUBLISH**: The prior `oxitls ^0.1.3` dependency carried a known
  L1 violation — `oxitls-webpki-roots` could fall back to native OS certificate stores,
  leaking system-CA trust into what should be a pure Mozilla-roots bundle. `oxitls 0.2.0`
  fixes this at the source: the `webpki-roots` feature now unconditionally uses only the
  Mozilla root set embedded in the crate. No API changes in oxihttp itself.

[0.2.0]: https://github.com/cool-japan/oxihttp/releases/tag/v0.2.0

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
