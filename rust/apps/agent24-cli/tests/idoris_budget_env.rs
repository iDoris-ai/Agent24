#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agent24"))
        .args(args)
        .env("A24_HW_BUDGET_CAP_GB", "8")
        .env_remove("IDORIS_ALLOCATED_BUDGET_BYTES")
        .output()
        .unwrap()
}

#[test]
fn allocated_bytes_reach_an_independent_process_without_rebudgeting() {
    let probe = run(&["hw", "probe"]);
    assert!(probe.status.success());
    let report: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    let usable = report["usable_bytes"].as_u64().unwrap();
    let physical = report["profile"]["total_mem_bytes"].as_u64().unwrap();
    assert!(physical > usable);
    for reserved in [0, 1234, usable, u64::MAX] {
        let out = run(&[
            "hw",
            "idoris-env",
            "--core-reserved-bytes",
            &reserved.to_string(),
        ]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let line = String::from_utf8(out.stdout).unwrap();
        let (name, value) = line.trim().split_once('=').unwrap();
        assert_eq!(name, "IDORIS_ALLOCATED_BUDGET_BYTES");
        assert_eq!(
            value.parse::<u64>().unwrap(),
            usable.saturating_sub(reserved)
        );
        // The supported manual startup boundary: an independent process receives
        // the exact nonsecret value rather than recomputing OS reserve from RAM.
        let child = Command::new("/bin/sh")
            .args([
                "-c",
                "test \"$IDORIS_ALLOCATED_BUDGET_BYTES\" = \"$EXPECTED\"",
            ])
            .env(name, value)
            .env("EXPECTED", usable.saturating_sub(reserved).to_string())
            .status()
            .unwrap();
        assert!(child.success());
    }
}

#[test]
fn core_reservation_is_explicit_and_validated() {
    for args in [
        vec!["hw", "idoris-env"],
        vec!["hw", "idoris-env", "--core-reserved-bytes", "-1"],
        vec!["hw", "idoris-env", "--core-reserved-bytes", "1.5"],
        vec![
            "hw",
            "idoris-env",
            "--core-reserved-bytes",
            "18446744073709551616",
        ],
    ] {
        let out = run(&args);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
    }
}
