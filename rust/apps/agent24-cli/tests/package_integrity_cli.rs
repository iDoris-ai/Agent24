#![allow(clippy::unwrap_used)]
use std::{fs, process::Command};

#[test]
fn package_integrity_cli_generates_verifies_and_reports_relative_failure() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("bin")).unwrap();
    fs::write(dir.path().join("bin/demo"), b"not executable code").unwrap();
    fs::write(dir.path().join("domain-os.yml"), "name: demo\nversion: '1.0.0'\nroute_namespace: /api/v1/demo\nevent_module: demo\ndata_dir: ~/.agent24/os/demo/\nkernel_capabilities: [events]\nimpl_kind: out_of_process_provider\nspawn:\n  command: bin/demo\n").unwrap();
    let run = |action: &str| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_agent24"));
        cmd.args(["package-integrity", action, "--root"])
            .arg(dir.path());
        if action == "generate" {
            cmd.args(["--executable", "bin/demo"]);
        }
        cmd.output().unwrap()
    };
    assert!(!run("verify").status.success());
    let generated = run("generate");
    assert!(generated.status.success(), "{:?}", generated);
    let yaml = fs::read(dir.path().join("domain-os.yml")).unwrap();
    let json = fs::read(dir.path().join("files.json")).unwrap();
    assert!(run("generate").status.success());
    assert_eq!(yaml, fs::read(dir.path().join("domain-os.yml")).unwrap());
    assert_eq!(json, fs::read(dir.path().join("files.json")).unwrap());
    let valid = run("verify");
    assert!(valid.status.success(), "{:?}", valid);
    assert!(valid.stdout.is_empty());
    #[cfg(unix)]
    {
        let parent = tempfile::tempdir().unwrap();
        let alias = parent.path().join("alias");
        std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
        for suffix in ["", "/", "/."] {
            for action in ["verify", "generate"] {
                let mut command = Command::new(env!("CARGO_BIN_EXE_agent24"));
                command
                    .args(["package-integrity", action, "--root"])
                    .arg(format!("{}{suffix}", alias.display()));
                if action == "generate" {
                    command.args(["--executable", "bin/demo"]);
                }
                let result = command.output().unwrap();
                assert!(!result.status.success(), "{action}: {suffix}");
            }
        }
    }
    fs::remove_file(dir.path().join("bin/demo")).unwrap();
    let invalid = run("verify");
    assert!(!invalid.status.success());
    let err = String::from_utf8(invalid.stderr).unwrap();
    assert!(
        err.contains("integrity_manifest_invalid") && err.contains("bin/demo"),
        "{err}"
    );
    assert!(!err.contains(dir.path().to_str().unwrap()), "{err}");
}
