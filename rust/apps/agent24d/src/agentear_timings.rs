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

use agent24_protocol::EventBody;
use agent24_store::{NewCallTiming, Store};
use serde_json::{Map, Value};
use tokio::task::JoinHandle;

use crate::events::EventsHub;

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

/// Pure extraction (no I/O, no clock) — zero rows unless `module`/`kind`
/// match AgentEar's own event exactly, `payload.type == "turn"`,
/// `payload.payload.phase` is `idle`/`failed`, and a `timings` object is
/// present. `ts` on every returned row is `String::new()` — the caller
/// (`spawn_agentear_timing_bridge`) fills it in at write time, the same
/// "recorder assigns the clock" split `timing_recorder.rs` already uses.
pub fn extract_agentear_timing_rows(
    module: &str,
    kind: &str,
    payload: &Map<String, Value>,
) -> Vec<NewCallTiming> {
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

    let session_id = str_field(payload, "session_id").map(str::to_owned);
    let seq = u64_field(payload, "seq");
    let model = str_field(inner, "model").map(str::to_owned);
    let tier = str_field(inner, "tier")
        .filter(|t| *t == "local" || *t == "remote")
        .map(str::to_owned);
    let served_by = str_field(inner, "llm_via").map(str::to_owned);
    let prompt_tokens = u64_field(inner, "prompt_tokens");
    let completion_tokens = u64_field(inner, "completion_tokens");

    TIMING_MS_FIELDS
        .iter()
        .filter_map(|field| {
            let ms = u64_field(timings, field)?;
            let step = field.strip_suffix("_ms").unwrap_or(field).to_owned();
            Some(NewCallTiming {
                ts: String::new(),
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

/// Subscribes to `events` and writes whatever `extract_agentear_timing_rows`
/// finds in each broadcast `EventBody::Module` event to `store`. Best-effort,
/// same posture as `timing_recorder.rs`: a lagged/closed bus just ends this
/// task quietly, and a write failure is logged, never propagated (nothing
/// awaits this task's result).
pub fn spawn_agentear_timing_bridge(events: EventsHub, store: Store) -> JoinHandle<()> {
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
            let rows = extract_agentear_timing_rows(&m.module, &m.kind, &m.payload);
            for mut row in rows {
                row.ts = agent24_core::util::now_iso8601();
                if let Err(e) = store.record_call_timing(&row).await {
                    tracing::warn!("agentear_timings: write failed: {e}");
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::json;

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

    #[tokio::test]
    async fn the_bridge_writes_rows_for_a_broadcast_idle_turn_event() {
        let store = Store::open_memory().await.unwrap();
        let hub = EventsHub::default();
        let _handle = spawn_agentear_timing_bridge(hub.clone(), store.clone());

        hub.broadcast(EventBody::Module(agent24_protocol::ModuleEventPayload {
            module: AGENTEAR_MODULE.to_owned(),
            kind: AGENTEAR_EVENT_KIND.to_owned(),
            payload: idle_turn_fixture(),
        }));

        let mut rows = Vec::new();
        for _ in 0..50 {
            rows = store
                .query_call_timings(Some("module:agentear"), None, 20)
                .await
                .unwrap();
            if rows.len() >= 8 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(rows.len(), 8);
    }
}
