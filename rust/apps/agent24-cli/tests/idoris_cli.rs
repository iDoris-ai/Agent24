#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Step {
    method: &'static str,
    path: &'static str,
    status: u16,
    body: Value,
    headers: &'static str,
}
fn step(method: &'static str, path: &'static str, body: Value) -> Step {
    Step {
        method,
        path,
        status: 200,
        body,
        headers: "",
    }
}
fn operation(action: &str, phase: &str, result: Option<&str>) -> Value {
    json!({"operation_id":"op-12", "action":action, "phase":phase,
        "result":result, "error_code":null, "cancellable":result.is_none(),
        "idempotency_key":"internal-key", "api_key":"sk-secret-sentinel"})
}
struct Fixture {
    home: tempfile::TempDir,
    server: Option<std::thread::JoinHandle<Vec<(String, Value)>>>,
}
impl Fixture {
    fn new(steps: Vec<Step>) -> Self {
        let home = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for step in steps {
                let deadline = Instant::now() + Duration::from_secs(15);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                Instant::now() < deadline,
                                "missing {} {}",
                                step.method,
                                step.path
                            );
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("{error}"),
                    }
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let (head, body) = loop {
                    let mut buf = [0; 4096];
                    let n = socket.read(&mut buf).unwrap();
                    assert_ne!(n, 0);
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8(bytes[..end].to_vec()).unwrap();
                        let len: usize = head
                            .lines()
                            .find_map(|line| {
                                line.split_once(':')
                                    .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                                    .map(|(_, v)| v.trim().parse().unwrap())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + len {
                            let body = if len == 0 {
                                Value::Null
                            } else {
                                serde_json::from_slice(&bytes[end + 4..end + 4 + len]).unwrap()
                            };
                            break (head, body);
                        }
                    }
                };
                assert!(
                    head.starts_with(&format!("{} {} HTTP/1.1", step.method, step.path)),
                    "{head}"
                );
                assert!(
                    head.to_lowercase()
                        .contains("authorization: bearer fixture")
                );
                requests.push((head, body));
                if step.status == 0 {
                    continue;
                }
                let body = step.body.to_string();
                write!(socket, "HTTP/1.1 {} Fixture\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n{}\r\n{body}", step.status, body.len(), step.headers).unwrap();
            }
            requests
        });
        let ready = json!({"type":"ready", "port":port, "token":"fixture", "version":"fixture"});
        let daemon = home.path().join("fake-agent24d");
        std::fs::write(&daemon, format!("#!/bin/sh\nprintf '%s\\n' '{ready}'\n")).unwrap();
        std::fs::set_permissions(daemon, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            home,
            server: Some(server),
        }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_agent24"));
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.home.path())
            .env("AGENT24D_BIN", self.home.path().join("fake-agent24d"))
            .args(["idoris"])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
    fn finish(mut self) -> Vec<(String, Value)> {
        self.server.take().unwrap().join().unwrap()
    }
}
fn output_json(output: &Output, success: bool) -> Value {
    assert_eq!(
        output.status.success(),
        success,
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains("sk-secret-sentinel"), "{text}");
        assert!(!text.contains("internal-key"), "{text}");
    }
    serde_json::from_slice(&output.stdout).unwrap()
}
const CONNECT: &[&str] = &[
    "connect",
    "--endpoint",
    "http://127.0.0.1:8088",
    "--daily",
    "daily-real",
    "--yes",
    "--json",
];

#[test]
fn connect_and_install_require_explicit_non_tty_consent() {
    for args in [
        vec![
            "connect",
            "--endpoint",
            "http://127.0.0.1:8088",
            "--daily",
            "daily-real",
            "--json",
        ],
        vec!["install", "--json"],
    ] {
        let fixture = Fixture::new(vec![]);
        let output = fixture.command(&args).output().unwrap();
        assert_eq!(
            output_json(&output, false)["error"]["code"],
            "consent_required"
        );
        fixture.finish();
    }
}

#[test]
fn status_and_candidates_json_success_omit_credentials() {
    for (command, path, body, expected) in [
        (
            "status",
            "/api/v1/idoris/status",
            json!({"state":"running", "version":"0.4.0", "data_port":7820, "admin_port":7821, "roles":["daily"], "last_error":null, "current_operation":null, "api_key":"sk-secret-sentinel"}),
            "state",
        ),
        (
            "candidates",
            "/api/v1/idoris/omlx/candidates",
            json!({"candidates":[{"endpoint":"http://127.0.0.1:8088", "identity":"omlx", "authentication":"authenticated", "models":["daily-real"], "key":"sk-secret-sentinel"}], "key_from_local_settings":true}),
            "candidates",
        ),
    ] {
        let fixture = Fixture::new(vec![step("GET", path, body)]);
        let output = fixture.command(&[command, "--json"]).output().unwrap();
        assert!(!output_json(&output, true)[expected].is_null());
        fixture.finish();
    }
}

#[test]
fn every_subcommand_preserves_daemon_error_json() {
    for (command, path) in [
        ("status", "/api/v1/idoris/status"),
        ("candidates", "/api/v1/idoris/omlx/candidates"),
        ("install", "/api/v1/idoris/install"),
        ("start", "/api/v1/idoris/start"),
        ("stop", "/api/v1/idoris/stop"),
        ("connect", "/api/v1/idoris/connect"),
    ] {
        let mut reply = step(
            if matches!(command, "status" | "candidates") {
                "GET"
            } else {
                "POST"
            },
            path,
            json!({"error":{"code":"omlx_auth_rejected", "message":"daemon message", "key":"sk-secret-sentinel"}}),
        );
        reply.status = 503;
        let fixture = Fixture::new(vec![reply]);
        let args = if command == "connect" {
            CONNECT.to_vec()
        } else if command == "install" {
            vec![command, "--yes", "--json"]
        } else {
            vec![command, "--json"]
        };
        let output = fixture.command(&args).output().unwrap();
        assert_eq!(
            output_json(&output, false),
            json!({"error":{"code":"omlx_auth_rejected", "message":"daemon message"}})
        );
        fixture.finish();
    }
}

#[test]
fn mutations_poll_until_terminal_and_report_operation_errors() {
    for action in ["install", "start", "stop"] {
        for failed in [false, true] {
            let path = match action {
                "install" => "/api/v1/idoris/install",
                "start" => "/api/v1/idoris/start",
                _ => "/api/v1/idoris/stop",
            };
            let mut done = operation(
                action,
                "completed",
                Some(if failed { "failed" } else { "succeeded" }),
            );
            if failed {
                done["error_code"] = json!("instance_conflict");
            }
            let fixture = Fixture::new(vec![
                step("POST", path, operation(action, "queued", None)),
                step(
                    "GET",
                    "/api/v1/idoris/operations/op-12",
                    operation(action, "starting", None),
                ),
                step("GET", "/api/v1/idoris/operations/op-12", done),
            ]);
            let args = if action == "install" {
                vec![action, "--yes", "--json"]
            } else {
                vec![action, "--json"]
            };
            let output = fixture.command(&args).output().unwrap();
            let body = output_json(&output, !failed);
            if failed {
                assert_eq!(body["error"]["code"], "instance_conflict");
            } else {
                assert_eq!(body["result"], "succeeded");
            }
            let requests = fixture.finish();
            assert!(requests[0].0.to_lowercase().contains("idempotency-key:"));
            if action == "install" {
                assert_eq!(requests[0].1, json!({"consent":true}));
            }
        }
    }
}

fn chat_reply() -> Step {
    let mut reply = step(
        "POST",
        "/api/v1/chat",
        json!({"message":{"role":"assistant", "content":"Hi"}, "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}, "model_id":"daily-real", "tier":"local"}),
    );
    reply.headers = "x-idoris-record-id: record-12\r\n";
    reply
}
#[test]
fn connect_retries_with_same_key_and_verifies_through_normal_chat() {
    let mut keys = Vec::new();
    for _ in 0..2 {
        let mut retry = step(
            "POST",
            "/api/v1/idoris/connect",
            json!({"error":{"code":"shutting_down", "message":"retry"}}),
        );
        retry.status = 0;
        let fixture = Fixture::new(vec![
            retry,
            step(
                "POST",
                "/api/v1/idoris/connect",
                operation("connect", "setup", None),
            ),
            step(
                "GET",
                "/api/v1/idoris/operations/op-12",
                operation("connect", "done", Some("succeeded")),
            ),
            chat_reply(),
        ]);
        let mut args = CONNECT.to_vec();
        args.extend([
            "--deep",
            "deep-real",
            "--core-reserved-bytes",
            "123",
            "--save-omlx-key",
            "--replace",
        ]);
        let output = fixture.command(&args).output().unwrap();
        assert_eq!(
            output_json(&output, true)["verification"],
            json!({"model_id":"daily-real", "record_id":"record-12", "locality":"loopback"})
        );
        let requests = fixture.finish();
        let key = |head: &str| {
            head.lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(k, _)| k.eq_ignore_ascii_case("idempotency-key"))
                        .map(|(_, v)| v.trim().to_owned())
                })
                .unwrap()
        };
        assert_eq!(key(&requests[0].0), key(&requests[1].0));
        keys.push(key(&requests[0].0));
        assert_eq!(requests[0].1, requests[1].1);
        assert_eq!(
            requests[0].1,
            json!({"endpoint":"http://127.0.0.1:8088", "daily_model":"daily-real", "deep_model":"deep-real", "core_reserved_bytes":123, "save_omlx_key":true, "replace":true, "consent":{"share_omlx_key":true, "daily_model":"daily-real", "deep_model":"deep-real"}})
        );
        assert_eq!(requests[3].1["messages"][0]["role"], "user");
        assert!(
            !requests[3].1["idoris_headroom_confirmed"]
                .as_bool()
                .unwrap_or(false)
        );
    }
    assert_ne!(keys[0], keys[1]);
}

#[test]
fn ctrl_c_cancels_the_current_operation() {
    let fixture = Fixture::new(vec![
        step(
            "POST",
            "/api/v1/idoris/install",
            operation("install", "downloading", None),
        ),
        step(
            "GET",
            "/api/v1/idoris/operations/op-12",
            operation("install", "verifying", None),
        ),
        step(
            "POST",
            "/api/v1/idoris/operations/op-12/cancel",
            operation("install", "rolling_back", None),
        ),
        step(
            "GET",
            "/api/v1/idoris/operations/op-12",
            operation("install", "done", Some("cancelled")),
        ),
    ]);
    let mut child = fixture.command(&["install", "--yes"]).spawn().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        use std::io::BufRead;
        let mut output = String::new();
        for line in std::io::BufReader::new(stderr).lines() {
            let line = line.unwrap();
            if line.contains("verifying") {
                tx.send(()).unwrap();
            }
            output.push_str(&line);
        }
        output
    });
    rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(
        Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let stderr = reader.join().unwrap();
    assert!(stderr.contains("cancelled"), "{stderr}");
    fixture.finish();
}

#[test]
fn connect_rejects_missing_or_remote_chat_evidence() {
    for missing in ["record", "model", "tier"] {
        let mut chat = chat_reply();
        match missing {
            "record" => chat.headers = "",
            "model" => chat.body["model_id"] = Value::Null,
            _ => chat.body["tier"] = json!("remote"),
        }
        let fixture = Fixture::new(vec![
            step(
                "POST",
                "/api/v1/idoris/connect",
                operation("connect", "done", Some("succeeded")),
            ),
            chat,
        ]);
        let output = fixture.command(CONNECT).output().unwrap();
        assert_eq!(
            output_json(&output, false)["error"]["code"],
            "verify_failed"
        );
        fixture.finish();
    }
}

#[test]
fn connect_operation_and_chat_errors_keep_their_codes() {
    for during_chat in [false, true] {
        let mut done = operation("connect", "done", Some("failed"));
        done["error_code"] = json!("setup_failed");
        let steps = if during_chat {
            let mut error = step(
                "POST",
                "/api/v1/chat",
                json!({"error":{"code":"idoris_headroom_confirmation_required", "message":"Confirmation required"}}),
            );
            error.status = 409;
            vec![
                step(
                    "POST",
                    "/api/v1/idoris/connect",
                    operation("connect", "done", Some("succeeded")),
                ),
                error,
            ]
        } else {
            vec![
                step(
                    "POST",
                    "/api/v1/idoris/connect",
                    operation("connect", "queued", None),
                ),
                step("GET", "/api/v1/idoris/operations/op-12", done),
            ]
        };
        let fixture = Fixture::new(steps);
        let output = fixture.command(CONNECT).output().unwrap();
        assert_eq!(
            output_json(&output, false)["error"]["code"],
            if during_chat {
                "idoris_headroom_confirmation_required"
            } else {
                "setup_failed"
            }
        );
        fixture.finish();
    }
}

#[test]
fn human_output_is_english_and_shows_progress_and_verification() {
    let fixture = Fixture::new(vec![
        step(
            "POST",
            "/api/v1/idoris/connect",
            operation("connect", "setup", None),
        ),
        step(
            "GET",
            "/api/v1/idoris/operations/op-12",
            operation("connect", "done", Some("succeeded")),
        ),
        chat_reply(),
    ]);
    let output = fixture
        .command(&CONNECT[..CONNECT.len() - 1])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("daily-real")
            && stdout.contains("record-12")
            && stdout.contains("loopback"),
        "{stdout}"
    );
    assert!(stdout.contains("agent24 chat"), "{stdout}");
    assert!(
        stderr.contains("setup") && stderr.contains("done"),
        "{stderr}"
    );
    assert!(!stdout.contains("sk-secret-sentinel") && !stderr.contains("sk-secret-sentinel"));
    fixture.finish();
    let mut error = step(
        "POST",
        "/api/v1/idoris/start",
        json!({"error":{"code":"not_configured", "message":"请先完成配置"}}),
    );
    error.status = 422;
    let fixture = Fixture::new(vec![error]);
    let output = fixture.command(&["start"]).output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.is_ascii() && stderr.contains("agent24 idoris connect"),
        "{stderr}"
    );
    fixture.finish();
}

#[test]
fn capabilities_without_enrollment_fail_closed_as_json() {
    let fixture = Fixture::new(vec![]);
    let daemon = fixture.home.path().join("fake-agent24d");
    let ready = json!({"type":"ready", "port":7824, "auth_mode":"capabilities", "generation":"test", "version":"test"});
    std::fs::write(&daemon, format!("#!/bin/sh\nprintf '%s\\n' '{ready}'\n")).unwrap();
    let output = fixture.command(&["status", "--json"]).output().unwrap();
    let body = output_json(&output, false);
    assert_eq!(body["error"]["code"], "host_authority_unavailable");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("CORE-2")
    );
    fixture.finish();
}

#[test]
fn tty_connect_requires_yes_and_sends_the_confirmed_models() {
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    use std::os::unix::ffi::OsStrExt;
    for answer in ["no\n", "yes\n"] {
        let fixture = Fixture::new(if answer == "yes\n" {
            vec![
                step(
                    "POST",
                    "/api/v1/idoris/connect",
                    operation("connect", "done", Some("succeeded")),
                ),
                chat_reply(),
            ]
        } else {
            vec![]
        });
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        grantpt(&master).unwrap();
        unlockpt(&master).unwrap();
        let name = ptsname(&master, Vec::new()).unwrap();
        let slave = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(std::ffi::OsStr::from_bytes(name.as_bytes()))
            .unwrap();
        let mut command = fixture.command(&[
            "connect",
            "--endpoint",
            "http://127.0.0.1:8088",
            "--daily",
            "daily-real",
            "--json",
        ]);
        command.stdin(Stdio::from(slave));
        let child = command.spawn().unwrap();
        let mut master = std::fs::File::from(master);
        master.write_all(answer.as_bytes()).unwrap();
        let output = child.wait_with_output().unwrap();
        let body = output_json(&output, answer == "yes\n");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("Type yes")
                && stderr.contains("daily-real")
                && stderr.contains("Share the local oMLX credential"),
            "{stderr}"
        );
        if answer == "no\n" {
            assert_eq!(body["error"]["code"], "consent_required");
        }
        fixture.finish();
    }
}
