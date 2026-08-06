//! Fuzz target for `oxihttp_server::ws_frame::read_frame`, the WebSocket
//! frame codec (RFC 6455 §5).
//!
//! This is the crate's only hand-written binary wire-format parser for
//! WebSocket: it runs directly on bytes received from the peer over an
//! HTTP/1 Upgrade connection, so it must never panic — only resolve to
//! `Ok(Frame)` or a typed `OxiHttpError`, on any input. `ws_frame.rs`
//! already carries proptest coverage for this same property
//! (`fuzz_tests::read_frame_never_panics_on_random_bytes` and
//! `..._on_structured_adversarial_bytes`); this target gives the same
//! parser coverage-guided (rather than purely random) exploration.
//!
//! Run with (requires the nightly toolchain `cargo-fuzz` uses internally):
//! ```text
//! cargo +nightly fuzz run ws_frame_read
//! ```

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxihttp_server::ws_frame::{read_frame, DEFAULT_MAX_PAYLOAD_LEN};
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async {
        let mut cursor = Cursor::new(data);
        // The return value is deliberately discarded: the only property
        // under test is "does not panic" (no OOB slice index, no
        // arithmetic overflow panic in a debug build, no unwrap/expect
        // anywhere on this path) — reaching this point is the assertion.
        let _ = read_frame(&mut cursor, DEFAULT_MAX_PAYLOAD_LEN).await;
    });
});
