//! The one way a first-party Rust client talks to the local daemon.
//!
//! FU-74: every call to `agent24d` carries a bearer token and chat content, and
//! the daemon is always on `127.0.0.1`. The default `reqwest` client reads
//! `HTTP_PROXY`/`ALL_PROXY` without bypassing loopback and follows redirects —
//! a stray proxy variable, or an impersonator answering with a 3xx (the daemon
//! itself never redirects), would ship the token off-box. This constructor is
//! the single place the three-part hardening lives so no caller can forget one.

use std::time::Duration;

/// A `reqwest` client for loopback-only daemon calls: `no_proxy()`, redirects
/// never followed, and a bounded connect. Per-request timeouts stay with the
/// caller. Fails closed (panics) if it cannot be built.
#[must_use]
pub fn loopback_client(connect_timeout: Duration) -> reqwest::Client {
    #[expect(
        clippy::expect_used,
        reason = "unwrap_or_default() here would silently rebuild the proxy-reading, \
                  redirect-following client FU-74 exists to rule out; fail closed instead"
    )]
    reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("building the loopback-only daemon HTTP client failed")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Accepts connections, counts them, answers `reply` verbatim.
    async fn counting_server(reply: String) -> (u16, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let reply = reply.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock.write_all(reply.as_bytes()).await;
                });
            }
        });
        (port, hits)
    }

    const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

    /// Child half: runs only when the parent re-executes this test binary with
    /// `HTTP_PROXY` set (mutating our own env is `unsafe` and racy).
    #[tokio::test]
    #[ignore = "child process of the proxy tests"]
    async fn proxy_child() {
        let url = std::env::var("a24_test_url").expect("a24_test_url");
        let client = if std::env::var("a24_test_raw").is_ok() {
            reqwest::Client::new()
        } else {
            loopback_client(Duration::from_millis(500))
        };
        let _ = client
            .get(url)
            .timeout(Duration::from_millis(800))
            .send()
            .await;
    }

    async fn run_child(raw: bool) -> (usize, usize) {
        let (target, target_hits) = counting_server(OK.to_owned()).await;
        // A "proxy" that answers 502 so the raw control fails fast.
        let (proxy, proxy_hits) =
            counting_server("HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n".to_owned())
                .await;
        let mut cmd = tokio::process::Command::new(std::env::current_exe().unwrap());
        cmd.args(["--ignored", "--exact", "client::tests::proxy_child"])
            .env("HTTP_PROXY", format!("http://127.0.0.1:{proxy}"))
            .env("http_proxy", format!("http://127.0.0.1:{proxy}"))
            .env("ALL_PROXY", format!("http://127.0.0.1:{proxy}"))
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .env("a24_test_url", format!("http://127.0.0.1:{target}/x"));
        if raw {
            cmd.env("a24_test_raw", "1");
        }
        let out = cmd.output().await.unwrap();
        assert!(out.status.success(), "child failed: {out:?}");
        (
            target_hits.load(Ordering::SeqCst),
            proxy_hits.load(Ordering::SeqCst),
        )
    }

    #[tokio::test]
    async fn loopback_client_ignores_http_proxy() {
        let (target, proxy) = run_child(false).await;
        assert_eq!((target, proxy), (1, 0), "must go direct, never via proxy");
    }

    #[tokio::test]
    async fn positive_control_the_default_client_does_use_the_proxy() {
        let (_target, proxy) = run_child(true).await;
        assert!(
            proxy >= 1,
            "control: the env really does steer a plain client"
        );
    }

    #[tokio::test]
    async fn loopback_client_does_not_follow_redirects() {
        let (sink, sink_hits) = counting_server(OK.to_owned()).await;
        let (port, _) = counting_server(format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{sink}/stolen\r\nContent-Length: 0\r\n\r\n"
        ))
        .await;
        let res = loopback_client(Duration::from_secs(2))
            .get(format!("http://127.0.0.1:{port}/"))
            .bearer_auth("secret")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), reqwest::StatusCode::FOUND);
        assert_eq!(sink_hits.load(Ordering::SeqCst), 0);
    }
}
