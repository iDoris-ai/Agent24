//! HttpTransport against a local stub server. The stub speaks raw TCP so a test
//! controls every byte sent and sees when the client closes the connection.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use agent24_knowledge::{
    Credential, HttpConfigError, HttpTransport, HttpTransportConfig, KnowledgeOperation,
    KnowledgeTransport, TransportError,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

const LIMIT: usize = 1024;
const LONG: Duration = Duration::from_secs(30);
const SHORT: Duration = Duration::from_millis(300);

/// One request as the stub read it: the head (lower-cased) and the body.
#[derive(Debug)]
struct Seen {
    head: String,
    body: Vec<u8>,
}

/// What the stub does after reading a request.
#[derive(Clone)]
enum Script {
    /// Write these bytes, then close.
    Respond(Vec<u8>),
    /// Write nothing; wait until the client closes.
    Stall,
    /// Write `head`, then `chunk` every `every` until a write fails.
    Drip {
        head: Vec<u8>,
        chunk: Vec<u8>,
        every: Duration,
    },
}

struct Stub {
    base: String,
    conns: Arc<AtomicUsize>,
    requests: mpsc::UnboundedReceiver<Seen>,
    /// One message per connection the client closed (a read hit EOF or a
    /// write failed).
    closed: mpsc::UnboundedReceiver<()>,
}

impl Stub {
    async fn start(script: Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let conns = Arc::new(AtomicUsize::new(0));
        let (req_tx, requests) = mpsc::unbounded_channel();
        let (closed_tx, closed) = mpsc::unbounded_channel();
        let counter = conns.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let (script, req_tx, closed_tx) =
                    (script.clone(), req_tx.clone(), closed_tx.clone());
                tokio::spawn(serve(stream, script, req_tx, closed_tx));
            }
        });
        Self {
            base: format!("http://{addr}/api/v1"),
            conns,
            requests,
            closed,
        }
    }

    fn transport(&self, credential: Credential) -> HttpTransport {
        transport_for(&self.base, credential, LIMIT)
    }

    async fn expect_closed(&mut self) {
        tokio::time::timeout(Duration::from_secs(5), self.closed.recv())
            .await
            .expect("client did not close the connection")
            .unwrap();
    }
}

async fn serve(
    mut stream: TcpStream,
    script: Script,
    req_tx: mpsc::UnboundedSender<Seen>,
    closed_tx: mpsc::UnboundedSender<()>,
) {
    let Some(seen) = read_request(&mut stream).await else {
        let _ = closed_tx.send(());
        return;
    };
    let _ = req_tx.send(seen);
    match script {
        Script::Respond(bytes) => {
            let _ = stream.write_all(&bytes).await;
            let _ = stream.shutdown().await;
        }
        Script::Stall => {
            let mut buf = [0u8; 256];
            while matches!(stream.read(&mut buf).await, Ok(n) if n > 0) {}
            let _ = closed_tx.send(());
        }
        Script::Drip { head, chunk, every } => {
            if stream.write_all(&head).await.is_err() {
                let _ = closed_tx.send(());
                return;
            }
            loop {
                if stream.write_all(&chunk).await.is_err() || stream.flush().await.is_err() {
                    let _ = closed_tx.send(());
                    return;
                }
                tokio::time::sleep(every).await;
            }
        }
    }
}

/// Reads the head up to the blank line, then `content-length` bytes of body.
async fn read_request(stream: &mut TcpStream) -> Option<Seen> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&buf[..n]);
    };
    let head = String::from_utf8_lossy(&data[..head_end]).to_lowercase();
    let len = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .map_or(0, |v| v.trim().parse::<usize>().unwrap());
    let mut body = data[head_end..].to_vec();
    while body.len() < len {
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&buf[..n]);
    }
    Some(Seen { head, body })
}

fn transport_for(base: &str, credential: Credential, limit: usize) -> HttpTransport {
    HttpTransport::new(HttpTransportConfig {
        base_url: base.to_owned(),
        credential,
        max_response_bytes: limit,
        connect_timeout: Duration::from_secs(5),
    })
    .unwrap()
}

fn response(status: &str, extra: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

fn chunked(body: &[u8]) -> Vec<u8> {
    let mut out =
        b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n".to_vec();
    for part in body.chunks(100) {
        out.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
        out.extend_from_slice(part);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"0\r\n\r\n");
    out
}

/// Fails the test instead of hanging when the transport's own deadline does not
/// end the call.
async fn bounded<T>(call: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .expect("the deadline did not end the call")
}

fn api_key() -> Credential {
    Credential::ApiKey("tk-secret-1".into())
}

#[tokio::test(flavor = "multi_thread")]
async fn post_carries_path_json_body_and_api_key() {
    let mut stub = Stub::start(Script::Respond(response("200 OK", "", b"{\"ok\":1}"))).await;
    let t = stub.transport(api_key());
    let got = t
        .send(KnowledgeOperation::Search, b"{\"q\":1}".to_vec(), LONG)
        .await
        .unwrap();
    assert_eq!(got.status, 200);
    assert_eq!(got.body, b"{\"ok\":1}");
    let seen = stub.requests.recv().await.unwrap();
    assert!(
        seen.head
            .starts_with("post /api/v1/agent24/knowledge/search http/1.1\r\n"),
        "{}",
        seen.head
    );
    assert!(
        seen.head.contains("\r\ncontent-type: application/json\r\n"),
        "{}",
        seen.head
    );
    assert!(
        seen.head.contains("\r\nx-api-key: tk-secret-1\r\n"),
        "{}",
        seen.head
    );
    assert!(
        seen.head.contains("\r\naccept: application/json\r\n"),
        "{}",
        seen.head
    );
    assert!(!seen.head.contains("authorization:"), "{}", seen.head);
    assert_eq!(seen.body, b"{\"q\":1}");
}

#[tokio::test(flavor = "multi_thread")]
async fn trailing_slash_in_the_base_adds_no_empty_segment() {
    let mut stub = Stub::start(Script::Respond(response("200 OK", "", b"{}"))).await;
    let t = transport_for(&format!("{}/", stub.base), api_key(), LIMIT);
    t.send(KnowledgeOperation::Ready, b"{}".to_vec(), LONG)
        .await
        .unwrap();
    let seen = stub.requests.recv().await.unwrap();
    assert!(
        seen.head
            .starts_with("post /api/v1/agent24/knowledge/ready http/1.1\r\n"),
        "{}",
        seen.head
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn zero_deadline_sends_nothing() {
    let stub = Stub::start(Script::Respond(response("200 OK", "", b"{}"))).await;
    let t = stub.transport(api_key());
    for _ in 0..20 {
        let err = t
            .send(KnowledgeOperation::Search, b"{}".to_vec(), Duration::ZERO)
            .await
            .unwrap_err();
        assert_eq!(err, TransportError::Timeout);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(stub.conns.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn get_capabilities_has_no_body_and_carries_bearer() {
    let mut stub = Stub::start(Script::Respond(response("200 OK", "", b"{}"))).await;
    let t = stub.transport(Credential::Bearer("jwt-secret".into()));
    t.send(KnowledgeOperation::Capabilities, Vec::new(), LONG)
        .await
        .unwrap();
    let seen = stub.requests.recv().await.unwrap();
    assert!(
        seen.head
            .starts_with("get /api/v1/system/capabilities http/1.1\r\n"),
        "{}",
        seen.head
    );
    assert!(
        seen.head
            .contains("\r\nauthorization: bearer jwt-secret\r\n"),
        "{}",
        seen.head
    );
    assert!(!seen.head.contains("x-api-key:"), "{}", seen.head);
    assert!(!seen.head.contains("content-type:"), "{}", seen.head);
    assert!(seen.body.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn get_with_a_body_is_refused_before_sending() {
    let stub = Stub::start(Script::Respond(response("200 OK", "", b"{}"))).await;
    let t = stub.transport(api_key());
    let err = t
        .send(KnowledgeOperation::Capabilities, b"x".to_vec(), LONG)
        .await
        .unwrap_err();
    assert_eq!(err, TransportError::Other("body on a request without one"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(stub.conns.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn non_2xx_comes_back_as_a_response() {
    let body = br#"{"error":{"code":"unavailable"}}"#;
    let stub = Stub::start(Script::Respond(response(
        "503 Service Unavailable",
        "",
        body,
    )))
    .await;
    let got = stub
        .transport(api_key())
        .send(KnowledgeOperation::Ready, b"{}".to_vec(), LONG)
        .await
        .unwrap();
    assert_eq!(got.status, 503);
    assert_eq!(got.body, body);
}

#[tokio::test(flavor = "multi_thread")]
async fn body_at_the_limit_is_accepted() {
    let body = vec![b'a'; LIMIT];
    let stub = Stub::start(Script::Respond(chunked(&body))).await;
    let got = stub
        .transport(api_key())
        .send(KnowledgeOperation::Read, b"{}".to_vec(), LONG)
        .await
        .unwrap();
    assert_eq!(got.body, body);
}

#[tokio::test(flavor = "multi_thread")]
async fn declared_length_at_the_limit_is_accepted() {
    let body = vec![b'a'; LIMIT];
    let stub = Stub::start(Script::Respond(response("200 OK", "", &body))).await;
    let got = stub
        .transport(api_key())
        .send(KnowledgeOperation::Read, b"{}".to_vec(), LONG)
        .await
        .unwrap();
    assert_eq!(got.body, body);
}

#[tokio::test(flavor = "multi_thread")]
async fn body_shorter_than_its_declared_length_is_an_error() {
    let mut bytes = b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\nconnection: close\r\n\r\n".to_vec();
    bytes.extend_from_slice(b"short");
    let stub = Stub::start(Script::Respond(bytes)).await;
    let err = stub
        .transport(api_key())
        .send(KnowledgeOperation::Read, b"{}".to_vec(), LONG)
        .await
        .unwrap_err();
    assert_eq!(err, TransportError::Other("response body interrupted"));
}

#[tokio::test(flavor = "multi_thread")]
async fn chunked_body_cut_off_midway_is_an_error() {
    let mut bytes = chunked(b"0123456789");
    bytes.truncate(bytes.len() - b"0\r\n\r\n".len());
    let stub = Stub::start(Script::Respond(bytes)).await;
    let err = stub
        .transport(api_key())
        .send(KnowledgeOperation::Read, b"{}".to_vec(), LONG)
        .await
        .unwrap_err();
    assert_eq!(err, TransportError::Other("response body interrupted"));
}

#[tokio::test(flavor = "multi_thread")]
async fn declared_length_over_the_limit_is_refused() {
    let stub = Stub::start(Script::Respond(response(
        "200 OK",
        "",
        &vec![b'a'; LIMIT + 1],
    )))
    .await;
    let err = stub
        .transport(api_key())
        .send(KnowledgeOperation::Read, b"{}".to_vec(), LONG)
        .await
        .unwrap_err();
    assert_eq!(err, TransportError::ResponseTooLarge);
}

#[tokio::test(flavor = "multi_thread")]
async fn declared_length_over_the_limit_is_refused_before_reading() {
    let stub = Stub::start(Script::Drip {
        head: format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", LIMIT * 100).into_bytes(),
        chunk: b"a".to_vec(),
        every: Duration::from_secs(1),
    })
    .await;
    let err = stub
        .transport(api_key())
        .send(KnowledgeOperation::Read, b"{}".to_vec(), SHORT)
        .await
        .unwrap_err();
    assert_eq!(err, TransportError::ResponseTooLarge);
}

#[tokio::test(flavor = "multi_thread")]
async fn streamed_body_over_the_limit_is_refused() {
    let stub = Stub::start(Script::Respond(chunked(&vec![b'a'; LIMIT + 1]))).await;
    let err = stub
        .transport(api_key())
        .send(KnowledgeOperation::Read, b"{}".to_vec(), LONG)
        .await
        .unwrap_err();
    assert_eq!(err, TransportError::ResponseTooLarge);
}

#[tokio::test(flavor = "multi_thread")]
async fn endless_body_stops_being_read_at_the_limit() {
    let mut stub = Stub::start(Script::Drip {
        head: b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n".to_vec(),
        chunk: format!("{:x}\r\n{}\r\n", 256, "a".repeat(256)).into_bytes(),
        every: Duration::from_millis(1),
    })
    .await;
    let t = stub.transport(api_key());
    let err = t
        .send(KnowledgeOperation::Read, b"{}".to_vec(), LONG)
        .await
        .unwrap_err();
    assert_eq!(err, TransportError::ResponseTooLarge);
    drop(t);
    stub.expect_closed().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn redirect_is_not_followed_and_the_credential_stays_home() {
    let elsewhere = Stub::start(Script::Respond(response("200 OK", "", b"{}"))).await;
    let location = format!("location: {}/agent24/knowledge/search\r\n", elsewhere.base);
    let stub = Stub::start(Script::Respond(response(
        "307 Temporary Redirect",
        &location,
        b"",
    )))
    .await;
    let got = stub
        .transport(api_key())
        .send(KnowledgeOperation::Search, b"{}".to_vec(), LONG)
        .await
        .unwrap();
    assert_eq!(got.status, 307);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(stub.conns.load(Ordering::SeqCst), 1);
    assert_eq!(elsewhere.conns.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn deadline_bounds_waiting_for_the_head() {
    let stub = Stub::start(Script::Stall).await;
    let start = Instant::now();
    let t = stub.transport(api_key());
    let err = bounded(t.send(KnowledgeOperation::Search, b"{}".to_vec(), SHORT))
        .await
        .unwrap_err();
    assert_eq!(err, TransportError::Timeout);
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn deadline_bounds_a_slow_body() {
    let stub = Stub::start(Script::Drip {
        head: b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n".to_vec(),
        chunk: b"1\r\na\r\n".to_vec(),
        every: Duration::from_millis(50),
    })
    .await;
    let start = Instant::now();
    let t = stub.transport(api_key());
    let err = bounded(t.send(KnowledgeOperation::Read, b"{}".to_vec(), SHORT))
        .await
        .unwrap_err();
    assert_eq!(err, TransportError::Timeout);
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn closed_port_is_unreachable() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let t = transport_for(&format!("http://{addr}/api/v1"), api_key(), LIMIT);
    let err = t
        .send(KnowledgeOperation::Ready, b"{}".to_vec(), LONG)
        .await
        .unwrap_err();
    assert_eq!(err, TransportError::Unreachable);
}

/// reqwest's `connect_timeout` covers the TLS handshake, so a server that
/// accepts TCP and never answers the ClientHello runs it out well before the
/// deadline.
#[tokio::test(flavor = "multi_thread")]
async fn connect_timeout_is_unreachable_not_timeout() {
    let stub = Stub::start(Script::Stall).await;
    let t = HttpTransport::new(HttpTransportConfig {
        base_url: stub.base.replace("http://", "https://"),
        credential: api_key(),
        max_response_bytes: LIMIT,
        connect_timeout: Duration::from_millis(200),
    })
    .unwrap();
    let start = Instant::now();
    let err = bounded(t.send(KnowledgeOperation::Search, b"{}".to_vec(), LONG))
        .await
        .unwrap_err();
    assert_eq!(err, TransportError::Unreachable);
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_send_while_waiting_closes_the_connection() {
    let mut stub = Stub::start(Script::Stall).await;
    let t = stub.transport(api_key());
    tokio::select! {
        r = t.send(KnowledgeOperation::Search, b"{}".to_vec(), LONG) => panic!("returned {r:?}"),
        seen = stub.requests.recv() => assert!(seen.is_some()),
    }
    stub.expect_closed().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_send_while_reading_stops_the_body() {
    let mut stub = Stub::start(Script::Drip {
        head: b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n".to_vec(),
        chunk: b"1\r\na\r\n".to_vec(),
        every: Duration::from_millis(20),
    })
    .await;
    let t = stub.transport(api_key());
    tokio::select! {
        r = t.send(KnowledgeOperation::Read, b"{}".to_vec(), LONG) => panic!("returned {r:?}"),
        () = tokio::time::sleep(SHORT) => {}
    }
    stub.expect_closed().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn https_never_falls_back_to_plain_http() {
    let stub = Stub::start(Script::Respond(response("200 OK", "", b"{}"))).await;
    let base = stub.base.replace("http://", "https://");
    // The stub waits for a request head that a TLS ClientHello never forms;
    // what matters is that no plain request arrives and nothing succeeds.
    transport_for(&base, api_key(), LIMIT)
        .send(
            KnowledgeOperation::Search,
            b"{}".to_vec(),
            Duration::from_secs(2),
        )
        .await
        .unwrap_err();
    let mut requests = stub.requests;
    assert!(
        requests.try_recv().is_err(),
        "a plain HTTP request reached the server"
    );
}

fn config(base: &str, credential: Credential) -> HttpTransportConfig {
    HttpTransportConfig {
        base_url: base.to_owned(),
        credential,
        max_response_bytes: LIMIT,
        connect_timeout: Duration::from_secs(5),
    }
}

#[test]
fn base_url_rules() {
    let ok = [
        "https://kb.example.com/api/v1",
        "https://kb.example.com/api/v1/",
        "https://10.0.0.5:8443/api/v1",
        "http://127.0.0.1:8080/api/v1",
        "http://127.9.9.9/api/v1",
        "http://[::1]:8080/api/v1",
    ];
    for base in ok {
        assert!(
            HttpTransport::new(config(base, api_key())).is_ok(),
            "{base}"
        );
    }
    let refused = [
        ("not a url", HttpConfigError::InvalidUrl),
        ("mailto:a@example.com", HttpConfigError::InvalidUrl),
        (
            "http://kb.example.com/api/v1",
            HttpConfigError::InsecureScheme,
        ),
        (
            "http://localhost:8080/api/v1",
            HttpConfigError::InsecureScheme,
        ),
        ("http://10.0.0.5/api/v1", HttpConfigError::InsecureScheme),
        ("http://[::2]/api/v1", HttpConfigError::InsecureScheme),
        (
            "ftp://kb.example.com/api/v1",
            HttpConfigError::InsecureScheme,
        ),
        (
            "https://user@kb.example.com/api/v1",
            HttpConfigError::UrlHasUserInfo,
        ),
        (
            "https://user:pw@kb.example.com/api/v1",
            HttpConfigError::UrlHasUserInfo,
        ),
        (
            "https://:pw@kb.example.com/api/v1",
            HttpConfigError::UrlHasUserInfo,
        ),
        (
            "https://kb.example.com/api/v1?k=1",
            HttpConfigError::UrlHasQueryOrFragment,
        ),
        (
            "https://kb.example.com/api/v1#f",
            HttpConfigError::UrlHasQueryOrFragment,
        ),
    ];
    for (base, want) in refused {
        assert_eq!(
            HttpTransport::new(config(base, api_key())).unwrap_err(),
            want,
            "{base}"
        );
    }
}

#[test]
fn credential_and_limit_rules() {
    let base = "https://kb.example.com/api/v1";
    for bad in [
        Credential::ApiKey(String::new()),
        Credential::Bearer(String::new()),
        Credential::ApiKey("a\nb".into()),
        Credential::Bearer("a\rb".into()),
        Credential::ApiKey("tk-é".into()),
        Credential::Bearer("jwt-é".into()),
    ] {
        assert_eq!(
            HttpTransport::new(config(base, bad)).unwrap_err(),
            HttpConfigError::InvalidCredential
        );
    }
    let mut zero = config(base, api_key());
    zero.max_response_bytes = 0;
    assert_eq!(
        HttpTransport::new(zero).unwrap_err(),
        HttpConfigError::InvalidLimit
    );
    let mut zero = config(base, api_key());
    zero.connect_timeout = Duration::ZERO;
    assert_eq!(
        HttpTransport::new(zero).unwrap_err(),
        HttpConfigError::InvalidLimit
    );
}

#[test]
fn debug_output_never_shows_the_secret() {
    let cfg = config(
        "https://kb.example.com/api/v1",
        Credential::Bearer("jwt-secret".into()),
    );
    let t = HttpTransport::new(cfg.clone()).unwrap();
    for text in [
        format!("{cfg:?}"),
        format!("{t:?}"),
        format!("{:?}", api_key()),
    ] {
        assert!(!text.contains("secret"), "{text}");
    }
}
