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
    // CORE-5a wording: "... module consent ...".
    assert!(stderr.contains("module consent"), "{stderr}");
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
    // CORE-5a: a package that is not installed never opens the store, so the
    // revocation is only reached for an installed one.
    let src = write_package(home.path());
    assert_ok(&agent24(home.path(), &["os", "install", &src]));
    let db = break_store(home.path());
    for _ in 0..2 {
        assert_fails_on_grants(&agent24(home.path(), &["os", "uninstall", "noted"]));
    }
    std::fs::remove_dir(&db).unwrap();
    assert_ok(&agent24(home.path(), &["os", "uninstall", "noted"]));
}

/// CORE-5a ordering (supersedes the K1-6a-G "remove, then revoke" one): the
/// revocation comes first, so a removal whose grants cannot be revoked fails
/// and leaves the files where they are; the next run revokes and removes.
#[test]
fn an_uninstall_that_cannot_revoke_keeps_the_files_and_retries() {
    let home = tmp_home();
    let src = write_package(home.path());
    assert_ok(&agent24(home.path(), &["os", "install", &src]));
    let installed = home.path().join(".agent24/packages/noted");
    assert!(installed.exists());
    let db = home.path().join(".agent24/agent24.db");
    for file in ["agent24.db", "agent24.db-wal", "agent24.db-shm"] {
        let _ = std::fs::remove_file(home.path().join(".agent24").join(file));
    }
    break_store(home.path());
    assert_fails_on_grants(&agent24(home.path(), &["os", "uninstall", "noted"]));
    assert!(installed.exists(), "files stay until the revocation lands");
    std::fs::remove_dir(&db).unwrap();
    assert_ok(&agent24(home.path(), &["os", "uninstall", "noted"]));
    assert!(!installed.exists());
}

/// An install whose grants cannot be revoked never lands (CORE-5a: the
/// revocation precedes `install`), so installing again retries it rather than
/// finding the package already there.
#[test]
fn an_install_whose_grants_cannot_be_revoked_is_undone() {
    let home = tmp_home();
    let src = write_package(home.path());
    let db = break_store(home.path());
    let out = agent24(home.path(), &["os", "install", &src]);
    assert_fails_on_grants(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("installed"), "{stdout}");
    assert!(!home.path().join(".agent24/packages/noted").exists());
    std::fs::remove_dir(&db).unwrap();
    assert_ok(&agent24(home.path(), &["os", "install", &src]));
}

/// A store that opens but cannot write the revocation fails the command
/// too, not only one that cannot open (#1157 review S1).
#[test]
fn an_uninstall_whose_revocation_cannot_be_written_fails() {
    let home = tmp_home();
    let src = write_package(home.path());
    assert_ok(&agent24(home.path(), &["os", "install", &src]));
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = home.path().join(".agent24/agent24.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = agent24_store::Store::open(&db).await.unwrap();
        sqlx::query(
            "CREATE TRIGGER refuse BEFORE INSERT ON module_consent_generations \
             BEGIN SELECT RAISE(ABORT, 'refused'); END",
        )
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
    });
    assert_fails_on_grants(&agent24(home.path(), &["os", "uninstall", "noted"]));
}

/// No HOME, no store to reach: the install and the uninstall stand, and say
/// any grants were not revoked rather than nothing (ADR-K1-03 §3, review G3;
/// failing them would break the `A24_OS_PACKAGES` install the CLI documents
/// for containers).
#[test]
fn an_install_and_uninstall_without_home_say_there_was_no_store() {
    let home = tmp_home();
    let src = write_package(home.path());
    let packages = home.path().join("packages");
    std::fs::create_dir(&packages).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_agent24"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("TMPDIR", home.path().join("tmp"))
            .env("A24_OS_PACKAGES", &packages)
            .args(args)
            .output()
            .unwrap()
    };
    for args in [
        vec!["os", "install", src.as_str()],
        vec!["os", "uninstall", "noted"],
    ] {
        let out = run(&args);
        assert_ok(&out);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("were not revoked"), "{args:?}: {stderr}");
    }
    assert!(!packages.join("noted").exists());
}
