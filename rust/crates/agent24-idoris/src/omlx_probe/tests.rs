#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::{PermissionsExt, symlink},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

const SECRET: &str = "sk-test-omlx-never-print-8372";
const IDENTITY: &str = r#"{"openapi":"3.1.0","info":{"title":"oMLX API","version":"0.6.4"},"paths":{"/health":{"get":{}},"/api/status":{"get":{}},"/v1/models":{"get":{}}}}"#;
const MODELS: &str = r#"{"object":"list","data":[{"id":"z-model","owned_by":"omlx"},{"id":"a-model","owned_by":"omlx"}]}"#;

fn key() -> ApiKey {
    ApiKey::new(Zeroizing::new(SECRET.into())).unwrap()
}
fn endpoint(port: u16) -> Endpoint {
    Endpoint::parse(&format!("http://127.0.0.1:{port}")).unwrap()
}

#[test]
fn endpoint_input_rejects_non_loopback_and_url_components() {
    for input in [
        "",
        "8088",
        "ftp://127.0.0.1:8088",
        "http://localhost:8088",
        "http://example.com",
        "http://0.0.0.0:8088",
        "http://192.168.1.1:8088",
        "http://[::]:8088",
        "http://[::ffff:127.0.0.1]:8088",
        "http://127.0.0.1:0",
        "http://u:p@127.0.0.1:8088",
        "http://@127.0.0.1:8088",
        "http://127.0.0.1:8088/v1",
        "http://127.0.0.1:8088/..",
        "http://127.0.0.1:8088/%2e",
        "http://127.0.0.1:8088?",
        "http://127.0.0.1:8088/#",
        " http://127.0.0.1:8088",
        "http://127.0.0.1:8088\n",
        "http://127.0.0.1:8088\\v1",
        "http://127.0.0.1:65536",
        "http://127.0.0.1:bad",
    ] {
        assert!(Endpoint::parse(input).is_err(), "accepted {input:?}");
    }
}

#[test]
fn ipv6_loopback_and_http_https_origins_are_allowed() {
    for input in [
        "http://[::1]:8088",
        "https://127.0.0.1:8000/",
        "http://127.0.0.2:8001",
    ] {
        assert!(Endpoint::parse(input).is_ok());
    }
}

#[test]
fn candidate_order_is_confirmed_then_three_defaults_without_duplicates() {
    let custom = endpoint(9012);
    let urls = candidates(Some(&custom));
    assert_eq!(
        urls,
        vec![custom, endpoint(8088), endpoint(8000), endpoint(8001)]
    );
    assert_eq!(
        candidates(Some(&endpoint(8000))),
        vec![endpoint(8000), endpoint(8088), endpoint(8001)]
    );
    assert_eq!(
        candidates(None),
        vec![endpoint(8088), endpoint(8000), endpoint(8001)]
    );
}

struct Server {
    endpoint: Endpoint,
    seen: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn new(handler: impl Fn(&str) -> (u16, String) + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("local TCP fixture must bind");
        let endpoint = endpoint(listener.local_addr().unwrap().port());
        listener.set_nonblocking(true).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let requests = seen.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let worker = thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("fixture accept failed: {e}"),
                };
                stream.set_nonblocking(false).unwrap();
                // A client may reject Content-Length without consuming the body.
                stream
                    .set_write_timeout(Some(Duration::from_millis(250)))
                    .unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buf = [0; 1024];
                while !request.windows(4).any(|s| s == b"\r\n\r\n") {
                    let n = stream.read(&mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..n]);
                }
                let request = String::from_utf8(request).unwrap();
                requests.lock().unwrap().push(request.clone());
                let (code, body) = handler(&request);
                let reply = format!(
                    "HTTP/1.1 {code} Fixture\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        Self {
            endpoint,
            seen,
            stop,
            worker: Some(worker),
        }
    }

    fn requests(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Err(error) = self.worker.take().unwrap().join()
            && !thread::panicking()
        {
            std::panic::resume_unwind(error);
        }
    }
}

fn has_auth(request: &str) -> bool {
    request.to_ascii_lowercase().contains("\r\nauthorization:")
}
fn is_identity(request: &str) -> bool {
    request.starts_with("GET /openapi.json ")
}
fn omlx(request: &str) -> (u16, String) {
    (
        200,
        if is_identity(request) {
            IDENTITY
        } else {
            MODELS
        }
        .into(),
    )
}
async fn run(server: &Server, supplied: Option<&ApiKey>) -> ProbeResult {
    OmlxProbe::new()
        .unwrap()
        .probe_endpoints(
            std::slice::from_ref(&server.endpoint),
            Some(&server.endpoint),
            supplied,
        )
        .await
        .remove(0)
}

#[tokio::test]
async fn keyless_success_preserves_real_model_order() {
    let server = Server::new(omlx);
    let result = run(&server, None).await;
    assert_eq!(result.identity, ServiceIdentity::Omlx);
    assert_eq!(
        result.status,
        ProbeStatus::Ready {
            models: vec!["z-model".into(), "a-model".into()]
        }
    );
    assert!(server.requests().iter().all(|r| !has_auth(r)));
}

#[tokio::test]
async fn http_401_and_403_are_auth_required_even_without_identity() {
    for code in [401, 403] {
        for known in [false, true] {
            let server = Server::new(move |request| {
                if is_identity(request) && known {
                    (200, IDENTITY.into())
                } else {
                    (code, "{}".into())
                }
            });
            let result = run(&server, None).await;
            assert_eq!(result.status, ProbeStatus::AuthRequired);
            assert_eq!(
                result.identity,
                if known {
                    ServiceIdentity::Omlx
                } else {
                    ServiceIdentity::UnknownService
                }
            );
        }
    }
}

#[tokio::test]
async fn unknown_openai_service_never_receives_a_key() {
    let server = Server::new(|r| {
        (
            200,
            if is_identity(r) {
                r#"{"info":{"title":"Other API"},"paths":{"/v1/models":{}}}"#
            } else {
                MODELS
            }
            .into(),
        )
    });
    let result = run(&server, Some(&key())).await;
    assert_eq!(result.identity, ServiceIdentity::UnknownService);
    assert_eq!(result.status, ProbeStatus::UnknownService);
    assert!(
        server
            .requests()
            .iter()
            .all(|r| !has_auth(r) && !r.contains(SECRET))
    );
}

#[tokio::test]
async fn incomplete_omlx_schema_is_unknown_and_gets_no_key() {
    let server = Server::new(|r| {
        (200, if is_identity(r) {
            r#"{"info":{"title":"oMLX API"},"paths":{"/v1/models":{},"/api/status":{},"/health":{}}}"#
        } else { MODELS }.into())
    });
    let result = run(&server, Some(&key())).await;
    assert_eq!(result.identity, ServiceIdentity::UnknownService);
    assert!(server.requests().iter().all(|r| !has_auth(r)));
}

#[tokio::test]
async fn supplied_key_is_redacted_even_when_authentication_is_unnecessary() {
    let body = serde_json::json!({"data":[{"id":SECRET}]}).to_string();
    let server = Server::new(move |r| {
        (
            200,
            if is_identity(r) {
                IDENTITY.into()
            } else {
                body.clone()
            },
        )
    });
    let result = run(&server, Some(&key())).await;
    assert_eq!(result.status, ProbeStatus::InvalidResponse);
    assert!(!format!("{result:?}").contains(SECRET));
}

#[tokio::test]
async fn supplied_key_is_checked_even_when_keyless_models_are_available() {
    let server = Server::new(|r| {
        if is_identity(r) {
            (200, IDENTITY.into())
        } else if has_auth(r) {
            (401, "{}".into())
        } else {
            (200, MODELS.into())
        }
    });
    assert_eq!(
        run(&server, Some(&key())).await.status,
        ProbeStatus::AuthRejected
    );
}

#[tokio::test]
async fn oversized_http_body_is_rejected() {
    let server = Server::new(|_| (200, " ".repeat(MAX_RESPONSE_BYTES + 1)));
    assert_eq!(
        run(&server, None).await.status,
        ProbeStatus::InvalidResponse
    );
}

#[tokio::test]
async fn unconfirmed_401_service_never_receives_a_key() {
    let server = Server::new(|r| {
        if is_identity(r) {
            (404, "{}".into())
        } else {
            (401, "{}".into())
        }
    });
    let result = run(&server, Some(&key())).await;
    assert_eq!(result.identity, ServiceIdentity::UnknownService);
    assert_eq!(result.status, ProbeStatus::AuthRequired);
    assert!(server.requests().iter().all(|r| !has_auth(r)));
}

#[tokio::test]
async fn confirmed_identity_gets_key_only_after_keyless_probe() {
    let server = Server::new(|r| {
        if is_identity(r) {
            (200, IDENTITY.into())
        } else if has_auth(r) {
            (200, MODELS.into())
        } else {
            (401, "{}".into())
        }
    });
    let result = run(&server, Some(&key())).await;
    assert!(matches!(result.status, ProbeStatus::Ready { .. }));
    let requests = server.requests();
    assert!(!has_auth(&requests[0]));
    assert_eq!(requests.iter().filter(|r| has_auth(r)).count(), 1);
    let authenticated = requests.iter().find(|r| has_auth(r)).unwrap();
    assert!(authenticated.starts_with("GET /v1/models "));
    assert!(authenticated.contains(&format!("Bearer {SECRET}")));
    assert!(
        requests
            .iter()
            .filter(|r| is_identity(r))
            .all(|r| !has_auth(r))
    );
}

#[tokio::test]
async fn rejected_key_is_distinct_from_missing_key() {
    for code in [401, 403] {
        let server = Server::new(move |r| {
            if is_identity(r) {
                (200, IDENTITY.into())
            } else {
                (code, SECRET.into())
            }
        });
        let result = run(&server, Some(&key())).await;
        assert_eq!(result.status, ProbeStatus::AuthRejected);
        assert!(!format!("{result:?}").contains(SECRET));
    }
}

#[tokio::test]
async fn all_candidates_return_and_only_selected_endpoint_gets_key() {
    let first = Server::new(|r| {
        if is_identity(r) {
            (200, IDENTITY.into())
        } else {
            (401, "{}".into())
        }
    });
    let second = Server::new(|r| {
        if is_identity(r) {
            (200, IDENTITY.into())
        } else if has_auth(r) {
            (200, MODELS.into())
        } else {
            (403, "{}".into())
        }
    });
    let endpoints = vec![first.endpoint.clone(), second.endpoint.clone()];
    let results = OmlxProbe::new()
        .unwrap()
        .probe_endpoints(&endpoints, Some(&second.endpoint), Some(&key()))
        .await;
    assert_eq!(results.len(), 2);
    assert_eq!(
        results
            .iter()
            .map(|r| r.endpoint.clone())
            .collect::<Vec<_>>(),
        endpoints
    );
    assert_eq!(results[0].status, ProbeStatus::AuthRequired);
    assert!(matches!(results[1].status, ProbeStatus::Ready { .. }));
    assert!(first.requests().iter().all(|r| !has_auth(r)));
    assert_eq!(second.requests().iter().filter(|r| has_auth(r)).count(), 1);
}

#[tokio::test]
async fn redirects_are_never_followed_including_authenticated_requests() {
    let sink = Server::new(omlx);
    for authenticated in [false, true] {
        let target = sink.endpoint.as_str().to_owned();
        // Use a raw response fixture to add Location without changing the shared server.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let source = endpoint(listener.local_addr().unwrap().port());
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let worker = thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut buf = [0; 4096];
                let n = stream.read(&mut buf).unwrap();
                let request = std::str::from_utf8(&buf[..n]).unwrap();
                let reply = if authenticated && is_identity(request) {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{IDENTITY}",
                        IDENTITY.len()
                    )
                } else if authenticated && !has_auth(request) {
                    "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .into()
                } else {
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                };
                stream.write_all(reply.as_bytes()).unwrap();
            }
        });
        let result = OmlxProbe::new()
            .unwrap()
            .probe_endpoints(std::slice::from_ref(&source), Some(&source), Some(&key()))
            .await
            .remove(0);
        stop.store(true, Ordering::Relaxed);
        worker.join().unwrap();
        assert_eq!(result.status, ProbeStatus::HttpError(302));
    }
    assert!(sink.requests().is_empty());
}

#[tokio::test]
async fn timeout_is_bounded() {
    let server = Server::new(|r| {
        thread::sleep(Duration::from_millis(150));
        omlx(r)
    });
    let probe = OmlxProbe::with_timeout(Duration::from_millis(25)).unwrap();
    let start = std::time::Instant::now();
    let result = probe
        .probe_endpoints(std::slice::from_ref(&server.endpoint), None, None)
        .await;
    assert_eq!(result[0].status, ProbeStatus::Unreachable);
    assert!(start.elapsed() < Duration::from_millis(500));
}

#[tokio::test]
async fn empty_models_and_malformed_models_are_distinct() {
    for (body, expected) in [
        (r#"{"data":[]}"#, ProbeStatus::Ready { models: vec![] }),
        (r#"{"data":[{"id":3}]}"#, ProbeStatus::InvalidResponse),
        (r#"{"error":"failed"}"#, ProbeStatus::InvalidResponse),
        ("not-json", ProbeStatus::InvalidResponse),
    ] {
        let server =
            Server::new(move |r| (200, if is_identity(r) { IDENTITY } else { body }.into()));
        assert_eq!(run(&server, None).await.status, expected);
    }
}

#[tokio::test]
async fn reflected_key_cannot_escape_via_model_ids_or_debug() {
    use base64::Engine;
    for reflected in [
        SECRET.to_owned(),
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(SECRET),
        base64::engine::general_purpose::URL_SAFE.encode(SECRET),
        SECRET
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    ] {
        let body = serde_json::json!({"data":[{"id": reflected}]}).to_string();
        let server = Server::new(move |r| {
            if is_identity(r) {
                (200, IDENTITY.into())
            } else if has_auth(r) {
                (200, body.clone())
            } else {
                (401, "{}".into())
            }
        });
        assert_eq!(
            run(&server, Some(&key())).await.status,
            ProbeStatus::InvalidResponse
        );
    }
    assert!(!format!("{:?}", key()).contains(SECRET));
}

struct Settings {
    dir: tempfile::TempDir,
    path: std::path::PathBuf,
}
impl Settings {
    fn new(bytes: &[u8]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        Self { dir, path }
    }
    fn read(&self) -> Result<ApiKey, SettingsError> {
        read_settings_key_at(&self.path, rustix::process::geteuid().as_raw())
    }
}
fn settings() -> Settings {
    Settings::new(
        format!(r#"{{"auth":{{"api_key":" {SECRET} "}},"unrelated":{{"ignored":true}}}}"#)
            .as_bytes(),
    )
}

#[test]
fn settings_reads_only_auth_api_key_and_redacts_debug() {
    let fixture = settings();
    let value = fixture.read().unwrap();
    assert_eq!(value.0.as_str(), SECRET);
    assert_eq!(format!("{value:?}"), "ApiKey([REDACTED])");
}

#[test]
fn settings_missing_file_is_distinct() {
    let fixture = settings();
    fs::remove_file(&fixture.path).unwrap();
    assert_eq!(fixture.read().unwrap_err(), SettingsError::NotFound);
}

#[test]
fn settings_symlink_file_and_parent_are_rejected() {
    let fixture = settings();
    let link = fixture.dir.path().join("link.json");
    symlink(&fixture.path, &link).unwrap();
    assert_eq!(
        read_settings_key_at(&link, rustix::process::geteuid().as_raw()).unwrap_err(),
        SettingsError::UnsafeFile
    );
    let other = tempfile::tempdir().unwrap();
    let parent = other.path().join("linked-dir");
    symlink(fixture.dir.path(), &parent).unwrap();
    assert_eq!(
        read_settings_key_at(
            &parent.join("settings.json"),
            rustix::process::geteuid().as_raw()
        )
        .unwrap_err(),
        SettingsError::UnsafeFile
    );
}

#[test]
fn settings_different_owner_is_rejected_with_simulated_euid() {
    let fixture = settings();
    assert_eq!(
        read_settings_key_at(&fixture.path, rustix::process::geteuid().as_raw() ^ 1).unwrap_err(),
        SettingsError::UnsafeFile
    );
}

#[test]
fn settings_group_or_other_writable_file_and_parent_are_rejected() {
    for mode in [0o620, 0o602] {
        let fixture = settings();
        fs::set_permissions(&fixture.path, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(fixture.read().unwrap_err(), SettingsError::UnsafeFile);
    }
    let fixture = settings();
    fs::set_permissions(fixture.dir.path(), fs::Permissions::from_mode(0o720)).unwrap();
    assert_eq!(fixture.read().unwrap_err(), SettingsError::UnsafeFile);
}

#[test]
fn settings_oversize_bad_json_and_missing_key_are_distinct() {
    for (bytes, expected) in [
        (vec![b' '; 65537], SettingsError::TooLarge),
        (
            format!(r#"{{"auth":{{"api_key":"{SECRET}"}},oops}}"#).into_bytes(),
            SettingsError::InvalidJson,
        ),
        (b"{}".to_vec(), SettingsError::MissingKey),
        (
            br#"{"auth":{"other":"value"}}"#.to_vec(),
            SettingsError::MissingKey,
        ),
    ] {
        let fixture = Settings::new(&bytes);
        let error = fixture.read().unwrap_err();
        assert_eq!(error, expected);
        assert!(!format!("{error:?} {error}").contains(SECRET));
    }
}

#[test]
fn settings_invalid_key_and_non_regular_file_are_rejected() {
    for value in [
        serde_json::json!(42),
        serde_json::json!(""),
        serde_json::json!("\n"),
        serde_json::json!("abc\nsecret"),
        serde_json::json!("a".repeat(513)),
    ] {
        let fixture = Settings::new(
            serde_json::json!({"auth":{"api_key":value}})
                .to_string()
                .as_bytes(),
        );
        assert_eq!(fixture.read().unwrap_err(), SettingsError::InvalidKey);
    }
    let fixture = settings();
    fs::remove_file(&fixture.path).unwrap();
    fs::create_dir(&fixture.path).unwrap();
    assert_eq!(fixture.read().unwrap_err(), SettingsError::UnsafeFile);
}

#[test]
fn settings_fifo_is_rejected_without_blocking() {
    let fixture = settings();
    fs::remove_file(&fixture.path).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fixture.path)
            .status()
            .unwrap()
            .success()
    );
    let start = std::time::Instant::now();
    assert_eq!(fixture.read().unwrap_err(), SettingsError::UnsafeFile);
    assert!(start.elapsed() < Duration::from_millis(500));
}
