//! WebSocket integration tests (RFC 6455).
//!
//! These tests use raw TCP connections that manually perform the HTTP→WebSocket
//! upgrade handshake, then exchange frames directly.  This keeps the test
//! infrastructure self-contained — no external WS client library required.

#[cfg(all(feature = "server", feature = "websocket"))]
mod ws_tests {
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use oxihttp_server::{ws, Message, Router, Server};

    // -----------------------------------------------------------------------
    // Helper: spawn a test server and return its bound address + shutdown tx.
    // -----------------------------------------------------------------------

    async fn spawn_ws_server(
        router: Router,
    ) -> (std::net::SocketAddr, tokio::sync::oneshot::Sender<()>) {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let (addr, _handle) = Server::bind("127.0.0.1:0")
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .serve_with_addr(router)
            .await
            .expect("server bind");
        tokio::time::sleep(Duration::from_millis(20)).await;
        (addr, tx)
    }

    // -----------------------------------------------------------------------
    // Helper: perform the HTTP→WebSocket upgrade handshake over a raw TCP
    // stream, returning the stream positioned at the first WebSocket frame.
    // -----------------------------------------------------------------------

    async fn ws_connect(addr: std::net::SocketAddr, path: &str) -> TcpStream {
        let mut stream = TcpStream::connect(addr).await.expect("TCP connect");

        // Fixed base64-encoded nonce (RFC 6455 example key).
        let ws_key = "dGhlIHNhbXBsZSBub25jZQ==";

        let request = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: {addr}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: {ws_key}\r\n\
             Sec-WebSocket-Version: 13\r\n\
             \r\n"
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write upgrade request");

        // Read response until we see the blank line terminating headers.
        let mut response_buf = Vec::with_capacity(512);
        loop {
            let mut byte = [0u8; 1];
            stream
                .read_exact(&mut byte)
                .await
                .expect("read response byte");
            response_buf.push(byte[0]);
            if response_buf.ends_with(b"\r\n\r\n") {
                break;
            }
            // Safety guard: don't read more than 8 KiB.
            if response_buf.len() > 8192 {
                panic!("response headers too large");
            }
        }

        let response_str = String::from_utf8_lossy(&response_buf);
        assert!(
            response_str.starts_with("HTTP/1.1 101"),
            "expected 101 Switching Protocols, got: {response_str}"
        );

        stream
    }

    // -----------------------------------------------------------------------
    // Helper: write a client→server masked WebSocket frame over raw TCP.
    // -----------------------------------------------------------------------

    async fn write_masked_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8], fin: bool) {
        let mut frame = Vec::with_capacity(payload.len() + 10);

        // First byte: FIN + opcode.
        frame.push(if fin { 0x80 | opcode } else { opcode });

        // Second byte: mask-bit + length.
        let len = payload.len();
        if len <= 125 {
            frame.push(0x80 | len as u8);
        } else if len <= 0xFFFF {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }

        // Masking key (deterministic for tests).
        let mask: [u8; 4] = [0x37, 0xfa, 0x21, 0x3d];
        frame.extend_from_slice(&mask);

        // Masked payload.
        for (i, &b) in payload.iter().enumerate() {
            frame.push(b ^ mask[i % 4]);
        }

        stream.write_all(&frame).await.expect("write masked frame");
        stream.flush().await.expect("flush");
    }

    // -----------------------------------------------------------------------
    // Helper: read one server→client (unmasked) WebSocket frame.
    // Returns (opcode, fin, payload).
    // -----------------------------------------------------------------------

    async fn read_server_frame(stream: &mut TcpStream) -> (u8, bool, Vec<u8>) {
        let mut header = [0u8; 2];
        stream
            .read_exact(&mut header)
            .await
            .expect("read frame header");

        let fin = (header[0] & 0x80) != 0;
        let opcode = header[0] & 0x0F;
        let len_byte = (header[1] & 0x7F) as usize;
        // Server frames must not be masked (RFC 6455 §5.1).
        assert_eq!(header[1] & 0x80, 0, "server frame must not be masked");

        let payload_len: usize = match len_byte {
            0..=125 => len_byte,
            126 => {
                let mut b = [0u8; 2];
                stream.read_exact(&mut b).await.expect("read ext len16");
                u16::from_be_bytes(b) as usize
            }
            127 => {
                let mut b = [0u8; 8];
                stream.read_exact(&mut b).await.expect("read ext len64");
                u64::from_be_bytes(b) as usize
            }
            _ => unreachable!(),
        };

        let mut payload = vec![0u8; payload_len];
        stream.read_exact(&mut payload).await.expect("read payload");

        (opcode, fin, payload)
    }

    // -----------------------------------------------------------------------
    // Test 1: Text echo — send a Text message, receive Text echo.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_websocket_text_echo() {
        let router = Router::new().get("/ws", |req| async move {
            let (upgrade, resp) = ws::upgrade(req)?;
            tokio::spawn(async move {
                if let Ok(mut socket) = upgrade.accept().await {
                    // Echo loop.
                    while let Ok(Some(msg)) = socket.recv().await {
                        match msg {
                            Message::Close(_) => break,
                            other => {
                                if socket.send(other).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
            });
            Ok(resp)
        });

        let (addr, _shutdown) = spawn_ws_server(router).await;
        let mut stream = ws_connect(addr, "/ws").await;

        // Send Text("hello")
        write_masked_frame(&mut stream, 0x1, b"hello", true).await;

        // Receive Text echo
        let (opcode, fin, payload) = read_server_frame(&mut stream).await;
        assert_eq!(opcode, 0x1, "expected Text opcode");
        assert!(fin, "expected FIN");
        assert_eq!(&payload, b"hello");

        // Send Close
        write_masked_frame(&mut stream, 0x8, &[0x03, 0xe8], true).await;
        // Receive Close echo
        let (opcode, _fin, _payload) = read_server_frame(&mut stream).await;
        assert_eq!(opcode, 0x8, "expected Close opcode");
    }

    // -----------------------------------------------------------------------
    // Test 2: Binary echo.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_websocket_binary_echo() {
        let router = Router::new().get("/ws-bin", |req| async move {
            let (upgrade, resp) = ws::upgrade(req)?;
            tokio::spawn(async move {
                if let Ok(mut socket) = upgrade.accept().await {
                    while let Ok(Some(msg)) = socket.recv().await {
                        match msg {
                            Message::Close(_) => break,
                            other => {
                                if socket.send(other).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
            });
            Ok(resp)
        });

        let (addr, _shutdown) = spawn_ws_server(router).await;
        let mut stream = ws_connect(addr, "/ws-bin").await;

        // Send Binary([1, 2, 3, 4])
        write_masked_frame(&mut stream, 0x2, &[0x01, 0x02, 0x03, 0x04], true).await;

        // Receive Binary echo
        let (opcode, fin, payload) = read_server_frame(&mut stream).await;
        assert_eq!(opcode, 0x2, "expected Binary opcode");
        assert!(fin, "expected FIN");
        assert_eq!(payload, vec![0x01, 0x02, 0x03, 0x04]);

        // Clean close
        write_masked_frame(&mut stream, 0x8, &[0x03, 0xe8], true).await;
        let (opcode, _fin, _payload) = read_server_frame(&mut stream).await;
        assert_eq!(opcode, 0x8, "expected Close opcode");
    }

    // -----------------------------------------------------------------------
    // Test 3: Automatic Ping/Pong — send Ping, expect auto-Pong.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_websocket_ping_pong() {
        let router = Router::new().get("/ws-ping", |req| async move {
            let (upgrade, resp) = ws::upgrade(req)?;
            tokio::spawn(async move {
                if let Ok(mut socket) = upgrade.accept().await {
                    // Just drain; Ping handling is automatic.
                    while let Ok(Some(msg)) = socket.recv().await {
                        if matches!(msg, Message::Close(_)) {
                            break;
                        }
                    }
                }
            });
            Ok(resp)
        });

        let (addr, _shutdown) = spawn_ws_server(router).await;
        let mut stream = ws_connect(addr, "/ws-ping").await;

        // Send Ping with payload "ping-data"
        write_masked_frame(&mut stream, 0x9, b"ping-data", true).await;

        // Server should auto-reply with Pong carrying the same payload.
        let (opcode, fin, payload) = read_server_frame(&mut stream).await;
        assert_eq!(opcode, 0xA, "expected Pong opcode (0xA)");
        assert!(fin, "Pong must have FIN=1");
        assert_eq!(
            &payload, b"ping-data",
            "Pong payload must mirror Ping payload"
        );

        // Clean close
        write_masked_frame(&mut stream, 0x8, &[0x03, 0xe8], true).await;
        let (opcode, _fin, _payload) = read_server_frame(&mut stream).await;
        assert_eq!(opcode, 0x8);
    }

    // -----------------------------------------------------------------------
    // Test 4: Fragment reassembly — send 3 fragments, receive one echoed message.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_websocket_fragmented_message() {
        let router = Router::new().get("/ws-frag", |req| async move {
            let (upgrade, resp) = ws::upgrade(req)?;
            tokio::spawn(async move {
                if let Ok(mut socket) = upgrade.accept().await {
                    while let Ok(Some(msg)) = socket.recv().await {
                        match msg {
                            Message::Close(_) => break,
                            other => {
                                if socket.send(other).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
            });
            Ok(resp)
        });

        let (addr, _shutdown) = spawn_ws_server(router).await;
        let mut stream = ws_connect(addr, "/ws-frag").await;

        // Send 3 fragments: Text (fin=false) + Continuation (fin=false) + Continuation (fin=true).
        write_masked_frame(&mut stream, 0x1, b"hello", false).await;
        write_masked_frame(&mut stream, 0x0, b", ", false).await;
        write_masked_frame(&mut stream, 0x0, b"world", true).await;

        // The server should echo the reassembled "hello, world" as a single Text frame.
        let (opcode, fin, payload) = read_server_frame(&mut stream).await;
        assert_eq!(opcode, 0x1, "expected Text opcode for reassembled message");
        assert!(fin, "reassembled message must have FIN=1");
        assert_eq!(&payload, b"hello, world");

        // Clean close
        write_masked_frame(&mut stream, 0x8, &[0x03, 0xe8], true).await;
        let (opcode, _fin, _payload) = read_server_frame(&mut stream).await;
        assert_eq!(opcode, 0x8);
    }

    // -----------------------------------------------------------------------
    // Test 5: Upgrade validation — missing Upgrade header returns 400/error.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_websocket_invalid_upgrade_missing_header() {
        let router = Router::new().get("/ws-check", |req| async move {
            match ws::upgrade(req) {
                Ok((upgrade, resp)) => {
                    tokio::spawn(async move {
                        let _ = upgrade.accept().await;
                    });
                    Ok(resp)
                }
                Err(e) => {
                    // Return 400 Bad Request when upgrade fails.
                    let resp = http::Response::builder()
                        .status(http::StatusCode::BAD_REQUEST)
                        .body(http_body_util::Full::new(bytes::Bytes::from(e.to_string())))
                        .map_err(|e| oxihttp_core::OxiHttpError::Http(std::sync::Arc::new(e)))?;
                    Ok(resp)
                }
            }
        });

        let (addr, _shutdown) = spawn_ws_server(router).await;

        // Send a regular GET (no upgrade headers) — server should return 400.
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        let request =
            format!("GET /ws-check HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.expect("write");
        stream.flush().await.expect("flush");

        // Read response status line.
        let mut response_buf = Vec::with_capacity(256);
        loop {
            let mut byte = [0u8; 1];
            if stream.read_exact(&mut byte).await.is_err() {
                break;
            }
            response_buf.push(byte[0]);
            if response_buf.ends_with(b"\r\n\r\n") {
                break;
            }
            if response_buf.len() > 4096 {
                break;
            }
        }

        let response_str = String::from_utf8_lossy(&response_buf);
        assert!(
            response_str.starts_with("HTTP/1.1 400"),
            "expected 400 Bad Request, got: {response_str}"
        );
    }
}
