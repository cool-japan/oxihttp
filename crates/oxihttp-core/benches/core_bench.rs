//! Criterion benchmarks for `oxihttp-core` types (M9 Block E).
//!
//! Groups:
//!   - `header_map_ext`          — typed accessor vs raw `HeaderMap::get`
//!   - `cookie_parsing`          — `Cookie::parse_set_cookie` throughput
//!   - `multipart_serialization` — `MultipartBuilder::build` for a ~1 MB payload
//!   - `request_builder`         — `CoreRequestBuilder` vs raw `http::Request::builder`
//!   - `body_collect`            — `PinnedBody::collect` for a 64 KB in-memory body

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use http_body_util::BodyExt as _;
use oxihttp_core::{Cookie, CoreRequestBuilder, HeaderMapExt, MultipartBuilder};
use std::hint::black_box;

// ---------------------------------------------------------------------------
// Bench 1 — HeaderMapExt typed accessor vs raw lookup
// ---------------------------------------------------------------------------

fn bench_header_map_ext(c: &mut Criterion) {
    let mut map = http::HeaderMap::new();
    map.insert(
        http::header::CONTENT_TYPE,
        "application/json".parse().expect("bench setup"),
    );
    map.insert(
        http::header::CONTENT_LENGTH,
        "1024".parse().expect("bench setup"),
    );
    map.insert(
        http::header::AUTHORIZATION,
        "Bearer token123".parse().expect("bench setup"),
    );
    map.insert(
        http::header::ACCEPT,
        "text/html,application/json".parse().expect("bench setup"),
    );
    map.insert(
        http::header::HOST,
        "example.com".parse().expect("bench setup"),
    );

    let mut group = c.benchmark_group("header_map_ext");

    group.bench_function("typed_content_type", |b| {
        b.iter(|| black_box(map.content_type()))
    });

    group.bench_function("raw_content_type", |b| {
        b.iter(|| black_box(map.get(http::header::CONTENT_TYPE)))
    });

    group.bench_function("typed_content_length", |b| {
        b.iter(|| black_box(map.content_length()))
    });

    group.bench_function("raw_content_length", |b| {
        b.iter(|| black_box(map.get(http::header::CONTENT_LENGTH)))
    });

    group.bench_function("typed_authorization", |b| {
        b.iter(|| black_box(map.authorization()))
    });

    group.bench_function("raw_authorization", |b| {
        b.iter(|| black_box(map.get(http::header::AUTHORIZATION)))
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Bench 2 — Cookie parsing throughput
// ---------------------------------------------------------------------------

fn bench_cookie_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("cookie_parsing");
    group.throughput(Throughput::Elements(1000));

    let cookie_str = "session=abc123; Domain=example.com; Path=/; Max-Age=3600; Secure; HttpOnly";

    group.bench_function("parse_1000_cookies", |b| {
        b.iter(|| {
            for _ in 0..1000 {
                let _ = black_box(Cookie::parse_set_cookie(black_box(cookie_str)));
            }
        })
    });

    // Also bench a minimal cookie (name=value only) vs full cookie
    let minimal_str = "s=x";
    group.bench_function("parse_1000_minimal", |b| {
        b.iter(|| {
            for _ in 0..1000 {
                let _ = black_box(Cookie::parse_set_cookie(black_box(minimal_str)));
            }
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Bench 3 — MultipartBuilder serialization
// ---------------------------------------------------------------------------

fn bench_multipart_serialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("multipart_serialization");

    // ~1 MB binary payload (refcounted Bytes — clone is O(1))
    let payload_1mb: Bytes = Bytes::from(vec![b'x'; 1024 * 1024]);
    group.throughput(Throughput::Bytes(1024 * 1024));

    group.bench_function("1mb_single_file_field", |b| {
        b.iter(|| {
            let body = MultipartBuilder::new()
                .add_file(
                    "upload",
                    "data.bin",
                    "application/octet-stream",
                    payload_1mb.clone(),
                )
                .build();
            black_box(body)
        })
    });

    // Small multi-field form (text fields only) — low-overhead path
    group.throughput(Throughput::Elements(5));
    group.bench_function("5_text_fields", |b| {
        b.iter(|| {
            let body = MultipartBuilder::new()
                .add_text("field1", "value_one")
                .add_text("field2", "value_two")
                .add_text("field3", "value_three")
                .add_text("field4", "value_four")
                .add_text("field5", "value_five")
                .build();
            black_box(body)
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Bench 4 — RequestBuilder construction overhead
// ---------------------------------------------------------------------------

fn bench_request_builder(c: &mut Criterion) {
    let mut group = c.benchmark_group("request_builder");

    group.bench_function("oxihttp_core_builder", |b| {
        b.iter(|| {
            let req = CoreRequestBuilder::post("http://localhost/api")
                .expect("bench uri")
                .header("content-type", "application/json")
                .expect("bench header")
                .build()
                .expect("bench build");
            black_box(req)
        })
    });

    group.bench_function("raw_http_builder", |b| {
        b.iter(|| {
            black_box(
                http::Request::builder()
                    .method("POST")
                    .uri("http://localhost/api")
                    .header("content-type", "application/json")
                    .body(Bytes::from("{}"))
                    .expect("bench build"),
            )
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Bench 5 — Body collection (PinnedBody::collect via http_body_util::BodyExt)
// ---------------------------------------------------------------------------

fn bench_body_collect(c: &mut Criterion) {
    let mut group = c.benchmark_group("body_collect");

    let rt = tokio::runtime::Runtime::new().expect("bench runtime");

    // 64 KB in-memory body — Full variant (fast path: single frame)
    let data_64kb: Bytes = Bytes::from(vec![0u8; 65_536]);
    group.throughput(Throughput::Bytes(65_536));

    group.bench_function("full_body_64kb", |b| {
        b.iter(|| {
            let pinned = oxihttp_core::Body::full(black_box(data_64kb.clone())).into_pinned();
            rt.block_on(async {
                let collected = pinned.collect().await.expect("bench collect");
                black_box(collected.to_bytes())
            })
        })
    });

    // Empty body — ultra-fast path
    group.throughput(Throughput::Bytes(0));
    group.bench_function("empty_body", |b| {
        b.iter(|| {
            let pinned = oxihttp_core::Body::empty().into_pinned();
            rt.block_on(async {
                let collected = pinned.collect().await.expect("bench collect");
                black_box(collected.to_bytes())
            })
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

criterion_group!(
    core_benches,
    bench_header_map_ext,
    bench_cookie_parsing,
    bench_multipart_serialization,
    bench_request_builder,
    bench_body_collect,
);
criterion_main!(core_benches);
