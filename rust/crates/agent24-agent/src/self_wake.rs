//! H5: self-wake — the agent scheduling its own follow-ups.
//!
//! A 24/7 assistant needs to say "check the build in ten minutes" or "remind me
//! tonight" without a human wiring a schedule. `self_wake` lets the agent create
//! a ONE-SHOT schedule that fires a follow-up run in THIS session — so the woken
//! run reloads the session's context and genuinely continues the conversation.
//!
//! It is built ENTIRELY on the existing scheduler: `tick()` re-reads the store
//! every tick (`list_schedules_lenient`), so a schedule this tool inserts is
//! picked up with no scheduler change, fires once (`ScheduleSpec::At`), and
//! clears its `next_run_at` after firing (the one-shot contract). Shutdown
//! cancellation is inherited: the scheduler owns the fired run's lifecycle, and
//! a run spawned by a wake is cancelled with the scheduler like any other.
//!
//! ## Why it is not gated, and why that is safe
//!
//! Creating a schedule is a control-plane operation, exactly what the existing
//! `POST /api/v1/schedules` endpoint does (and that is not behind the tool gate).
//! The DANGER is never in scheduling — it is in what the woken run then does, and
//! that run's every tool call goes through the SAME C4/D3/H1–H4 gate. A self-wake
//! into the small hours cannot run an ungated action: with durable resume (H3) a
//! 3am approval simply parks until the human answers. So `self_wake` is `Read`
//! (no machine side effect of its own) and bounded by [`MAX_PENDING_WAKES`] to
//! stop a runaway loop from flooding the schedule table.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use agent24_core::util::{iso8601_after, ulid};
use agent24_protocol::{RiskClass, Schedule, ScheduleAction, ScheduleSpec, ToolInfo};
use agent24_store::Store;
use agent24_tools::{Tool, ToolContext, ToolError};

/// The schedule `name` every self-wake carries, so pending ones can be counted
/// and a UI can tell an agent's own follow-ups apart from user schedules.
pub const SELF_WAKE_NAME: &str = "self-wake";

/// Ceiling on outstanding (enabled, not-yet-fired) self-wakes. Bounds a runaway
/// loop that keeps rescheduling itself — fail-closed: over the cap, refuse.
pub const MAX_PENDING_WAKES: usize = 32;

/// The `self_wake` tool. Holds a store handle so it can persist a one-shot
/// schedule; the daemon builds it with the same store the scheduler reads.
pub struct SelfWakeTool {
    store: Store,
}

impl SelfWakeTool {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

impl SelfWakeTool {
    /// The wake time: `after_secs` (relative, always well-formed) or `at`
    /// (absolute ISO-8601 UTC). Exactly one must be given.
    fn wake_time(input: &Map<String, Value>) -> Result<String, ToolError> {
        let after = input.get("after_secs").and_then(Value::as_u64);
        let at = input
            .get("at")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        match (after, at) {
            (Some(_), Some(_)) => Err(ToolError::Invalid(
                "give either after_secs or at, not both".to_owned(),
            )),
            (Some(secs), None) => {
                if secs == 0 {
                    return Err(ToolError::Invalid("after_secs must be >= 1".to_owned()));
                }
                Ok(iso8601_after(Duration::from_secs(secs)))
            }
            // Absolute time: require the fixed-width UTC shape the rest of the
            // system emits (YYYY-MM-DDTHH:MM:SSZ), so the scheduler can parse it.
            (None, Some(at)) => {
                if is_iso8601_utc(at) {
                    Ok(at.to_owned())
                } else {
                    Err(ToolError::Invalid(format!(
                        "`at` must be ISO-8601 UTC like 2026-07-28T14:30:00Z, got {at:?}"
                    )))
                }
            }
            (None, None) => Err(ToolError::Invalid(
                "either after_secs or at is required".to_owned(),
            )),
        }
    }
}

/// Cheap shape check for the fixed-width UTC timestamp the daemon uses. Not a
/// full parser — it screens obvious garbage so the scheduler never inherits an
/// unparsable `next_run_at`; the scheduler's own parse is the real gate.
fn is_iso8601_utc(s: &str) -> bool {
    let b = s.as_bytes();
    s.len() == 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'Z'
        && b.iter()
            .enumerate()
            .filter(|(i, _)| ![4, 7, 10, 13, 16, 19].contains(i))
            .all(|(_, c)| c.is_ascii_digit())
}

#[async_trait]
impl Tool for SelfWakeTool {
    fn info(&self) -> ToolInfo {
        ToolInfo::new(
            "self_wake",
            "builtin",
            "Schedule yourself to wake later and continue in THIS session — for \
             follow-ups like \"check the deploy in 10 minutes\" or \"remind me at \
             18:00\". Give a `prompt` (what to do on waking) and EITHER `after_secs` \
             (relative) OR `at` (ISO-8601 UTC). The woken run reloads this session's \
             context. Scheduling is free; whatever the woken run then does is gated \
             like any other action, so plan your own follow-ups freely.",
            // Control-plane only: it creates a schedule, no machine side effect of
            // its own. The woken run's tools are gated normally. See module docs.
            RiskClass::Read,
        )
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "What to do when you wake up."
                },
                "after_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Wake this many seconds from now (use this OR at)."
                },
                "at": {
                    "type": "string",
                    "description": "Absolute wake time, ISO-8601 UTC e.g. 2026-07-28T18:00:00Z (use this OR after_secs)."
                }
            },
            "required": ["prompt"],
            "additionalProperties": false
        })
    }

    async fn call(
        &self,
        ctx: &ToolContext,
        input: &Map<String, Value>,
        _cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        let prompt = input
            .get("prompt")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| ToolError::Invalid("prompt is required".to_owned()))?
            .to_owned();
        let ts = Self::wake_time(input)?;

        // Bound outstanding self-wakes so a loop can't flood the schedule table.
        // Fail-closed on a store error (don't create under uncertainty).
        let pending = self
            .store
            .list_schedules_lenient()
            .await
            .map_err(|e| ToolError::Failed(format!("could not read schedules: {e}")))?
            .into_iter()
            .filter(|s| s.name == SELF_WAKE_NAME && s.enabled && s.next_run_at.is_some())
            .count();
        if pending >= MAX_PENDING_WAKES {
            return Err(ToolError::Denied(format!(
                "you already have {pending} pending self-wakes (max {MAX_PENDING_WAKES}); \
                 cancel some before scheduling more"
            )));
        }

        let schedule = Schedule {
            id: format!("wake_{}", ulid()),
            name: SELF_WAKE_NAME.to_owned(),
            enabled: true,
            spec: ScheduleSpec::At { ts: ts.clone() },
            action: ScheduleAction::AgentRun {
                prompt,
                // Deliver into THIS session so the woken run continues the
                // conversation (its prior context is reloaded on run).
                session_id: ctx.session_id.clone(),
                model_override: None,
            },
            delivery: vec![],
            last_run_at: None,
            // One-shot: fire once at `ts`, then the scheduler clears this.
            next_run_at: Some(ts.clone()),
            consecutive_failures: 0,
        };
        self.store
            .upsert_schedule(&schedule)
            .await
            .map_err(|e| ToolError::Failed(format!("could not schedule the wake: {e}")))?;

        Ok(format!(
            "Scheduled a self-wake at {ts} (id {}). It will run in this session then.",
            schedule.id
        ))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn ctx(session: Option<&str>) -> ToolContext {
        ToolContext {
            run_id: "run_1".to_owned(),
            session_id: session.map(str::to_owned),
            schedule_id: None,
            tool_call_id: "tc_1".to_owned(),
        }
    }

    async fn tool() -> (SelfWakeTool, Store) {
        let store = Store::open_memory().await.unwrap();
        (SelfWakeTool::new(store.clone()), store)
    }

    #[tokio::test]
    async fn after_secs_creates_a_one_shot_schedule_in_the_session() {
        let (tool, store) = tool().await;
        let mut input = Map::new();
        input.insert("prompt".to_owned(), json!("check the build"));
        input.insert("after_secs".to_owned(), json!(600));
        let out = tool
            .call(&ctx(Some("sess_1")), &input, &CancellationToken::new())
            .await
            .unwrap();
        assert!(out.contains("Scheduled a self-wake"), "{out}");

        let schedules = store.list_schedules_lenient().await.unwrap();
        assert_eq!(schedules.len(), 1);
        let s = &schedules[0];
        assert_eq!(s.name, SELF_WAKE_NAME);
        assert!(s.enabled);
        assert!(matches!(s.spec, ScheduleSpec::At { .. }));
        assert!(s.next_run_at.is_some());
        match &s.action {
            ScheduleAction::AgentRun {
                prompt, session_id, ..
            } => {
                assert_eq!(prompt, "check the build");
                assert_eq!(session_id.as_deref(), Some("sess_1"));
            }
        }
    }

    #[tokio::test]
    async fn at_accepts_a_well_formed_utc_timestamp() {
        let (tool, store) = tool().await;
        let mut input = Map::new();
        input.insert("prompt".to_owned(), json!("nightly recap"));
        input.insert("at".to_owned(), json!("2026-07-28T18:00:00Z"));
        tool.call(&ctx(None), &input, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            store.list_schedules_lenient().await.unwrap()[0]
                .next_run_at
                .as_deref(),
            Some("2026-07-28T18:00:00Z")
        );
    }

    #[tokio::test]
    async fn a_malformed_at_is_rejected() {
        let (tool, _store) = tool().await;
        let mut input = Map::new();
        input.insert("prompt".to_owned(), json!("x"));
        input.insert("at".to_owned(), json!("next tuesday"));
        let err = tool
            .call(&ctx(None), &input, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Invalid(_)), "{err:?}");
    }

    #[tokio::test]
    async fn requires_a_time_and_a_prompt() {
        let (tool, _store) = tool().await;
        // No time.
        let mut only_prompt = Map::new();
        only_prompt.insert("prompt".to_owned(), json!("x"));
        assert!(matches!(
            tool.call(&ctx(None), &only_prompt, &CancellationToken::new())
                .await,
            Err(ToolError::Invalid(_))
        ));
        // No prompt.
        let mut only_time = Map::new();
        only_time.insert("after_secs".to_owned(), json!(60));
        assert!(matches!(
            tool.call(&ctx(None), &only_time, &CancellationToken::new())
                .await,
            Err(ToolError::Invalid(_))
        ));
        // Both after_secs and at.
        let mut both = Map::new();
        both.insert("prompt".to_owned(), json!("x"));
        both.insert("after_secs".to_owned(), json!(60));
        both.insert("at".to_owned(), json!("2026-07-28T18:00:00Z"));
        assert!(matches!(
            tool.call(&ctx(None), &both, &CancellationToken::new())
                .await,
            Err(ToolError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn pending_self_wakes_are_capped() {
        let (tool, _store) = tool().await;
        for i in 0..MAX_PENDING_WAKES {
            let mut input = Map::new();
            input.insert("prompt".to_owned(), json!(format!("wake {i}")));
            input.insert("after_secs".to_owned(), json!(3600));
            tool.call(&ctx(Some("s")), &input, &CancellationToken::new())
                .await
                .unwrap();
        }
        let mut over = Map::new();
        over.insert("prompt".to_owned(), json!("one too many"));
        over.insert("after_secs".to_owned(), json!(3600));
        let err = tool
            .call(&ctx(Some("s")), &over, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Denied(_)), "{err:?}");
    }

    #[test]
    fn iso8601_shape_check() {
        assert!(is_iso8601_utc("2026-07-28T18:00:00Z"));
        assert!(!is_iso8601_utc("2026-07-28T18:00:00")); // no Z
        assert!(!is_iso8601_utc("2026-07-28 18:00:00Z")); // space not T
        assert!(!is_iso8601_utc("not a time"));
        // Shape + digits pass the CHEAP screen even for an impossible date; the
        // scheduler's real parse is what rejects it. This documents that split.
        assert!(is_iso8601_utc("2026-13-99T99:99:99Z"));
    }
}
