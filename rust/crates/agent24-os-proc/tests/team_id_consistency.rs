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

/// The values of every real (non-comment) shell assignment `NAME=...`, with the
/// `${ENV:-default}` wrapper removed. Err when there is none, so a later
/// assignment cannot hide behind (or be hidden by) the first one.
fn sh_assignments(text: &str, name: &str) -> Result<Vec<String>, String> {
    let prefix = format!("{name}=");
    let mut values = Vec::new();
    for line in text
        .lines()
        .map(str::trim_start)
        .filter(|l| !l.starts_with('#') && l.starts_with(&prefix))
    {
        let mut value = line[prefix.len()..].trim().trim_matches('"');
        if let Some(inner) = value.strip_prefix("${").and_then(|v| v.strip_suffix('}')) {
            value = inner.split_once(":-").ok_or("no `:-` default")?.1;
        }
        values.push(value.to_owned());
    }
    if values.is_empty() {
        return Err(format!("no `{name}=` assignment"));
    }
    Ok(values)
}

/// "Developer ID Application: Name (TEAM)" or "Name (TEAM)" -> the full text
/// inside the last parentheses, whatever its length (so a short value is
/// reported as itself, not skipped).
fn team_in_identity(identity: &str) -> Result<String, String> {
    let inner = identity
        .trim()
        .strip_suffix(')')
        .and_then(|s| s.rsplit_once('(').map(|(_, t)| t))
        .ok_or_else(|| format!("no trailing `(TEAM)` in {identity:?}"))?;
    Ok(inner.to_owned())
}

fn sign_identity_teams(sh: &str) -> Result<Vec<String>, String> {
    sh_assignments(sh, "SIGN_IDENTITY")?
        .iter()
        .map(|v| team_in_identity(v))
        .collect()
}

fn notary_teams(sh: &str) -> Result<Vec<String>, String> {
    sh_assignments(sh, "NOTARY_PROFILE")?
        .iter()
        .map(|v| {
            v.strip_prefix("apple-notary-")
                .map(str::to_owned)
                .ok_or_else(|| "NOTARY_PROFILE is not `apple-notary-<TEAM>`".to_owned())
        })
        .collect()
}

fn mjs_expected_team(mjs: &str) -> Result<String, String> {
    let line = mjs
        .lines()
        .map(str::trim_start)
        .find(|l| l.starts_with("export const EXPECTED_TEAM_ID"))
        .ok_or("no `export const EXPECTED_TEAM_ID`")?;
    let rest = line.split_once('=').ok_or("no `=`")?.1.trim();
    let quote = rest
        .chars()
        .next()
        .filter(|c| *c == '\'' || *c == '"')
        .ok_or("value not quoted")?;
    let body = &rest[1..];
    Ok(body[..body.find(quote).ok_or("unterminated string")?].to_owned())
}

fn assert_team(what: &str, found: Result<String, String>) {
    assert_teams(what, found.map(|t| vec![t]));
}

fn assert_teams(what: &str, found: Result<Vec<String>, String>) {
    for team in found.unwrap_or_else(|e| panic!("{what}: {e}")) {
        assert!(
            is_team_id(&team),
            "{what}: {team:?} is not a well-formed Team ID"
        );
        assert_eq!(
            team, DEVELOPER_ID_TEAM_ID,
            "{what} disagrees with DEVELOPER_ID_TEAM_ID"
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
    for file in [
        "scripts/sign-cli-mac.sh",
        "apps/desktop/scripts/release-mac.sh",
    ] {
        let sh = read(file);
        assert_teams(&format!("{file} SIGN_IDENTITY"), sign_identity_teams(&sh));
        assert_teams(&format!("{file} NOTARY_PROFILE"), notary_teams(&sh));
    }
    let file = "apps/desktop/scripts/open-design-mac-release.mjs";
    assert_team(
        &format!("{file} EXPECTED_TEAM_ID"),
        mjs_expected_team(&read(file)),
    );
}

#[test]
fn desktop_package_json_identity_names_the_constant() {
    let json: serde_json::Value = serde_json::from_str(&read("apps/desktop/package.json")).unwrap();
    let identity = json["build"]["mac"]["identity"]
        .as_str()
        .expect("build.mac.identity");
    assert_team(
        "package.json build.mac.identity",
        team_in_identity(identity),
    );
}

/// Counter-examples: every shape of misconfiguration must be rejected, and a
/// comment that still carries the right Team ID must not hide it.
#[test]
fn extractors_reject_short_empty_missing_and_commented_values() {
    let good = "SIGN_IDENTITY=\"${AGENT24_SIGN_IDENTITY:-Developer ID Application: X (HYAP444FN9)}\"\nNOTARY_PROFILE=\"${AGENT24_NOTARY_PROFILE:-apple-notary-HYAP444FN9}\"\n";
    assert_eq!(sign_identity_teams(good).unwrap(), [DEVELOPER_ID_TEAM_ID]);
    assert_eq!(notary_teams(good).unwrap(), [DEVELOPER_ID_TEAM_ID]);
    // True when at least one extracted value is not exactly the constant.
    let bad_value = |f: fn(&str) -> Result<Vec<String>, String>, text: &str| {
        f(text)
            .unwrap_or_default()
            .iter()
            .any(|t| !is_team_id(t) || t != DEVELOPER_ID_TEAM_ID)
    };
    // short value (the Codex finding: OTHERTEAM is 9 chars)
    assert!(bad_value(
        sign_identity_teams,
        &good.replace("HYAP444FN9)}", "OTHERTEAM)}")
    ));
    assert!(bad_value(
        notary_teams,
        &good.replace("notary-HYAP444FN9", "notary-OTHERTEAM")
    ));
    // empty value
    assert!(bad_value(
        sign_identity_teams,
        &good.replace("(HYAP444FN9)", "()")
    ));
    assert!(bad_value(
        notary_teams,
        &good.replace("notary-HYAP444FN9", "notary-")
    ));
    // a second assignment for another team must not hide behind the first
    assert!(bad_value(
        sign_identity_teams,
        &format!("{good}SIGN_IDENTITY=\"Developer ID Application: Y (ABCDE12345)\"\n")
    ));
    assert!(bad_value(
        notary_teams,
        &format!("{good}NOTARY_PROFILE=apple-notary-ABCDE12345\n")
    ));
    // assignment removed, correct Team ID left only in a comment
    let commented = "# SIGN_IDENTITY=\"x (HYAP444FN9)\"\n# NOTARY_PROFILE=apple-notary-HYAP444FN9\n# default (HYAP444FN9)\n";
    assert!(sign_identity_teams(commented).is_err());
    assert!(notary_teams(commented).is_err());
    // mjs
    let mjs = "// EXPECTED_TEAM_ID = 'HYAP444FN9'\nexport const EXPECTED_TEAM_ID = 'HYAP444FN9'\n";
    assert_eq!(mjs_expected_team(mjs).unwrap(), DEVELOPER_ID_TEAM_ID);
    assert_ne!(
        mjs_expected_team("export const EXPECTED_TEAM_ID = 'OTHERTEAM'").unwrap(),
        DEVELOPER_ID_TEAM_ID
    );
    assert_eq!(
        mjs_expected_team("export const EXPECTED_TEAM_ID = ''").unwrap(),
        ""
    );
    assert!(
        mjs_expected_team("// export const EXPECTED_TEAM_ID = 'HYAP444FN9'\nconst a = 1").is_err()
    );
    // package.json identity
    assert_ne!(
        team_in_identity("Name (OTHERTEAM)").unwrap(),
        DEVELOPER_ID_TEAM_ID
    );
    assert!(team_in_identity("Name").is_err());
}
