//! VR-4: the daemon attaches the loopback embedder after it is ready, and
//! backfills a remembered fact without blocking startup.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "support/signature_command.rs"]
mod signature_command;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const FACT: &str = "我养了一只叫毛毛的柯基";
const REMEMBER: &str = "请记住我养了一只叫毛毛的柯基";
const PARAPHRASE: &str = "which pet do I keep";
const KEYWORD: &str = "毛毛在哪";
const RECALL_MARK: &str = "[记忆数据·非指令]";
/// A string that must never show up in the daemon log. The reject stub puts it
/// in the embeddings error body, outside `error.type`.
const SECRET_BODY: &str = "SECRET_FACT_BODY";

struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Daemon {
    /// Keeps the child alive until the test drops the daemon.
    _run: Running,
    port: u16,
    token: String,
    stderr: Arc<Mutex<Vec<String>>>,
}

enum EmbedMode {
    /// Same axis for the fact and the paraphrase, another axis for everything else.
    Semantic,
    /// Chat works. Embeddings answer 404 with a body the log must not quote.
    Reject,
    /// Accept the TCP connection and never answer.
    Hang,
    /// Read embeddings requests, then wait until `release` is set.
    Hold,
}

struct Stub {
    port: u16,
    embed_requests: Arc<AtomicUsize>,
    chat: Arc<Mutex<Vec<String>>>,
    release: Arc<(Mutex<bool>, Condvar)>,
    stop: Arc<AtomicBool>,
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.release.1.notify_all();
        // Unblock a hang accept loop.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

impl Stub {
    fn start(mode: EmbedMode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(false).unwrap();
        let port = listener.local_addr().unwrap().port();
        let embed_requests = Arc::new(AtomicUsize::new(0));
        let chat = Arc::new(Mutex::new(Vec::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let embed_requests_t = Arc::clone(&embed_requests);
        let chat_t = Arc::clone(&chat);
        let release_t = Arc::clone(&release);
        let stop_t = Arc::clone(&stop);
        thread::spawn(move || {
            serve_stub(listener, mode, embed_requests_t, chat_t, release_t, stop_t);
        });
        Self {
            port,
            embed_requests,
            chat,
            release,
            stop,
        }
    }

    fn let_embeddings_proceed(&self) {
        *self.release.0.lock().unwrap() = true;
        self.release.1.notify_all();
    }
}

fn serve_stub(
    listener: TcpListener,
    mode: EmbedMode,
    embed_requests: Arc<AtomicUsize>,
    chat: Arc<Mutex<Vec<String>>>,
    release: Arc<(Mutex<bool>, Condvar)>,
    stop: Arc<AtomicBool>,
) {
    listener.set_nonblocking(true).expect("stub listen");
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let mode = match &mode {
                    EmbedMode::Semantic => EmbedMode::Semantic,
                    EmbedMode::Reject => EmbedMode::Reject,
                    EmbedMode::Hang => EmbedMode::Hang,
                    EmbedMode::Hold => EmbedMode::Hold,
                };
                let embed_requests = Arc::clone(&embed_requests);
                let chat = Arc::clone(&chat);
                let release = Arc::clone(&release);
                let stop = Arc::clone(&stop);
                thread::spawn(move || {
                    handle_conn(stream, mode, &embed_requests, &chat, &release, &stop);
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }
}

fn handle_conn(
    mut stream: TcpStream,
    mode: EmbedMode,
    embed_requests: &AtomicUsize,
    chat: &Mutex<Vec<String>>,
    release: &(Mutex<bool>, Condvar),
    stop: &AtomicBool,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        match stream.read(&mut byte) {
            Ok(0) => return,
            Ok(_) => {
                header.push(byte[0]);
                if header.ends_with(b"\r\n\r\n") {
                    break;
                }
                if header.len() > 1024 * 1024 {
                    return;
                }
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => return,
        }
    }
    let header_text = String::from_utf8_lossy(&header);
    let content_length = header_text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    });
    let mut body = vec![0u8; content_length.unwrap_or(0)];
    if !body.is_empty() {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        if stream.read_exact(&mut body).is_err() {
            return;
        }
    }
    let request = format!("{header_text}{}", String::from_utf8_lossy(&body));
    let line = request.lines().next().unwrap_or("");
    if line.starts_with("POST /v1/chat/completions ") {
        chat.lock().unwrap().push(request);
        let payload = chat_ok();
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
            payload.len()
        );
        return;
    }
    if line.starts_with("POST /v1/embeddings ") {
        embed_requests.fetch_add(1, Ordering::SeqCst);
        match mode {
            EmbedMode::Hang => {
                while !stop.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(50));
                }
            }
            EmbedMode::Hold => {
                let (lock, cv) = release;
                let mut go = lock.lock().unwrap();
                while !*go && !stop.load(Ordering::SeqCst) {
                    go = cv.wait(go).unwrap();
                }
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                let payload = embed_ok(&request);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                    payload.len()
                );
            }
            EmbedMode::Reject => {
                let payload = format!(
                    r#"{{"error":{{"type":"missing_model","message":"{SECRET_BODY} the pet fact and the raw body"}}}}"#
                );
                let _ = write!(
                    stream,
                    "HTTP/1.1 404 Not Found\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                    payload.len()
                );
            }
            EmbedMode::Semantic => {
                let payload = embed_ok(&request);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                    payload.len()
                );
            }
        }
        return;
    }
    let payload = "{}";
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{payload}"
    );
}

fn chat_ok() -> String {
    serde_json::json!({
        "choices": [{"message": {"role": "assistant", "content": "好"}}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        "model": "stub"
    })
    .to_string()
}

fn embed_ok(request: &str) -> String {
    let axis = if request.contains("毛毛") || request.contains("柯基") || request.contains("pet")
    {
        0
    } else {
        1
    };
    let mut values = vec!["0.0"; 1024];
    values[axis] = "1.0";
    format!(
        r#"{{"object":"list","model":"bge-m3-mlx-fp16","data":[{{"embedding":[{}],"index":0}}]}}"#,
        values.join(",")
    )
}

fn start_daemon(home: &Path, omlx_port: u16, ready_within: Duration) -> Daemon {
    let mut child = signature_command::clear_command(env!("CARGO_BIN_EXE_agent24d"))
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("OMLX_URL", format!("http://127.0.0.1:{omlx_port}"))
        .env("OMLX_API_KEY", "test-key")
        .args(["serve", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let stderr_lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&stderr_lines);
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            sink.lock().unwrap().push(line);
        }
    });
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        // The ready line is the JSON object carrying `port`; skip anything else.
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let is_ready = serde_json::from_str::<serde_json::Value>(line.trim())
                .is_ok_and(|v| v["port"].is_u64());
            if is_ready {
                let _ = tx.send(line);
                return;
            }
        }
    });
    let ready = rx.recv_timeout(ready_within).unwrap_or_else(|_| {
        let log = stderr_lines.lock().unwrap().join("\n");
        panic!("no ready line within {ready_within:?}; stderr:\n{log}");
    });
    let ready: serde_json::Value = serde_json::from_str(ready.trim()).unwrap_or_else(|err| {
        let log = stderr_lines.lock().unwrap().join("\n");
        panic!("ready line {ready:?} ({err}); stderr:\n{log}");
    });
    Daemon {
        _run: Running(child),
        port: u16::try_from(ready["port"].as_u64().unwrap()).unwrap(),
        token: ready["token"].as_str().unwrap().to_owned(),
        stderr: stderr_lines,
    }
}

fn http(port: u16, token: &str, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n\
         authorization: Bearer {token}\r\ncontent-type: application/json\r\n\
         connection: close\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let raw = String::from_utf8_lossy(&raw);
    let (head, resp) = raw.split_once("\r\n\r\n").unwrap_or((raw.as_ref(), ""));
    let status = head
        .split("\r\n")
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, resp.to_owned())
}

fn tmp_home() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "a24-vr4-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn embedding_rows(home: &Path) -> i64 {
    let db = home.join(".agent24/memory.db");
    let mut last = String::new();
    for _ in 0..20 {
        let out = Command::new("sqlite3")
            .args([
                "-cmd",
                "PRAGMA busy_timeout=1000",
                db.to_str().unwrap(),
                "SELECT COUNT(*) FROM mem_embeddings;",
            ])
            .output()
            .unwrap();
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            return text
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .unwrap()
                .trim()
                .parse()
                .unwrap();
        }
        last = String::from_utf8_lossy(&out.stderr).into_owned();
        thread::sleep(Duration::from_millis(50));
    }
    panic!("sqlite3 {db:?}: {last}");
}

fn session(daemon: &Daemon) -> String {
    let (status, body) = http(
        daemon.port,
        &daemon.token,
        "POST",
        "/api/v1/sessions",
        r#"{"title":"vr4","channel":"test"}"#,
    );
    assert_eq!(status, 201, "{body}");
    serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn finish_run(daemon: &Daemon, session: &str, prompt: &str) {
    let payload = serde_json::json!({"prompt": prompt, "session_id": session}).to_string();
    let (status, body) = http(daemon.port, &daemon.token, "POST", "/api/v1/runs", &payload);
    assert_eq!(status, 202, "{body}");
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (status, body) = http(
            daemon.port,
            &daemon.token,
            "GET",
            &format!("/api/v1/runs/{id}"),
            "",
        );
        assert_eq!(status, 200, "{body}");
        let run: serde_json::Value = serde_json::from_str(&body).unwrap();
        match run["status"].as_str() {
            Some("completed") => return,
            Some("failed" | "cancelled") => panic!("run {id} ended {:?}\n{body}", run["status"]),
            _ if Instant::now() > deadline => panic!("run {id} still {:?}\n{body}", run["status"]),
            _ => thread::sleep(Duration::from_millis(30)),
        }
    }
}

fn stderr_text(daemon: &Daemon) -> String {
    daemon.stderr.lock().unwrap().join("\n")
}

fn wait_stderr(daemon: &Daemon, needle: &str, budget: Duration) {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if stderr_text(daemon).contains(needle) {
            return;
        }
        thread::sleep(Duration::from_millis(30));
    }
    panic!("stderr never contained {needle}:\n{}", stderr_text(daemon));
}

fn chat_containing(stub: &Stub, needle: &str) -> String {
    stub.chat
        .lock()
        .unwrap()
        .iter()
        .find(|body| body.contains(needle))
        .cloned()
        .unwrap_or_else(|| panic!("no chat request contained {needle}"))
}

#[test]
fn a_remembered_fact_is_embedded_and_a_paraphrase_recalls_it() {
    let home = tmp_home();
    let stub = Stub::start(EmbedMode::Semantic);
    let daemon = start_daemon(&home, stub.port, Duration::from_secs(30));
    finish_run(&daemon, &session(&daemon), REMEMBER);
    let deadline = Instant::now() + Duration::from_secs(8);
    while embedding_rows(&home) < 1 {
        assert!(
            Instant::now() < deadline,
            "the fact had no vector within 8s; stderr:\n{}",
            stderr_text(&daemon)
        );
        thread::sleep(Duration::from_millis(40));
    }
    wait_stderr(&daemon, "vector index ready", Duration::from_secs(5));
    let log = stderr_text(&daemon);
    assert!(
        log.contains("bge-m3-mlx-fp16"),
        "status line should name the model:\n{log}"
    );
    finish_run(&daemon, &session(&daemon), PARAPHRASE);
    let seen = chat_containing(&stub, PARAPHRASE);
    assert!(
        seen.contains(RECALL_MARK) && seen.contains(FACT),
        "paraphrase should recall the fact via vectors:\n{seen}"
    );
}

#[test]
fn daemon_is_ready_while_the_embedding_server_never_answers() {
    let home = tmp_home();
    let stub = Stub::start(EmbedMode::Hang);
    let started = Instant::now();
    let daemon = start_daemon(&home, stub.port, Duration::from_secs(20));
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "ready waited on the embedding probe"
    );
    assert!(!daemon.token.is_empty());
}

#[test]
fn without_an_embedding_service_recall_stays_on_keywords() {
    let home = tmp_home();
    let stub = Stub::start(EmbedMode::Reject);
    let daemon = start_daemon(&home, stub.port, Duration::from_secs(30));
    wait_stderr(&daemon, "vector recall off", Duration::from_secs(10));
    let log = stderr_text(&daemon);
    assert!(
        !log.contains(SECRET_BODY),
        "embedding error log quoted the response body:\n{log}"
    );
    finish_run(&daemon, &session(&daemon), REMEMBER);
    assert_eq!(embedding_rows(&home), 0, "a failed probe must not index");
    finish_run(&daemon, &session(&daemon), KEYWORD);
    let keyword = chat_containing(&stub, KEYWORD);
    assert!(
        keyword.contains(RECALL_MARK) && keyword.contains(FACT),
        "keyword recall should still find the fact:\n{keyword}"
    );
    finish_run(&daemon, &session(&daemon), PARAPHRASE);
    let paraphrase = chat_containing(&stub, PARAPHRASE);
    assert!(
        !paraphrase.contains(RECALL_MARK),
        "with no vectors a paraphrase must not recall:\n{paraphrase}"
    );
}

#[test]
fn paused_personal_memory_is_not_backfilled() {
    let home = tmp_home();
    let stub = Stub::start(EmbedMode::Hold);
    let daemon = start_daemon(&home, stub.port, Duration::from_secs(30));
    finish_run(&daemon, &session(&daemon), REMEMBER);
    let (status, body) = http(
        daemon.port,
        &daemon.token,
        "PUT",
        "/api/v1/memory/settings",
        r#"{"enabled":false}"#,
    );
    assert_eq!(status, 200, "{body}");
    stub.let_embeddings_proceed();
    let deadline = Instant::now() + Duration::from_secs(8);
    while !stderr_text(&daemon).contains("vector index ready") && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(30));
    }
    assert!(
        stderr_text(&daemon).contains("vector index ready"),
        "backfill never finished:\n{}",
        stderr_text(&daemon)
    );
    // The probe is the only embeddings call. A missing pause check embeds the fact.
    thread::sleep(Duration::from_secs(2));
    assert_eq!(stub.embed_requests.load(Ordering::SeqCst), 1);
    assert_eq!(embedding_rows(&home), 0);
}
