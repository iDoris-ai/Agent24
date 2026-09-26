//! ME4-S3 §2.4/§3.5 — `SchedulerClient` (`_a24/scheduler/{upsert,delete,
//! list}`).

use std::sync::Arc;

use agent24_os_proto::module::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{Core, malformed_result, set_opt};
use crate::context::RequestId;
use crate::error::ClientError;

pub const UPSERT: &str = "_a24/scheduler/upsert";
pub const DELETE: &str = "_a24/scheduler/delete";
pub const LIST: &str = "_a24/scheduler/list";
pub(crate) const METHODS: [&str; 3] = [UPSERT, DELETE, LIST];
const OFFER_PREFIX: &str = "_a24/scheduler/";

/// A module schedule's desired firing rule — the same three shapes the
/// kernel's wire params accept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScheduleSpec {
    Cron {
        expr: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tz: Option<String>,
    },
    Every {
        secs: u32,
    },
    At {
        ts: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpsertOutcome {
    Created,
    Updated,
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteOutcome {
    Deleted,
    Absent,
}

/// The schedule's most recent fire from one trigger source.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LastFire {
    pub fire_id: String,
    pub scheduled_for: String,
    /// `pending` | `deferred` | `delivered` | `failed` | `expired`.
    pub status: String,
    pub last_error: Option<String>,
}

/// The latest fire per source: `tick` and `run_now` are tracked separately so
/// one never hides what happened to the other's last slot.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LastFires {
    pub tick: Option<LastFire>,
    pub run_now: Option<LastFire>,
}

/// One module row as the module itself sees it: its desired state plus the
/// kernel-side reasons it may not fire and when it next will. Never carries
/// the kernel's internal schedule id.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ScheduleState {
    pub key: String,
    pub spec: ScheduleSpec,
    pub enabled: bool,
    pub label: String,
    pub user_suspended: bool,
    pub system_disabled_reason: Option<String>,
    pub next_run_at: Option<String>,
    pub last_fire: LastFires,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UpsertResult {
    pub outcome: UpsertOutcome,
    pub schedule: ScheduleState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct DeleteResult {
    pub outcome: DeleteOutcome,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ListResult {
    pub schedules: Vec<ScheduleState>,
}

pub struct UpsertRequest<'a> {
    pub key: &'a str,
    pub spec: &'a ScheduleSpec,
    /// Desired-state semantics: this is always sent explicitly — there is no
    /// "leave it as it was" meaning to omitting it.
    pub enabled: bool,
    /// Absent (`None`) means "use the key as the label" (kernel default).
    pub label: Option<&'a str>,
}

pub struct SchedulerClient(Core);

impl SchedulerClient {
    #[must_use]
    pub fn new(conn: &Arc<Connection>) -> Option<Self> {
        Core::new(conn, OFFER_PREFIX).map(Self)
    }

    /// # Errors
    /// See [`ClientError`].
    pub async fn upsert(
        &self,
        r: &UpsertRequest<'_>,
        request_id: Option<&RequestId>,
    ) -> Result<UpsertResult, ClientError> {
        let mut params = Map::new();
        params.insert("key".to_owned(), Value::String(r.key.to_owned()));
        let spec =
            serde_json::to_value(r.spec).map_err(|e| malformed_result("schedule spec", e))?;
        params.insert("spec".to_owned(), spec);
        params.insert("enabled".to_owned(), Value::Bool(r.enabled));
        set_opt(
            &mut params,
            "label",
            r.label.map(|l| Value::String(l.to_owned())),
        );
        set_opt(
            &mut params,
            "request_id",
            request_id.map(|id| Value::String(id.as_str().to_owned())),
        );
        let value = self.0.call(UPSERT, Value::Object(params)).await?;
        serde_json::from_value(value).map_err(|e| malformed_result("upsert result", e))
    }

    /// # Errors
    /// See [`ClientError`].
    pub async fn delete(
        &self,
        key: &str,
        request_id: Option<&RequestId>,
    ) -> Result<DeleteResult, ClientError> {
        let mut params = Map::new();
        params.insert("key".to_owned(), Value::String(key.to_owned()));
        set_opt(
            &mut params,
            "request_id",
            request_id.map(|id| Value::String(id.as_str().to_owned())),
        );
        let value = self.0.call(DELETE, Value::Object(params)).await?;
        serde_json::from_value(value).map_err(|e| malformed_result("delete result", e))
    }

    /// # Errors
    /// See [`ClientError`].
    pub async fn list(&self, request_id: Option<&RequestId>) -> Result<ListResult, ClientError> {
        let mut params = Map::new();
        set_opt(
            &mut params,
            "request_id",
            request_id.map(|id| Value::String(id.as_str().to_owned())),
        );
        let value = self.0.call(LIST, Value::Object(params)).await?;
        serde_json::from_value(value).map_err(|e| malformed_result("list result", e))
    }
}

#[cfg(all(test, feature = "test-util"))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;
    use crate::testing;

    #[tokio::test]
    async fn without_the_offer_prefix_new_returns_none() {
        let (conn, _peer) = testing::fake_kernel(vec![]).await;
        assert!(SchedulerClient::new(&conn).is_none());
    }

    #[tokio::test]
    async fn upsert_serializes_the_tagged_spec_and_omits_absent_label() {
        let (conn, mut peer) = testing::fake_kernel(vec![OFFER_PREFIX.to_owned()]).await;
        let client = SchedulerClient::new(&conn).expect("offer covers scheduler");
        let spec = ScheduleSpec::Every { secs: 3600 };
        let req = UpsertRequest {
            key: "k1",
            spec: &spec,
            enabled: true,
            label: None,
        };
        // `tokio::join!`, not `tokio::spawn`: `req` borrows `spec`, which is
        // not `'static`.
        let (result, ()) = tokio::join!(client.upsert(&req, None), async {
            let sent = testing::read_request(&mut peer).await;
            assert_eq!(sent["method"], UPSERT);
            assert_eq!(sent["params"]["key"], "k1");
            assert_eq!(
                sent["params"]["spec"],
                json!({"type": "every", "secs": 3600})
            );
            assert_eq!(sent["params"]["enabled"], true);
            assert!(sent["params"].as_object().unwrap().get("label").is_none());
            testing::respond(
                &mut peer,
                &sent,
                json!({
                    "outcome": "created",
                    "schedule": {
                        "key": "k1",
                        "spec": {"type": "every", "secs": 3600},
                        "enabled": true,
                        "label": "k1",
                        "user_suspended": false,
                        "system_disabled_reason": null,
                        "next_run_at": "2026-01-01T01:00:00Z",
                        "last_fire": {"tick": null, "run_now": null},
                    },
                }),
            )
            .await;
        });
        let result = result.unwrap();
        assert_eq!(result.outcome, UpsertOutcome::Created);
        assert_eq!(result.schedule.key, "k1");
    }

    #[tokio::test]
    async fn delete_and_list_round_trip() {
        let (conn, mut peer) = testing::fake_kernel(vec![OFFER_PREFIX.to_owned()]).await;
        let client = SchedulerClient::new(&conn).expect("offer covers scheduler");
        let call = tokio::spawn(async move { client.delete("k1", None).await });
        let req = testing::read_request(&mut peer).await;
        assert_eq!(req["method"], DELETE);
        testing::respond(&mut peer, &req, json!({"outcome": "deleted"})).await;
        assert_eq!(call.await.unwrap().unwrap().outcome, DeleteOutcome::Deleted);
    }
}
