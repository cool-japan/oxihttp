//! WebSocket upgrade and message handling (RFC 6455).
//!
//! # Example
//!
//! ```no_run
//! use oxihttp_server::{Router, Server};
//! use oxihttp_server::ws;
//!
//! # async fn example() -> Result<(), oxihttp_core::OxiHttpError> {
//! let router = Router::new()
//!     .get("/ws", |req| async move {
//!         let (upgrade, resp) = ws::upgrade(req)?;
//!         tokio::spawn(async move {
//!             if let Ok(mut socket) = upgrade.accept().await {
//!                 while let Ok(Some(msg)) = socket.recv().await {
//!                     if socket.send(msg).await.is_err() {
//!                         break;
//!                     }
//!                 }
//!             }
//!         });
//!         Ok(resp)
//!     });
//! # Ok(())
//! # }
//! ```

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use bytes::Bytes;
use http_body_util::Full;
use hyper_util::rt::TokioIo;
use oxihttp_core::OxiHttpError;
use sha1::{Digest, Sha1};

use crate::ws_frame::{read_frame, write_frame, Opcode};

/// RFC 6455 §1.3 GUID appended to the client key for the accept handshake.
const WS_MAGIC: &str = "258EAFA5-E914-47DA-95CA-5AF986DFEC23";

// ---------------------------------------------------------------------------
// Message
// ---------------------------------------------------------------------------

/// A complete WebSocket message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// UTF-8 text message.
    Text(String),
    /// Binary message.
    Binary(Vec<u8>),
    /// Ping with optional payload (≤ 125 bytes).
    Ping(Vec<u8>),
    /// Pong with optional payload (≤ 125 bytes).
    Pong(Vec<u8>),
    /// Connection-close message with optional close frame payload.
    Close(Option<CloseFrame>),
}

/// Close frame payload (RFC 6455 §5.5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseFrame {
    /// Status code (e.g. 1000 = normal closure, 1001 = going away, …).
    pub code: u16,
    /// Human-readable close reason (UTF-8, may be empty).
    pub reason: String,
}

// ---------------------------------------------------------------------------
// WebSocket<S>
// ---------------------------------------------------------------------------

/// A WebSocket connection over an arbitrary async stream.
///
/// `S` is typically `TokioIo<hyper::upgrade::Upgraded>` on the server side.
/// The stream must implement `tokio::io::{AsyncRead, AsyncWrite} + Unpin`.
pub struct WebSocket<S> {
    stream: S,
    /// Accumulated payload bytes for a fragmented message in progress.
    frag_buf: Vec<u8>,
    /// Opcode of the first fragment (Text or Binary).
    frag_opcode: Option<Opcode>,
    /// True after a Close frame has been received OR after `close()` was called.
    closed: bool,
    /// True after a Close frame was sent by *us* (prevents double-send in recv).
    close_sent: bool,
}

impl<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin> WebSocket<S> {
    /// Wrap an existing async stream in a WebSocket.
    pub(crate) fn new(stream: S) -> Self {
        Self {
            stream,
            frag_buf: Vec::new(),
            frag_opcode: None,
            closed: false,
            close_sent: false,
        }
    }

    /// Receive the next complete message.
    ///
    /// - Ping frames automatically trigger a Pong reply (RFC §5.5.3) and are
    ///   then returned to the caller as `Message::Ping`.
    /// - Fragmented messages are reassembled before returning.
    /// - Returns `Ok(None)` when the connection has been closed.
    pub async fn recv(&mut self) -> Result<Option<Message>, OxiHttpError> {
        if self.closed {
            return Ok(None);
        }
        loop {
            let frame = read_frame(&mut self.stream).await?;
            match (frame.opcode, frame.fin) {
                // ── Control frames (must not be fragmented, RFC §5.5) ──────────
                (Opcode::Ping, _) => {
                    // Auto-reply with Pong (RFC §5.5.3).
                    write_frame(&mut self.stream, Opcode::Pong, &frame.payload, true).await?;
                    return Ok(Some(Message::Ping(frame.payload.to_vec())));
                }
                (Opcode::Pong, _) => {
                    return Ok(Some(Message::Pong(frame.payload.to_vec())));
                }
                (Opcode::Close, _) => {
                    // Echo the Close back only if we haven't sent one ourselves.
                    if !self.close_sent {
                        write_frame(&mut self.stream, Opcode::Close, &frame.payload, true).await?;
                    }
                    self.closed = true;
                    let close = parse_close_frame(&frame.payload);
                    return Ok(Some(Message::Close(close)));
                }

                // ── Unfragmented data frame ────────────────────────────────────
                (opcode @ (Opcode::Text | Opcode::Binary), true) if self.frag_buf.is_empty() => {
                    return Ok(Some(make_data_message(opcode, frame.payload.to_vec())?));
                }

                // ── First fragment of a fragmented message ─────────────────────
                (opcode @ (Opcode::Text | Opcode::Binary), false) if self.frag_buf.is_empty() => {
                    self.frag_opcode = Some(opcode);
                    self.frag_buf.extend_from_slice(&frame.payload);
                }

                // ── Continuation frame ─────────────────────────────────────────
                (Opcode::Continuation, fin) => {
                    self.frag_buf.extend_from_slice(&frame.payload);
                    if fin {
                        let opcode = self.frag_opcode.take().ok_or_else(|| {
                            OxiHttpError::Body(
                                "WebSocket: continuation frame without start frame".into(),
                            )
                        })?;
                        let data = std::mem::take(&mut self.frag_buf);
                        return Ok(Some(make_data_message(opcode, data)?));
                    }
                }

                // ── Unexpected combinations ────────────────────────────────────
                _ => {
                    return Err(OxiHttpError::Body(
                        "WebSocket: unexpected frame sequence".into(),
                    ));
                }
            }
        }
    }

    /// Send a WebSocket message.
    pub async fn send(&mut self, msg: Message) -> Result<(), OxiHttpError> {
        match msg {
            Message::Text(s) => {
                write_frame(&mut self.stream, Opcode::Text, s.as_bytes(), true).await
            }
            Message::Binary(b) => write_frame(&mut self.stream, Opcode::Binary, &b, true).await,
            Message::Ping(p) => write_frame(&mut self.stream, Opcode::Ping, &p, true).await,
            Message::Pong(p) => write_frame(&mut self.stream, Opcode::Pong, &p, true).await,
            Message::Close(cf) => {
                let mut payload = Vec::new();
                if let Some(cf) = cf {
                    payload.extend_from_slice(&cf.code.to_be_bytes());
                    payload.extend_from_slice(cf.reason.as_bytes());
                }
                self.close_sent = true;
                self.closed = true;
                write_frame(&mut self.stream, Opcode::Close, &payload, true).await
            }
        }
    }

    /// Initiate a clean Close handshake and drain until the peer's echo arrives.
    ///
    /// This sends a Close frame with the given code and reason, then reads
    /// incoming frames until the peer's Close echo is received or an I/O error
    /// occurs.
    pub async fn close(mut self, code: u16, reason: &str) -> Result<(), OxiHttpError> {
        let mut payload = code.to_be_bytes().to_vec();
        payload.extend_from_slice(reason.as_bytes());
        self.close_sent = true;
        write_frame(&mut self.stream, Opcode::Close, &payload, true).await?;
        // Drain until peer echoes Close.
        while let Ok(Some(msg)) = self.recv().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// WebSocketUpgrade
// ---------------------------------------------------------------------------

/// Pending WebSocket upgrade returned by [`upgrade`].
///
/// The caller must:
/// 1. Return the 101 response to hyper immediately.
/// 2. In a `tokio::spawn`ed task, call `upgrade.accept().await` to obtain the
///    [`WebSocket`] handle.
///
/// This two-step dance is required because hyper only flushes the 101 response
/// (and completes the upgrade) once the response future is polled by the
/// connection handler — which happens *after* the handler returns.
pub struct WebSocketUpgrade {
    /// Holds the hyper `OnUpgrade` future directly.  We resolve it lazily
    /// inside `accept()` so there is no need for an extra oneshot channel.
    on_upgrade: hyper::upgrade::OnUpgrade,
}

impl WebSocketUpgrade {
    /// Resolve the upgrade future and return the WebSocket.
    ///
    /// Call this **inside a `tokio::spawn`** task, *after* returning the 101
    /// response from your handler.
    pub async fn accept(
        self,
    ) -> Result<WebSocket<TokioIo<hyper::upgrade::Upgraded>>, OxiHttpError> {
        let upgraded = self
            .on_upgrade
            .await
            .map_err(|e| OxiHttpError::Body(format!("WebSocket upgrade failed: {e}")))?;
        Ok(WebSocket::new(TokioIo::new(upgraded)))
    }
}

// ---------------------------------------------------------------------------
// upgrade()
// ---------------------------------------------------------------------------

/// Validate an HTTP→WebSocket upgrade request and build the 101 response.
///
/// Returns `(WebSocketUpgrade, 101 response)` on success.  The caller must:
/// 1. Spawn a task that calls `upgrade.accept().await` — the `WebSocket` is
///    available inside that task once hyper flushes the 101.
/// 2. Return the 101 response from the handler *synchronously* (no await after
///    the spawn).
///
/// # Errors
/// Returns `OxiHttpError::Body` when mandatory upgrade headers are missing or
/// invalid (e.g. wrong version, not a WebSocket upgrade).
pub fn upgrade(
    req: crate::router::Request,
) -> Result<(WebSocketUpgrade, http::Response<Full<Bytes>>), OxiHttpError> {
    // 1. Validate upgrade headers.
    let key = validate_upgrade_request(req.headers())?;

    // 2. Compute the Sec-WebSocket-Accept value.
    let accept = compute_accept_key(&key);

    // 3. Consume the request to obtain the upgrade future.
    let inner = req.into_inner();
    let on_upgrade = hyper::upgrade::on(inner);

    // 4. Build the 101 Switching Protocols response.
    let response = http::Response::builder()
        .status(http::StatusCode::SWITCHING_PROTOCOLS)
        .header(http::header::UPGRADE, "websocket")
        .header(http::header::CONNECTION, "Upgrade")
        .header("Sec-WebSocket-Accept", accept)
        .body(Full::new(Bytes::new()))
        .map_err(|e| OxiHttpError::Http(std::sync::Arc::new(e)))?;

    Ok((WebSocketUpgrade { on_upgrade }, response))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Validate the mandatory WebSocket upgrade headers.
///
/// Per RFC 6455 §4.1 the request must contain:
/// - `Upgrade: websocket` (case-insensitive)
/// - `Sec-WebSocket-Version: 13`
/// - `Sec-WebSocket-Key` (a non-empty value)
fn validate_upgrade_request(headers: &http::HeaderMap) -> Result<String, OxiHttpError> {
    let upgrade = headers
        .get(http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| OxiHttpError::Body("WebSocket: missing Upgrade header".into()))?;
    if !upgrade.eq_ignore_ascii_case("websocket") {
        return Err(OxiHttpError::Body(format!(
            "WebSocket: Upgrade header is '{upgrade}', expected 'websocket'"
        )));
    }

    let version = headers
        .get("Sec-WebSocket-Version")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            OxiHttpError::Body("WebSocket: missing Sec-WebSocket-Version header".into())
        })?;
    if version != "13" {
        return Err(OxiHttpError::Body(format!(
            "WebSocket: unsupported version '{version}', only version 13 is supported"
        )));
    }

    let key = headers
        .get("Sec-WebSocket-Key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| OxiHttpError::Body("WebSocket: missing Sec-WebSocket-Key header".into()))?
        .to_owned();

    Ok(key)
}

/// Compute `Sec-WebSocket-Accept` per RFC 6455 §4.2.2.
fn compute_accept_key(key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(WS_MAGIC.as_bytes());
    let hash = hasher.finalize();
    BASE64.encode(hash)
}

/// Convert raw payload bytes into a Text or Binary message.
fn make_data_message(opcode: Opcode, data: Vec<u8>) -> Result<Message, OxiHttpError> {
    match opcode {
        Opcode::Text => {
            let s = String::from_utf8(data)
                .map_err(|e| OxiHttpError::Body(format!("WebSocket: invalid UTF-8: {e}")))?;
            Ok(Message::Text(s))
        }
        Opcode::Binary => Ok(Message::Binary(data)),
        _ => Err(OxiHttpError::Body(
            "WebSocket: unexpected opcode in make_data_message".into(),
        )),
    }
}

/// Parse a Close frame payload into a `CloseFrame`, if the payload is
/// long enough to contain a status code.
fn parse_close_frame(payload: &[u8]) -> Option<CloseFrame> {
    if payload.len() < 2 {
        return None;
    }
    let code = u16::from_be_bytes([payload[0], payload[1]]);
    let reason = String::from_utf8_lossy(&payload[2..]).into_owned();
    Some(CloseFrame { code, reason })
}
