//! Minimal OxiHTTP server demonstrating routing, path parameters, JSON
//! responses, and graceful shutdown.
//!
//! Run with:
//!
//! ```sh
//! cargo run -p oxihttp --example server_router
//! ```
//!
//! Then, in another terminal:
//!
//! ```sh
//! curl http://127.0.0.1:3000/
//! curl http://127.0.0.1:3000/hello/world
//! curl http://127.0.0.1:3000/health
//! curl -X POST -d '{"name":"oxihttp"}' http://127.0.0.1:3000/echo
//! ```
//!
//! Stop the server with Ctrl+C — the graceful shutdown handler lets any
//! in-flight requests finish before the process exits.

use oxihttp::{response, OxiHttpError, Router, Server, ServerRequest};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
struct EchoRequest {
    name: String,
}

#[derive(Debug, Serialize)]
struct EchoResponse {
    message: String,
}

async fn hello(
    req: ServerRequest,
) -> Result<http::Response<http_body_util::Full<bytes::Bytes>>, OxiHttpError> {
    let name = req.param("name").unwrap_or("stranger").to_owned();
    response::text_response(format!("Hello, {name}!"))
}

async fn echo(
    req: ServerRequest,
) -> Result<http::Response<http_body_util::Full<bytes::Bytes>>, OxiHttpError> {
    use http_body_util::BodyExt as _;

    let body_bytes = req
        .into_inner()
        .into_body()
        .collect()
        .await
        .map_err(|e| OxiHttpError::Body(e.to_string()))?
        .to_bytes();

    let payload: EchoRequest =
        serde_json::from_slice(&body_bytes).map_err(|e| OxiHttpError::Json(e.to_string()))?;

    response::json_response(&EchoResponse {
        message: format!("you said: {}", payload.name),
    })
}

#[tokio::main]
async fn main() -> oxihttp::Result<()> {
    let router = Router::new()
        .get("/", |_req| async {
            response::text_response("OxiHTTP server example")
        })
        .get("/hello/:name", hello)
        .post("/echo", echo)
        .health("/health");

    println!("Listening on http://127.0.0.1:3000 (Ctrl+C to stop)");

    Server::bind("127.0.0.1:3000")
        .shutdown_on_ctrl_c()
        .serve(router)
        .await
}
