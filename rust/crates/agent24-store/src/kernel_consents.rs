//! One row per OS package that a person has agreed kernel capabilities for.
//! Not the per-tool `module_consents` table.

use super::{Result, Store, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelConsentRow {
    pub module: String,
    pub fingerprint: String,
    pub capabilities: Vec<String>,
    pub team_id: Option<String>,
    pub version: String,
    pub decided_at: String,
    pub expires_at: String,
}

#[derive(Debug)]
struct Raw {
    module: String,
    fingerprint: String,
    capabilities: String,
    team_id: Option<String>,
    version: String,
    decided_at: String,
    expires_at: String,
}

impl TryFrom<Raw> for KernelConsentRow {
    type Error = StoreError;

    fn try_from(raw: Raw) -> Result<Self> {
        let capabilities = serde_json::from_str(&raw.capabilities)?;
        Ok(Self {
            module: raw.module,
            fingerprint: raw.fingerprint,
            capabilities,
            team_id: raw.team_id,
            version: raw.version,
            decided_at: raw.decided_at,
            expires_at: raw.expires_at,
        })
    }
}

type ConsentColumns = (
    String,
    String,
    String,
    Option<String>,
    String,
    String,
    String,
);

impl Store {
    pub async fn get_kernel_consent(&self, module: &str) -> Result<Option<KernelConsentRow>> {
        let row: Option<ConsentColumns> = sqlx::query_as(
            "SELECT module, fingerprint, capabilities, team_id, version, decided_at, expires_at \
                 FROM kernel_consents WHERE module = ?",
        )
        .bind(module)
        .fetch_optional(self.pool())
        .await?;
        row.map(
            |(module, fingerprint, capabilities, team_id, version, decided_at, expires_at)| {
                Raw {
                    module,
                    fingerprint,
                    capabilities,
                    team_id,
                    version,
                    decided_at,
                    expires_at,
                }
                .try_into()
            },
        )
        .transpose()
    }

    pub async fn upsert_kernel_consent(&self, row: &KernelConsentRow) -> Result<()> {
        let capabilities = serde_json::to_string(&row.capabilities)?;
        sqlx::query(
            "INSERT INTO kernel_consents \
             (module, fingerprint, capabilities, team_id, version, decided_at, expires_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(module) DO UPDATE SET \
                fingerprint = excluded.fingerprint, \
                capabilities = excluded.capabilities, \
                team_id = excluded.team_id, \
                version = excluded.version, \
                decided_at = excluded.decided_at, \
                expires_at = excluded.expires_at",
        )
        .bind(&row.module)
        .bind(&row.fingerprint)
        .bind(capabilities)
        .bind(&row.team_id)
        .bind(&row.version)
        .bind(&row.decided_at)
        .bind(&row.expires_at)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn delete_kernel_consent(&self, module: &str) -> Result<()> {
        sqlx::query("DELETE FROM kernel_consents WHERE module = ?")
            .bind(module)
            .execute(self.pool())
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[tokio::test]
    async fn a_kernel_consent_round_trips_and_delete_removes_it() {
        let store = Store::open_memory().await.unwrap();
        assert!(store.get_kernel_consent("widget").await.unwrap().is_none());
        let row = KernelConsentRow {
            module: "widget".into(),
            fingerprint: "k=events;o=".into(),
            capabilities: vec!["events".into()],
            team_id: Some("OTHERTEAM1".into()),
            version: "1".into(),
            decided_at: "2026-11-18T00:00:00Z".into(),
            expires_at: "2026-12-18T00:00:00Z".into(),
        };
        store.upsert_kernel_consent(&row).await.unwrap();
        assert_eq!(store.get_kernel_consent("widget").await.unwrap(), Some(row));
        store.delete_kernel_consent("widget").await.unwrap();
        assert!(store.get_kernel_consent("widget").await.unwrap().is_none());
    }
}
