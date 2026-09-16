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

/// A package whose module writes its own pid to `<data>/pid` only AFTER its
/// handshake completes — so waiting for that file is waiting for a
/// genuinely admitted, running module, not merely `Status::Starting`
/// (which `os list` also renders as `[mounted]` — round-2 code review
/// Medium 1). `slow_exit`: if true, the module sleeps briefly on SIGTERM
/// before exiting, giving `agent24 daemon stop` a real drain/grace window
/// to race — without this, a daemon with nothing to stop shuts down fast
/// enough that even the OLD fire-and-forget `daemon stop` would happen to
/// print after the process was already gone, defeating the point of
/// testing it.
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
        "import signal, time\n\
         signal.signal(signal.SIGTERM, lambda *_: (time.sleep(1), exit(0)))\n"
    } else {
        ""
    };
    std::fs::write(
        dir.join("mod.py"),
        format!(
            r#"import hashlib, json, os, socket
{sleep_on_term}with open("domain-os.yml", "rb") as f:
    digest = "sha256:" + hashlib.sha256(f.read()).hexdigest()
cb = socket.socket(socket.AF_UNIX)
cb.connect(os.environ["A24_CALLBACK_SOCK"])
req = {{"jsonrpc": "2.0", "id": "1", "method": "initialize", "params": {{
    "protocol_versions": {{"min": 1, "max": 1000}}, "module": {name:?},
    "manifest_digest": digest, "auth_token": os.environ["A24_HANDSHAKE_TOKEN"],
    "capabilities": []}}}}
cb.sendall((json.dumps(req) + "\n").encode())
f = cb.makefile("rb")
f.readline()
with open(os.path.join(os.environ["A24_DATA_DIR"], "pid"), "w") as pf:
    pf.write(str(os.getpid()))
while f.readline():
    pass
"#
        ),
    )
    .unwrap();
    dir
}

/// Waits for `name`'s module to have completed its handshake (its pid file
/// exists) and returns that pid.
fn wait_for_running_pid(home: &Path, name: &str) -> i32 {
    let pid_file = home.join(".agent24/os").join(name).join("pid");
    let by = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(s) = std::fs::read_to_string(&pid_file)
            && let Ok(pid) = s.trim().parse()
        {
            return pid;
        }
        assert!(
            Instant::now() < by,
            "{name} never completed its handshake (no pid file at {})",
            pid_file.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Kills the daemon this test started, whatever happens — a failing
/// assertion must not leave a daemon holding the singleton lock or running
/// a module process. Tries a clean `daemon stop` first; if that does not
/// make the daemon's own pid disappear promptly (a hung/unhealthy daemon,
/// or an assertion firing mid-drain), force-kills it directly — mirroring
/// `agent24d`'s own `tests/daemon_modules.rs::Running` guard, which does
/// not trust a graceful path to always work either.
struct Daemon<'a> {
    home: &'a Path,
}

impl Drop for Daemon<'_> {
    fn drop(&mut self) {
        let _ = run(self.home, &["daemon", "stop"]);
        if let Ok(s) = std::fs::read_to_string(self.home.join(".agent24/daemon.json"))
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&s)
            && let Some(pid) = v["pid"].as_i64()
        {
            let pid = pid as i32;
            if alive(pid) {
                let _ = Command::new("kill")
                    .args(["-KILL", &pid.to_string()])
                    .stderr(std::process::Stdio::null())
                    .status();
            }
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
/// tombstone, and an unrelated module (the compiled-in `sin90`) survives a
/// restart mounted and running.
#[test]
fn uninstall_hot_stops_a_running_module_and_leaves_no_tombstone() {
    let home = tmp_home();
    let src = write_package(home.path(), "fu61demo", false);
    let (ok, out) = run(home.path(), &["os", "install", &src.to_string_lossy()]);
    assert!(ok, "{out}");

    let (ok, out) = run(home.path(), &["daemon", "start"]);
    assert!(ok, "{out}");
    let _daemon = Daemon { home: home.path() };

    // Proof the module is genuinely running (handshake completed), not
    // merely `Status::Starting` — which `os list` also renders as
    // `[mounted]`.
    let pid = wait_for_running_pid(home.path(), "fu61demo");
    assert!(
        alive(pid),
        "the module process must be alive before uninstall"
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
    // registry is genuinely healthy — `sin90` (compiled in, unrelated)
    // mounts and reports `[mounted]`, not merely "no error string appeared
    // somewhere in the output" (round-2 code review Medium 2).
    let (ok, _) = run(home.path(), &["daemon", "stop"]);
    assert!(ok);
    let (ok, out) = run(home.path(), &["daemon", "start"]);
    assert!(ok, "{out}");
    let _daemon2 = Daemon { home: home.path() };
    let (ok, out) = run(home.path(), &["os", "list"]);
    assert!(ok, "{out}");
    assert!(
        !out.contains("fu61demo"),
        "the uninstalled package must be gone from the catalogue: {out}"
    );
    assert!(
        out.contains("sin90") && out.contains("[mounted]"),
        "an unrelated compiled-in module must still mount cleanly after the restart: {out}"
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
    let (ok, out) = run(home.path(), &["daemon", "start"]);
    assert!(ok, "{out}");
    let _daemon = Daemon { home: home.path() };
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
    let _daemon2 = Daemon { home: home.path() };
}
