//! A3-2a CLI snapshot tests — `agent24 os attach add --json`'s two required
//! outputs (design doc `docs/design/A3-ATTACHED-MODULE.md` §3.6): the success
//! shape and the `relax_requires_confirmation` failure shape. Driven through
//! the real `agent24`/`agent24d` binaries, same isolation pattern as
//! `uninstall_hot_stop.rs` (see that file's own doc comment for why: this
//! CLI's control flow — `connect`, `resolve_allow_relax`, `finish` — is thin
//! glue over cross-process/global-`$HOME` state a unit test inside `main.rs`
//! cannot safely exercise without mutating the TEST PROCESS's own
//! environment).
//!
//! "字段集合与类型钉死，token 值打码" (A3-2a's own scope note): the token
//! itself is random per run, so these tests assert its SHAPE (a lowercase hex
//! string) and that the field SET is exactly right, never a literal captured
//! value.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Stdio};

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("a24attach")
        .tempdir_in("/tmp")
        .unwrap()
}

/// Same resolution `agent24-cli`'s own `agent24d_binary()` uses in
/// production when `AGENT24D_BIN` is unset (see `uninstall_hot_stop.rs`).
fn agent24d_bin() -> std::path::PathBuf {
    let path = std::path::Path::new(env!("CARGO_BIN_EXE_agent24")).with_file_name("agent24d");
    assert!(
        path.exists(),
        "{} does not exist — run these tests via `cargo test --workspace`",
        path.display()
    );
    path
}

/// Runs the CLI with stdin, stdout and stderr all PIPED (never inherited) —
/// this is what makes stdin NOT a TTY, which is exactly the condition
/// §3.5/§3.6 describe for an unattended caller (AgentEar's own auto-pairing).
/// Returns `(success, stdout, stderr)` separately: the `--json` contract is
/// about stdout alone, and conflating the two (as `uninstall_hot_stop.rs`
/// does for its plain-text assertions) would let a stray stderr line pass as
/// valid JSON on stdout.
fn run(home: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_agent24"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("AGENT24D_BIN", agent24d_bin())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn write_manifest(home: &Path, name: &str, model_access: Option<&str>) -> std::path::PathBuf {
    let path = home.join(format!("{name}.yml"));
    let ma = model_access
        .map(|m| format!("model_access: {m}\n"))
        .unwrap_or_default();
    std::fs::write(
        &path,
        format!(
            "name: {name}\nversion: \"1\"\nroute_namespace: /api/v1/{name}\n\
             event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
             impl_kind: attached_process\nkernel_capabilities: [events, models]\n{ma}"
        ),
    )
    .unwrap();
    path
}

fn is_lowercase_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Kills the daemon this test started (same guard shape as
/// `uninstall_hot_stop.rs::Daemon`, trimmed to what this file needs).
struct Daemon<'a> {
    home: &'a Path,
}

impl Drop for Daemon<'_> {
    fn drop(&mut self) {
        let _ = run(self.home, &["daemon", "stop"]);
    }
}

/// SNAPSHOT 1 — `add --json`'s success shape: exactly one JSON object on
/// stdout, with exactly the field set `{name, manifest_digest, token,
/// socket_path, token_id}` (§3.2/§3.6), each of the right TYPE and SHAPE.
#[test]
fn attach_add_json_success_snapshot() {
    let home = tmp_home();
    let (ok, out, err) = run(home.path(), &["daemon", "start"]);
    assert!(ok, "{out} {err}");
    let _daemon = Daemon { home: home.path() };

    let manifest = write_manifest(home.path(), "agentear", None);
    let (ok, stdout, stderr) = run(
        home.path(),
        &["os", "attach", "add", &manifest.to_string_lossy(), "--json"],
    );
    assert!(ok, "stdout={stdout} stderr={stderr}");

    // Exactly one line, exactly one JSON object — "stdout 恰为一个 JSON 对象".
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "stdout must be exactly one line: {stdout:?}"
    );
    let body: serde_json::Value = serde_json::from_str(lines[0]).expect("valid JSON");
    let obj = body.as_object().expect("a JSON object");

    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "manifest_digest",
            "name",
            "socket_path",
            "token",
            "token_id"
        ],
        "field set must be exactly {{name, manifest_digest, token, socket_path, token_id}}"
    );

    assert_eq!(body["name"], "agentear");
    let digest = body["manifest_digest"].as_str().unwrap();
    assert!(
        digest
            .strip_prefix("sha256:")
            .is_some_and(|h| is_lowercase_hex(h, 64)),
        "manifest_digest must be sha256:<64 lowercase hex>: {digest:?}"
    );
    // Token value is masked here on purpose (§3.6's own note): only its SHAPE
    // is pinned, never a literal capture of the random value.
    let token = body["token"].as_str().unwrap();
    assert!(
        is_lowercase_hex(token, 64),
        "token must be 64 lowercase hex chars (32 bytes, launch::TOKEN_BYTES)"
    );
    let token_id = body["token_id"].as_str().unwrap();
    assert!(
        token_id
            .strip_prefix("tok_")
            .is_some_and(|h| is_lowercase_hex(h, 8)),
        "token_id must be tok_<8 lowercase hex>: {token_id:?}"
    );
    assert!(
        body["socket_path"]
            .as_str()
            .unwrap()
            .ends_with("attach/agent24d.sock"),
        "{body}"
    );
}

/// SNAPSHOT 2 — `add --json`'s `relax_requires_confirmation` failure shape.
/// Driven non-interactively (stdin is `/dev/null`, never a TTY): per
/// §3.5/§3.6 this must refuse a relaxing manifest even with `--allow-remote`
/// on the command line, because the TTY+"yes" confirmation can never happen.
#[test]
fn attach_add_json_relax_requires_confirmation_snapshot() {
    let home = tmp_home();
    let (ok, out, err) = run(home.path(), &["daemon", "start"]);
    assert!(ok, "{out} {err}");
    let _daemon = Daemon { home: home.path() };

    let manifest = write_manifest(home.path(), "agentear", Some("remote_allowed"));
    let (ok, stdout, stderr) = run(
        home.path(),
        &[
            "os",
            "attach",
            "add",
            &manifest.to_string_lossy(),
            "--allow-remote",
            "--json",
        ],
    );
    assert!(
        !ok,
        "a relax without a TTY confirmation must fail: {stdout} {stderr}"
    );

    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "stdout must be exactly one line: {stdout:?}"
    );
    let body: serde_json::Value = serde_json::from_str(lines[0]).expect("valid JSON");
    let obj = body.as_object().expect("a JSON object");
    assert_eq!(
        obj.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["error"],
        "the failure body must be exactly {{\"error\": {{...}}}}"
    );
    assert_eq!(body["error"]["code"], "relax_requires_confirmation");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty())
    );

    // And the registry genuinely was not touched.
    let (ok, stdout2, stderr2) = run(home.path(), &["os", "attach", "list", "--json"]);
    assert!(ok, "{stdout2} {stderr2}");
    let list: serde_json::Value = serde_json::from_str(stdout2.lines().next().unwrap()).unwrap();
    assert_eq!(list["modules"].as_array().unwrap().len(), 0);
}

/// The plain (non-`--json`) success path, and `list`/`remove` round-tripping
/// through the real daemon — the storage-layer and REST-layer unit tests in
/// `agent24d` already cover the registry logic itself; this is the seam
/// those tests cannot reach: the CLI's own request building, response
/// parsing and exit codes.
#[test]
fn attach_add_list_remove_round_trip() {
    let home = tmp_home();
    let (ok, out, err) = run(home.path(), &["daemon", "start"]);
    assert!(ok, "{out} {err}");
    let _daemon = Daemon { home: home.path() };

    let manifest = write_manifest(home.path(), "agentear", None);
    let (ok, out, err) = run(
        home.path(),
        &["os", "attach", "add", &manifest.to_string_lossy()],
    );
    assert!(ok, "{out} {err}");
    assert!(out.contains("registered agentear"), "{out}");
    assert!(out.contains("shown once"), "{out}");

    let (ok, out, err) = run(home.path(), &["os", "attach", "list"]);
    assert!(ok, "{out} {err}");
    assert!(out.contains("agentear"), "{out}");
    assert!(out.contains("detached"), "{out}");

    let (ok, out, err) = run(home.path(), &["os", "attach", "remove", "agentear"]);
    assert!(ok, "{out} {err}");
    assert!(out.contains("removed agentear"), "{out}");

    let (ok, out, err) = run(home.path(), &["os", "attach", "list"]);
    assert!(ok, "{out} {err}");
    assert!(out.contains("no attached module"), "{out}");

    // `revoke` is accepted as an alias for `remove` (§3.6's own spelling).
    run(
        home.path(),
        &["os", "attach", "add", &manifest.to_string_lossy()],
    );
    let (ok, out, err) = run(home.path(), &["os", "attach", "revoke", "agentear"]);
    assert!(ok, "{out} {err}");
    assert!(out.contains("removed agentear"), "{out}");
}
