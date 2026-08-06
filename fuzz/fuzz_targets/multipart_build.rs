//! Fuzz target for `MultipartBuilder::build`, the `multipart/form-data`
//! (RFC 7578) body serializer.
//!
//! Field names, filenames, content-types, and body bytes routinely
//! originate from caller/user-controlled data (e.g. an uploaded file's
//! original filename), so the serializer must never panic regardless of
//! what those strings contain — including control characters, CRLF
//! sequences, non-UTF-8-adjacent byte patterns (via lossy conversion), or a
//! part body that happens to contain the boundary string itself (which
//! `MultipartBuilder::build`'s boundary-collision handling must resolve,
//! not crash on).
//!
//! Run with (requires the nightly toolchain `cargo-fuzz` uses internally):
//! ```text
//! cargo +nightly fuzz run multipart_build
//! ```

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxihttp_core::MultipartBuilder;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }

    // Steal a handful of leading bytes to decide how to carve up the rest
    // of `data` into several independent (name, value/body) pairs, so a
    // single fuzz input drives a builder with a variable, adversarial part
    // count rather than always exactly one part.
    let n_parts = (data[0] % 5) as usize + 1;
    let rest = &data[1..];
    let chunk_len = if n_parts == 0 {
        rest.len()
    } else {
        rest.len() / n_parts.max(1)
    };

    let mut builder = MultipartBuilder::new();
    for i in 0..n_parts {
        let start = i * chunk_len;
        let end = if i + 1 == n_parts {
            rest.len()
        } else {
            (start + chunk_len).min(rest.len())
        };
        if start >= end {
            continue;
        }
        let chunk = &rest[start..end];
        // Split the chunk roughly in half: first half becomes the
        // name/filename/content-type strings (lossily decoded so every
        // byte value, including control characters and CRLF, is still
        // exercised as *some* valid UTF-8 string), second half becomes the
        // raw part body (kept as arbitrary bytes, no UTF-8 requirement).
        let mid = chunk.len() / 2;
        let (str_part, body_part) = chunk.split_at(mid);
        let text = String::from_utf8_lossy(str_part).into_owned();

        if i % 2 == 0 {
            builder = builder.add_text(&text, text.clone());
        } else {
            builder = builder.add_file(&text, &text, &text, body_part.to_vec());
        }
    }

    // The return value is deliberately discarded: the only property under
    // test is "does not panic" (in particular, `find_unique_boundary`'s
    // `windows(boundary.len())` must never see a zero-length boundary, and
    // no header/body byte sequence should be able to trigger an
    // out-of-bounds slice or arithmetic overflow in a debug build).
    let _ = builder.build();
});
