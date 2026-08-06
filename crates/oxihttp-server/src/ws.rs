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

/// The frame-level payload cap ([`ws_frame::read_frame`]'s `max_payload_len`)
/// is never allowed below this floor, regardless of how small
/// [`WebSocket::set_max_message_size`] is configured.
///
/// Control frames (Ping/Pong/Close) are legal up to 125 bytes per RFC 6455
/// §5.5 and are not subject to `max_message_size` at all (they are never
/// reassembled or accumulated); flooring the frame-level cap at 125 keeps
/// them deliverable even when an operator sets a very small message-size
/// budget to bound *data* frames.
const MIN_FRAME_PAYLOAD_CAP: u64 = 125;

/// RFC 6455 §1.3 GUID appended to the client key for the accept handshake.
const WS_MAGIC: &str = "258EAFA5-E914-47DA-95CA-5AF986DFEC23";

/// Default cap on the total reassembled size of a fragmented message (16 MiB).
///
/// A malicious peer can send an unbounded stream of continuation frames with
/// FIN unset; without a ceiling the accumulated buffer would grow until the
/// process is killed. Callers can adjust the limit via
/// [`WebSocket::set_max_message_size`].
const DEFAULT_MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

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
    /// Maximum total size (in bytes) of a reassembled fragmented message.
    max_message_size: usize,
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
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
        }
    }

    /// Set the maximum total size (in bytes) of a reassembled fragmented
    /// message. Once the accumulated payload of an in-progress fragmented
    /// message would exceed this limit, [`recv`](Self::recv) fails with
    /// [`OxiHttpError::Body`] instead of buffering without bound.
    ///
    /// Defaults to 16 MiB.
    pub fn set_max_message_size(&mut self, max_bytes: usize) {
        self.max_message_size = max_bytes;
    }

    /// Receive the next complete message.
    ///
    /// - Ping frames automatically trigger a Pong reply (RFC §5.5.3) and are
    ///   then returned to the caller as `Message::Ping`.
    /// - Fragmented messages are reassembled before returning.
    /// - Returns `Ok(None)` when the connection has been closed.
    ///
    /// # Frame- and message-size limits
    ///
    /// Every frame read from the peer — fragmented or not — is bounded by
    /// [`max_message_size`](Self::set_max_message_size): a single
    /// unfragmented data frame larger than the configured limit is rejected
    /// exactly like an over-budget reassembled (fragmented) message, so
    /// `set_max_message_size` genuinely bounds per-connection memory rather
    /// than only the fragmented-message path. Control frames (Ping/Pong/
    /// Close, capped at 125 bytes by RFC 6455 §5.5 regardless) are never
    /// subject to this limit.
    pub async fn recv(&mut self) -> Result<Option<Message>, OxiHttpError> {
        if self.closed {
            return Ok(None);
        }
        loop {
            // The wire-level frame cap floors at `MIN_FRAME_PAYLOAD_CAP` so a
            // very small `max_message_size` cannot make legal (≤125-byte)
            // control frames unreadable; *data* frames are still bounded
            // precisely to `max_message_size` below via `check_size_limit`.
            let frame_cap = (self.max_message_size as u64).max(MIN_FRAME_PAYLOAD_CAP);
            let frame = match read_frame(&mut self.stream, frame_cap).await {
                Ok(frame) => frame,
                Err(e) => {
                    // Any wire-level parse/protocol failure (including the
                    // RFC 6455 §5.1 unmasked-frame rejection and the
                    // per-frame size cap above) ends the connection — do not
                    // let the caller keep pulling frames from a peer that
                    // just violated the protocol.
                    self.closed = true;
                    return Err(e);
                }
            };
            match (frame.opcode, frame.fin) {
                // ── Control frames (must not be fragmented, RFC §5.5) ──────────
                (Opcode::Ping, _) => {
                    // Auto-reply with Pong (RFC §5.5.3). Move (not copy) the
                    // payload into the reply and the returned message.
                    let payload = frame.payload;
                    write_frame(&mut self.stream, Opcode::Pong, &payload, true).await?;
                    return Ok(Some(Message::Ping(payload)));
                }
                (Opcode::Pong, _) => {
                    return Ok(Some(Message::Pong(frame.payload)));
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
                    // Bound a single unfragmented frame by the same budget as
                    // a reassembled fragmented message (see doc comment
                    // above) — this is the fix for the "single 64 MiB frame
                    // bypasses set_max_message_size" gap.
                    self.check_size_limit(frame.payload.len())?;
                    return Ok(Some(make_data_message(opcode, frame.payload)?));
                }

                // ── First fragment of a fragmented message ─────────────────────
                (opcode @ (Opcode::Text | Opcode::Binary), false) if self.frag_buf.is_empty() => {
                    self.check_size_limit(frame.payload.len())?;
                    self.frag_opcode = Some(opcode);
                    self.frag_buf.extend_from_slice(&frame.payload);
                }

                // ── Continuation frame ─────────────────────────────────────────
                (Opcode::Continuation, fin) => {
                    self.check_size_limit(frame.payload.len())?;
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

    /// Ensure that appending `incoming` bytes to the current message (either
    /// the in-progress reassembly buffer, or — for a single unfragmented
    /// frame — a message of exactly `incoming` bytes) would not exceed
    /// [`max_message_size`](Self::set_max_message_size).
    ///
    /// On overflow the connection is marked closed (so no further frames are
    /// read) and an [`OxiHttpError::Body`] is returned. This bounds the memory
    /// a single message — fragmented or not — can consume.
    fn check_size_limit(&mut self, incoming: usize) -> Result<(), OxiHttpError> {
        let total = self.frag_buf.len().saturating_add(incoming);
        if total > self.max_message_size {
            self.closed = true;
            self.frag_buf = Vec::new();
            self.frag_opcode = None;
            return Err(OxiHttpError::Body(format!(
                "WebSocket: message exceeds maximum of {} bytes",
                self.max_message_size
            )));
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ws_frame::write_frame_masked;
    use tokio::io::AsyncWriteExt;

    /// A fragmented message whose total reassembled size exceeds the configured
    /// maximum must be rejected rather than buffered without bound.
    #[tokio::test]
    async fn oversized_reassembly_is_rejected() {
        // Duplex pipe: we write client frames into `client`, the WebSocket
        // reads from `server`.
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let mut ws = WebSocket::new(server);
        // Small cap so the test stays fast and deterministic.
        ws.set_max_message_size(1024);

        // Writer task: first fragment (512 B, FIN=0) then a continuation frame
        // (1024 B, FIN=0) which pushes the total to 1536 B > 1024 B cap.
        let writer = tokio::spawn(async move {
            let first = vec![0xAAu8; 512];
            write_frame_masked(&mut client, Opcode::Binary, &first, false, [1, 2, 3, 4])
                .await
                .expect("write first fragment");
            let cont = vec![0xBBu8; 1024];
            let _ = write_frame_masked(
                &mut client,
                Opcode::Continuation,
                &cont,
                false,
                [5, 6, 7, 8],
            )
            .await;
            // Keep the pipe open briefly so the reader observes both frames.
            let _ = client.flush().await;
        });

        let err = ws
            .recv()
            .await
            .expect_err("oversized reassembly must error");
        match err {
            OxiHttpError::Body(msg) => {
                assert!(
                    msg.contains("exceeds maximum"),
                    "unexpected error message: {msg}"
                );
            }
            other => panic!("expected OxiHttpError::Body, got {other:?}"),
        }

        // After the overflow the connection is marked closed.
        assert!(ws.recv().await.expect("closed recv").is_none());
        writer.await.expect("writer task");
    }

    /// A fragmented message that stays within the cap is reassembled normally.
    #[tokio::test]
    async fn in_limit_reassembly_succeeds() {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let mut ws = WebSocket::new(server);
        ws.set_max_message_size(1024);

        let writer = tokio::spawn(async move {
            let first = vec![0x01u8; 400];
            write_frame_masked(&mut client, Opcode::Binary, &first, false, [1, 2, 3, 4])
                .await
                .expect("write first fragment");
            let cont = vec![0x02u8; 400];
            write_frame_masked(&mut client, Opcode::Continuation, &cont, true, [5, 6, 7, 8])
                .await
                .expect("write final fragment");
            let _ = client.flush().await;
        });

        let msg = ws.recv().await.expect("recv ok").expect("some message");
        match msg {
            Message::Binary(data) => assert_eq!(data.len(), 800),
            other => panic!("expected Binary, got {other:?}"),
        }
        writer.await.expect("writer task");
    }

    /// Regression test: a single **unfragmented** data frame larger than
    /// `max_message_size` must be rejected exactly like an over-budget
    /// reassembled message — before this fix, `set_max_message_size` only
    /// bounded the fragmented-reassembly path, so a peer could send one
    /// large unfragmented frame (up to the 64 MiB wire-level ceiling) and
    /// bypass the configured budget entirely.
    ///
    /// With `max_message_size` at or above the `MIN_FRAME_PAYLOAD_CAP`
    /// floor (as here), the wire-level `read_frame` cap and
    /// `check_size_limit` enforce the *same* threshold, so the rejection
    /// happens at the `read_frame` layer (cheaper: it never even allocates
    /// a buffer for the oversized payload). Either layer catching it is a
    /// correct outcome; see
    /// `unfragmented_frame_between_max_message_size_and_frame_floor_is_rejected`
    /// below for a case that specifically exercises `check_size_limit`.
    #[tokio::test]
    async fn oversized_unfragmented_frame_is_rejected() {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let mut ws = WebSocket::new(server);
        ws.set_max_message_size(1024);

        let writer = tokio::spawn(async move {
            // A single FIN=1 Binary frame of 2048 bytes — no fragmentation
            // at all — well over the 1024-byte configured cap.
            let payload = vec![0xCCu8; 2048];
            let _ =
                write_frame_masked(&mut client, Opcode::Binary, &payload, true, [9, 9, 9, 9]).await;
            let _ = client.flush().await;
        });

        let err = ws
            .recv()
            .await
            .expect_err("oversized unfragmented frame must error");
        match err {
            OxiHttpError::Body(msg) => {
                assert!(
                    msg.contains("too large") || msg.contains("exceeds maximum"),
                    "unexpected error message: {msg}"
                );
            }
            other => panic!("expected OxiHttpError::Body, got {other:?}"),
        }
        assert!(ws.recv().await.expect("closed recv").is_none());
        writer.await.expect("writer task");
    }

    /// Regression test specifically for the `check_size_limit` layer (as
    /// opposed to the `read_frame`-level wire cap, which floors at
    /// [`MIN_FRAME_PAYLOAD_CAP`] to keep control frames deliverable): with a
    /// `max_message_size` below that floor, a data frame whose payload fits
    /// under the floor but still exceeds `max_message_size` must still be
    /// rejected — the floor only protects legal-sized control frames, it
    /// must not silently raise the effective data-frame budget.
    #[tokio::test]
    async fn unfragmented_frame_between_max_message_size_and_frame_floor_is_rejected() {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let mut ws = WebSocket::new(server);
        ws.set_max_message_size(16); // well under MIN_FRAME_PAYLOAD_CAP (125)

        let writer = tokio::spawn(async move {
            // 100 bytes: passes the floored 125-byte read_frame cap, but
            // must still be rejected by max_message_size=16.
            let payload = vec![0xEEu8; 100];
            let _ =
                write_frame_masked(&mut client, Opcode::Binary, &payload, true, [3, 1, 4, 1]).await;
            let _ = client.flush().await;
        });

        let err = ws
            .recv()
            .await
            .expect_err("frame between max_message_size and the frame floor must be rejected");
        match err {
            OxiHttpError::Body(msg) => {
                assert!(
                    msg.contains("exceeds maximum"),
                    "unexpected error message: {msg}"
                );
            }
            other => panic!("expected OxiHttpError::Body, got {other:?}"),
        }
        writer.await.expect("writer task");
    }

    /// A single unfragmented frame that stays within `max_message_size`
    /// still succeeds (regression guard against an over-eager cap).
    #[tokio::test]
    async fn in_limit_unfragmented_frame_succeeds() {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let mut ws = WebSocket::new(server);
        ws.set_max_message_size(1024);

        let writer = tokio::spawn(async move {
            let payload = vec![0x07u8; 900];
            write_frame_masked(&mut client, Opcode::Binary, &payload, true, [1, 1, 1, 1])
                .await
                .expect("write frame");
            let _ = client.flush().await;
        });

        let msg = ws.recv().await.expect("recv ok").expect("some message");
        match msg {
            Message::Binary(data) => assert_eq!(data.len(), 900),
            other => panic!("expected Binary, got {other:?}"),
        }
        writer.await.expect("writer task");
    }

    /// Regression test for the RFC 6455 §5.1 conformance gap: the server
    /// must reject an unmasked frame from the client rather than silently
    /// accepting it, and the connection must be marked closed afterward so
    /// a caller that ignores the error cannot keep reading from a peer that
    /// just violated the protocol.
    #[tokio::test]
    async fn unmasked_client_frame_is_rejected_and_closes_connection() {
        use crate::ws_frame::write_frame;

        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let mut ws = WebSocket::new(server);

        let writer = tokio::spawn(async move {
            // `write_frame` (not `write_frame_masked`) never sets the mask
            // bit — exactly what an RFC-violating client would send.
            let _ = write_frame(&mut client, Opcode::Text, b"hello", true).await;
            let _ = client.flush().await;
        });

        let err = ws
            .recv()
            .await
            .expect_err("unmasked frame must be rejected");
        match err {
            OxiHttpError::Body(msg) => {
                assert!(msg.contains("unmasked"), "unexpected error message: {msg}");
            }
            other => panic!("expected OxiHttpError::Body, got {other:?}"),
        }
        // The connection must be closed, not merely have returned one error.
        assert!(ws.recv().await.expect("closed recv").is_none());
        writer.await.expect("writer task");
    }

    /// A Ping with a payload between `max_message_size` and 125 bytes must
    /// still be delivered (and auto-Pong'd) even when the operator has
    /// configured a very small message-size budget — control frames are not
    /// subject to `max_message_size`, only to their own RFC-mandated
    /// 125-byte ceiling.
    #[tokio::test]
    async fn ping_within_125_bytes_survives_tiny_max_message_size() {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let mut ws = WebSocket::new(server);
        // Far below 125: without the frame-cap floor this would make even a
        // legal control frame's payload unreadable.
        ws.set_max_message_size(16);

        // Written directly (not from a spawned task) and `client` kept alive
        // for the whole test: `recv()` triggers an auto-Pong write-back on
        // the server side, which needs a live peer to write to. The 100-byte
        // write fits comfortably in the 64 KiB duplex buffer without a
        // concurrent reader.
        let payload = vec![0x5Au8; 100];
        write_frame_masked(&mut client, Opcode::Ping, &payload, true, [2, 4, 6, 8])
            .await
            .expect("write ping");
        client.flush().await.expect("flush");

        let msg = ws.recv().await.expect("recv ok").expect("some message");
        match msg {
            Message::Ping(data) => assert_eq!(data.len(), 100),
            other => panic!("expected Ping, got {other:?}"),
        }
        drop(client);
    }
}
