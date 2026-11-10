//! Managed connection state; keychain and supervisor implementations are injected.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{any::Any, io::Write, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

/// Same `Display` text as the three [`agent24_models::ModelError`] variants this
/// host constructs. `agent24-models` already depends on this crate, so the host
/// cannot name that error (or `ModelRouter`) without a cycle.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    #[error("provider unavailable: {0}")]
    Unavailable(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("cancelled")]
    Cancelled,
    /// A gateway error this host does not match on. The string is that error's
    /// full `Display`, so startup logs stay the same.
    #[error("{0}")]
    Other(String),
}

/// A gateway built by [`IdorisSlot::open_gateway`], still unpublished.
pub struct PreparedGateway {
    inner: Box<dyn Any + Send + Sync>,
}

impl PreparedGateway {
    pub fn new<T: Send + Sync + 'static>(value: T) -> Self {
        Self {
            inner: Box::new(value),
        }
    }

    pub fn downcast<T: Send + Sync + 'static>(self) -> Result<T, Self> {
        match self.inner.downcast::<T>() {
            Ok(value) => Ok(*value),
            Err(inner) => Err(Self { inner }),
        }
    }

    pub fn downcast_ref<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.inner.downcast_ref::<T>()
    }
}

/// How many strong references the published gateway still has.
/// The closure closes over the same `Weak` the router stored.
pub struct GatewayLease {
    strong_count: Box<dyn Fn() -> usize + Send + Sync>,
}

impl GatewayLease {
    pub fn new(strong_count: impl Fn() -> usize + Send + Sync + 'static) -> Self {
        Self {
            strong_count: Box::new(strong_count),
        }
    }

    pub fn strong_count(&self) -> usize {
        (self.strong_count)()
    }
}

/// Evidence from one real daily chat, copied so this crate does not name the
/// gateway type that produces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionCheck {
    pub model_id: String,
    pub record_id: String,
    pub locality: String,
}

/// What the host used to call on `ModelRouter` / `IdorisGateway`.
/// Implemented for `ModelRouter` in `agent24-models`.
#[async_trait]
pub trait IdorisSlot: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn block_idoris(&self, reason: &str);
    fn replace_idoris(&self, gateway: PreparedGateway);
    fn published_lease(&self) -> Option<GatewayLease>;
    async fn open_gateway(
        &self,
        url: &str,
        secret: Zeroizing<String>,
        cancel: &CancellationToken,
    ) -> Result<PreparedGateway, ModelError>;
    async fn verify_connection(
        &self,
        gateway: &PreparedGateway,
        headroom_confirmed: bool,
        cancel: &CancellationToken,
    ) -> Result<ConnectionCheck, ModelError>;
}

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
    router: Arc<dyn IdorisSlot>,
    path: PathBuf,
    update: Mutex<()>,
}

fn error(message: &str) -> ModelError {
    ModelError::Provider(message.into())
}

impl ManagedIdoris {
    pub fn new(router: Arc<dyn IdorisSlot>, path: PathBuf) -> std::io::Result<Self> {
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
        let gateway = Self::prepare(
            &config,
            dependencies.secrets.as_ref(),
            cancel,
            self.router.as_ref(),
        )
        .await?;
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
        let gateway = Self::prepare(&config, secrets, cancel, self.router.as_ref()).await?;
        if cancel.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        self.persist(&config)?;
        self.router.replace_idoris(gateway);
        Ok(())
    }

    pub(crate) async fn prepare(
        config: &ManagedConfig,
        secrets: &dyn SecretSource,
        cancel: &CancellationToken,
        slot: &dyn IdorisSlot,
    ) -> Result<PreparedGateway, ModelError> {
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
        slot.open_gateway(&config.url, key, cancel).await
    }

    pub(crate) fn persist_config(
        &self,
        config: &ManagedConfig,
    ) -> Result<(), crate::idoris_host::Code> {
        self.persist(config)
            .map_err(|_| crate::idoris_host::Code::PersistenceFailed)?;
        Ok(())
    }
    pub(crate) fn restore_config(
        &self,
        config: Option<ManagedConfig>,
    ) -> Result<(), crate::idoris_host::Code> {
        match config {
            Some(config) => self
                .persist(&config)
                .map_err(|_| crate::idoris_host::Code::PersistenceFailed),
            None => match std::fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(_) => Err(crate::idoris_host::Code::PersistenceFailed),
            },
        }
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
