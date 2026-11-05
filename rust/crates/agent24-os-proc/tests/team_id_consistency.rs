//! S1-2 / E2: the Apple Team ID has one Rust definition
//! (`agent24_os_proc::DEVELOPER_ID_TEAM_ID`); every other place that repeats
//! it must agree. Change any one of them and a test here goes red.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_os_proc::DEVELOPER_ID_TEAM_ID;
use std::path::PathBuf;

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(rel)
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn is_team_id(s: &str) -> bool {
    s.len() == 10
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// Every Team ID a text file states: `(TEAMID)` as in "Developer ID
/// Application: Name (TEAMID)", `apple-notary-TEAMID`, and
/// `EXPECTED_TEAM_ID = 'TEAMID'`.
fn team_ids_in(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (open, close) in [("(", ")"), ("apple-notary-", "")] {
        let mut rest = text;
        while let Some(at) = rest.find(open) {
            let after = &rest[at + open.len()..];
            let candidate = after.get(..10).unwrap_or("");
            let closes = close.is_empty() || after.get(10..).is_some_and(|t| t.starts_with(close));
            if is_team_id(candidate) && closes {
                found.push(candidate.to_owned());
            }
            rest = after;
        }
    }
    for line in text.lines().filter(|l| l.contains("EXPECTED_TEAM_ID")) {
        if let Some(q) = line.find(['\'', '"']) {
            let value: String = line[q + 1..].chars().take(10).collect();
            if is_team_id(&value) {
                found.push(value);
            }
        }
    }
    found
}

fn assert_all_match(file: &str, ids: &[String], at_least: usize) {
    assert!(
        ids.len() >= at_least,
        "{file}: expected at least {at_least} Team ID mentions, found {ids:?} (extractor out of date?)"
    );
    for id in ids {
        assert_eq!(
            id, DEVELOPER_ID_TEAM_ID,
            "{file} disagrees with DEVELOPER_ID_TEAM_ID"
        );
    }
}

#[test]
fn constant_is_a_well_formed_team_id() {
    assert!(is_team_id(DEVELOPER_ID_TEAM_ID));
}

#[test]
fn component_locks_name_the_constant() {
    for lock in [
        "rust/crates/agent24-comm/hyphae.lock.json",
        "rust/crates/agent24-idoris/idoris.lock.json",
    ] {
        let json: serde_json::Value = serde_json::from_str(&read(lock)).unwrap();
        let id = json["signing"]["macos_team_id"]
            .as_str()
            .unwrap_or_else(|| panic!("{lock}: signing.macos_team_id missing"));
        assert_eq!(id, DEVELOPER_ID_TEAM_ID, "{lock}");
    }
}

#[test]
fn release_scripts_name_the_constant() {
    for (file, at_least) in [
        ("scripts/sign-cli-mac.sh", 2),
        ("apps/desktop/scripts/release-mac.sh", 3),
        ("apps/desktop/scripts/open-design-mac-release.mjs", 2),
    ] {
        assert_all_match(file, &team_ids_in(&read(file)), at_least);
    }
}

#[test]
fn desktop_package_json_identity_names_the_constant() {
    let json: serde_json::Value = serde_json::from_str(&read("apps/desktop/package.json")).unwrap();
    let identity = json["build"]["mac"]["identity"]
        .as_str()
        .expect("build.mac.identity");
    assert_all_match("apps/desktop/package.json", &team_ids_in(identity), 1);
}

/// The extractor itself must see a wrong value (positive control for the above).
#[test]
fn extractor_detects_a_drifted_team_id() {
    let drifted = "SIGN=\"Developer ID Application: X (ABCDE12345)\"\nP=apple-notary-ABCDE12345\nexport const EXPECTED_TEAM_ID = 'ABCDE12345'";
    let ids = team_ids_in(drifted);
    assert_eq!(ids, ["ABCDE12345", "ABCDE12345", "ABCDE12345"]);
    assert!(ids.iter().all(|i| i != DEVELOPER_ID_TEAM_ID));
}
