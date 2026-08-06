//! Property-based fuzz test for HTTP/1 **response** parsing on the client side.
//!
//! `server_fuzz_test.rs` already covers the *request* half (arbitrary bytes
//! sent to a live `oxihttp_server::Server`). This file covers the other half
//! of the same wire protocol: a real `oxihttp::Client` sending a well-formed
//! request to a raw TCP listener that replies with adversarial/random bytes
//! instead of a valid HTTP/1.1 response. The response parser (hyper, driven
//! through `oxihttp-client`) must never panic — it may only resolve to `Ok`
//! (if the garbage happens to be a minimally valid response) or `Err` via the
//! crate's typed `OxiHttpError`.

#[cfg(feature = "client")]
mod client_response_fuzz {
    use proptest::prelude::*;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener as StdTcpListener;
    use std::time::Duration;

    /// Bind a raw (non-HTTP-aware) TCP listener on an OS-assigned port, spawn a
    /// thread that accepts exactly one connection, drains whatever the client
    /// sends, then writes `reply_bytes` back verbatim before closing the
    /// socket. Returns the address to connect to.
    fn spawn_garbage_responder(reply_bytes: Vec<u8>) -> std::net::SocketAddr {
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind garbage responder");
        let addr = listener.local_addr().expect("local addr");

        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
            let mut drain = [0u8; 4096];
            // Drain the request; a timeout/EOF here is expected and fine.
            let _ = stream.read(&mut drain);

            let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
            let _ = stream.write_all(&reply_bytes);
            let _ = stream.flush();
            // Dropping `stream` closes the connection.
        });

        addr
    }

    proptest! {
        // Limit cases to keep CI time reasonable; shrinking is also capped.
        #![proptest_config(ProptestConfig {
            cases: 64,
            max_shrink_iters: 16,
            ..ProptestConfig::default()
        })]

        /// A server that responds with arbitrary bytes instead of a valid
        /// HTTP/1.1 status line + headers must not crash the client. The only
        /// permitted outcomes are `Ok(Response)` (the garbage happened to be
        /// parseable) or `Err(OxiHttpError)`.
        #[test]
        fn test_malformed_http_response_no_panic(
            reply_bytes in prop::collection::vec(any::<u8>(), 0..1024)
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");

            rt.block_on(async {
                let addr = spawn_garbage_responder(reply_bytes);
                let url = format!("http://{addr}/");

                let Ok(client) = oxihttp::Client::builder().build() else {
                    return;
                };
                let Ok(builder) = client.get(&url) else {
                    return;
                };
                // Bound the wait so a hung connection cannot stall the test suite.
                let _ = tokio::time::timeout(Duration::from_secs(2), builder.send()).await;
                // Reaching this point (Ok, Err, or timeout) means no panic occurred.
            });
        }
    }
}
