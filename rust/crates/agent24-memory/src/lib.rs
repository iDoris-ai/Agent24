//! Agent24 memory (M-D / D1).
//!
//! Two layers, both persisted in one SQLite file:
//! - **L0 KV store** ([`KvStore`]): a namespaced key/value store holding
//!   arbitrary JSON. Replaces the ad-hoc `module-state.ts` and is the
//!   substrate for higher layers.
//! - **Canonical session** ([`session`]): a session's conversation with
//!   threshold-triggered LLM-summary compaction, so an unbounded chat stays a
//!   bounded prompt.

pub mod session;

use std::path::Path;
use std::str::FromStr;

use agent24_core::util::now_iso8601;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("summarizer: {0}")]
    Summarizer(String),
}

pub type Result<T> = std::result::Result<T, MemoryError>;

/// L0: a namespaced JSON key-value store over SQLite (WAL, 5s busy timeout).
#[derive(Clone)]
pub struct KvStore {
    pool: SqlitePool,
}

impl KvStore {
    /// Open (creating if needed) a database file and run migrations.
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| MemoryError::Summarizer(format!("mkdir: {e}")))?;
        }
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    /// In-memory database for tests. A single connection: every `:memory:`
    /// connection is a distinct database.
    pub async fn open_memory() -> Result<Self> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")?
            .busy_timeout(std::time::Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    /// Upsert a raw JSON value.
    pub async fn set(&self, namespace: &str, key: &str, value: &Value) -> Result<()> {
        sqlx::query(
            "INSERT INTO kv (namespace, key, value, updated_at) VALUES (?, ?, ?, ?)
             ON CONFLICT(namespace, key) DO UPDATE SET
                 value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(namespace)
        .bind(key)
        .bind(serde_json::to_string(value)?)
        .bind(now_iso8601())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Get a raw JSON value, or `None` if the key is absent.
    pub async fn get(&self, namespace: &str, key: &str) -> Result<Option<Value>> {
        let row = sqlx::query("SELECT value FROM kv WHERE namespace = ? AND key = ?")
            .bind(namespace)
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(row) => Ok(Some(serde_json::from_str(&row.get::<String, _>("value"))?)),
            None => Ok(None),
        }
    }

    /// Typed upsert — serializes `value` to JSON.
    pub async fn put<T: Serialize>(&self, namespace: &str, key: &str, value: &T) -> Result<()> {
        self.set(namespace, key, &serde_json::to_value(value)?)
            .await
    }

    /// Typed get — deserializes into `T`, or `None` if absent.
    pub async fn fetch<T: DeserializeOwned>(
        &self,
        namespace: &str,
        key: &str,
    ) -> Result<Option<T>> {
        match self.get(namespace, key).await? {
            Some(value) => Ok(Some(serde_json::from_value(value)?)),
            None => Ok(None),
        }
    }

    /// Delete a key; returns whether a row was removed.
    pub async fn delete(&self, namespace: &str, key: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM kv WHERE namespace = ? AND key = ?")
            .bind(namespace)
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// All keys in a namespace, sorted.
    pub async fn keys(&self, namespace: &str) -> Result<Vec<String>> {
        let rows = sqlx::query("SELECT key FROM kv WHERE namespace = ? ORDER BY key ASC")
            .bind(namespace)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| r.get::<String, _>("key")).collect())
    }

    /// All (key, value) pairs in a namespace, sorted by key.
    pub async fn entries(&self, namespace: &str) -> Result<Vec<(String, Value)>> {
        let rows = sqlx::query("SELECT key, value FROM kv WHERE namespace = ? ORDER BY key ASC")
            .bind(namespace)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                let key: String = r.get("key");
                let value: Value = serde_json::from_str(&r.get::<String, _>("value"))?;
                Ok((key, value))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde::Deserialize;

    #[tokio::test]
    async fn set_get_delete_roundtrip() {
        let kv = KvStore::open_memory().await.unwrap();
        assert_eq!(kv.get("ns", "missing").await.unwrap(), None);

        kv.set("ns", "k", &serde_json::json!({"a": 1}))
            .await
            .unwrap();
        assert_eq!(
            kv.get("ns", "k").await.unwrap(),
            Some(serde_json::json!({"a": 1}))
        );

        // upsert overwrites
        kv.set("ns", "k", &serde_json::json!({"a": 2}))
            .await
            .unwrap();
        assert_eq!(
            kv.get("ns", "k").await.unwrap(),
            Some(serde_json::json!({"a": 2}))
        );

        assert!(kv.delete("ns", "k").await.unwrap());
        assert!(!kv.delete("ns", "k").await.unwrap());
        assert_eq!(kv.get("ns", "k").await.unwrap(), None);
    }

    #[tokio::test]
    async fn namespaces_are_isolated() {
        let kv = KvStore::open_memory().await.unwrap();
        kv.set("a", "shared", &serde_json::json!(1)).await.unwrap();
        kv.set("b", "shared", &serde_json::json!(2)).await.unwrap();
        assert_eq!(
            kv.get("a", "shared").await.unwrap(),
            Some(serde_json::json!(1))
        );
        assert_eq!(
            kv.get("b", "shared").await.unwrap(),
            Some(serde_json::json!(2))
        );
        // deleting in one namespace leaves the other
        kv.delete("a", "shared").await.unwrap();
        assert_eq!(kv.get("a", "shared").await.unwrap(), None);
        assert_eq!(
            kv.get("b", "shared").await.unwrap(),
            Some(serde_json::json!(2))
        );
    }

    #[tokio::test]
    async fn typed_put_fetch() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Prefs {
            theme: String,
            count: u32,
        }
        let kv = KvStore::open_memory().await.unwrap();
        let prefs = Prefs {
            theme: "dark".to_owned(),
            count: 3,
        };
        kv.put("cfg", "prefs", &prefs).await.unwrap();
        assert_eq!(
            kv.fetch::<Prefs>("cfg", "prefs").await.unwrap(),
            Some(prefs)
        );
        assert_eq!(kv.fetch::<Prefs>("cfg", "nope").await.unwrap(), None);
    }

    #[tokio::test]
    async fn keys_and_entries_are_sorted_and_scoped() {
        let kv = KvStore::open_memory().await.unwrap();
        kv.set("ns", "b", &serde_json::json!("B")).await.unwrap();
        kv.set("ns", "a", &serde_json::json!("A")).await.unwrap();
        kv.set("other", "z", &serde_json::json!("Z")).await.unwrap();
        assert_eq!(kv.keys("ns").await.unwrap(), vec!["a", "b"]);
        let entries = kv.entries("ns").await.unwrap();
        assert_eq!(entries[0], ("a".to_owned(), serde_json::json!("A")));
        assert_eq!(entries[1], ("b".to_owned(), serde_json::json!("B")));
        assert_eq!(kv.keys("other").await.unwrap(), vec!["z"]);
    }

    #[tokio::test]
    async fn persists_across_reopen() {
        let dir = std::env::temp_dir().join(format!("a24mem-{}", std::process::id()));
        let path = dir.join("mem.db");
        let _ = std::fs::remove_dir_all(&dir);
        {
            let kv = KvStore::open(&path).await.unwrap();
            kv.set("ns", "k", &serde_json::json!("v")).await.unwrap();
        }
        let kv = KvStore::open(&path).await.unwrap();
        assert_eq!(
            kv.get("ns", "k").await.unwrap(),
            Some(serde_json::json!("v"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
