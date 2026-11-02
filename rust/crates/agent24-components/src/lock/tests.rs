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

const V1: &str = r#"{
  "schema": 1, "tag": "v0.4.0", "commit": "20136552cac476ccf6f266a3210a1a021c717ea3",
  "assets": { "aarch64-apple-darwin": { "name": "idoris-v0.4.0-aarch64-apple-darwin.tar.gz",
    "sha256": "ab46e0180090ef5143b8fb6ea98b0333f2ef9c3a137bab2dc3cf7307960476d5", "files": ["idoris"] } },
  "team_id": "HYAP444FN9", "contract_major": 1, "min_contract_minor": 1,
  "tested_contract_version": "1.1.0",
  "capabilities": [ { "id": "L1", "source": "x" } ],
  "release_url_base": "https://github.com/iDoris-ai/iDoris/releases/download"
}"#;

#[test]
fn schema1_is_readable_only_through_the_migration_helper() {
    assert!(matches!(
        ComponentLock::parse(V1),
        Err(LockError::Schema(1))
    ));
    let lock = ComponentLock::migrate_schema1(
        V1,
        "idoris",
        "idoris-http",
        vec![],
        Requires::default(),
        &["idoris"],
    )
    .unwrap();
    assert_eq!(lock.schema, 2);
    assert_eq!(lock.version, "v0.4.0");
    assert_eq!(lock.assets["darwin-arm64"].executables, ["idoris"]);
    assert_eq!(lock.assurance[0].id, "L1");
    assert_eq!(lock.signing.unwrap().macos_team_id, "HYAP444FN9");
    // A schema-2 document is not a schema-1 document.
    assert!(
        ComponentLock::migrate_schema1(TEST_LOCK, "demo", "x", vec![], Requires::default(), &[])
            .is_err()
    );
}
