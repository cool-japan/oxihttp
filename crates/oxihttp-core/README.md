# oxihttp-core — Foundational HTTP types for the OxiHTTP stack

[![Crates.io](https://img.shields.io/crates/v/oxihttp-core.svg)](https://crates.io/crates/oxihttp-core)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxihttp-core` defines the shared types used across the OxiHTTP Pure-Rust HTTP stack: the `oxihttp` facade, the `oxihttp-client` client, and the `oxihttp-server` server. It carries no transport logic of its own — it provides the body abstraction, error type, typed-header machinery, cookie jar, multipart and form builders, content-type negotiation, and a set of convenience extension traits over the `http` crate.

The crate re-exports the core `http` crate types (`Request`, `Response`, `HeaderMap`, `Method`, `StatusCode`, `Uri`, `Version`, …) and `bytes` (`Bytes`, `BytesMut`) so downstream crates depend on a single coherent surface. It is `#![forbid(unsafe_code)]` and Pure Rust; the only optional dependency is `oxitls-core`, enabled by the `tls` feature solely to provide a `From<oxitls_core::TlsError>` conversion into `OxiHttpError`.

## Installation

```toml
[dependencies]
oxihttp-core = "0.1.0"

# With the TLS error-conversion glue:
oxihttp-core = { version = "0.1.0", features = ["tls"] }
```

## Quick Start

Build a request with the execution-free builder, then consume a response body
via the extension trait:

```rust
use oxihttp_core::{CoreRequestBuilder, Method};

let req = CoreRequestBuilder::post("http://example.com/api")?
    .header("x-api-key", "secret")?
    .json(&serde_json::json!({ "hello": "world" }))?
    .build()?;

assert_eq!(req.method(), &Method::POST);
# Ok::<(), oxihttp_core::OxiHttpError>(())
```

```rust,ignore
use oxihttp_core::ResponseExt;

// `response` is any `http::Response<B>` where `B: http_body::Body`
let text: String = response.body_text().await?;
let value: MyStruct = response.body_json().await?;
```

## API Overview

### Re-exports

| Item | Source | Notes |
|------|--------|-------|
| `Request`, `Response`, `HeaderMap`, `HeaderName`, `HeaderValue`, `Method`, `StatusCode`, `Uri`, `Version` | `http` | Core HTTP crate types |
| `Bytes`, `BytesMut` | `bytes` | Byte buffer types |

### Type aliases

| Alias | Definition |
|-------|------------|
| `Result<T>` | `std::result::Result<T, OxiHttpError>` |
| `OxiRequest<B = Body>` | `http::Request<B>` |
| `OxiResponse<B = Body>` | `http::Response<B>` |

### `Body` — request/response body (`body` module)

A three-mode body that implements `http_body::Body` (via `PinnedBody`) for hyper integration.

| Variant | Meaning |
|---------|---------|
| `Body::Empty` | No content (the `Default`) |
| `Body::Full(FullBody)` | Fully buffered in memory |
| `Body::Stream(StreamBody)` | Backed by an async `Stream<Item = Result<Bytes, OxiHttpError>>` |

| Method / impl | Description |
|---------------|-------------|
| `Body::empty()` | Construct an empty body |
| `Body::full(impl Into<Bytes>)` | Construct an in-memory body |
| `Body::stream(Pin<Box<dyn Stream<…> + Send>>)` | Construct a streaming body |
| `Body::content_length()` | `Some(n)` for empty/full, `None` for streams |
| `Body::into_pinned()` | Convert to `PinnedBody` |
| `From<() / Bytes / Vec<u8> / String / &'static str / &'static [u8]>` | Ergonomic constructors |

`PinnedBody` is a pinnable enum implementing `http_body::Body` (`Data = Bytes`, `Error = OxiHttpError`) with `poll_frame`, `is_end_stream`, and `size_hint`. `FullBody` and `StreamBody` are the wrapped inner body kinds.

### `OxiHttpError` — error type (`error` module)

`#[derive(Debug, Clone, Error)]`. Cheap to clone (`Arc`-wraps non-`Clone` sources). See the [Error variants](#error-variants) table below. Helper methods:

| Method | Description |
|--------|-------------|
| `status_code()` | `Option<http::StatusCode>` for `RouteNotFound`/`MethodNotAllowed`/`Timeout` |
| `is_timeout()` | `true` for `Timeout` |
| `is_connect()` | `true` for `Dns`/`ConnectionPool`/`Tls` |
| `is_body()` | `true` for `Body` |
| `is_redirect()` | `true` for `Redirect` |

`From` conversions: `http::uri::InvalidUri`, `std::io::Error`, `http::Error`, and (feature `tls`) `oxitls_core::TlsError`.

### `ContentType` — MIME types & negotiation (`content_type` module)

| Variant | MIME |
|---------|------|
| `Json` | `application/json` |
| `Form` | `application/x-www-form-urlencoded` |
| `Multipart(Option<String>)` | `multipart/form-data` (+ boundary) |
| `OctetStream` | `application/octet-stream` |
| `Text(Option<String>)` | `text/plain` (+ charset) |
| `Html(Option<String>)` | `text/html` (+ charset) |
| `Xml` | `application/xml` / `text/xml` |
| `Other(String)` | Any other raw MIME string |

| Item | Description |
|------|-------------|
| `mime_type()` | MIME string without parameters |
| `charset()` | Charset parameter if present |
| `is_text()` | `true` for textual types (Text/Html/Json/Xml) |
| `from_extension(ext)` | Detect from a file extension |
| `impl Display`, `impl FromStr` | Format / parse `type/subtype; params` |
| `AcceptEntry { content_type, quality }` | One parsed `Accept` entry |
| `parse_accept(header) -> Vec<AcceptEntry>` | Parse an `Accept` header, sorted by quality |
| `negotiate_content_type(accept, supported) -> Option<ContentType>` | Pick the best supported type (handles `*/*` and `type/*`) |

### `Cookie` & `CookieJar` — RFC 6265 (`cookie` module)

`Cookie` is a public-field struct (`name`, `value`, `domain`, `path`, `max_age`, `secure`, `http_only`, `same_site`, `expires_at`).

| `Cookie` item | Description |
|---------------|-------------|
| `new(name, value)` | Construct a bare cookie |
| `name()` / `value()` / `domain()` / `path()` / `max_age()` | Accessors |
| `is_secure()` / `is_http_only()` / `same_site()` | Flag accessors |
| `set_domain` / `set_path` / `set_max_age` / `set_secure` / `set_http_only` / `set_same_site` | Builder-style setters |
| `parse_set_cookie(header) -> Option<Cookie>` | Parse a `Set-Cookie` value |
| `to_cookie_header()` | Serialise as `name=value` for a `Cookie` header |
| `impl Display` | Full `Set-Cookie`-style rendering |

`SameSite` — `Strict`, `Lax`, `None`.

| `CookieJar` item | Description |
|------------------|-------------|
| `new()` | Empty jar (also `Default`) |
| `insert` / `get` / `remove` | Manage by name |
| `iter()` / `len()` / `is_empty()` | Inspection |
| `to_cookie_header()` | Build a `Cookie` header from all cookies |
| `add_from_set_cookie_headers(iter)` | Bulk-parse `Set-Cookie` values |
| `insert_for_url(cookie, &Uri)` | Insert with default domain/path + expiry, dedup by (name, domain, path) |
| `cookies_for_url(&Uri) -> Vec<&Cookie>` | RFC 6265 §5.4 match (domain/path/secure/expiry), sorted by path length |
| `to_cookie_header_for_url(&Uri) -> Option<String>` | Build a `Cookie` header scoped to a URL |
| `add_from_response_headers(&HeaderMap, &Uri)` | Ingest all `Set-Cookie` headers from a response |

### `FormBody` — URL-encoded forms (`form` module)

| Method | Description |
|--------|-------------|
| `new()` | Empty form (also `Default`) |
| `field(name, value)` | Append a field (builder-style) |
| `build() -> Bytes` | Percent-encode to `application/x-www-form-urlencoded` |
| `len()` / `is_empty()` | Field count |

### Multipart — RFC 7578 (`multipart` module)

`MultipartBuilder` (re-exported at the root) and `Part` (re-exported as `MultipartPart`).

| `MultipartBuilder` item | Description |
|-------------------------|-------------|
| `new()` | Builder with an auto-generated boundary (also `Default`) |
| `boundary()` | The boundary string (no leading `--`) |
| `content_type()` | Full `multipart/form-data; boundary=…` header value |
| `add_text(name, value)` | Add a text field |
| `add_file(name, filename, content_type, body)` | Add a file/binary part |
| `add_part(Part)` | Add a pre-built part |
| `build() -> Bytes` | Serialise the wire format (auto-resolves boundary collisions) |

| `Part` (alias `MultipartPart`) item | Description |
|-------------------------------------|-------------|
| `text(name, value)` | Text part with `Content-Disposition` |
| `file(name, filename, content_type, body)` | File part with `Content-Disposition` + `Content-Type` |
| `custom(headers, body)` | Fully custom headers and body |

### `CoreRequestBuilder` — execution-free request builder (`request_builder` module)

Exported as `CoreRequestBuilder` (distinct from the client's network-bound `RequestBuilder`). Terminal `build()` returns `OxiRequest<Body>` with no I/O.

| Method | Description |
|--------|-------------|
| `new(method, uri)` | Construct directly |
| `get` / `post` / `put` / `delete` / `patch` / `head` (uri) | Method constructors (parse the URI) |
| `header(name, value)` | Add/replace one header |
| `headers(HeaderMap)` | Merge a header map |
| `body(impl Into<Body>)` | Set a raw body |
| `json(&T)` | Serialise JSON + set `Content-Type` |
| `form(FormBody)` | URL-encoded body + set `Content-Type` |
| `build() -> Result<OxiRequest<Body>>` | Build the final request |

### Extension traits

#### `HeaderMapExt` (`header_ext` module)

Typed getters/setters for `HeaderMap`: `content_type()`, `content_length()`, `authorization()`, `accept()`, `host()`, `user_agent()`, `cache_control()`, `etag()`, `if_none_match()`, `if_modified_since()`, `cookie_header()`, `location()`, `referer()`; and `set_content_type`, `set_content_length`, `set_bearer_auth`, `set_basic_auth`, `set_cache_control`, `set_etag`, `set_location`, `set_cookie_header` (appends per RFC 6265).

#### `ResponseExt` (`response_ext` module)

Async body consumers for any `http::Response<B>` where `B: http_body::Body`: `body_bytes()`, `body_text()`, `body_json::<T>()`.

#### `UriExt` (`uri_ext` module)

Convenience over `http::Uri`: `host_str()`, `port_or_default()` (80/443 defaults), `is_https()`, `is_http()`, `origin()`.

### Typed headers (`header_types` module)

The `Header` trait — `header_name()` + `decode(&HeaderMap) -> Result<Self, OxiHttpError>` — with concrete impls:

| Type | Header | Shape |
|------|--------|-------|
| `ContentType` | `Content-Type` | (enum, see above) |
| `ContentLength` | `Content-Length` | `ContentLength(pub u64)` |
| `Host` | `Host` | `Host(pub String)` |
| `ETag` | `ETag` | `ETag(pub String)` |
| `Authorization` | `Authorization` | `Authorization(pub String)` |
| `CacheControl` | `Cache-Control` | `CacheControl(pub String)` |
| `Referer` | `Referer` | `Referer(pub String)` |
| `Location` | `Location` | `Location(pub String)` |

All string-typed headers implement `Display`.

### `HttpVersion` (`version` module)

| Variant | String |
|---------|--------|
| `Http10` | `HTTP/1.0` |
| `Http11` | `HTTP/1.1` |
| `H2` | `HTTP/2` |
| `H3` | `HTTP/3` |

Methods: `as_str()`, `is_http1()`, `is_http2()`, `is_http3()`; `impl Display`; `impl FromStr` (case-insensitive, accepts `h2`/`h3`); bidirectional `From`/`Into` with `http::Version`.

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `tls` | off | Pulls in `oxitls-core` to add `From<oxitls_core::TlsError> for OxiHttpError` |

## Error variants

| Variant | Display | Notes |
|---------|---------|-------|
| `InvalidUri(Arc<http::uri::InvalidUri>)` | `invalid URI: …` | URI parse failure |
| `Http(Arc<http::Error>)` | `HTTP error: …` | `http` crate error |
| `Hyper(String)` | `hyper error: …` | Transport error (stringified to hide hyper types) |
| `Io(Arc<std::io::Error>)` | `I/O error: …` | I/O failure |
| `Body(String)` | `body error: …` | Body read/processing failure |
| `Timeout(String)` | `timeout: …` | Connect/request timeout → 408 |
| `Redirect(String)` | `redirect error: …` | Redirect loop/limit |
| `Tls(String)` | `TLS error: …` | TLS failure (from oxitls) |
| `Dns(String)` | `DNS error: …` | Resolution failure |
| `ConnectionPool(String)` | `connection pool error: …` | Pool exhaustion / proxy failure |
| `Json(String)` | `JSON error: …` | (De)serialisation failure |
| `FormEncoding(String)` | `form encoding error: …` | URL-encoded form failure |
| `InvalidHeader(String)` | `invalid header: …` | Invalid header name/value |
| `Server(String)` | `server error: …` | Server-side failure |
| `RouteNotFound { method, path }` | `route not found: …` | → 404 |
| `MethodNotAllowed { method, path }` | `method not allowed: …` | → 405 |
| `H3(String)` | `HTTP/3 error: …` | QUIC / HTTP/3 transport error |

## Related crates

`oxihttp-core` is the bottom layer of the OxiHTTP stack:

- [`oxihttp`](https://crates.io/crates/oxihttp) — the unified facade re-exporting client and server.
- [`oxihttp-client`](https://crates.io/crates/oxihttp-client) — the HTTP client built on these types.
- [`oxihttp-server`](https://crates.io/crates/oxihttp-server) — the HTTP server built on these types.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)
