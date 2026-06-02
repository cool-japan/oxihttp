//! Integration tests for streaming response bodies.

use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::Full;
use std::time::Duration;

/// Spawn an HTTP server that returns a body of `size` bytes.
async fn spawn_large_body_server(size: usize) -> u16 {
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let io = hyper_util::rt::TokioIo::new(stream);
                let svc = service_fn(move |_req: hyper::Request<hyper::body::Incoming>| {
                    let data = vec![42u8; size];
                    async move {
                        Ok::<_, std::convert::Infallible>(
                            hyper::Response::builder()
                                .status(200u16)
                                .body(Full::new(Bytes::from(data)))
                                .expect("response build"),
                        )
                    }
                });
                let _ = http1::Builder::new().serve_connection(io, svc).await;
            });
        }
    });

    tokio::time::sleep(Duration::from_millis(5)).await;
    port
}

#[tokio::test]
async fn test_body_stream_1mb() {
    let size = 1024 * 1024; // 1 MiB
    let port = spawn_large_body_server(size).await;

    let client = oxihttp_client::Client::builder().build().expect("client");
    let url = format!("http://127.0.0.1:{port}/big");

    let resp = client
        .get(&url)
        .expect("builder")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status().as_u16(), 200);

    let mut stream = resp.body_stream();
    let mut total_bytes = 0usize;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("chunk error");
        total_bytes += chunk.len();
    }

    assert_eq!(
        total_bytes, size,
        "streamed {total_bytes} bytes, expected {size}"
    );
}

#[tokio::test]
async fn test_body_stream_empty() {
    let port = spawn_large_body_server(0).await;

    let client = oxihttp_client::Client::builder().build().expect("client");
    let url = format!("http://127.0.0.1:{port}/empty");

    let resp = client
        .get(&url)
        .expect("builder")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status().as_u16(), 200);

    let mut stream = resp.body_stream();
    let mut total_bytes = 0usize;
    while let Some(chunk) = stream.next().await {
        total_bytes += chunk.expect("chunk").len();
    }
    assert_eq!(total_bytes, 0);
}
