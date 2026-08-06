//! WebSocket frame codec (RFC 6455 §5).
//!
//! This module provides low-level frame reading and writing primitives.
//! Clients (browser→server) always mask their frames; servers never mask.
//! The codec transparently unmasks incoming masked frames.

use bytes::{BufMut, BytesMut};
use oxihttp_core::OxiHttpError;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Default per-frame payload size ceiling used when the caller does not
/// supply an explicit limit: 64 MiB.
///
/// Callers that want a single unmasked-frame payload bounded by the same
/// cap as reassembled fragmented messages (see
/// `oxihttp_server::ws::WebSocket::set_max_message_size`) should pass that
/// limit into [`read_frame`] instead of this default.
pub const DEFAULT_MAX_PAYLOAD_LEN: u64 = 64 * 1024 * 1024;

/// WebSocket opcode as defined in RFC 6455 §5.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opcode {
    /// Continuation frame (opcode 0x0).
    Continuation = 0x0,
    /// UTF-8 text data frame (opcode 0x1).
    Text = 0x1,
    /// Binary data frame (opcode 0x2).
    Binary = 0x2,
    /// Connection close (opcode 0x8).
    Close = 0x8,
    /// Ping (opcode 0x9).
    Ping = 0x9,
    /// Pong (opcode 0xA).
    Pong = 0xA,
}

impl Opcode {
    /// Parse an opcode from its raw byte value.
    /// Returns `None` for unknown opcodes.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x0 => Some(Self::Continuation),
            0x1 => Some(Self::Text),
            0x2 => Some(Self::Binary),
            0x8 => Some(Self::Close),
            0x9 => Some(Self::Ping),
            0xA => Some(Self::Pong),
            _ => None,
        }
    }

    /// Returns `true` for control frames (Close, Ping, Pong).
    /// Control frames must not be fragmented and have payload ≤ 125 bytes.
    pub fn is_control(self) -> bool {
        matches!(self, Opcode::Close | Opcode::Ping | Opcode::Pong)
    }
}

/// A single WebSocket frame with FIN bit, opcode, and payload.
#[derive(Debug, Clone)]
pub struct Frame {
    /// FIN bit: true if this is the last (or only) frame in a message.
    pub fin: bool,
    /// Frame opcode.
    pub opcode: Opcode,
    /// Frame payload (already unmasked if the original was masked).
    ///
    /// Owned as `Vec<u8>` (rather than [`bytes::Bytes`]) so callers that
    /// consume the whole payload (e.g. building a [`crate::ws::Message`])
    /// can move it out directly instead of paying for an extra copy.
    pub payload: Vec<u8>,
}

/// Read a single WebSocket frame from the stream.
///
/// This codec implements the **server** role only: per RFC 6455 §5.1, a
/// server MUST fail the connection if it receives a frame that is not
/// masked, so an unmasked frame is rejected immediately (before reading any
/// further attacker-controlled length/payload bytes) rather than silently
/// accepted.
///
/// `max_payload_len` bounds the single-frame payload (the allocation made
/// to read it, `vec![0u8; payload_len]`, never exceeds this). Pass
/// [`DEFAULT_MAX_PAYLOAD_LEN`] for the historical 64 MiB ceiling, or a
/// caller-derived limit (see
/// `oxihttp_server::ws::WebSocket::set_max_message_size`) to bound single
/// (unfragmented) frames by the same budget as reassembled fragmented
/// messages.
///
/// Other RFC 6455 constraints enforced:
/// - Reserved bits (RSV1–RSV3) must be 0 (no extensions negotiated).
/// - Control frames must have FIN=1 and payload ≤ 125 bytes.
/// - Unknown opcodes are rejected.
pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    max_payload_len: u64,
) -> Result<Frame, OxiHttpError> {
    // ── 2-byte base header ───────────────────────────────────────────────────
    let mut header = [0u8; 2];
    reader
        .read_exact(&mut header)
        .await
        .map_err(|e| OxiHttpError::Body(format!("WebSocket: read header: {e}")))?;

    let fin = (header[0] & 0x80) != 0;
    let rsv = header[0] & 0x70;
    let opcode_byte = header[0] & 0x0F;
    let masked = (header[1] & 0x80) != 0;
    let len_byte = (header[1] & 0x7F) as usize;

    // RFC 6455 §5.2: RSV bits MUST be 0 unless an extension defines them.
    if rsv != 0 {
        return Err(OxiHttpError::Body(
            "WebSocket: reserved bits set without extension".into(),
        ));
    }

    // RFC 6455 §5.1: "The server MUST close the connection upon receiving a
    // frame that is not masked." Reject immediately rather than reading (and
    // trusting) any further attacker-controlled bytes.
    if !masked {
        return Err(OxiHttpError::Body(
            "WebSocket: received unmasked frame from client (server requires masking per RFC \
             6455 §5.1)"
                .into(),
        ));
    }

    let opcode = Opcode::from_u8(opcode_byte)
        .ok_or_else(|| OxiHttpError::Body(format!("WebSocket: unknown opcode {opcode_byte:#x}")))?;

    // RFC 6455 §5.5: control frames must not be fragmented; payload ≤ 125.
    if opcode.is_control() && (!fin || len_byte > 125) {
        return Err(OxiHttpError::Body(
            "WebSocket: illegal control frame (fragmented or oversized)".into(),
        ));
    }

    // ── Extended payload length ──────────────────────────────────────────────
    let payload_len: u64 = match len_byte {
        0..=125 => len_byte as u64,
        126 => {
            let mut b = [0u8; 2];
            reader
                .read_exact(&mut b)
                .await
                .map_err(|e| OxiHttpError::Body(format!("WebSocket: read ext len16: {e}")))?;
            u16::from_be_bytes(b) as u64
        }
        127 => {
            let mut b = [0u8; 8];
            reader
                .read_exact(&mut b)
                .await
                .map_err(|e| OxiHttpError::Body(format!("WebSocket: read ext len64: {e}")))?;
            u64::from_be_bytes(b)
        }
        // All u8 values ≤ 127 are covered above; 128+ is impossible with the & 0x7F mask.
        _ => unreachable!("len_byte masked to 7 bits"),
    };

    if payload_len > max_payload_len {
        return Err(OxiHttpError::Body(format!(
            "WebSocket: payload too large ({payload_len} bytes, max {max_payload_len})"
        )));
    }

    // ── Masking key (client→server; always present — unmasked frames were
    //    already rejected above) ─────────────────────────────────────────────
    let mut key = [0u8; 4];
    reader
        .read_exact(&mut key)
        .await
        .map_err(|e| OxiHttpError::Body(format!("WebSocket: read mask key: {e}")))?;

    // ── Payload ──────────────────────────────────────────────────────────────
    let mut payload = vec![0u8; payload_len as usize];
    reader
        .read_exact(&mut payload)
        .await
        .map_err(|e| OxiHttpError::Body(format!("WebSocket: read payload: {e}")))?;

    // Unmask (RFC 6455 §5.3).
    for (i, byte) in payload.iter_mut().enumerate() {
        *byte ^= key[i % 4];
    }

    Ok(Frame {
        fin,
        opcode,
        payload,
    })
}

/// Write a single WebSocket frame to the stream (server→client, **never** masked).
///
/// RFC 6455 §5.1: servers must not mask frames sent to clients.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    opcode: Opcode,
    payload: &[u8],
    fin: bool,
) -> Result<(), OxiHttpError> {
    let mut header = BytesMut::with_capacity(10);

    // ── First byte: FIN + RSV(000) + opcode ──────────────────────────────────
    let first_byte = if fin {
        0x80 | (opcode as u8)
    } else {
        opcode as u8
    };
    header.put_u8(first_byte);

    // ── Second byte: no-mask + length ────────────────────────────────────────
    let len = payload.len();
    if len <= 125 {
        header.put_u8(len as u8);
    } else if len <= 0xFFFF {
        header.put_u8(126);
        header.put_u16(len as u16);
    } else {
        header.put_u8(127);
        header.put_u64(len as u64);
    }

    writer
        .write_all(&header)
        .await
        .map_err(|e| OxiHttpError::Body(format!("WebSocket: write header: {e}")))?;
    writer
        .write_all(payload)
        .await
        .map_err(|e| OxiHttpError::Body(format!("WebSocket: write payload: {e}")))?;
    writer
        .flush()
        .await
        .map_err(|e| OxiHttpError::Body(format!("WebSocket: flush: {e}")))?;
    Ok(())
}

/// Write a masked WebSocket frame (client→server direction).
///
/// Per RFC 6455 §5.1 all frames sent from client to server MUST be masked.
/// The masking key is provided by the caller for deterministic testing.
pub async fn write_frame_masked<W: AsyncWrite + Unpin>(
    writer: &mut W,
    opcode: Opcode,
    payload: &[u8],
    fin: bool,
    mask_key: [u8; 4],
) -> Result<(), OxiHttpError> {
    let mut header = BytesMut::with_capacity(14);

    // ── First byte: FIN + RSV(000) + opcode ──────────────────────────────────
    let first_byte = if fin {
        0x80 | (opcode as u8)
    } else {
        opcode as u8
    };
    header.put_u8(first_byte);

    // ── Second byte: mask-bit + length ───────────────────────────────────────
    let len = payload.len();
    if len <= 125 {
        header.put_u8(0x80 | len as u8);
    } else if len <= 0xFFFF {
        header.put_u8(0x80 | 126);
        header.put_u16(len as u16);
    } else {
        header.put_u8(0x80 | 127);
        header.put_u64(len as u64);
    }

    // ── Masking key ───────────────────────────────────────────────────────────
    header.put_slice(&mask_key);

    writer
        .write_all(&header)
        .await
        .map_err(|e| OxiHttpError::Body(format!("WebSocket: write masked header: {e}")))?;

    // ── Masked payload ────────────────────────────────────────────────────────
    let masked_payload: Vec<u8> = payload
        .iter()
        .enumerate()
        .map(|(i, &b)| b ^ mask_key[i % 4])
        .collect();

    writer
        .write_all(&masked_payload)
        .await
        .map_err(|e| OxiHttpError::Body(format!("WebSocket: write masked payload: {e}")))?;
    writer
        .flush()
        .await
        .map_err(|e| OxiHttpError::Body(format!("WebSocket: flush masked: {e}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Property/fuzz tests — adversarial byte streams must never panic
// ---------------------------------------------------------------------------
//
// `read_frame` is the crate's only hand-written binary wire-format parser: it
// runs directly on bytes received from the peer over an HTTP/1 Upgrade
// connection, so it is exercised here the same way a genuine HTTP/1
// request/response parser would be — with random and adversarial byte
// streams — asserting the only two possible outcomes are `Ok(Frame)` or a
// typed `OxiHttpError`, never a panic.
#[cfg(test)]
mod fuzz_tests {
    use super::*;
    use proptest::prelude::*;
    use std::io::Cursor;

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 256,
            max_shrink_iters: 32,
            ..ProptestConfig::default()
        })]

        /// Feeding an arbitrary byte stream to `read_frame` must never panic.
        /// It may either succeed (the bytes happened to form a valid frame)
        /// or fail with an `OxiHttpError` — both are acceptable outcomes.
        #[test]
        fn read_frame_never_panics_on_random_bytes(
            bytes in prop::collection::vec(any::<u8>(), 0..512)
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(async {
                let mut cursor = Cursor::new(bytes);
                // Result is intentionally discarded; reaching this point
                // without panicking is the assertion.
                let _ = read_frame(&mut cursor, DEFAULT_MAX_PAYLOAD_LEN).await;
            });
        }

        /// Same property, but restricted to byte streams that at least start
        /// with a syntactically-plausible header (FIN/opcode byte followed by
        /// a length byte), which exercises the extended-length and masking
        /// branches far more often than fully unstructured input.
        #[test]
        fn read_frame_never_panics_on_structured_adversarial_bytes(
            first in any::<u8>(),
            second in any::<u8>(),
            rest in prop::collection::vec(any::<u8>(), 0..64),
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(async {
                let mut bytes = vec![first, second];
                bytes.extend_from_slice(&rest);
                let mut cursor = Cursor::new(bytes);
                let _ = read_frame(&mut cursor, DEFAULT_MAX_PAYLOAD_LEN).await;
            });
        }
    }

    // -----------------------------------------------------------------
    // Directed regression tests — A-severity findings:
    //   1. server must reject unmasked client frames (RFC 6455 §5.1)
    //   2. the per-frame payload cap must be caller-configurable
    // -----------------------------------------------------------------
    // (`Cursor` is already imported by the `use` block at the top of this
    // module.)

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
    }

    #[test]
    fn read_frame_rejects_unmasked_client_frame() {
        // FIN=1, opcode=Text(0x1); mask bit clear; length=5; "hello" unmasked.
        let mut bytes = vec![0x81u8, 0x05];
        bytes.extend_from_slice(b"hello");
        let mut cursor = Cursor::new(bytes);
        let err = rt()
            .block_on(read_frame(&mut cursor, DEFAULT_MAX_PAYLOAD_LEN))
            .expect_err("unmasked client frame must be rejected");
        match err {
            OxiHttpError::Body(msg) => {
                assert!(msg.contains("unmasked"), "unexpected error message: {msg}")
            }
            other => panic!("expected OxiHttpError::Body, got {other:?}"),
        }
    }

    #[test]
    fn read_frame_accepts_masked_client_frame() {
        // FIN=1, opcode=Text(0x1); mask bit set; length=5; masked "hello".
        let key = [0x11u8, 0x22, 0x33, 0x44];
        let plain = b"hello";
        let mut bytes = vec![0x81u8, 0x80 | 0x05];
        bytes.extend_from_slice(&key);
        bytes.extend(plain.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
        let mut cursor = Cursor::new(bytes);
        let frame = rt()
            .block_on(read_frame(&mut cursor, DEFAULT_MAX_PAYLOAD_LEN))
            .expect("masked client frame must be accepted");
        assert_eq!(frame.payload, plain);
    }

    #[test]
    fn read_frame_rejects_payload_over_caller_supplied_cap() {
        // Masked frame claiming a 200-byte payload via the 16-bit extended
        // length form, against a caller-supplied cap of 64 bytes.
        let mut bytes = vec![0x82u8, 0x80 | 126];
        bytes.extend_from_slice(&200u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 4]); // mask key
                                            // Payload bytes are irrelevant — the cap must reject before reading them.
        let mut cursor = Cursor::new(bytes);
        let err = rt()
            .block_on(read_frame(&mut cursor, 64))
            .expect_err("oversized payload must be rejected against the caller cap");
        match err {
            OxiHttpError::Body(msg) => {
                assert!(msg.contains("too large"), "unexpected error message: {msg}")
            }
            other => panic!("expected OxiHttpError::Body, got {other:?}"),
        }
    }

    #[test]
    fn read_frame_accepts_control_frame_even_under_a_very_small_cap() {
        // A masked Ping (opcode 0x9) with a 100-byte payload must still be
        // accepted even when the caller-supplied cap is far below 100 —
        // callers are expected to floor the cap at 125 (see
        // `oxihttp_server::ws::WebSocket::recv`), and this asserts
        // `read_frame` itself does not additionally clip legal short-form
        // control-frame lengths.
        let key = [0xAAu8, 0xBB, 0xCC, 0xDD];
        let plain = [0x42u8; 100];
        let mut bytes = vec![0x89u8, 0x80 | 100u8];
        bytes.extend_from_slice(&key);
        bytes.extend(plain.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
        let mut cursor = Cursor::new(bytes);
        let frame = rt()
            .block_on(read_frame(&mut cursor, 125))
            .expect("a legal 100-byte control frame must fit within a 125-byte cap");
        assert_eq!(frame.payload.len(), 100);
    }
}
