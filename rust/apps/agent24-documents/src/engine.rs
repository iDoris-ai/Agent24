//! Text engines and building the text layer (ADR-DOC-01 D6, ADR-DOC-02
//! §3.1). The OS depends only on [`Engine`]; slice 1's is the PDFKit helper,
//! and a later cross-platform one plugs in the same way.
//!
//! A read that needs a layer the OS has not pinned starts building it and
//! waits a bounded time: the engine keeps going in the background, so a
//! client that got `engine_unavailable` finds the layer pinned when it tries
//! again. One build per layer key at a time, at most [`PARALLEL`] engines
//! running and [`QUEUE`] more builds waiting.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{Semaphore, watch};

use crate::error::StorageCause;
use crate::state::{Storage, blob_cause, unavailable_cause};
use crate::text_layer::{self, EngineRef, LayerError, TextLayer, config_sha256};

/// Engines parsing at once.
pub const PARALLEL: usize = 2;
/// Builds waiting for an engine; more are refused as busy.
pub const QUEUE: usize = 16;
/// How long a read waits for a layer: inside the kernel proxy's 10 s for a
/// response's head.
pub const WAIT: Duration = Duration::from_secs(8);

pub type Parse = Pin<Box<dyn Future<Output = Result<TextLayer, EngineError>> + Send>>;

/// A text engine: who it is, what it is asked to do, and a parse of the
/// file at `path` (a stored blob) into a layer from exactly that engine and
/// config.
pub trait Engine: Send + Sync {
    fn engine(&self) -> EngineRef;
    fn config(&self) -> Value;
    fn parse(&self, content_sha256: &str, media_type: &str, path: &Path) -> Parse;
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EngineError {
    #[error("the engine does not read this format")]
    Unsupported,
    #[error("the engine could not read the file: {0}")]
    Failed(String),
}

/// Why a read gets no layer.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LayerFailure {
    /// No engine on this platform (D10).
    #[error("no text engine")]
    NoEngine,
    /// Too many builds already waiting.
    #[error("the text engine is busy")]
    Busy,
    /// Still building when the wait ran out; it goes on.
    #[error("the text layer is still being built")]
    Pending,
    #[error(transparent)]
    Engine(EngineError),
    /// Storage failed: with its cause when it is unavailable (§6), none
    /// when the failure is unexpected.
    #[error("storage failed while building the text layer: {1}")]
    Storage(Option<StorageCause>, String),
}

type Outcome = Option<Result<String, LayerFailure>>;

pub struct Layers {
    engine: Option<Arc<dyn Engine>>,
    slots: Arc<Semaphore>,
    /// Builds started and not finished, by layer key.
    building: Mutex<HashMap<String, watch::Receiver<Outcome>>>,
}

impl Layers {
    /// The engine layers are built with, if this platform has one.
    #[must_use]
    pub fn engine(&self) -> Option<&Arc<dyn Engine>> {
        self.engine.as_ref()
    }

    #[must_use]
    pub fn new(engine: Option<Arc<dyn Engine>>) -> Arc<Self> {
        Arc::new(Self {
            engine,
            slots: Arc::new(Semaphore::new(PARALLEL)),
            building: Mutex::default(),
        })
    }

    /// The layer the current engine makes of `content_sha256`: the pinned
    /// one, or one built now. Everything, the first lookup included, ends
    /// within `wait`.
    pub async fn layer(
        self: &Arc<Self>,
        storage: &Arc<Storage>,
        content_sha256: &str,
        media_type: &str,
        wait: Duration,
    ) -> Result<String, LayerFailure> {
        let deadline = tokio::time::Instant::now() + wait;
        let engine = self.engine.clone().ok_or(LayerFailure::NoEngine)?;
        let key = LayerKey::of(engine.as_ref(), content_sha256);
        let stored = tokio::time::timeout_at(deadline, key.pinned(storage))
            .await
            .map_err(|_| LayerFailure::Pending)?;
        if let Some(address) = stored? {
            return Ok(address);
        }
        // Decided under the lock; the task is spawned after it is released
        // (a spawn refused at shutdown drops the guard, which takes the lock).
        let started = {
            let mut building = self.building.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(started) = building.get(&key.name) {
                Err(started.clone())
            } else if building.len() >= PARALLEL + QUEUE {
                return Err(LayerFailure::Busy);
            } else {
                let (tx, rx) = watch::channel(None);
                building.insert(key.name.clone(), rx.clone());
                Ok((tx, rx))
            }
        };
        let mut outcome = match started {
            Err(theirs) => theirs,
            Ok((tx, rx)) => {
                // Leaves `building` however the task ends: finished,
                // panicked or dropped with the runtime.
                let started = Started {
                    layers: self.clone(),
                    name: key.name.clone(),
                };
                let (slots, storage) = (self.slots.clone(), storage.clone());
                let media = media_type.to_owned();
                // Its own task: a read that stops waiting does not stop it.
                tokio::spawn(async move {
                    let result = match slots.acquire().await {
                        // And the parse in one more, so an engine that panics
                        // is a failed build, not a stuck one.
                        Ok(_slot) => tokio::spawn(build(engine, storage, key, media))
                            .await
                            .unwrap_or_else(|_| Err(failed("the engine stopped"))),
                        Err(_) => Err(LayerFailure::Busy),
                    };
                    // Pinned (or failed) before it stops counting as started,
                    // so a later read finds the layer or starts afresh.
                    drop(started);
                    let _ = tx.send(Some(result));
                });
                rx
            }
        };
        match tokio::time::timeout_at(deadline, outcome.wait_for(Option::is_some)).await {
            Ok(Ok(done)) => done.clone().unwrap_or(Err(LayerFailure::Pending)),
            // The task ended without an answer (the runtime is stopping).
            Ok(Err(_)) | Err(_) => Err(LayerFailure::Pending),
        }
    }
}

struct Started {
    layers: Arc<Layers>,
    name: String,
}

impl Drop for Started {
    fn drop(&mut self) {
        self.layers
            .building
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.name);
    }
}

/// One layer's key: the content and the engine build and config that read it.
struct LayerKey {
    content: String,
    engine: EngineRef,
    config: String,
    name: String,
}

impl LayerKey {
    fn of(engine: &dyn Engine, content_sha256: &str) -> Self {
        let (id, config) = (engine.engine(), config_sha256(&engine.config()));
        let name = format!("{content_sha256} {} {} {config}", id.id, id.version);
        Self {
            content: content_sha256.to_owned(),
            engine: id,
            config,
            name,
        }
    }

    async fn pinned(&self, storage: &Storage) -> Result<Option<String>, LayerFailure> {
        text_layer::pinned(storage, &self.content, &self.engine, &self.config)
            .await
            .map_err(|e| LayerFailure::Storage(unavailable_cause(&e), e.to_string()))
    }
}

fn failed(why: &str) -> LayerFailure {
    LayerFailure::Engine(EngineError::Failed(why.to_owned()))
}

/// Parses the key's content and pins the result. A layer that claims
/// another engine or config, or breaks §3.1, is the engine's failure.
async fn build(
    engine: Arc<dyn Engine>,
    storage: Arc<Storage>,
    key: LayerKey,
    media_type: String,
) -> Result<String, LayerFailure> {
    // A build that finished between this one's lookup and its start.
    if let Some(address) = key.pinned(&storage).await? {
        return Ok(address);
    }
    let path = storage
        .blobs
        .path_of(&key.content)
        .map_err(|e| LayerFailure::Storage(Some(blob_cause(&e)), e.to_string()))?;
    let layer = engine
        .parse(&key.content, &media_type, &path)
        .await
        .map_err(LayerFailure::Engine)?;
    if layer.engine != key.engine || layer.config != engine.config() {
        return Err(failed("the layer names another engine or config"));
    }
    text_layer::pin(&storage, &key.content, &layer)
        .await
        .map_err(|e| match e {
            LayerError::Invalid(why) => failed(&why),
            LayerError::Blob(e) => LayerFailure::Storage(Some(blob_cause(&e)), e.to_string()),
            LayerError::Db(e) => LayerFailure::Storage(unavailable_cause(&e), e.to_string()),
        })
}

#[cfg(test)]
mod tests;

pub mod pdfkit;
