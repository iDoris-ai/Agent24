//! C9-B capability-token decisions on the existing hash-chained audit log.
//!
//! [`crate::ModuleToolAuditEvent`] cannot carry these: every variant requires
//! a module-tool relation (run, tool call, attempt). A token decision has
//! none of those, and inventing them would be a false correlation. This is a
//! closed variant family on the same `append_verified_audit_tx` chain. There
//! is no free-text field and no token secret.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AuditEntry, AuditRef, AuditTimestamp, Result, Store, StoreError};

/// The only component-runtime tier this process issues (C9 dev-bridge).
pub const COMPONENT_RUNTIME_TTL_SECONDS: u64 = 8 * 60 * 60;
pub const DEV_BRIDGE_COMPONENT_ID: &str = "wechat-bridge";
pub const DEV_BRIDGE_OPERATION: &str = "inbound.ingest";

fn invalid(msg: impl Into<String>) -> StoreError {
    StoreError::InvalidAuditMetadata(msg.into())
}

/// Why an explicit revoke was recorded. Generation expiry and TTL are
/// validation failures, not rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityRevokeReason {
    Explicit,
}

/// Why a presented credential was refused. Closed; not a message string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityDenyReason {
    Audience,
    Component,
    Operation,
    Route,
    /// Presented after the body was read; the in-memory record is revoked.
    Revoked,
    /// Monotonic deadline or, for a bridge token, wall-clock `expires_at`.
    Expired,
    /// `daemon_generation` does not match this process.
    StaleGeneration,
}

/// One capability-token decision. `deny_unknown_fields` rejects a smuggled
/// JSON field instead of storing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CapabilityTokenAuditEvent {
    Issued {
        component_id: AuditRef,
        capability_id: AuditRef,
        operation: AuditRef,
        ttl_seconds: u64,
    },
    Revoked {
        component_id: AuditRef,
        capability_id: AuditRef,
        reason: CapabilityRevokeReason,
    },
    Denied {
        reason: CapabilityDenyReason,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        component_id: Option<AuditRef>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        capability_id: Option<AuditRef>,
        operation: AuditRef,
    },
}

impl CapabilityTokenAuditEvent {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Issued {
                component_id,
                operation,
                ttl_seconds,
                ..
            } => {
                if component_id.as_str() != DEV_BRIDGE_COMPONENT_ID
                    || operation.as_str() != DEV_BRIDGE_OPERATION
                    || *ttl_seconds != COMPONENT_RUNTIME_TTL_SECONDS
                {
                    return Err(invalid(
                        "capability issue audit must be the dev-bridge profile",
                    ));
                }
            }
            Self::Revoked { component_id, .. } => {
                if component_id.as_str() != DEV_BRIDGE_COMPONENT_ID {
                    return Err(invalid(
                        "capability revoke audit must name the dev-bridge component",
                    ));
                }
            }
            Self::Denied { operation, .. } => {
                if operation.as_str() != DEV_BRIDGE_OPERATION {
                    return Err(invalid("capability deny audit must name inbound.ingest"));
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn action(&self) -> &'static str {
        match self {
            Self::Issued { .. } => "capability.token.issued",
            Self::Revoked { .. } => "capability.token.revoked",
            Self::Denied { .. } => "capability.token.denied",
        }
    }
}

impl Store {
    /// Append one token decision. `detail` is only this event. A failed
    /// chain check writes nothing.
    pub async fn append_capability_token_audit(
        &self,
        ts: &AuditTimestamp,
        event: &CapabilityTokenAuditEvent,
    ) -> Result<AuditEntry> {
        event.validate()?;
        let detail: Value = serde_json::to_value(event)?;
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        Store::verify_audit_chain_tx(&mut tx).await?;
        let entry = Store::append_verified_audit_tx(
            &mut tx,
            ts.as_str(),
            "kernel",
            event.action(),
            &detail,
        )
        .await?;
        tx.commit().await?;
        Ok(entry)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn ts() -> AuditTimestamp {
        AuditTimestamp::new("2026-11-12T00:00:00.000Z").unwrap()
    }

    fn issued() -> CapabilityTokenAuditEvent {
        CapabilityTokenAuditEvent::Issued {
            component_id: AuditRef::new(DEV_BRIDGE_COMPONENT_ID).unwrap(),
            capability_id: AuditRef::new("aabbccddeeff00112233445566778899").unwrap(),
            operation: AuditRef::new(DEV_BRIDGE_OPERATION).unwrap(),
            ttl_seconds: COMPONENT_RUNTIME_TTL_SECONDS,
        }
    }

    #[tokio::test]
    async fn issue_audit_round_trips_and_rejects_a_foreign_field() {
        let store = Store::open_memory().await.unwrap();
        let event = issued();
        let entry = store
            .append_capability_token_audit(&ts(), &event)
            .await
            .unwrap();
        assert_eq!(entry.action, "capability.token.issued");
        assert_eq!(entry.actor, "kernel");
        let parsed: CapabilityTokenAuditEvent =
            serde_json::from_value(entry.detail.clone()).unwrap();
        assert_eq!(parsed, event);
        assert!(!entry.detail.to_string().contains("token"));

        let mut smuggled = serde_json::to_value(&event).unwrap();
        smuggled["issued"]
            .as_object_mut()
            .unwrap()
            .insert("note".into(), Value::String("secret-body".into()));
        assert!(serde_json::from_value::<CapabilityTokenAuditEvent>(smuggled).is_err());
    }

    #[tokio::test]
    async fn a_non_bridge_issue_is_not_written() {
        let store = Store::open_memory().await.unwrap();
        let mut event = issued();
        if let CapabilityTokenAuditEvent::Issued { ttl_seconds, .. } = &mut event {
            *ttl_seconds = 60;
        }
        assert!(
            store
                .append_capability_token_audit(&ts(), &event)
                .await
                .is_err()
        );
        assert!(store.list_audit().await.unwrap().is_empty());
    }

    fn revoked() -> CapabilityTokenAuditEvent {
        CapabilityTokenAuditEvent::Revoked {
            component_id: AuditRef::new(DEV_BRIDGE_COMPONENT_ID).unwrap(),
            capability_id: AuditRef::new("aabbccddeeff00112233445566778899").unwrap(),
            reason: CapabilityRevokeReason::Explicit,
        }
    }

    fn denied() -> CapabilityTokenAuditEvent {
        CapabilityTokenAuditEvent::Denied {
            reason: CapabilityDenyReason::Operation,
            component_id: Some(AuditRef::new(DEV_BRIDGE_COMPONENT_ID).unwrap()),
            capability_id: Some(AuditRef::new("aabbccddeeff00112233445566778899").unwrap()),
            operation: AuditRef::new(DEV_BRIDGE_OPERATION).unwrap(),
        }
    }

    /// Revoked rows name `wechat-bridge`. A foreign component is not written.
    /// Deleting the component check in `validate` turns this red.
    #[tokio::test]
    async fn revoke_audit_rejects_a_foreign_component() {
        let store = Store::open_memory().await.unwrap();
        let mut event = revoked();
        if let CapabilityTokenAuditEvent::Revoked { component_id, .. } = &mut event {
            *component_id = AuditRef::new("other-bridge").unwrap();
        }
        assert!(
            store
                .append_capability_token_audit(&ts(), &event)
                .await
                .is_err()
        );
        assert!(store.list_audit().await.unwrap().is_empty());
    }

    /// Denied rows name `inbound.ingest`. Another operation is not written.
    /// Deleting the operation check in `validate` turns this red.
    #[tokio::test]
    async fn deny_audit_rejects_a_foreign_operation() {
        let store = Store::open_memory().await.unwrap();
        let mut event = denied();
        if let CapabilityTokenAuditEvent::Denied { operation, .. } = &mut event {
            *operation = AuditRef::new("models.read").unwrap();
        }
        assert!(
            store
                .append_capability_token_audit(&ts(), &event)
                .await
                .is_err()
        );
        assert!(store.list_audit().await.unwrap().is_empty());
    }
}
