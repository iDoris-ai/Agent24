//! K1-6a-G §2.1 F1: `agent24 os install` and `uninstall` end the module's
//! grants, and a store that cannot record that fails the command — and
//! running it again retries, rather than finding nothing left to do.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Output};

fn agent24(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agent24"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("TMPDIR", home.join("tmp"))
        .args(args)
        .output()
        .unwrap()
}

fn tmp_home() -> tempfile::TempDir {
    let home = tempfile::Builder::new()
        .prefix("a24f1")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::create_dir(home.path().join("tmp")).unwrap();
    home
}

/// A directory where the store file goes: the store cannot open.
fn break_store(home: &Path) -> std::path::PathBuf {
    let db = home.join(".agent24").join("agent24.db");
    std::fs::create_dir_all(&db).unwrap();
    db
}

fn write_package(home: &Path) -> String {
    let dir = home.join("srcpkg").join("noted");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("domain-os.yml"),
        "name: noted\nversion: \"0.1.0\"\nroute_namespace: /api/v1/noted\n\
         event_module: noted\ndata_dir: ~/.agent24/os/noted/\n\
         kernel_capabilities: []\nimpl_kind: out_of_process_provider\n\
         spawn:\n  command: \"false\"\n  args: []\n",
    )
    .unwrap();
    dir.to_string_lossy().into_owned()
}

fn assert_fails_on_grants(out: &Output) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("grants"), "{stderr}");
}

fn assert_ok(out: &Output) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn an_uninstall_whose_grants_cannot_be_revoked_fails_every_time() {
    let home = tmp_home();
    let db = break_store(home.path());
    for _ in 0..2 {
        assert_fails_on_grants(&agent24(home.path(), &["os", "uninstall", "ghost"]));
    }
    std::fs::remove_dir(&db).unwrap();
    assert_ok(&agent24(home.path(), &["os", "uninstall", "ghost"]));
}

/// A removal whose grants are not revoked fails; the next run finds the
/// files gone and revokes again.
#[test]
fn an_uninstall_that_removed_the_files_retries_the_revocation() {
    let home = tmp_home();
    let src = write_package(home.path());
    assert_ok(&agent24(home.path(), &["os", "install", &src]));
    let installed = home.path().join(".agent24/packages/noted");
    assert!(installed.exists());
    let db = home.path().join(".agent24/agent24.db");
    std::fs::remove_file(&db).unwrap();
    break_store(home.path());
    assert_fails_on_grants(&agent24(home.path(), &["os", "uninstall", "noted"]));
    assert!(!installed.exists());
    std::fs::remove_dir(&db).unwrap();
    assert_ok(&agent24(home.path(), &["os", "uninstall", "noted"]));
}

/// An install whose grants are not revoked is undone, so installing again
/// retries the revocation rather than finding the package already there.
#[test]
fn an_install_whose_grants_cannot_be_revoked_is_undone() {
    let home = tmp_home();
    let src = write_package(home.path());
    let db = break_store(home.path());
    assert_fails_on_grants(&agent24(home.path(), &["os", "install", &src]));
    assert!(!home.path().join(".agent24/packages/noted").exists());
    std::fs::remove_dir(&db).unwrap();
    assert_ok(&agent24(home.path(), &["os", "install", &src]));
}

/// No HOME, no store to revoke in: not a success (review G3).
#[test]
fn an_uninstall_without_home_does_not_claim_the_grants_ended() {
    let home = tmp_home();
    let packages = home.path().join("packages");
    std::fs::create_dir(&packages).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_agent24"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("TMPDIR", home.path().join("tmp"))
        .env("A24_OS_PACKAGES", &packages)
        .args(["os", "uninstall", "ghost"])
        .output()
        .unwrap();
    assert_fails_on_grants(&out);
}
