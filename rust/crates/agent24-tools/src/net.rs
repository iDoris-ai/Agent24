//! `http_fetch` builtin with SSRF protection.
//!
//! Guard rails:
//! - http/https only, GET/HEAD only
//! - the target host is resolved FIRST; every resolved address must be public
//!   (loopback, RFC1918, link-local/cloud-metadata, CGNAT, ULA … all denied)
//! - the connection is then PINNED to those checked addresses
//!   (`ClientBuilder::resolve_to_addrs`) so a DNS rebind between check and
//!   connect cannot redirect the request
//! - redirects are NOT followed (a redirect to an internal address would
//!   bypass the check); the redirect status + Location are returned instead
//! - response body capped at 256 KiB

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;
use std::{future::Future, io};

use agent24_protocol::{RiskClass, ToolInfo};
use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;
use url::{Host, Url};

use crate::{Tool, ToolContext, ToolError, truncate};
use agent24_domain::{EgressDestination, EgressPurpose};

const MAX_BODY_BYTES: usize = 256 * 1024;

pub struct HttpFetchTool {
    /// Allow loopback/private targets — for tests and explicit dev setups
    /// only; the daemon registers this as `false`.
    allow_local: bool,
}

impl HttpFetchTool {
    pub fn new(allow_local: bool) -> Self {
        Self { allow_local }
    }
}

fn ipv4_is_public(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(o[0] == 0 // 0.0.0.0/8 "this network"
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local() // includes 169.254.169.254 metadata
        || (o[0] == 100 && (o[1] & 0b1100_0000) == 64) // 100.64.0.0/10 CGNAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24 IETF special
        || (o[0] == 198 && (o[1] & 0b1111_1110) == 18) // 198.18.0.0/15 benchmarking
        || ip.is_documentation()
        || ip.is_broadcast()
        || ip.is_multicast()
        || o[0] >= 240) // 240.0.0.0/4 reserved (broadcast already above)
}

/// IPv6 must decode every embedded-IPv4 transition scheme and re-check the
/// inner address — otherwise e.g. NAT64 `64:ff9b::a9fe:a9fe` reaches the
/// 169.254.169.254 metadata service while looking like a "public" v6 address.
fn ipv6_is_public(ip: Ipv6Addr) -> bool {
    let seg = ip.segments();
    // v4-mapped ::ffff:a.b.c.d
    if let Some(v4) = ip.to_ipv4_mapped() {
        return ipv4_is_public(v4);
    }
    // v4-compatible ::a.b.c.d (deprecated) — everything else in ::/96 is
    // covered by the unspecified/loopback checks below
    if seg[..5] == [0, 0, 0, 0, 0] && seg[5] == 0 && (seg[6] != 0 || seg[7] > 1) {
        return ipv4_is_public(Ipv4Addr::from(((seg[6] as u32) << 16) | seg[7] as u32));
    }
    // NAT64: the well-known 64:ff9b::/96 embeds v4 in the last 32 bits;
    // local-use 64:ff9b:1::/48 allows variable embedding (RFC 6052), so it is
    // denied outright rather than decoded
    if seg[0] == 0x64 && seg[1] == 0xff9b {
        if seg[2] == 1 {
            return false;
        }
        if seg[2..6] == [0, 0, 0, 0] {
            return ipv4_is_public(Ipv4Addr::from(((seg[6] as u32) << 16) | seg[7] as u32));
        }
    }
    // 6to4 2002:AABB:CCDD::/48 embeds v4 in segments 1-2
    if seg[0] == 0x2002 {
        return ipv4_is_public(Ipv4Addr::from(((seg[1] as u32) << 16) | seg[2] as u32));
    }
    // Teredo 2001:0::/32 embeds the server v4 in segs 2-3 and the client v4
    // XOR ffff in segs 6-7 — both must be public
    if seg[0] == 0x2001 && seg[1] == 0 {
        let server = Ipv4Addr::from(((seg[2] as u32) << 16) | seg[3] as u32);
        let client = Ipv4Addr::from((((seg[6] ^ 0xffff) as u32) << 16) | (seg[7] ^ 0xffff) as u32);
        return ipv4_is_public(server) && ipv4_is_public(client);
    }
    // 2001:db8::/32 documentation
    if seg[0] == 0x2001 && seg[1] == 0x0db8 {
        return false;
    }
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local())
}

fn ip_is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => ipv4_is_public(v4),
        IpAddr::V6(v6) => ipv6_is_public(v6),
    }
}

fn str_arg<'a>(input: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    input.get(key).and_then(Value::as_str)
}

async fn resolve_domain_with_egress<F, Fut>(
    ctx: &ToolContext,
    destination: EgressDestination,
    resolve: F,
) -> Result<Vec<SocketAddr>, ToolError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = io::Result<Vec<SocketAddr>>>,
{
    ctx.authorize_egress_if_restricted(EgressPurpose::HttpFetch, destination)
        .await?;
    // Kept in the same helper as the resolver so tests can verify that a
    // denied request never invokes DNS at all.
    let addrs = resolve()
        .await
        .map_err(|e| ToolError::Failed(format!("dns lookup failed: {e}")))?;
    if addrs.is_empty() {
        return Err(ToolError::Failed("domain resolved to nothing".into()));
    }
    Ok(addrs)
}

#[async_trait]
impl Tool for HttpFetchTool {
    fn requires_outbound_policy(&self) -> bool {
        // Egress authorization is conditional on this run's source resources.
        false
    }

    fn info(&self) -> ToolInfo {
        // RiskClass::Read, deliberately — a GET/HEAD changes nothing off the
        // machine, so it is not `External`. What makes it dangerous is
        // exfiltration, which is a taint-propagation concern; filing it under
        // `External` would make it eligible for target-scoped standing grants
        // (H4), which is the wrong semantics for a tool whose "target" is an
        // arbitrary URL.
        ToolInfo::new(
            "http_fetch",
            "builtin",
            "Fetch a public http(s) URL (GET/HEAD). Returns status and body \
             (truncated at 256 KiB). Private/internal addresses are blocked; \
             redirects are returned, not followed.",
            RiskClass::Read,
        )
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "Absolute http(s) URL" },
                "method": { "type": "string", "enum": ["GET", "HEAD"], "default": "GET" }
            },
            "required": ["url"],
            "additionalProperties": false
        })
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(30)
    }

    async fn call(
        &self,
        ctx: &ToolContext,
        input: &Map<String, Value>,
        cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        let raw_url =
            str_arg(input, "url").ok_or_else(|| ToolError::Invalid("url is required".into()))?;
        let method = match str_arg(input, "method").unwrap_or("GET") {
            "GET" => reqwest::Method::GET,
            "HEAD" => reqwest::Method::HEAD,
            other => {
                return Err(ToolError::Invalid(format!(
                    "method {other} not allowed (GET/HEAD only)"
                )));
            }
        };
        let url =
            Url::parse(raw_url).map_err(|e| ToolError::Invalid(format!("invalid url: {e}")))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(ToolError::Invalid(format!(
                "scheme {} not allowed (http/https only)",
                url.scheme()
            )));
        }
        let host = url
            .host()
            .ok_or_else(|| ToolError::Invalid("url has no host".into()))?
            .to_owned();
        let port = url
            .port_or_known_default()
            .ok_or_else(|| ToolError::Invalid("url has no port".into()))?;

        // Resolve-then-pin: every address the name resolves to must be public,
        // and the actual connection is restricted to exactly those addresses.
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(25));
        match &host {
            Host::Ipv4(ip) => {
                if !self.allow_local && !ipv4_is_public(*ip) {
                    return Err(ToolError::Denied(format!(
                        "address {ip} is not public (SSRF guard)"
                    )));
                }
            }
            Host::Ipv6(ip) => {
                if !self.allow_local && !ipv6_is_public(*ip) {
                    return Err(ToolError::Denied(format!(
                        "address {ip} is not public (SSRF guard)"
                    )));
                }
            }
            Host::Domain(domain) => {
                let destination =
                    EgressDestination::exact(format!("{}://{}:{}", url.scheme(), host, port));
                let addrs = resolve_domain_with_egress(ctx, destination, || async {
                    tokio::select! {
                        r = tokio::net::lookup_host((domain.as_str(), port)) => {
                            r.map(Iterator::collect)
                        },
                        () = cancel.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled")),
                    }
                })
                .await
                .map_err(|err| {
                    if cancel.is_cancelled() {
                        ToolError::Cancelled
                    } else {
                        err
                    }
                })?;
                if !self.allow_local
                    && let Some(bad) = addrs.iter().find(|a| !ip_is_public(a.ip()))
                {
                    return Err(ToolError::Denied(format!(
                        "{domain} resolves to non-public address {} (SSRF guard)",
                        bad.ip()
                    )));
                }
                builder = builder.resolve_to_addrs(domain, &addrs);
            }
        }

        let client = builder
            .build()
            .map_err(|e| ToolError::Failed(format!("http client init: {e}")))?;
        if !matches!(host, Host::Domain(_)) {
            ctx.authorize_egress_if_restricted(
                EgressPurpose::HttpFetch,
                EgressDestination::exact(format!("{}://{}:{}", url.scheme(), host, port)),
            )
            .await?;
        }
        let response = tokio::select! {
            r = client.request(method, url.clone()).send() => {
                r.map_err(|e| ToolError::Failed(format!("request failed: {e}")))?
            }
            () = cancel.cancelled() => return Err(ToolError::Cancelled),
        };

        let status = response.status().as_u16();
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        // Stream the body with a hard cap — a huge (or endless) response must
        // not exhaust memory.
        let mut body = Vec::new();
        let mut truncated = false;
        let mut stream = response;
        loop {
            let chunk = tokio::select! {
                c = stream.chunk() => c.map_err(|e| ToolError::Failed(format!("read body: {e}")))?,
                () = cancel.cancelled() => return Err(ToolError::Cancelled),
            };
            let Some(chunk) = chunk else { break };
            if chunk.is_empty() {
                continue;
            }
            // truncated only when bytes were actually dropped — an exact-fit
            // final chunk is not a truncation
            if body.len() + chunk.len() > MAX_BODY_BYTES {
                let room = MAX_BODY_BYTES - body.len();
                body.extend_from_slice(&chunk[..room]);
                truncated = true;
                break;
            }
            body.extend_from_slice(&chunk);
        }

        let mut out = serde_json::json!({
            "status": status,
            "body": truncate(&String::from_utf8_lossy(&body), MAX_BODY_BYTES),
            "truncated": truncated,
        });
        if let Some(loc) = location {
            out["location"] = Value::String(loc);
        }
        Ok(out.to_string())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_domain::{EgressDecision, EgressGate, EgressRequest, EgressResource};
    use async_trait::async_trait;
    use std::sync::Arc;

    struct FixtureGrant;
    #[async_trait]
    impl EgressGate for FixtureGrant {
        async fn check(&self, _request: &EgressRequest) -> Result<(), EgressDecision> {
            Ok(())
        }
    }

    struct FixtureDeny;
    #[async_trait]
    impl EgressGate for FixtureDeny {
        async fn check(&self, _request: &EgressRequest) -> Result<(), EgressDecision> {
            Err(EgressDecision)
        }
    }

    fn input(url: &str) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("url".to_owned(), Value::String(url.to_owned()));
        m
    }

    fn ctx() -> ToolContext {
        ToolContext::legacy("run_test", None, None, "tc_test").with_egress_policy(
            Arc::new(FixtureGrant),
            vec![
                EgressResource::cloud_authorized("fixture", "fixture-rev", 1)
                    .with_authorization_ref("fixture-grant"),
            ],
            1,
        )
    }

    fn denied_ctx() -> ToolContext {
        ToolContext::legacy("run_test", None, None, "tc_test").with_egress_policy(
            Arc::new(FixtureDeny),
            vec![
                EgressResource::cloud_authorized("fixture", "fixture-rev", 1)
                    .with_authorization_ref("fixture-grant"),
            ],
            1,
        )
    }

    #[tokio::test]
    async fn denied_fetch_does_not_invoke_dns_resolver() {
        let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let resolver_calls = Arc::clone(&lookups);
        let result = resolve_domain_with_egress(
            &denied_ctx(),
            EgressDestination::exact("https://encoded.attacker.invalid:443"),
            move || async move {
                resolver_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(vec!["203.0.113.10:443".parse().unwrap()])
            },
        )
        .await;

        assert!(matches!(result, Err(ToolError::Denied(_))), "{result:?}");
        assert_eq!(lookups.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn ordinary_fetch_does_not_require_egress_policy_before_dns() {
        let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let resolver_calls = Arc::clone(&lookups);
        let result = resolve_domain_with_egress(
            &ToolContext::legacy("run_test", None, None, "tc_test"),
            EgressDestination::exact("https://example.invalid:443"),
            || async move {
                resolver_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(vec![SocketAddr::from(([93, 184, 216, 34], 443))])
            },
        )
        .await
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(lookups.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn public_ip_classification() {
        for bad in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254", // cloud metadata
            "100.64.0.1",      // CGNAT
            "0.0.0.0",
            "::1",
            "fe80::1",
            "fc00::1",
            "::ffff:127.0.0.1",           // v4-mapped loopback
            "64:ff9b::a9fe:a9fe",         // NAT64 → 169.254.169.254 metadata
            "64:ff9b:1::a00:1",           // NAT64 local-use → 10.0.0.1
            "2002:7f00:1::1",             // 6to4 → 127.0.0.1
            "2001:0:100:0:0:0:5601:5601", // Teredo client XOR ffff → 169.254.169.254
            "2001:db8::1",                // documentation
            "0.1.2.3",                    // 0.0.0.0/8
            "198.18.0.1",                 // benchmarking
            "240.0.0.1",                  // reserved
            "192.0.0.8",                  // IETF special
        ] {
            let ip: IpAddr = bad.parse().unwrap();
            assert!(!ip_is_public(ip), "{bad} must be non-public");
        }
        for good in ["1.1.1.1", "93.184.216.34", "2606:4700:4700::1111"] {
            let ip: IpAddr = good.parse().unwrap();
            assert!(ip_is_public(ip), "{good} must be public");
        }
    }

    #[tokio::test]
    async fn private_targets_are_denied() {
        let tool = HttpFetchTool::new(false);
        let cancel = CancellationToken::new();
        for url in [
            "http://127.0.0.1:8080/x",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/",
        ] {
            let err = tool.call(&ctx(), &input(url), &cancel).await.unwrap_err();
            assert!(matches!(err, ToolError::Denied(_)), "{url}: {err}");
        }
    }

    #[tokio::test]
    async fn bad_scheme_and_method_are_invalid() {
        let tool = HttpFetchTool::new(true);
        let cancel = CancellationToken::new();
        let err = tool
            .call(&ctx(), &input("file:///etc/passwd"), &cancel)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Invalid(_)), "{err}");
        let mut m = input("http://example.com/");
        m.insert("method".to_owned(), Value::String("POST".to_owned()));
        let err = tool.call(&ctx(), &m, &cancel).await.unwrap_err();
        assert!(matches!(err, ToolError::Invalid(_)), "{err}");
    }

    /// Canned-response fixture server: real socket, one HTTP/1.1 response.
    async fn fixture(body: &'static str, extra_headers: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n{}",
                        body.len(),
                        extra_headers,
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        format!("http://{addr}/")
    }

    #[tokio::test]
    async fn fetches_a_fixture_body() {
        let url = fixture("hello from fixture", "").await;
        let tool = HttpFetchTool::new(true);
        let out = tool
            .call(&ctx(), &input(&url), &CancellationToken::new())
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], 200);
        assert_eq!(parsed["body"], "hello from fixture");
        assert_eq!(parsed["truncated"], false);
    }

    #[tokio::test]
    async fn redirects_are_returned_not_followed() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let resp = "HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        let tool = HttpFetchTool::new(true);
        let out = tool
            .call(
                &ctx(),
                &input(&format!("http://{addr}/")),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], 302);
        assert_eq!(parsed["location"], "http://169.254.169.254/");
    }
}
