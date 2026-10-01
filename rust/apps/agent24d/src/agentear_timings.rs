//! ME4-desktop-model-ui follow-up — AgentEar per-step turn timing ingestion
//! (design ask: AgentEar PR #102 / agent-speaker v0.26.0).
//!
//! `events_emit.rs`'s doc comment is explicit: the kernel relays every
//! module's `_a24/events/emit` call verbatim and deliberately does NOT
//! understand any module's payload schema — that stays true here. This file
//! doesn't sit in the RPC path at all; it's a passive OBSERVER on the same
//! WS broadcast bus every connected client already receives
//! (`crate::events::EventsHub`), recognizing exactly ONE shape — AgentEar's
//! own `agentear.event/1` `turn` event, on `phase: idle|failed`, carrying a
//! `timings` object of per-step millisecond durations — and turning it into
//! `model_call_timings` rows, one per present `*_ms` field. This is a
//! narrow, explicitly-documented exception (same posture as the `module`
//! wire type's own "one declared exemption" in `agent24-protocol/events.rs`),
//! not a precedent for the kernel understanding module payloads generally.
//!
//! Never reads or stores transcript/prompt/reply text: only numbers, the
//! closed set of field names below, `session_id`/`seq` (opaque correlation
//! ids, not content), and `model`/`tier`/`llm_via`.
//!
//! Review M2: this bridge no longer writes `agent24-store` directly. It
//! builds [`crate::timing_recorder::TimingObservation`]s (the SAME type
//! `model_callback.rs`/`routes.rs` build) and hands them to the SAME
//! `Arc<dyn TimingSink>` writer — one channel, one writer task, so every
//! `model_call_timings` write is serialized through it, the retention prune
//! counter counts AgentEar's rows too, and this file has no `Store`/SQL
//! dependency of its own any more.

use agent24_protocol::EventBody;
use serde_json::{Map, Value};
use tokio::task::JoinHandle;

use crate::events::EventsHub;
use crate::model_callback::MODEL_MAX_MODEL_ID_BYTES;
use crate::timing_recorder::{TimingObservation, TimingSink};

pub const AGENTEAR_MODULE: &str = "agentear";
const AGENTEAR_EVENT_KIND: &str = "agentear.event";

/// Design ask's closed set of `*_ms` fields, each becoming its own row with
/// `step` = the field name minus the `_ms` suffix (e.g. `asr_ms` → `"asr"`).
/// An unknown field is ignored — consumers ignore unknown fields, same rule
/// AgentEar's own contract documents.
const TIMING_MS_FIELDS: &[&str] = &[
    "record_ms",
    "asr_ms",
    "llm_first_ms",
    "llm_ms",
    "tts_first_ms",
    "to_first_audio_ms",
    "play_ms",
    "total_ms",
];

fn str_field<'a>(m: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    m.get(key).and_then(Value::as_str)
}

fn u64_field(m: &Map<String, Value>, key: &str) -> Option<u64> {
    m.get(key).and_then(Value::as_u64)
}

/// Review M2: bounds a string field AgentEar reported (`model`/`llm_via`/
/// `session_id`) to [`MODEL_MAX_MODEL_ID_BYTES`] — the same ceiling
/// `model_callback.rs` already holds a provider-reported `model_id` to,
/// reused rather than inventing a second, possibly-disagreeing limit for
/// "an id string a module reported". An over-limit value is DROPPED
/// (`None`), never truncated — same reasoning as `model_callback.rs`'s own
/// `model_id` handling: a truncated id could name a turn/model/provider that
/// doesn't exist, which is worse than reporting nothing. Logs once per drop
/// (`field` names which one) — the closest thing to a counter this
/// diagnostic-only path needs; a metrics counter would be more machinery
/// than a rare malformed-event case justifies.
fn bounded(field: &'static str, value: Option<&str>) -> Option<String> {
    let v = value?;
    if v.len() <= MODEL_MAX_MODEL_ID_BYTES {
        return Some(v.to_owned());
    }
    tracing::warn!(
        field,
        len = v.len(),
        max = MODEL_MAX_MODEL_ID_BYTES,
        "agentear_timings: dropping an over-limit {field} rather than truncating it"
    );
    None
}

/// Pure extraction (no I/O, no clock) — zero rows unless `module`/`kind`
/// match AgentEar's own event exactly, `payload.type == "turn"`,
/// `payload.payload.phase` is `idle`/`failed`, and a `timings` object is
/// present. Builds [`TimingObservation`]s directly — the caller
/// (`spawn_agentear_timing_bridge`) only ever hands them to a
/// [`TimingSink`], never touches storage itself.
pub fn extract_agentear_timing_rows(
    module: &str,
    kind: &str,
    payload: &Map<String, Value>,
) -> Vec<TimingObservation> {
    if module != AGENTEAR_MODULE || kind != AGENTEAR_EVENT_KIND {
        return Vec::new();
    }
    if str_field(payload, "type") != Some("turn") {
        return Vec::new();
    }
    let Some(inner) = payload.get("payload").and_then(Value::as_object) else {
        return Vec::new();
    };
    // Only a turn's own idle/failed summary carries `timings` (design ask) —
    // every other phase (listening/thinking/speaking) is ignored here.
    let ok = match str_field(inner, "phase") {
        Some("idle") => true,
        Some("failed") => false,
        _ => return Vec::new(),
    };
    let Some(timings) = inner.get("timings").and_then(Value::as_object) else {
        return Vec::new();
    };

    let session_id = bounded("session_id", str_field(payload, "session_id"));
    let seq = u64_field(payload, "seq");
    let model = bounded("model", str_field(inner, "model"));
    let tier = str_field(inner, "tier")
        .filter(|t| *t == "local" || *t == "remote")
        .map(str::to_owned);
    let served_by = bounded("llm_via", str_field(inner, "llm_via"));
    let prompt_tokens = u64_field(inner, "prompt_tokens");
    let completion_tokens = u64_field(inner, "completion_tokens");

    TIMING_MS_FIELDS
        .iter()
        .filter_map(|field| {
            let ms = u64_field(timings, field)?;
            let step = field.strip_suffix("_ms").unwrap_or(field).to_owned();
            Some(TimingObservation {
                source: format!("module:{AGENTEAR_MODULE}"),
                model_id: model.clone(),
                tier: tier.clone(),
                served_by: served_by.clone(),
                ok,
                error_kind: None,
                step: Some(step),
                session_id: session_id.clone(),
                seq,
                first_token_ms: None,
                total_ms: ms,
                prompt_tokens,
                completion_tokens,
            })
        })
        .collect()
}

/// Subscribes to `events` and hands whatever `extract_agentear_timing_rows`
/// finds in each broadcast `EventBody::Module` event to `timings` — the SAME
/// sink `_a24/model/complete`/`/api/v1/chat` use, so writes are serialized
/// through one writer task and periodic pruning already covers these rows
/// too. Best-effort, same posture as `timing_recorder.rs`: a lagged/closed
/// bus just ends this task quietly.
pub fn spawn_agentear_timing_bridge(
    events: EventsHub,
    timings: std::sync::Arc<dyn TimingSink>,
) -> JoinHandle<()> {
    // Subscribed HERE, synchronously, before this function returns — not
    // inside the spawned task. `tokio::spawn` only SCHEDULES the task; if
    // the subscribe happened inside it, a caller that broadcasts right after
    // calling this function could race it (a `broadcast` channel only
    // delivers to receivers that already exist at send time, so a message
    // broadcast before the task gets to run its first line is silently
    // missed — exactly what the retry-loop in this file's own test caught).
    let mut rx = events.subscribe();
    tokio::spawn(async move {
        loop {
            let (_, body) = match rx.recv().await {
                Ok(v) => v,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            let EventBody::Module(m) = body else { continue };
            for obs in extract_agentear_timing_rows(&m.module, &m.kind, &m.payload) {
                timings.record(obs);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::timing_recorder::MemoryTimingSink;
    use serde_json::json;
    use std::sync::Arc;

    /// The design ask's own fixture shape: a `turn` event, `phase: "idle"`,
    /// every field it names present.
    fn idle_turn_fixture() -> Map<String, Value> {
        json!({
            "schema": "agentear.event/1",
            "event_id": "evt_1",
            "session_id": "ses_1",
            "seq": 7,
            "type": "turn",
            "payload": {
                "phase": "idle",
                "model": "Qwen3.6-35B-A3B-MLX-8bit",
                "tier": "local",
                "llm_via": "omlx",
                "asr_backend": "whisper-local",
                "prompt_tokens": 12,
                "completion_tokens": 8,
                "timings": {
                    "record_ms": 300,
                    "asr_ms": 150,
                    "llm_first_ms": 200,
                    "llm_ms": 900,
                    "tts_first_ms": 100,
                    "to_first_audio_ms": 1400,
                    "play_ms": 2000,
                    "total_ms": 3500
                }
            }
        })
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn an_idle_turn_with_timings_yields_one_row_per_ms_field() {
        let rows =
            extract_agentear_timing_rows(AGENTEAR_MODULE, "agentear.event", &idle_turn_fixture());
        assert_eq!(rows.len(), 8, "one row per *_ms field in the fixture");

        let steps: std::collections::BTreeSet<&str> =
            rows.iter().map(|r| r.step.as_deref().unwrap()).collect();
        assert_eq!(
            steps,
            [
                "record",
                "asr",
                "llm_first",
                "llm",
                "tts_first",
                "to_first_audio",
                "play",
                "total",
            ]
            .into_iter()
            .collect()
        );

        let total = rows
            .iter()
            .find(|r| r.step.as_deref() == Some("total"))
            .unwrap();
        assert_eq!(total.total_ms, 3500);
        assert!(total.ok);
        assert_eq!(total.source, "module:agentear");
        assert_eq!(total.model_id.as_deref(), Some("Qwen3.6-35B-A3B-MLX-8bit"));
        assert_eq!(total.tier.as_deref(), Some("local"));
        assert_eq!(total.served_by.as_deref(), Some("omlx"));
        assert_eq!(total.session_id.as_deref(), Some("ses_1"));
        assert_eq!(total.seq, Some(7));
        assert_eq!(total.prompt_tokens, Some(12));
        assert_eq!(total.completion_tokens, Some(8));
    }

    #[test]
    fn a_failed_turn_yields_rows_marked_not_ok() {
        let mut fixture = idle_turn_fixture();
        let inner = fixture.get_mut("payload").unwrap().as_object_mut().unwrap();
        inner.insert("phase".to_owned(), json!("failed"));
        let rows = extract_agentear_timing_rows(AGENTEAR_MODULE, "agentear.event", &fixture);
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|r| !r.ok));
    }

    #[test]
    fn llm_first_ms_absent_is_simply_skipped_not_a_zero_row() {
        // "llm_first_ms（附着时没有）" — a turn attached mid-session may omit it.
        let mut fixture = idle_turn_fixture();
        let inner = fixture.get_mut("payload").unwrap().as_object_mut().unwrap();
        inner
            .get_mut("timings")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("llm_first_ms");
        let rows = extract_agentear_timing_rows(AGENTEAR_MODULE, "agentear.event", &fixture);
        assert_eq!(rows.len(), 7);
        assert!(rows.iter().all(|r| r.step.as_deref() != Some("llm_first")));
    }

    #[test]
    fn non_idle_non_failed_phases_yield_nothing() {
        for phase in ["listening", "thinking", "speaking"] {
            let mut fixture = idle_turn_fixture();
            let inner = fixture.get_mut("payload").unwrap().as_object_mut().unwrap();
            inner.insert("phase".to_owned(), json!(phase));
            assert!(
                extract_agentear_timing_rows(AGENTEAR_MODULE, "agentear.event", &fixture)
                    .is_empty(),
                "phase {phase} must never be treated as a turn summary"
            );
        }
    }

    #[test]
    fn a_turn_with_no_timings_object_yields_nothing() {
        let mut fixture = idle_turn_fixture();
        fixture
            .get_mut("payload")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("timings");
        assert!(
            extract_agentear_timing_rows(AGENTEAR_MODULE, "agentear.event", &fixture).is_empty()
        );
    }

    #[test]
    fn a_different_module_or_kind_or_event_type_never_matches() {
        let fixture = idle_turn_fixture();
        assert!(extract_agentear_timing_rows("sin90", "agentear.event", &fixture).is_empty());
        assert!(
            extract_agentear_timing_rows(AGENTEAR_MODULE, "agentear.debug", &fixture).is_empty()
        );

        let mut wrong_type = fixture.clone();
        wrong_type.insert("type".to_owned(), json!("transcript"));
        assert!(
            extract_agentear_timing_rows(AGENTEAR_MODULE, "agentear.event", &wrong_type).is_empty()
        );
    }

    // ── review M2: length caps (reusing MODEL_MAX_MODEL_ID_BYTES) ───────────

    #[test]
    fn an_over_limit_model_session_id_or_llm_via_is_dropped_not_truncated() {
        let mut fixture = idle_turn_fixture();
        let too_long = "x".repeat(MODEL_MAX_MODEL_ID_BYTES + 1);
        fixture.insert("session_id".to_owned(), json!(too_long.clone()));
        let inner = fixture.get_mut("payload").unwrap().as_object_mut().unwrap();
        inner.insert("model".to_owned(), json!(too_long.clone()));
        inner.insert("llm_via".to_owned(), json!(too_long));

        let rows = extract_agentear_timing_rows(AGENTEAR_MODULE, "agentear.event", &fixture);
        assert!(!rows.is_empty());
        for r in &rows {
            assert!(
                r.session_id.is_none(),
                "over-limit session_id must be dropped"
            );
            assert!(r.model_id.is_none(), "over-limit model must be dropped");
            assert!(r.served_by.is_none(), "over-limit llm_via must be dropped");
        }
    }

    #[test]
    fn a_model_session_id_or_llm_via_exactly_at_the_limit_is_kept() {
        let mut fixture = idle_turn_fixture();
        let exactly = "x".repeat(MODEL_MAX_MODEL_ID_BYTES);
        fixture.insert("session_id".to_owned(), json!(exactly.clone()));
        let inner = fixture.get_mut("payload").unwrap().as_object_mut().unwrap();
        inner.insert("model".to_owned(), json!(exactly.clone()));

        let rows = extract_agentear_timing_rows(AGENTEAR_MODULE, "agentear.event", &fixture);
        assert!(
            rows.iter()
                .all(|r| r.session_id.as_deref() == Some(exactly.as_str()))
        );
        assert!(
            rows.iter()
                .all(|r| r.model_id.as_deref() == Some(exactly.as_str()))
        );
    }

    // ── review M2: the bridge now records through the SAME TimingSink ──────

    #[tokio::test]
    async fn the_bridge_records_through_the_shared_timing_sink_for_a_broadcast_idle_turn_event() {
        let sink = Arc::new(MemoryTimingSink::default());
        let hub = EventsHub::default();
        let _handle =
            spawn_agentear_timing_bridge(hub.clone(), sink.clone() as Arc<dyn TimingSink>);

        hub.broadcast(EventBody::Module(agent24_protocol::ModuleEventPayload {
            module: AGENTEAR_MODULE.to_owned(),
            kind: AGENTEAR_EVENT_KIND.to_owned(),
            payload: idle_turn_fixture(),
        }));

        let mut recorded = Vec::new();
        for _ in 0..50 {
            recorded = sink.take();
            if recorded.len() >= 8 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(recorded.len(), 8);
        assert!(recorded.iter().all(|o| o.source == "module:agentear"));
    }
}
