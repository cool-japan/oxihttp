//! Integration tests for the multipart body builder with a real HTTP round-trip.

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::Request as HyperRequest;
use hyper::Response as HyperResponse;
use std::convert::Infallible;
use std::net::SocketAddr;

use oxihttp::MultipartBuilder;

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
