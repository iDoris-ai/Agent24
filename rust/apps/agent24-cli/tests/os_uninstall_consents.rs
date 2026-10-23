//! K1-6a-G §2.1 F1: `agent24 os uninstall` ends the module's grants, and a
//! store that cannot record that fails the command — again on every run,
//! not "nothing to remove" the second time.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Output};

fn uninstall(home: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agent24"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("TMPDIR", home.join("tmp"))
        .args(["os", "uninstall", "ghost"])
        .output()
        .unwrap()
}

#[test]
fn an_uninstall_whose_grants_cannot_be_revoked_fails_every_time() {
    let home = tempfile::Builder::new()
        .prefix("a24f1")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::create_dir(home.path().join("tmp")).unwrap();
    // A directory where the store file goes: the store cannot open.
    let db = home.path().join(".agent24").join("agent24.db");
    std::fs::create_dir_all(&db).unwrap();
    for _ in 0..2 {
        let out = uninstall(home.path());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{stderr}");
        assert!(stderr.contains("grants"), "{stderr}");
    }
    std::fs::remove_dir(&db).unwrap();
    let out = uninstall(home.path());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
