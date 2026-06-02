//! WebSocket frame codec (RFC 6455 §5).
//!
//! This module provides low-level frame reading and writing primitives.
//! Clients (browser→server) always mask their frames; servers never mask.
//! The codec transparently unmasks incoming masked frames.

use bytes::{BufMut, Bytes, BytesMut};
use oxihttp_core::OxiHttpError;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum payload size accepted: 64 MiB.
const MAX_PAYLOAD_LEN: u64 = 64 * 1024 * 1024;

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
    pub payload: Bytes,
}

/// Read a single WebSocket frame from the stream.
///
/// Handles client→server masking: the mask bit and masking key are read from
/// the frame header if present, and the payload is XOR-unmasked before return.
///
/// RFC 6455 constraints enforced:
/// - Reserved bits (RSV1–RSV3) must be 0 (no extensions negotiated).
/// - Control frames must have FIN=1 and payload ≤ 125 bytes.
/// - Unknown opcodes are rejected.
/// - Payload length is capped at 64 MiB.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Frame, OxiHttpError> {
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

    if payload_len > MAX_PAYLOAD_LEN {
        return Err(OxiHttpError::Body(format!(
            "WebSocket: payload too large ({payload_len} bytes, max {MAX_PAYLOAD_LEN})"
        )));
    }

    // ── Masking key (client→server) ──────────────────────────────────────────
    let mask = if masked {
        let mut key = [0u8; 4];
        reader
            .read_exact(&mut key)
            .await
            .map_err(|e| OxiHttpError::Body(format!("WebSocket: read mask key: {e}")))?;
        Some(key)
    } else {
        None
    };

    // ── Payload ──────────────────────────────────────────────────────────────
    let mut payload = vec![0u8; payload_len as usize];
    reader
        .read_exact(&mut payload)
        .await
        .map_err(|e| OxiHttpError::Body(format!("WebSocket: read payload: {e}")))?;

    // Unmask if needed (RFC 6455 §5.3).
    if let Some(key) = mask {
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= key[i % 4];
        }
    }

    Ok(Frame {
        fin,
        opcode,
        payload: Bytes::from(payload),
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
