//! Proxy variables in the environment must not route a request elsewhere. The
//! workspace forbids `unsafe`, so the variables cannot be set in this process:
//! the test reruns itself as a child with them set.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use agent24_knowledge::{
    Credential, HttpTransport, HttpTransportConfig, KnowledgeOperation, KnowledgeTransport,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CHILD: &str = "AGENT24_KB_PROXY_ENV_CHILD";
const TARGET: &str = "AGENT24_KB_PROXY_ENV_TARGET";
const PROXY_VARS: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];

/// Accepts connections, counts them and answers each with `reply` once the
/// request head has arrived.
async fn listen(reply: &'static [u8]) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let conns = Arc::new(AtomicUsize::new(0));
    let counter = conns.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut data = Vec::new();
                let mut buf = [0u8; 1024];
                while !data.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf).await {
                        Ok(n) if n > 0 => data.extend_from_slice(&buf[..n]),
                        _ => return,
                    }
                }
                let _ = stream.write_all(reply).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    (addr, conns)
}

#[tokio::test(flavor = "multi_thread")]
async fn proxy_environment_is_ignored() {
    if let Ok(target) = std::env::var(TARGET) {
        assert!(std::env::var_os(CHILD).is_some());
        let t = HttpTransport::new(HttpTransportConfig {
            base_url: format!("http://{target}/api/v1"),
            credential: Credential::ApiKey("tk-secret-1".into()),
            max_response_bytes: 1024,
            connect_timeout: Duration::from_secs(5),
        })
        .unwrap();
        let got = t
            .send(
                KnowledgeOperation::Capabilities,
                Vec::new(),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_eq!(got.status, 200);
        assert_eq!(got.body, b"target");
        return;
    }

    let (target, target_conns) =
        listen(b"HTTP/1.1 200 OK\r\ncontent-length: 6\r\nconnection: close\r\n\r\ntarget").await;
    let (proxy, proxy_conns) =
        listen(b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\nconnection: close\r\n\r\nproxy").await;
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "proxy_environment_is_ignored",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env(TARGET, &target)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy");
    for var in PROXY_VARS {
        child.env(var, format!("http://{proxy}"));
    }
    let status = tokio::task::spawn_blocking(move || child.status())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "child run failed: {status}");
    assert_eq!(
        proxy_conns.load(Ordering::SeqCst),
        0,
        "the request went through the proxy"
    );
    assert_eq!(target_conns.load(Ordering::SeqCst), 1);
}
