//! # reqwest → OxiHTTP Migration Guide
//!
//! This module documents how to migrate from [`reqwest`](https://crates.io/crates/reqwest)
//! to OxiHTTP. OxiHTTP is a pure-Rust HTTP stack (no C/C++ dependencies) designed as a
//! drop-in replacement for most common reqwest usage patterns.
//!
//! ## Basic client setup
//!
//! **reqwest:**
//! ```rust,ignore
//! let client = reqwest::Client::new();
//! ```
//!
//! **OxiHTTP:**
//! ```rust,no_run
//! use oxihttp::Client;
//! let client = Client::builder().build().expect("client build");
//! ```
//!
//! ## GET request
//!
//! **reqwest:**
//! ```rust,ignore
//! let resp = client.get("https://httpbin.org/get").send().await?;
//! let body = resp.text().await?;
//! ```
//!
//! **OxiHTTP:**
//! ```rust,no_run
//! # async fn run() -> oxihttp::Result<()> {
//! # let client = oxihttp::Client::builder().build()?;
//! let resp = client.get("https://httpbin.org/get")?.send().await?;
//! let body = resp.body_text().await?;
//! # Ok(()) }
//! ```
//!
//! ## POST with JSON
//!
//! **reqwest:**
//! ```rust,ignore
//! #[derive(serde::Serialize)] struct Payload { name: String }
//! let resp = client.post(url).json(&Payload { name: "Alice".into() }).send().await?;
//! ```
//!
//! **OxiHTTP:**
//! ```rust,ignore
//! # use serde::Serialize;
//! # #[derive(Serialize)] struct Payload { name: String }
//! # async fn run() -> oxihttp::Result<()> {
//! # let client = oxihttp::Client::builder().build()?;
//! let resp = client.post("https://example.com/api")?
//!     .json(&Payload { name: "Alice".into() })?
//!     .send().await?;
//! # Ok(()) }
//! ```
//!
//! Note: in OxiHTTP, `json()` returns `Result<RequestBuilder, Error>` because it
//! serializes immediately. Use `?` to propagate errors.
//!
//! ## Custom headers
//!
//! **reqwest:**
//! ```rust,ignore
//! let resp = client.get(url).header("X-API-Key", "secret").send().await?;
//! ```
//!
//! **OxiHTTP:**
//! ```rust,no_run
//! # async fn run() -> oxihttp::Result<()> {
//! # let client = oxihttp::Client::builder().build()?;
//! let resp = client.get("https://api.example.com/data")?
//!     .header("X-API-Key", "secret")?
//!     .send().await?;
//! # Ok(()) }
//! ```
//!
//! Note: `header()` also returns `Result<RequestBuilder, Error>` because it validates
//! the header name/value immediately. Use `?` to propagate errors.
//!
//! ## Timeouts
//!
//! **reqwest:**
//! ```rust,ignore
//! use std::time::Duration;
//! let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
//! ```
//!
//! **OxiHTTP:**
//! ```rust,no_run
//! # use oxihttp::Result;
//! # fn example() -> Result<()> {
//! use std::time::Duration;
//! use oxihttp::Client;
//! let client = Client::builder()
//!     .connect_timeout(Duration::from_secs(5))
//!     .read_timeout(Duration::from_secs(10))
//!     .build()?;
//! # Ok(()) }
//! ```
//!
//! OxiHTTP separates connect timeout from read timeout for finer control.
//! Per-request timeout is set via `RequestBuilder::timeout()`.
//!
//! ## Per-request timeout
//!
//! **reqwest:**
//! ```rust,ignore
//! let resp = client.get(url).timeout(Duration::from_secs(5)).send().await?;
//! ```
//!
//! **OxiHTTP:**
//! ```rust,no_run
//! # use std::time::Duration;
//! # async fn run() -> oxihttp::Result<()> {
//! # let client = oxihttp::Client::builder().build()?;
//! let resp = client.get("https://example.com")?
//!     .timeout(Duration::from_secs(5))
//!     .send().await?;
//! # Ok(()) }
//! ```
//!
//! ## TLS / HTTPS
//!
//! **reqwest:**
//! ```rust,ignore
//! let client = reqwest::Client::builder()
//!     .danger_accept_invalid_certs(true)  // for testing
//!     .build()?;
//! ```
//!
//! **OxiHTTP (tls feature):**
//! ```rust,ignore
//! # #[cfg(feature = "tls")]
//! # fn example() -> oxihttp::Result<()> {
//! use oxihttp::Client;
//! let client = Client::builder()
//!     .with_tls()                              // use webpki-roots CA bundle
//!     .with_danger_accept_invalid_certs()      // for testing only
//!     .build_https()?;
//! # Ok(()) }
//! ```
//!
//! Unlike reqwest, OxiHTTP requires you to opt-in to a CA bundle explicitly
//! via `with_tls()` (webpki-roots) or `with_trusted_cert_der()`.
//!
//! ## Redirects
//!
//! **reqwest:**
//! ```rust,ignore
//! let client = reqwest::Client::builder()
//!     .redirect(reqwest::redirect::Policy::none())
//!     .build()?;
//! ```
//!
//! **OxiHTTP:**
//! ```rust,no_run
//! # fn example() -> oxihttp::Result<()> {
//! use oxihttp::{Client, RedirectPolicy};
//! let client = Client::builder()
//!     .redirect_policy(RedirectPolicy::None)
//!     .build()?;
//! # Ok(()) }
//! ```
//!
//! OxiHTTP's `RedirectPolicy` variants:
//! - `RedirectPolicy::None` — never follow redirects
//! - `RedirectPolicy::Limited(n)` — follow at most `n` redirects (default: 10)
//! - `RedirectPolicy::All` — follow all redirects
//!
//! ## Retry policy
//!
//! reqwest does not include built-in retry. OxiHTTP provides `RetryPolicy`:
//!
//! ```rust,no_run
//! # fn example() -> oxihttp::Result<()> {
//! use oxihttp::{Client, RetryPolicy};
//! let client = Client::builder()
//!     .retry_policy(RetryPolicy::default())
//!     .build()?;
//! # Ok(()) }
//! ```
//!
//! ## Streaming response body
//!
//! **reqwest:**
//! ```rust,ignore
//! use futures_util::StreamExt;
//! let mut stream = resp.bytes_stream();
//! while let Some(chunk) = stream.next().await { let _data = chunk?; }
//! ```
//!
//! **OxiHTTP:**
//! ```rust,no_run
//! # use futures_util::StreamExt;
//! # async fn run() -> oxihttp::Result<()> {
//! # let client = oxihttp::Client::builder().build()?;
//! # let resp = client.get("http://example.com/large")?.send().await?;
//! let mut stream = resp.body_stream();
//! while let Some(chunk) = stream.next().await { let _data = chunk?; }
//! # Ok(()) }
//! ```
//!
//! ## Error handling
//!
//! **reqwest:**
//! ```rust,ignore
//! let resp = client.get(url).send().await?;
//! resp.error_for_status()?;   // 4xx/5xx → Err
//! ```
//!
//! **OxiHTTP:**
//! ```rust,no_run
//! # async fn run() -> oxihttp::Result<()> {
//! # let client = oxihttp::Client::builder().build()?;
//! let resp = client.get("http://example.com")?.send().await?;
//! resp.error_for_status()?;   // same API
//! # Ok(()) }
//! ```
//!
//! ## JSON response deserialization
//!
//! **reqwest:**
//! ```rust,ignore
//! #[derive(serde::Deserialize)] struct ApiResult { id: u64 }
//! let result: ApiResult = resp.json().await?;
//! ```
//!
//! **OxiHTTP:**
//! ```rust,ignore
//! # use serde::Deserialize;
//! # #[derive(Deserialize)] struct ApiResult { id: u64 }
//! # async fn run() -> oxihttp::Result<()> {
//! # let client = oxihttp::Client::builder().build()?;
//! # let resp = client.get("http://example.com/api")?.send().await?;
//! let result: ApiResult = resp.body_json().await?;
//! # Ok(()) }
//! ```
//!
//! ## Authentication helpers
//!
//! **reqwest:**
//! ```rust,ignore
//! let resp = client.get(url).bearer_auth("my-token").send().await?;
//! let resp = client.get(url).basic_auth("user", Some("pass")).send().await?;
//! ```
//!
//! **OxiHTTP:**
//! ```rust,no_run
//! # async fn run() -> oxihttp::Result<()> {
//! # let client = oxihttp::Client::builder().build()?;
//! let resp = client.get("https://api.example.com/data")?
//!     .bearer_token("my-token")?
//!     .send().await?;
//!
//! let resp2 = client.get("https://api.example.com/data")?
//!     .basic_auth("user", Some("pass"))?
//!     .send().await?;
//! # Ok(()) }
//! ```
//!
//! ## HTTP server
//!
//! reqwest does not provide server functionality. OxiHTTP includes a full HTTP server:
//!
//! ```rust,no_run
//! # #[cfg(feature = "server")]
//! # async fn run() -> oxihttp::Result<()> {
//! use oxihttp::{Router, Server};
//!
//! let router = Router::new()
//!     .get("/hello", |_req| async {
//!         oxihttp_server::response::text_response("Hello!")
//!     })
//!     .health("/health");
//!
//! Server::bind("127.0.0.1:3000")
//!     .serve(router)
//!     .await?;
//! # Ok(()) }
//! ```
//!
//! ## Middleware (client-side)
//!
//! reqwest does not have built-in middleware. OxiHTTP provides a `ClientMiddleware` trait:
//!
//! ```rust,no_run
//! # fn example() -> oxihttp::Result<()> {
//! use oxihttp_client::middleware::LoggingMiddleware;
//! use oxihttp::Client;
//!
//! let client = Client::builder()
//!     .with_middleware(LoggingMiddleware::new("my-client"))
//!     .build()?;
//! # Ok(()) }
//! ```
//!
//! ## Feature flags comparison
//!
//! | reqwest feature | OxiHTTP feature | Notes |
//! |---|---|---|
//! | `default-tls` | `tls` | Pure Rust, uses rustls + oxitls |
//! | `rustls-tls` | `tls` | Same, always rustls |
//! | `gzip`, `deflate` | `decompression` | Uses oxiarc-deflate |
//! | `json` | always included | serde_json always available |
//! | *(no server)* | `server` | HTTP/1.1 + HTTP/2 server |
//! | *(no ws)* | `websocket` | RFC 6455 WebSocket |
//! | *(no socks)* | `socks` | SOCKS5 proxy (feature-gated) |
//! | `stream` | always included | `BodyStream` is always available |
//! | `multipart` | always included | `MultipartBuilder` in `oxihttp-core` |
//! | `cookies` | always included | `CookieJar` in `oxihttp-core` |
//!
//! ## Crate organisation
//!
//! OxiHTTP is split into multiple crates under the `oxihttp` workspace:
//!
//! - `oxihttp` — this facade crate; the only one most users need
//! - `oxihttp-core` — shared types (`OxiHttpError`, `Body`, `Cookie`, etc.)
//! - `oxihttp-client` — HTTP client implementation
//! - `oxihttp-server` — HTTP server, router, and middleware
//!
//! If you need low-level access, you can depend on the sub-crates directly.
//! For most migrations from reqwest, depending only on `oxihttp` is sufficient.
