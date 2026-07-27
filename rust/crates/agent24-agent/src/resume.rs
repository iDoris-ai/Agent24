//! Resume-point analysis for durable crash recovery (H3).
//!
//! When the daemon dies while a run is parked on a human approval, the process
//! and its in-memory loop state are gone — but the run's conversation was
//! persisted turn by turn (PR-1, `run_messages`). This module is the pure
//! function that reads that persisted thread plus the run's last known
//! `RunStatus` and decides whether the run can be resumed and, if so, exactly
//! where it was suspended.
//!
//! It deliberately owns NO I/O and NO policy: it is a decision over data, so it
//! can be exhaustively unit-tested without a daemon, a store, or a clock. The
//! startup path joins its verdict with the live approval row and the tool
//! registry (staleness re-validation) before acting on it.
//!
//! ## Why status, not shape alone
//!
//! A run that the user *cancelled* mid-tool and a run that *died awaiting
//! approval* leave an identical on-disk thread: an assistant turn whose
//! trailing `tool_call` has no answering `tool` message. The shape cannot tell
//! them apart — only `RunStatus` can (`cancelled` vs `awaiting_approval`). So
//! the status gate comes first and is load-bearing, not a formality.

use agent24_protocol::RunStatus;
use agent24_store::RunMessage;
use serde_json::{Map, Value};

/// The verdict of [`plan_resume`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumePlan {
    /// The run cannot be resumed — it is terminal, was never parked on a human,
    /// or its thread is inconsistent with an awaiting-approval state. The caller
    /// lands it aborted (fail-closed): losing an unrecoverable run is safe,
    /// silently replaying one is not.
    NotResumable { reason: String },
    /// The run was suspended waiting for approval of exactly this tool call. The
    /// caller re-issues the approval (as a durable inbox item) and, once it is
    /// answered, continues the loop from the persisted thread.
    AwaitingToolApproval {
        /// The assistant turn (by `seq`) that requested the unanswered call.
        assistant_seq: i64,
        /// The tool_call still lacking an answering `tool` message. When a turn
        /// fanned out several calls and only some were answered before the
        /// crash, this is the FIRST still-unanswered one.
        tool_call_id: String,
    },
}

impl ResumePlan {
    fn not_resumable(reason: impl Into<String>) -> Self {
        ResumePlan::NotResumable {
            reason: reason.into(),
        }
    }
}

/// Extract the tool_call ids requested by an assistant message row. Empty for
/// any non-assistant row, a text-only assistant turn, or a malformed
/// `tool_calls` value (treated as "no calls" rather than erroring — a row we
/// cannot parse must not be mistaken for a resumable suspension point).
fn requested_call_ids(msg: &RunMessage) -> Vec<String> {
    if msg.role != "assistant" {
        return Vec::new();
    }
    let Some(calls) = msg.tool_calls.as_array() else {
        return Vec::new();
    };
    calls
        .iter()
        .filter_map(|c| c.get("id").and_then(|v| v.as_str()).map(str::to_owned))
        .collect()
}

/// Decide how (or whether) to resume a run from its persisted thread.
///
/// `thread` must be in append order (as `list_run_messages` returns it).
///
/// Resumable iff ALL hold:
/// 1. `status == AwaitingApproval` — the only state in which a run is genuinely
///    parked on a human. Terminal states and bare `Running`/`Queued` are not
///    (a `Running` crash left no pending human; the orphan-run sweep lands it).
/// 2. the thread's LAST tool-calling assistant turn has at least one call with
///    no answering `tool` message after it.
///
/// Anything else is `NotResumable` with a reason (the caller aborts + logs).
pub fn plan_resume(status: RunStatus, thread: &[RunMessage]) -> ResumePlan {
    // (1) Status gate — this is what distinguishes a cancelled run from one that
    // died awaiting approval; the two are indistinguishable by thread shape.
    if status != RunStatus::AwaitingApproval {
        return ResumePlan::not_resumable(format!(
            "run status is {status:?}, not awaiting_approval — nothing was parked on a human"
        ));
    }

    // (2) Find the LAST assistant turn that requested tool calls. Resume always
    // concerns the most recent suspension point; earlier answered turns are
    // history the reconstructed loop will replay from the thread.
    let Some((idx, assistant)) = thread
        .iter()
        .enumerate()
        .rev()
        .find(|(_, m)| !requested_call_ids(m).is_empty())
    else {
        return ResumePlan::not_resumable(
            "awaiting_approval but no tool-calling assistant turn on disk (inconsistent thread)",
        );
    };
    let requested = requested_call_ids(assistant);

    // Which of that turn's calls already have an answering `tool` message AFTER
    // it? A turn can fan out several calls and be answered partially before a
    // crash (call[0] ran, call[1] was the one awaiting approval).
    let answered: std::collections::HashSet<&str> = thread[idx + 1..]
        .iter()
        .filter(|m| m.role == "tool")
        .filter_map(|m| m.tool_call_id.as_deref())
        .collect();

    match requested.iter().find(|id| !answered.contains(id.as_str())) {
        Some(unanswered) => ResumePlan::AwaitingToolApproval {
            assistant_seq: assistant.seq,
            tool_call_id: unanswered.clone(),
        },
        // Every requested call already has an answer, yet the run is parked
        // awaiting approval: the thread and the status disagree. Fail closed —
        // resuming here would re-run an already-answered call.
        None => ResumePlan::not_resumable(
            "awaiting_approval but every tool_call in the last turn is already answered \
             (thread/status disagree)",
        ),
    }
}

/// The verdict of [`assess_restore`]: whether a parked approval survives the
/// restart re-validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreDecision {
    /// Re-issue this parked approval as a durable inbox item and continue the
    /// run from its persisted thread once a human answers.
    Restore { tool_call_id: String },
    /// The parked approval cannot be safely restored; the caller aborts it
    /// (fail-closed) and lands the run cancelled.
    Abort { reason: String },
}

/// The (name, raw arguments) of the tool call with `id` in the thread's most
/// recent assistant turn that requested it.
fn find_requested_call<'a>(
    thread: &'a [RunMessage],
    tool_call_id: &str,
) -> Option<(&'a str, &'a str)> {
    for m in thread.iter().rev() {
        if m.role != "assistant" {
            continue;
        }
        let Some(calls) = m.tool_calls.as_array() else {
            continue;
        };
        for c in calls {
            if c.get("id").and_then(Value::as_str) == Some(tool_call_id) {
                let name = c.get("name").and_then(Value::as_str).unwrap_or_default();
                let args = c
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                return Some((name, args));
            }
        }
    }
    None
}

/// Re-validate a parked approval on restart before re-issuing it (H3 staleness
/// re-validation). The persisted hash/shape proves the payload did not change,
/// but NOT that the world did not — so this layers three live checks on top of
/// [`plan_resume`], and ANY failure aborts (fail-closed):
///
/// 1. the analyzer independently agrees the run is parked on THIS exact call;
/// 2. the tool that call names still exists in the live registry (`tool_exists`)
///    — a tool uninstalled or an MCP server gone offline while down can't run;
/// 3. the input rebuilt from the thread still equals what was approved — the
///    payload-integrity property ("refuse to run B for an approval of A");
/// 4. the approval is not older than the resume TTL (`created_at >= ttl_cutoff`;
///    both fixed-width UTC, so the lexical compare is chronological).
///
/// Pure over its inputs: the caller derives `tool_exists` from the registry and
/// `ttl_cutoff` from the clock ([`agent24_core::util::iso8601_before`]).
pub fn assess_restore(
    tool_call_id: &str,
    approved_payload: &Map<String, Value>,
    created_at: &str,
    run_status: RunStatus,
    thread: &[RunMessage],
    tool_exists: impl Fn(&str) -> bool,
    ttl_cutoff: &str,
) -> RestoreDecision {
    match plan_resume(run_status, thread) {
        ResumePlan::AwaitingToolApproval {
            tool_call_id: point,
            ..
        } if point == tool_call_id => {}
        ResumePlan::AwaitingToolApproval {
            tool_call_id: point,
            ..
        } => {
            return RestoreDecision::Abort {
                reason: format!(
                    "approval parked on {tool_call_id} but the thread's suspension point is {point}"
                ),
            };
        }
        ResumePlan::NotResumable { reason } => return RestoreDecision::Abort { reason },
    }

    let Some((name, args)) = find_requested_call(thread, tool_call_id) else {
        return RestoreDecision::Abort {
            reason: format!("tool_call {tool_call_id} not found in the persisted thread"),
        };
    };
    if !tool_exists(name) {
        return RestoreDecision::Abort {
            reason: format!("tool `{name}` is no longer registered — cannot resume its call"),
        };
    }

    // Payload integrity: rebuild the input the same way the loop parsed it, and
    // require it to equal what was approved. A parse failure or any difference
    // fails closed — the whole point is to never run B for an approval of A.
    let rebuilt: Map<String, Value> = if args.trim().is_empty() {
        Map::new()
    } else {
        match serde_json::from_str::<Value>(args) {
            Ok(Value::Object(m)) => m,
            _ => {
                return RestoreDecision::Abort {
                    reason: format!(
                        "tool_call {tool_call_id} arguments no longer parse as an object"
                    ),
                };
            }
        }
    };
    if &rebuilt != approved_payload {
        return RestoreDecision::Abort {
            reason: format!(
                "tool_call {tool_call_id} input changed since it was approved \
                 (refusing to run B for an approval of A)"
            ),
        };
    }

    if created_at < ttl_cutoff {
        return RestoreDecision::Abort {
            reason: format!(
                "approval queued at {created_at} is older than the resume TTL (cutoff {ttl_cutoff})"
            ),
        };
    }

    RestoreDecision::Restore {
        tool_call_id: tool_call_id.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::json;

    fn user(seq: i64, text: &str) -> RunMessage {
        RunMessage {
            run_id: "run_1".to_owned(),
            seq,
            role: "user".to_owned(),
            content: Some(text.to_owned()),
            tool_calls: json!([]),
            tool_call_id: None,
            created_at: "t".to_owned(),
        }
    }

    /// An assistant turn requesting one or more tool calls (by id).
    fn assistant_calls(seq: i64, ids: &[&str]) -> RunMessage {
        let calls: Vec<_> = ids
            .iter()
            .map(|id| json!({ "id": id, "name": "shell_exec", "arguments": "{}" }))
            .collect();
        RunMessage {
            run_id: "run_1".to_owned(),
            seq,
            role: "assistant".to_owned(),
            content: None,
            tool_calls: json!(calls),
            tool_call_id: None,
            created_at: "t".to_owned(),
        }
    }

    fn assistant_text(seq: i64, text: &str) -> RunMessage {
        RunMessage {
            run_id: "run_1".to_owned(),
            seq,
            role: "assistant".to_owned(),
            content: Some(text.to_owned()),
            tool_calls: json!([]),
            tool_call_id: None,
            created_at: "t".to_owned(),
        }
    }

    fn tool_answer(seq: i64, call_id: &str) -> RunMessage {
        RunMessage {
            run_id: "run_1".to_owned(),
            seq,
            role: "tool".to_owned(),
            content: Some("ok".to_owned()),
            tool_calls: json!([]),
            tool_call_id: Some(call_id.to_owned()),
            created_at: "t".to_owned(),
        }
    }

    #[test]
    fn a_completed_run_is_not_resumable() {
        let thread = [user(0, "go"), assistant_text(1, "done")];
        assert!(matches!(
            plan_resume(RunStatus::Completed, &thread),
            ResumePlan::NotResumable { .. }
        ));
    }

    /// The load-bearing disambiguation: a cancelled run and a run that died
    /// awaiting approval leave the SAME on-disk shape (trailing unanswered
    /// call). Only status separates them, and cancelled must NOT resume.
    #[test]
    fn a_cancelled_run_with_a_trailing_call_is_not_resumable() {
        let thread = [user(0, "go"), assistant_calls(1, &["call_a"])];
        assert!(
            matches!(
                plan_resume(RunStatus::Cancelled, &thread),
                ResumePlan::NotResumable { .. }
            ),
            "identical shape to an awaiting-approval run — status must gate it"
        );
    }

    #[test]
    fn awaiting_approval_with_one_trailing_call_resumes_at_that_call() {
        let thread = [user(0, "go"), assistant_calls(1, &["call_a"])];
        assert_eq!(
            plan_resume(RunStatus::AwaitingApproval, &thread),
            ResumePlan::AwaitingToolApproval {
                assistant_seq: 1,
                tool_call_id: "call_a".to_owned(),
            }
        );
    }

    /// Partial-turn answering: the assistant fanned out two calls, the first ran
    /// before the crash, the second was the one awaiting approval. Resume must
    /// target the SECOND, not the first (which already has an answer).
    #[test]
    fn a_partially_answered_turn_resumes_at_the_first_unanswered_call() {
        let thread = [
            user(0, "go"),
            assistant_calls(1, &["call_a", "call_b"]),
            tool_answer(2, "call_a"),
        ];
        assert_eq!(
            plan_resume(RunStatus::AwaitingApproval, &thread),
            ResumePlan::AwaitingToolApproval {
                assistant_seq: 1,
                tool_call_id: "call_b".to_owned(),
            }
        );
    }

    #[test]
    fn awaiting_approval_but_every_call_answered_is_not_resumable() {
        // Thread says the turn is fully answered, status says still parked — a
        // contradiction. Fail closed rather than re-run an answered call.
        let thread = [
            user(0, "go"),
            assistant_calls(1, &["call_a"]),
            tool_answer(2, "call_a"),
        ];
        assert!(matches!(
            plan_resume(RunStatus::AwaitingApproval, &thread),
            ResumePlan::NotResumable { .. }
        ));
    }

    #[test]
    fn awaiting_approval_with_no_tool_turn_is_not_resumable() {
        // Parked awaiting approval but the thread never recorded a tool-calling
        // turn — inconsistent, cannot locate a suspension point.
        let thread = [user(0, "go")];
        assert!(matches!(
            plan_resume(RunStatus::AwaitingApproval, &thread),
            ResumePlan::NotResumable { .. }
        ));
    }

    /// Resume concerns the MOST RECENT suspension point: an earlier fully
    /// answered tool turn must not shadow a later unanswered one.
    #[test]
    fn resume_targets_the_last_tool_turn_not_an_earlier_answered_one() {
        let thread = [
            user(0, "go"),
            assistant_calls(1, &["call_a"]),
            tool_answer(2, "call_a"),
            assistant_calls(3, &["call_b"]),
        ];
        assert_eq!(
            plan_resume(RunStatus::AwaitingApproval, &thread),
            ResumePlan::AwaitingToolApproval {
                assistant_seq: 3,
                tool_call_id: "call_b".to_owned(),
            }
        );
    }

    #[test]
    fn a_malformed_tool_calls_value_is_treated_as_no_calls() {
        // A row whose tool_calls cannot be read as an id-bearing array must not
        // be mistaken for a resumable suspension point.
        let mut bad = assistant_calls(1, &["call_a"]);
        bad.tool_calls = json!("not-an-array");
        let thread = [user(0, "go"), bad];
        assert!(matches!(
            plan_resume(RunStatus::AwaitingApproval, &thread),
            ResumePlan::NotResumable { .. }
        ));
    }

    // ── assess_restore: staleness re-validation ──────────────────────────────

    /// An assistant turn requesting one call with an explicit name + arguments.
    fn assistant_call_full(seq: i64, id: &str, name: &str, args: &str) -> RunMessage {
        RunMessage {
            run_id: "run_1".to_owned(),
            seq,
            role: "assistant".to_owned(),
            content: None,
            tool_calls: json!([{ "id": id, "name": name, "arguments": args }]),
            tool_call_id: None,
            created_at: "t".to_owned(),
        }
    }

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    const CUTOFF: &str = "2026-07-26T00:00:00Z";
    const FRESH: &str = "2026-07-27T00:00:00Z"; // >= CUTOFF
    const STALE: &str = "2026-07-25T00:00:00Z"; // <  CUTOFF

    #[test]
    fn a_valid_parked_approval_restores() {
        let thread = [
            user(0, "go"),
            assistant_call_full(1, "call_a", "shell_exec", "{}"),
        ];
        assert_eq!(
            assess_restore(
                "call_a",
                &Map::new(),
                FRESH,
                RunStatus::AwaitingApproval,
                &thread,
                |_| true,
                CUTOFF,
            ),
            RestoreDecision::Restore {
                tool_call_id: "call_a".to_owned()
            }
        );
    }

    #[test]
    fn empty_arguments_match_an_empty_approved_payload() {
        let thread = [
            user(0, "go"),
            assistant_call_full(1, "call_a", "fs_write", ""),
        ];
        assert_eq!(
            assess_restore(
                "call_a",
                &Map::new(),
                FRESH,
                RunStatus::AwaitingApproval,
                &thread,
                |_| true,
                CUTOFF,
            ),
            RestoreDecision::Restore {
                tool_call_id: "call_a".to_owned()
            }
        );
    }

    #[test]
    fn a_tool_that_no_longer_exists_aborts() {
        let thread = [
            user(0, "go"),
            assistant_call_full(1, "call_a", "mcp_slack_post", "{}"),
        ];
        // Registry no longer has this tool (e.g. its MCP server went offline).
        let decision = assess_restore(
            "call_a",
            &Map::new(),
            FRESH,
            RunStatus::AwaitingApproval,
            &thread,
            |name| name != "mcp_slack_post",
            CUTOFF,
        );
        assert!(
            matches!(decision, RestoreDecision::Abort { ref reason } if reason.contains("no longer registered")),
            "{decision:?}"
        );
    }

    #[test]
    fn a_payload_changed_since_approval_aborts() {
        // Approved {"a":2}; the thread's call now rebuilds to {"a":1}. Refuse.
        let thread = [
            user(0, "go"),
            assistant_call_full(1, "call_a", "shell_exec", r#"{"a":1}"#),
        ];
        let decision = assess_restore(
            "call_a",
            &obj(json!({ "a": 2 })),
            FRESH,
            RunStatus::AwaitingApproval,
            &thread,
            |_| true,
            CUTOFF,
        );
        assert!(
            matches!(decision, RestoreDecision::Abort { ref reason } if reason.contains("changed since it was approved")),
            "{decision:?}"
        );
    }

    #[test]
    fn an_approval_older_than_the_ttl_aborts() {
        let thread = [
            user(0, "go"),
            assistant_call_full(1, "call_a", "shell_exec", "{}"),
        ];
        let decision = assess_restore(
            "call_a",
            &Map::new(),
            STALE,
            RunStatus::AwaitingApproval,
            &thread,
            |_| true,
            CUTOFF,
        );
        assert!(
            matches!(decision, RestoreDecision::Abort { ref reason } if reason.contains("resume TTL")),
            "{decision:?}"
        );
    }

    #[test]
    fn a_non_awaiting_status_aborts_via_the_analyzer() {
        let thread = [
            user(0, "go"),
            assistant_call_full(1, "call_a", "shell_exec", "{}"),
        ];
        assert!(matches!(
            assess_restore(
                "call_a",
                &Map::new(),
                FRESH,
                RunStatus::Cancelled,
                &thread,
                |_| true,
                CUTOFF,
            ),
            RestoreDecision::Abort { .. }
        ));
    }

    #[test]
    fn an_approval_pointing_at_a_different_call_than_the_suspension_point_aborts() {
        // The approval claims call_x, but the thread's actual unanswered call is
        // call_a. The two must agree, or we could resume the wrong call.
        let thread = [
            user(0, "go"),
            assistant_call_full(1, "call_a", "shell_exec", "{}"),
        ];
        let decision = assess_restore(
            "call_x",
            &Map::new(),
            FRESH,
            RunStatus::AwaitingApproval,
            &thread,
            |_| true,
            CUTOFF,
        );
        assert!(
            matches!(decision, RestoreDecision::Abort { ref reason } if reason.contains("suspension point")),
            "{decision:?}"
        );
    }
}
