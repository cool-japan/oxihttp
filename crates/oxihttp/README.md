# oxihttp — The COOLJAPAN Pure-Rust HTTP facade

[![Crates.io](https://img.shields.io/crates/v/oxihttp.svg)](https://crates.io/crates/oxihttp)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxihttp` is the top-level façade for the OxiHTTP stack. A single dependency gives you a complete, Pure-Rust HTTP client **and** server, with a unified type surface re-exported from [`oxihttp-core`](https://crates.io/crates/oxihttp-core), [`oxihttp-client`](https://crates.io/crates/oxihttp-client), and [`oxihttp-server`](https://crates.io/crates/oxihttp-server). Feature flags compose the stack: client and server are on by default; TLS, compression, WebSockets, Server-Sent Events, Tower integration, SOCKS5, and HTTP/3 are opt-in.

The entire stack is Pure Rust — no OpenSSL, no C/C++ libraries. TLS is provided by [`oxitls`](https://crates.io/crates/oxitls) + `rustls`, compression by `oxiarc-deflate`, and HTTP/3 by `oxiquic-h3`. The crate is `#![forbid(unsafe_code)]`, and ships a `reqwest → OxiHTTP` migration guide in its `migration` module.

## Installation

```toml
[dependencies]
# Client + server (defaults)
oxihttp = "0.1.0"

# Client only
oxihttp = { version = "0.1.0", default-features = false, features = ["client"] }

# Server only
oxihttp = { version = "0.1.0", default-features = false, features = ["server"] }

# With HTTPS (client + server TLS)
oxihttp = { version = "0.1.0", features = ["tls"] }

# Everything
oxihttp = { version = "0.1.0", features = ["all"] }
```

## Quick Start

### Client

```rust,no_run
# async fn example() -> oxihttp::Result<()> {
let client = oxihttp::Client::builder().build()?;
let resp = client.get("http://example.com")?.send().await?;
println!("Status: {}", resp.status());
let body = resp.body_text().await?;
println!("Body: {body}");
# Ok(())
# }
```

### Server

```rust,no_run
use oxihttp::{Server, Router, response};

# async fn example() -> oxihttp::Result<()> {
let router = Router::new()
    .get("/", |_req| async { response::text_response("Hello!") });

Server::bind("127.0.0.1:3000").serve(router).await?;
# Ok(())
# }
```

### One-shot helpers

```rust,no_run
# async fn example() -> oxihttp::Result<()> {
let resp = oxihttp::get("http://example.com").await?;
let resp = oxihttp::post("http://example.com/x", "payload").await?;
# Ok(())
# }
```

## Top-level entry points

### Client functions *(feature `client`)*

| Function | Description |
|----------|-------------|
| `get(url)` | One-shot GET with a temporary default client |
| `post(url, body)` | One-shot POST |
| `put(url, body)` | One-shot PUT |
| `delete(url)` | One-shot DELETE |

For repeated requests, build and reuse a `Client` via `Client::builder()`.

### Misc

| Item | Description |
|------|-------------|
| `version() -> &'static str` | Crate version at runtime |
| `Result<T>` | Alias for `Result<T, OxiHttpError>` |
| `Error` | Alias for `OxiHttpError` |
| `Request` | Re-export of `http::Request` |
| `RawResponse` | Re-export of `http::Response` (the raw `http` type) |

## Re-exported types at crate root

### From `oxihttp-core` (always available)

`Body`, `PinnedBody`, `Bytes`, `BytesMut`, `ContentType`, `Cookie`, `CookieJar`, `SameSite`, `FormBody`, `MultipartBuilder`, `MultipartPart`, `HeaderMap`, `HeaderName`, `HeaderValue`, `HeaderMapExt`, `Method`, `StatusCode`, `Uri`, `UriExt`, `Version`, `OxiHttpError`.

### From `oxihttp-client` *(feature `client`)*

`Client`, `ClientBuilder`, `RequestBuilder`, `Response`, `BodyStream`, `RedirectPolicy`, `RetryPolicy`.

With feature `tls`: `HttpsClient`, `OxiHttpsConnector`, `MaybeHttpsStream`, `RequestTlsConfig`.

### From `oxihttp-server` *(feature `server`)*

`Server`, `ServerBuilder`, `Router`, `ServerRequest` (the server's `Request`), `CorsConfig`, `RateLimiter`, and the `response` helper module.

With features `tls` + `server`: `TlsConfig`.

## Module map

| Module | Feature | Contents |
|--------|---------|----------|
| `oxihttp::response` | `server` | Response builders (`text_response`, `json_response`, …) |
| `oxihttp::tls` | `tls` + `server` | `TlsConfig`, `PeerCertInfo` |
| `oxihttp::ws` | `websocket` | `upgrade`, `WebSocket`, `WebSocketUpgrade`, `Message`, `CloseFrame` |
| `oxihttp::middleware` | `tower` | Client `ClientMiddleware`/`LoggingMiddleware`/`TimingMiddleware`; server `LoggingLayer`/`RequestIdLayer` |
| `oxihttp::h3` | `h3` | `H3Connection`, `H3ConnectionBuilder`, `H3Server`, `H3Request`, `H3Response` |
| `oxihttp::migration` | — | `reqwest → OxiHTTP` migration guide (docs only) |
| `oxihttp::prelude` | — | The most common types (see below) |

## Prelude

```rust
use oxihttp::prelude::*;
```

Imports `Bytes`, `HeaderMap`, `HeaderValue`, `Method`, `StatusCode`, `Uri`, `OxiHttpError`, `Result`; plus `Client` and `Response` (feature `client`) and `Router` and `Server` (feature `server`).

## Feature Flags

| Feature | Default | Enables |
|---------|---------|---------|
| `client` | ✓ | HTTP client (pooling, redirects, retries) via `oxihttp-client` |
| `server` | ✓ | HTTP server (routing, middleware, graceful shutdown) via `oxihttp-server` |
| `tls` | ✗ | HTTPS client **and** server TLS via `oxitls`/`rustls` (implies `client`) |
| `compression` | ✗ | Server response compression (implies `server`) |
| `decompression` | ✗ | Client auto-decompression (implies `client`) |
| `static-files` | ✗ | `ServeDir`/`ServeFile` static serving (implies `server`) |
| `sse` | ✗ | Server-Sent Events (implies `server`) |
| `tower` | ✗ | Tower middleware integration (implies `server`) |
| `websocket` | ✗ | RFC 6455 WebSocket (implies `server`) |
| `socks` | ✗ | SOCKS5 proxy client support (implies `client`) |
| `h3` | ✗ | HTTP/3 client + server via `oxiquic-h3` (implies `client` + `server`) |
| `all` | ✗ | All of the above |

## Migration from reqwest

The `oxihttp::migration` module documents how common `reqwest` patterns map onto OxiHTTP — client setup, GET/POST, JSON bodies, headers, and timeouts — to ease porting existing code to the Pure-Rust stack.

## Architecture

OxiHTTP is layered:

```
            oxihttp  (this facade)
           /         \
 oxihttp-client     oxihttp-server
           \         /
            oxihttp-core  (shared types: Body, OxiHttpError, headers, cookies, …)
```

- [`oxihttp-core`](https://crates.io/crates/oxihttp-core) — foundational types shared by every layer.
- [`oxihttp-client`](https://crates.io/crates/oxihttp-client) — the HTTP client implementation.
- [`oxihttp-server`](https://crates.io/crates/oxihttp-server) — the HTTP server implementation.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)
