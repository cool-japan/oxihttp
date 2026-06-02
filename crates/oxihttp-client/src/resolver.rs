//! Custom DNS resolver support for oxihttp clients.
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use hyper_util::client::legacy::connect::dns::Name;
use oxihttp_core::OxiHttpError;
use tower_service::Service;

/// A pluggable async DNS resolver.
///
/// Implement this trait to override the default system resolver (getaddrinfo).
pub trait DnsResolver: Send + Sync + 'static {
    /// Resolve `name` to a list of socket addresses.
    fn resolve(
        &self,
        name: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, OxiHttpError>> + Send>>;
}

/// Default DNS resolver using `tokio::net::lookup_host` (wraps getaddrinfo).
#[derive(Debug, Clone, Default)]
pub struct GaiDnsResolver;

impl DnsResolver for GaiDnsResolver {
    fn resolve(
        &self,
        name: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, OxiHttpError>> + Send>> {
        let host = format!("{name}:0");
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host(host)
                .await
                .map_err(OxiHttpError::from)?
                .collect();
            Ok(addrs)
        })
    }
}

/// Adapts an `Arc<dyn DnsResolver>` into a `tower_service::Service<Name>` that hyper-util's
/// `HttpConnector` accepts. This is the bridge between our trait and hyper-util internals.
///
/// This is a public type because it appears in the `ResolverClient` and `ResolverHttpsClient`
/// type aliases, but it is not intended for direct use by library consumers.
#[derive(Clone)]
pub struct BoxResolver(pub(crate) std::sync::Arc<dyn DnsResolver>);

/// Iterator adapter for the resolved addresses.
///
/// Part of the public interface through `BoxResolver`'s `Service` impl.
pub struct BoxResolverAddrs(std::vec::IntoIter<SocketAddr>);

impl Iterator for BoxResolverAddrs {
    type Item = SocketAddr;
    fn next(&mut self) -> Option<SocketAddr> {
        self.0.next()
    }
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

impl Service<Name> for BoxResolver {
    type Response = BoxResolverAddrs;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, name: Name) -> Self::Future {
        let resolver = self.0.clone();
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs = resolver
                .resolve(&host)
                .await
                .map_err(|e| Box::new(e) as BoxError)?;
            Ok(BoxResolverAddrs(addrs.into_iter()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    struct FixedResolver(Vec<SocketAddr>);
    impl DnsResolver for FixedResolver {
        fn resolve(
            &self,
            _name: &str,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, OxiHttpError>> + Send>> {
            let addrs = self.0.clone();
            Box::pin(async move { Ok(addrs) })
        }
    }

    struct EmptyResolver;
    impl DnsResolver for EmptyResolver {
        fn resolve(
            &self,
            name: &str,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, OxiHttpError>> + Send>> {
            let name = name.to_owned();
            Box::pin(async move { Err(OxiHttpError::Dns(format!("no address for {name}"))) })
        }
    }

    #[tokio::test]
    async fn test_gai_resolver() {
        let r = GaiDnsResolver;
        let addrs = r.resolve("localhost").await.expect("resolve");
        assert!(!addrs.is_empty());
    }

    #[tokio::test]
    async fn test_fixed_resolver_builds_client() {
        let resolver = FixedResolver(vec!["127.0.0.1:0".parse().expect("addr")]);
        let _client = crate::ClientBuilder::new()
            .with_resolver(resolver)
            .build_with_resolver()
            .expect("build");
    }

    #[tokio::test]
    async fn test_empty_resolver_error() {
        let r = EmptyResolver;
        let result = r.resolve("nonexistent.example").await;
        assert!(result.is_err());
        let err = result.expect_err("should be error");
        assert!(matches!(err, OxiHttpError::Dns(_)));
    }
}
