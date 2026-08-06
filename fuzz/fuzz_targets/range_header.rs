//! Fuzz target for the `Range` request-header parser (`parse_single_range`
//! in `oxihttp-server`'s `static_files` module), exercised through the
//! public `ServeFile::serve` API rather than the private parsing function
//! directly — this is the real boundary where untrusted bytes (an
//! attacker-controlled `Range` header on a GET request) enter the crate,
//! and it additionally exercises the conditional-GET and streaming-body
//! construction paths that consume the parsed range.
//!
//! `static_files.rs` already carries proptest coverage for
//! `parse_single_range` directly (`fuzz_parse_single_range_never_panics`
//! and `..._structured`); this target gives the same parser coverage-guided
//! exploration through the end-to-end request path instead of calling it in
//! isolation.
//!
//! Uses a small fixed-content fixture file
//! (`fuzz/fixtures/range_fixture.bin`, 4096 bytes, committed to the repo)
//! resolved via `CARGO_MANIFEST_DIR` so the target has no dependency on the
//! process's current working directory or any machine-specific path.
//!
//! Run with (requires the nightly toolchain `cargo-fuzz` uses internally):
//! ```text
//! cargo +nightly fuzz run range_header
//! ```

#![no_main]

use http::{HeaderMap, HeaderValue, Method};
use libfuzzer_sys::fuzz_target;
use oxihttp_server::ServeFile;
use std::sync::OnceLock;

fn fixture_path() -> &'static std::path::Path {
    static PATH: OnceLock<std::path::PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/range_fixture.bin")
    })
}

fuzz_target!(|data: &[u8]| {
    // `Range` header values must be valid UTF-8 to become a `HeaderValue`
    // in the first place (`HeaderValue::from_bytes` accepts opaque bytes,
    // but the parser under test reads it via `to_str()`, which rejects
    // non-UTF-8) — skip inputs that could never arrive as a real header
    // value rather than lossily rewriting them.
    let Ok(range_str) = std::str::from_utf8(data) else {
        return;
    };
    // `HeaderValue` additionally rejects control characters other than
    // horizontal tab; skip those too rather than mutating the input, so
    // every accepted input is byte-for-byte what the parser sees.
    let Ok(range_value) = HeaderValue::from_str(range_str) else {
        return;
    };

    let mut headers = HeaderMap::new();
    headers.insert(http::header::RANGE, range_value);

    let serve_file = ServeFile::new(fixture_path());

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async {
        // The return value is deliberately discarded: the only property
        // under test is "does not panic" — a malformed/adversarial `Range`
        // value must resolve to some `http::Response` (e.g. `416 Range Not
        // Satisfiable`) or a typed `OxiHttpError`, never a panic.
        let _ = serve_file.serve(&Method::GET, &headers).await;
    });
});
