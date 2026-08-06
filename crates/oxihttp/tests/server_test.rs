//! Integration tests for the OxiHTTP server.

use bytes::Bytes;
use http_body_util::Full;
use oxihttp_core::OxiHttpError;
use std::time::Duration;

use oxihttp_server::Server;

/// Spawn a test server and return its address, along with a shutdown sender.
async fn spawn_test_server(
    router: oxihttp_server::Router,
) -> (std::net::SocketAddr, tokio::sync::oneshot::Sender<()>) {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let (addr, _handle) = oxihttp_server::Server::bind("127.0.0.1:0")
        .with_graceful_shutdown(async move {
            let _ = rx.await;
        })
        .serve_with_addr(router)
        .await
        .expect("server bind");

    // Give the server a moment to start accepting
    tokio::time::sleep(Duration::from_millis(10)).await;

    (addr, tx)
}

#[tokio::test]
async fn test_server_get_handler() {
    let router = oxihttp_server::Router::new().get("/hello", |_req| async {
        oxihttp_server::response::text_response("Hello from server!")
    });

    let (addr, _shutdown) = spawn_test_server(router).await;

    let client = oxihttp_client::Client::builder().build().expect("client");
    let url = format!("http://{addr}/hello");
    let resp = client.get(&url).expect("GET").send().await.expect("send");
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.body_text().await.expect("body");
    assert_eq!(body, "Hello from server!");
}

#[tokio::test]
async fn test_server_post_json_handler() {
    let router = oxihttp_server::Router::new().post("/json", |req| async {
        let value: serde_json::Value = req.body_json().await?;
        oxihttp_server::response::json_response(&value)
    });

    let (addr, _shutdown) = spawn_test_server(router).await;

    let client = oxihttp_client::Client::builder().build().expect("client");
    let url = format!("http://{addr}/json");
    let payload = serde_json::json!({"echo": true, "count": 42});
    let resp = client
        .post(&url)
        .expect("POST")
        .json(&payload)
        .expect("json")
        .send()
        .await
        .expect("send");

    assert_eq!(resp.status(), http::StatusCode::OK);
    let result: serde_json::Value = resp.body_json().await.expect("json parse");
    assert_eq!(result["echo"], true);
    assert_eq!(result["count"], 42);
}

#[tokio::test]
async fn test_server_path_params() {
    let router = oxihttp_server::Router::new().get("/users/:id", |req| async move {
        let id = req.param("id").unwrap_or("unknown").to_string();
        oxihttp_server::response::text_response(format!("user={id}"))
    });

    let (addr, _shutdown) = spawn_test_server(router).await;

    let client = oxihttp_client::Client::builder().build().expect("client");
    let url = format!("http://{addr}/users/42");
    let resp = client.get(&url).expect("GET").send().await.expect("send");
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.body_text().await.expect("body");
    assert_eq!(body, "user=42");
}

#[tokio::test]
async fn test_server_query_params() {
    let router = oxihttp_server::Router::new().get("/search", |req| async move {
        let q = req.query("q").unwrap_or_default();
        oxihttp_server::response::text_response(format!("query={q}"))
    });

    let (addr, _shutdown) = spawn_test_server(router).await;

    let client = oxihttp_client::Client::builder().build().expect("client");
    let url = format!("http://{addr}/search?q=hello");
    let resp = client.get(&url).expect("GET").send().await.expect("send");
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.body_text().await.expect("body");
    assert_eq!(body, "query=hello");
}

#[tokio::test]
async fn test_server_404_not_found() {
    let router = oxihttp_server::Router::new().get("/exists", |_req| async {
        oxihttp_server::response::text_response("found")
    });

    let (addr, _shutdown) = spawn_test_server(router).await;

    let client = oxihttp_client::Client::builder()
        .redirect_policy(oxihttp_client::RedirectPolicy::None)
        .build()
        .expect("client");
    let url = format!("http://{addr}/not-here");
    let resp = client.get(&url).expect("GET").send().await.expect("send");
    assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_server_405_method_not_allowed() {
    let router = oxihttp_server::Router::new().get("/only-get", |_req| async {
        oxihttp_server::response::text_response("ok")
    });

    let (addr, _shutdown) = spawn_test_server(router).await;

    let client = oxihttp_client::Client::builder()
        .redirect_policy(oxihttp_client::RedirectPolicy::None)
        .build()
        .expect("client");
    let url = format!("http://{addr}/only-get");
    let resp = client.post(&url).expect("POST").send().await.expect("send");
    assert_eq!(resp.status(), http::StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn test_server_nested_router() {
    let api_router = oxihttp_server::Router::new()
        .get("/users", |_req| async {
            oxihttp_server::response::text_response("users list")
        })
        .get("/items", |_req| async {
            oxihttp_server::response::text_response("items list")
        });

    let router = oxihttp_server::Router::new()
        .get("/", |_req| async {
            oxihttp_server::response::text_response("home")
        })
        .nest("/api", api_router);

    let (addr, _shutdown) = spawn_test_server(router).await;

    let client = oxihttp_client::Client::builder().build().expect("client");

    let resp = client
        .get(&format!("http://{addr}/api/users"))
        .expect("GET")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.body_text().await.expect("body");
    assert_eq!(body, "users list");

    let resp = client
        .get(&format!("http://{addr}/api/items"))
        .expect("GET")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.body_text().await.expect("body");
    assert_eq!(body, "items list");
}

#[tokio::test]
async fn test_server_health_check() {
    let router = oxihttp_server::Router::new().health("/health");

    let (addr, _shutdown) = spawn_test_server(router).await;

    let client = oxihttp_client::Client::builder().build().expect("client");
    let resp = client
        .get(&format!("http://{addr}/health"))
        .expect("GET")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.body_text().await.expect("body");
    assert_eq!(body, "OK");
}

#[tokio::test]
async fn test_server_body_limit() {
    let router = oxihttp_server::Router::new().post("/upload", |req| async {
        let data = req.body_bytes().await?;
        oxihttp_server::response::text_response(format!("received {} bytes", data.len()))
    });

    let (addr, _shutdown) = {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();

        let (addr, _handle) = oxihttp_server::Server::bind("127.0.0.1:0")
            .with_body_limit(100) // 100 bytes max
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .serve_with_addr(router)
            .await
            .expect("server bind");

        tokio::time::sleep(Duration::from_millis(10)).await;
        (addr, tx)
    };

    let client = oxihttp_client::Client::builder().build().expect("client");

    // Small body - should succeed
    let resp = client
        .post(&format!("http://{addr}/upload"))
        .expect("POST")
        .body(Bytes::from("small"))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), http::StatusCode::OK);

    // Large body - should be rejected (413)
    let large_body = Bytes::from(vec![b'x'; 200]);
    let resp = client
        .post(&format!("http://{addr}/upload"))
        .expect("POST")
        .header("content-length", "200")
        .expect("header")
        .body(large_body)
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), http::StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn test_server_cors_preflight() {
    let cors = oxihttp_server::CorsConfig::permissive();
    let router = oxihttp_server::Router::new().get("/api", |_req| async {
        oxihttp_server::response::text_response("ok")
    });

    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
    let (addr, _handle) = oxihttp_server::Server::bind("127.0.0.1:0")
        .with_cors(cors)
        .with_graceful_shutdown(async move {
            let _ = rx.await;
        })
        .serve_with_addr(router)
        .await
        .expect("server bind");
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Build a pre-built OPTIONS request
    let client = oxihttp_client::Client::builder().build().expect("client");
    let req = http::Request::builder()
        .method(http::Method::OPTIONS)
        .uri(format!("http://{addr}/api"))
        .header("origin", "http://example.com")
        .body(http_body_util::Full::new(Bytes::new()))
        .expect("request build");
    let resp = client.execute(req).await.expect("execute");
    assert_eq!(resp.status(), http::StatusCode::NO_CONTENT);
    let headers = resp.headers();
    assert!(headers.contains_key(http::header::ACCESS_CONTROL_ALLOW_ORIGIN));
}

#[tokio::test]
async fn test_server_response_helpers() {
    let router = oxihttp_server::Router::new()
        .get("/text", |_req| async {
            oxihttp_server::response::text_response("plain text")
        })
        .get("/html", |_req| async {
            oxihttp_server::response::html_response("<h1>Hello</h1>")
        })
        .get("/no-content", |_req| async {
            oxihttp_server::response::no_content()
        })
        .get("/bad-request", |_req| async {
            oxihttp_server::response::bad_request("invalid input")
        })
        .get("/error", |_req| async {
            oxihttp_server::response::internal_error("something broke")
        });

    let (addr, _shutdown) = spawn_test_server(router).await;
    let client = oxihttp_client::Client::builder()
        .redirect_policy(oxihttp_client::RedirectPolicy::None)
        .build()
        .expect("client");

    // Text
    let resp = client
        .get(&format!("http://{addr}/text"))
        .expect("GET")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.content_type(), Some("text/plain; charset=utf-8"));

    // HTML
    let resp = client
        .get(&format!("http://{addr}/html"))
        .expect("GET")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.content_type(), Some("text/html; charset=utf-8"));

    // No Content
    let resp = client
        .get(&format!("http://{addr}/no-content"))
        .expect("GET")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), http::StatusCode::NO_CONTENT);

    // Bad Request
    let resp = client
        .get(&format!("http://{addr}/bad-request"))
        .expect("GET")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);

    // Internal Server Error
    let resp = client
        .get(&format!("http://{addr}/error"))
        .expect("GET")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn test_server_custom_fallback() {
    let router = oxihttp_server::Router::new()
        .get("/known", |_req| async {
            oxihttp_server::response::text_response("known route")
        })
        .fallback(|_req| async {
            let resp = hyper::Response::builder()
                .status(http::StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::from("custom 404")))
                .map_err(|e| OxiHttpError::Http(std::sync::Arc::new(e)))?;
            Ok(resp)
        });

    let (addr, _shutdown) = spawn_test_server(router).await;

    let client = oxihttp_client::Client::builder()
        .redirect_policy(oxihttp_client::RedirectPolicy::None)
        .build()
        .expect("client");
    let resp = client
        .get(&format!("http://{addr}/unknown"))
        .expect("GET")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
    let body = resp.body_text().await.expect("body");
    assert_eq!(body, "custom 404");
}

#[tokio::test]
async fn test_rate_limiting_returns_429() {
    // Rate limiter: 1 token max, refill at 0.001 tokens/second (essentially no
    // refill during the test).  The first request consumes the single token;
    // the second immediate request should receive 429 Too Many Requests.
    let rate_limiter = oxihttp_server::RateLimiter::new(1, 0.001_f64);

    let router = oxihttp_server::Router::new().get("/limited", |_req| async {
        oxihttp_server::response::text_response("ok")
    });

    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
    let (addr, _handle) = oxihttp_server::Server::bind("127.0.0.1:0")
        .with_rate_limiter(rate_limiter)
        .with_graceful_shutdown(async move {
            let _ = rx.await;
        })
        .serve_with_addr(router)
        .await
        .expect("server start");

    tokio::time::sleep(Duration::from_millis(10)).await;

    let client = oxihttp_client::Client::builder()
        .redirect_policy(oxihttp_client::RedirectPolicy::None)
        .build()
        .expect("client build");
    let url = format!("http://{addr}/limited");

    // First request should succeed (consumes the 1 available token).
    let resp1 = client
        .get(&url)
        .expect("builder")
        .send()
        .await
        .expect("first request");
    assert_eq!(
        resp1.status(),
        http::StatusCode::OK,
        "first request should be 200 OK"
    );

    // Second immediate request should be rate-limited (429).
    let resp2 = client
        .get(&url)
        .expect("builder")
        .send()
        .await
        .expect("second request");
    assert_eq!(
        resp2.status(),
        http::StatusCode::TOO_MANY_REQUESTS,
        "second immediate request should be 429 Too Many Requests"
    );
}

/// Regression test for the spoofable-`X-Forwarded-For` rate-limit bypass.
///
/// Before the fix, the rate limiter keyed its bucket solely on the
/// client-controlled `X-Forwarded-For` header. An attacker could therefore
/// send a different (fabricated) value on every request and get a fresh
/// token bucket each time, completely bypassing the limit. With the fix,
/// `X-Forwarded-For` is ignored by default (no trusted-proxy configuration),
/// so two requests from the same TCP peer share one bucket *regardless* of
/// how many different XFF values they claim.
#[tokio::test]
async fn test_rate_limiting_ignores_spoofed_x_forwarded_for_by_default() {
    let rate_limiter = oxihttp_server::RateLimiter::new(1, 0.001_f64);

    let router = oxihttp_server::Router::new().get("/limited", |_req| async {
        oxihttp_server::response::text_response("ok")
    });

    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
    let (addr, _handle) = oxihttp_server::Server::bind("127.0.0.1:0")
        .with_rate_limiter(rate_limiter)
        .with_graceful_shutdown(async move {
            let _ = rx.await;
        })
        .serve_with_addr(router)
        .await
        .expect("server start");

    tokio::time::sleep(Duration::from_millis(10)).await;

    let client = oxihttp_client::Client::builder()
        .redirect_policy(oxihttp_client::RedirectPolicy::None)
        .build()
        .expect("client build");
    let url = format!("http://{addr}/limited");

    // First request claims to be forwarded from 1.1.1.1 and consumes the
    // single token.
    let resp1 = client
        .get(&url)
        .expect("builder")
        .header("x-forwarded-for", "1.1.1.1")
        .expect("header")
        .send()
        .await
        .expect("first request");
    assert_eq!(resp1.status(), http::StatusCode::OK);

    // Second request — same TCP peer (127.0.0.1), but a *different* claimed
    // X-Forwarded-For. Under the old key-on-XFF behavior this would land in
    // a brand-new bucket and succeed, bypassing the limit entirely. It must
    // now be rejected because both requests share the same peer-IP bucket.
    let resp2 = client
        .get(&url)
        .expect("builder")
        .header("x-forwarded-for", "2.2.2.2")
        .expect("header")
        .send()
        .await
        .expect("second request");
    assert_eq!(
        resp2.status(),
        http::StatusCode::TOO_MANY_REQUESTS,
        "rotating X-Forwarded-For must not bypass the rate limit"
    );
}

#[tokio::test]
async fn test_full_client_server_roundtrip() {
    // Set up a server with multiple endpoints
    let router = oxihttp_server::Router::new()
        .get("/ping", |_req| async {
            oxihttp_server::response::text_response("pong")
        })
        .post("/echo", |req| async {
            let body = req.body_bytes().await?;
            hyper::Response::builder()
                .status(http::StatusCode::OK)
                .body(Full::new(body))
                .map_err(|e| OxiHttpError::Http(std::sync::Arc::new(e)))
        })
        .get("/users/:id/profile", |req| async move {
            let id = req.param("id").unwrap_or("0").to_string();
            oxihttp_server::response::json_response(
                &serde_json::json!({"user_id": id, "name": "Test User"}),
            )
        })
        .health("/health");

    let (addr, _shutdown) = spawn_test_server(router).await;

    // Use the facade API for the client
    let client = oxihttp::Client::builder().build().expect("client");

    // GET /ping
    let resp = client
        .get(&format!("http://{addr}/ping"))
        .expect("GET")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.body_text().await.expect("body"), "pong");

    // POST /echo
    let resp = client
        .post(&format!("http://{addr}/echo"))
        .expect("POST")
        .body(Bytes::from("roundtrip"))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.body_text().await.expect("body"), "roundtrip");

    // GET /users/99/profile
    let user: serde_json::Value = client
        .get_json(&format!("http://{addr}/users/99/profile"))
        .await
        .expect("get_json");
    assert_eq!(user["user_id"], "99");
    assert_eq!(user["name"], "Test User");

    // GET /health
    let resp = client
        .get(&format!("http://{addr}/health"))
        .expect("GET")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), http::StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Feature tests: BoundServer::local_addr(), virtual hosts, Display
// ---------------------------------------------------------------------------

/// `BoundServer::local_addr()` returns a non-zero port when binding port 0.
#[tokio::test]
async fn test_bound_server_local_addr() {
    let bound = Server::bind("127.0.0.1:0").listen().await.expect("listen");
    let port = bound.local_addr().port();
    assert!(port > 0, "local_addr port should be non-zero, got {port}");
    // Drop `bound` without calling serve() — that is fine for this test.
}

/// Virtual host dispatch routes requests to the correct sub-router.
#[tokio::test]
async fn test_virtual_host_dispatch() {
    let api_router = oxihttp_server::Router::new().get("/data", |_req| async {
        oxihttp_server::response::text_response("api-data")
    });
    let web_router = oxihttp_server::Router::new().get("/", |_req| async {
        oxihttp_server::response::text_response("web-home")
    });

    let router = oxihttp_server::Router::new()
        .host("api.local", api_router)
        .host("web.local", web_router);

    let (addr, _shutdown) = spawn_test_server(router).await;

    // Use a raw hyper client so we can set Host headers explicitly.
    let client = oxihttp_client::Client::builder().build().expect("client");

    // Request to api.local
    let resp = client
        .get(&format!("http://{addr}/data"))
        .expect("GET")
        .header("host", "api.local")
        .expect("header")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.body_text().await.expect("body");
    assert_eq!(body, "api-data", "api.local should route to api_router");

    // Request to web.local
    let resp = client
        .get(&format!("http://{addr}/"))
        .expect("GET")
        .header("host", "web.local")
        .expect("header")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.body_text().await.expect("body");
    assert_eq!(body, "web-home", "web.local should route to web_router");
}

/// `Display for Router` produces non-empty output listing registered routes.
#[tokio::test]
async fn test_router_display() {
    let api_router = oxihttp_server::Router::new().get("/v1", |_req| async {
        oxihttp_server::response::text_response("v1")
    });

    let router = oxihttp_server::Router::new()
        .get("/hello", |_req| async {
            oxihttp_server::response::text_response("hello")
        })
        .post("/echo", |_req| async {
            oxihttp_server::response::text_response("echo")
        })
        .host("api.example.com", api_router)
        .nest(
            "/api",
            oxihttp_server::Router::new().get("/ping", |_req| async {
                oxihttp_server::response::text_response("pong")
            }),
        );

    let display = format!("{router}");
    assert!(!display.is_empty(), "Display output should not be empty");
    assert!(display.contains("GET"), "should mention GET method");
    assert!(display.contains("POST"), "should mention POST method");
    assert!(display.contains("api.example.com"), "should mention vhost");
    assert!(display.contains("nested"), "should mention nested prefix");
}

// ---------------------------------------------------------------------------
// New tests (M9 Block D)
// ---------------------------------------------------------------------------

/// GET handler returns a JSON response with correct Content-Type and body.
#[tokio::test]
async fn test_server_get_json_response() {
    let router = oxihttp_server::Router::new().get("/api/status", |_req| async {
        oxihttp_server::response::json_response(&serde_json::json!({"status": "ok", "version": 1}))
    });

    let (addr, _shutdown) = spawn_test_server(router).await;

    let client = oxihttp_client::Client::builder()
        .build()
        .expect("client build");
    let url = format!("http://{addr}/api/status");
    let resp = client
        .get(&url)
        .expect("GET builder")
        .send()
        .await
        .expect("send");

    assert_eq!(resp.status(), http::StatusCode::OK, "expected 200 OK");

    let ct = resp.content_type().expect("Content-Type header present");
    assert!(
        ct.contains("application/json"),
        "Content-Type should contain application/json, got: {ct}"
    );

    let body: serde_json::Value = resp.body_json().await.expect("JSON body parse");
    assert_eq!(body["status"], "ok", "status field mismatch");
    assert_eq!(body["version"], 1, "version field mismatch");
}

/// Graceful shutdown fires while a request is in-flight; the already-spawned
/// connection handler must complete the response before the test exits.
///
/// The key invariant: a request that has been accepted and is being processed
/// at the time the shutdown signal fires must still deliver a valid response —
/// the hyper connection task was spawned before shutdown and runs to completion
/// independently of the accept loop lifecycle.
#[tokio::test]
async fn test_graceful_shutdown_drains_active_requests() {
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let router = oxihttp_server::Router::new().get("/slow", |_req| async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        oxihttp_server::response::text_response("done")
    });

    let (addr, server_handle) = oxihttp_server::Server::bind("127.0.0.1:0")
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        })
        .serve_with_addr(router)
        .await
        .expect("server bind");

    tokio::time::sleep(Duration::from_millis(10)).await;

    // Spawn a client task that starts a slow request but doesn't await it yet.
    let client_task = tokio::spawn(async move {
        let client = oxihttp_client::Client::builder()
            .build()
            .expect("client build");
        let url = format!("http://{addr}/slow");
        client.get(&url).expect("GET builder").send().await
    });

    // Let the request get in-flight before we fire shutdown.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Fire the shutdown signal.  The outer server task will exit (select! arm).
    let _ = shutdown_tx.send(());
    let _ = server_handle.await;

    // The in-flight request should still complete because the hyper connection
    // handler task was already spawned before shutdown fired.
    let resp = client_task
        .await
        .expect("client task join")
        .expect("response");
    assert_eq!(
        resp.status(),
        http::StatusCode::OK,
        "in-flight request should complete"
    );
    let body = resp.body_text().await.expect("body text");
    assert_eq!(body, "done", "response body mismatch");
}

/// A server configured with `with_max_connections(1)` drops the second
/// connection when the first is still active (semaphore exhausted).
#[tokio::test]
async fn test_max_connections_rejects_excess() {
    let router = oxihttp_server::Router::new().get("/hold", |_req| async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        oxihttp_server::response::text_response("held")
    });

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let (addr, _server_handle) = oxihttp_server::Server::bind("127.0.0.1:0")
        .with_max_connections(1)
        .with_graceful_shutdown(async move {
            let _ = rx.await;
        })
        .serve_with_addr(router)
        .await
        .expect("server bind");

    tokio::time::sleep(Duration::from_millis(10)).await;

    // Start the first (slow) request but don't await it yet.
    let first_task = tokio::spawn(async move {
        let client = oxihttp_client::Client::builder().build().expect("client");
        let url = format!("http://{addr}/hold");
        client.get(&url).expect("GET builder").send().await
    });

    // Let the first request get in-flight so the semaphore is held.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The second request should fail — the accept loop drops the connection
    // when the semaphore is exhausted (try_acquire_owned returns Err).
    let second_result = {
        let client = oxihttp_client::Client::builder().build().expect("client2");
        let url = format!("http://{addr}/hold");
        client.get(&url).expect("GET builder").send().await
    };
    assert!(
        second_result.is_err(),
        "second request should fail when max_connections=1 is exhausted"
    );

    // The first request should eventually complete successfully.
    let first_resp = first_task
        .await
        .expect("first task join")
        .expect("first response");
    assert_eq!(
        first_resp.status(),
        http::StatusCode::OK,
        "first request should succeed"
    );

    // Clean up server.
    let _ = tx.send(());
}
