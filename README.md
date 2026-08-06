# OxiHTTP

**OxiHTTP** is the COOLJAPAN Pure-Rust HTTP stack — a `reqwest`/`hyper` facade for the COOLJAPAN
ecosystem. It provides HTTP/1.1, HTTP/2, and HTTP/3 support without any C/C++/Fortran dependencies
in the default feature set.

[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust: 1.80+](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://www.rust-lang.org)

## Crates

| Crate | Description |
|-------|-------------|
| `oxihttp-core` | Core types: `OxiHttpError`, `Body`, `CookieJar`, `ContentType`, `HeaderMapExt`, `UriExt`, `RequestBuilder`, `ResponseExt`, `HttpVersion` |
| `oxihttp-client` | Async HTTP client: HTTP/1.1, HTTPS, HTTP/2, redirects, retries, proxies, decompression, streaming |
| `oxihttp-server` | HTTP server: routing, CORS, rate-limiting, SSE, WebSocket, static files, tower middleware, mTLS |
| `oxihttp` | Facade re-exporting the full API with `get`/`post`/`put`/`delete` convenience functions |

## Quick Start

```rust
use oxihttp::prelude::*;

// One-shot GET
let resp = oxihttp::get("http://httpbin.org/get").await?;
println!("{}", resp.status());

// Full client with TLS and retries
let client = Client::builder()
    .with_tls()
    .with_retry(RetryPolicy::default())
    .build()?;

let body: serde_json::Value = client
    .get("https://httpbin.org/json")
    .send()
    .await?
    .json()
    .await?;
```

## Server Example

```rust
use oxihttp::prelude::*;

let router = Router::new()
    .get("/", |_req| async {
        ok_response("Hello, world!")
    })
    .post("/echo", |req| async move {
        let body = req.body_bytes().await?;
        Ok(Response::new(Full::from(body)))
    });

Server::builder()
    .bind("0.0.0.0:8080")
    .serve(router)
    .await?;
```

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `client` | yes | HTTP client (oxihttp-client) |
| `server` | yes | HTTP server (oxihttp-server) |
| `tls` | no | HTTPS/TLS via oxitls (Pure-Rust, rustls-based) |
| `decompression` | no | Auto-decompress gzip/deflate via oxiarc-deflate |
| `compression` | no | Server-side gzip/deflate compression |
| `static-files` | no | Static file serving with ETag + byte-range |
| `sse` | no | Server-Sent Events |
| `tower` | no | tower `Service` + `Layer` integration |
| `websocket` | no | RFC 6455 WebSocket + WSS |
| `socks` | no | SOCKS5 proxy support |
| `h3` | no | HTTP/3 via oxiquic (QUIC transport) |
| `all` | no | All features combined |

## Client Features

- **HTTP/1.1 + HTTPS + HTTP/2** via `hyper` 1.x + `hyper-util` connection pool
- **HTTP/3** (feature-gated `h3`) via `oxiquic-h3`
- **TLS** via `oxitls` / `tokio-rustls` — Pure-Rust, no openssl/native-tls
- **Per-request TLS override** — custom trust anchors per request
- **TLS 0-RTT early data** — `with_early_data()` for resumed sessions
- **TLS key logging** — `with_key_log_file()` for Wireshark/tcpdump analysis
- **Redirect policy** — follow (configurable limit), none, manual
- **Retry policy** — configurable retryable status codes + exponential backoff
- **Timeouts** — global and per-request connect / response timeouts
- **HTTP CONNECT proxy** — HTTPS-over-proxy via `ProxyConnector`
- **SOCKS5 proxy** — RFC 1928/1929, server-side DNS, user/pass auth
- **Auto-decompression** — gzip/deflate via `oxiarc-deflate`
- **Streaming** — `Response::body_stream()` for large downloads
- **Cookie management** — `Response::cookies()` parses `Set-Cookie` headers
- **Tower middleware** — `ClientBuilder::with_layer()` for `LoggingMiddleware`, `TimingMiddleware`
- **Custom DNS resolver** — `with_resolver()` for test isolation

## Server Features

- **Routing** — literal, `/:param`, `/*wildcard`, nested, virtual host
- **State injection** — `Router::with_state::<T>()` + `req.state::<T>()`
- **Graceful shutdown** — drains active requests on signal
- **HTTPS + HTTP/2** — `oxitls`/`tokio-rustls` + `hyper-util::auto::Builder`
- **mTLS** — client cert verification + `req.peer_certificates()`
- **CORS** — preflight handling, configurable origins/methods/headers
- **Rate limiting** — token bucket per source IP
- **Body size limit** — configurable maximum request body size
- **Compression** — gzip/deflate via `oxiarc-deflate`
- **Static files** — ETag, conditional GET, byte-range, path-traversal guard
- **SSE** — `Server-Sent Events` stream
- **WebSocket** — RFC 6455, all opcodes, masking, fragmentation; WSS supported
- **Tower integration** — `RouterService implements tower::Service`, `ServerBuilder::with_layer()`
- **Built-in layers** — `LoggingLayer`, `RequestIdLayer`, `TimingLayer`
- **Max connections** — semaphore-guarded listener
- **TCP tuning** — keepalive, nodelay, buffer sizes via `socket2`

## TLS

TLS uses **oxitls** (COOLJAPAN Pure-Rust TLS stack based on `rustls`). No `native-tls`,
`openssl`, or C-backed crypto in the default feature set. The `webpki-roots` trust store
is bundled. Custom CA bundles and PEM/DER certificate loading are supported.

## Compression

All compression/decompression uses **oxiarc-deflate** (COOLJAPAN Pure-Rust compression).
`flate2`, `zstd`, and `brotli` are never introduced.

## HTTP/3

HTTP/3 is available behind the `h3` feature flag and uses **oxiquic-h3** for the QUIC
transport layer. Requires a valid TLS configuration (QUIC mandates TLS 1.3).

## Migration from reqwest

See the rustdoc for `oxihttp::migration` — a mapping of common reqwest patterns to their
OxiHTTP equivalents.

## MSRV

The default feature set (`client` + `server`, no `tls`/`h3`/`compression`/`decompression`)
requires **Rust 1.80+** (`rust-version` in the workspace `Cargo.toml`). Optional features that
pull in a sibling COOLJAPAN crate raise the effective floor for that feature only:

| Feature | Effective MSRV | Why |
|---------|----------------|-----|
| `tls` | 1.89 | `oxitls`/`oxitls-core`'s declared `rust-version` |
| `h3` | 1.85 | `oxiquic-h3`/`oxiquic-crypto`'s declared `rust-version` (raised from 1.80 as of `oxiquic` 0.2.1) |
| `compression` / `decompression` | 1.85 | `oxiarc-deflate`/`oxiarc-core`'s declared `rust-version` |

Verified 2026-08-07 against the actual dependency tree resolved by
`cargo tree --features <feature>` and each sibling crate's manifest as published to
crates.io at the version pinned in this crate's own `Cargo.toml` — not merely a sibling
repo's current in-progress checkout, which can be ahead of what it last published.
`cargo`'s dependency resolver enforces each floor automatically: building with one of
these features on a toolchain below its floor fails at `cargo build`/`cargo check` time
with an explicit `rust-version` resolver error — it does not silently miscompile.
Re-verify this table whenever `oxitls`, `oxiquic-h3`/`oxiquic-crypto`, or
`oxiarc-deflate`/`oxiarc-core` are bumped in `Cargo.toml`.

## Status

**v0.2.1** — production-ready for HTTP/1.1, HTTPS, and HTTP/2 workloads.
All milestones M0–M5 complete: 320 tests with default features, 446 with `--all-features`
(plus 56 doctests), 0 failures (`cargo nextest run` / `cargo test --doc`, 2026-08-07).
Security: clears L1 PENDING-REPUBLISH — oxitls 0.2.0 uses pure Mozilla root store exclusively.

| Milestone | Status |
|-----------|--------|
| M0 — Workspace skeleton | Complete |
| M1 — HTTP/1.1 client | Complete |
| M2 — HTTPS + HTTP/2 | Complete |
| M3 — HTTP server | Complete |
| M4 — Tower middleware | Complete |
| M5 — Proxy + WebSocket + SSE + Ergonomics | Complete |
| HTTP/3 (h3 feature) | Complete |

## License

Apache-2.0 — Copyright COOLJAPAN OU (Team Kitasan)
