//! COMM-1b: `PasswordStore` — where the Hyphae keystore password lives
//! between agent24d restarts (COMM-HYPHAE.md §6.4).
//!
//! Two implementations:
//! - [`KeyringPasswordStore`]: the OS credential store (macOS Keychain,
//!   Linux Secret Service). This is the only implementation agent24d itself
//!   should ever construct in production.
//! - [`MemoryPasswordStore`]: a plain in-process map, for tests and for the
//!   explicitly-degraded path a caller may choose when a keychain is known
//!   to be unavailable. It is never selected automatically — COMM-HYPHAE.md
//!   §6.4 is explicit that "降级不会自动发生" (degradation never happens on
//!   its own).
//!
//! Both implementations store the password as base64url (no padding) text,
//! never raw bytes: the OS credential APIs this crate's `keyring` backends
//! expose are string-typed (`set_password`/`get_password`), and a uniform
//! text encoding means a password that happens to contain non-UTF-8 bytes
//! (e.g. an imported keystore's hand-typed password, COMM-HYPHAE.md §3's
//! "导入或手输的口令原样保存为 UTF-8 字节") round-trips identically through
//! either backend.

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use tokio::sync::Mutex;
use zeroize::Zeroizing;

use crate::password::Password;

/// `service` used for every keyring entry this crate creates
/// (COMM-HYPHAE.md §2: `service="ai.idoris.agent24.comm"`).
pub const KEYRING_SERVICE: &str = "ai.idoris.agent24.comm";

/// The keyring *account* name for a given keystore.
///
/// Before a keystore has a salt (no identity has been created yet) there is
/// nothing stable to key the account on, so the first identity's password is
/// filed under a locally-unique [`Account::Pending`] placeholder and renamed
/// to [`Account::Salt`] once `identity create` returns the new salt
/// (COMM-HYPHAE.md §6.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Account {
    /// A short-lived placeholder used only between generating a password for
    /// the very first identity and learning that identity's keystore salt.
    Pending(String),
    /// `hex(sha256(keystore.salt))` — stable for the lifetime of a keystore
    /// (COMM-HYPHAE.md §2, §6.4).
    Salt(String),
}

impl Account {
    /// `Salt(hex(sha256(salt_b64)))`. `salt_b64` is `keystore.json`'s `salt`
    /// field taken verbatim (Hyphae stores it as a standard-base64 string,
    /// `internal/identity/keystore.go`'s `createVerification`); this hashes
    /// that field's own text, not bytes decoded out of it — `keystore.salt`
    /// names the field's string value, and hashing it directly (rather than
    /// first base64-decoding it) needs no assumption about Hyphae's salt
    /// encoding ever staying base64.
    pub fn from_salt(salt_b64: &str) -> Self {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(salt_b64.as_bytes());
        Self::Salt(hex::encode(hasher.finalize()))
    }

    /// A fresh, locally-unique `Pending` account. Not a cryptographic
    /// secret — it is only ever used as a keyring *account name*, visible to
    /// anyone who can already read the service's entries — so plain
    /// process-local randomness (no new dependency) is enough to make
    /// concurrent "create the first identity" attempts not collide.
    pub fn new_pending() -> Self {
        use std::hash::{BuildHasher, Hasher};
        // Two independently-seeded `RandomState`s give 128 bits of
        // OS-seeded randomness without pulling in a `uuid`/`rand` crate.
        let a = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        let b = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        Self::Pending(format!("pending-{a:016x}{b:016x}"))
    }

    /// The literal string used as the keyring/in-memory-map account key.
    fn as_key(&self) -> &str {
        match self {
            Account::Pending(s) | Account::Salt(s) => s,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The backing store itself couldn't be reached or used — e.g. no
    /// Keychain/Secret Service session, a platform error, or (in the
    /// in-memory store) a worker-thread panic. Callers map this to
    /// `locked{keychain_unavailable}` (COMM-HYPHAE.md §6.4).
    #[error("password store unavailable: {0}")]
    Unavailable(String),
    /// The store is reachable, but has no entry for this account.
    #[error("no password stored for this account")]
    NotFound,
}

/// Where the Hyphae keystore password lives between agent24d restarts.
#[async_trait::async_trait]
pub trait PasswordStore: Send + Sync {
    async fn get(&self, account: &Account) -> Result<Password, StoreError>;
    /// Replaces any existing entry for `account` (COMM-HYPHAE.md: "「remember」
    /// 要能替换掉已有的条目").
    async fn put(&self, account: &Account, password: &Password) -> Result<(), StoreError>;
    /// Registers a password that has already been verified by Hyphae.
    /// The return value reports whether it is durable across daemon restarts.
    /// Stores that only keep process-local state deliberately report false.
    async fn register_verified(
        &self,
        _account: &Account,
        _password: &Password,
        _remember: bool,
    ) -> Result<bool, StoreError> {
        Err(StoreError::Unavailable(
            "verified password registration is unsupported".to_owned(),
        ))
    }
    /// `get(from)` → `put(to, _)` → `delete(from)`. If `get` or `put` fails,
    /// nothing has moved: `from`'s entry (if any) is untouched. If the
    /// trailing `delete` fails, `to` has *already* been written correctly —
    /// the stray `from` entry is a harmless leftover (COMM-HYPHAE.md §6.4:
    /// "agent24d 启动时，残留的 Pending 条目只记日志、不使用"), not a
    /// correctness problem, and the error is still surfaced so a caller can
    /// retry the cleanup.
    async fn rename(&self, from: &Account, to: &Account) -> Result<(), StoreError> {
        let password = self.get(from).await?;
        self.put(to, &password).await?;
        self.delete(from).await
    }
    async fn delete(&self, account: &Account) -> Result<(), StoreError>;
}

fn encode_password(password: &Password) -> Zeroizing<String> {
    Zeroizing::new(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(password.as_bytes()))
}

fn decode_password(encoded: &str) -> Result<Password, StoreError> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded.trim())
        .map_err(|e| StoreError::Unavailable(format!("stored password is not base64url: {e}")))?;
    Password::new(bytes).map_err(|e| StoreError::Unavailable(format!("stored password: {e}")))
}

// ---------------------------------------------------------------------
// MemoryPasswordStore
// ---------------------------------------------------------------------

/// A plain in-process `PasswordStore`. Used by tests, and by callers that
/// have explicitly decided to run without a keychain (COMM-HYPHAE.md §6.4:
/// this never happens automatically — nothing in this crate selects it on
/// `KeyringPasswordStore`'s behalf).
#[derive(Default)]
pub struct MemoryPasswordStore {
    entries: Mutex<HashMap<String, Zeroizing<String>>>,
}

impl MemoryPasswordStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test-only: a sorted snapshot of every account key currently stored.
    /// Used by `router.rs`'s `KeystoreWriteLock` coverage test to assert no
    /// `Pending` account survives a promoted first identity (PR #622
    /// Medium).
    #[cfg(test)]
    pub(crate) async fn snapshot_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.entries.lock().await.keys().cloned().collect();
        keys.sort();
        keys
    }
}

#[async_trait::async_trait]
impl PasswordStore for MemoryPasswordStore {
    async fn get(&self, account: &Account) -> Result<Password, StoreError> {
        let entries = self.entries.lock().await;
        let encoded = entries.get(account.as_key()).ok_or(StoreError::NotFound)?;
        decode_password(encoded)
    }

    async fn put(&self, account: &Account, password: &Password) -> Result<(), StoreError> {
        let encoded = encode_password(password);
        self.entries
            .lock()
            .await
            .insert(account.as_key().to_string(), encoded);
        Ok(())
    }

    async fn register_verified(
        &self,
        account: &Account,
        password: &Password,
        _remember: bool,
    ) -> Result<bool, StoreError> {
        self.put(account, password).await?;
        // A MemoryPasswordStore never claims durable persistence, even when
        // callers pass --remember.
        Ok(false)
    }

    async fn delete(&self, account: &Account) -> Result<(), StoreError> {
        let mut entries = self.entries.lock().await;
        match entries.remove(account.as_key()) {
            Some(_) => Ok(()),
            None => Err(StoreError::NotFound),
        }
    }
}

// ---------------------------------------------------------------------
// KeyringPasswordStore
// ---------------------------------------------------------------------

/// The OS credential store: macOS Keychain, or the Linux Secret Service.
/// Every call runs on a blocking-pool thread (`tokio::task::spawn_blocking`)
/// since the `keyring` crate's `Entry` API is synchronous regardless of
/// backend.
pub struct KeyringPasswordStore {
    session: Mutex<HashMap<String, Zeroizing<String>>>,
    backend: Arc<dyn CredentialBackend>,
}

impl KeyringPasswordStore {
    pub fn new() -> Self {
        Self::with_backend(Arc::new(OsCredentialBackend))
    }

    fn with_backend(backend: Arc<dyn CredentialBackend>) -> Self {
        Self {
            session: Mutex::new(HashMap::new()),
            backend,
        }
    }
}

impl std::fmt::Debug for KeyringPasswordStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyringPasswordStore")
            .finish_non_exhaustive()
    }
}

impl Default for KeyringPasswordStore {
    fn default() -> Self {
        Self::new()
    }
}

trait CredentialBackend: Send + Sync {
    fn get(&self, account: &str) -> Result<Zeroizing<String>, StoreError>;
    fn set(&self, account: &str, encoded: &str) -> Result<(), StoreError>;
    fn delete(&self, account: &str) -> Result<(), StoreError>;
}

struct OsCredentialBackend;

impl CredentialBackend for OsCredentialBackend {
    fn get(&self, account: &str) -> Result<Zeroizing<String>, StoreError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, account).map_err(map_keyring_error)?;
        entry
            .get_password()
            .map(Zeroizing::new)
            .map_err(map_keyring_error)
    }

    fn set(&self, account: &str, encoded: &str) -> Result<(), StoreError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, account).map_err(map_keyring_error)?;
        entry.set_password(encoded).map_err(map_keyring_error)
    }

    fn delete(&self, account: &str) -> Result<(), StoreError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, account).map_err(map_keyring_error)?;
        entry.delete_credential().map_err(map_keyring_error)
    }
}

/// Everything except "no entry" collapses to `Unavailable`: a platform
/// failure, no Secret Service session, an ambiguous match, etc. are all
/// states where the caller should treat the keychain as unreachable
/// (COMM-HYPHAE.md §6.4's `locked{keychain_unavailable}`), not states worth
/// distinguishing further here.
fn map_keyring_error(err: keyring::Error) -> StoreError {
    match err {
        keyring::Error::NoEntry => StoreError::NotFound,
        other => StoreError::Unavailable(format!("keychain_unavailable: {other}")),
    }
}

/// Runs a blocking keyring operation on the blocking pool and flattens a
/// worker-thread panic into [`StoreError::Unavailable`] rather than
/// propagating a `JoinError`.
async fn run_blocking<F, T>(f: F) -> Result<T, StoreError>
where
    F: FnOnce() -> Result<T, StoreError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(e) => Err(StoreError::Unavailable(format!(
            "keyring worker thread panicked: {e}"
        ))),
    }
}

#[async_trait::async_trait]
impl PasswordStore for KeyringPasswordStore {
    async fn get(&self, account: &Account) -> Result<Password, StoreError> {
        let key = account.as_key().to_string();
        let session = self.session.lock().await;
        if let Some(encoded) = session.get(&key) {
            return decode_password(encoded);
        }
        let backend = Arc::clone(&self.backend);
        let result = run_blocking(move || {
            let encoded = backend.get(&key)?;
            decode_password(&encoded)
        })
        .await;
        drop(session);
        result
    }

    async fn put(&self, account: &Account, password: &Password) -> Result<(), StoreError> {
        let key = account.as_key().to_string();
        let encoded = encode_password(password);
        let backend = Arc::clone(&self.backend);
        let backend_key = key.clone();
        run_blocking(move || backend.set(&backend_key, &encoded)).await?;
        self.session.lock().await.remove(&key);
        Ok(())
    }

    async fn register_verified(
        &self,
        account: &Account,
        password: &Password,
        remember: bool,
    ) -> Result<bool, StoreError> {
        let key = account.as_key().to_string();
        let encoded = encode_password(password);
        if remember {
            // Serialize against session reads/writes. Preserve the old
            // session credential until durable storage succeeds; a failed
            // explicit remember must not replace it with the new input.
            let mut session = self.session.lock().await;
            let backend = Arc::clone(&self.backend);
            run_blocking({
                let key = key.clone();
                move || backend.set(&key, &encoded)
            })
            .await?;
            session.remove(&key);
            Ok(true)
        } else {
            self.session.lock().await.insert(key, encoded);
            Ok(false)
        }
    }

    async fn delete(&self, account: &Account) -> Result<(), StoreError> {
        let key = account.as_key().to_string();
        let mut session = self.session.lock().await;
        let had_session = session.remove(&key).is_some();
        let backend = Arc::clone(&self.backend);
        let result = run_blocking({
            let key = key.clone();
            move || backend.delete(&key)
        })
        .await;
        match result {
            Ok(()) => Ok(()),
            Err(StoreError::NotFound) if had_session => Ok(()),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn pw(s: &str) -> Password {
        Password::new(s.as_bytes().to_vec()).unwrap()
    }

    #[derive(Default)]
    struct PutOnlyStore {
        puts: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl PasswordStore for PutOnlyStore {
        async fn get(&self, _account: &Account) -> Result<Password, StoreError> {
            Err(StoreError::NotFound)
        }

        async fn put(&self, _account: &Account, _password: &Password) -> Result<(), StoreError> {
            self.puts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn delete(&self, _account: &Account) -> Result<(), StoreError> {
            Err(StoreError::NotFound)
        }
    }

    #[tokio::test]
    async fn default_verified_registration_fails_closed_without_put() {
        let store = PutOnlyStore::default();
        let result = store
            .register_verified(&Account::from_salt("salt"), &pw("verified"), false)
            .await;
        assert!(matches!(result, Err(StoreError::Unavailable(_))));
        assert_eq!(store.puts.load(Ordering::SeqCst), 0);
    }

    #[derive(Default)]
    struct TestCredentialBackend {
        entries: std::sync::Mutex<HashMap<String, String>>,
        writes: AtomicUsize,
        fail_set: AtomicBool,
        fail_delete: AtomicBool,
    }

    impl CredentialBackend for TestCredentialBackend {
        fn get(&self, account: &str) -> Result<Zeroizing<String>, StoreError> {
            self.entries
                .lock()
                .unwrap()
                .get(account)
                .cloned()
                .map(Zeroizing::new)
                .ok_or(StoreError::NotFound)
        }

        fn set(&self, account: &str, encoded: &str) -> Result<(), StoreError> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            if self.fail_set.load(Ordering::SeqCst) {
                return Err(StoreError::Unavailable("injected set failure".to_owned()));
            }
            self.entries
                .lock()
                .unwrap()
                .insert(account.to_owned(), encoded.to_owned());
            Ok(())
        }

        fn delete(&self, account: &str) -> Result<(), StoreError> {
            if self.fail_delete.load(Ordering::SeqCst) {
                return Err(StoreError::Unavailable(
                    "injected delete failure".to_owned(),
                ));
            }
            self.entries
                .lock()
                .unwrap()
                .remove(account)
                .map(drop)
                .ok_or(StoreError::NotFound)
        }
    }

    // ---- Account --------------------------------------------------------

    #[test]
    fn from_salt_is_deterministic_and_hex() {
        let a = Account::from_salt("c2FtZS1zYWx0");
        let b = Account::from_salt("c2FtZS1zYWx0");
        assert_eq!(a, b);
        let Account::Salt(hex) = a else {
            panic!("expected Salt variant")
        };
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn from_salt_differs_for_different_salts() {
        let a = Account::from_salt("salt-one");
        let b = Account::from_salt("salt-two");
        assert_ne!(a, b);
    }

    #[test]
    fn new_pending_values_are_distinct() {
        let a = Account::new_pending();
        let b = Account::new_pending();
        assert_ne!(a, b);
        assert!(matches!(a, Account::Pending(_)));
    }

    // ---- MemoryPasswordStore ---------------------------------------------

    #[tokio::test]
    async fn memory_store_roundtrips_password_bytes() {
        let store = MemoryPasswordStore::new();
        let account = Account::new_pending();
        store.put(&account, &pw("hello world")).await.unwrap();
        let got = store.get(&account).await.unwrap();
        assert_eq!(got.as_bytes(), b"hello world");
    }

    #[tokio::test]
    async fn memory_store_roundtrips_non_utf8_bytes() {
        // "导入或手输的口令原样保存为 UTF-8 字节" — but the store itself must
        // not assume valid UTF-8 either; base64url makes that moot.
        let store = MemoryPasswordStore::new();
        let account = Account::new_pending();
        let bytes = vec![0xff_u8, 0x00, 0xfe, 0x7f, 0x80];
        store
            .put(&account, &Password::new(bytes.clone()).unwrap())
            .await
            .unwrap();
        let got = store.get(&account).await.unwrap();
        assert_eq!(got.as_bytes(), bytes.as_slice());
    }

    #[tokio::test]
    async fn memory_store_get_missing_is_not_found() {
        let store = MemoryPasswordStore::new();
        let err = store.get(&Account::new_pending()).await.unwrap_err();
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn memory_store_delete_missing_is_not_found() {
        let store = MemoryPasswordStore::new();
        let err = store.delete(&Account::new_pending()).await.unwrap_err();
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn memory_store_put_replaces_existing_entry() {
        let store = MemoryPasswordStore::new();
        let account = Account::new_pending();
        store.put(&account, &pw("first")).await.unwrap();
        store.put(&account, &pw("second")).await.unwrap();
        let got = store.get(&account).await.unwrap();
        assert_eq!(got.as_bytes(), b"second");
    }

    #[tokio::test]
    async fn memory_store_register_verified_never_claims_remembered() {
        let store = MemoryPasswordStore::new();
        let account = Account::from_salt("salt");
        assert!(
            !store
                .register_verified(&account, &pw("verified"), true)
                .await
                .unwrap()
        );
        assert_eq!(store.get(&account).await.unwrap().as_bytes(), b"verified");
    }

    #[tokio::test]
    async fn keyring_unlock_without_remember_uses_only_session_overlay() {
        let backend = Arc::new(TestCredentialBackend::default());
        let store =
            KeyringPasswordStore::with_backend(Arc::clone(&backend) as Arc<dyn CredentialBackend>);
        let account = Account::from_salt("salt");

        assert!(
            !store
                .register_verified(&account, &pw("session-only"), false)
                .await
                .unwrap()
        );
        assert_eq!(backend.writes.load(Ordering::SeqCst), 0);
        assert_eq!(
            store.get(&account).await.unwrap().as_bytes(),
            b"session-only"
        );
        assert!(backend.entries.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn keyring_session_overlay_replaces_previous_and_is_not_in_a_fresh_store() {
        let backend = Arc::new(TestCredentialBackend::default());
        let store =
            KeyringPasswordStore::with_backend(Arc::clone(&backend) as Arc<dyn CredentialBackend>);
        let account = Account::from_salt("salt");
        store
            .register_verified(&account, &pw("first"), false)
            .await
            .unwrap();
        store
            .register_verified(&account, &pw("replacement"), false)
            .await
            .unwrap();

        assert_eq!(
            store.get(&account).await.unwrap().as_bytes(),
            b"replacement"
        );
        let fresh =
            KeyringPasswordStore::with_backend(Arc::clone(&backend) as Arc<dyn CredentialBackend>);
        assert!(matches!(
            fresh.get(&account).await,
            Err(StoreError::NotFound)
        ));
    }

    #[tokio::test]
    async fn keyring_delete_clears_session_when_backend_has_no_entry() {
        let backend = Arc::new(TestCredentialBackend::default());
        let store = KeyringPasswordStore::with_backend(backend as Arc<dyn CredentialBackend>);
        let account = Account::from_salt("salt");
        store
            .register_verified(&account, &pw("session-only"), false)
            .await
            .unwrap();

        store.delete(&account).await.unwrap();

        assert!(matches!(
            store.get(&account).await,
            Err(StoreError::NotFound)
        ));
    }

    #[tokio::test]
    async fn keyring_delete_clears_session_but_surfaces_backend_unavailable() {
        let backend = Arc::new(TestCredentialBackend::default());
        let store =
            KeyringPasswordStore::with_backend(Arc::clone(&backend) as Arc<dyn CredentialBackend>);
        let account = Account::from_salt("salt");
        store
            .register_verified(&account, &pw("session-only"), false)
            .await
            .unwrap();
        backend.fail_delete.store(true, Ordering::SeqCst);

        assert!(matches!(
            store.delete(&account).await,
            Err(StoreError::Unavailable(_))
        ));
        assert!(matches!(
            store.get(&account).await,
            Err(StoreError::NotFound)
        ));
    }

    #[tokio::test]
    async fn keyring_failed_persistent_replacement_preserves_existing_backend_value() {
        let backend = Arc::new(TestCredentialBackend::default());
        let store =
            KeyringPasswordStore::with_backend(Arc::clone(&backend) as Arc<dyn CredentialBackend>);
        let account = Account::from_salt("salt");
        store
            .register_verified(&account, &pw("existing"), true)
            .await
            .unwrap();
        backend.fail_set.store(true, Ordering::SeqCst);

        assert!(matches!(
            store
                .register_verified(&account, &pw("replacement"), true)
                .await,
            Err(StoreError::Unavailable(_))
        ));
        assert_eq!(store.get(&account).await.unwrap().as_bytes(), b"existing");
    }

    #[tokio::test]
    async fn keyring_unlock_with_remember_persists_and_reads_from_backend() {
        let backend = Arc::new(TestCredentialBackend::default());
        let store =
            KeyringPasswordStore::with_backend(Arc::clone(&backend) as Arc<dyn CredentialBackend>);
        let account = Account::from_salt("salt");

        assert!(
            store
                .register_verified(&account, &pw("remembered"), true)
                .await
                .unwrap()
        );
        assert_eq!(backend.writes.load(Ordering::SeqCst), 1);
        assert_eq!(store.get(&account).await.unwrap().as_bytes(), b"remembered");
    }

    #[tokio::test]
    async fn keyring_remember_failure_preserves_old_session_without_replacing_it() {
        let backend = Arc::new(TestCredentialBackend::default());
        backend.fail_set.store(true, Ordering::SeqCst);
        let store =
            KeyringPasswordStore::with_backend(Arc::clone(&backend) as Arc<dyn CredentialBackend>);
        let account = Account::from_salt("salt");
        store
            .register_verified(&account, &pw("earlier-session"), false)
            .await
            .unwrap();

        assert!(matches!(
            store
                .register_verified(&account, &pw("not-fallback"), true)
                .await,
            Err(StoreError::Unavailable(_))
        ));
        assert_eq!(backend.writes.load(Ordering::SeqCst), 1);
        assert_eq!(
            store.get(&account).await.unwrap().as_bytes(),
            b"earlier-session"
        );
        let fresh = KeyringPasswordStore::with_backend(backend as Arc<dyn CredentialBackend>);
        assert!(matches!(
            fresh.get(&account).await,
            Err(StoreError::NotFound)
        ));
    }

    #[tokio::test]
    async fn memory_store_rename_moves_the_password_and_deletes_the_source() {
        let store = MemoryPasswordStore::new();
        let pending = Account::new_pending();
        let salt = Account::from_salt("some-salt-b64");
        store.put(&pending, &pw("first-identity-pw")).await.unwrap();

        store.rename(&pending, &salt).await.unwrap();

        let got = store.get(&salt).await.unwrap();
        assert_eq!(got.as_bytes(), b"first-identity-pw");
        assert!(matches!(
            store.get(&pending).await.unwrap_err(),
            StoreError::NotFound
        ));
    }

    #[tokio::test]
    async fn memory_store_rename_onto_an_existing_salt_account_replaces_it() {
        // Mirrors `unlock --remember` replacing an existing keyring entry.
        let store = MemoryPasswordStore::new();
        let pending = Account::new_pending();
        let salt = Account::from_salt("salt-b64");
        store.put(&salt, &pw("stale-password")).await.unwrap();
        store.put(&pending, &pw("fresh-password")).await.unwrap();

        store.rename(&pending, &salt).await.unwrap();

        let got = store.get(&salt).await.unwrap();
        assert_eq!(got.as_bytes(), b"fresh-password");
    }

    #[tokio::test]
    async fn memory_store_rename_of_missing_source_fails_without_touching_target() {
        let store = MemoryPasswordStore::new();
        let pending = Account::new_pending();
        let salt = Account::from_salt("salt-b64");
        store.put(&salt, &pw("untouched")).await.unwrap();

        let err = store.rename(&pending, &salt).await.unwrap_err();
        assert!(matches!(err, StoreError::NotFound));
        // `to` must be untouched: `get` never ran, so `put` never ran.
        let got = store.get(&salt).await.unwrap();
        assert_eq!(got.as_bytes(), b"untouched");
    }

    #[tokio::test]
    async fn put_then_delete_leaves_no_entry() {
        let store = MemoryPasswordStore::new();
        let account = Account::new_pending();
        store.put(&account, &pw("x")).await.unwrap();
        store.delete(&account).await.unwrap();
        assert!(matches!(
            store.get(&account).await.unwrap_err(),
            StoreError::NotFound
        ));
    }

    // ---- map_keyring_error -------------------------------------------
    //
    // These construct `keyring::Error` values directly rather than forcing
    // a real OS keychain into an unavailable state (which isn't reliably
    // reproducible from a test, especially on a developer's own Mac where
    // the login keychain is essentially always reachable). This is the
    // "simulate keychain unavailable" path COMM-1b's acceptance criteria
    // calls for.

    #[test]
    fn keyring_no_entry_maps_to_not_found() {
        assert!(matches!(
            map_keyring_error(keyring::Error::NoEntry),
            StoreError::NotFound
        ));
    }

    #[test]
    fn keyring_platform_failure_maps_to_unavailable() {
        let err = keyring::Error::PlatformFailure(Box::new(std::io::Error::other(
            "no Secret Service session bus",
        )));
        let mapped = map_keyring_error(err);
        assert!(matches!(mapped, StoreError::Unavailable(_)));
        let StoreError::Unavailable(msg) = mapped else {
            unreachable!()
        };
        assert!(msg.contains("keychain_unavailable"));
    }

    #[test]
    fn keyring_no_storage_access_maps_to_unavailable() {
        let err = keyring::Error::NoStorageAccess(Box::new(std::io::Error::other("locked")));
        assert!(matches!(map_keyring_error(err), StoreError::Unavailable(_)));
    }

    // ---- encode/decode round trip (shared by both backends) -------------

    #[test]
    fn encode_decode_roundtrips() {
        let original = pw("round-trip-me");
        let encoded = encode_password(&original);
        // Must be valid base64url text (no '+', '/', or '=' padding).
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('='));
        let decoded = decode_password(&encoded).unwrap();
        assert_eq!(decoded.as_bytes(), original.as_bytes());
    }

    #[test]
    fn decode_rejects_non_base64() {
        let err = decode_password("not base64url!!").unwrap_err();
        assert!(matches!(err, StoreError::Unavailable(_)));
    }

    // ---- KeyringPasswordStore: constructible, and NotFound round-trips --
    //
    // No test here calls `put`/`get` against `KeyringPasswordStore` with a
    // *real* OS keychain: COMM-1b's instructions are explicit that tests
    // must never write to the real login keychain. A get-before-any-put
    // against a per-test-run-unique account is the one keyring-backed
    // assertion that's safe everywhere: on a real Keychain it's a genuine
    // "not found" (COMM-1b never created this account); in CI containers
    // with no Secret Service / no Keychain daemon at all, `Entry::new` or
    // `get_password` instead report the backend as unavailable — both
    // outcomes are informative, and `#[ignore]` only once the test would
    // otherwise need to leave a real credential behind.
    #[tokio::test]
    async fn keyring_store_get_on_fresh_account_is_not_found_or_unavailable() {
        let store = KeyringPasswordStore::new();
        let account = Account::new_pending();
        match store.get(&account).await {
            Err(StoreError::NotFound) => {}
            Err(StoreError::Unavailable(reason)) => {
                eprintln!("keyring backend unavailable in this environment: {reason}");
            }
            Ok(_) => panic!("a freshly-generated pending account must not already have a secret"),
        }
    }
}
