//! Integration tests for the multipart body builder with a real HTTP round-trip.

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::Request as HyperRequest;
use hyper::Response as HyperResponse;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use oxihttp::MultipartBuilder;
use oxihttp_core::OxiHttpError;

/// Spawn an echo server that returns the raw request body unchanged.
async fn spawn_echo_server() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let _ = http1::Builder::new()
                    .serve_connection(
                        hyper_util::rt::TokioIo::new(stream),
                        service_fn(echo_handler),
                    )
                    .await;
            });
        }
    });
    addr
}

async fn echo_handler(
    req: HyperRequest<hyper::body::Incoming>,
) -> Result<HyperResponse<Full<Bytes>>, Infallible> {
    use http_body_util::BodyExt;
    let body_bytes = req
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    Ok(HyperResponse::new(Full::new(body_bytes)))
}

#[tokio::test]
async fn test_multipart_post_roundtrip() {
    let addr = spawn_echo_server().await;
    let url = format!("http://{addr}/echo");

    let client = oxihttp::Client::builder().build().expect("client build");

    let builder = MultipartBuilder::new()
        .add_text("username", "alice")
        .add_file("avatar", "avatar.png", "image/png", b"PNGDATA".as_ref());

    let content_type = builder.content_type();
    let body_bytes: Bytes = builder.build();

    let resp = client
        .post(&url)
        .expect("POST builder")
        .header("content-type", &content_type)
        .expect("content-type header")
        .body(body_bytes)
        .send()
        .await
        .expect("POST send");

    assert_eq!(resp.status(), oxihttp::StatusCode::OK);

    let returned = resp.body_bytes().await.expect("body read");
    let s = String::from_utf8(returned.to_vec()).expect("utf-8 body");

    assert!(
        s.contains("alice"),
        "response must contain text field value"
    );
    assert!(s.contains("PNGDATA"), "response must contain file body");
    assert!(s.contains("avatar.png"), "response must contain filename");
    assert!(s.contains("username"), "response must contain field name");
    // The wire body contains `Content-Type: image/png` (part header), not the outer multipart CT.
    assert!(
        s.contains("image/png"),
        "response must contain file part Content-Type"
    );
}

#[tokio::test]
async fn test_multipart_content_type_header() {
    let builder = MultipartBuilder::new();
    let ct = builder.content_type();
    let bnd = builder.boundary().to_owned();

    assert!(ct.starts_with("multipart/form-data; boundary="));
    assert!(ct.contains(&bnd));
}

#[tokio::test]
async fn test_multipart_empty_body_round_trip() {
    let addr = spawn_echo_server().await;
    let url = format!("http://{addr}/echo");

    let client = oxihttp::Client::builder().build().expect("client build");

    let builder = MultipartBuilder::new();
    let content_type = builder.content_type();
    let body_bytes: Bytes = builder.build();

    let resp = client
        .post(&url)
        .expect("POST builder")
        .header("content-type", &content_type)
        .expect("content-type header")
        .body(body_bytes)
        .send()
        .await
        .expect("POST send");

    assert_eq!(resp.status(), oxihttp::StatusCode::OK);
    let returned = resp.body_bytes().await.expect("body read");
    let s = String::from_utf8(returned.to_vec()).expect("utf-8");
    // Even empty builder produces a final boundary line.
    assert!(s.ends_with("--\r\n"));
}

#[tokio::test]
async fn test_multipart_multiple_fields() {
    let addr = spawn_echo_server().await;
    let url = format!("http://{addr}/echo");

    let client = oxihttp::Client::builder().build().expect("client build");

    let builder = MultipartBuilder::new()
        .add_text("first", "value1")
        .add_text("second", "value2")
        .add_text("third", "value3");

    let content_type = builder.content_type();
    let body_bytes: Bytes = builder.build();

    let resp = client
        .post(&url)
        .expect("POST builder")
        .header("content-type", &content_type)
        .expect("content-type header")
        .body(body_bytes)
        .send()
        .await
        .expect("POST send");

    assert_eq!(resp.status(), oxihttp::StatusCode::OK);
    let returned = resp.body_bytes().await.expect("body read");
    let s = String::from_utf8(returned.to_vec()).expect("utf-8");

    assert!(s.contains("value1"));
    assert!(s.contains("value2"));
    assert!(s.contains("value3"));
}

// ---------------------------------------------------------------------------
// Streaming multipart (`StreamingMultipart::build_stream`) — zero-copy
// large-upload regression coverage.
// ---------------------------------------------------------------------------

/// Read a [`tokio::fs::File`] in fixed-size chunks as a
/// `Stream<Item = Result<Bytes, OxiHttpError>>`, never holding more than one
/// chunk in memory at a time — the realistic shape of a caller streaming a
/// large file into [`MultipartBuilder::add_file_stream`] without
/// materializing the whole thing up front.
fn tokio_file_chunk_stream(
    file: tokio::fs::File,
    chunk_size: usize,
) -> impl futures_util::Stream<Item = Result<Bytes, OxiHttpError>> + Send + 'static {
    futures_util::stream::unfold(Some(file), move |state| async move {
        use tokio::io::AsyncReadExt;
        let mut file = state?;
        let mut buf = vec![0u8; chunk_size];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(Bytes::from(buf)), Some(file)))
            }
            Err(e) => Some((Err(OxiHttpError::Io(std::sync::Arc::new(e))), None)),
        }
    })
}

/// Drain a `oxihttp_core::Body` (of any variant) into `Bytes` via the public
/// `http_body::Body` interface — the same interface hyper itself drives.
async fn collect_core_body(body: oxihttp_core::Body) -> Result<Bytes, OxiHttpError> {
    use hyper::body::Body as _;
    let mut pinned = Box::pin(body.into_pinned());
    let mut out = Vec::new();
    loop {
        let frame = std::future::poll_fn(|cx| pinned.as_mut().poll_frame(cx)).await;
        match frame {
            None => return Ok(Bytes::from(out)),
            Some(Err(e)) => return Err(e),
            Some(Ok(f)) => {
                if let Ok(data) = f.into_data() {
                    out.extend_from_slice(&data);
                }
            }
        }
    }
}

/// End-to-end regression test for the "multipart streaming (zero-copy large
/// uploads)" gap: build a multi-megabyte temp file on disk, stream it into a
/// multipart part via [`MultipartBuilder::add_file_stream`] and a real
/// `tokio::fs::File` read in 64 KiB chunks (never the whole file at once),
/// collect the resulting [`oxihttp_core::Body::Stream`], and verify the
/// collected bytes reproduce the source file exactly — then send the
/// collected body through a real HTTP round-trip to confirm the wire format
/// a receiving server sees is unaffected by having been produced
/// incrementally rather than by `MultipartBuilder::build()`'s single
/// concatenated buffer.
#[tokio::test]
async fn test_multipart_stream_from_real_temp_file() {
    // A few MiB — large enough that "read the whole file up front" and
    // "read it in 64 KiB chunks" would behave differently if the streaming
    // path secretly buffered everything before emitting the first chunk.
    const FILE_LEN: usize = 3 * 1024 * 1024 + 777; // deliberately not a multiple of the chunk size
    let mut file_content = Vec::with_capacity(FILE_LEN);
    for i in 0..FILE_LEN {
        file_content.push((i % 251) as u8);
    }

    let temp_path = std::env::temp_dir().join(format!(
        "oxihttp_multipart_stream_test_{}_{}.bin",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos()
    ));
    tokio::fs::write(&temp_path, &file_content)
        .await
        .expect("write temp file");

    // Guard so the temp file is removed even if an assertion below panics.
    struct TempFileGuard(std::path::PathBuf);
    impl Drop for TempFileGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _guard = TempFileGuard(temp_path.clone());

    let file = tokio::fs::File::open(&temp_path)
        .await
        .expect("open temp file");
    let chunk_stream = tokio_file_chunk_stream(file, 64 * 1024);

    let streaming = MultipartBuilder::new()
        .add_text("description", "large upload")
        .add_file_stream(
            "payload",
            "data.bin",
            "application/octet-stream",
            chunk_stream,
        );

    let content_type = streaming.content_type();
    let body: oxihttp_core::Body = streaming.build_stream();
    let collected = collect_core_body(body).await.expect("collect stream body");

    // The exact source-file bytes must appear intact, verbatim, somewhere
    // in the collected multipart body.
    let file_pos = find_subslice(&collected, &file_content)
        .expect("streamed file content must appear byte-for-byte in the collected body");
    assert_eq!(
        &collected[file_pos..file_pos + FILE_LEN],
        file_content.as_slice()
    );

    let s = String::from_utf8_lossy(&collected);
    assert!(s.contains("name=\"description\""));
    assert!(s.contains("large upload"));
    assert!(s.contains("filename=\"data.bin\""));
    assert!(s.contains("Content-Type: application/octet-stream"));
    assert!(s.ends_with("--\r\n"));

    // Send the collected body through a real server round-trip — proves the
    // wire format a receiving server sees is unaffected by having been
    // produced incrementally.
    let addr = spawn_echo_server().await;
    let client = oxihttp::Client::builder().build().expect("client build");
    let resp = client
        .post(&format!("http://{addr}/echo"))
        .expect("POST builder")
        .header("content-type", &content_type)
        .expect("content-type header")
        .body(collected)
        .send()
        .await
        .expect("POST send");
    assert_eq!(resp.status(), oxihttp::StatusCode::OK);
    let echoed = resp.body_bytes().await.expect("body read");
    let echoed_pos = find_subslice(&echoed, &file_content).expect("echoed body must round-trip");
    assert_eq!(
        &echoed[echoed_pos..echoed_pos + FILE_LEN],
        file_content.as_slice()
    );
}

/// Find the first occurrence of `needle` within `haystack`, if any.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A streamed part combined with in-memory parts before and after it must
/// keep every part in insertion order end-to-end, matching the equivalent
/// all-in-memory `build()` layout.
#[tokio::test]
async fn test_multipart_stream_preserves_part_order() {
    let chunk_stream = futures_util::stream::iter([Ok::<_, OxiHttpError>(Bytes::from_static(
        b"streamed-file-content",
    ))]);

    let streaming = MultipartBuilder::new()
        .add_text("first", "value1")
        .add_stream_part(
            vec![(
                "Content-Disposition".into(),
                "form-data; name=\"middle\"".into(),
            )],
            chunk_stream,
        )
        .add_text("last", "value3");

    let body: oxihttp_core::Body = streaming.build_stream();
    let collected = collect_core_body(body).await.expect("collect stream body");
    let s = String::from_utf8(collected.to_vec()).expect("utf8");

    let first_pos = s.find("name=\"first\"").expect("first part");
    let middle_pos = s.find("name=\"middle\"").expect("middle (streamed) part");
    let last_pos = s.find("name=\"last\"").expect("last part");
    assert!(first_pos < middle_pos && middle_pos < last_pos);
    assert!(s.contains("streamed-file-content"));
    assert!(s.contains("value1"));
    assert!(s.contains("value3"));
}

// ---------------------------------------------------------------------------
// Client wire streaming (`RequestBuilder::multipart_stream`) — proves a
// multipart file part streams to the wire through `oxihttp_client::Client`
// with bounded memory, rather than being materialized in full before the
// request is sent. This is the client-side counterpart to
// `test_multipart_stream_from_real_temp_file` above, which only exercises
// `StreamingMultipart::build_stream()` in isolation and then falls back to
// the buffered `.body()` for the actual send.
// ---------------------------------------------------------------------------

/// A `Bytes`-owning wrapper whose `Drop` decrements a shared "bytes
/// currently alive" counter — a peak-memory proxy. As long as the client
/// streams each chunk to the wire as it's produced (rather than
/// materializing the whole request body before sending), only a small,
/// bounded number of chunks should ever be alive simultaneously, however
/// large the total payload is.
struct TrackedChunk {
    data: Vec<u8>,
    outstanding: Arc<AtomicUsize>,
}

impl AsRef<[u8]> for TrackedChunk {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

impl Drop for TrackedChunk {
    fn drop(&mut self) {
        self.outstanding
            .fetch_sub(self.data.len(), Ordering::SeqCst);
    }
}

/// Build a `Stream` over `chunks` that (a) tracks `outstanding` — bytes
/// produced by this stream but not yet dropped by whatever holds them
/// further down the pipeline — recording the running maximum into
/// `peak_outstanding` (the peak-memory proxy), and (b) after the very
/// first chunk, will not produce the next one until `gate_rx` yields a
/// signal.
///
/// The gate is what turns "the client is *permitted* to stream" into "the
/// client *must* stream to make any progress at all": the signal is only
/// sent by the receiving server (see `spawn_gated_echo_router`) after it
/// has actually read the *previous* chunk off the wire. If the client
/// buffered the whole part before sending anything, chunk 2 would be
/// requested from this stream before the server could possibly have
/// acknowledged chunk 1 — the `.await` on the gate then never resolves,
/// deadlocking the send, which the test catches with `tokio::time::timeout`
/// rather than hanging forever.
fn gated_tracked_chunk_stream(
    chunks: VecDeque<Vec<u8>>,
    gate_rx: tokio::sync::mpsc::UnboundedReceiver<()>,
    outstanding: Arc<AtomicUsize>,
    peak_outstanding: Arc<AtomicUsize>,
) -> impl futures_util::Stream<Item = Result<Bytes, OxiHttpError>> + Send + 'static {
    let state = (chunks, gate_rx, outstanding, peak_outstanding, true);
    futures_util::stream::unfold(
        state,
        |(mut chunks, mut gate_rx, outstanding, peak_outstanding, is_first)| async move {
            if !is_first {
                gate_rx.recv().await;
            }
            let data = chunks.pop_front()?;
            let len = data.len();
            let now = outstanding.fetch_add(len, Ordering::SeqCst) + len;
            peak_outstanding.fetch_max(now, Ordering::SeqCst);
            let bytes = Bytes::from_owner(TrackedChunk {
                data,
                outstanding: outstanding.clone(),
            });
            Some((
                Ok(bytes),
                (chunks, gate_rx, outstanding, peak_outstanding, false),
            ))
        },
    )
}

/// Spawn a server built on this crate's own `oxihttp_server::Router` /
/// `Server` (not a raw hyper server, unlike `spawn_echo_server` above) that
/// reads its request body incrementally — frame by frame, via the raw
/// `hyper::body::Incoming` — and echoes it back once fully received.
/// `on_frame` fires once per body frame actually read off the wire; the
/// caller uses this as the "the server really did see this data" signal
/// that gates `gated_tracked_chunk_stream`.
async fn spawn_gated_echo_router(
    on_frame: tokio::sync::mpsc::UnboundedSender<()>,
) -> (SocketAddr, tokio::sync::oneshot::Sender<()>) {
    use http_body_util::BodyExt;

    let router = oxihttp_server::Router::new().post("/upload", move |req| {
        let on_frame = on_frame.clone();
        async move {
            let mut incoming = req.into_inner().into_body();
            let mut collected: Vec<u8> = Vec::new();
            loop {
                match incoming.frame().await {
                    Some(Ok(frame)) => {
                        if let Ok(data) = frame.into_data() {
                            collected.extend_from_slice(&data);
                        }
                        // Best-effort: if the receiver side has already
                        // been dropped (shouldn't happen — it outlives the
                        // request), there's nothing more to signal.
                        let _ = on_frame.send(());
                    }
                    Some(Err(e)) => return Err(OxiHttpError::Body(e.to_string())),
                    None => break,
                }
            }
            Ok(HyperResponse::new(Full::new(Bytes::from(collected))))
        }
    });

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let (addr, _handle) = oxihttp_server::Server::bind("127.0.0.1:0")
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        })
        .serve_with_addr(router)
        .await
        .expect("server bind");
    // Give the server a moment to start accepting, matching the pattern in
    // `crates/oxihttp/tests/server_test.rs::spawn_test_server`.
    tokio::time::sleep(Duration::from_millis(10)).await;
    (addr, shutdown_tx)
}

/// `RequestBuilder::multipart_stream` must stream a large file part to the
/// wire through `oxihttp_client::Client` with bounded memory, and the
/// result must round-trip byte-for-byte through a real server.
///
/// Two independent, complementary proofs:
///
/// 1. **Categorical (deadlock-or-not)**: the synthetic part is produced as
///    a sequence of chunks gated one-at-a-time on the crate's own server
///    actually having read the previous chunk off the wire (see
///    `gated_tracked_chunk_stream` / `spawn_gated_echo_router`). A client
///    that buffered the whole part before sending anything would deadlock
///    requesting the second chunk — caught by the outer
///    `tokio::time::timeout` as a hard pass/fail, not a tuned threshold.
/// 2. **Quantitative (peak-memory proxy)**: every chunk is wrapped in a
///    `Bytes::from_owner` (`TrackedChunk`) whose `Drop` decrements a
///    shared "bytes currently alive" counter, whose running maximum is
///    recorded. Because the gate above forces the client to fully hand
///    off (and, transitively, finish sending) one chunk before the next
///    one even exists, a correctly streaming implementation keeps at most
///    a couple of chunks alive at once; a regression that re-buffers the
///    whole body first would keep every chunk alive simultaneously,
///    spiking the peak far past the generous threshold asserted below.
#[tokio::test]
async fn test_multipart_stream_client_wire_bounded_memory() {
    const CHUNK_SIZE: usize = 256 * 1024; // 256 KiB
    const NUM_CHUNKS: usize = 32;
    const TOTAL_SIZE: usize = CHUNK_SIZE * NUM_CHUNKS; // 8 MiB — "large synthetic part"

    let mut chunks: VecDeque<Vec<u8>> = VecDeque::with_capacity(NUM_CHUNKS);
    let mut full_payload = Vec::with_capacity(TOTAL_SIZE);
    for i in 0..NUM_CHUNKS {
        // A deterministic pattern that varies across chunks (not just
        // within one), so a corrupted or reordered wire format is
        // detectable rather than merely a length mismatch.
        let chunk: Vec<u8> = (0..CHUNK_SIZE)
            .map(|b| ((i * 37 + b) % 251) as u8)
            .collect();
        full_payload.extend_from_slice(&chunk);
        chunks.push_back(chunk);
    }

    let outstanding = Arc::new(AtomicUsize::new(0));
    let peak_outstanding = Arc::new(AtomicUsize::new(0));
    let (gate_tx, gate_rx) = tokio::sync::mpsc::unbounded_channel::<()>();

    let (addr, _shutdown) = spawn_gated_echo_router(gate_tx).await;

    let chunk_stream = gated_tracked_chunk_stream(
        chunks,
        gate_rx,
        outstanding.clone(),
        peak_outstanding.clone(),
    );

    let streaming = MultipartBuilder::new()
        .add_text("description", "large gated upload")
        .add_file_stream(
            "payload",
            "large.bin",
            "application/octet-stream",
            chunk_stream,
        );

    let client = oxihttp::Client::builder().build().expect("client build");
    let send_fut = client
        .post(&format!("http://{addr}/upload"))
        .expect("POST builder")
        .multipart_stream(streaming)
        .send();

    // A generous but finite budget: a correctly streaming implementation
    // finishes in well under a second on loopback. A client that
    // regressed to full buffering deadlocks on the gate (see
    // `gated_tracked_chunk_stream`'s doc comment) and never completes at
    // all, so this timeout is a hard pass/fail, not a tuned performance
    // threshold.
    let resp = tokio::time::timeout(Duration::from_secs(20), send_fut)
        .await
        .expect(
            "client must not deadlock waiting to stream a later chunk — the next chunk was \
             requested before the server could have acknowledged the previous one, meaning \
             the multipart body was buffered rather than streamed",
        )
        .expect("POST send");

    assert_eq!(resp.status(), oxihttp::StatusCode::OK);

    let peak = peak_outstanding.load(Ordering::SeqCst);
    // Streaming (gated one chunk at a time): peak stays near one chunk.
    // Fully buffered: every chunk alive at once, peak == TOTAL_SIZE. The
    // threshold sits far below the buffered case and comfortably above any
    // reasonable transient overlap between "chunk N handed to the OS
    // socket" and "chunk N's local buffer actually dropped".
    assert!(
        peak <= CHUNK_SIZE * 4,
        "peak outstanding bytes {peak} exceeds {} (4 chunks) — the multipart part was not \
         streamed with bounded memory (fully-buffered would reach {TOTAL_SIZE})",
        CHUNK_SIZE * 4,
    );
    assert!(
        peak < TOTAL_SIZE,
        "peak outstanding bytes {peak} reached the full payload size {TOTAL_SIZE} — the body \
         was buffered in full rather than streamed"
    );

    let echoed = resp.body_bytes().await.expect("body read");
    let file_pos = find_subslice(&echoed, &full_payload)
        .expect("streamed file content must appear byte-for-byte in the echoed body");
    assert_eq!(
        &echoed[file_pos..file_pos + TOTAL_SIZE],
        full_payload.as_slice()
    );
    let s = String::from_utf8_lossy(&echoed);
    assert!(s.contains("name=\"description\""));
    assert!(s.contains("large gated upload"));
    assert!(s.contains("filename=\"large.bin\""));
    assert!(s.contains("Content-Type: application/octet-stream"));
}

// ---------------------------------------------------------------------------
// `multipart_stream`'s one-shot-body contract with redirects and retries
// (documented in `RequestBuilder::multipart_stream`'s "Trade-offs of a
// one-shot body" section, oxihttp-client/src/lib.rs). Three promises made
// there, each proven independently:
//
//   1. A body-preserving redirect (307/308) on a one-shot body fails with a
//      typed `OxiHttpError::Body`, and the redirect target is never reached
//      (the body cannot be resent, so the client must not even try).
//   2. A body-dropping redirect (301/302/303) succeeds normally: the body
//      is not needed again, so there is nothing the one-shot restriction
//      prevents.
//   3. The client's `RetryPolicy` is bypassed entirely for a one-shot body:
//      exactly one attempt is made regardless of the configured policy.
// ---------------------------------------------------------------------------

/// Spawn a server that, on POST to `/original`, fully drains the request
/// body (so the streaming send always completes cleanly regardless of which
/// redirect status is under test) then responds with `redirect_status` and
/// `Location: /destination`; on any method to `/destination`, responds 200
/// "destination-reached". Both paths' call counts are tracked so a test can
/// assert whether `/destination` was ever actually reached.
async fn spawn_redirect_server(
    redirect_status: u16,
) -> (SocketAddr, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let original_hits = Arc::new(AtomicUsize::new(0));
    let destination_hits = Arc::new(AtomicUsize::new(0));
    let original_hits_srv = original_hits.clone();
    let destination_hits_srv = destination_hits.clone();

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let original_hits = original_hits_srv.clone();
            let destination_hits = destination_hits_srv.clone();
            tokio::spawn(async move {
                let _ = http1::Builder::new()
                    .serve_connection(
                        hyper_util::rt::TokioIo::new(stream),
                        service_fn(move |req: HyperRequest<hyper::body::Incoming>| {
                            let original_hits = original_hits.clone();
                            let destination_hits = destination_hits.clone();
                            async move {
                                use http_body_util::BodyExt;
                                let path = req.uri().path().to_string();
                                match path.as_str() {
                                    "/original" => {
                                        original_hits.fetch_add(1, Ordering::SeqCst);
                                        let _ = req.into_body().collect().await;
                                        Ok::<_, Infallible>(
                                            HyperResponse::builder()
                                                .status(redirect_status)
                                                .header("location", "/destination")
                                                .body(Full::new(Bytes::new()))
                                                .expect("resp build"),
                                        )
                                    }
                                    "/destination" => {
                                        destination_hits.fetch_add(1, Ordering::SeqCst);
                                        let _ = req.into_body().collect().await;
                                        Ok(HyperResponse::new(Full::new(Bytes::from(
                                            "destination-reached",
                                        ))))
                                    }
                                    _ => {
                                        let mut r =
                                            HyperResponse::new(Full::new(Bytes::from("not found")));
                                        *r.status_mut() = hyper::StatusCode::NOT_FOUND;
                                        Ok(r)
                                    }
                                }
                            }
                        }),
                    )
                    .await;
            });
        }
    });
    (addr, original_hits, destination_hits)
}

/// Build a trivial one-part `StreamingMultipart` — enough to exercise the
/// one-shot-body contract without needing a real large payload.
fn one_shot_streaming_part() -> oxihttp_core::StreamingMultipart {
    let chunk_stream =
        futures_util::stream::iter([Ok::<_, OxiHttpError>(Bytes::from_static(b"one-shot-chunk"))]);
    MultipartBuilder::new().add_file_stream(
        "payload",
        "data.bin",
        "application/octet-stream",
        chunk_stream,
    )
}

/// Promise 1: a 307 (body-preserving) redirect on a `multipart_stream`
/// request must fail with a typed `OxiHttpError::Body` — not silently drop,
/// truncate, or resend a corrupted body — and must never even attempt the
/// second request, since the one-shot body cannot be produced twice.
#[tokio::test]
async fn test_multipart_stream_307_redirect_returns_typed_error_body_not_resent() {
    let (addr, original_hits, destination_hits) = spawn_redirect_server(307).await;

    let client = oxihttp::Client::builder()
        .redirect_policy(oxihttp::RedirectPolicy::Limited(5))
        .build()
        .expect("client build");

    let result = client
        .post(&format!("http://{addr}/original"))
        .expect("POST builder")
        .multipart_stream(one_shot_streaming_part())
        .send()
        .await;

    let err = result.expect_err(
        "a 307 redirect on a one-shot multipart_stream body must fail rather than silently \
         dropping or corrupting the body",
    );
    assert!(err.is_body(), "expected OxiHttpError::Body, got: {err:?}");
    assert!(
        err.to_string().contains("cannot be sent again"),
        "error message should explain the one-shot body limitation, got: {err}"
    );

    assert_eq!(
        original_hits.load(Ordering::SeqCst),
        1,
        "the first hop must still have been sent exactly once"
    );
    assert_eq!(
        destination_hits.load(Ordering::SeqCst),
        0,
        "the redirect target must never be reached — the client must fail before attempting \
         to resend the one-shot body"
    );
}

/// Promise 1 (308 variant): 308 Permanent Redirect has the same
/// body-preserving contract as 307 and must behave identically.
#[tokio::test]
async fn test_multipart_stream_308_redirect_returns_typed_error_body_not_resent() {
    let (addr, original_hits, destination_hits) = spawn_redirect_server(308).await;

    let client = oxihttp::Client::builder()
        .redirect_policy(oxihttp::RedirectPolicy::Limited(5))
        .build()
        .expect("client build");

    let result = client
        .post(&format!("http://{addr}/original"))
        .expect("POST builder")
        .multipart_stream(one_shot_streaming_part())
        .send()
        .await;

    let err = result.expect_err("a 308 redirect on a one-shot body must fail, not resend it");
    assert!(err.is_body(), "expected OxiHttpError::Body, got: {err:?}");
    assert_eq!(original_hits.load(Ordering::SeqCst), 1);
    assert_eq!(destination_hits.load(Ordering::SeqCst), 0);
}

/// Promise 2: a 301 (body-dropping) redirect on a `multipart_stream`
/// request must succeed normally — the one-shot restriction only bites when
/// the body would need to be sent a second time, and a body-dropping
/// redirect never needs that.
#[tokio::test]
async fn test_multipart_stream_301_redirect_drops_body_and_succeeds() {
    let (addr, original_hits, destination_hits) = spawn_redirect_server(301).await;

    let client = oxihttp::Client::builder()
        .redirect_policy(oxihttp::RedirectPolicy::Limited(5))
        .build()
        .expect("client build");

    let resp = client
        .post(&format!("http://{addr}/original"))
        .expect("POST builder")
        .multipart_stream(one_shot_streaming_part())
        .send()
        .await
        .expect("a 301 redirect must drop the one-shot body and succeed, not error");

    assert_eq!(resp.status(), oxihttp::StatusCode::OK);
    let body = resp.body_bytes().await.expect("body read");
    assert_eq!(&body[..], b"destination-reached");

    assert_eq!(original_hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        destination_hits.load(Ordering::SeqCst),
        1,
        "the redirect target must be reached exactly once after the body-dropping redirect"
    );
}

/// Spawn a server that always returns 503, tracking how many times it was
/// called — used to prove a `multipart_stream` request is never retried.
async fn spawn_always_503_server() -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let call_count = Arc::new(AtomicUsize::new(0));
    let cc = call_count.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let cc = cc.clone();
            tokio::spawn(async move {
                let _ = http1::Builder::new()
                    .serve_connection(
                        hyper_util::rt::TokioIo::new(stream),
                        service_fn(move |req: HyperRequest<hyper::body::Incoming>| {
                            let cc = cc.clone();
                            async move {
                                use http_body_util::BodyExt;
                                cc.fetch_add(1, Ordering::SeqCst);
                                let _ = req.into_body().collect().await;
                                Ok::<_, Infallible>(
                                    HyperResponse::builder()
                                        .status(503)
                                        .body(Full::new(Bytes::from("unavailable")))
                                        .expect("resp build"),
                                )
                            }
                        }),
                    )
                    .await;
            });
        }
    });
    (addr, call_count)
}

/// Promise 3: a `RetryPolicy` configured on the client must be bypassed
/// entirely for a `multipart_stream` request — exactly one attempt,
/// whatever it returns — because the one-shot body cannot be resent for a
/// retry any more than it can for a redirect.
#[tokio::test]
async fn test_multipart_stream_bypasses_retry_policy() {
    let (addr, call_count) = spawn_always_503_server().await;

    let policy = oxihttp::RetryPolicy::new(3).with_backoff_base(Duration::from_millis(5));
    let client = oxihttp::Client::builder()
        .retry_policy(policy)
        .build()
        .expect("client build");

    let resp = client
        .post(&format!("http://{addr}/upload"))
        .expect("POST builder")
        .multipart_stream(one_shot_streaming_part())
        .send()
        .await
        .expect("send");

    // A one-shot streaming body cannot be resent, so the retry policy must
    // be bypassed entirely: exactly one attempt, whatever it returns —
    // contrast with `retry_test.rs::test_retry_exhausted_returns_last_response`,
    // which proves the same policy against a non-streaming body *does*
    // retry up to `max_retries + 1` times.
    assert_eq!(resp.status().as_u16(), 503);
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "a multipart_stream request must never be retried — the one-shot body cannot be resent"
    );
}
