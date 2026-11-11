#![allow(clippy::unwrap_used)]
#[path = "support/signature_command.rs"]
mod signature_command;

use std::fs;

#[test]
fn package_integrity_cli_generates_verifies_and_reports_relative_failure() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("bin")).unwrap();
    let current = std::env::current_exe().unwrap();
    fs::copy(current, dir.path().join("bin/demo")).unwrap();
    fs::write(dir.path().join("domain-os.yml"), "name: demo\nversion: '1.0.0'\nroute_namespace: /api/v1/demo\nevent_module: demo\ndata_dir: ~/.agent24/os/demo/\nkernel_capabilities: [events]\nimpl_kind: out_of_process_provider\nspawn:\n  command: bin/demo\n").unwrap();
    let run = |action: &str| {
        let mut cmd = signature_command::command(env!("CARGO_BIN_EXE_agent24"));
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
                let mut command = signature_command::command(env!("CARGO_BIN_EXE_agent24"));
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

#[test]
fn package_integrity_cli_rejects_unknown_arch() {
    let output = signature_command::command(env!("CARGO_BIN_EXE_agent24"))
        .args([
            "package-integrity",
            "generate",
            "--root",
            ".",
            "--executable",
            "bin/demo",
            "--arch",
            "unknown",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("invalid value"), "{error}");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn package_integrity_cli_generates_x86_64_but_verifies_host_arch() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("bin")).unwrap();
    let mut macho = vec![0u8; 56];
    for (offset, value) in [
        (0, 0xfeedfacfu32),
        (4, 0x01000007),
        (12, 2),
        (16, 1),
        (20, 24),
        (32, 0x80000028),
        (36, 24),
    ] {
        macho[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    fs::write(dir.path().join("bin/demo"), macho).unwrap();
    fs::write(dir.path().join("domain-os.yml"), "name: demo\nversion: '1.0.0'\nroute_namespace: /api/v1/demo\nevent_module: demo\ndata_dir: ~/.agent24/os/demo/\nkernel_capabilities: [events]\nimpl_kind: out_of_process_provider\nspawn:\n  command: bin/demo\n").unwrap();
    let run = |action: &str, extra: &[&str]| {
        signature_command::command(env!("CARGO_BIN_EXE_agent24"))
            .args(["package-integrity", action, "--root"])
            .arg(dir.path())
            .args(extra)
            .output()
            .unwrap()
    };
    let default = run("generate", &["--executable", "bin/demo"]);
    assert!(!default.status.success());
    assert!(
        String::from_utf8(default.stderr)
            .unwrap()
            .contains("not_native_executable: bin/demo")
    );
    assert!(!dir.path().join("files.json").exists());
    let targeted = run(
        "generate",
        &["--executable", "bin/demo", "--arch", "x86_64"],
    );
    assert!(targeted.status.success(), "{targeted:?}");
    let verified = run("verify", &[]);
    assert!(!verified.status.success());
    assert!(
        String::from_utf8(verified.stderr)
            .unwrap()
            .contains("not_native_executable: bin/demo")
    );
    // The release script's self-check of a cross-architecture package.
    let targeted_verify = run("verify", &["--arch", "x86_64"]);
    assert!(targeted_verify.status.success(), "{targeted_verify:?}");
}
