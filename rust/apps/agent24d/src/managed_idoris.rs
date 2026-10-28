//! Managed connection state; keychain and supervisor implementations are injected.
use agent24_models::{IdorisGateway, ModelError, router::ModelRouter};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{io::Write, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedConfig {
    pub url: String,
    pub key_id: String,
}

#[derive(Debug)]
pub enum SecretError {
    Locked,
    Unavailable,
}

#[async_trait]
pub trait SecretSource: Send + Sync {
    async fn secret(&self, key_id: &str) -> Result<Zeroizing<String>, SecretError>;
}

#[async_trait]
pub trait SupervisorReady: Send + Sync {
    async fn wait_ready(&self, cancel: &CancellationToken) -> Result<(), ModelError>;
}

pub struct ManagedDependencies {
    pub secrets: Arc<dyn SecretSource>,
    pub supervisor: Arc<dyn SupervisorReady>,
}

pub struct ManagedIdoris {
    router: Arc<ModelRouter>,
    path: PathBuf,
    update: Mutex<()>,
}

fn error(message: &str) -> ModelError {
    ModelError::Provider(message.into())
}

impl ManagedIdoris {
    pub fn new(router: Arc<ModelRouter>, path: PathBuf) -> std::io::Result<Self> {
        if path.try_exists()? {
            router.block_idoris("managed iDoris is waiting for supervisor readiness and Keychain");
        }
        Ok(Self {
            router,
            path,
            update: Mutex::new(()),
        })
    }

    pub fn is_configured(&self) -> std::io::Result<bool> {
        self.path.try_exists()
    }

    /// Restore before admitting requests. No secret is read until the service is ready.
    pub async fn restore(
        &self,
        dependencies: &ManagedDependencies,
        cancel: &CancellationToken,
    ) -> Result<bool, ModelError> {
        let _update = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ModelError::Cancelled),
            guard = self.update.lock() => guard,
        };
        let body = match std::fs::read(&self.path) {
            Ok(body) => body,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(error("cannot read managed iDoris configuration")),
        };
        let config: ManagedConfig = serde_json::from_slice(&body)
            .map_err(|_| error("invalid managed iDoris configuration"))?;
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ModelError::Cancelled),
            result = dependencies.supervisor.wait_ready(cancel) => result?,
        }
        let gateway = Self::prepare(&config, dependencies.secrets.as_ref(), cancel).await?;
        if cancel.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        self.router.replace_idoris(gateway);
        Ok(true)
    }

    /// Validation and persistence precede the one publication point. There is no
    /// await between the disk commit and snapshot replacement.
    pub async fn connect(
        &self,
        config: ManagedConfig,
        secrets: &dyn SecretSource,
        cancel: &CancellationToken,
    ) -> Result<(), ModelError> {
        let _update = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ModelError::Cancelled),
            guard = self.update.lock() => guard,
        };
        let gateway = Self::prepare(&config, secrets, cancel).await?;
        if cancel.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        self.persist(&config)?;
        self.router.replace_idoris(gateway);
        Ok(())
    }

    async fn prepare(
        config: &ManagedConfig,
        secrets: &dyn SecretSource,
        cancel: &CancellationToken,
    ) -> Result<IdorisGateway, ModelError> {
        if config.key_id.is_empty()
            || config.key_id.len() > 128
            || !config
                .key_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(error("invalid iDoris key reference"));
        }
        let key = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ModelError::Cancelled),
            result = secrets.secret(&config.key_id) => result.map_err(|e| match e {
                SecretError::Locked => error("unlock Keychain to connect to iDoris"),
                SecretError::Unavailable => error("iDoris data key unavailable in Keychain"),
            })?,
        };
        let gateway =
            IdorisGateway::from_credentials(&config.url, key).map_err(ModelError::Provider)?;
        gateway.check_health(cancel).await?;
        Ok(gateway)
    }

    fn persist(&self, config: &ManagedConfig) -> Result<(), ModelError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| error("invalid iDoris config path"))?;
        let body = serde_json::to_vec(config).map_err(|_| error("cannot encode iDoris config"))?;
        let write = || -> std::io::Result<()> {
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            file.write_all(&body)?;
            file.as_file().sync_all()?;
            file.persist(&self.path).map_err(|e| e.error)?;
            Ok(())
        };
        write().map_err(|_| error("cannot persist managed iDoris configuration"))?;
        // The rename has committed. A directory-sync failure must not leave the
        // live router on the old reference while disk already names the new one.
        if let Err(e) = std::fs::File::open(parent).and_then(|dir| dir.sync_all()) {
            tracing::warn!(error = %e, "iDoris configuration directory sync failed after commit");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
