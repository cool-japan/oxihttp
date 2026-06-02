//! Server-Sent Events (SSE) support for `oxihttp-server`.
//!
//! Provides types to build and send SSE streams from async handlers.
//!
//! # Example
//!
//! ```rust,no_run
//! use oxihttp_server::sse::{SseEvent, SseResponse};
//! use std::time::Duration;
//!
//! async fn handler() -> http::Response<oxihttp_core::Body> {
//!     let (sender, sse) = SseResponse::channel(16);
//!     tokio::spawn(async move {
//!         sender.send(SseEvent::data("hello")).await.ok();
//!         sender.send(SseEvent::data("world")).await.ok();
//!     });
//!     sse.into_response()
//! }
//! ```

#![forbid(unsafe_code)]

use std::time::Duration;

use bytes::Bytes;
use futures_util::stream::{self, StreamExt};
use http::StatusCode;
use tokio::sync::mpsc;
use tokio::time;

use oxihttp_core::{Body, OxiHttpError};

// ---------------------------------------------------------------------------
// SseEvent
// ---------------------------------------------------------------------------

/// A single Server-Sent Event.
#[derive(Debug, Clone)]
pub struct SseEvent {
    /// Optional event ID.
    pub id: Option<String>,
    /// Optional event type.
    pub event: Option<String>,
    /// The data payload.
    pub data: String,
    /// Optional reconnect timeout in milliseconds.
    pub retry: Option<u64>,
}

impl SseEvent {
    /// Create an event with the given data field.
    pub fn data(data: impl Into<String>) -> Self {
        Self {
            id: None,
            event: None,
            data: data.into(),
            retry: None,
        }
    }

    /// Set the event ID.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Set the event type.
    pub fn with_event(mut self, event: impl Into<String>) -> Self {
        self.event = Some(event.into());
        self
    }

    /// Set the reconnect timeout (in milliseconds).
    pub fn with_retry(mut self, ms: u64) -> Self {
        self.retry = Some(ms);
        self
    }

    /// Encode as SSE wire format. Multi-line `data` is split into multiple `data:` lines.
    pub fn encode(&self) -> Bytes {
        let mut buf = String::new();
        if let Some(id) = &self.id {
            buf.push_str("id: ");
            buf.push_str(id);
            buf.push('\n');
        }
        if let Some(event) = &self.event {
            buf.push_str("event: ");
            buf.push_str(event);
            buf.push('\n');
        }
        if let Some(retry) = self.retry {
            buf.push_str("retry: ");
            buf.push_str(&retry.to_string());
            buf.push('\n');
        }
        // Split multi-line data into separate data: lines
        for line in self.data.lines() {
            buf.push_str("data: ");
            buf.push_str(line);
            buf.push('\n');
        }
        if self.data.is_empty() {
            buf.push_str("data: \n");
        }
        buf.push('\n'); // blank line terminates the event
        Bytes::from(buf)
    }
}

// ---------------------------------------------------------------------------
// SseSender
// ---------------------------------------------------------------------------

/// Sender half for pushing SSE events into a live response stream.
pub struct SseSender {
    tx: mpsc::Sender<SseEvent>,
}

impl SseSender {
    /// Send an event. Returns `Err` if the receiver has been dropped (client disconnected).
    pub async fn send(&self, event: SseEvent) -> Result<(), OxiHttpError> {
        self.tx
            .send(event)
            .await
            .map_err(|_| OxiHttpError::Body("SSE channel closed".into()))
    }

    /// Non-blocking send. Returns `Err` if the channel is full or closed.
    pub fn try_send(&self, event: SseEvent) -> Result<(), OxiHttpError> {
        self.tx
            .try_send(event)
            .map_err(|_| OxiHttpError::Body("SSE channel full or closed".into()))
    }
}

// ---------------------------------------------------------------------------
// SseResponse
// ---------------------------------------------------------------------------

/// Builder for SSE responses.
pub struct SseResponse {
    rx: mpsc::Receiver<SseEvent>,
    heartbeat: Option<Duration>,
}

impl SseResponse {
    /// Create a channel and return the sender and a response builder.
    ///
    /// `buffer` is the mpsc channel capacity.
    pub fn channel(buffer: usize) -> (SseSender, Self) {
        let (tx, rx) = mpsc::channel(buffer);
        (
            SseSender { tx },
            Self {
                rx,
                heartbeat: None,
            },
        )
    }

    /// Set a heartbeat interval. The server will emit a comment (`:\n\n`) at this
    /// interval to prevent proxy/browser timeouts.
    pub fn with_heartbeat(mut self, interval: Duration) -> Self {
        self.heartbeat = Some(interval);
        self
    }

    /// Consume this builder and produce an `http::Response<Body>` suitable for returning
    /// from a server handler. The response body is a `Body::Stream` driven by the mpsc channel.
    pub fn into_response(self) -> http::Response<Body> {
        let rx = self.rx;
        let heartbeat = self.heartbeat;

        // Build a stream from the mpsc receiver.
        // Each event is encoded as Bytes; the stream ends when the sender is dropped.
        let event_stream = stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| {
                let bytes = event.encode();
                (Ok::<Bytes, OxiHttpError>(bytes), rx)
            })
        });

        let body = if let Some(interval) = heartbeat {
            // Heartbeat stream: emits `:\n\n` (SSE comment) at each tick.
            let heartbeat_stream = stream::unfold((), move |()| async move {
                time::sleep(interval).await;
                Some((Ok::<Bytes, OxiHttpError>(Bytes::from_static(b":\n\n")), ()))
            });

            // Merge events and heartbeats: pick whichever arrives first.
            // When the event stream ends (sender dropped), stop the loop.
            let (merged_tx, merged_rx) = mpsc::channel::<Bytes>(32);
            tokio::spawn(async move {
                let mut event_stream = Box::pin(event_stream);
                let mut hb_stream = Box::pin(heartbeat_stream);
                loop {
                    tokio::select! {
                        result = event_stream.next() => match result {
                            Some(Ok(bytes)) => {
                                if merged_tx.send(bytes).await.is_err() {
                                    break;
                                }
                            }
                            Some(Err(_)) | None => break,
                        },
                        Some(Ok(hb)) = hb_stream.next() => {
                            if merged_tx.send(hb).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
            let merged_stream = stream::unfold(merged_rx, |mut rx| async move {
                rx.recv().await.map(|b| (Ok::<Bytes, OxiHttpError>(b), rx))
            });
            Body::stream(Box::pin(merged_stream))
        } else {
            Body::stream(Box::pin(event_stream))
        };

        http::Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/event-stream")
            .header("Cache-Control", "no-cache")
            .header("Connection", "keep-alive")
            .header("X-Accel-Buffering", "no")
            .body(body)
            .unwrap_or_else(|_| http::Response::new(Body::Empty))
    }
}
