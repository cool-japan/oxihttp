# oxihttp-core TODO

## Status
Fully-implemented foundation crate (~1200 SLOC) providing Body type, HeaderMapExt,
Cookie/CookieJar, ContentType, Accept negotiation, UriExt, multipart/form builders,
extended OxiHttpError, and re-exports of `http` and `bytes` crate types.
All M0+M1 core items complete.

## Core Implementation
- [x] Add `Body` enum: `Empty`, `Full(Bytes)`, `Stream(Pin<Box<dyn AsyncRead>>)`, `Multipart(MultipartBody)` with `content_length()` and `into_bytes()` (~200 SLOC)
- [x] Add `RequestBuilder` for ergonomic request construction: method, URI, headers, body, timeout (~250 SLOC)
- [x] Add `ResponseExt` trait: `body_bytes()`, `body_text()`, `body_json<T: DeserializeOwned>()` extension methods (~120 SLOC)
- [x] Add `HeaderMapExt` trait: typed header accessors for Content-Type, Content-Length, Authorization, Accept, Cookie, Set-Cookie (~150 SLOC)
- [x] Add `Cookie` struct: name, value, domain, path, max-age, secure, http-only, same-site parsing and serialization (~200 SLOC)
- [x] Add `CookieJar` for managing request/response cookies across redirects (~150 SLOC)
- [x] Add `ContentType` enum: `Json`, `Form`, `Multipart`, `OctetStream`, `Text(charset)`, `Xml`, `Html` with parsing from Content-Type header (~100 SLOC)
- [x] Add `Accept` header negotiation: `negotiate_content_type(accept_header, supported_types) -> ContentType` (~80 SLOC)
- [x] Add `UriExt` trait: `host()`, `port_or_default()`, `is_https()`, `origin()` convenience methods (~60 SLOC)
- [x] Add `OxiHttpError::Timeout` variant for request/connect timeout errors (~10 SLOC)
- [x] Add `OxiHttpError::Redirect(String)` variant for redirect loop/limit errors (~10 SLOC)
- [x] Add `OxiHttpError::Tls(String)` variant for TLS-specific errors from oxitls (~10 SLOC)
- [x] Add `OxiHttpError::Dns(String)` variant for DNS resolution failures (~10 SLOC)
- [x] Add `OxiHttpError::ConnectionPool(String)` variant for pool exhaustion (~10 SLOC)
- [x] Add `From<oxitls_core::TlsError> for OxiHttpError` conversion (~10 SLOC)
- [x] Add multipart body builder: `MultipartBody::new().field(name, value).file(name, path, mime) -> MultipartBody` (~200 SLOC)
- [x] Add URL-encoded form body: `FormBody::new().field(name, value).build() -> Bytes` (~60 SLOC)
- [x] Add `HttpVersion` enum wrapping `http::Version` with Display and FromStr (~30 SLOC)

## API Improvements
- [x] Add `OxiHttpError::status_code() -> Option<StatusCode>` for extracting HTTP status from error context
- [x] Add `OxiHttpError::is_timeout()`, `is_connect()`, `is_body()` predicates
- [x] Implement `Clone` on `OxiHttpError` where possible (Io variant wraps ErrorKind)
- [x] Add `From<hyper::Error> for OxiHttpError` with proper error chain preservation
- [x] Add `From<hyper_util::client::legacy::Error> for OxiHttpError`
- [x] Re-export `bytes::Bytes` and `bytes::BytesMut` for downstream convenience
- [x] Add `Request<Body>` and `Response<Body>` type aliases using the new `Body` type

## Testing
- [x] Unit test: all `OxiHttpError` Display formatting variants
- [x] Unit test: `From<http::uri::InvalidUri>` conversion
- [x] Unit test: `From<http::Error>` conversion
- [x] Unit test: `From<io::Error>` conversion
- [x] Unit test: `Cookie` parsing from Set-Cookie header (RFC 6265 compliance)
- [x] Unit test: `CookieJar` accumulates cookies across multiple responses
- [x] Unit test: `ContentType` parsing from Content-Type header with charset
- [x] Unit test: `Accept` negotiation selects highest-quality match
- [x] Unit test: `MultipartBody` serialization produces valid multipart/form-data
- [x] Unit test: `FormBody` serialization produces valid application/x-www-form-urlencoded
- [x] Unit test: `RequestBuilder` chain produces correct `Request<Body>`
- [x] Unit test: `ResponseExt::body_json()` deserializes valid JSON
- [x] Unit test: `UriExt::port_or_default()` returns 443 for https, 80 for http
- [x] Property test: `Cookie` round-trip (serialize -> parse -> serialize)

## Performance
- [x] Benchmark `HeaderMapExt` typed accessor vs raw `headers().get()` lookup
  - **Files:** crates/oxihttp-core/benches/core_bench.rs (new)
- [x] Benchmark `Cookie` parsing speed (1000 cookies)
  - **Files:** crates/oxihttp-core/benches/core_bench.rs (new)
- [x] Benchmark `MultipartBody` serialization for large files (1MB, 10MB)
  - **Files:** crates/oxihttp-core/benches/core_bench.rs (new)
- [x] Benchmark `RequestBuilder` construction overhead vs raw `http::Request::builder()`
  - **Files:** crates/oxihttp-core/benches/core_bench.rs (new)
- [x] Profile `Body::into_bytes()` for streaming bodies (memory allocation pattern)
  - **Files:** crates/oxihttp-core/benches/core_bench.rs (new, async criterion bench)

## Integration
- [x] Coordinate `Body` type with `oxihttp-client` request sending
- [x] Coordinate `Body` type with `oxihttp-server` request receiving
- [x] Coordinate `OxiHttpError` variants with `oxitls_core::TlsError` mapping
- [x] Wire `Cookie`/`CookieJar` into `oxihttp-client` for automatic cookie management (RFC 6265 domain_match §5.1.3, path_match §5.1.4, expiry via max_age, secure-flag enforcement)
- [x] Wire `ContentType` negotiation into `oxihttp-server` request routing
  - **Goal:** Add Request::negotiate(supported: &[ContentType]) -> Option<ContentType> to server router.rs; delegates to core's negotiate_content_type()
  - **Files:** crates/oxihttp-server/src/router.rs, crates/oxihttp-server/src/lib.rs
- [x] Ensure `Body` stream variant works with hyper's body trait requirements
