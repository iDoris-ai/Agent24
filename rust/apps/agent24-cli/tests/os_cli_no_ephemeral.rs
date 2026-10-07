//! M2-05 — `agent24 os list/enable/disable` must attach only to the resident
//! daemon. A temporary daemon sees an isolated package root and would report
//! false registry state.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::Command;

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("a24m205")
        .tempdir_in("/tmp")
        .unwrap()
}

fn cli(home: &Path, daemon_bin: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_agent24"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("TMPDIR", home.join("tmp"))
        .env("AGENT24D_BIN", daemon_bin)
        .args(args)
        .output()
        .unwrap()
}

fn install_package(home: &Path, daemon_bin: &Path) {
    let package = home.join("m205demo");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("domain-os.yml"),
        "name: m205demo\nversion: \"0.1.0\"\nroute_namespace: /api/v1/m205demo\n\
         event_module: m205demo\ndata_dir: ~/.agent24/os/m205demo/\n\
         kernel_capabilities: []\nimpl_kind: attached_process\n",
    )
    .unwrap();
    let path = package.to_string_lossy();
    let out = cli(home, daemon_bin, &["os", "install", &path]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fake_daemon(home: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let binary = home.join("fake-agent24d.sh");
    let marker = home.join("daemon-was-started");
    let real_daemon =
        std::path::Path::new(env!("CARGO_BIN_EXE_agent24")).with_file_name("agent24d");
    std::fs::write(
        &binary,
        format!(
            "#!/bin/sh\nprintf started >> '{}'\nexec '{}' \"$@\"\n",
            marker.display(),
            real_daemon.display(),
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (binary, marker)
}

fn output_text(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn os_list_without_daemon_reports_offline_and_spawns_nothing() {
    let home = tmp_home();
    let (daemon, marker) = fake_daemon(home.path());
    install_package(home.path(), &daemon);

    let out = cli(home.path(), &daemon, &["os", "list"]);
    let text = output_text(&out);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("not running"), "{text}");
    assert!(text.contains("os.json"), "{text}");
    assert!(!text.contains("(no domain OS installed)"), "{text}");
    assert!(!marker.exists(), "os list started an agent24d process");
    assert!(
        !home.path().join(".agent24/daemon.json").exists(),
        "os list created daemon state despite no resident daemon"
    );
    let temp_root = home.path().join("tmp");
    assert!(
        !temp_root.exists() || std::fs::read_dir(temp_root).unwrap().next().is_none(),
        "os list created an ephemeral package root"
    );
}

#[test]
fn os_disable_without_daemon_fails_with_os_json_edit_hint_and_spawns_nothing() {
    let home = tmp_home();
    let (daemon, marker) = fake_daemon(home.path());
    install_package(home.path(), &daemon);

    let out = cli(home.path(), &daemon, &["os", "disable", "m205demo"]);
    let text = output_text(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("not running"), "{text}");
    assert!(text.contains("os.json"), "{text}");
    assert!(text.contains("m205demo"), "{text}");
    assert!(!marker.exists(), "os disable started an agent24d process");
    assert!(!home.path().join(".agent24/daemon.json").exists());
    let temp_root = home.path().join("tmp");
    assert!(
        !temp_root.exists() || std::fs::read_dir(temp_root).unwrap().next().is_none(),
        "os disable created an ephemeral package root"
    );
}
