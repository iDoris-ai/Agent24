//! The daemon's saved default chat model.
//!
//! One row in the memory base's existing `kv` table (`daemon_settings` /
//! `default_chat_model`). There is no second settings store. The value is
//! applied onto the live [`ModelRouter`](agent24_models::router::ModelRouter)
//! at startup and again on every successful `PUT /api/v1/models/default`.

use agent24_memory::{KvStore, MemoryError};
use agent24_models::router::ModelRouter;
use serde::{Deserialize, Serialize};

pub(crate) const NAMESPACE: &str = "daemon_settings";
pub(crate) const KEY: &str = "default_chat_model";

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SavedDefaultChatModel {
    pub model: String,
}

/// Trimmed model id. Empty, longer than 128 characters, or any control
/// character is rejected. The stored value is the trimmed id.
pub(crate) fn validate_model_id(raw: &str) -> Result<String, &'static str> {
    let model = raw.trim();
    if model.is_empty() {
        return Err("model is empty");
    }
    if model.chars().count() > 128 {
        return Err("model is too long");
    }
    if model.chars().any(char::is_control) {
        return Err("model contains a control character");
    }
    Ok(model.to_owned())
}

/// Saved value wins over whatever the router was built with. A missing row
/// leaves the router alone. A corrupt or invalid row is ignored so startup
/// still serves with the env or built-in default.
pub(crate) async fn apply_saved(router: &ModelRouter, kv: &KvStore) {
    match kv.fetch::<SavedDefaultChatModel>(NAMESPACE, KEY).await {
        Ok(Some(saved)) => match validate_model_id(&saved.model) {
            Ok(model) => router.set_default_model(model),
            Err(_) => {
                tracing::warn!("saved default chat model is invalid; keeping the startup default");
            }
        },
        Ok(None) => {}
        Err(_) => {
            tracing::warn!(
                "could not read the saved default chat model; keeping the startup default"
            );
        }
    }
}

pub(crate) async fn store_default(kv: &KvStore, model: &str) -> Result<(), MemoryError> {
    kv.put(
        NAMESPACE,
        KEY,
        &SavedDefaultChatModel {
            model: model.to_owned(),
        },
    )
    .await
}
