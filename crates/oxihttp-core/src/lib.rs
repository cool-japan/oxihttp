//! OxiHTTP Core - foundational types for the OxiHTTP Pure-Rust HTTP stack.
//!
//! This crate provides the shared types used by `oxihttp-client`, `oxihttp-server`,
//! and the `oxihttp` facade crate.
//!
//! # Re-exports
//!
//! The `http` crate types are re-exported for convenience.

#![forbid(unsafe_code)]

pub mod body;
pub mod content_type;
pub mod cookie;
pub mod error;
pub mod form;
pub mod header_ext;
pub mod header_types;
pub mod multipart;
pub mod request_builder;
pub mod response_ext;
pub mod uri_ext;
pub mod version;

// Re-export the http crate types
pub use http::{
    HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Uri, Version,
};

// Re-export bytes for downstream convenience
pub use bytes::{Bytes, BytesMut};

// Re-export our types at the crate root
pub use body::{Body, PinnedBody};
pub use content_type::ContentType;
pub use cookie::{Cookie, CookieJar, SameSite};
pub use error::OxiHttpError;
pub use form::FormBody;
pub use header_ext::HeaderMapExt;
pub use header_types::{
    Authorization, CacheControl, ContentLength, ETag, Header, Host, Location, Referer,
};
pub use multipart::{MultipartBuilder, Part as MultipartPart, StreamingMultipart};
pub use request_builder::RequestBuilder as CoreRequestBuilder;
pub use response_ext::ResponseExt;
pub use uri_ext::UriExt;
pub use version::HttpVersion;

/// Type alias for `Result<T, OxiHttpError>`.
pub type Result<T> = std::result::Result<T, OxiHttpError>;

/// Type alias for an HTTP request carrying an [`Body`].
pub type OxiRequest<B = body::Body> = http::Request<B>;

/// Type alias for an HTTP response carrying an [`Body`].
pub type OxiResponse<B = body::Body> = http::Response<B>;
