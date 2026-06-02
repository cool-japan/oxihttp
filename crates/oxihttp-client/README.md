# oxihttp-client — Pure-Rust HTTP client for the OxiHTTP stack

[![Crates.io](https://img.shields.io/crates/v/oxihttp-client.svg)](https://crates.io/crates/oxihttp-client)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxihttp-client` is the HTTP client of the OxiHTTP stack. It provides a fluent, `reqwest`-style API over a pooled connection layer: connection pooling, redirect following, retry with exponential backoff, per-request timeouts, automatic cookie management (RFC 6265), a lightweight middleware chain, custom DNS resolution, HTTP/HTTPS CONNECT and SOCKS5 proxies, optional transparent decompression, and optional HTTP/3.

The client builds on `hyper` and `hyper-util`'s legacy pooled client for the HTTP/1.1 and HTTP/2 transports; all shared types (`OxiHttpError`, `Body`, cookies, multipart, forms) come from [`oxihttp-core`](https://crates.io/crates/oxihttp-core). TLS is optional and Pure Rust via [`oxitls`](https://crates.io/crates/oxitls) + `rustls`/`tokio-rustls` (no OpenSSL); decompression is Pure Rust via `oxiarc-deflate`; HTTP/3 is Pure Rust via `oxiquic-h3`. The crate is `#![forbid(unsafe_code)]`.

## Installation

```toml
[dependencies]
# Plain HTTP only
oxihttp-client = "0.1.0"

# With HTTPS (Pure-Rust rustls/oxitls)
oxihttp-client = { version = "0.1.0", features = ["tls"] }

# Everything: TLS, decompression, SOCKS5, HTTP/3
oxihttp-client = { version = "0.1.0", features = ["tls", "decompression", "socks", "h3"] }
```

## Quick Start

```rust,no_run
use oxihttp_client::Client;

# async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
let client = Client::builder().build()?;

// Fluent request building
let resp = client
    .get("http://example.com")?
    .header("accept", "application/json")?
    .send()
    .await?;

assert_eq!(resp.status(), http::StatusCode::OK);
let body = resp.body_text().await?;
println!("{body}");
# Ok(())
# }
```

### POST JSON and deserialize the response

```rust,no_run
use oxihttp_client::Client;

# async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
#[derive(serde::Serialize)]
struct CreateUser { name: String }
#[derive(serde::Deserialize)]
struct User { id: u64 }

let client = Client::builder().build()?;
let user: User = client
    .post_json("http://example.com/users", &CreateUser { name: "Alice".into() })
    .await?;
println!("created id={}", user.id);
# Ok(())
# }
```

## API Overview

### `Client<C>` — the HTTP client

Cloneable and cheap to share. The default `Client<HttpConnector>` is HTTP-only; type aliases provide TLS- and resolver-backed variants (see below).

| Method | Description |
|--------|-------------|
| `Client::builder()` | Start a `ClientBuilder` (only on the default `HttpConnector` variant, for ergonomic inference) |
| `get` / `post` / `put` / `delete` / `patch` / `head` (url) | Build a `RequestBuilder<C>` for the method |
| `execute(http::Request<Full<Bytes>>)` | Send a pre-built request |
| `get_bytes(url)` | GET → `error_for_status()` → body `Bytes` |
| `get_json::<T>(url)` | GET → deserialize JSON body |
| `post_json::<T, R>(url, &body)` | POST JSON → deserialize JSON response |
| `retry_policy()` / `connect_timeout()` / `read_timeout()` | Inspect configured settings |

### Client type aliases

| Alias | Built via | Notes |
|-------|-----------|-------|
| `Client<HttpConnector>` (default) | `ClientBuilder::build()` | Plain HTTP |
| `HttpsClient` *(feature `tls`)* | `ClientBuilder::build_https()` | `http://` + `https://` |
| `ResolverClient` | `ClientBuilder::build_with_resolver()` | Custom DNS, plain HTTP |
| `ResolverHttpsClient` *(feature `tls`)* | `ClientBuilder::build_https_with_resolver()` | Custom DNS + TLS |

### `HttpsClient` extras *(feature `tls`)*

| Method | Description |
|--------|-------------|
| `with_request_tls_config(RequestTlsConfig)` | Derive a new `HttpsClient` (own pool) with overridden TLS trust — per-endpoint certificate pinning |

### `ClientBuilder`

Construct with `Client::builder()` or `ClientBuilder::new()`. All setters are chainable.

| Method | Description |
|--------|-------------|
| `pool_max_idle_per_host(n)` | Max idle pooled connections per host |
| `pool_idle_timeout(Duration)` | Idle connection timeout |
| `connect_timeout(Duration)` | TCP connect timeout |
| `read_timeout(Duration)` | Response read timeout |
| `redirect_policy(RedirectPolicy)` | Redirect behaviour |
| `retry_policy(RetryPolicy)` | Retry behaviour |
| `default_headers(HeaderMap)` | Headers added to every request |
| `user_agent(impl Into<String>)` | Set the `User-Agent` |
| `with_decompression(bool)` | Toggle transparent decompression (needs `decompression`) |
| `with_middleware(M)` / `with_layer(M)` | Append a `ClientMiddleware` |
| `with_cookie_jar(Arc<Mutex<CookieJar>>)` / `with_new_cookie_jar()` | Enable RFC 6265 cookie management |
| `with_http2_settings(Http2Settings)` | HTTP/2 tuning |
| `with_tcp_nodelay(bool)` / `with_tcp_keepalive(Duration)` | TCP socket tuning |
| `with_resolver(R)` | Set a custom `DnsResolver` |
| `with_http_proxy(Uri)` | Route via HTTP CONNECT proxy |
| `with_socks5_proxy(Uri)` *(feature `socks`)* | Route via SOCKS5 proxy |
| **Build methods** | |
| `build()` | Plain HTTP `Client` |
| `build_https()` *(tls)* | `HttpsClient` |
| `build_with_resolver()` | `ResolverClient` |
| `build_https_with_resolver()` *(tls)* | `ResolverHttpsClient` |
| `build_proxy()` | `Client<ProxyConnector>` (HTTP CONNECT) |
| `build_proxy_https()` *(tls)* | HTTPS over HTTP CONNECT |
| `build_socks5_proxy()` *(socks)* | `Client<Socks5Connector>` |
| `build_socks5_proxy_https()` *(tls + socks)* | HTTPS over SOCKS5 |
| **TLS setters** *(feature `tls`)* | |
| `with_tls()` / `with_webpki_roots()` | Trust the Mozilla CA bundle |
| `with_trusted_cert_der(Vec<u8>)` | Add a DER-encoded trusted CA |
| `with_alpn(&[&str])` | Advertise ALPN protocols |
| `with_danger_accept_invalid_certs()` | **Testing only** — disable cert verification |
| `with_key_log_file(PathBuf)` | Write `SSLKEYLOGFILE`-format secrets (dev only) |
| `with_early_data()` | Enable TLS 1.3 0-RTT early data (idempotent requests only) |

### `RequestBuilder<C>` — per-request builder

Returned by `Client::get()` etc. Terminal method is `send()`.

| Method | Description |
|--------|-------------|
| `header(key, value)` | Add a header |
| `headers(HeaderMap)` | Merge headers |
| `bearer_token(token)` | Set `Authorization: Bearer …` |
| `basic_auth(user, Option<pass>)` | Set `Authorization: Basic …` |
| `body(impl Into<Bytes>)` | Raw request body |
| `json(&T)` | JSON body + `Content-Type: application/json` |
| `form(&FormBody)` | URL-encoded form body |
| `multipart(MultipartBuilder)` | `multipart/form-data` body (auto `Content-Type` unless preset) |
| `timeout(Duration)` | Per-request timeout |
| `send() -> Result<Response>` | Execute (honours redirects, retries, middleware, cookies) |

### `Response`

| Method | Description |
|--------|-------------|
| `status()` | `StatusCode` |
| `headers()` | `&HeaderMap` |
| `version()` | `http::Version` |
| `content_length()` | `Option<u64>` |
| `content_type()` | `Content-Type` as `&str` |
| `cookies()` | Parse all `Set-Cookie` into `Vec<oxihttp_core::Cookie>` |
| `error_for_status()` | `Err` on 4xx/5xx, else `Ok(self)` |
| `body_bytes()` | Consume body → `Bytes` (auto-decompress if enabled) |
| `body_text()` | Consume body → `String` |
| `body_json::<T>()` | Consume body → deserialized `T` |
| `body_stream()` | Consume body → `BodyStream` |

`BodyStream` implements `futures_core::Stream<Item = Result<Bytes, OxiHttpError>>`.

### `RedirectPolicy` (`redirect` module)

| Variant / item | Description |
|----------------|-------------|
| `None` | Never follow; return the redirect response |
| `Limited(usize)` | Follow up to `n` (the `Default` is `Limited(10)`) |
| `All` | Follow all redirects |
| `max_redirects()` / `is_none()` | Inspection |

Free functions: `is_redirect_status(StatusCode)`, `redirect_method(StatusCode, &Method)` (301–303 demote POST→GET), `should_preserve_body(StatusCode)` (307/308 only).

### `RetryPolicy` (`retry` module)

Public fields: `max_retries`, `backoff_base`, `backoff_max`, `retryable_status_codes`, `retry_on_connection_error`, `retry_on_timeout`. `Default` retries 3 times.

| Method | Description |
|--------|-------------|
| `new(max_retries)` | Defaults: 100 ms base, 30 s cap, retry 429/500/502/503/504 |
| `with_backoff_base` / `with_backoff_max` | Tune backoff |
| `with_retryable_status_codes(set)` / `add_retryable_status(code)` | Customise retryable statuses |
| `with_retry_on_connection_error(bool)` / `with_retry_on_timeout(bool)` | Toggle error retries |
| `should_retry_status(u16)` | Predicate |
| `backoff_delay(attempt)` | Exponential delay (capped) |

### `Http2Settings` (`client_builder` module)

A `Default` struct of optional HTTP/2 knobs: `initial_stream_window_size`, `initial_connection_window_size`, `adaptive_window`, `keep_alive_interval`, `keep_alive_timeout`, `max_frame_size`, `max_concurrent_reset_streams`, `max_send_buf_size`.

### Middleware (`middleware` module)

| Item | Description |
|------|-------------|
| `ClientMiddleware` (trait) | `before_request(&RequestContext)` + `after_response(&ResponseContext)` observers |
| `RequestContext<'a>` | `method`, `uri`, `headers` of the outgoing request |
| `ResponseContext` | Final `status` + `elapsed` duration |
| `LoggingMiddleware::new(name)` | Logs each request/response to stderr |
| `TimingMiddleware::new(Fn(Duration))` | Invokes a callback with elapsed time |

### Custom DNS (`resolver` module)

| Item | Description |
|------|-------------|
| `DnsResolver` (trait) | `resolve(&str) -> Future<Vec<SocketAddr>>` |
| `GaiDnsResolver` | Default resolver via `tokio::net::lookup_host` |
| `BoxResolver`, `BoxResolverAddrs` | Public glue used by the resolver `Client` aliases |

### Proxies (`proxy` module)

| Item | Description |
|------|-------------|
| `ProxyKind` | `HttpConnect(Uri)` or `Socks5(Uri)` *(feature `socks`)* |
| `ProxyConnector` | `Service<Uri>` tunnelling via HTTP CONNECT (+ optional `Proxy-Authorization`) |
| `Socks5Connector` *(feature `socks`)* | `Service<Uri>` SOCKS5 (RFC 1928/1929), forwards DNS names to the proxy |

### TLS types *(feature `tls`)*

| Item | Module | Description |
|------|--------|-------------|
| `OxiHttpsConnector<H>` | `connector` | Upgrades to TLS for `https://`, passes through `http://` |
| `MaybeHttpsStream<T>` | `connector` | Plain-or-TLS stream (`Http` / `Https`) |
| `RequestTlsConfig` | `request_config` | Per-request TLS trust override (`trusted_cert_ders`, `accept_invalid_certs`); `new`, `with_trusted_cert`, `with_accept_invalid_certs` |

### HTTP/3 *(feature `h3`, `h3` module)*

A thin wrapper over `oxiquic-h3`.

| Item | Description |
|------|-------------|
| `H3ConnectionBuilder::new(server_name)` | Builder; `with_tls_config(rustls::ClientConfig)` then `connect(SocketAddr)` |
| `H3Connection` | `get` / `post` / `head` / `put` / `delete` / `request(H3Request, Option<Bytes>)` / `close()` |
| `H3Request`, `H3Response` | Re-exported from `oxiquic-h3` |

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `tls` | off | HTTPS via `oxitls` + `rustls`/`tokio-rustls` (Pure Rust); enables `HttpsClient`, `OxiHttpsConnector`, `RequestTlsConfig` |
| `decompression` | off | Transparent gzip/deflate decompression via `oxiarc-deflate` |
| `socks` | off | SOCKS5 proxy support (`Socks5Connector`, `ProxyKind::Socks5`) |
| `h3` | off | HTTP/3 client via `oxiquic-h3` (pulls in `rustls`) |

## Error type

All fallible APIs return `oxihttp_core::OxiHttpError`. See the [`oxihttp-core` README](https://crates.io/crates/oxihttp-core) for the full variant table; the client most commonly produces `Hyper`, `Io`, `Timeout`, `Redirect`, `Tls`, `Dns`, `ConnectionPool`, `Body`, `Json`, `InvalidHeader`, `InvalidUri`, and (HTTP/3) `H3`.

## Related crates

- [`oxihttp`](https://crates.io/crates/oxihttp) — the unified facade re-exporting this client.
- [`oxihttp-core`](https://crates.io/crates/oxihttp-core) — shared types (`OxiHttpError`, `Body`, cookies, multipart, forms).
- [`oxihttp-server`](https://crates.io/crates/oxihttp-server) — the matching HTTP server.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)
