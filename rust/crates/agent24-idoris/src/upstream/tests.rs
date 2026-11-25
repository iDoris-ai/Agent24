#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    time::Duration,
};

const SECRET: &str = "sk-test-omlx-never-print-8372";

#[test]
fn endpoint_input_rejects_non_loopback_and_url_components() {
    for input in [
        "",
        "8088",
        "ftp://127.0.0.1:8088",
        "http://localhost:8088",
        "http://example.com",
        "http://0.0.0.0:8088",
        "http://192.168.1.1:8088",
        "http://[::]:8088",
        "http://[::ffff:127.0.0.1]:8088",
        "http://127.0.0.1:0",
        "http://u:p@127.0.0.1:8088",
        "http://@127.0.0.1:8088",
        "http://127.0.0.1:8088/v1",
        "http://127.0.0.1:8088/..",
        "http://127.0.0.1:8088/%2e",
        "http://127.0.0.1:8088?",
        "http://127.0.0.1:8088/#",
        " http://127.0.0.1:8088",
        "http://127.0.0.1:8088\n",
        "http://127.0.0.1:8088\\v1",
        "http://127.0.0.1:65536",
        "http://127.0.0.1:bad",
    ] {
        assert!(Endpoint::parse(input).is_err(), "accepted {input:?}");
    }
}

#[test]
fn ipv6_loopback_and_http_https_origins_are_allowed() {
    for input in [
        "http://[::1]:8088",
        "https://127.0.0.1:8000/",
        "http://127.0.0.2:8001",
    ] {
        assert!(Endpoint::parse(input).is_ok());
    }
}

struct Settings {
    dir: tempfile::TempDir,
    path: std::path::PathBuf,
}
impl Settings {
    fn new(bytes: &[u8]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        Self { dir, path }
    }
    fn read(&self) -> Result<ApiKey, SettingsError> {
        read_settings_key_at(&self.path, rustix::process::geteuid().as_raw())
    }
}
fn settings() -> Settings {
    Settings::new(
        format!(r#"{{"auth":{{"api_key":" {SECRET} "}},"unrelated":{{"ignored":true}}}}"#)
            .as_bytes(),
    )
}

#[test]
fn settings_reads_only_auth_api_key_and_redacts_debug() {
    let fixture = settings();
    let value = fixture.read().unwrap();
    assert_eq!(value.0.as_str(), SECRET);
    assert_eq!(format!("{value:?}"), "ApiKey([REDACTED])");
}

#[test]
fn settings_missing_file_is_distinct() {
    let fixture = settings();
    fs::remove_file(&fixture.path).unwrap();
    assert_eq!(fixture.read().unwrap_err(), SettingsError::NotFound);
}

#[test]
fn settings_symlink_file_and_parent_are_rejected() {
    let fixture = settings();
    let link = fixture.dir.path().join("link.json");
    symlink(&fixture.path, &link).unwrap();
    assert_eq!(
        read_settings_key_at(&link, rustix::process::geteuid().as_raw()).unwrap_err(),
        SettingsError::UnsafeFile
    );
    let other = tempfile::tempdir().unwrap();
    let parent = other.path().join("linked-dir");
    symlink(fixture.dir.path(), &parent).unwrap();
    assert_eq!(
        read_settings_key_at(
            &parent.join("settings.json"),
            rustix::process::geteuid().as_raw()
        )
        .unwrap_err(),
        SettingsError::UnsafeFile
    );
}

#[test]
fn settings_different_owner_is_rejected_with_simulated_euid() {
    let fixture = settings();
    assert_eq!(
        read_settings_key_at(&fixture.path, rustix::process::geteuid().as_raw() ^ 1).unwrap_err(),
        SettingsError::UnsafeFile
    );
}

#[test]
fn settings_group_or_other_writable_file_and_parent_are_rejected() {
    for mode in [0o620, 0o602] {
        let fixture = settings();
        fs::set_permissions(&fixture.path, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(fixture.read().unwrap_err(), SettingsError::UnsafeFile);
    }
    let fixture = settings();
    fs::set_permissions(fixture.dir.path(), fs::Permissions::from_mode(0o720)).unwrap();
    assert_eq!(fixture.read().unwrap_err(), SettingsError::UnsafeFile);
}

#[test]
fn settings_oversize_bad_json_and_missing_key_are_distinct() {
    for (bytes, expected) in [
        (vec![b' '; 65537], SettingsError::TooLarge),
        (
            format!(r#"{{"auth":{{"api_key":"{SECRET}"}},oops}}"#).into_bytes(),
            SettingsError::InvalidJson,
        ),
        (b"{}".to_vec(), SettingsError::MissingKey),
        (
            br#"{"auth":{"other":"value"}}"#.to_vec(),
            SettingsError::MissingKey,
        ),
    ] {
        let fixture = Settings::new(&bytes);
        let error = fixture.read().unwrap_err();
        assert_eq!(error, expected);
        assert!(!format!("{error:?} {error}").contains(SECRET));
    }
}

#[test]
fn settings_invalid_key_and_non_regular_file_are_rejected() {
    for value in [
        serde_json::json!(42),
        serde_json::json!(""),
        serde_json::json!("\n"),
        serde_json::json!("abc\nsecret"),
        serde_json::json!("a".repeat(513)),
    ] {
        let fixture = Settings::new(
            serde_json::json!({"auth":{"api_key":value}})
                .to_string()
                .as_bytes(),
        );
        assert_eq!(fixture.read().unwrap_err(), SettingsError::InvalidKey);
    }
    let fixture = settings();
    fs::remove_file(&fixture.path).unwrap();
    fs::create_dir(&fixture.path).unwrap();
    assert_eq!(fixture.read().unwrap_err(), SettingsError::UnsafeFile);
}

#[test]
fn settings_fifo_is_rejected_without_blocking() {
    let fixture = settings();
    fs::remove_file(&fixture.path).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fixture.path)
            .status()
            .unwrap()
            .success()
    );
    let start = std::time::Instant::now();
    assert_eq!(fixture.read().unwrap_err(), SettingsError::UnsafeFile);
    assert!(start.elapsed() < Duration::from_millis(500));
}
