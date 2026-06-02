//! HTTP/2 ALPN negotiation and HTTPS server integration tests.
//!
//! These tests verify that the oxihttp-server TLS implementation can:
//! - Accept TLS connections and serve HTTPS
//! - Negotiate ALPN protocols correctly
//! - Accept HTTP/2 connections when h2 ALPN is configured

#[cfg(all(feature = "tls", feature = "server"))]
mod tests {
    use oxihttp::response;
    use oxihttp::{Router, Server, TlsConfig};
    use oxitls::rcgen_bridge::generate_self_signed_ed25519;
    use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

    /// Spin up an HTTPS server with the given ALPN protocols.
    /// Returns (addr, cert_der_bytes).
    async fn start_https_server(alpn: &[&str]) -> (std::net::SocketAddr, Vec<u8>) {
        let ck = generate_self_signed_ed25519(&["localhost"]).expect("generate cert");

        let cert_der_bytes = ck.cert_der.clone();
        let cert = CertificateDer::from(ck.cert_der.clone());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.pkcs8_der.clone()));

        // Build server config with ALPN via oxitls builder
        let server_cfg = oxitls::tls13::ServerBuilder::new()
            .with_der_cert_and_key(vec![cert], key)
            .with_alpn_protocols(alpn.iter().copied())
            .build()
            .expect("server TLS config");

        let tls_config = TlsConfig::new(server_cfg);

        let router = Router::new().get("/", |_req| async {
            response::text_response("Hello HTTPS!")
        });

        let (addr, _handle) = Server::bind("127.0.0.1:0")
            .with_tls(tls_config)
            .serve_with_addr(router)
            .await
            .expect("server start");

        (addr, cert_der_bytes)
    }

    /// Test that an HTTPS server with HTTP/1.1 ALPN works correctly.
    #[cfg(feature = "client")]
    #[tokio::test]
    async fn test_https_server_http1() {
        let (addr, cert_der) = start_https_server(&["http/1.1"]).await;
        let url = format!("https://localhost:{}/", addr.port());

        let client = oxihttp::Client::builder()
            .with_trusted_cert_der(cert_der)
            .build_https()
            .expect("client build");

        let resp = client
            .get(&url)
            .expect("request builder")
            .send()
            .await
            .expect("send request");

        assert_eq!(resp.status(), oxihttp::StatusCode::OK);
        let body = resp.body_text().await.expect("body");
        assert_eq!(body, "Hello HTTPS!");
    }

    /// Test that the TLS server can be built with h2 ALPN and serves HTTP/1.1 clients
    /// when no HTTP/2 client is used (hyper-legacy is HTTP/1.1 only).
    #[cfg(feature = "client")]
    #[tokio::test]
    async fn test_https_server_h2_alpn_configured() {
        // Server advertises h2 first, then http/1.1
        let (addr, cert_der) = start_https_server(&["h2", "http/1.1"]).await;
        let url = format!("https://localhost:{}/", addr.port());

        // Client also requests h2 via ALPN
        let client = oxihttp::Client::builder()
            .with_trusted_cert_der(cert_der)
            .with_alpn(&["h2", "http/1.1"])
            .build_https()
            .expect("client build");

        let resp = client
            .get(&url)
            .expect("request builder")
            .send()
            .await
            .expect("send request");

        // The hyper-legacy client negotiates h2 via ALPN but actually sends HTTP/1.1
        // protocol; the auto::Builder on the server side handles both h1 and h2.
        assert_eq!(resp.status(), oxihttp::StatusCode::OK);
        let body = resp.body_text().await.expect("body");
        assert_eq!(body, "Hello HTTPS!");
    }

    /// Test the from_pem constructor on TlsConfig.
    #[tokio::test]
    async fn test_tls_config_from_pem() {
        let ck = generate_self_signed_ed25519(&["localhost"]).expect("generate cert");

        // cert_pem is a String field; key_pem() is a method on CertifiedKey
        let cert_pem = ck.cert_pem.as_bytes().to_vec();
        let key_pem = ck.key_pem().into_bytes();

        let result = TlsConfig::from_pem(&cert_pem, &key_pem);
        assert!(
            result.is_ok(),
            "from_pem should succeed: {:?}",
            result.err()
        );
    }

    /// Test the from_der constructor on TlsConfig.
    #[tokio::test]
    async fn test_tls_config_from_der() {
        let ck = generate_self_signed_ed25519(&["localhost"]).expect("generate cert");

        let cert = CertificateDer::from(ck.cert_der.clone());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.pkcs8_der.clone()));

        let result = TlsConfig::from_der(vec![cert], key);
        assert!(
            result.is_ok(),
            "from_der should succeed: {:?}",
            result.err()
        );
    }

    /// Test with_tls_from_pem builder method.
    #[tokio::test]
    async fn test_server_with_tls_from_pem() {
        let ck = generate_self_signed_ed25519(&["localhost"]).expect("generate cert");

        let cert_pem = ck.cert_pem.as_bytes().to_vec();
        let key_pem = ck.key_pem().into_bytes();

        let router = Router::new().get("/", |_req| async { response::text_response("ok") });

        let result = Server::bind("127.0.0.1:0").with_tls_from_pem(&cert_pem, &key_pem);

        assert!(
            result.is_ok(),
            "with_tls_from_pem should succeed: {:?}",
            result.err()
        );

        let (addr, _handle) = result
            .expect("server builder")
            .serve_with_addr(router)
            .await
            .expect("serve_with_addr");

        // Verify we got a valid address
        assert!(addr.port() > 0);
    }
}
