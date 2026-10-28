//! Local key commands and crash-recoverable issuance compensation.
//!
//! No serve/instance lock is acquired. Call `retry_pending` at daemon startup;
//! `issue` also checks the journal and never issues during a recovery attempt.
//! Configuration stores only `key_id`; the returned secret stays in memory.

use crate::{
    install::InstalledVersion,
    keychain::{Keychain, KeychainError, SystemKeychain},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use zeroize::Zeroizing;

const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_JOURNAL: u64 = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssuedKey {
    pub key_id: String,
    pub secret: Zeroizing<String>,
}

impl std::fmt::Debug for IssuedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedKey")
            .field("key_id", &self.key_id)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

/// Errors never carry child stdout/stderr, serde diagnostics, or keychain details.
#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error(transparent)]
    Keychain(#[from] KeychainError),
    #[error("invalid iDoris key request")]
    InvalidRequest,
    #[error("iDoris key operation is already in progress")]
    Busy,
    #[error("iDoris key command failed")]
    Command,
    #[error("iDoris key command timed out")]
    Timeout,
    #[error("iDoris key output exceeded the limit")]
    OutputLimit,
    #[error("invalid iDoris key response")]
    InvalidResponse,
    #[error("iDoris key operation record I/O failed")]
    Record,
    #[error("previous iDoris issuance was compensated; no new key was issued")]
    Recovered,
    #[error("iDoris key compensation remains pending: {cause}")]
    CompensationPending { cause: Box<KeyError> },
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KeyStatus {
    Active,
    Expired,
    Revoked,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyMetadata {
    pub key_id: String,
    pub owner: String,
    pub tenant_id: Option<String>,
    pub allowed_privacy: Vec<String>,
    pub allowed_roles: Vec<String>,
    pub expires_at_ms: Option<i64>,
    pub revoked_at_ms: Option<i64>,
    pub status: KeyStatus,
}

impl std::fmt::Debug for KeyMetadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyMetadata")
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    owner: String,
    reconcile_owner: bool,
    pending_key_ids: Vec<String>,
}

pub struct Keys {
    binary: PathBuf,
    db_path: PathBuf,
    store: Arc<dyn Keychain>,
    timeout: Duration,
}

/// Issue and immediately persist to the dedicated macOS keychain namespace.
/// Only `daily` and an explicitly requested `deep` are accepted.
pub async fn issue(
    installed_version: &InstalledVersion,
    db_path: &Path,
    install_id: &str,
    roles: &[&str],
) -> Result<IssuedKey, KeyError> {
    if !cfg!(target_os = "macos") {
        return Err(KeychainError::Unsupported.into());
    }
    Keys::new(installed_version, db_path, Arc::new(SystemKeychain))?
        .issue(install_id, roles)
        .await
}

impl Keys {
    pub fn new(
        installed_version: &InstalledVersion,
        db_path: &Path,
        store: Arc<dyn Keychain>,
    ) -> Result<Self, KeyError> {
        if !db_path.is_absolute() {
            return Err(KeyError::InvalidRequest);
        }
        // Canonical DB identity also makes aliases share the compensation lock.
        let db_path = if db_path.exists() {
            fs::canonicalize(db_path).map_err(|_| KeyError::Record)?
        } else {
            let parent = db_path.parent().ok_or(KeyError::InvalidRequest)?;
            fs::canonicalize(parent)
                .map_err(|_| KeyError::Record)?
                .join(db_path.file_name().ok_or(KeyError::InvalidRequest)?)
        };
        Ok(Self {
            binary: installed_version.binary(),
            db_path,
            store,
            timeout: Duration::from_secs(10),
        })
    }

    pub async fn issue(&self, install_id: &str, roles: &[&str]) -> Result<IssuedKey, KeyError> {
        let _lock = self.operation_lock()?;
        if let Some(mut operation) = self.read_operation()? {
            self.compensate(&mut operation).await?;
            return Err(KeyError::Recovered);
        }
        if install_id.is_empty()
            || install_id.len() > 85
            || !install_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
            || roles.iter().any(|role| !matches!(*role, "daily" | "deep"))
        {
            return Err(KeyError::InvalidRequest);
        }
        let owner = format!("agent24d:{install_id}:{}", uuid::Uuid::new_v4().simple());
        let mut operation = Operation {
            owner,
            reconcile_owner: true,
            pending_key_ids: vec![],
        };
        // Write ahead of spawn so cancellation/crash still has an exact owner.
        self.write_operation(&operation)?;
        let result = match self.issue_inner(&mut operation, roles).await {
            // Until this durable removal, a crash is treated as failed issuance.
            Ok(issued) => self.clear_operation().map(|()| issued),
            Err(error) => Err(error),
        };
        match result {
            Ok(issued) => Ok(issued),
            Err(cause) => {
                if self.compensate(&mut operation).await.is_err() {
                    Err(KeyError::CompensationPending {
                        cause: Box::new(cause),
                    })
                } else {
                    Err(cause)
                }
            }
        }
    }

    async fn issue_inner(
        &self,
        operation: &mut Operation,
        roles: &[&str],
    ) -> Result<IssuedKey, KeyError> {
        let allowed_roles = if roles.contains(&"deep") {
            vec!["daily", "deep"]
        } else {
            vec!["daily"]
        };
        let spec = serde_json::to_vec(&serde_json::json!({
            "owner": operation.owner,
            "allowed_privacy": ["local_only"],
            "allowed_roles": allowed_roles,
            "admin_scopes": [],
        }))
        .map_err(|_| KeyError::InvalidRequest)?;
        let output = self.run(&["issue", "--spec-stdin"], &spec).await?;
        let issued: IssuedKey = parse(&output)?;
        if !valid_id(&issued.key_id) || !valid_hex_token(&issued.secret, "idk_", 64) {
            return Err(KeyError::InvalidResponse);
        }
        operation.pending_key_ids = vec![issued.key_id.clone()];
        operation.reconcile_owner = false;
        self.write_operation(operation)?;
        let store = self.store.clone();
        let (issued, saved) = tokio::task::spawn_blocking(move || {
            let saved = store.put(&issued.key_id, &issued.secret);
            (issued, saved)
        })
        .await
        .map_err(|_| KeyError::Keychain(KeychainError::Unavailable))?;
        saved?;
        Ok(issued)
    }

    /// v0.4.0 has no owner flag: filter the returned metadata by exact equality.
    pub async fn list(&self, owner: Option<&str>) -> Result<Vec<KeyMetadata>, KeyError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Reply {
            keys: Vec<KeyMetadata>,
        }
        let reply: Reply = parse(&self.run(&["list"], &[]).await?)?;
        if reply.keys.iter().any(|key| !valid_id(&key.key_id)) {
            return Err(KeyError::InvalidResponse);
        }
        Ok(reply
            .keys
            .into_iter()
            .filter(|key| owner.is_none_or(|owner| key.owner == owner))
            .collect())
    }

    /// Both first-time and already-revoked v0.4.0 responses count as success.
    pub async fn revoke(&self, key_id: &str) -> Result<(), KeyError> {
        if !valid_id(key_id) {
            return Err(KeyError::InvalidRequest);
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Reply {
            key_id: String,
            revoked: bool,
            already_revoked: Option<bool>,
        }
        let reply: Reply = parse(&self.run(&["revoke", key_id], &[]).await?)?;
        if reply.key_id != key_id
            || !(reply.revoked && reply.already_revoked.is_none()
                || !reply.revoked && reply.already_revoked == Some(true))
        {
            return Err(KeyError::InvalidResponse);
        }
        Ok(())
    }

    /// Retry at startup without minting another credential. Failure retains the record.
    pub async fn retry_pending(&self) -> Result<(), KeyError> {
        let _lock = self.operation_lock()?;
        if let Some(mut operation) = self.read_operation()? {
            self.compensate(&mut operation).await?;
        }
        Ok(())
    }

    async fn compensate(&self, operation: &mut Operation) -> Result<(), KeyError> {
        if operation.reconcile_owner {
            operation.pending_key_ids = self
                .list(Some(&operation.owner))
                .await?
                .into_iter()
                .filter(|key| key.status != KeyStatus::Revoked)
                .map(|key| key.key_id)
                .collect();
            operation.reconcile_owner = false;
            self.write_operation(operation)?;
        }
        while let Some(key_id) = operation.pending_key_ids.first() {
            self.revoke(key_id).await?;
            operation.pending_key_ids.remove(0);
            self.write_operation(operation)?;
        }
        self.clear_operation()
    }

    async fn run(&self, args: &[&str], input: &[u8]) -> Result<Zeroizing<Vec<u8>>, KeyError> {
        let mut child = Command::new(&self.binary)
            .arg("key")
            .args(args)
            .env_clear()
            .env("IDORIS_DB_PATH", &self.db_path)
            .env("IDORIS_DEPLOY_MODE", "personal")
            .env("IDORIS_BIND_HOST", "127.0.0.1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| KeyError::Command)?;
        // The key CLI opens only SQLite; it needs no HOME/PATH, model config,
        // state directory, oMLX credentials, or inherited system variables.
        let result = tokio::time::timeout(self.timeout, async {
            let mut stdin = child.stdin.take().ok_or(KeyError::Command)?;
            stdin
                .write_all(input)
                .await
                .map_err(|_| KeyError::Command)?;
            drop(stdin);
            let mut stdout = child.stdout.take().ok_or(KeyError::Command)?;
            let mut output = Zeroizing::new(Vec::with_capacity(MAX_OUTPUT));
            let mut chunk = Zeroizing::new([0u8; 8192]);
            loop {
                let count = stdout
                    .read(&mut *chunk)
                    .await
                    .map_err(|_| KeyError::Command)?;
                if count == 0 {
                    break;
                }
                if output.len() + count > MAX_OUTPUT {
                    return Err(KeyError::OutputLimit);
                }
                output.extend_from_slice(&chunk[..count]);
            }
            if !child.wait().await.map_err(|_| KeyError::Command)?.success() {
                return Err(KeyError::Command);
            }
            Ok(output)
        })
        .await
        .unwrap_or(Err(KeyError::Timeout));
        if result.is_err() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        result
    }

    fn operation_path(&self) -> PathBuf {
        self.db_path.with_extension("key-operation.json")
    }

    fn operation_lock(&self) -> Result<KeyOperationLock, KeyError> {
        let lock = private_open(&self.db_path.with_extension("key-operation.lock"), true)?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| KeyError::Busy)?;
        Ok(KeyOperationLock(lock))
    }

    fn read_operation(&self) -> Result<Option<Operation>, KeyError> {
        let path = self.operation_path();
        if !path.try_exists().map_err(|_| KeyError::Record)? {
            return Ok(None);
        }
        let mut raw = Vec::new();
        private_open(&path, false)?
            .take(MAX_JOURNAL + 1)
            .read_to_end(&mut raw)
            .map_err(|_| KeyError::Record)?;
        if raw.len() as u64 > MAX_JOURNAL {
            return Err(KeyError::Record);
        }
        let operation: Operation = serde_json::from_slice(&raw).map_err(|_| KeyError::Record)?;
        let valid_owner = operation
            .owner
            .strip_prefix("agent24d:")
            .and_then(|tail| tail.rsplit_once(':'))
            .is_some_and(|(install, nonce)| {
                !install.is_empty()
                    && install.len() <= 85
                    && install
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
                    && valid_hex_token(nonce, "", 32)
            });
        if !valid_owner || operation.pending_key_ids.iter().any(|id| !valid_id(id)) {
            return Err(KeyError::Record);
        }
        Ok(Some(operation))
    }

    fn write_operation(&self, operation: &Operation) -> Result<(), KeyError> {
        let path = self.operation_path();
        let parent = path.parent().ok_or(KeyError::Record)?;
        // NamedTempFile starts at 0600; atomic replacement preserves that mode.
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|_| KeyError::Record)?;
        serde_json::to_writer(&mut temp, operation).map_err(|_| KeyError::Record)?;
        temp.flush()
            .and_then(|_| temp.as_file().sync_all())
            .map_err(|_| KeyError::Record)?;
        temp.persist(&path).map_err(|_| KeyError::Record)?;
        sync_directory(parent)
    }

    fn clear_operation(&self) -> Result<(), KeyError> {
        let path = self.operation_path();
        fs::remove_file(&path).map_err(|_| KeyError::Record)?;
        sync_directory(path.parent().ok_or(KeyError::Record)?)
    }
}

fn parse<T: serde::de::DeserializeOwned>(raw: &[u8]) -> Result<T, KeyError> {
    serde_json::from_slice(raw).map_err(|_| KeyError::InvalidResponse)
}

fn valid_hex_token(value: &str, prefix: &str, length: usize) -> bool {
    value.strip_prefix(prefix).is_some_and(|hex| {
        hex.len() == length
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn valid_id(value: &str) -> bool {
    valid_hex_token(value, "vk_", 32)
}

fn private_open(path: &Path, create: bool) -> Result<File, KeyError> {
    let mut options = OpenOptions::new();
    options.read(true).write(create).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| KeyError::Record)?;
    let metadata = file.metadata().map_err(|_| KeyError::Record)?;
    if !metadata.is_file() {
        return Err(KeyError::Record);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(KeyError::Record);
        }
    }
    Ok(file)
}

fn sync_directory(path: &Path) -> Result<(), KeyError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| KeyError::Record)
}

#[cfg(all(test, unix))]
mod tests;

// Explicit unlock also releases descriptors briefly inherited by concurrent forks.
struct KeyOperationLock(File);
impl Drop for KeyOperationLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}
