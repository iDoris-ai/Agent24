#![allow(clippy::unwrap_used)]

use super::*;
use serde_json::{Value, json};
use std::{collections::HashMap, fs, os::unix::fs::PermissionsExt, sync::Mutex};
use tempfile::TempDir;

const KEY: &str = "vk_0123456789abcdef0123456789abcdef";
const OTHER: &str = "vk_ffffffffffffffffffffffffffffffff";

fn secret() -> String {
    format!("idk_{}", "a".repeat(64))
}

#[derive(Default)]
struct MemoryKeychain {
    entries: Mutex<HashMap<String, Zeroizing<String>>>,
    locked: bool,
}
impl Keychain for MemoryKeychain {
    fn put(&self, id: &str, value: &str) -> Result<(), KeychainError> {
        if self.locked {
            return Err(KeychainError::KeychainLocked);
        }
        self.entries
            .lock()
            .unwrap()
            .insert(id.into(), Zeroizing::new(value.into()));
        Ok(())
    }
    fn get(&self, id: &str) -> Result<Zeroizing<String>, KeychainError> {
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or(KeychainError::NotFound)
    }
    fn delete(&self, id: &str) -> Result<(), KeychainError> {
        self.entries.lock().unwrap().remove(id);
        Ok(())
    }
}

struct Fixture {
    dir: TempDir,
    keys: Keys,
    store: Arc<MemoryKeychain>,
}
impl Fixture {
    fn new(mode: &str, locked: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("idoris");
        // The fixture uses absolute tools because key commands clear the environment.
        fs::write(&binary, r##"#!/usr/bin/python3
import json, os, sys, time
from pathlib import Path
root = Path(__file__).parent
mode = (root / 'mode').read_text()
command = sys.argv[2]
with (root / 'calls').open('a') as f:
    f.write(json.dumps(sys.argv[1:]) + '\n')
if command == 'issue':
    spec = json.load(sys.stdin)
    (root / 'spec').write_text(json.dumps(spec))
    (root / 'env').write_text(json.dumps(dict(os.environ)))
    if (root / 'issue-reply').exists():
        print((root / 'issue-reply').read_text())
        sys.exit(0)
    if mode == 'timeout':
        time.sleep(30)
    if mode in ['lost', 'list-fails']:
        print('idk_' + 'a' * 64)
    elif mode == 'oversized':
        print('idk_' + 'a' * 2000000)
    elif mode == 'unknown':
        print(json.dumps({'key_id': 'vk_' + '0123456789abcdef' * 2, 'secret': 'idk_' + 'a' * 64, 'unexpected': True}))
    else:
        print(json.dumps({'key_id': 'vk_' + '0123456789abcdef' * 2, 'secret': 'idk_' + 'a' * 64}))
    if mode == 'exit-failure':
        sys.exit(1)
elif command == 'list':
    if (root / 'list-reply').exists():
        print((root / 'list-reply').read_text())
        sys.exit(0)
    if mode == 'list-fails':
        sys.exit(1)
    owner = json.loads((root / 'spec').read_text())['owner']
    def record(id, owner):
        return dict(key_id=id, owner=owner, tenant_id=None, allowed_privacy=['local_only'], allowed_roles=['daily'], expires_at_ms=None, revoked_at_ms=None, status='active')
    print(json.dumps({'keys': [record('vk_' + '0123456789abcdef' * 2, owner), record('vk_' + 'f' * 32, owner + ':other')]}))
elif command == 'revoke':
    if (root / 'revoke-reply').exists():
        print((root / 'revoke-reply').read_text())
        sys.exit(0)
    if mode == 'revoke-fails':
        print('idk_' + 'a' * 64, file=sys.stderr)
        sys.exit(1)
    print(json.dumps(dict(key_id=sys.argv[3], revoked=True)))
"##).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(dir.path().join("mode"), mode).unwrap();
        let store = Arc::new(MemoryKeychain {
            locked,
            ..Default::default()
        });
        let keys = Keys {
            binary,
            db_path: dir.path().join("idoris.sqlite3"),
            store: store.clone(),
            timeout: Duration::from_secs(5),
        };
        Self { dir, keys, store }
    }
    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.dir.path().join(name)).unwrap()
    }
    fn calls(&self) -> Vec<Vec<String>> {
        self.read("calls")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn mode(&self, mode: &str) {
        fs::write(self.dir.path().join("mode"), mode).unwrap();
    }
}

#[tokio::test]
async fn issue_stores_secret_and_defaults_to_daily() {
    let f = Fixture::new("ok", false);
    let issued = f.keys.issue("installation", &[]).await.unwrap();
    assert_eq!(issued.key_id, KEY);
    assert_eq!(*issued.secret, secret());
    assert_eq!(*f.store.get(KEY).unwrap(), secret());
    assert!(!format!("{issued:?}").contains(&secret()));
    let spec: Value = serde_json::from_str(&f.read("spec")).unwrap();
    assert_eq!(spec["allowed_roles"], json!(["daily"]));
    assert_eq!(spec["allowed_privacy"], json!(["local_only"]));
    assert!(spec.get("tenant_id").is_none());
    assert!(
        spec["owner"]
            .as_str()
            .unwrap()
            .starts_with("agent24d:installation:")
    );
    let env: Value = serde_json::from_str(&f.read("env")).unwrap();
    assert_eq!(env["IDORIS_DB_PATH"], f.keys.db_path.to_str().unwrap());
    assert!(env.get("IDORIS_OMLX_API_KEY").is_none());
    assert!(env.get("IDORIS_API_KEY").is_none());
    assert!(env.get("IDORIS_TENANTS_CONFIG").is_none());
    assert!(env.get("HOME").is_none());
}

#[tokio::test]
async fn keychain_failure_revokes_issued_key() {
    let f = Fixture::new("ok", true);
    let error = f.keys.issue("installation", &[]).await.unwrap_err();
    assert!(matches!(
        error,
        KeyError::Keychain(KeychainError::KeychainLocked)
    ));
    assert!(
        f.calls()
            .contains(&vec!["key".into(), "revoke".into(), KEY.into()])
    );
    assert!(!format!("{error:?} {error}").contains(&secret()));
}

#[tokio::test]
async fn lost_issue_result_lists_exact_owner_and_revokes_only_its_key() {
    for mode in ["lost", "unknown", "exit-failure", "oversized", "timeout"] {
        let mut f = Fixture::new(mode, false);
        if mode == "timeout" {
            // One timeout covers issue, list and revoke, and a fresh script can take
            // over a second to start under load, so keep it well above that.
            f.keys.timeout = Duration::from_secs(5);
        }
        let error = f.keys.issue("installation", &[]).await.unwrap_err();
        let calls = f.calls();
        assert!(calls.contains(&vec!["key".into(), "list".into()]), "{mode}");
        assert!(
            calls.contains(&vec!["key".into(), "revoke".into(), KEY.into()]),
            "{mode}"
        );
        assert!(!calls.iter().any(|call| call.iter().any(|arg| arg == OTHER)));
        assert!(f.store.entries.lock().unwrap().is_empty());
        assert!(!format!("{error:?} {error}").contains(&secret()));
    }
}

#[tokio::test]
async fn failed_revoke_persists_private_record_and_restart_retries_without_issuing() {
    let f = Fixture::new("revoke-fails", true);
    assert!(f.keys.issue("installation", &[]).await.is_err());
    let journal = f.keys.db_path.with_extension("key-operation.json");
    let record = fs::read_to_string(&journal).unwrap();
    assert!(record.contains(KEY));
    assert!(!record.contains("idk_"));
    assert_eq!(
        fs::metadata(&journal).unwrap().permissions().mode() & 0o777,
        0o600
    );
    f.mode("ok");
    let restarted = Keys {
        binary: f.keys.binary.clone(),
        db_path: f.keys.db_path.clone(),
        store: f.store.clone(),
        timeout: f.keys.timeout,
    };
    assert!(restarted.issue("installation", &[]).await.is_err());
    assert!(!journal.exists());
    assert_eq!(
        f.calls().iter().filter(|call| call[1] == "issue").count(),
        1
    );
    assert_eq!(
        f.calls().iter().filter(|call| call[1] == "revoke").count(),
        2
    );
    restarted.retry_pending().await.unwrap();
}

#[tokio::test]
async fn explicit_deep_and_unique_owner_but_no_other_roles() {
    let f = Fixture::new("ok", false);
    f.keys.issue("installation", &["deep"]).await.unwrap();
    let first: Value = serde_json::from_str(&f.read("spec")).unwrap();
    assert_eq!(first["allowed_roles"], json!(["daily", "deep"]));
    f.keys.issue("installation", &["daily"]).await.unwrap();
    let second: Value = serde_json::from_str(&f.read("spec")).unwrap();
    assert_ne!(first["owner"], second["owner"]);
    for role in ["*", "auto", "vision", "model-id"] {
        assert!(f.keys.issue("installation", &[role]).await.is_err());
    }
    assert_eq!(
        f.calls().iter().filter(|call| call[1] == "issue").count(),
        2
    );
}

#[tokio::test]
async fn failed_owner_reconciliation_survives_restart_without_reissuing() {
    let f = Fixture::new("list-fails", false);
    let error = f.keys.issue("installation", &[]).await.unwrap_err();
    assert!(matches!(error, KeyError::CompensationPending { .. }));
    let journal = f.keys.db_path.with_extension("key-operation.json");
    let operation: Value = serde_json::from_str(&fs::read_to_string(&journal).unwrap()).unwrap();
    assert_eq!(operation["reconcile_owner"], true);
    let spec: Value = serde_json::from_str(&f.read("spec")).unwrap();
    assert_eq!(operation["owner"], spec["owner"]);
    assert!(f.keys.issue("installation", &[]).await.is_err());
    f.mode("ok");
    f.keys.retry_pending().await.unwrap();
    assert!(!journal.exists());
    assert_eq!(
        f.calls().iter().filter(|call| call[1] == "issue").count(),
        1
    );
    assert!(
        f.calls()
            .contains(&vec!["key".into(), "revoke".into(), KEY.into()])
    );
}

#[tokio::test]
async fn cancelled_issuance_keeps_owner_for_startup_compensation() {
    let f = Fixture::new("timeout", false);
    {
        let issue = f.keys.issue("installation", &[]);
        tokio::pin!(issue);
        // Cancel only after the child has received the issuance request.
        let ready = async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !f.dir.path().join("spec").exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        };
        tokio::select! {
            result = &mut issue => panic!("issuance completed before cancellation: {result:?}"),
            () = ready => {}
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(300), issue)
                .await
                .is_err()
        );
    }
    assert!(f.dir.path().join("spec").exists());
    f.mode("ok");
    f.keys.retry_pending().await.unwrap();
    assert_eq!(
        f.calls().iter().filter(|call| call[1] == "issue").count(),
        1
    );
    assert!(
        f.calls()
            .contains(&vec!["key".into(), "revoke".into(), KEY.into()])
    );
}

#[tokio::test]
async fn corrupt_or_public_operation_record_blocks_issuance() {
    let f = Fixture::new("ok", false);
    let journal = f.keys.db_path.with_extension("key-operation.json");
    fs::write(&journal, "idk_secret_invalid_json").unwrap();
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o600)).unwrap();
    let error = f.keys.issue("installation", &[]).await.unwrap_err();
    assert!(matches!(error, KeyError::Record));
    assert!(!format!("{error:?} {error}").contains("idk_"));
    fs::write(&journal, "{}").unwrap();
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        f.keys.retry_pending().await,
        Err(KeyError::Record)
    ));
    assert!(!f.dir.path().join("calls").exists());
}

#[test]
fn operation_lock_release_does_not_wait_for_inherited_descriptors() {
    let f = Fixture::new("ok", false);
    let first = f.keys.operation_lock().unwrap();
    assert!(matches!(f.keys.operation_lock(), Err(KeyError::Busy)));
    let inherited = first.0.try_clone().unwrap();
    drop(first);
    assert!(f.keys.operation_lock().is_ok());
    drop(inherited);
}

#[tokio::test]
async fn strict_issue_schema_rejects_invalid_fields_and_token_shapes() {
    for raw in [
        json!({"key_id": KEY}),
        json!({"key_id": KEY, "secret": secret(), "extra": "idk_sensitive"}),
        json!({"key_id": KEY, "secret": 123}),
        json!({"key_id": "vk_bad", "secret": secret()}),
        json!({"key_id": KEY, "secret": "idk_bad"}),
        json!({"key_id": KEY, "secret": format!("idk_{}", "A".repeat(64))}),
    ] {
        let f = Fixture::new("ok", false);
        fs::write(f.dir.path().join("issue-reply"), raw.to_string()).unwrap();
        let error = f.keys.issue("installation", &[]).await.unwrap_err();
        assert!(!format!("{error:?} {error}").contains("idk_"));
        assert!(f.store.entries.lock().unwrap().is_empty());
        assert!(
            f.calls()
                .contains(&vec!["key".into(), "revoke".into(), KEY.into()])
        );
    }
}

#[tokio::test]
async fn list_and_revoke_reject_unrecognized_or_mismatched_responses() {
    let f = Fixture::new("ok", false);
    for raw in [
        json!({"keys": [], "secret": secret()}),
        json!({"keys": [{"key_id": KEY, "owner": "owner"}]}),
        json!({"keys": "idk_sensitive"}),
    ] {
        fs::write(f.dir.path().join("list-reply"), raw.to_string()).unwrap();
        let error = f.keys.list(None).await.unwrap_err();
        assert!(!format!("{error:?} {error}").contains("idk_"));
    }
    for raw in [
        json!({"key_id": OTHER, "revoked": true}),
        json!({"key_id": KEY, "revoked": false}),
        json!({"key_id": KEY, "revoked": true, "secret": secret()}),
    ] {
        fs::write(f.dir.path().join("revoke-reply"), raw.to_string()).unwrap();
        assert!(matches!(
            f.keys.revoke(KEY).await,
            Err(KeyError::InvalidResponse)
        ));
    }
    let before = f.calls().len();
    for id in ["vk_bad", "vk_0123456789abcdef0123456789abcdeF", "--help"] {
        assert!(matches!(
            f.keys.revoke(id).await,
            Err(KeyError::InvalidRequest)
        ));
    }
    assert_eq!(f.calls().len(), before);
}

#[tokio::test]
#[ignore = "requires the pinned real v0.4.0 binary via IDORIS_TEST_BINARY"]
async fn real_v040_issue_list_revoke() {
    let mut f = Fixture::new("ok", false);
    f.keys.binary = fs::canonicalize(std::env::var_os("IDORIS_TEST_BINARY").unwrap()).unwrap();
    // Hold the exact serve lock: real key commands must remain available.
    let instance_lock = fs::File::create(f.keys.db_path.with_extension("sqlite3.lock")).unwrap();
    fs2::FileExt::try_lock_exclusive(&instance_lock).unwrap();
    let issued = f.keys.issue("real-v040-test", &[]).await.unwrap();
    let listed = f.keys.list(None).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].key_id, issued.key_id);
    assert_eq!(listed[0].allowed_roles, vec!["daily"]);
    assert_eq!(listed[0].allowed_privacy, vec!["local_only"]);
    assert_eq!(listed[0].status, KeyStatus::Active);
    assert_eq!(*f.store.get(&issued.key_id).unwrap(), *issued.secret);
    f.keys.revoke(&issued.key_id).await.unwrap();
    assert_eq!(
        f.keys.list(None).await.unwrap()[0].status,
        KeyStatus::Revoked
    );
    f.keys.revoke(&issued.key_id).await.unwrap();
    f.store.delete(&issued.key_id).unwrap();
    assert!(f.keys.list(Some("another-owner")).await.unwrap().is_empty());
}
