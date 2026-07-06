//! Article fetching for the Reading module (issue #61, docs/MODULES.md
//! §3.6). Unlike Kite/WhatsApp/browser-use, fetching a public URL needs no
//! owned credentials - so this client is always wired in production
//! (`reading_from_config` ignores `config` and always returns `Some`);
//! tests inject `reading::mock::MockArticleFetcher` instead of hitting the
//! network, following the same trait+mock pattern as every other external
//! client in this crate (`kite.rs`, `whatsapp.rs`, `browser.rs`).
//!
//! Full Mozilla Readability.js extraction (the vendored `external/readability`
//! submodule, run via a Node/jsdom subprocess) is deferred - `parse_article`
//! in `routes/reading.rs` does a lighter, dependency-light HTML→text
//! extraction with the `scraper` crate today. See docs/MODULES.md §3.6 for
//! the explicit scope note.

use crate::error::{ApiError, ApiResult};
use async_trait::async_trait;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Maximum redirect hops followed manually - each hop is SSRF-revalidated
/// before it is touched (finding 4).
const MAX_REDIRECTS: usize = 5;

/// Per-request timeout for article fetches - also bounds how long a probe to an
/// (accidentally reachable) internal host can hang.
const FETCH_TIMEOUT_SECS: u64 = 30;

#[async_trait]
pub trait ArticleFetcher: Send + Sync {
    /// Fetches `url` and returns the raw response body (HTML).
    async fn fetch(&self, url: &str) -> ApiResult<String>;
}

pub struct HttpArticleFetcher {
    http: reqwest::Client,
}

impl HttpArticleFetcher {
    pub fn new() -> Self {
        // Auto-redirect is DISABLED (finding 4): reqwest would otherwise follow
        // a `Location: http://169.254.169.254/...` hop without re-checking it.
        // We follow manually below, SSRF-validating each hop.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
            .build()
            .expect("failed to build reqwest client for article fetching");
        Self { http }
    }
}

impl Default for HttpArticleFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ArticleFetcher for HttpArticleFetcher {
    async fn fetch(&self, url: &str) -> ApiResult<String> {
        let mut current = url.to_string();
        for _ in 0..=MAX_REDIRECTS {
            // Validate BEFORE every network touch, including each redirect hop.
            assert_public_url(&current)?;
            let resp = self.http.get(&current).send().await.map_err(|e| {
                tracing::error!("article fetch failed for {current}: {e}");
                ApiError::Upstream("article fetch failed".into())
            })?;
            let status = resp.status();
            if status.is_redirection() {
                let location = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| ApiError::Upstream("redirect without a Location header".into()))?;
                // Resolve relative redirects against the current URL, then loop
                // so the next hop is SSRF-revalidated before we reach out.
                let base = reqwest::Url::parse(&current)
                    .map_err(|_| ApiError::Upstream("invalid redirect base url".into()))?;
                let next = base
                    .join(location)
                    .map_err(|_| ApiError::Upstream("invalid redirect target".into()))?;
                current = next.to_string();
                continue;
            }
            if !status.is_success() {
                return Err(ApiError::Upstream(format!("article fetch returned {status}")));
            }
            return resp.text().await.map_err(|e| {
                tracing::error!("article body decode failed for {current}: {e}");
                ApiError::Upstream("malformed article response".into())
            });
        }
        Err(ApiError::Upstream("too many redirects while fetching article".into()))
    }
}

/// SSRF guard (finding 4): reject any URL that is not safe to fetch
/// server-side. Blocks non-http(s) schemes and any host that resolves to a
/// loopback, private (RFC1918 / RFC6598 CGNAT), link-local (incl.
/// `169.254.169.254` cloud metadata), unique-local, multicast, or unspecified
/// address. Uses the OS resolver in production; the injectable variant below
/// keeps the unit tests hermetic.
fn assert_public_url(url: &str) -> ApiResult<()> {
    assert_public_url_with(url, &system_resolve)
}

/// Default resolver: real DNS via the OS (`ToSocketAddrs`).
fn system_resolve(host: &str, port: u16) -> std::io::Result<Vec<IpAddr>> {
    use std::net::ToSocketAddrs;
    Ok((host, port).to_socket_addrs()?.map(|sa| sa.ip()).collect())
}

/// Core of [`assert_public_url`] with an injected resolver so tests never touch
/// real DNS. An IP-literal host is checked directly (no resolver), so an
/// attacker cannot smuggle an internal address past DNS by writing it out; a
/// domain is resolved and EVERY returned address must be public (fail-closed).
fn assert_public_url_with(
    url: &str,
    resolve: &dyn Fn(&str, u16) -> std::io::Result<Vec<IpAddr>>,
) -> ApiResult<()> {
    let parsed = reqwest::Url::parse(url).map_err(|_| ApiError::BadRequest("invalid url".into()))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(ApiError::BadRequest("only http(s) urls may be fetched".into()));
    }
    let raw_host = parsed
        .host_str()
        .ok_or_else(|| ApiError::BadRequest("url has no host".into()))?;
    // `host_str()` brackets IPv6 literals (`[::1]`); strip them before parsing.
    let host = raw_host.trim_start_matches('[').trim_end_matches(']');
    let port = parsed.port_or_known_default().unwrap_or(0);

    if let Ok(ip) = host.parse::<IpAddr>() {
        return guard_ip(&ip);
    }

    let ips = resolve(host, port).map_err(|e| {
        tracing::warn!("could not resolve host {host}: {e}");
        ApiError::BadRequest("could not resolve host".into())
    })?;
    if ips.is_empty() {
        return Err(ApiError::BadRequest("host did not resolve to any address".into()));
    }
    for ip in &ips {
        guard_ip(ip)?;
    }
    Ok(())
}

fn guard_ip(ip: &IpAddr) -> ApiResult<()> {
    if is_public_ip(ip) {
        Ok(())
    } else {
        Err(ApiError::BadRequest("url resolves to a non-public address".into()))
    }
}

/// Whether `ip` is a globally-routable public address safe to fetch from.
fn is_public_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        // `::ffff:a.b.c.d` must be judged as the embedded v4 address, or a
        // mapped `169.254.169.254` would slip past the v6-only checks.
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public_v4(&v4),
            None => is_public_v6(v6),
        },
    }
}

fn is_public_v4(v4: &Ipv4Addr) -> bool {
    // 100.64.0.0/10 - RFC 6598 carrier-grade NAT shared address space.
    let octets = v4.octets();
    let is_cgnat = octets[0] == 100 && (0x40..0x80).contains(&octets[1]);
    !(v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_broadcast()
        || v4.is_documentation()
        || v4.is_unspecified()
        || v4.is_multicast()
        || is_cgnat)
}

fn is_public_v6(v6: &Ipv6Addr) -> bool {
    !(v6.is_loopback()
        || v6.is_unspecified()
        || v6.is_multicast()
        || v6.is_unique_local()
        || v6.is_unicast_link_local())
}

/// In-memory fake for tests - no real network needed to exercise the API
/// surface.
pub mod mock {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct MockArticleFetcher {
        pages: Mutex<HashMap<String, String>>,
        /// Every URL fetched, in order.
        pub calls: Mutex<Vec<String>>,
    }

    impl MockArticleFetcher {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn seed(&self, url: &str, html: &str) {
            self.pages.lock().unwrap().insert(url.to_string(), html.to_string());
        }
    }

    #[async_trait]
    impl ArticleFetcher for MockArticleFetcher {
        async fn fetch(&self, url: &str) -> ApiResult<String> {
            self.calls.lock().unwrap().push(url.to_string());
            self.pages.lock().unwrap().get(url).cloned().ok_or_else(|| ApiError::NotFound(format!("no mock page seeded for {url}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stub resolver returning a fixed answer regardless of host - lets the
    /// SSRF guard be exercised without any real DNS.
    fn resolver_to(ips: Vec<IpAddr>) -> impl Fn(&str, u16) -> std::io::Result<Vec<IpAddr>> {
        move |_host, _port| Ok(ips.clone())
    }

    #[test]
    fn rejects_localhost_ip_literal() {
        assert!(assert_public_url_with("http://127.0.0.1/x", &resolver_to(vec![])).is_err());
    }

    #[test]
    fn rejects_cloud_metadata_ip_literal() {
        // 169.254.169.254 is link-local: the AWS/GCP/Azure instance-metadata IP.
        assert!(assert_public_url_with(
            "http://169.254.169.254/latest/meta-data/",
            &resolver_to(vec![])
        )
        .is_err());
    }

    #[test]
    fn rejects_private_ip_literals() {
        for url in ["http://10.0.0.5/", "http://192.168.1.1/", "http://172.16.5.4/"] {
            assert!(
                assert_public_url_with(url, &resolver_to(vec![])).is_err(),
                "{url} should be blocked"
            );
        }
    }

    #[test]
    fn rejects_ipv6_loopback_literal() {
        assert!(assert_public_url_with("http://[::1]/", &resolver_to(vec![])).is_err());
    }

    #[test]
    fn rejects_ipv4_mapped_ipv6_metadata() {
        assert!(assert_public_url_with(
            "http://[::ffff:169.254.169.254]/",
            &resolver_to(vec![])
        )
        .is_err());
    }

    #[test]
    fn rejects_non_http_schemes() {
        assert!(assert_public_url_with("file:///etc/passwd", &resolver_to(vec![])).is_err());
        assert!(assert_public_url_with("ftp://example.com/x", &resolver_to(vec![])).is_err());
        assert!(assert_public_url_with("gopher://example.com/", &resolver_to(vec![])).is_err());
    }

    #[test]
    fn rejects_domain_resolving_to_link_local_metadata() {
        // The classic DNS-rebinding SSRF: a public-looking hostname whose DNS
        // answer is the metadata IP. Must be blocked on the resolved address.
        let r = resolver_to(vec!["169.254.169.254".parse().unwrap()]);
        assert!(assert_public_url_with("http://metadata.evil.example/", &r).is_err());
    }

    #[test]
    fn rejects_domain_resolving_to_private() {
        let r = resolver_to(vec!["10.1.2.3".parse().unwrap()]);
        assert!(assert_public_url_with("http://internal.evil.example/", &r).is_err());
    }

    #[test]
    fn rejects_when_any_resolved_ip_is_internal() {
        // Even one internal address in a multi-record answer must fail closed.
        let r = resolver_to(vec![
            "93.184.216.34".parse().unwrap(),
            "127.0.0.1".parse().unwrap(),
        ]);
        assert!(assert_public_url_with("http://mixed.evil.example/", &r).is_err());
    }

    #[test]
    fn allows_public_host() {
        let r = resolver_to(vec!["93.184.216.34".parse().unwrap()]);
        assert!(assert_public_url_with("https://example.com/article", &r).is_ok());
    }

    #[test]
    fn allows_public_ip_literal_without_resolving() {
        // Public IP literal passes without consulting the resolver at all.
        assert!(assert_public_url_with("http://93.184.216.34/", &resolver_to(vec![])).is_ok());
    }
}
