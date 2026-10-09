//! COMM-5b T3: an end-to-end, local-only zero-run gate.
//!
//! Run explicitly with `HYPHAE_SOURCE_DIR=/absolute/path/to/locked-hyphae \
//! A24_HYPHAE_BIN=/absolute/path/to/locked-hyphae/hyphae \
//! cargo test -p agent24d --test comm_blackbox -- --ignored --nocapture`.
//! Set A24_HYPHAE_BIN to the lock-built binary; this builds a local relay,
//! starts agent24d and a bare Hyphae peer under temporary homes, and uses one
//! local counter for the model and MCP module. Missing inputs are failures.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use agent24_comm::{HyphaeLock, current_platform};
use serde_json::{Value, json};

struct Counter {
    address: std::net::SocketAddr,
    models: Arc<AtomicUsize>,
    modules: Arc<AtomicUsize>,
    stopped: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Counter {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local shared counter");
        listener.set_nonblocking(true).expect("nonblocking counter");
        let address = listener.local_addr().expect("counter address");
        let models = Arc::new(AtomicUsize::new(0));
        let modules = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let (model_count, module_count, stop) = (models.clone(), modules.clone(), stopped.clone());
        let thread = thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let (stream, _) = match listener.accept() {
                    Ok(c) => c,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                let mut reader = BufReader::new(stream);
                let mut header = Vec::new();
                loop {
                    let mut line = Vec::new();
                    if reader.read_until(b'\n', &mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let end = line == b"\r\n";
                    header.extend_from_slice(&line);
                    if end {
                        break;
                    }
                }
                // A connect-and-close probe sends no HTTP request. Count all
                // nonempty requests, including unknown/health paths below.
                if header.is_empty() {
                    continue;
                }
                let header_text = String::from_utf8_lossy(&header);
                let content_length = header_text
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                let mut body = vec![0; content_length];
                let _ = reader.read_exact(&mut body);
                let mut request = header_text.into_owned();
                request.push_str(&String::from_utf8_lossy(&body));
                let mut stream = reader.into_inner();
                let line = request.lines().next().unwrap_or("");
                eprintln!("COMM-5b counter: {line}");
                let (status, body) = if line.starts_with("GET /__ready ") {
                    ("200 OK", json!({"status":"ok"}).to_string())
                } else if line.starts_with("GET /counts ") {
                    ("200 OK", json!({"models": model_count.load(Ordering::SeqCst), "modules": module_count.load(Ordering::SeqCst)}).to_string())
                } else if line.starts_with("POST /module-call ") {
                    module_count.fetch_add(1, Ordering::SeqCst);
                    ("200 OK", "{}".to_owned())
                } else if line.starts_with("POST /v1/chat/completions ") {
                    model_count.fetch_add(1, Ordering::SeqCst);
                    // A real run must reach the mounted MCP tool before it can
                    // finish. The next model turn follows the tool result.
                    let has_tool_result = request.contains("\"role\":\"tool\"");
                    let body = if has_tool_result {
                        json!({"choices":[{"message":{"role":"assistant","content":"control complete"}}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2},"model":"comm5b-stub"})
                    } else {
                        json!({"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"comm5b-call","type":"function","function":{"name":"mcp_fixture_echo","arguments":r#"{"text":"comm5b positive control"}"#}}]}}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2},"model":"comm5b-stub"})
                    };
                    ("200 OK", body.to_string())
                } else {
                    model_count.fetch_add(1, Ordering::SeqCst);
                    ("200 OK", "{}".to_owned())
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Self {
            address,
            models,
            modules,
            stopped,
            thread: Some(thread),
        }
    }

    fn counts(&self) -> (usize, usize) {
        (
            self.models.load(Ordering::SeqCst),
            self.modules.load(Ordering::SeqCst),
        )
    }

    fn wait_ready(&self) {
        let url = format!("http://{}", self.address);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok((200, body)) =
                std::panic::catch_unwind(|| local_http(&url, "GET", "/__ready", None, None))
                && body["status"] == "ok"
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "fake model counter did not become ready at {}",
                self.address
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Counter {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            handle.join().expect("counter joins");
        }
    }
}

#[test]
fn counter_counts_http_requests_but_not_empty_tcp_connections() {
    let counter = Counter::start();
    // A TCP probe that closes without HTTP bytes is not a model request.
    drop(TcpStream::connect(counter.address).unwrap());
    counter.wait_ready();
    assert_eq!(counter.counts(), (0, 0));
    let url = format!("http://{}", counter.address);
    for path in ["/unexpected-health", "/v1/chat/completions", "/module-call"] {
        assert_eq!(
            local_http(&url, "POST", path, None, Some(&json!({}))).0,
            200
        );
    }
    assert_eq!(counter.counts(), (2, 1));
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            if let Some(pid) = rustix::process::Pid::from_raw(self.0.id() as i32) {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::Term);
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.0.try_wait().unwrap().is_none() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

struct DaemonGuard {
    child: ChildGuard,
    _home: tempfile::TempDir,
    base: String,
    token: String,
}
impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = std::panic::catch_unwind(|| {
            local_http(
                &self.base,
                "POST",
                "/api/v1/comm/daemon/stop",
                Some(&self.token),
                Some(&json!({})),
            )
        });
        let pid = rustix::process::Pid::from_raw(self.child.0.id() as i32).unwrap();
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::Term);
        let deadline = Instant::now() + Duration::from_secs(15);
        while self.child.0.try_wait().unwrap().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
    }
}

fn local_http(
    url: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&Value>,
) -> (u16, Value) {
    let rest = url.strip_prefix("http://").expect("HTTP URL");
    let (authority, prefix) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
    let (host, port) = authority.rsplit_once(':').expect("URL port");
    assert!(
        host == "127.0.0.1" || host == "localhost",
        "test networking must be local: {url}"
    );
    let mut stream =
        TcpStream::connect((host, port.parse::<u16>().unwrap())).expect("connect local HTTP");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let path = format!(
        "/{}/{}",
        prefix.trim_matches('/'),
        path.trim_start_matches('/')
    )
    .replace("//", "/");
    let bytes = body
        .map(serde_json::to_vec)
        .transpose()
        .unwrap()
        .unwrap_or_default();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nhost: {authority}\r\nconnection: close\r\n"
    )
    .unwrap();
    if let Some(token) = token {
        write!(stream, "authorization: Bearer {token}\r\n").unwrap();
    }
    if body.is_some() {
        write!(
            stream,
            "content-type: application/json\r\ncontent-length: {}\r\n",
            bytes.len()
        )
        .unwrap();
    }
    stream.write_all(b"\r\n").unwrap();
    stream.write_all(&bytes).unwrap();
    let mut raw = String::new();
    stream
        .read_to_string(&mut raw)
        .unwrap_or_else(|error| panic!("{method} {path}: local HTTP read failed: {error}"));
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    (
        status,
        serde_json::from_str(body).unwrap_or_else(|_| json!({"raw":body})),
    )
}

fn http(base: &str, method: &str, path: &str, token: &str, body: Option<&Value>) -> (u16, Value) {
    local_http(base, method, path, Some(token), body)
}

fn build(source: &Path, output: &Path, target: &str, lock: &HyphaeLock) {
    let (os, arch) = match current_platform().as_str() {
        "darwin-arm64" => ("darwin", "arm64"),
        "linux-x64" => ("linux", "amd64"),
        platform => panic!("T3 requires a lock-supported host platform: {platform}"),
    };
    let result = bounded_output(
        Command::new("go")
            .current_dir(source)
            .env("GOTOOLCHAIN", &lock.go)
            .env("CGO_ENABLED", "0")
            .env("GOOS", os)
            .env("GOARCH", arch)
            .args([
                "build",
                "-trimpath",
                "-buildvcs=false",
                "-ldflags=-buildid=",
                "-o",
            ])
            .arg(output)
            .arg(target),
        None,
        Duration::from_secs(180),
    );
    assert!(
        result.status.success(),
        "go build {target}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn bounded_output(
    command: &mut Command,
    input: Option<&[u8]>,
    timeout: Duration,
) -> std::process::Output {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    command.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = ChildGuard(command.spawn().expect("spawn fixture command"));
    let stdout = child.0.stdout.take().unwrap();
    let stderr = child.0.stderr.take().unwrap();
    let read = |mut pipe: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).unwrap();
            bytes
        })
    };
    let stdout = read(Box::new(stdout));
    let stderr = read(Box::new(stderr));
    if let Some(input) = input {
        child.0.stdin.take().unwrap().write_all(input).unwrap();
    }
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "fixture command timeout: {command:?}"
        );
        thread::sleep(Duration::from_millis(20));
    };
    std::process::Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    }
}

fn spawn_relay(bin: &Path, port: u16, data: &Path) -> ChildGuard {
    ChildGuard(
        Command::new(bin)
            .args([
                "-listen",
                "127.0.0.1",
                "-port",
                &port.to_string(),
                "-data-dir",
            ])
            .arg(data)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn local Hyphae relay"),
    )
}

fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

fn wait_for_port(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "local Hyphae relay did not bind port {port}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn spawn_agent24d(
    home: tempfile::TempDir,
    model_url: &str,
    hyphae: &Path,
) -> (DaemonGuard, u16, String) {
    // Other worktrees share CARGO_TARGET_DIR and may replace agent24d while
    // this scenario runs. Snapshot the Cargo-built executable into our HOME.
    let binary = home.path().join("agent24d-under-test");
    std::fs::copy(env!("CARGO_BIN_EXE_agent24d"), &binary).expect("snapshot real agent24d");
    let mut command = Command::new(&binary);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .env("A24_COMM_PASSWORD_STORE", "memory")
        .env("A24_HYPHAE_BIN", hyphae)
        .env("OMLX_URL", model_url)
        .env("OLLAMA_URL", model_url)
        .env("OPENAI_BASE_URL", model_url)
        .env("A24_BASE_URL", model_url)
        .env("DEFAULT_MODEL", "comm5b-stub")
        .args(["serve", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = ChildGuard(command.spawn().expect("spawn actual agent24d"));
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let mut reader = BufReader::new(stdout);
        let _ = reader.read_line(&mut line);
        let _ = tx.send(line);
        let _ = std::io::copy(&mut reader, &mut std::io::sink());
    });
    let ready = rx
        .recv_timeout(Duration::from_secs(120))
        .expect("agent24d ready line (bounded cold-start deadline)");
    let value: Value = serde_json::from_str(&ready).expect("ready JSON");
    let port = value["port"].as_u64().unwrap() as u16;
    let token = value["token"].as_str().expect("daemon token").to_owned();
    (
        DaemonGuard {
            child,
            _home: home,
            base: format!("http://127.0.0.1:{port}"),
            token: token.clone(),
        },
        port,
        token,
    )
}

fn b_run(bin: &Path, home: &Path, args: &[&str], password: Option<&str>) -> Value {
    let mut command = Command::new(bin);
    command
        .args(args)
        .env("HOME", home)
        .env("HYPHAE_OUTPUT", "json")
        .current_dir(home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if password.is_some() {
        command.arg("--password-stdin").stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    let output = bounded_output(
        &mut command,
        password.map(str::as_bytes),
        Duration::from_secs(30),
    );
    assert!(
        output.status.success(),
        "Hyphae {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("Hyphae envelope JSON")
}

fn runs(base: &str, token: &str) -> Vec<Value> {
    let (s, v) = http(base, "GET", "/api/v1/runs", token, None);
    assert_eq!(s, 200, "runs: {v}");
    v["runs"].as_array().expect("run list").clone()
}

#[test]
#[ignore = "builds and runs real local agent24d, Hyphae and relay; see test module docs"]
fn local_real_peer_reads_are_zero_run_with_same_counter_positive_control() {
    let source = PathBuf::from(
        std::env::var("HYPHAE_SOURCE_DIR")
            .expect("HYPHAE_SOURCE_DIR is required; T3 cannot pass without source"),
    );
    let source = source.canonicalize().expect("HYPHAE_SOURCE_DIR must exist");
    let fixture_path = source.join("tests/contracts/testdata/public-query-fixtures.json");
    let fixture_bytes = std::fs::read(&fixture_path).expect("Hyphae query fixture is required");
    let fixture: Value =
        serde_json::from_slice(&fixture_bytes).expect("Hyphae public query fixture JSON");
    let lock = HyphaeLock::embedded().expect("embedded Hyphae lock");
    let source_sha = Command::new("git")
        .args([
            "-C",
            source.to_str().expect("UTF-8 source path"),
            "rev-parse",
            "HEAD",
        ])
        .output()
        .expect("git is required to validate Hyphae source");
    assert!(
        source_sha.status.success(),
        "HYPHAE_SOURCE_DIR must be a git checkout"
    );
    assert_eq!(
        String::from_utf8_lossy(&source_sha.stdout).trim(),
        lock.source_sha,
        "Hyphae source must match embedded lock; use the lock-pinned local source revision"
    );
    assert!(
        Command::new("git")
            .current_dir(&source)
            .args(["diff", "--quiet", "HEAD"])
            .status()
            .unwrap()
            .success(),
        "locked source must have no tracked modifications"
    );
    assert_eq!(
        agent24_comm::binary::sha256_of(&fixture_bytes)
            .to_hex()
            .to_string(),
        "5139728518dc0be5db28eb9f5a3cb40198874d0d7a27fa57e9d232731f731a72",
        "query fixture provenance changed; review before updating this acceptance"
    );
    let root = tempfile::tempdir().unwrap();
    let bin = PathBuf::from(
        std::env::var_os("A24_HYPHAE_BIN")
            .expect("A24_HYPHAE_BIN required: build using lock recipe"),
    )
    .canonicalize()
    .expect("real Hyphae binary exists");
    let relay_bin = root.path().join("hyphae-relay");
    build(&source, &relay_bin, "./cmd/hyphae-relay", &lock);
    let hash = agent24_comm::binary::sha256_of(&std::fs::read(&bin).unwrap());
    let expected = lock.expected_for(&current_platform()).unwrap();
    assert_eq!(hash, expected, "built Hyphae must match embedded lock");
    eprintln!(
        "COMM-5b T3 verified Hyphae source={} platform={} sha256={hash:?}",
        lock.source_sha,
        current_platform()
    );

    let samples = serde_json::from_str::<Value>(include_str!(
        "../../../crates/agent24-comm/tests/fixtures/comm_inbound_six.json"
    ))
    .unwrap();
    let mut samples = samples["samples"].as_array().unwrap().clone();
    // Replace query/response with the actual source fixture bodies, never a
    // hand-authored approximation. Keep the complete six-shape matrix.
    // `public-query-fixtures.json` uses named rows in `cases`.
    let cases = fixture["cases"]
        .as_array()
        .expect("public query fixture cases");
    let body = |name: &str| {
        cases
            .iter()
            .find(|case| case["id"] == name)
            .unwrap_or_else(|| panic!("{name} missing from {}", fixture_path.display()))["body"]
            .clone()
    };
    samples[2]["content"] = body("query-profile");
    samples[3]["content"] = body("response-ok-profile");

    let counter = Counter::start();
    // Binding the socket is not enough: wait until its serving thread has
    // accepted and answered a request before injecting the URL into the
    // agent24d child. This avoids a first-request refused connection racing
    // the accept loop under a cold/parallel test runner.
    counter.wait_ready();
    let model_url = format!("http://{}", counter.address);
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".agent24")).unwrap();
    // A tiny stdio MCP server whose only tool reports its invocation to the
    // same counter process used by the model endpoint.
    let mcp = home.path().join("mcp.py");
    let script = r#"import json,sys,urllib.request
url=sys.argv[1]
for line in sys.stdin:
 try: req=json.loads(line)
 except Exception: continue
 method=req.get('method'); ident=req.get('id')
 if ident is None: continue
 if method=='initialize': result={'protocolVersion':'2024-11-05','capabilities':{'tools':{}},'serverInfo':{'name':'comm5b','version':'1'}}
 elif method=='tools/list': result={'tools':[{'name':'echo','description':'counter positive control','inputSchema':{'type':'object','properties':{'text':{'type':'string'}},'required':['text']}}]}
 elif method=='tools/call':
  urllib.request.urlopen(urllib.request.Request(url+'/module-call',data=b'{}',method='POST'),timeout=3).read()
  result={'content':[{'type':'text','text':'module invoked'}],'isError':False}
 else: result={}
 print(json.dumps({'jsonrpc':'2.0','id':ident,'result':result}),flush=True)
"#;
    std::fs::write(&mcp, script).unwrap();
    std::fs::write(home.path().join(".agent24/mcp.json"),serde_json::to_vec(&json!({"mcpServers":{"fixture":{"command":"python3","args":["-u",mcp.to_string_lossy(),model_url]}}})).unwrap()).unwrap();
    let daemon_home = home;
    let (daemon, port, token) = spawn_agent24d(daemon_home, &model_url, &bin);
    let base = format!("http://127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(30);
    while local_http(&base, "GET", "/api/v1/health", None, None).0 != 200 {
        assert!(Instant::now() < deadline, "agent24d health timeout");
        thread::sleep(Duration::from_millis(50));
    }
    let (s, id) = http(
        &base,
        "POST",
        "/api/v1/comm/identity",
        &token,
        Some(&json!({"nickname":"a","default":true})),
    );
    assert_eq!(s, 200, "identity: {id}");
    let npub = id["data"]["npub"].as_str().unwrap().to_owned();
    let relay_port = free_port();
    let relay_dir = root.path().join("relay");
    std::fs::create_dir_all(&relay_dir).unwrap();
    let _relay = spawn_relay(&relay_bin, relay_port, &relay_dir);
    wait_for_port(relay_port);
    let relay = format!("ws://127.0.0.1:{relay_port}");
    let (s, v) = http(
        &base,
        "PUT",
        "/api/v1/comm/relay",
        &token,
        Some(&json!({"relays":[relay]})),
    );
    assert_eq!(s, 200, "relay config: {v}");
    let (s, v) = http(
        &base,
        "PUT",
        "/api/v1/tool-overrides/mcp_fixture_echo",
        &token,
        Some(&json!({"risk_class":"read","source":"comm5b-test"})),
    );
    assert_eq!(s, 200, "MCP override: {v}");
    let (s, v) = http(
        &base,
        "POST",
        "/api/v1/comm/daemon/start",
        &token,
        Some(&json!({})),
    );
    assert_eq!(s, 200, "start managed Hyphae: {v}");
    let b_home = root.path().join("home-b");
    std::fs::create_dir_all(&b_home).unwrap();
    let password = "comm5b-local-peer-password";
    let env = b_run(
        &bin,
        &b_home,
        &["identity", "create", "--nickname", "b"],
        Some(password),
    );
    let _npub_b = env["data"]["npub"].as_str().expect("peer npub").to_owned();
    let _ = b_run(
        &bin,
        &b_home,
        &["contact", "add", "--nickname", "a", "--npub", &npub],
        None,
    );
    let _ = b_run(&bin, &b_home, &["relay", "set", "--relay", &relay], None);

    // Positive control first. It uses actual agent24d run dispatch, its real
    // model client, and the mounted MCP module counted by the same process.
    let before_runs = runs(&base, &token).len();
    let (before_models, before_modules) = counter.counts();
    assert_eq!(
        before_models, 0,
        "shared model counter starts at zero before the control run"
    );
    assert_eq!(
        before_modules, 0,
        "shared module counter starts at zero before the control run"
    );
    counter.wait_ready();
    let (s, session) = http(
        &base,
        "POST",
        "/api/v1/sessions",
        &token,
        Some(&json!({"title":"COMM-5b control","channel":"test"})),
    );
    assert_eq!(s, 201, "session: {session}");
    let (s, run) = http(
        &base,
        "POST",
        "/api/v1/runs",
        &token,
        Some(
            &json!({"prompt":"Use mcp_fixture_echo with text comm5b positive control.","session_id":session["id"]}),
        ),
    );
    assert_eq!(s, 202, "run: {run}");
    let run_id = run["id"].as_str().expect("created run id");
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let (s, state) = http(
            &base,
            "GET",
            &format!("/api/v1/runs/{run_id}"),
            &token,
            None,
        );
        assert_eq!(s, 200, "get positive-control run: {state}");
        if matches!(state["status"].as_str(), Some("failed" | "cancelled")) {
            panic!("positive-control run did not complete: {state}");
        }
        if state["status"] == "completed" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "positive-control run did not finish: {state}; counters={:?}",
            counter.counts()
        );
        thread::sleep(Duration::from_millis(100));
    }
    let positive_runs = runs(&base, &token).len();
    assert_eq!(positive_runs, before_runs + 1, "positive control run delta");
    let (models, modules) = counter.counts();
    assert!(
        models > before_models && modules > before_modules,
        "completed control must reach both backends: models={models}, modules={modules}"
    );
    let (base_models, base_modules) = counter.counts();

    let (s, v) = http(
        &base,
        "GET",
        "/api/v1/comm/history?as=a&limit=200",
        &token,
        None,
    );
    assert_eq!(s, 200, "history before peer: {v}");
    let mut event_ids = Vec::new();
    for sample in &samples {
        let content = sample["content"].as_str().expect("sample body");
        let env = b_run(
            &bin,
            &b_home,
            &[
                "agent",
                "msg",
                "--from",
                "b",
                "--to",
                &npub,
                "--content",
                content,
            ],
            Some(password),
        );
        event_ids.push(
            env["data"]["event_id"]
                .as_str()
                .expect("dynamic sent event id")
                .to_owned(),
        );
    }
    // Pull/read in bounded loops, then use `id` (Hyphae event id) from each
    // history row. No pre-delivered events or hard-coded IDs are accepted.
    let deadline = Instant::now() + Duration::from_secs(90);
    let history_path = "/api/v1/comm/history?as=a&limit=200";
    let mut seen = false;
    while Instant::now() < deadline {
        let (s, h) = http(&base, "GET", history_path, &token, None);
        assert_eq!(s, 200, "history: {h}");
        let rows = h["data"]
            .as_array()
            .unwrap_or_else(|| h["data"]["messages"].as_array().expect("history array"))
            .clone();
        if event_ids
            .iter()
            .all(|id| rows.iter().any(|row| row["id"].as_str() == Some(id)))
        {
            for (id, sample) in event_ids.iter().zip(&samples) {
                let row = rows
                    .iter()
                    .find(|row| row["id"].as_str() == Some(id))
                    .unwrap();
                assert_eq!(
                    row["plaintext"], sample["content"],
                    "received peer payload unchanged"
                );
            }
            let direct = b_run(
                &bin,
                &daemon._home.path().join(".agent24/comm/hyphae-home"),
                &["history", "inbox", "--as", "a", "--limit", "200"],
                None,
            );
            assert_eq!(
                h["data"], direct["data"],
                "COMM must return the complete Hyphae history unchanged, including ciphertext"
            );
            seen = true;
            break;
        }
        let (s, pulled) = http(
            &base,
            "POST",
            "/api/v1/comm/inbox/pull",
            &token,
            Some(&json!({"as":"a"})),
        );
        assert_eq!(s, 200, "pull inbox: {pulled}");
        thread::sleep(Duration::from_millis(250));
    }
    assert!(
        seen,
        "did not receive all dynamically generated peer events: {event_ids:?}"
    );
    for _ in 0..20 {
        for path in [
            history_path,
            "/api/v1/comm/identity",
            "/api/v1/comm/contact",
            "/api/v1/comm/relay",
            "/api/v1/comm/outbox",
            "/api/v1/comm/daemon",
        ] {
            let (s, v) = http(&base, "GET", path, &token, None);
            assert_eq!(s, 200, "{path}: {v}");
        }
    }
    assert_eq!(
        runs(&base, &token).len(),
        positive_runs,
        "six inbound messages and reads must not create runs"
    );
    assert_eq!(
        counter.counts(),
        (base_models, base_modules),
        "passive reads must not call model or MCP module"
    );
    let (s, stopped) = http(
        &base,
        "POST",
        "/api/v1/comm/daemon/stop",
        &token,
        Some(&json!({})),
    );
    assert_eq!(s, 200, "stop managed Hyphae: {stopped}");
    drop(daemon);
    assert_eq!(
        counter.counts(),
        (base_models, base_modules),
        "stop/shutdown must remain passive"
    );
    eprintln!(
        "COMM-5b T3 PASS: positive runs delta=1, models={base_models}, modules={base_modules}; six real peer events; passive runs delta=0, model/module delta=0"
    );
}
