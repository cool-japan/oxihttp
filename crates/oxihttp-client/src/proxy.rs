//! HTTP CONNECT and SOCKS5 proxy connectors.
//!
//! `ProxyConnector` tunnels TCP connections through an HTTP CONNECT proxy.
//! `Socks5Connector` tunnels TCP connections through a SOCKS5 proxy (RFC 1928/1929).
//! Both implement `tower_service::Service<http::Uri>` and return a
//! `hyper_util::rt::TokioIo<tokio::net::TcpStream>` suitable for use as an
//! inner connector inside `OxiHttpsConnector` or directly with a hyper legacy
//! `Client`.

use std::future::Future;
#[cfg(feature = "socks")]
use std::net::IpAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use http::Uri;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tower_service::Service;

use oxihttp_core::OxiHttpError;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract `(host, port)` authority from an `http::Uri`.
///
/// Returns `Err` when host or port are missing/invalid.
fn uri_host_port(uri: &Uri) -> Result<(String, u16), OxiHttpError> {
    let host = uri
        .host()
        .ok_or_else(|| OxiHttpError::ConnectionPool(format!("URI has no host: {uri}")))?
        .to_owned();
    let port = match uri.port_u16() {
        Some(p) => p,
        None => match uri.scheme_str() {
            Some("https") => 443u16,
            Some("http") => 80u16,
            _ => {
                return Err(OxiHttpError::ConnectionPool(format!(
                    "URI has no port and unknown scheme: {uri}"
                )))
            }
        },
    };
    Ok((host, port))
}

/// Extract `(user, pass)` from URI userinfo, if present.
fn extract_auth(uri: &Uri) -> Option<(String, String)> {
    let authority = uri.authority()?;
    let userinfo = authority.as_str().split('@').next()?;
    // userinfo must contain '@' separator to be a credential, not a host
    if !authority.as_str().contains('@') {
        return None;
    }
    let (user, pass) = userinfo.split_once(':')?;
    if user.is_empty() {
        return None;
    }
    Some((user.to_owned(), pass.to_owned()))
}

// ---------------------------------------------------------------------------
// ProxyKind
// ---------------------------------------------------------------------------

/// Selects the proxy protocol for `ClientBuilder::build_proxy*` methods.
#[derive(Clone, Debug)]
pub enum ProxyKind {
    /// Plain HTTP CONNECT tunneling.
    HttpConnect(Uri),
    /// SOCKS5 proxy (RFC 1928 + RFC 1929).
    #[cfg(feature = "socks")]
    Socks5(Uri),
}

// ---------------------------------------------------------------------------
// ProxyConnector — HTTP CONNECT
// ---------------------------------------------------------------------------

/// A `tower_service::Service<Uri>` that connects through an HTTP CONNECT proxy.
///
/// On `call(target_uri)` it:
/// 1. Opens a TCP connection to the configured proxy.
/// 2. Sends `CONNECT target_host:target_port HTTP/1.1` with optional
///    `Proxy-Authorization: Basic ...` when credentials are present in the
///    proxy URI.
/// 3. Reads the proxy's `HTTP/1.1 200 ...` response.
/// 4. Returns the raw TCP stream, ready for further use (plain HTTP or TLS).
#[derive(Clone, Debug)]
pub struct ProxyConnector {
    proxy_uri: Uri,
    connect_timeout: Option<Duration>,
    auth: Option<(String, String)>,
}

impl ProxyConnector {
    /// Create a new `ProxyConnector`.
    ///
    /// `proxy_uri` is the address of the HTTP CONNECT proxy server (e.g.
    /// `http://proxy.example.com:8080`).  User/password credentials in the URI
    /// userinfo are automatically extracted and used as `Proxy-Authorization`.
    pub fn new(proxy_uri: Uri, connect_timeout: Option<Duration>) -> Self {
        let auth = extract_auth(&proxy_uri);
        Self {
            proxy_uri,
            connect_timeout,
            auth,
        }
    }
}

impl Service<Uri> for ProxyConnector {
    type Response = TokioIo<TcpStream>;
    type Error = OxiHttpError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let proxy_uri = self.proxy_uri.clone();
        let connect_timeout = self.connect_timeout;
        let auth = self.auth.clone();

        Box::pin(async move {
            // Resolve proxy address
            let (proxy_host, proxy_port) = uri_host_port(&proxy_uri)?;
            let proxy_addr = format!("{proxy_host}:{proxy_port}");

            // Resolve target address for the CONNECT header
            let (target_host, target_port) = uri_host_port(&uri)?;
            let target_authority = format!("{target_host}:{target_port}");

            // Connect to the proxy
            let stream = if let Some(timeout) = connect_timeout {
                tokio::time::timeout(timeout, TcpStream::connect(&proxy_addr))
                    .await
                    .map_err(|_| {
                        OxiHttpError::Timeout(format!(
                            "proxy connect timeout after {}ms",
                            timeout.as_millis()
                        ))
                    })??
            } else {
                TcpStream::connect(&proxy_addr).await?
            };

            let mut stream = stream;

            // Build CONNECT request
            let mut req =
                format!("CONNECT {target_authority} HTTP/1.1\r\nHost: {target_authority}\r\n");
            if let Some((user, pass)) = &auth {
                let credentials = format!("{user}:{pass}");
                let encoded = BASE64.encode(credentials.as_bytes());
                req.push_str(&format!("Proxy-Authorization: Basic {encoded}\r\n"));
            }
            req.push_str("\r\n");

            stream.write_all(req.as_bytes()).await?;

            // Read response until \r\n\r\n
            let mut response_buf = Vec::with_capacity(256);
            let mut single = [0u8; 1];
            loop {
                let n = stream.read(&mut single).await?;
                if n == 0 {
                    return Err(OxiHttpError::ConnectionPool(
                        "proxy closed connection during CONNECT handshake".to_owned(),
                    ));
                }
                response_buf.push(single[0]);
                if response_buf.ends_with(b"\r\n\r\n") {
                    break;
                }
                if response_buf.len() > 8192 {
                    return Err(OxiHttpError::ConnectionPool(
                        "proxy CONNECT response too large".to_owned(),
                    ));
                }
            }

            // Verify 200 status
            let first_line = response_buf
                .split(|&b| b == b'\n')
                .next()
                .and_then(|l| std::str::from_utf8(l).ok())
                .unwrap_or("");

            if !first_line.contains("200") {
                return Err(OxiHttpError::ConnectionPool(format!(
                    "proxy CONNECT rejected: {first_line}"
                )));
            }

            Ok(TokioIo::new(stream))
        })
    }
}

// ---------------------------------------------------------------------------
// Socks5Connector — SOCKS5 proxy (RFC 1928 + RFC 1929)
// ---------------------------------------------------------------------------

/// A `tower_service::Service<Uri>` that connects through a SOCKS5 proxy.
///
/// Implements the full SOCKS5 handshake including optional username/password
/// sub-negotiation (RFC 1929).  DNS names are forwarded to the proxy without
/// local resolution (ATYP 0x03).
#[cfg(feature = "socks")]
#[derive(Clone, Debug)]
pub struct Socks5Connector {
    proxy_uri: Uri,
    connect_timeout: Option<Duration>,
    auth: Option<(String, String)>,
}

#[cfg(feature = "socks")]
impl Socks5Connector {
    /// Create a new `Socks5Connector`.
    ///
    /// `proxy_uri` is the address of the SOCKS5 proxy (e.g.
    /// `socks5://user:pass@proxy.example.com:1080`).
    pub fn new(proxy_uri: Uri, connect_timeout: Option<Duration>) -> Self {
        let auth = extract_auth(&proxy_uri);
        Self {
            proxy_uri,
            connect_timeout,
            auth,
        }
    }
}

#[cfg(feature = "socks")]
impl Service<Uri> for Socks5Connector {
    type Response = TokioIo<TcpStream>;
    type Error = OxiHttpError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let proxy_uri = self.proxy_uri.clone();
        let connect_timeout = self.connect_timeout;
        let auth = self.auth.clone();

        Box::pin(async move {
            let (proxy_host, proxy_port) = uri_host_port(&proxy_uri)?;
            let proxy_addr = format!("{proxy_host}:{proxy_port}");
            let (target_host, target_port) = uri_host_port(&uri)?;

            // Connect to proxy
            let stream = if let Some(timeout) = connect_timeout {
                tokio::time::timeout(timeout, TcpStream::connect(&proxy_addr))
                    .await
                    .map_err(|_| {
                        OxiHttpError::Timeout(format!(
                            "SOCKS5 proxy connect timeout after {}ms",
                            timeout.as_millis()
                        ))
                    })??
            } else {
                TcpStream::connect(&proxy_addr).await?
            };
            let mut stream = stream;

            // Step 1: Greeting — advertise supported auth methods
            let (nmethods, methods): (u8, Vec<u8>) = if auth.is_some() {
                (2, vec![0x00, 0x02]) // no-auth + user/pass
            } else {
                (1, vec![0x00]) // no-auth only
            };
            let mut greeting = vec![0x05, nmethods];
            greeting.extend_from_slice(&methods);
            stream.write_all(&greeting).await?;

            // Step 2: Server method selection
            let mut method_resp = [0u8; 2];
            stream.read_exact(&mut method_resp).await?;
            if method_resp[0] != 0x05 {
                return Err(OxiHttpError::ConnectionPool(
                    "SOCKS5 greeting response has wrong version byte".to_owned(),
                ));
            }
            let selected = method_resp[1];
            if selected == 0xFF {
                return Err(OxiHttpError::ConnectionPool(
                    "SOCKS5 proxy rejected all authentication methods".to_owned(),
                ));
            }

            // Step 3: RFC 1929 user/pass sub-negotiation (if selected)
            if selected == 0x02 {
                let (user, pass) = auth.as_ref().ok_or_else(|| {
                    OxiHttpError::ConnectionPool(
                        "SOCKS5 proxy requires authentication but none configured".to_owned(),
                    )
                })?;
                let user_bytes = user.as_bytes();
                let pass_bytes = pass.as_bytes();
                let mut auth_req = Vec::with_capacity(3 + user_bytes.len() + pass_bytes.len());
                auth_req.push(0x01); // sub-negotiation version
                auth_req.push(user_bytes.len() as u8);
                auth_req.extend_from_slice(user_bytes);
                auth_req.push(pass_bytes.len() as u8);
                auth_req.extend_from_slice(pass_bytes);
                stream.write_all(&auth_req).await?;

                let mut auth_resp = [0u8; 2];
                stream.read_exact(&mut auth_resp).await?;
                if auth_resp[1] != 0x00 {
                    return Err(OxiHttpError::ConnectionPool(
                        "SOCKS5 authentication failed".to_owned(),
                    ));
                }
            }

            // Step 4: CONNECT command
            // Determine address type and bytes
            let (atyp, addr_bytes): (u8, Vec<u8>) = match target_host.parse::<IpAddr>() {
                Ok(IpAddr::V4(v4)) => (0x01, v4.octets().to_vec()),
                Ok(IpAddr::V6(v6)) => (0x04, v6.octets().to_vec()),
                Err(_) => {
                    // DNS hostname — send as-is (ATYP=0x03)
                    let host_bytes = target_host.as_bytes();
                    let len = host_bytes.len() as u8;
                    let mut b = Vec::with_capacity(1 + host_bytes.len());
                    b.push(len);
                    b.extend_from_slice(host_bytes);
                    (0x03, b)
                }
            };

            let port_hi = (target_port >> 8) as u8;
            let port_lo = (target_port & 0xFF) as u8;

            let mut connect_req = vec![0x05, 0x01, 0x00, atyp];
            connect_req.extend_from_slice(&addr_bytes);
            connect_req.push(port_hi);
            connect_req.push(port_lo);
            stream.write_all(&connect_req).await?;

            // Step 5: Parse reply
            let mut reply_hdr = [0u8; 4]; // VER, REP, RSV, ATYP
            stream.read_exact(&mut reply_hdr).await?;

            let rep = reply_hdr[1];
            if rep != 0x00 {
                return Err(OxiHttpError::ConnectionPool(format!(
                    "SOCKS5 error code {rep:#04x}"
                )));
            }

            // Drain BND.ADDR based on ATYP (only on success path)
            let bnd_atyp = reply_hdr[3];
            match bnd_atyp {
                0x01 => {
                    let mut buf = [0u8; 4];
                    stream.read_exact(&mut buf).await?;
                }
                0x04 => {
                    let mut buf = [0u8; 16];
                    stream.read_exact(&mut buf).await?;
                }
                0x03 => {
                    let mut len_buf = [0u8; 1];
                    stream.read_exact(&mut len_buf).await?;
                    let mut domain_buf = vec![0u8; len_buf[0] as usize];
                    stream.read_exact(&mut domain_buf).await?;
                }
                other => {
                    return Err(OxiHttpError::ConnectionPool(format!(
                        "SOCKS5 reply has unknown ATYP {other:#04x}"
                    )));
                }
            }
            // Drain BND.PORT
            let mut port_buf = [0u8; 2];
            stream.read_exact(&mut port_buf).await?;

            Ok(TokioIo::new(stream))
        })
    }
}
