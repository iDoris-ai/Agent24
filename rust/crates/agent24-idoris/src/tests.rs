#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use serde_json::json;

#[test]
fn embedded_release_and_asset_are_pinned() {
    let lock = IdorisLock::embedded();
    assert!(std::ptr::eq(lock, IdorisLock::embedded()));
    assert_eq!(lock.schema, 1);
    assert_eq!(lock.tag, "v0.4.0");
    assert_eq!(lock.commit, "20136552cac476ccf6f266a3210a1a021c717ea3");
    assert_eq!(lock.team_id, "HYAP444FN9");
    assert_eq!((lock.contract_major, lock.min_contract_minor), (1, 1));
    assert_eq!(lock.tested_contract_version, "1.1.0");
    assert_eq!(lock.assets.len(), 1);
    let asset = lock.asset_for("aarch64-apple-darwin").unwrap();
    assert_eq!(asset.name, "idoris-v0.4.0-aarch64-apple-darwin.tar.gz");
    assert_eq!(
        asset.sha256,
        "ab46e0180090ef5143b8fb6ea98b0333f2ef9c3a137bab2dc3cf7307960476d5"
    );
    assert_eq!(
        asset.files,
        [
            "idoris",
            "config/catalog.yaml",
            "config/routing-policy.yaml",
            "config/components/omlx.yaml",
            "config/components/subscription.yaml"
        ]
    );
    assert_eq!(
        lock.release_url_base,
        "https://github.com/iDoris-ai/iDoris/releases/download"
    );
    assert_eq!(
        lock.asset_url(asset),
        "https://github.com/iDoris-ai/iDoris/releases/download/v0.4.0/idoris-v0.4.0-aarch64-apple-darwin.tar.gz"
    );
    assert!(lock.asset_for("x86_64-unknown-linux-gnu").is_none());
    assert_eq!(lock.capabilities.len(), 6);
    for (capability, (id, references)) in lock.capabilities.iter().zip([
        ("L1", vec!["/594", "/591"]),
        ("L2", vec!["/590", "/594"]),
        ("L3", vec!["/589"]),
        ("L4", vec!["/591", "/594"]),
        ("L5", vec!["/586"]),
        ("L6", vec!["v0.4.0", "71b37549-168c-44e6-8b66-970a3b949121"]),
    ]) {
        assert_eq!(capability.id, id);
        for reference in references {
            assert!(capability.source.contains(reference));
        }
    }
}

#[test]
fn invalid_lock_fields_are_rejected() {
    let cases = json!({
        "/schema": [2],
        "/commit": ["2013655", "G".repeat(40), "20136552CAC476CCF6F266A3210A1A021C717EA3", "main"],
        "/tag": ["latest", "main", " v0.4.0", "v0.4.0\n", "v0.04.0", "v0.4", "v0.4.0-rc.1", "v0.4.0+build"],
        "/assets/aarch64-apple-darwin/sha256": ["a".repeat(63), "a".repeat(65), "A".repeat(64), "g".repeat(64)],
        "/team_id": ["HYAP444FN", "hYAP444FN9", "HYAP444FN!"],
        "/assets/aarch64-apple-darwin/name": ["wrong.tar.gz"],
        "/assets/aarch64-apple-darwin/files": [[], ["/idoris"], ["../idoris"], ["config/../idoris"], ["idoris", "idoris"]],
        "/tested_contract_version": ["2.1.0", "1.0.9", "garbage"],
        "/capabilities/2/id": ["L7", "L1"],
        "/release_url_base": ["http://github.com/releases/download", "https://github.com.evil.test", "https://github.com@evil.test"]
    });
    for (pointer, replacements) in cases.as_object().unwrap() {
        for replacement in replacements.as_array().unwrap() {
            let mut value: serde_json::Value = serde_json::from_str(EMBEDDED).unwrap();
            *value.pointer_mut(pointer).unwrap() = replacement.clone();
            assert!(
                IdorisLock::parse(&value.to_string()).is_err(),
                "accepted {pointer}: {value}"
            );
        }
    }
    let mut value: serde_json::Value = serde_json::from_str(EMBEDDED).unwrap();
    value["capabilities"].as_array_mut().unwrap().remove(2);
    assert!(
        IdorisLock::parse(&value.to_string()).is_err(),
        "accepted missing L3"
    );
}

#[test]
fn unknown_fields_are_rejected_at_every_level() {
    for pointer in ["", "/assets/aarch64-apple-darwin", "/capabilities/0"] {
        let mut value: serde_json::Value = serde_json::from_str(EMBEDDED).unwrap();
        value.pointer_mut(pointer).unwrap()["unknown"] = json!(true);
        assert!(
            IdorisLock::parse(&value.to_string()).is_err(),
            "accepted unknown field at {pointer}"
        );
    }
}

#[test]
fn contract_requires_the_locked_major_and_minimum_minor() {
    let lock = IdorisLock::embedded();
    for version in ["1.1.0", "1.2.5", "1.2.5+build.7"] {
        assert_eq!(lock.check_contract(version), Ok(()));
    }
    for version in [
        "2.0.0", "1.0.9", "garbage", "1.01.0", "1.1", "v1.1.0", "1.1.0 ",
    ] {
        assert_eq!(
            lock.check_contract(version),
            Err(ContractMismatch {
                expected_major: 1,
                min_minor: 1,
                actual: version.into(),
            })
        );
    }
}

#[test]
fn release_base_rejects_explicit_ports_and_wrong_path_prefixes() {
    for base in [
        "https://github.com:443/iDoris-ai/iDoris/releases/download",
        "https://github.com:444/iDoris-ai/iDoris/releases/download",
        "https://github.com/other/releases/download",
        "https://github.com/iDoris-ai/iDoris/releases/download-evil",
    ] {
        let mut value: serde_json::Value = serde_json::from_str(EMBEDDED).unwrap();
        value["release_url_base"] = json!(base);
        assert!(
            IdorisLock::parse(&value.to_string()).is_err(),
            "accepted {base}"
        );
    }
}

#[test]
fn asset_triples_reject_path_and_url_syntax() {
    for triple in [
        "",
        "../escape",
        "a/b",
        "a.b",
        "AARCH64",
        "a?b",
        "a%2fb",
        "a\\b",
    ] {
        let mut value: serde_json::Value = serde_json::from_str(EMBEDDED).unwrap();
        let mut asset = value["assets"]["aarch64-apple-darwin"].take();
        asset["name"] = json!(format!("idoris-v0.4.0-{triple}.tar.gz"));
        value["assets"] = json!({triple: asset});
        assert!(
            IdorisLock::parse(&value.to_string()).is_err(),
            "accepted {triple}"
        );
    }
}
