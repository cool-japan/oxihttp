//! Minimal OxiHTTP client demonstrating GET/POST requests, JSON bodies, and
//! typed error handling.
//!
//! This example spins up a tiny local server (via `oxihttp::Server`) so it
//! can run standalone with no external network dependency. In a real
//! application you would point the client at an existing HTTP endpoint
//! instead.
//!
//! Run with:
//!
//! ```sh
//! cargo run -p oxihttp --example client_requests
//! ```

use oxihttp::{response, Client, OxiHttpError, Router, Server, ServerRequest};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
struct Greeting {
    name: String,
}

/// Spawn a throwaway local server for the example to talk to, returning its
/// bound address once it is ready to accept connections.
async fn spawn_demo_server() -> oxihttp::Result<std::net::SocketAddr> {
    let router = Router::new()
        .get("/status", |_req| async {
            response::text_response("all good")
        })
        .post("/greet", handle_greet);

    let bound = Server::bind("127.0.0.1:0").listen().await?;
    let addr = bound.local_addr();
    tokio::spawn(bound.serve(router));
    Ok(addr)
}

async fn handle_greet(
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
    let greeting: Greeting =
        serde_json::from_slice(&body_bytes).map_err(|e| OxiHttpError::Json(e.to_string()))?;
    response::json_response(&Greeting {
        name: format!("Hello, {}!", greeting.name),
    })
}

#[tokio::main]
async fn main() -> oxihttp::Result<()> {
    let addr = spawn_demo_server().await?;
    let client = Client::builder().build()?;

    // --- Plain-text GET -----------------------------------------------------
    let status_url = format!("http://{addr}/status");
    let resp = client.get(&status_url)?.send().await?;
    println!("GET {status_url} -> {}", resp.status());
    println!("body: {}", resp.body_text().await?);

    // --- JSON POST -----------------------------------------------------------
    let greet_url = format!("http://{addr}/greet");
    let resp = client
        .post(&greet_url)?
        .json(&Greeting {
            name: "OxiHTTP".to_string(),
        })?
        .send()
        .await?;
    let reply: Greeting = resp.body_json().await?;
    println!("POST {greet_url} -> {}", reply.name);

    // --- Typed error handling --------------------------------------------------
    // A GET to a route that does not exist on the demo server surfaces as a
    // normal HTTP 404 response (not an `Err`) — `oxihttp` only returns
    // `Err(OxiHttpError)` for transport-level failures (DNS, connect, TLS,
    // timeout, etc.), matching how a real HTTP client should behave.
    let missing_url = format!("http://{addr}/does-not-exist");
    let resp = client.get(&missing_url)?.send().await?;
    println!("GET {missing_url} -> {} (expected 404)", resp.status());

    // A connection failure, by contrast, surfaces as a typed error.
    match client.get("http://127.0.0.1:1")?.send().await {
        Ok(resp) => println!("unexpected success: {}", resp.status()),
        Err(err) => println!("connection to port 1 failed as expected: {err}"),
    }

    Ok(())
}
