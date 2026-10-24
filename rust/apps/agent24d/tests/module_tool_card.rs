//! K1-6a-G §2.1, through the real binary: a module tool's card is made from
//! what `serve` assembled for it, and carries the identity of the package
//! actually running — none before it runs.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A package module that shakes hands with its own manifest's digest and
/// stays up until the callback connection ends.
const MODULE: &str = r#"import hashlib, json, os, socket
with open("domain-os.yml", "rb") as f:
    digest = "sha256:" + hashlib.sha256(f.read()).hexdigest()
listener = socket.socket(fileno=int(os.environ["A24_LISTEN_FD"]))
cb = socket.socket(socket.AF_UNIX)
cb.connect(os.environ["A24_CALLBACK_SOCK"])
req = {"jsonrpc": "2.0", "id": "1", "method": "initialize", "params": {
    "protocol_versions": {"min": 1, "max": 1000}, "module": "noted",
    "manifest_digest": digest, "auth_token": os.environ["A24_HANDSHAKE_TOKEN"],
    "capabilities": []}}
cb.sendall((json.dumps(req) + "\n").encode())
f = cb.makefile("rb")
f.readline()
while f.readline():
    pass
"#;

fn install(home: &Path) {
    let dir = home.join(".agent24/packages/noted");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("domain-os.yml"),
        "name: noted\nversion: \"0.1.0\"\nroute_namespace: /api/v1/noted\n\
         event_module: noted\ndata_dir: ~/.agent24/os/noted/\n\
         kernel_capabilities: []\nimpl_kind: out_of_process_provider\n\
         min_daemon_protocol: 2\n\
         spawn:\n  command: python3\n  args: [\"-I\", \"-S\", \"mod.py\"]\n\
         tools:\n  - operation: list-notes\n    description: lists notes\n    \
         input_schema: {type: object, properties: {}}\n    risk: read\n    \
         output_privacy: local_only\n    resource_kinds: [note]\n    \
         timeout_ms: 5000\n    inline_wait_ms: 1000\n",
    )
    .unwrap();
    std::fs::write(dir.join("mod.py"), MODULE).unwrap();
    // On the grants protocol but never running: no identity, so no card.
    let dir = home.join(".agent24/packages/idle");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("domain-os.yml"),
        "name: idle\nversion: \"0.1.0\"\nroute_namespace: /api/v1/idle\n\
         event_module: idle\ndata_dir: ~/.agent24/os/idle/\n\
         kernel_capabilities: []\nimpl_kind: out_of_process_provider\n\
         min_daemon_protocol: 2\n\
         spawn:\n  command: \"false\"\n  args: []\n\
         tools:\n  - operation: list-notes\n    description: lists notes\n    \
         input_schema: {type: object, properties: {}}\n    risk: read\n    \
         output_privacy: local_only\n    timeout_ms: 5000\n    inline_wait_ms: 1000\n",
    )
    .unwrap();
    // A tool module on an older protocol (§2.6): never a card.
    let dir = home.join(".agent24/packages/plain");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("domain-os.yml"),
        "name: plain\nversion: \"0.1.0\"\nroute_namespace: /api/v1/plain\n\
         event_module: plain\ndata_dir: ~/.agent24/os/plain/\n\
         kernel_capabilities: []\nimpl_kind: out_of_process_provider\n\
         spawn:\n  command: \"false\"\n  args: []\n\
         tools:\n  - operation: list-notes\n    description: lists notes\n    \
         input_schema: {type: object, properties: {}}\n    risk: read\n    \
         output_privacy: local_only\n    timeout_ms: 5000\n    inline_wait_ms: 1000\n",
    )
    .unwrap();
}

struct Daemon {
    child: Child,
    port: u16,
    token: String,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn daemon(home: &Path, env: &[(&str, &str)], args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agent24d"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .envs(env.iter().copied())
        .args(["serve", "--port", "0"])
        .args(args);
    command
}

fn start(home: &Path) -> Daemon {
    start_with(home, &[], &[])
}

fn start_with(home: &Path, env: &[(&str, &str)], args: &[&str]) -> Daemon {
    let mut child = daemon(home, env, args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let mut daemon = Daemon {
        child,
        port: 0,
        token: String::new(),
    };
    let ready = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("no ready line within 30s");
    let ready: serde_json::Value = serde_json::from_str(&ready).expect("the ready line");
    daemon.port = u16::try_from(ready["port"].as_u64().unwrap()).unwrap();
    daemon.token = ready["token"].as_str().unwrap().to_owned();
    daemon
}

fn get(d: &Daemon, path: &str) -> (u16, serde_json::Value) {
    send(d, "GET", path, "")
}

fn send(d: &Daemon, method: &str, path: &str, body: &str) -> (u16, serde_json::Value) {
    let mut s = TcpStream::connect(("127.0.0.1", d.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {}\r\n\
         content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        d.token,
        body.len()
    )
    .unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let status = raw.split(' ').nth(1).unwrap().parse().unwrap();
    let body = raw.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    (status, serde_json::from_str(body).unwrap_or_default())
}

const CARD: &str = "/api/v1/module-consents/modules/noted/tools/list-notes/card";
const DECIDE: &str = "/api/v1/module-consents/modules/noted/tools/list-notes";

/// The card, once the module runs: before then there is none (409).
fn running_card(d: &Daemon) -> serde_json::Value {
    let by = Instant::now() + Duration::from_secs(30);
    loop {
        let (status, body) = get(d, CARD);
        if status == 200 {
            return body;
        }
        assert_eq!(status, 409, "{body}");
        assert!(Instant::now() < by, "the module never ran: {body}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_card_carries_the_running_package_identity() {
    let home = tempfile::tempdir().unwrap();
    install(home.path());
    let d = start(home.path());
    let card = running_card(&d);
    let package = home.path().join(".agent24/packages/noted");
    assert_eq!(
        card["artifact"],
        agent24_os_proto::artifact::identity(&package).unwrap()
    );
    assert_eq!(card["module_version"], "0.1.0");
    assert_eq!(card["resource_kinds"], serde_json::json!(["note"]));
    assert_eq!(card["host_risk"], "high");
    assert!(card["fingerprint"].as_str().is_some_and(|f| !f.is_empty()));
    let (status, _) = get(&d, "/api/v1/module-consents/modules/noted/tools/other/card");
    assert_eq!(status, 404);
    let (status, body) = get(
        &d,
        "/api/v1/module-consents/modules/plain/tools/list-notes/card",
    );
    assert_eq!(
        (status, &body["error"]["code"]),
        (409, &serde_json::json!("protocol_too_old"))
    );
    let (status, body) = get(
        &d,
        "/api/v1/module-consents/modules/idle/tools/list-notes/card",
    );
    assert_eq!(
        (status, &body["error"]["code"]),
        (409, &serde_json::json!("no_package_identity"))
    );
}

/// K1-6a-G §2.1 through the real binary: with the host switch on, a
/// decision made on the card is written, and one on a card that is no
/// longer current is not.
#[test]
fn a_decision_on_the_running_card_is_written() {
    let home = tempfile::tempdir().unwrap();
    install(home.path());
    let d = start_with(home.path(), &[("A24_MODULE_TOOL_GRANTS", "1")], &[]);
    let card = running_card(&d);
    let decision = |fingerprint: &serde_json::Value| {
        serde_json::json!({
            "decision": "granted",
            "fingerprint": fingerprint,
            "module_version": card["module_version"],
            "artifact": card["artifact"],
            "resources": ["note:n1"],
            "expires_at": (chrono::Utc::now() + chrono::Duration::days(7)).to_rfc3339(),
        })
        .to_string()
    };
    let (status, body) = send(&d, "POST", DECIDE, &decision(&"sha256:other".into()));
    assert_eq!(
        (status, &body["error"]["code"]),
        (409, &"card_changed".into())
    );
    let (_, list) = get(&d, "/api/v1/module-consents");
    assert_eq!(list["authorizations"], serde_json::json!([]));
    let (status, body) = send(&d, "POST", DECIDE, &decision(&card["fingerprint"]));
    assert_eq!(
        (status, &body["status"]),
        (200, &"granted".into()),
        "{body}"
    );
    let (_, list) = get(&d, "/api/v1/module-consents");
    assert_eq!(list["authorizations"][0]["status"], "granted", "{list}");
}

/// F6: an ephemeral daemon, whose store does not outlive it, takes no
/// decision even with the switch on.
#[test]
fn an_ephemeral_daemon_takes_no_decision() {
    let home = tempfile::tempdir().unwrap();
    let d = start_with(
        home.path(),
        &[("A24_MODULE_TOOL_GRANTS", "1")],
        &["--ephemeral"],
    );
    let (status, body) = send(&d, "POST", DECIDE, "{}");
    assert_eq!(
        (status, &body["error"]["code"]),
        (403, &"grants_closed".into())
    );
}

/// Criterion 14: a host bound past 90 days stops the start.
#[test]
fn a_bound_past_ninety_days_stops_the_start() {
    let home = tempfile::tempdir().unwrap();
    let out = daemon(home.path(), &[("A24_MODULE_GRANT_MAX_DAYS", "91")], &[])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "no ready line");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("A24_MODULE_GRANT_MAX_DAYS"), "{stderr}");
}
