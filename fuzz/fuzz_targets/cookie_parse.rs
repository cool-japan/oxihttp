//! Fuzz target for `Cookie::parse_set_cookie`, the `Set-Cookie` response
//! header parser.
//!
//! This is the crate's only hand-written `Set-Cookie` attribute parser: it
//! runs directly on a header value received from an untrusted (or
//! compromised) server, so it must never panic — only return `Some(Cookie)`
//! or `None`, on any input.
//!
//! Run with (requires the nightly toolchain `cargo-fuzz` uses internally):
//! ```text
//! cargo +nightly fuzz run cookie_parse
//! ```

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxihttp_core::Cookie;

fuzz_target!(|data: &[u8]| {
    // `parse_set_cookie` takes `&str`; skip inputs libFuzzer generated that
    // are not valid UTF-8 rather than lossily rewriting them, so every run
    // exercises the parser against exactly the bytes discovered — matching
    // how a real header value reaches it (`HeaderValue::to_str()` already
    // rejects non-UTF-8 header values upstream of this parser in the rest
    // of the crate).
    let Ok(header) = std::str::from_utf8(data) else {
        return;
    };

    // The return value is deliberately discarded: the only property under
    // test is "does not panic" (no OOB slice index, no arithmetic overflow
    // panic in a debug build, no unwrap/expect anywhere on this path).
    let _ = Cookie::parse_set_cookie(header);
});
