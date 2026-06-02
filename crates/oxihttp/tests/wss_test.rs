//! WebSocket-over-TLS (WSS) smoke test.
//!
//! Verifies that WebSocket connections work correctly over a TLS transport
//! by combining the TLS server setup pattern from `mtls_test.rs` with the
//! raw WS frame I/O pattern from `websocket_test.rs`.

#![cfg(all(feature = "tls", feature = "server", feature = "websocket"))]

use oxihttp_server::{ws, Message, Router, Server};
use oxitls::rcgen_bridge::{generate_ca, generate_ca_signed_leaf, SigningAlgorithm};
use rustls::ClientConfig;
use rustls::RootCertStore;
use rustls_pki_types::{CertificateDer, ServerName};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

// ---------------------------------------------------------------------------
// Certificate helpers
// ---------------------------------------------------------------------------

/// Generate a root CA + server cert for localhost (ECDSA P-256).
///
/// Returns `(server_cert_pem, server_key_pem, ca_cert_der)`.
fn make_wss_certs() -> (String, String, Vec<u8>) {
    let ca = generate_ca("WSS Test CA", SigningAlgorithm::EcdsaP256).expect("CA gen");
    let server_ck = generate_ca_signed_leaf(&["localhost"], SigningAlgorithm::EcdsaP256, &ca)
        .expect("server cert gen");
    (
        server_ck.cert_pem.clone(),
        server_ck.key_pem(),
        ca.certified_key.cert_der.clone(),
    )
}

/// Build a `TlsConnector` that trusts the given CA cert DER and presents no client cert.
fn make_tls_connector(ca_cert_der: &[u8]) -> TlsConnector {
    let provider = oxitls::pure_provider();

    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(ca_cert_der.to_vec()))
        .expect("add CA root");

    let client_cfg = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("TLS 1.3 supported")
        .with_root_certificates(roots)
        .with_no_client_auth();

    TlsConnector::from(Arc::new(client_cfg))
}

// ---------------------------------------------------------------------------
// WS frame helpers (mirrored from websocket_test.rs, adapted for generic
// AsyncRead + AsyncWrite streams so they work over TLS)
// ---------------------------------------------------------------------------

/// Write a client→server masked WebSocket frame over any `AsyncWrite`.
async fn write_masked_frame<W: AsyncWriteExt + Unpin>(
    stream: &mut W,
    opcode: u8,
    payload: &[u8],
    fin: bool,
) {
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

    // Deterministic masking key.
    let mask: [u8; 4] = [0x37, 0xfa, 0x21, 0x3d];
    frame.extend_from_slice(&mask);

    // Masked payload.
    for (i, &b) in payload.iter().enumerate() {
        frame.push(b ^ mask[i % 4]);
    }

    stream.write_all(&frame).await.expect("write masked frame");
    stream.flush().await.expect("flush");
}

/// Read one server→client (unmasked) WebSocket frame from any `AsyncRead`.
/// Returns `(opcode, fin, payload)`.
async fn read_server_frame<R: AsyncReadExt + Unpin>(stream: &mut R) -> (u8, bool, Vec<u8>) {
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

// ---------------------------------------------------------------------------
// Test: WSS text echo
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_wss_text_echo() {
    // 1. Generate self-signed cert chain.
    let (cert_pem, key_pem, ca_cert_der) = make_wss_certs();

    // 2. Build a WS echo router (identical to websocket_test.rs).
    let router = Router::new().get("/wss", |req| async move {
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

    // 3. Start TLS server.
    let tls_cfg = oxihttp_server::TlsConfig::from_pem(cert_pem.as_bytes(), key_pem.as_bytes())
        .expect("TlsConfig from pem");

    let (addr, server_handle) = Server::bind("127.0.0.1:0")
        .with_tls(tls_cfg)
        .serve_with_addr(router)
        .await
        .expect("server bind");

    // Allow the server a moment to start accepting.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    // 4. Connect via raw TLS.
    let connector = make_tls_connector(&ca_cert_der);
    let tcp = TcpStream::connect(addr).await.expect("TCP connect");
    let server_name = ServerName::try_from("localhost").expect("server name");
    let mut tls_stream = connector
        .connect(server_name, tcp)
        .await
        .expect("TLS connect");

    // 5. Send the HTTP upgrade request over TLS.
    let ws_key = "dGhlIHNhbXBsZSBub25jZQ==";
    let upgrade_request = format!(
        "GET /wss HTTP/1.1\r\n\
         Host: localhost:{port}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {ws_key}\r\n\
         Sec-WebSocket-Version: 13\r\n\
         \r\n",
        port = addr.port(),
    );
    tls_stream
        .write_all(upgrade_request.as_bytes())
        .await
        .expect("write WS upgrade request");
    tls_stream.flush().await.expect("flush upgrade request");

    // 6. Read the HTTP 101 response.
    let mut response_buf = Vec::with_capacity(512);
    loop {
        let mut byte = [0u8; 1];
        tls_stream
            .read_exact(&mut byte)
            .await
            .expect("read upgrade response byte");
        response_buf.push(byte[0]);
        if response_buf.ends_with(b"\r\n\r\n") {
            break;
        }
        assert!(
            response_buf.len() <= 8192,
            "response headers too large (> 8 KiB)"
        );
    }

    let response_str = String::from_utf8_lossy(&response_buf);
    assert!(
        response_str.starts_with("HTTP/1.1 101"),
        "expected 101 Switching Protocols, got: {response_str}"
    );

    // 7. Send a text frame "hello WSS".
    write_masked_frame(&mut tls_stream, 0x1, b"hello WSS", true).await;

    // 8. Read the echoed text frame and assert its content.
    let (opcode, fin, payload) = read_server_frame(&mut tls_stream).await;
    assert_eq!(opcode, 0x1, "expected Text opcode (0x1)");
    assert!(fin, "expected FIN=1 on echo");
    assert_eq!(
        &payload,
        b"hello WSS",
        "echoed payload mismatch: {:?}",
        String::from_utf8_lossy(&payload)
    );

    // 9. Send a Close frame and wait for the server's Close echo.
    write_masked_frame(&mut tls_stream, 0x8, &[0x03, 0xe8], true).await;
    let (close_opcode, _fin, _payload) = read_server_frame(&mut tls_stream).await;
    assert_eq!(close_opcode, 0x8, "expected Close opcode (0x8)");

    server_handle.abort();
}
