//! FU-61 — `agent24 os uninstall`'s hot-stop step, and `agent24 daemon
//! stop`'s reliability fix, driven through the real `agent24`/`agent24d`
//! binaries (round-1 code review Medium 1: the CLI's own control flow
//! — `cmd_uninstall`, `attach_only`, `hot_disable_best_effort`,
//! `wait_for_stop` — is thin glue over cross-process/global-`$HOME` state
//! that a unit test inside `main.rs` cannot safely exercise without
//! mutating the TEST PROCESS's own environment, which would race other
//! tests running in parallel threads. Isolating `HOME`/`PATH`/`AGENT24D_BIN`
//! on each spawned `Command` instead — never on the test process itself —
//! is exactly the pattern `agent24d`'s own `tests/daemon_modules.rs` already
//! uses for the same reason.
//!
//! Run via `cargo test --workspace` (or build `agent24d` first): these
//! tests spawn the real `agent24d` binary, which a targeted `cargo test -p
//! agent24-cli` alone does not build, since `agent24-cli` has no Cargo
//! dependency edge on it (it only shells out to it at runtime).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("a24")
        .tempdir_in("/tmp")
        .unwrap()
}

/// `agent24d` has no lib target, so it cannot be a `[dev-dependencies]`
/// entry (Cargo needs one to wire up `CARGO_BIN_EXE_agent24d` at compile
/// time) — resolved instead the same way `agent24-cli`'s own
/// `agent24d_binary()` resolves it in production when `AGENT24D_BIN` is
/// unset: next to this test binary's sibling, `agent24`, which `cargo
/// build --workspace`/`cargo test --workspace` always places in the same
/// output directory.
fn agent24d_bin() -> std::path::PathBuf {
    let path = std::path::Path::new(env!("CARGO_BIN_EXE_agent24")).with_file_name("agent24d");
    assert!(
        path.exists(),
        "{} does not exist — build it first (`cargo build -p agent24d`), or run these \
         tests via `cargo test --workspace` rather than a package-scoped invocation",
        path.display()
    );
    path
}

fn cli(home: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_agent24"));
    c.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("AGENT24D_BIN", agent24d_bin());
    c
}

fn run(home: &Path, args: &[&str]) -> (bool, String) {
    let out = cli(home).args(args).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

fn alive(pid: i32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// A package whose module answers real HTTP on the listener the kernel
/// hands it (`A24_LISTEN_FD`) and writes its own pid to `<data>/pid` before
/// serving — so a caller that gets a real 200 through the proxy has PROOF
/// the generation is `Running` and admitting requests, not merely
/// `Status::Starting` (which `os list` also renders as `[mounted]`, and
/// whose PID a bare post-handshake write would race — round-3 code review
/// Medium 1: the handshake response is sent by the daemon BEFORE it marks
/// the generation ready, so a pid file written right after reading that
/// response does not by itself prove admission is open yet). `slow_exit`:
/// if true, the module sleeps briefly on SIGTERM before exiting, giving
/// `agent24 daemon stop` a real drain/grace window to race — without this,
/// a daemon with nothing to stop shuts down fast enough that even the OLD
/// fire-and-forget `daemon stop` would happen to print after the process
/// was already gone, defeating the point of testing it.
fn write_package(home: &Path, name: &str, slow_exit: bool) -> std::path::PathBuf {
    let dir = home.join("srcpkg").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("domain-os.yml"),
        format!(
            "name: {name}\nversion: \"0.1.0\"\nroute_namespace: /api/v1/{name}\n\
             event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
             kernel_capabilities: []\nimpl_kind: out_of_process_provider\n\
             spawn:\n  command: python3\n  args: [\"-I\", \"-S\", \"mod.py\"]\n"
        ),
    )
    .unwrap();
    let sleep_on_term = if slow_exit {
        // `python3 -I -S` disables `site`, which is what normally defines
        // the `exit` builtin — `os._exit`, not that, is what actually ends
        // the process here (round-4 code review Low: a lambda calling the
        // undefined `exit` would raise `NameError` instead of exiting
        // cleanly after the sleep; the process still dies, but not the way
        // this fixture's own comment claims). `\x20`-prefixed indentation:
        // Rust's `\` line-continuation strips ALL leading whitespace off
        // the next line, not just the newline, so plain spaces here would
        // produce unindented (syntax-error) Python.
        "import os, signal, time\n\
        def _term(*_):\n\
        \x20\x20\x20time.sleep(1)\n\
        \x20\x20\x20os._exit(0)\n\
        signal.signal(signal.SIGTERM, _term)\n"
    } else {
        ""
    };
    std::fs::write(
        dir.join("mod.py"),
        format!(
            r#"import hashlib, json, os, socket, threading
{sleep_on_term}with open("domain-os.yml", "rb") as f:
    digest = "sha256:" + hashlib.sha256(f.read()).hexdigest()
with open(os.path.join(os.environ["A24_DATA_DIR"], "pid"), "w") as pf:
    pf.write(str(os.getpid()))
listener = socket.socket(fileno=int(os.environ["A24_LISTEN_FD"]))
def serve():
    while True:
        conn, _ = listener.accept()
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = conn.recv(4096)
            if not chunk:
                return
            head += chunk
        body = b"hello"
        conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: %d\r\n\r\n%s" % (len(body), body))
        conn.close()
threading.Thread(target=serve, daemon=True).start()
cb = socket.socket(socket.AF_UNIX)
cb.connect(os.environ["A24_CALLBACK_SOCK"])
req = {{"jsonrpc": "2.0", "id": "1", "method": "initialize", "params": {{
    "protocol_versions": {{"min": 1, "max": 1000}}, "module": {name:?},
    "manifest_digest": digest, "auth_token": os.environ["A24_HANDSHAKE_TOKEN"],
    "capabilities": []}}}}
cb.sendall((json.dumps(req) + "\n").encode())
f = cb.makefile("rb")
f.readline()
while f.readline():
    pass
"#
        ),
    )
    .unwrap();
    dir
}

/// This daemon's `(port, token)`, from the state file `agent24 daemon
/// start` just wrote.
fn daemon_state(home: &Path) -> (u16, String) {
    let s = std::fs::read_to_string(home.join(".agent24/daemon.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    (
        u16::try_from(v["port"].as_u64().unwrap()).unwrap(),
        v["token"].as_str().unwrap().to_owned(),
    )
}

/// `GET path` through the daemon's proxy: `(status, body)`, or `None` if
/// unreachable. Mirrors `agent24d`'s own `tests/daemon_modules.rs::get`.
fn get(port: u16, token: &str, path: &str) -> Option<(u16, String)> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {token}\r\n\
         connection: close\r\n\r\n"
    )
    .ok()?;
    let mut raw = String::new();
    s.read_to_string(&mut raw).ok()?;
    let status = raw.split(' ').nth(1)?.parse().ok()?;
    let body = raw.split_once("\r\n\r\n").map(|(_, b)| b.to_owned())?;
    Some((status, body))
}

/// Waits until `name`'s module answers a real request through the proxy —
/// unambiguous proof its generation is `Running`, not `Starting` — and
/// returns its pid (written before it started serving, so it is already on
/// disk by the time a request can succeed).
fn wait_for_running_pid(home: &Path, name: &str) -> i32 {
    let by = Instant::now() + Duration::from_secs(30);
    loop {
        let (port, token) = daemon_state(home);
        if let Some((200, body)) = get(port, &token, &format!("/api/v1/{name}/hi")) {
            assert_eq!(body, "hello");
            break;
        }
        assert!(
            Instant::now() < by,
            "{name} never answered a real request through the proxy"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    std::fs::read_to_string(home.join(".agent24/os").join(name).join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// Kills the daemon this test started, whatever happens — a failing
/// assertion must not leave a daemon holding the singleton lock or running
/// a module process. Tries a clean `daemon stop` first; if that does not
/// make the daemon's own pid disappear promptly (a hung/unhealthy daemon,
/// or an assertion firing mid-drain), force-kills it directly — mirroring
/// `agent24d`'s own `tests/daemon_modules.rs::Running` guard, which does
/// not trust a graceful path to always work either. `pid` is captured from
/// `daemon start`'s own output at construction time, NOT re-read from
/// `daemon.json` during `drop` (round-3 code review Low: the daemon
/// deletes that file at the very start of its own shutdown, before
/// draining/stopping modules — reading it here would usually find nothing
/// to fall back on exactly when the fallback is needed).
struct Daemon<'a> {
    home: &'a Path,
    pid: i32,
}

/// Parses a pid out of a CLI message shaped like "...(pid N, port M)" —
/// both `daemon start`'s "daemon started" and "daemon already running"
/// lines use it.
fn parse_pid(out: &str) -> i32 {
    out.split("pid ")
        .nth(1)
        .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("could not parse a pid out of: {out}"))
}

impl<'a> Daemon<'a> {
    /// Starts the daemon and wraps it, parsing its pid out of `daemon
    /// start`'s own success line ("daemon started (pid N, port M)").
    fn start(home: &'a Path) -> Self {
        let (ok, out) = run(home, &["daemon", "start"]);
        assert!(ok, "{out}");
        Self {
            home,
            pid: parse_pid(&out),
        }
    }

    /// Wraps an already-started daemon (the caller already ran `daemon
    /// start` itself, typically to assert on its exact output) purely for
    /// the cleanup this guard's `Drop` gives.
    fn attached(home: &'a Path, out: &str) -> Self {
        Self {
            home,
            pid: parse_pid(out),
        }
    }
}

impl Drop for Daemon<'_> {
    fn drop(&mut self) {
        let _ = run(self.home, &["daemon", "stop"]);
        if alive(self.pid) {
            let _ = Command::new("kill")
                .args(["-KILL", &self.pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

/// Installing a package, hot-stopping it via `uninstall` while a daemon is
/// reachable, must not leave any trace in `os.json` — the round-3 design
/// finding this whole route exists to avoid: the old (rejected) design
/// reused the persistent `PATCH`, which would have tripped
/// `unknown_disabled`'s fail-closed check on the next start and degraded
/// EVERY module, not just the uninstalled one. This test proves the real,
/// wired-together system: the module's process actually exits, no
/// tombstone, and an unrelated module survives a restart mounted and
/// running.
///
/// The unrelated module used to be the compiled-in `sin90` — free, since the
/// daemon always had it. T11 removed the last compiled-in domain OS, so this
/// installs a second out-of-process package (`survivor`) instead: same
/// property (uninstalling one module must not disturb any other), proven the
/// same way a second real package would be affected if the registry write
/// were scoped wrong, just without a compiled-in module to lean on for free.
#[test]
fn uninstall_hot_stops_a_running_module_and_leaves_no_tombstone() {
    let home = tmp_home();
    let src = write_package(home.path(), "fu61demo", false);
    let (ok, out) = run(home.path(), &["os", "install", &src.to_string_lossy()]);
    assert!(ok, "{out}");
    let survivor_src = write_package(home.path(), "survivor", false);
    let (ok, out) = run(
        home.path(),
        &["os", "install", &survivor_src.to_string_lossy()],
    );
    assert!(ok, "{out}");

    let _daemon = Daemon::start(home.path());

    // Proof the module is genuinely running (handshake completed), not
    // merely `Status::Starting` — which `os list` also renders as
    // `[mounted]`.
    let pid = wait_for_running_pid(home.path(), "fu61demo");
    assert!(
        alive(pid),
        "the module process must be alive before uninstall"
    );
    let survivor_pid = wait_for_running_pid(home.path(), "survivor");
    assert!(
        alive(survivor_pid),
        "the unrelated module must be alive before uninstall too"
    );

    let (ok, out) = run(home.path(), &["os", "uninstall", "fu61demo"]);
    assert!(ok, "{out}");
    assert!(out.contains("removed fu61demo"), "{out}");
    assert!(
        out.contains("told the running daemon to stop serving it"),
        "the daemon was reachable and the module was running: {out}"
    );
    assert!(
        !home.path().join(".agent24/packages/fu61demo").exists(),
        "the package directory must be gone"
    );

    // The property that actually matters: the process is gone, not merely
    // that the CLI printed an optimistic message.
    let by = Instant::now() + Duration::from_secs(10);
    while alive(pid) {
        assert!(
            Instant::now() < by,
            "the hot-stopped module's process is still alive"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // The critical property: no PATCH-shaped tombstone. `stop_now_os` must
    // never have touched `os.json` at all.
    assert!(
        !home.path().join(".agent24/os.json").exists(),
        "stop_now_os wrote to os.json — this is exactly the tombstone bug \
         the design's round-3 review caught and this route exists to avoid"
    );

    // Restart: the uninstalled package is gone from the catalogue, and the
    // registry is genuinely healthy — `survivor` (an unrelated package that
    // was never touched) mounts and reports `[mounted]`, not merely "no
    // error string appeared somewhere in the output" (round-2 code review
    // Medium 2).
    let (ok, _) = run(home.path(), &["daemon", "stop"]);
    assert!(ok);
    let _daemon2 = Daemon::start(home.path());
    let (ok, out) = run(home.path(), &["os", "list"]);
    assert!(ok, "{out}");
    assert!(
        !out.contains("fu61demo"),
        "the uninstalled package must be gone from the catalogue: {out}"
    );
    assert!(
        out.contains("survivor") && out.contains("[mounted]"),
        "an unrelated package must still mount cleanly after the restart: {out}"
    );
}

/// `uninstall` with no reachable daemon: file removal still succeeds, the
/// CLI says so honestly, and — the property that matters most — it does
/// NOT start a daemon to tell it. `attach_only` must never fall back to
/// `spawn_daemon` the way `connect` does; a fake `AGENT24D_BIN` that leaves
/// a durable marker on invocation catches that regression even if the
/// spawned process were too short-lived to observe any other way (FU-61
/// design doc, 判据 13).
#[test]
fn uninstall_without_a_daemon_removes_files_and_spawns_nothing() {
    let home = tmp_home();
    let src = write_package(home.path(), "fu61demo", false);
    let (ok, out) = run(home.path(), &["os", "install", &src.to_string_lossy()]);
    assert!(ok, "{out}");

    let marker = home.path().join("spawned.marker");
    let fake_bin = home.path().join("fake-agent24d.sh");
    std::fs::write(
        &fake_bin,
        format!("#!/bin/sh\necho spawned >> {}\nexit 1\n", marker.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let mut cmd = cli(home.path());
    cmd.env("AGENT24D_BIN", &fake_bin);
    let out = cmd.args(["os", "uninstall", "fu61demo"]).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(text.contains("removed fu61demo"), "{text}");
    assert!(
        text.contains("no reachable daemon"),
        "attach_only must not report a fabricated daemon: {text}"
    );
    assert!(
        !home.path().join(".agent24/packages/fu61demo").exists(),
        "file removal must still succeed with no daemon reachable"
    );
    assert!(
        !marker.exists(),
        "attach_only started a daemon — it must only attach to one already running"
    );
}

/// `agent24 daemon stop` must wait for the daemon to actually release its
/// singleton lock (not just for `POST /shutdown`'s immediate `202`) before
/// reporting success — otherwise the immediately-following `daemon start`
/// can race the still-shutting-down process for the same lock (round-2/3
/// design review High: this is what makes the compound "stop && start"
/// restart advice, which `PackageChanged`'s own hint depends on, reliable).
/// The installed module sleeps briefly on SIGTERM: without a real drain/
/// grace window, the daemon would shut down fast enough that even the OLD
/// fire-and-forget behavior could happen to print after it was already
/// gone, proving nothing.
#[test]
fn daemon_stop_waits_for_the_lock_before_reporting_success() {
    let home = tmp_home();
    let src = write_package(home.path(), "fu61demo", true);
    let (ok, out) = run(home.path(), &["os", "install", &src.to_string_lossy()]);
    assert!(ok, "{out}");
    let _daemon = Daemon::start(home.path());
    wait_for_running_pid(home.path(), "fu61demo");

    let (ok, out) = run(home.path(), &["daemon", "stop"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("stopped"),
        "must report a confirmed stop, not the old fire-and-forget \
         \"shutdown requested\": {out}"
    );
    assert!(!out.contains("shutdown requested"), "{out}");

    // Immediately afterward, `start` must take the plain spawn path — not
    // the lock-collision retry loop, which would still often succeed but
    // defeats the point of this test.
    let (ok, out) = run(home.path(), &["daemon", "start"]);
    assert!(ok, "daemon start raced the lock right after stop: {out}");
    assert!(out.contains("daemon started"), "{out}");
    let _daemon2 = Daemon::attached(home.path(), &out);
}
