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

/// Trimmed model id. Empty, longer than [`crate::capabilities::first_party::SHOWN_MAX_CHARS`]
/// (120), or any character [`crate::capabilities::first_party::hidden`] refuses
/// is rejected. The stored value is the trimmed id. An id the presence dialog
/// cannot show is not saved.
pub(crate) fn validate_model_id(raw: &str) -> Result<String, &'static str> {
    let model = raw.trim();
    if model.is_empty() {
        return Err("model is empty");
    }
    if model.chars().count() > crate::capabilities::first_party::SHOWN_MAX_CHARS {
        return Err("model is too long");
    }
    if model.chars().any(crate::capabilities::first_party::hidden) {
        return Err("model contains a character that cannot be shown");
    }
    Ok(model.to_owned())
}

/// `Ok` when the id can be shown. The shell gate returns this error as 400
/// before presence.
pub(crate) fn validate_put_body(bytes: &[u8]) -> Result<(), &'static str> {
    let parsed: crate::routes::PutDefaultModelBody =
        serde_json::from_slice(bytes).map_err(|_| "model is required")?;
    validate_model_id(&parsed.model).map(|_| ())
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

/// Seed the first chat default from the local iDoris recommendation. This is
/// deliberately best effort and is called from a background task at startup.
pub(crate) async fn seed_from_idoris(
    router: std::sync::Arc<ModelRouter>,
    kv: KvStore,
    host: Option<std::sync::Arc<agent24_idoris::idoris_host::IdorisHost>>,
) {
    if !matches!(
        kv.fetch::<SavedDefaultChatModel>(NAMESPACE, KEY).await,
        Ok(None)
    ) {
        return;
    }
    let Some(host) = host else {
        return;
    };
    let (source, recommendation) =
        if let Some(v) = host.read_data_json("/v1/recommendation?role=chat").await {
            ("idoris_http", Some(v))
        } else {
            (
                "idoris_cli",
                host.read_model_cli(&["recommend", "--role", "chat", "--json"])
                    .await,
            )
        };
    let Some(value) = recommendation else {
        return;
    };
    match seed_recommendation(&kv, source, &value).await {
        Ok(Some(model)) => router.set_default_model(model),
        Ok(None) => {}
        Err(err) => tracing::warn!("could not seed default chat model from iDoris: {err}"),
    }
}

pub(crate) async fn seed_recommendation(
    kv: &KvStore,
    source: &str,
    recommendation: &serde_json::Value,
) -> Result<Option<String>, MemoryError> {
    if !matches!(source, "idoris_http" | "idoris_cli")
        || kv
            .fetch::<SavedDefaultChatModel>(NAMESPACE, KEY)
            .await?
            .is_some()
        || recommendation.get("installed") != Some(&serde_json::Value::Bool(true))
    {
        return Ok(None);
    }
    let Some(raw) = recommendation
        .get("model_id")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(None);
    };
    let Ok(model) = validate_model_id(raw) else {
        return Ok(None);
    };
    store_default(kv, &model).await?;
    Ok(Some(model))
}
