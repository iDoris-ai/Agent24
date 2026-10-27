//! The CLI against a daemon it finds in `daemon.json`, end to end through
//! `main`: `chat --session` sends the session and prints the daemon's memory
//! receipt (reviews of #1326, M1/M2); `runs` only ever talks to a resident
//! daemon, never an ephemeral one it would start (review of #1311, Low-1).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

fn tmp_home() -> tempfile::TempDir {
    let home = tempfile::Builder::new()
        .prefix("a24res")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::create_dir(home.path().join("tmp")).unwrap();
    home
}

/// `agent24 ARGS` with only the data dir set; a daemon it would start is a
/// script that leaves a marker and exits.
fn cli(home: &Path, args: &[&str]) -> Output {
    let fake = home.join("fake-agent24d.sh");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nprintf started > '{}'\nexit 1\n",
            home.join("spawned").display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    Command::new(env!("CARGO_BIN_EXE_agent24"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("TMPDIR", home.join("tmp"))
        .env("AGENT24_DATA_DIR", home.join("data"))
        .env("AGENT24D_BIN", &fake)
        .args(args)
        .output()
        .unwrap()
}

/// A daemon on 127.0.0.1 that answers health; `/api/v1/chat` with
/// `chat_reply` and `/api/v1/runs` with no runs, both only with the bearer
/// `daemon.json` gives (401 otherwise, as the daemon does). The chat bodies
/// and the request lines it got come back through the handle.
struct FakeDaemon {
    port: u16,
    chats: Arc<Mutex<Vec<serde_json::Value>>>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl FakeDaemon {
    fn start(chat_reply: serde_json::Value) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let chats = Arc::new(Mutex::new(Vec::new()));
        let seen = chats.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let lines = requests.clone();
        // Serves for as long as the test process lives: no deadline for a
        // first CLI start that a malware scan slows down.
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let mut reader = BufReader::new(stream);
                let (head, body) = read_request(&mut reader);
                let line = head.lines().next().unwrap_or("").to_owned();
                lines.lock().unwrap().push(line.clone());
                let bearer = head
                    .lines()
                    .any(|l| l.eq_ignore_ascii_case("authorization: Bearer t"));
                let (status, reply) = if line.starts_with("GET /api/v1/health ") {
                    ("200 OK", r#"{"status":"ok"}"#.to_owned())
                } else if !bearer {
                    (
                        "401 Unauthorized",
                        r#"{"error":{"code":"unauthorized"}}"#.to_owned(),
                    )
                } else if line.starts_with("POST /api/v1/chat ") {
                    seen.lock()
                        .unwrap()
                        .push(serde_json::from_slice(&body).unwrap());
                    ("200 OK", chat_reply.to_string())
                } else if line.starts_with("GET /api/v1/runs ") {
                    ("200 OK", r#"{"runs":[]}"#.to_owned())
                } else {
                    (
                        "404 Not Found",
                        r#"{"error":{"code":"not_found"}}"#.to_owned(),
                    )
                };
                let _ = write!(
                    reader.get_mut(),
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
            }
        });
        Self {
            port,
            chats,
            requests,
        }
    }

    /// `daemon.json` naming this daemon, alive as the test process.
    fn discoverable_in(&self, home: &Path) {
        write_state(
            home,
            serde_json::json!({
                "port": self.port, "token": "t", "pid": std::process::id(),
                "version": "0.0.0", "generation": "g",
            }),
        );
    }
}

fn write_state(home: &Path, state: serde_json::Value) {
    let data = home.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("daemon.json"), state.to_string()).unwrap();
}

/// The head, then as many body bytes as its content-length says.
fn read_request(reader: &mut BufReader<std::net::TcpStream>) -> (String, Vec<u8>) {
    let mut head = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
            break;
        }
        head.push_str(&line);
    }
    let length = head
        .lines()
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    (head, body)
}

fn chat_reply(receipt: Option<&str>) -> serde_json::Value {
    let mut reply = serde_json::json!({
        "message": {"role": "assistant", "content": "ok"},
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
    });
    if let Some(receipt) = receipt {
        reply["memory_receipt"] = receipt.into();
    }
    reply
}

#[test]
fn chat_sends_its_session_and_prints_the_receipt() {
    let home = tmp_home();
    let daemon = FakeDaemon::start(chat_reply(Some("paused_not_saved")));
    daemon.discoverable_in(home.path());

    let out = cli(
        home.path(),
        &["chat", "记住我喜欢茶", "--session", "chat_1"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let chats = daemon.chats.lock().unwrap();
    assert_eq!(chats.len(), 1, "{chats:?}");
    assert_eq!(chats[0]["session_id"], "chat_1", "{}", chats[0]);
    assert_eq!(chats[0]["messages"][0]["content"], "记住我喜欢茶");
    assert!(
        stdout.contains("· not remembered: personal memory is paused"),
        "{stdout}"
    );
}

#[test]
fn a_chat_with_no_receipt_prints_none() {
    let home = tmp_home();
    let daemon = FakeDaemon::start(chat_reply(None));
    daemon.discoverable_in(home.path());

    let out = cli(home.path(), &["chat", "hi"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        daemon.chats.lock().unwrap()[0]["session_id"],
        serde_json::Value::Null
    );
    assert!(!stdout.contains("remembered"), "{stdout}");
}

#[test]
fn an_empty_session_is_refused_before_anything_is_sent() {
    let home = tmp_home();
    let out = cli(home.path(), &["chat", "hi", "--session", ""]);
    assert!(!out.status.success());
    assert!(
        !home.path().join("spawned").exists(),
        "a daemon was started"
    );
}

#[test]
fn runs_with_no_resident_daemon_says_so_and_starts_none() {
    let home = tmp_home();
    let out = cli(home.path(), &["runs", "list"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("daemon not running"), "{stderr}");
    assert!(
        !home.path().join("spawned").exists(),
        "an ephemeral daemon was started"
    );
}

#[test]
fn runs_on_a_capability_daemon_is_refused_not_started_elsewhere() {
    let home = tmp_home();
    write_state(
        home.path(),
        serde_json::json!({
            "port": 1, "pid": std::process::id(), "version": "0.0.0",
            "generation": "g", "auth_mode": "capabilities",
        }),
    );
    let out = cli(home.path(), &["runs", "list"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("host authority unavailable"), "{stderr}");
    assert!(
        !home.path().join("spawned").exists(),
        "an ephemeral daemon was started"
    );
}

#[test]
fn runs_lists_what_the_resident_daemon_has() {
    let home = tmp_home();
    let daemon = FakeDaemon::start(chat_reply(None));
    daemon.discoverable_in(home.path());

    let out = cli(home.path(), &["runs", "list"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout.trim(), "(no runs)");
    let requests = daemon.requests.lock().unwrap();
    assert!(
        requests.iter().any(|l| l.starts_with("GET /api/v1/runs ")),
        "{requests:?}"
    );
    assert!(
        !home.path().join("spawned").exists(),
        "an ephemeral daemon was started"
    );
}

/// A state file whose pid is alive but whose port answers nothing is no
/// resident daemon: `runs` says so and does not start one of its own.
#[test]
fn a_state_file_with_nothing_on_its_port_is_no_daemon() {
    let home = tmp_home();
    let closed = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    write_state(
        home.path(),
        serde_json::json!({
            "port": closed, "token": "t", "pid": std::process::id(),
            "version": "0.0.0", "generation": "g",
        }),
    );
    let out = cli(home.path(), &["runs", "list"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("daemon not running"), "{stderr}");
    assert!(
        !home.path().join("spawned").exists(),
        "an ephemeral daemon was started"
    );
}
