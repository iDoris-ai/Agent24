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
         output_privacy: local_only\n    timeout_ms: 5000\n    inline_wait_ms: 1000\n",
    )
    .unwrap();
    std::fs::write(dir.join("mod.py"), MODULE).unwrap();
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

fn start(home: &Path) -> Daemon {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent24d"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .args(["serve", "--port", "0"])
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
    let mut s = TcpStream::connect(("127.0.0.1", d.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {}\r\nconnection: close\r\n\r\n",
        d.token
    )
    .unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let status = raw.split(' ').nth(1).unwrap().parse().unwrap();
    let body = raw.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    (status, serde_json::from_str(body).unwrap_or_default())
}

const CARD: &str = "/api/v1/module-consents/modules/noted/tools/list-notes/card";

#[test]
fn a_card_carries_the_running_package_identity() {
    let home = tempfile::tempdir().unwrap();
    install(home.path());
    let d = start(home.path());
    let by = Instant::now() + Duration::from_secs(30);
    let card = loop {
        let (status, body) = get(&d, CARD);
        if status == 200 {
            break body;
        }
        assert_eq!(status, 409, "{body}");
        assert!(Instant::now() < by, "the module never ran: {body}");
        std::thread::sleep(Duration::from_millis(100));
    };
    let package = home.path().join(".agent24/packages/noted");
    assert_eq!(
        card["artifact"],
        agent24_os_proto::artifact::identity(&package).unwrap()
    );
    assert_eq!(card["module_version"], "0.1.0");
    assert_eq!(card["resource_kinds"], serde_json::json!([]));
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
}
