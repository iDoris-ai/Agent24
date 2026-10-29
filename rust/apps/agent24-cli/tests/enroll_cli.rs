//! CORE-2 slice S4 (2f-CLI): `agent24 os attach add --json` in capabilities
//! mode, driven through the real `agent24` binary against an in-test fake
//! daemon: a Unix socket speaking the enrolment protocol v1
//! (`docs/agent/CORE-2-AGENTEAR-SLICE.md` §3.1) and a tiny HTTP server that
//! answers `403 presence_required` until the elevation header matches.
//!
//! The fake daemon speaks first, as the real one does (design §10 R8): it
//! sends `challenge`, then requires a `hello` that echoes the nonce. The real
//! daemon side is covered by `enroll_real_daemon.rs`; these tests pin the
//! CLI's half of the wire contract and the A3 §3.6 output shapes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

const TOKEN: &str = "tok-SECRET-0123456789abcdef";
const GRANT: &str = "grant-SECRET-fedcba9876543210";
const GENERATION: &str = "gen-test-1";
const NONCE: &str = "00112233445566778899aabbccddeeff";
const ATTACH_TOKEN: &str = "abababababababababababababababababababababababababababababababab";

#[derive(Clone)]
struct HttpSeen {
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

/// What the fake HTTP side does with `POST /api/v1/attached`.
#[derive(Clone, Copy, PartialEq)]
enum HttpMode {
    /// 403 presence_required until the right grant arrives, then 201.
    ElevateThenCreate,
    /// 201 immediately.
    CreateDirect,
    /// 403 presence_required, and 403 elevation_invalid even with a grant.
    AlwaysInvalid,
    /// 403 with a code that is not presence_required.
    OtherForbidden,
    AlwaysPresenceRequired,
}

struct Fake {
    http_port: u16,
    posts: Arc<Mutex<Vec<HttpSeen>>>,
    /// Every HTTP request line with its headers, health probes included.
    requests: Requests,
    frames: Arc<Mutex<Vec<Value>>>,
    /// Enrolment connections the fake has finished with.
    closed: Arc<AtomicUsize>,
}

fn read_http(stream: &mut TcpStream) -> Option<(String, HashMap<String, String>, Vec<u8>)> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    if request_line.is_empty() {
        return None;
    }
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (k, v) = line.split_once(':')?;
        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
    }
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; len];
    reader.read_exact(&mut body).ok()?;
    Some((request_line, headers, body))
}

fn respond(stream: &mut TcpStream, status: &str, body: &Value) {
    let text = body.to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    );
}

type Requests = Arc<Mutex<Vec<(String, HashMap<String, String>)>>>;

fn spawn_http(mode: HttpMode, posts: Arc<Mutex<Vec<HttpSeen>>>) -> u16 {
    spawn_http_recording(mode, posts, Arc::default())
}

fn spawn_http_recording(
    mode: HttpMode,
    posts: Arc<Mutex<Vec<HttpSeen>>>,
    requests: Requests,
) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let Some((line, headers, body)) = read_http(&mut stream) else {
                continue;
            };
            requests
                .lock()
                .unwrap()
                .push((line.trim_end().to_owned(), headers.clone()));
            if line.starts_with("GET /api/v1/health") {
                respond(
                    &mut stream,
                    "200 OK",
                    &json!({"protocol_version": agent24_protocol::PROTOCOL_VERSION}),
                );
                continue;
            }
            assert!(
                line.starts_with("POST /api/v1/attached ")
                    || line.starts_with("POST /api/v1/os/agentear/delegation "),
                "{line}"
            );
            posts.lock().unwrap().push(HttpSeen {
                headers: headers.clone(),
                body,
            });
            let bearer_ok = headers.get("authorization").map(String::as_str)
                == Some(&format!("Bearer {}", expected_bearer(&headers)));
            if !bearer_ok {
                respond(
                    &mut stream,
                    "401 Unauthorized",
                    &json!({"error":{"code":"unauthorized","message":"bad bearer"}}),
                );
                continue;
            }
            let has_grant = headers.get("x-agent24-elevation").map(String::as_str) == Some(GRANT);
            let created = json!({
                "name": "agentear", "manifest_digest": format!("sha256:{}", "0".repeat(64)),
                "token": ATTACH_TOKEN, "socket_path": "/x/attach/agent24d.sock",
                "token_id": "tok_00000000"
            });
            match mode {
                HttpMode::CreateDirect => respond(&mut stream, "201 Created", &created),
                HttpMode::ElevateThenCreate if has_grant => {
                    respond(&mut stream, "201 Created", &created);
                }
                HttpMode::AlwaysInvalid if has_grant => respond(
                    &mut stream,
                    "403 Forbidden",
                    &json!({"error":{"code":"elevation_invalid","message":"grant does not match"}}),
                ),
                HttpMode::OtherForbidden => respond(
                    &mut stream,
                    "403 Forbidden",
                    &json!({"error":{"code":"forbidden","message":"nope"}}),
                ),
                _ => respond(
                    &mut stream,
                    "403 Forbidden",
                    &json!({"error":{"code":"presence_required","message":"needs approval","operation":"ModuleAdmin"}}),
                ),
            }
        }
    });
    port
}

/// Legacy mode authenticates with the `daemon.json` bearer, capabilities mode
/// with the enrolled token.
fn expected_bearer(headers: &HashMap<String, String>) -> String {
    match headers.get("authorization") {
        Some(v) if v == "Bearer legacy-bearer" => "legacy-bearer".to_owned(),
        _ => TOKEN.to_owned(),
    }
}

/// How the fake enrolment socket behaves.
#[derive(Clone)]
struct UdsScript {
    /// The generation the daemon believes it has.
    generation: &'static str,
    /// Replace `enrolled` with this error frame.
    handshake_error: Option<Value>,
    /// Frames sent in reply to an `elevate`.
    elevate_replies: Vec<Value>,
    /// Replace the well-formed `challenge` with this frame.
    challenge: Option<Value>,
    /// Report this HTTP port in `enrolled` instead of the fake's real one.
    enrolled_port: Option<u16>,
}

fn pending_then(final_frame: Value) -> Vec<Value> {
    vec![
        json!({"v":1,"op":"elevation_pending","challenge_id":"c1c1c1c1c1c1c1c1"}),
        final_frame,
    ]
}

fn elevated() -> Value {
    json!({"v":1,"op":"elevated","challenge_id":"c1c1c1c1c1c1c1c1","grant":GRANT,"expires_in":60})
}

fn presence_error(code: &str) -> Value {
    json!({"v":1,"op":"error","code":code,"message":"daemon text"})
}

fn serve_uds(
    listener: UnixListener,
    http_port: u16,
    script: UdsScript,
    frames: Arc<Mutex<Vec<Value>>>,
    closed: Arc<AtomicUsize>,
) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            handle_uds(stream, http_port, &script, &frames);
            closed.fetch_add(1, Ordering::SeqCst);
        }
    });
}

fn send(stream: &mut UnixStream, frame: &Value) {
    let _ = writeln!(stream, "{frame}");
}

fn challenge() -> Value {
    json!({"v":1,"op":"challenge","conn_id":"00112233445566aa","nonce":NONCE})
}

/// Server first, like the real daemon: `challenge`, then a `hello` that must
/// echo the nonce, then `enrolled`; afterwards answer each frame with the
/// scripted elevation replies. Every frame the client sends is recorded.
fn handle_uds(
    mut stream: UnixStream,
    http_port: u16,
    script: &UdsScript,
    frames: &Arc<Mutex<Vec<Value>>>,
) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let sent = script.challenge.clone().unwrap_or_else(challenge);
    send(&mut stream, &sent);
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let hello: Value = serde_json::from_str(&line).unwrap();
    frames.lock().unwrap().push(hello.clone());
    if script.challenge.is_some() || hello["op"] != "hello" || hello["nonce"] != NONCE {
        send(&mut stream, &presence_error("identity_rejected"));
        return;
    }
    if hello["generation"] != script.generation {
        send(&mut stream, &presence_error("generation_mismatch"));
        return;
    }
    if let Some(err) = &script.handshake_error {
        send(&mut stream, err);
        return;
    }
    send(
        &mut stream,
        &json!({"v":1,"op":"enrolled","token":TOKEN,"expires_in":3600,"tier":"daily",
                "identity":{"kind":"macos_seccode","identifier":"agent24"},
                "http_port": script.enrolled_port.unwrap_or(http_port)}),
    );
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let frame: Value = serde_json::from_str(&line).unwrap();
        frames.lock().unwrap().push(frame);
        for reply in &script.elevate_replies {
            send(&mut stream, reply);
        }
    }
}

fn start(mode: HttpMode, script: UdsScript, home: &Path) -> Fake {
    let posts = Arc::new(Mutex::new(Vec::new()));
    let frames = Arc::new(Mutex::new(Vec::new()));
    let closed = Arc::new(AtomicUsize::new(0));
    let requests: Requests = Arc::default();
    let http_port = spawn_http_recording(mode, Arc::clone(&posts), Arc::clone(&requests));
    let sock = home.join("enroll.sock");
    serve_uds(
        UnixListener::bind(&sock).unwrap(),
        http_port,
        script,
        Arc::clone(&frames),
        Arc::clone(&closed),
    );
    write_daemon_json(
        home,
        &json!({"port": http_port, "pid": std::process::id(), "version": "9.9.9",
                "generation": GENERATION, "auth_mode": "capabilities",
                "protocol_version": agent24_protocol::PROTOCOL_VERSION,
                "enroll_endpoint": sock}),
    );
    Fake {
        http_port,
        posts,
        requests,
        frames,
        closed,
    }
}

fn write_daemon_json(home: &Path, value: &Value) {
    let dir = home.join(".agent24");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("daemon.json"), value.to_string()).unwrap();
}

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("a24enr")
        .tempdir_in("/tmp")
        .unwrap()
}

fn run_add(home: &Path) -> (bool, String, String) {
    let manifest = home.join("agentear.yml");
    std::fs::write(
        &manifest,
        "name: agentear\nversion: \"1\"\nroute_namespace: /api/v1/agentear\n\
         event_module: agentear\ndata_dir: ~/.agent24/os/agentear/\n\
         impl_kind: attached_process\nkernel_capabilities: [events, models]\n",
    )
    .unwrap();
    // The enrolment socket lives in `home`; the data dir is `home/.agent24`.
    let out = Command::new(env!("CARGO_BIN_EXE_agent24"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .args(["os", "attach", "add", &manifest.to_string_lossy(), "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut child = out;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("CLI exceeded the 10 second test deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn assert_no_secret(stdout: &str, stderr: &str) {
    for secret in [TOKEN, GRANT] {
        assert!(
            !stdout.contains(secret),
            "secret leaked to stdout: {stdout}"
        );
        assert!(
            !stderr.contains(secret),
            "secret leaked to stderr: {stderr}"
        );
    }
}

fn error_body(stdout: &str) -> Value {
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout must be one line: {stdout:?}");
    let body: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(
        body.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["error"],
        "{body}"
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty())
    );
    body
}

fn script(elevate_replies: Vec<Value>) -> UdsScript {
    UdsScript {
        generation: GENERATION,
        handshake_error: None,
        elevate_replies,
        challenge: None,
        enrolled_port: None,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn capabilities_add_succeeds_after_elevation_with_a3_shape() {
    let home = tmp_home();
    let fake = start(
        HttpMode::ElevateThenCreate,
        script(pending_then(elevated())),
        home.path(),
    );
    let (ok, stdout, stderr) = run_add(home.path());
    assert!(ok, "stdout={stdout} stderr={stderr}");
    assert_no_secret(&stdout, &stderr);

    // A3 §3.6 snapshot: one line, exactly this field set, token masked.
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "{stdout:?}");
    let body: Value = serde_json::from_str(lines[0]).unwrap();
    let mut keys: Vec<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "manifest_digest",
            "name",
            "socket_path",
            "token",
            "token_id"
        ]
    );
    assert_eq!(body["name"], "agentear");
    assert_eq!(body["token"], ATTACH_TOKEN);
    assert!(body["token_id"].as_str().unwrap().starts_with("tok_"));

    // Waiting notice on stderr only.
    assert!(
        stderr.contains("Waiting for approval in the Agent24 app"),
        "{stderr}"
    );
    assert!(!stdout.contains("Waiting"));

    // Two POSTs: first bearer-only, second with the grant; same exact bytes.
    let posts = fake.posts.lock().unwrap();
    assert_eq!(posts.len(), 2);
    assert!(!posts[0].headers.contains_key("x-agent24-elevation"));
    assert_eq!(posts[0].headers["authorization"], format!("Bearer {TOKEN}"));
    assert_eq!(posts[1].headers["x-agent24-elevation"], GRANT);
    assert_eq!(posts[1].headers["authorization"], format!("Bearer {TOKEN}"));
    assert_eq!(posts[0].body, posts[1].body);

    // Wire frames: hello with daemon.json's generation echoing the challenge
    // nonce, then elevate whose hash is that of the exact body bytes the CLI
    // sent.
    let frames = fake.frames.lock().unwrap();
    assert_eq!(frames.len(), 2);
    assert_eq!(
        frames[0],
        json!({"v":1,"op":"hello","generation":GENERATION,"nonce":NONCE})
    );
    assert_eq!(frames[1]["op"], "elevate");
    assert_eq!(frames[1]["method"], "POST");
    assert_eq!(frames[1]["path"], "/api/v1/attached");
    assert_eq!(frames[1]["body_sha256"], sha256_hex(&posts[0].body));
    assert!(frames[1]["summary"].as_str().unwrap().chars().count() <= 200);
    let _ = fake.http_port;
}

#[test]
fn no_elevation_round_when_the_first_request_succeeds() {
    let home = tmp_home();
    let fake = start(HttpMode::CreateDirect, script(vec![]), home.path());
    let (ok, stdout, stderr) = run_add(home.path());
    assert!(ok, "{stdout} {stderr}");
    assert_eq!(fake.posts.lock().unwrap().len(), 1);
    assert_eq!(fake.frames.lock().unwrap().len(), 1, "only hello was sent");
    assert!(!stderr.contains("Waiting"));
}

#[test]
fn other_403_is_not_elevated() {
    let home = tmp_home();
    let fake = start(HttpMode::OtherForbidden, script(vec![]), home.path());
    let (ok, stdout, stderr) = run_add(home.path());
    assert!(!ok);
    assert_eq!(error_body(&stdout)["error"]["code"], "forbidden");
    assert_eq!(fake.frames.lock().unwrap().len(), 1);
    assert_no_secret(&stdout, &stderr);
}

fn assert_presence_failure(code: &str, expect_in_message: Option<&str>) {
    let home = tmp_home();
    let fake = start(
        HttpMode::ElevateThenCreate,
        script(pending_then(presence_error(code))),
        home.path(),
    );
    let (ok, stdout, stderr) = run_add(home.path());
    assert!(!ok, "{stdout} {stderr}");
    let body = error_body(&stdout);
    assert_eq!(body["error"]["code"], code);
    if let Some(needle) = expect_in_message {
        assert!(
            body["error"]["message"].as_str().unwrap().contains(needle),
            "{body}"
        );
    }
    // No retry after a refusal.
    assert_eq!(fake.posts.lock().unwrap().len(), 1);
    assert_no_secret(&stdout, &stderr);
}

#[test]
fn presence_denied_maps_to_json_error() {
    assert_presence_failure("presence_denied", None);
}

#[test]
fn presence_unavailable_tells_user_to_open_the_app() {
    assert_presence_failure("presence_unavailable", Some("desktop app"));
}

#[test]
fn presence_timeout_maps_to_json_error() {
    assert_presence_failure("presence_timeout", None);
}

#[test]
fn retry_happens_once_and_elevation_invalid_is_reported() {
    let home = tmp_home();
    let fake = start(
        HttpMode::AlwaysInvalid,
        script(pending_then(elevated())),
        home.path(),
    );
    let (ok, stdout, stderr) = run_add(home.path());
    assert!(!ok);
    assert_eq!(error_body(&stdout)["error"]["code"], "elevation_invalid");
    assert_eq!(fake.posts.lock().unwrap().len(), 2, "exactly one retry");
    assert_eq!(fake.frames.lock().unwrap().len(), 2, "exactly one elevate");
    assert_no_secret(&stdout, &stderr);
}

#[test]
fn generation_mismatch_is_reported_and_nothing_is_sent_over_http() {
    let home = tmp_home();
    let fake = start(
        HttpMode::ElevateThenCreate,
        UdsScript {
            generation: "some-other-generation",
            handshake_error: None,
            elevate_replies: vec![],
            challenge: None,
            enrolled_port: None,
        },
        home.path(),
    );
    let (ok, stdout, stderr) = run_add(home.path());
    assert!(!ok);
    assert_eq!(error_body(&stdout)["error"]["code"], "generation_mismatch");
    assert!(fake.posts.lock().unwrap().is_empty());
    assert_no_secret(&stdout, &stderr);
}

#[test]
fn identity_rejected_is_reported() {
    let home = tmp_home();
    let fake = start(
        HttpMode::ElevateThenCreate,
        UdsScript {
            generation: GENERATION,
            handshake_error: Some(presence_error("identity_rejected")),
            elevate_replies: vec![],
            challenge: None,
            enrolled_port: None,
        },
        home.path(),
    );
    let (ok, stdout, _) = run_add(home.path());
    assert!(!ok);
    assert_eq!(error_body(&stdout)["error"]["code"], "identity_rejected");
    assert!(fake.posts.lock().unwrap().is_empty());
}

#[test]
fn unreachable_enroll_socket_is_daemon_unavailable() {
    let home = tmp_home();
    let posts = Arc::new(Mutex::new(Vec::new()));
    let port = spawn_http(HttpMode::CreateDirect, Arc::clone(&posts));
    write_daemon_json(
        home.path(),
        &json!({"port": port, "pid": std::process::id(), "version": "9.9.9",
                "generation": GENERATION, "auth_mode": "capabilities",
                "protocol_version": agent24_protocol::PROTOCOL_VERSION,
                "enroll_endpoint": home.path().join("missing.sock")}),
    );
    let (ok, stdout, _) = run_add(home.path());
    assert!(!ok);
    assert_eq!(error_body(&stdout)["error"]["code"], "daemon_unavailable");
    assert!(posts.lock().unwrap().is_empty());
}

#[test]
fn capabilities_without_enroll_endpoint_keeps_host_authority_unavailable() {
    let home = tmp_home();
    let posts = Arc::new(Mutex::new(Vec::new()));
    let port = spawn_http(HttpMode::CreateDirect, Arc::clone(&posts));
    write_daemon_json(
        home.path(),
        &json!({"port": port, "pid": std::process::id(), "version": "9.9.9",
                "generation": GENERATION, "auth_mode": "capabilities",
                "protocol_version": agent24_protocol::PROTOCOL_VERSION}),
    );
    let (ok, stdout, stderr) = run_add(home.path());
    assert!(!ok);
    let body = error_body(&stdout);
    assert_eq!(body["error"]["code"], "daemon_unavailable");
    assert_eq!(body["error"]["message"], "host authority unavailable");
    assert!(stderr.contains("host authority unavailable"), "{stderr}");
    assert!(posts.lock().unwrap().is_empty());
}

#[test]
fn legacy_mode_is_unchanged_single_request_with_daemon_json_bearer() {
    let home = tmp_home();
    let posts = Arc::new(Mutex::new(Vec::new()));
    let port = spawn_http(HttpMode::CreateDirect, Arc::clone(&posts));
    write_daemon_json(
        home.path(),
        &json!({"port": port, "token": "legacy-bearer", "pid": std::process::id(),
                "version": "9.9.9", "generation": GENERATION,
                "protocol_version": agent24_protocol::PROTOCOL_VERSION}),
    );
    let (ok, stdout, stderr) = run_add(home.path());
    assert!(ok, "{stdout} {stderr}");
    let body: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(body["name"], "agentear");
    let posts = posts.lock().unwrap();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].headers["authorization"], "Bearer legacy-bearer");
    assert!(!posts[0].headers.contains_key("x-agent24-elevation"));
    assert!(
        stderr.is_empty(),
        "legacy prints nothing to stderr: {stderr}"
    );
}

#[test]
fn second_presence_required_stops_after_one_elevation() {
    let home = tmp_home();
    let fake = start(
        HttpMode::AlwaysPresenceRequired,
        script(pending_then(elevated())),
        home.path(),
    );
    let (ok, stdout, stderr) = run_add(home.path());
    assert!(!ok);
    assert_eq!(error_body(&stdout)["error"]["code"], "presence_required");
    assert_eq!(fake.posts.lock().unwrap().len(), 2);
    assert_eq!(
        fake.frames
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f["op"] == "elevate")
            .count(),
        1
    );
    assert_no_secret(&stdout, &stderr);
}

fn assert_bad_elevation(replies: Vec<Value>) {
    let home = tmp_home();
    let fake = start(HttpMode::ElevateThenCreate, script(replies), home.path());
    let (ok, stdout, stderr) = run_add(home.path());
    assert!(!ok, "{stdout} {stderr}");
    assert_eq!(error_body(&stdout)["error"]["code"], "bad_frame");
    assert_eq!(fake.posts.lock().unwrap().len(), 1);
    assert_no_secret(&stdout, &stderr);
}

#[test]
fn elevated_without_pending_is_rejected() {
    assert_bad_elevation(vec![elevated()]);
}
#[test]
fn pending_without_id_is_rejected() {
    assert_bad_elevation(vec![json!({"v":1,"op":"elevation_pending"}), elevated()]);
}
#[test]
fn elevated_without_id_is_rejected() {
    let mut frame = elevated();
    frame.as_object_mut().unwrap().remove("challenge_id");
    assert_bad_elevation(pending_then(frame));
}
#[test]
fn duplicate_pending_cannot_replace_challenge() {
    let mut replies = pending_then(elevated());
    replies.insert(
        0,
        json!({"v":1,"op":"elevation_pending","challenge_id":"different"}),
    );
    assert_bad_elevation(replies);
}
#[test]
fn mismatched_challenge_is_rejected() {
    let mut frame = elevated();
    frame["challenge_id"] = json!("different");
    assert_bad_elevation(pending_then(frame));
}

fn legacy_snapshot(status: &str, body: &str, declared_len: usize) -> (bool, String, String) {
    let home = tmp_home();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {declared_len}\r\nConnection: close\r\n\r\n{body}"
    );
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let (line, _, _) = read_http(&mut stream).unwrap();
            if line.starts_with("GET /api/v1/health") {
                respond(
                    &mut stream,
                    "200 OK",
                    &json!({"protocol_version":agent24_protocol::PROTOCOL_VERSION}),
                );
            } else {
                stream.write_all(response.as_bytes()).unwrap();
                break;
            }
        }
    });
    write_daemon_json(
        home.path(),
        &json!({"port":port,"token":"legacy-bearer","pid":std::process::id(),"version":"9.9.9","generation":GENERATION,"protocol_version":agent24_protocol::PROTOCOL_VERSION}),
    );
    run_add(home.path())
}

#[test]
fn legacy_success_bytes_snapshot() {
    let body = format!(
        r#"{{"name":"agentear","manifest_digest":"sha256:{}","token":"{ATTACH_TOKEN}","socket_path":"/x/attach/agent24d.sock","token_id":"tok_00000000"}}"#,
        "0".repeat(64)
    );
    let (ok, stdout, stderr) = legacy_snapshot("201 Created", &body, body.len());
    assert!(ok);
    assert_eq!(stdout.as_bytes(), format!("{body}\n").as_bytes());
    assert_eq!(stderr.as_bytes(), b"");
}

#[test]
fn legacy_invalid_json_bytes_snapshot() {
    let (ok, stdout, stderr) = legacy_snapshot("201 Created", "!", 1);
    assert!(!ok);
    assert_eq!(stdout.as_bytes(), b"{\"error\":{\"code\":\"daemon_unavailable\",\"message\":\"daemon returned an unreadable response: error decoding response body\"}}\n");
    assert_eq!(
        stderr.as_bytes(),
        b"error: daemon returned an unreadable response: error decoding response body\n"
    );
}

#[test]
fn legacy_truncated_body_bytes_snapshot() {
    let (ok, stdout, stderr) = legacy_snapshot("403 Forbidden", "{", 100);
    assert!(!ok);
    assert_eq!(stdout.as_bytes(), b"{\"error\":{\"code\":\"daemon_unavailable\",\"message\":\"daemon returned 403 Forbidden\"}}\n");
    assert_eq!(stderr.as_bytes(), b"error: daemon returned 403 Forbidden\n");
}

#[test]
fn not_elevated_operation_preserves_json_code_and_explains_limit() {
    assert_presence_failure(
        "not_elevated_operation",
        Some("This action can't be approved from the CLI yet"),
    );
}

#[test]
fn capabilities_delegate_elevates_on_enrolled_connection() {
    let home = tmp_home();
    let fake = start(
        HttpMode::ElevateThenCreate,
        script(pending_then(elevated())),
        home.path(),
    );
    let out = Command::new(env!("CARGO_BIN_EXE_agent24"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .args(["os", "delegate", "agentear"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let posts = fake.posts.lock().unwrap();
    assert_eq!(posts.len(), 2);
    assert!(posts[0].body.is_empty());
    assert_eq!(posts[0].body, posts[1].body);
    assert_eq!(posts[1].headers["x-agent24-elevation"], GRANT);
    let frames = fake.frames.lock().unwrap();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[1]["path"], "/api/v1/os/agentear/delegation");
    assert_eq!(frames[1]["body_sha256"], sha256_hex(b""));
}

/// A `challenge` without a usable nonce is a protocol violation: the CLI
/// reports `bad_frame` and never answers with a `hello`.
#[test]
fn bad_challenge_is_bad_frame_and_no_hello_is_sent() {
    let mut cases = Vec::new();
    let mut missing = challenge();
    missing.as_object_mut().unwrap().remove("nonce");
    cases.push(missing);
    for nonce in [
        json!(null),
        json!(""),
        json!(42),
        json!("00112233445566778899aabbccddeef"),
        json!("00112233445566778899aabbccddeeff00"),
        json!("zz112233445566778899aabbccddeeff"),
    ] {
        let mut frame = challenge();
        frame["nonce"] = nonce;
        cases.push(frame);
    }
    cases.push(json!({"v":1,"op":"enrolled","token":TOKEN}));
    for bad in cases {
        let home = tmp_home();
        let fake = start(
            HttpMode::CreateDirect,
            UdsScript {
                challenge: Some(bad.clone()),
                ..script(vec![])
            },
            home.path(),
        );
        let (ok, stdout, stderr) = run_add(home.path());
        assert!(!ok, "{bad}: {stdout} {stderr}");
        assert_eq!(error_body(&stdout)["error"]["code"], "bad_frame", "{bad}");
        // The fake records anything the CLI wrote before it hung up.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while fake.closed.load(Ordering::SeqCst) == 0 {
            assert!(std::time::Instant::now() < deadline, "fake never closed");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(fake.frames.lock().unwrap().is_empty(), "{bad}: hello sent");
        assert!(fake.posts.lock().unwrap().is_empty());
        assert_no_secret(&stdout, &stderr);
    }
}

/// #1446 F1: the HTTP side is an independent listener named only by
/// `daemon.json`. If the daemon that enrolled the CLI reports a different
/// HTTP port, the token must never reach that listener: no request carries a
/// credential, and nothing but the anonymous pre-enrolment health probe
/// (which carries no credential) arrives at all.
#[test]
fn enrolled_port_mismatch_sends_nothing_to_daemon_json_port() {
    let home = tmp_home();
    let posts = Arc::new(Mutex::new(Vec::new()));
    let requests: Requests = Arc::default();
    let listener_port = spawn_http_recording(
        HttpMode::CreateDirect,
        Arc::clone(&posts),
        Arc::clone(&requests),
    );
    // The enrolling daemon reports its own HTTP port (the fake's); daemon.json
    // is then rewritten to point at the independent listener instead.
    let fake = start(HttpMode::CreateDirect, script(vec![]), home.path());
    let path = home.path().join(".agent24/daemon.json");
    let mut record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(record["port"], fake.http_port);
    assert_ne!(fake.http_port, listener_port);
    record["port"] = json!(listener_port);
    write_daemon_json(home.path(), &record);

    let (ok, stdout, stderr) = run_add(home.path());
    assert!(!ok, "{stdout} {stderr}");
    assert_eq!(error_body(&stdout)["error"]["code"], "endpoint_mismatch");
    assert_no_secret(&stdout, &stderr);
    let seen = requests.lock().unwrap();
    assert!(
        seen.iter()
            .all(|(line, headers)| line.starts_with("GET /api/v1/health")
                && !headers.contains_key("authorization")
                && !headers.contains_key("x-agent24-elevation")),
        "{seen:?}"
    );
    assert!(posts.lock().unwrap().is_empty());
    assert!(fake.requests.lock().unwrap().is_empty());
    assert!(
        fake.frames
            .lock()
            .unwrap()
            .iter()
            .all(|f| f["op"] == "hello")
    );
}

/// An `enrolled` frame naming any other port (0 is what a daemon with no
/// HTTP port to report sends) is refused the same way.
#[test]
fn enrolled_frame_must_name_daemon_json_port() {
    for reported in [Some(0), Some(1)] {
        let home = tmp_home();
        let fake = start(
            HttpMode::CreateDirect,
            UdsScript {
                enrolled_port: reported,
                ..script(vec![])
            },
            home.path(),
        );
        let (ok, stdout, stderr) = run_add(home.path());
        assert!(!ok, "{stdout} {stderr}");
        assert_eq!(error_body(&stdout)["error"]["code"], "endpoint_mismatch");
        assert!(fake.posts.lock().unwrap().is_empty());
        assert!(
            fake.requests
                .lock()
                .unwrap()
                .iter()
                .all(|(line, h)| line.starts_with("GET /api/v1/health")
                    && !h.contains_key("authorization"))
        );
        assert_no_secret(&stdout, &stderr);
    }
}

// ---- I-1: `agent24 idoris` over the enrolment session -------------------

/// One request the fake iDoris HTTP side saw.
#[derive(Clone)]
struct IdorisSeen {
    line: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

/// A fake capabilities daemon for `/api/v1/idoris/*`: status and operation
/// progress are everyday (bearer only); install needs a matching grant.
fn spawn_idoris_http(seen: Arc<Mutex<Vec<IdorisSeen>>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let Some((line, headers, body)) = read_http(&mut stream) else {
                continue;
            };
            let line = line.trim_end().to_owned();
            if line.starts_with("GET /api/v1/health") {
                respond(
                    &mut stream,
                    "200 OK",
                    &json!({"protocol_version": agent24_protocol::PROTOCOL_VERSION}),
                );
                continue;
            }
            seen.lock().unwrap().push(IdorisSeen {
                line: line.clone(),
                headers: headers.clone(),
                body,
            });
            if headers.get("authorization").map(String::as_str) != Some(&format!("Bearer {TOKEN}"))
            {
                respond(
                    &mut stream,
                    "401 Unauthorized",
                    &json!({"error":{"code":"unauthorized","message":"bad bearer"}}),
                );
                continue;
            }
            let op = |phase: &str, result: Option<&str>| {
                json!({"operation_id":"op-12","action":"install","phase":phase,
                       "result":result,"error_code":null,"cancellable":result.is_none(),
                       "idempotency_key":"internal-key"})
            };
            if line.starts_with("GET /api/v1/idoris/status ") {
                respond(
                    &mut stream,
                    "200 OK",
                    &json!({"state":"not_installed","version":null,"data_port":null,
                            "admin_port":null,"roles":[],"last_error":null,
                            "current_operation":null}),
                );
            } else if line.starts_with("GET /api/v1/idoris/operations/op-12 ") {
                respond(&mut stream, "200 OK", &op("installed", Some("succeeded")));
            } else if line.starts_with("POST /api/v1/idoris/install ") {
                if headers.get("x-agent24-elevation").map(String::as_str) == Some(GRANT) {
                    respond(&mut stream, "202 Accepted", &op("downloading", None));
                } else {
                    respond(
                        &mut stream,
                        "403 Forbidden",
                        &json!({"error":{"code":"presence_required","message":"Presence confirmation required","operation":"IdorisLifecycle"}}),
                    );
                }
            } else {
                respond(
                    &mut stream,
                    "404 Not Found",
                    &json!({"error":{"code":"not_found","message":line}}),
                );
            }
        }
    });
    port
}

struct IdorisFake {
    seen: Arc<Mutex<Vec<IdorisSeen>>>,
    frames: Arc<Mutex<Vec<Value>>>,
}

fn start_idoris(script: UdsScript, home: &Path) -> IdorisFake {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let frames = Arc::new(Mutex::new(Vec::new()));
    let http_port = spawn_idoris_http(Arc::clone(&seen));
    let sock = home.join("enroll.sock");
    serve_uds(
        UnixListener::bind(&sock).unwrap(),
        http_port,
        script,
        Arc::clone(&frames),
        Arc::new(AtomicUsize::new(0)),
    );
    write_daemon_json(
        home,
        &json!({"port": http_port, "pid": std::process::id(), "version": "9.9.9",
                "generation": GENERATION, "auth_mode": "capabilities",
                "protocol_version": agent24_protocol::PROTOCOL_VERSION,
                "enroll_endpoint": sock}),
    );
    IdorisFake { seen, frames }
}

/// Run `agent24 idoris <args>`. A spawned daemon would mask the result, so
/// `AGENT24D_BIN` points nowhere.
fn run_idoris(home: &Path, args: &[&str], path: &std::ffi::OsStr) -> (bool, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent24"))
        .env_clear()
        .env("PATH", path)
        .env("HOME", home)
        .env("AGENT24D_BIN", home.join("no-spawned-daemon"))
        .arg("idoris")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("CLI exceeded the 15 second test deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn os_path() -> std::ffi::OsString {
    std::env::var_os("PATH").unwrap_or_default()
}

#[test]
fn idoris_status_is_everyday_with_the_enrolled_token() {
    let home = tmp_home();
    let fake = start_idoris(script(vec![]), home.path());
    let (ok, stdout, stderr) = run_idoris(home.path(), &["status", "--json"], &os_path());
    assert!(ok, "{stdout} {stderr}");
    let body: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(body["state"], "not_installed");
    let seen = fake.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].line.starts_with("GET /api/v1/idoris/status "));
    assert!(!seen[0].headers.contains_key("x-agent24-elevation"));
    let frames = fake.frames.lock().unwrap();
    assert!(frames.iter().all(|f| f["op"] != "elevate"), "{frames:?}");
    assert!(!stderr.contains("Waiting for approval"), "{stderr}");
    assert_no_secret(&stdout, &stderr);
}

#[test]
fn idoris_install_elevates_once_on_the_enrolled_connection() {
    let home = tmp_home();
    let fake = start_idoris(script(pending_then(elevated())), home.path());
    let (ok, stdout, stderr) = run_idoris(home.path(), &["install", "--yes", "--json"], &os_path());
    assert!(ok, "{stdout} {stderr}");
    let body: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(body["result"], "succeeded");
    assert!(body.get("idempotency_key").is_none(), "{body}");
    assert!(
        stderr.contains("Waiting for approval in the Agent24 app"),
        "{stderr}"
    );
    let seen = fake.seen.lock().unwrap();
    let lines: Vec<_> = seen.iter().map(|s| s.line.as_str()).collect();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[0].starts_with("POST /api/v1/idoris/install "));
    assert!(lines[1].starts_with("POST /api/v1/idoris/install "));
    // Progress polling is everyday: no second approval.
    assert!(lines[2].starts_with("GET /api/v1/idoris/operations/op-12 "));
    assert!(!seen[0].headers.contains_key("x-agent24-elevation"));
    assert_eq!(seen[1].headers["x-agent24-elevation"], GRANT);
    assert!(!seen[2].headers.contains_key("x-agent24-elevation"));
    // The approved request is the rejected one: same bytes, same key.
    assert_eq!(seen[0].body, br#"{"consent":true}"#);
    assert_eq!(seen[0].body, seen[1].body);
    assert_eq!(
        seen[0].headers["idempotency-key"],
        seen[1].headers["idempotency-key"]
    );
    let frames = fake.frames.lock().unwrap();
    let elevates: Vec<_> = frames.iter().filter(|f| f["op"] == "elevate").collect();
    assert_eq!(elevates.len(), 1);
    assert_eq!(elevates[0]["method"], "POST");
    assert_eq!(elevates[0]["path"], "/api/v1/idoris/install");
    assert_eq!(elevates[0]["body_sha256"], sha256_hex(&seen[0].body));
    assert_no_secret(&stdout, &stderr);
}

#[test]
fn idoris_install_denied_in_the_app_is_presence_denied() {
    let home = tmp_home();
    let fake = start_idoris(
        script(pending_then(presence_error("presence_denied"))),
        home.path(),
    );
    let (ok, stdout, stderr) = run_idoris(home.path(), &["install", "--yes", "--json"], &os_path());
    assert!(!ok);
    let body = error_body(&stdout);
    assert_eq!(body["error"]["code"], "presence_denied");
    assert!(stderr.contains("denied in the Agent24 app"), "{stderr}");
    assert_eq!(
        fake.seen.lock().unwrap().len(),
        1,
        "no retry without a grant"
    );
    assert_no_secret(&stdout, &stderr);
}

#[test]
fn idoris_enrolment_failures_keep_their_codes() {
    for (code, mut script) in [
        ("identity_rejected", script(vec![])),
        ("endpoint_mismatch", script(vec![])),
    ] {
        if code == "identity_rejected" {
            script.handshake_error = Some(presence_error("identity_rejected"));
        } else {
            script.enrolled_port = Some(1);
        }
        let home = tmp_home();
        let fake = start_idoris(script, home.path());
        let (ok, stdout, stderr) = run_idoris(home.path(), &["status", "--json"], &os_path());
        assert!(!ok, "{code}");
        assert_eq!(error_body(&stdout)["error"]["code"], code, "{stdout}");
        assert!(fake.seen.lock().unwrap().is_empty(), "{code}: nothing sent");
        assert_no_secret(&stdout, &stderr);
    }
}

#[test]
fn idoris_unanswering_capabilities_daemon_is_daemon_unavailable() {
    let home = tmp_home();
    // A port nothing listens on: bind, read the port, then close it.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    write_daemon_json(
        home.path(),
        &json!({"port": port, "pid": std::process::id(), "version": "9.9.9",
                "generation": GENERATION, "auth_mode": "capabilities",
                "protocol_version": agent24_protocol::PROTOCOL_VERSION,
                "enroll_endpoint": home.path().join("enroll.sock")}),
    );
    let (ok, stdout, _) = run_idoris(home.path(), &["status", "--json"], &os_path());
    assert!(!ok);
    assert_eq!(error_body(&stdout)["error"]["code"], "daemon_unavailable");
}

/// The 10-29 `daemon_unavailable` report: liveness of the recorded pid was
/// probed with a bare `ps`, so a PATH without /bin made the running
/// capabilities daemon look dead and the CLI tried to spawn a second one.
#[test]
fn idoris_finds_the_running_daemon_without_ps_on_path() {
    let home = tmp_home();
    let fake = start_idoris(script(vec![]), home.path());
    let empty = home.path().join("empty-bin");
    std::fs::create_dir_all(&empty).unwrap();
    let (ok, stdout, stderr) = run_idoris(home.path(), &["status", "--json"], empty.as_os_str());
    assert!(ok, "{stdout} {stderr}");
    assert_eq!(fake.seen.lock().unwrap().len(), 1);
}
