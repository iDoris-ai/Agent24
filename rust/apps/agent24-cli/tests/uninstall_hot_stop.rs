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
    std::path::Path::new(env!("CARGO_BIN_EXE_agent24")).with_file_name("agent24d")
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

fn write_package(home: &Path, name: &str) -> std::path::PathBuf {
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
    std::fs::write(
        dir.join("mod.py"),
        r#"import hashlib, json, os, socket
with open("domain-os.yml", "rb") as f:
    digest = "sha256:" + hashlib.sha256(f.read()).hexdigest()
cb = socket.socket(socket.AF_UNIX)
cb.connect(os.environ["A24_CALLBACK_SOCK"])
req = {"jsonrpc": "2.0", "id": "1", "method": "initialize", "params": {
    "protocol_versions": {"min": 1, "max": 1000}, "module": os.environ["A24_MODULE_NAME"],
    "manifest_digest": digest, "auth_token": os.environ["A24_HANDSHAKE_TOKEN"],
    "capabilities": []}}
cb.sendall((json.dumps(req) + "\n").encode())
f = cb.makefile("rb")
f.readline()
while f.readline():
    pass
"#
        .replace("os.environ[\"A24_MODULE_NAME\"]", &format!("{name:?}")),
    )
    .unwrap();
    dir
}

/// Kills the daemon this test started, whatever happens — a failing
/// assertion must not leave a daemon holding the singleton lock for the
/// next test that reuses (or, worse, does not reuse) this `$HOME`.
struct Daemon<'a> {
    home: &'a Path,
}

impl Drop for Daemon<'_> {
    fn drop(&mut self) {
        let _ = run(self.home, &["daemon", "stop"]);
    }
}

/// Installing a package, hot-stopping it via `uninstall` while a daemon is
/// reachable, must not leave any trace in `os.json` — the round-3 design
/// finding this whole route exists to avoid: the old (rejected) design
/// reused the persistent `PATCH`, which would have tripped
/// `unknown_disabled`'s fail-closed check on the next start and degraded
/// EVERY module, not just the uninstalled one. This test proves the real,
/// wired-together system: no tombstone, and an unrelated module survives a
/// restart untouched.
#[test]
fn uninstall_hot_stops_a_running_module_and_leaves_no_tombstone() {
    let home = tmp_home();
    let src = write_package(home.path(), "fu61demo");
    let (ok, out) = run(home.path(), &["os", "install", &src.to_string_lossy()]);
    assert!(ok, "{out}");

    let (ok, out) = run(home.path(), &["daemon", "start"]);
    assert!(ok, "{out}");
    let _daemon = Daemon { home: home.path() };

    // Wait for the mount to settle before uninstalling — otherwise the hot
    // stop can race the module's very first handshake.
    let by = Instant::now() + Duration::from_secs(30);
    loop {
        let (_, out) = run(home.path(), &["os", "list"]);
        if out.contains("fu61demo") && out.contains("[mounted]") {
            break;
        }
        assert!(Instant::now() < by, "the package never mounted: {out}");
        std::thread::sleep(Duration::from_millis(100));
    }

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

    // The critical property: no PATCH-shaped tombstone. `stop_now_os` must
    // never have touched `os.json` at all.
    assert!(
        !home.path().join(".agent24/os.json").exists(),
        "stop_now_os wrote to os.json — this is exactly the tombstone bug \
         the design's round-3 review caught and this route exists to avoid"
    );

    // Restart: an unrelated healthy module (there is none installed here
    // besides the compiled-in `sin90`) must mount cleanly — no
    // `registry_error` from a phantom disabled entry for a package that no
    // longer exists.
    let (ok, _) = run(home.path(), &["daemon", "stop"]);
    assert!(ok);
    let (ok, out) = run(home.path(), &["daemon", "start"]);
    assert!(ok, "{out}");
    let (_, out) = run(home.path(), &["os", "list"]);
    assert!(
        !out.contains("fu61demo"),
        "the uninstalled package must be gone from the catalogue: {out}"
    );
    assert!(
        !out.to_lowercase().contains("registry"),
        "the registry must not be reported broken: {out}"
    );
    let _daemon2 = Daemon { home: home.path() };
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
    let src = write_package(home.path(), "fu61demo");
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
#[test]
fn daemon_stop_waits_for_the_lock_before_reporting_success() {
    let home = tmp_home();
    let (ok, out) = run(home.path(), &["daemon", "start"]);
    assert!(ok, "{out}");

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
    let _daemon = Daemon { home: home.path() };
}
