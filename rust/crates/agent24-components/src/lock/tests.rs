#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use serde_json::{Value, json};

fn base() -> Value {
    serde_json::from_str(TEST_LOCK).unwrap()
}

fn parse(value: &Value) -> Result<ComponentLock, LockError> {
    ComponentLock::parse(&value.to_string())
}

fn set(pointer: &str, to: Value) -> Value {
    let mut value = base();
    *value.pointer_mut(pointer).unwrap() = to;
    value
}

#[test]
fn valid_lock_parses_and_resolves_urls() {
    let lock = parse(&base()).unwrap();
    let asset = lock.asset_for("darwin-arm64").unwrap();
    assert_eq!(
        lock.asset_url(asset),
        "https://github.com/example/demo/releases/download/v1.2.3/demo-v1.2.3-darwin-arm64.tar.gz"
    );
    assert!(lock.asset_for("linux-x64").is_none());
    assert!(lock.accepts_contract("1.1.0") && lock.accepts_contract("1.9.0"));
    assert!(!lock.accepts_contract("1.0.9") && !lock.accepts_contract("2.1.0"));
    assert!(!lock.accepts_contract("junk"));
}

#[test]
fn rejects_unknown_fields_everywhere() {
    for pointer in [
        "",
        "/source",
        "/contract",
        "/assets/darwin-arm64",
        "/signing",
        "/requires",
        "/provides/0",
    ] {
        let mut value = base();
        value
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("surprise".into(), json!(1));
        assert!(
            matches!(parse(&value), Err(LockError::Json(_))),
            "unknown field accepted at {pointer:?}"
        );
    }
}

#[test]
fn rejects_wrong_schema_versions() {
    assert!(matches!(
        parse(&set("/schema", json!(1))),
        Err(LockError::Schema(1))
    ));
    assert!(matches!(
        parse(&set("/schema", json!(3))),
        Err(LockError::Schema(3))
    ));
}

#[test]
fn rejects_invalid_values() {
    let cases = [
        ("/id", json!("Demo")),
        ("/id", json!("")),
        ("/version", json!("1.2.3")),
        ("/version", json!("v1.2.3-rc1")),
        ("/source/commit", json!("abc")),
        ("/source/repo", json!("https://example.com/x")),
        ("/contract/tested", json!("2.0.0")),
        ("/contract/tested", json!("1.0.0")),
        ("/assets/darwin-arm64/sha256", json!("A".repeat(64))),
        ("/assets/darwin-arm64/sha256", json!("ab")),
        ("/assets/darwin-arm64/name", json!("../x.tar.gz")),
        ("/assets/darwin-arm64/files", json!([])),
        ("/assets/darwin-arm64/files", json!(["demo", "../x"])),
        ("/assets/darwin-arm64/files", json!(["demo", "demo"])),
        ("/assets/darwin-arm64/executables", json!([])),
        ("/assets/darwin-arm64/executables", json!(["other"])),
        ("/assets/darwin-arm64/executables", json!(["demo", "demo"])),
        ("/signing/macos_team_id", json!("hyap444fn9")),
        ("/signing/macos_team_id", json!("SHORT")),
        (
            "/release_url_base",
            json!("http://github.com/example/demo/releases/download"),
        ),
        (
            "/release_url_base",
            json!("https://github.com:444/example/demo/releases/download"),
        ),
        (
            "/release_url_base",
            json!("https://github.com/other/demo/releases/download"),
        ),
        (
            "/release_url_base",
            json!("https://evil.invalid/example/demo/releases/download"),
        ),
        ("/assets/darwin-arm64/files", json!(["demo", "a\nb"])),
        ("/assets/darwin-arm64/files", json!(["demo", "a\u{7f}b"])),
        ("/provides/0/id", json!("X")),
        ("/provides/0/id", json!("a")),
    ];
    for (pointer, to) in cases {
        assert!(
            matches!(parse(&set(pointer, to.clone())), Err(LockError::Invalid(_))),
            "{pointer} = {to} accepted"
        );
    }
}

#[test]
fn macos_assets_require_signing_and_linux_do_not() {
    let mut value = base();
    value.as_object_mut().unwrap().remove("signing");
    assert!(matches!(parse(&value), Err(LockError::Invalid("signing"))));
    let asset = value["assets"]["darwin-arm64"].take();
    value["assets"] = json!({ "linux-x64": asset });
    parse(&value).unwrap();
}

#[test]
fn executables_field_is_required() {
    let mut value = base();
    value["assets"]["darwin-arm64"]
        .as_object_mut()
        .unwrap()
        .remove("executables");
    assert!(matches!(parse(&value), Err(LockError::Json(_))));
}

#[test]
fn duplicate_capability_ids_are_rejected() {
    let mut value = base();
    let first = value["provides"][0].clone();
    value["provides"].as_array_mut().unwrap().push(first);
    assert!(matches!(
        parse(&value),
        Err(LockError::Invalid("provides.id"))
    ));
}

#[test]
fn reproducible_digests_are_validated_and_kept_separate() {
    let mut value = base();
    value["reproducible"] = json!({
        "toolchain": "go1.26.4", "recipe": "go build",
        "unsigned_sha256": { "darwin-arm64": "c".repeat(64) }
    });
    let lock = parse(&value).unwrap();
    let unsigned = &lock.reproducible.as_ref().unwrap().unsigned_sha256["darwin-arm64"];
    assert_ne!(unsigned, &lock.assets["darwin-arm64"].sha256);
    value["reproducible"]["unsigned_sha256"] = json!({ "darwin-arm64": "zz" });
    assert!(parse(&value).is_err());
    value["reproducible"]["unsigned_sha256"] = json!({ "linux-x64": "c".repeat(64) });
    assert!(parse(&value).is_err());
}

#[test]
fn platform_keys() {
    assert_eq!(platform_key("macos", "aarch64").unwrap(), "darwin-arm64");
    assert_eq!(platform_key("linux", "x86_64").unwrap(), "linux-x64");
    assert!(platform_key("windows", "x86_64").is_none());
    assert_eq!(
        platform_key_from_triple("aarch64-apple-darwin").unwrap(),
        "darwin-arm64"
    );
    assert_eq!(
        platform_key_from_triple("x86_64-unknown-linux-gnu").unwrap(),
        "linux-x64"
    );
    assert!(platform_key_from_triple("garbage").is_none());
}

#[test]
fn cli_major_minor_contract_versions_and_id_only_provides_parse() {
    // Hyphae's published lock: `tested` is `major.minor`, provides carry ids only.
    let mut value = set(
        "/contract",
        json!({ "name": "hyphae-cli", "major": 1, "min_minor": 0, "tested": "1.0" }),
    );
    value["provides"] = json!([{ "id": "nostr-dm" }, { "id": "nostr-relay" }]);
    let lock = parse(&value).unwrap();
    assert!(
        lock.provides
            .iter()
            .all(|p| p.name.is_none() && p.summary.is_none())
    );
    assert!(lock.accepts_contract("1.0") && lock.accepts_contract("1.3"));
    assert!(lock.accepts_contract("1.3.0"));
    for bad in [
        "2.0", "0.9", "1", "1.0.0.0", "1.x", "01.0", "1.0-rc1", "+1.0", "1.0 ",
    ] {
        assert!(!lock.accepts_contract(bad), "{bad}");
    }
    let value = set("/contract/tested", json!("2.0"));
    assert!(matches!(
        parse(&value),
        Err(LockError::Invalid("contract.tested"))
    ));
}

#[test]
fn health_failures_before_degraded_is_optional_and_typed() {
    assert_eq!(
        parse(&base()).unwrap().health_failures_before_degraded,
        None
    );
    let mut value = base();
    value["health_failures_before_degraded"] = json!(5);
    assert_eq!(
        parse(&value).unwrap().health_failures_before_degraded,
        Some(5)
    );
    value["health_failures_before_degraded"] = json!("five");
    assert!(parse(&value).is_err());
    value["health_failures_before_degraded"] = json!(-1);
    assert!(parse(&value).is_err());
}
