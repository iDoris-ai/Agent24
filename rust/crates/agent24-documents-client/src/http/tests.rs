#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

/// A one-shot server on 127.0.0.1: reads one request (head and body), hands
/// it back, and answers with `reply` (written raw), or never answers.
async fn serve(reply: Option<Vec<u8>>) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut seen = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = sock.read(&mut buf).await.unwrap();
            seen.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&seen).to_string();
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text[..end]
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if seen.len() >= end + 4 + length {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        match reply {
            Some(bytes) => sock.write_all(&bytes).await.unwrap(),
            None => tokio::time::sleep(Duration::from_secs(5)).await,
        }
        String::from_utf8_lossy(&seen).to_string()
    });
    (base, task)
}

fn http(status: &str, body: &str, extra: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn post(path: &str, body: &[u8]) -> Request {
    Request {
        method: Method::Post,
        path: path.to_owned(),
        headers: vec![("Content-Type".to_owned(), "application/json".to_owned())],
        body: Some(body.to_vec()),
    }
}

#[test]
fn only_a_loopback_daemon_address_is_taken() {
    assert!(HttpTransport::new("http://127.0.0.1:8080", None).is_ok());
    for bad in [
        "https://127.0.0.1:8080",
        "http://localhost:8080",
        "http://10.0.0.1:8080",
        "http://127.0.0.1:8080/api",
        "http://127.0.0.1:",
        "http://127.0.0.1:0",
        "http://127.0.0.1:65536",
        "http://127.0.0.1:80@evil.example",
    ] {
        assert!(HttpTransport::new(bad, None).is_err(), "{bad}");
    }
}

#[tokio::test]
async fn a_request_goes_out_with_its_bearer_headers_and_body_and_the_reply_comes_back() {
    let scale = "Documents-Render-Scale: 0.5\r\n";
    let (base, task) = serve(Some(http("201 Created", r#"{"ok":1}"#, scale))).await;
    let t = HttpTransport::new(&base, Some("tok-123".to_owned())).unwrap();
    let reply = t
        .send(post("/api/v1/documents/uploads", b"{\"a\":1}"))
        .await;
    let Reply::Http {
        status,
        headers,
        body,
    } = reply
    else {
        panic!("{reply:?}");
    };
    assert_eq!((status, body), (201, br#"{"ok":1}"#.to_vec()));
    // Names in lower case, values as sent.
    let header = ("documents-render-scale".to_owned(), "0.5".to_owned());
    assert!(headers.contains(&header), "{headers:?}");
    let seen = task.await.unwrap();
    assert!(
        seen.starts_with("POST /api/v1/documents/uploads HTTP/1.1\r\n"),
        "{seen}"
    );
    let lower = seen.to_ascii_lowercase();
    assert!(
        lower.contains("authorization: bearer tok-123\r\n"),
        "{seen}"
    );
    assert!(
        lower.contains("content-type: application/json\r\n"),
        "{seen}"
    );
    assert!(seen.ends_with("{\"a\":1}"), "{seen}");
}

#[tokio::test]
async fn a_redirect_is_returned_as_it_is_never_followed() {
    let (base, task) = serve(Some(http(
        "302 Found",
        "",
        "Location: http://127.0.0.1:9/x\r\n",
    )))
    .await;
    let t = HttpTransport::new(&base, Some("tok".to_owned())).unwrap();
    let reply = t.send(post("/api/v1/documents/imports", b"{}")).await;
    assert!(
        matches!(&reply, Reply::Http { status: 302, body, .. } if body.is_empty()),
        "{reply:?}"
    );
    task.await.unwrap();
}

#[tokio::test]
async fn a_reply_past_the_cap_is_invalid_response() {
    let big = "x".repeat(64);
    let (base, task) = serve(Some(http("200 OK", &big, ""))).await;
    let t = HttpTransport::with_limits(&base, None, Duration::from_secs(5), 32).unwrap();
    let reply = t.send(post("/x", b"")).await;
    assert_eq!(
        reply,
        failed(502, "invalid_response", "the reply is too large")
    );
    task.await.unwrap();
}

#[tokio::test]
async fn no_reply_in_time_is_backend_timeout() {
    let (base, task) = serve(None).await;
    let t = HttpTransport::with_limits(&base, None, Duration::from_millis(200), 1024).unwrap();
    assert_eq!(t.send(post("/x", b"")).await, timed_out());
    task.abort();
}

#[tokio::test]
async fn a_daemon_that_is_not_there_is_backend_unreachable_without_the_address() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let t = HttpTransport::new(&base, None).unwrap();
    let reply = t.send(post("/x", b"")).await;
    assert_eq!(
        reply,
        failed(
            503,
            "backend_unreachable",
            "the daemon could not be reached"
        )
    );
}

#[tokio::test]
async fn a_path_that_does_not_start_with_a_slash_is_never_sent() {
    let (base, task) = serve(Some(http("200 OK", "{}", ""))).await;
    let t = HttpTransport::new(&base, Some("tok".to_owned())).unwrap();
    let reply = t.send(post("@evil.example/x", b"")).await;
    assert_eq!(reply, failed(400, "invalid_request", "not a daemon path"));
    task.abort();
}

#[tokio::test]
async fn a_header_that_is_not_text_is_kept_not_dropped() {
    // Dropped, it could hide a second Documents-Render-Scale (ops.rs).
    let mut reply =
        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\nDocuments-Render-Scale: "
            .to_vec();
    reply.extend_from_slice(b"\xff\r\n\r\n");
    let (base, task) = serve(Some(reply)).await;
    let t = HttpTransport::new(&base, Some("tok".to_owned())).unwrap();
    let Reply::Http { headers, .. } = t.send(post("/api/v1/documents/x", b"{}")).await else {
        panic!("expected a reply");
    };
    let header = ("documents-render-scale".to_owned(), "\u{fffd}".to_owned());
    assert!(headers.contains(&header), "{headers:?}");
    task.await.unwrap();
}
