# oxihttp-server — Pure-Rust HTTP server for the OxiHTTP stack

[![Crates.io](https://img.shields.io/crates/v/oxihttp-server.svg)](https://crates.io/crates/oxihttp-server)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxihttp-server` is the HTTP server of the OxiHTTP stack. It serves HTTP/1.1 and HTTP/2 (auto-negotiated) with a path-parameter router, virtual hosts, nested routers, typed request extraction, application state injection, graceful shutdown, connection limits, TCP/HTTP-2 tuning, and a built-in middleware pipeline (CORS, body-size limits, token-bucket rate limiting, timeouts). Optional features add response compression, static-file serving, Server-Sent Events, WebSockets, Tower-layer integration, mutual-TLS, and HTTP/3.

The server builds on `hyper` and `hyper-util`'s auto connection builder; all shared types (`OxiHttpError`, `Body`, typed headers, content negotiation) come from [`oxihttp-core`](https://crates.io/crates/oxihttp-core). TLS is Pure Rust via [`oxitls`](https://crates.io/crates/oxitls) + `rustls`/`tokio-rustls` (no OpenSSL); compression is Pure Rust via `oxiarc-deflate`; HTTP/3 is Pure Rust via `oxiquic-h3`. The crate is `#![forbid(unsafe_code)]`.

## Installation

```toml
[dependencies]
# Core HTTP/1.1 + HTTP/2 server
oxihttp-server = "0.1.0"

# With TLS, compression, static files, and WebSockets
oxihttp-server = { version = "0.1.0", features = ["tls", "compression", "static-files", "websocket"] }
```

## Quick Start

```rust,no_run
use oxihttp_server::{Server, Router, response};

# async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
let router = Router::new()
    .get("/hello", |_req| async {
        response::text_response("Hello, World!")
    })
    .get("/users/:id", |req| async move {
        let id = req.param("id").unwrap_or("?").to_string();
        response::text_response(format!("user {id}"))
    })
    .health("/health");

Server::bind("127.0.0.1:3000")
    .serve(router)
    .await?;
# Ok(())
# }
```

### Graceful shutdown and a JSON handler

```rust,no_run
use oxihttp_server::{Server, Router, response};

# async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
let router = Router::new().post("/echo", |req| async move {
    let value: serde_json::Value = req.body_json().await?;
    response::json_response(&value)
});

Server::bind("127.0.0.1:0")
    .with_max_connections(1024)
    .shutdown_on_ctrl_c()
    .serve(router)
    .await?;
# Ok(())
# }
```

## API Overview

### `Server` / `ServerBuilder` / `BoundServer`

`Server::bind(addr) -> ServerBuilder` starts configuration.

| `ServerBuilder` method | Description |
|------------------------|-------------|
| `with_cors(CorsConfig)` | Add CORS middleware |
| `with_body_limit(u64)` | Reject bodies over `n` bytes (413) |
| `with_rate_limiter(RateLimiter)` | Token-bucket rate limiting |
| `with_timeout(Duration)` | Per-request processing timeout |
| `with_max_connections(usize)` | Cap concurrent connections (semaphore) |
| `with_tcp_nodelay(bool)` / `with_tcp_keepalive(Duration)` | TCP socket tuning |
| `with_http2_settings(ServerHttp2Settings)` | HTTP/2 connection tuning |
| `with_graceful_shutdown(Future)` / `shutdown_on_ctrl_c()` | Stop accepting on a signal |
| `with_layer(L)` *(feature `tower`)* | Add a `tower_layer::Layer` |
| `with_tls(TlsConfig)` *(feature `tls`)* | Use a pre-built TLS config |
| `with_tls_from_pem(cert, key)` *(tls)* | Build TLS from PEM bytes |
| `with_tls_from_der(certs, key)` *(tls)* | Build TLS from DER |
| `with_alpn(protocols)` *(tls)* | Set ALPN (call before `with_tls_from_*`) |
| `listen() -> BoundServer` | Bind eagerly to inspect the address |
| `serve(Router)` | Run until shutdown / fatal error |
| `serve_with_addr(Router)` | Return the bound `SocketAddr` + a `JoinHandle` |

`BoundServer` exposes `local_addr() -> SocketAddr` and `serve(Router)` — useful when binding port 0 in tests.

### `ServerHttp2Settings`

A `Default` struct of optional HTTP/2 knobs: `initial_stream_window_size`, `initial_connection_window_size`, `adaptive_window`, `max_concurrent_streams`, `max_frame_size`, `keep_alive_interval`, `keep_alive_timeout`.

### `Router` (`router` module)

| Method | Description |
|--------|-------------|
| `new()` | Empty router (also `Default`) |
| `route(Method, path, handler)` | Register a route for any method |
| `get` / `post` / `put` / `delete` / `patch` / `head` (path, handler) | Method shortcuts |
| `nest(prefix, Router)` | Mount a sub-router under a prefix |
| `host(host, Router)` | Virtual-host dispatch (case-insensitive, port-stripped) |
| `fallback(handler)` | Custom 404 handler |
| `method_not_allowed(handler)` | Custom 405 handler |
| `health(path)` | 200 OK health-check route |
| `with_state(T)` | Inject `Arc<T>` into every request (inherited by nested/vhost routers) |
| `resolve(&Method, path)` | Match without dispatching (introspection/bench) |
| `dispatch(req) -> DispatchFuture` | Dispatch a hyper request |
| `route_count()` | Number of top-level routes |
| `into_make_service()` *(feature `tower`)* | Wrap as a `RouterMakeService` |

Path patterns support literal segments, `:param` parameters, and `*wildcard` (matches the remainder). `Router` implements `Display` (route listing) and `Debug`.

Public type aliases: `HandlerFn`, `DispatchFuture<'a>`.

### `Request` (`router` module)

The handler argument. Path-params + query plus body consumers.

| Method | Description |
|--------|-------------|
| `method()` / `uri()` / `headers()` / `path()` | Request line + headers |
| `param(name)` / `params()` | Path parameters |
| `query_params()` / `query(name)` | Query-string parameters (percent-decoded) |
| `body_bytes()` / `body_text()` / `body_json::<T>()` | Consume the body |
| `into_inner()` | Recover the raw `hyper::Request<Incoming>` |
| `state::<T>()` | Retrieve injected `Arc<T>` |
| `extension::<T>()` / `extensions()` / `extensions_mut()` | Per-request extensions |
| `parts()` / `extract::<T>()` | Typed extraction (see below) |
| `negotiate(&[ContentType])` | Content negotiation from `Accept` |
| `tls_info()` / `peer_certificates()` / `tls_connection_info()` *(feature `tls`)* | TLS/mTLS connection info |

### Extraction (`extractor` module)

| Item | Description |
|------|-------------|
| `RequestParts<'a>` | Non-body view (`method`, `uri`, `headers`, `path_params`) |
| `FromRequestParts` (trait) | `type Rejection: Into<OxiHttpError>` + `from_request_parts` |
| `TypedHeader<H: Header>` | Extracts and decodes a typed header from `oxihttp-core` |

### Response helpers (`response` module)

Free functions returning `Result<hyper::Response<Full<Bytes>>, OxiHttpError>`:

| Function | Result |
|----------|--------|
| `json_response(&T)` | 200 + `application/json` |
| `json_response_with_status(status, &T)` | Custom status + JSON |
| `text_response(impl Into<String>)` | 200 + `text/plain` |
| `html_response(impl Into<String>)` | 200 + `text/html` |
| `form_response(FormBody)` | 200 + `application/x-www-form-urlencoded` |
| `redirect_response(location)` | 302 Found + `Location` |
| `empty_response(status)` | Empty body |
| `no_content()` | 204 No Content |
| `bad_request(msg)` | 400 Bad Request |
| `internal_error(msg)` | 500 Internal Server Error |

### Middleware (`middleware` module)

| Type | Description |
|------|-------------|
| `CorsConfig` | Fields for origins/methods/headers/credentials/max-age; `permissive()`, `with_origins(..)`, `apply_headers`, `preflight_response` |
| `BodyLimitConfig` | `new(max_bytes)`, `check_content_length` |
| `RateLimiter` | Token bucket; `new(max_tokens, refill_rate)`, `check(key)`, `too_many_requests()` |
| `TimeoutConfig` | `new(Duration)`, `timeout_response()` |

These are composed internally via the `MiddlewarePipeline` configured by the `ServerBuilder::with_*` methods.

### Compression — *feature `compression`* (`compression` module)

| Item | Description |
|------|-------------|
| `CompressionAlgorithm` | `Gzip` / `Deflate`; `as_str()` |
| `CompressionConfig` | `min_size`, `algorithms`, `level` (`Default`: 1 KiB, gzip+deflate, level 6) |
| `Compression` | `new()`, `min_size(n)`, `level(n)`, `algorithms(..)`, `apply(accept_encoding, response)` |

### Static files — *feature `static-files`* (`static_files` module)

| Item | Description |
|------|-------------|
| `ServeDir` | `new(root)`, `with_index`, `with_fallback`, `with_cache_control`, `add_mime_override`, `serve(method, path, headers)`. ETag + If-Modified-Since + single-range + path-traversal protection |
| `ServeFile` | `new(path)`, `with_cache_control`, `with_mime`, `serve(..)` |

### Server-Sent Events — *feature `sse`* (`sse` module)

| Item | Description |
|------|-------------|
| `SseEvent` | `data(..)`, `with_id`, `with_event`, `with_retry`, `encode()`; public fields `id`/`event`/`data`/`retry` |
| `SseSender` | `send(SseEvent)` (async), `try_send(SseEvent)` |
| `SseResponse` | `channel(buffer) -> (SseSender, SseResponse)`, `with_heartbeat(interval)`, `into_response() -> Response<Body>` |

### WebSockets — *feature `websocket`* (`ws` module)

| Item | Description |
|------|-------------|
| `upgrade(Request) -> (WebSocketUpgrade, Response<Full<Bytes>>)` | RFC 6455 handshake (101 response) |
| `WebSocketUpgrade` | `accept()` → `WebSocket` once the connection upgrades |
| `WebSocket<S>` | `recv()`, `send(Message)`, `close(code, reason)` |
| `Message` | `Text(String)`, `Binary(Vec<u8>)`, `Ping(Vec<u8>)`, `Pong(Vec<u8>)`, `Close(Option<CloseFrame>)` |
| `CloseFrame` | `code: u16`, `reason: String` |

### Tower integration — *feature `tower`* (`tower_compat`, `tower_middleware` modules)

| Item | Description |
|------|-------------|
| `RouterMakeService` | Service factory from a `Router` (also via `Router::into_make_service()`) |
| `LoggingLayer` | `tower_layer::Layer` logging method/path/status/elapsed |
| `RequestIdLayer` | Injects an incrementing `x-request-id` header |

### TLS — *feature `tls`* (`tls` module)

| Item | Description |
|------|-------------|
| `TlsConfig` | `new(rustls::ServerConfig)`, `from_pem` / `from_der` (+ `_with_alpn`), `with_client_auth(cert, key, client_ca)` for mTLS, `http2_defaults` / `http2_defaults_from_der`, `from_pem_with_key_log` / `from_der_with_key_log` |
| `PeerCertInfo` | Per-connection TLS facts: `peer_certificates`, `alpn_protocol`, `protocol_version`, typed `version`, `cipher_suite`, `sni` |

### HTTP/3 — *feature `h3`* (`h3` module)

| Item | Description |
|------|-------------|
| `H3Server` | `bind(..)`, `local_addr()`, `serve(handler)` — QUIC/HTTP-3 server via `oxiquic-h3` |

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `compression` | off | gzip/deflate response compression via `oxiarc-deflate` |
| `static-files` | off | `ServeDir`/`ServeFile` with ETag + range support |
| `sse` | off | Server-Sent Events |
| `tls` | off | HTTPS + mTLS via `oxitls` + `rustls`/`tokio-rustls` (Pure Rust) |
| `tower` | off | Tower `Layer` integration (`with_layer`, `LoggingLayer`, `RequestIdLayer`) |
| `websocket` | off | RFC 6455 WebSocket upgrades |
| `h3` | off | HTTP/3 server via `oxiquic-h3` (pulls in `rustls`) |

## Error type

Handlers and builder methods return `oxihttp_core::OxiHttpError`. The server commonly produces `Server`, `Io`, `Http`, `Body`, `Json`, `Tls`, `RouteNotFound`, and `MethodNotAllowed`; see the [`oxihttp-core` README](https://crates.io/crates/oxihttp-core) for the full variant table and the `status_code()` mapping.

## Related crates

- [`oxihttp`](https://crates.io/crates/oxihttp) — the unified facade re-exporting this server.
- [`oxihttp-core`](https://crates.io/crates/oxihttp-core) — shared types (`OxiHttpError`, `Body`, typed headers, negotiation).
- [`oxihttp-client`](https://crates.io/crates/oxihttp-client) — the matching HTTP client.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)
