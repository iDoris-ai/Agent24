use agent24_models::{Msg, ToolCallRequest};
use agent24_store::RunMessage;

use crate::{RECALL_END_MARKER, RECALL_PREFIX};

/// Rebuild the in-memory conversation from a persisted run thread (H3 resume).
/// The inverse of the agent loop's per-message persistence: a malformed
/// `tool_calls` value degrades to "no calls" rather than erroring — a row we
/// cannot parse must never resurrect as a phantom tool request.
pub(crate) fn thread_to_messages(thread: &[RunMessage]) -> Vec<Msg> {
    thread
        .iter()
        .map(|m| Msg {
            role: m.role.clone(),
            content: m.content.clone(),
            tool_calls: serde_json::from_value(m.tool_calls.clone()).unwrap_or_default(),
            tool_call_id: m.tool_call_id.clone(),
        })
        .collect()
}

/// Request-only message normalization (M1-T07.1 H1 fix). The persisted
/// snapshot (recall block, then the session's prior context, then this
/// turn's prompt) can legally contain `system` anywhere — a compaction
/// summary is itself injected as `system` — and, when there is no prior
/// context, two adjacent `user` turns (the recall block immediately followed
/// by this run's own prompt). Several HF-strict chat templates (the
/// Gemma/Mistral family) reject that shape outright ("roles must alternate"
/// / system must lead). This NEVER touches the persisted thread or the
/// snapshot fail-closed contract — it only reshapes what is SENT to the
/// provider on THIS call, rebuilt fresh from `messages` every iteration of
/// [`crate::RunManager::run_loop`]:
/// 1. every `system` message is pulled out and merged into ONE, placed first
///    (dropped entirely if there were none);
/// 2. the remaining messages keep their relative order; adjacent `user`
///    turns (now possibly newly adjacent, since removing an in-between
///    `system` message can create a fresh adjacency) are merged with `\n\n`.
pub fn normalize_for_provider(messages: &[Msg]) -> Vec<Msg> {
    let mut system_parts = Vec::new();
    let mut rest = Vec::with_capacity(messages.len());
    for msg in messages {
        if msg.role == "system" {
            if let Some(content) = &msg.content {
                system_parts.push(content.clone());
            }
        } else {
            rest.push(msg.clone());
        }
    }
    let mut normalized = Vec::with_capacity(rest.len() + 1);
    if !system_parts.is_empty() {
        normalized.push(Msg::system(system_parts.join("\n\n")));
    }
    for msg in rest {
        if msg.role == "user"
            && let Some(last) = normalized.last_mut()
            && last.role == "user"
        {
            last.content = Some(match (last.content.take(), msg.content) {
                (Some(a), Some(b)) => format!("{a}\n\n{b}"),
                (Some(a), None) => a,
                (None, Some(b)) => b,
                (None, None) => String::new(),
            });
            continue;
        }
        normalized.push(msg);
    }
    normalized
}

/// M1-T07.1 M4: a resumed thread's recall message was captured at the run's
/// FIRST model call and may now name an id the owner has since retracted or
/// superseded — resume must never resurrect a forgotten fact. Re-validates
/// every `id=` against `active` and keeps only the still-active lines,
/// rebuilding the block's header + end marker around them; drops the whole
/// message if nothing survives. A block this parser cannot make sense of
/// (missing the end marker, no `id=` tokens) is dropped outright — treated
/// as unverifiable, not trusted by default.
pub(crate) fn prune_stale_recall_message(
    msg: Msg,
    active: &std::collections::HashSet<String>,
) -> Option<Msg> {
    let Some(content) = msg.content.as_deref() else {
        return Some(msg);
    };
    if msg.role != "user" || !content.starts_with(RECALL_PREFIX) {
        return Some(msg);
    }
    // Round 2 ②: a message that starts with the header but does NOT end
    // with the marker is not a real recall block (it is left untouched,
    // not dropped) — a real one always has this exact shape; only a
    // malformed/foreign message fails this, and the fail-safe direction
    // for something we cannot parse as a recall block is to PRESERVE it,
    // never silently delete it.
    let Some(body) = content.strip_suffix(RECALL_END_MARKER) else {
        return Some(msg);
    };
    let mut parts = body.split("\n- [id=");
    let Some(header) = parts.next() else {
        return Some(msg);
    };
    let mut kept = String::from(header);
    let mut any = false;
    for entry in parts {
        let Some(space) = entry.find(' ') else {
            continue;
        };
        if active.contains(&entry[..space]) {
            kept.push_str("\n- [id=");
            kept.push_str(entry);
            any = true;
        }
    }
    if any {
        kept.push_str(RECALL_END_MARKER);
        Some(Msg::user(kept))
    } else {
        None
    }
}

/// Shape one message that is PROVEN (by its persisted origin) to be a
/// `HostRecall`. Unlike [`prune_stale_recall_message`], which deliberately
/// passes a malformed lookalike through for the old text-based decision, a
/// proven HostRecall that is not a well-formed recall block is dropped: it
/// cannot be re-validated, so it must not reach the provider.
pub(crate) fn shape_host_recall(
    msg: Msg,
    active: &std::collections::HashSet<String>,
) -> Option<Msg> {
    let content = msg.content.as_deref()?;
    if msg.role != "user" || !content.starts_with(RECALL_PREFIX) {
        return None;
    }
    let body = content.strip_suffix(RECALL_END_MARKER)?;
    let has_id = body
        .split("\n- [id=")
        .skip(1)
        .any(|entry| entry.find(' ').is_some_and(|space| space > 0));
    if !has_id {
        return None;
    }
    prune_stale_recall_message(msg, active)
}

/// The still-unanswered tool calls of the reconstructed thread's LAST
/// tool-calling assistant turn, in request order. `None` when there is no such
/// turn (an inconsistent resume — the caller aborts). Returns the full ordered
/// remainder, so a partially answered fan-out resumes EVERY leftover call, not
/// just the first — otherwise the model would be handed an assistant turn with
/// dangling unanswered tool_calls.
pub(crate) fn unanswered_calls_of_last_turn(messages: &[Msg]) -> Option<Vec<ToolCallRequest>> {
    let idx = messages
        .iter()
        .rposition(|m| m.role == "assistant" && !m.tool_calls.is_empty())?;
    let answered: std::collections::HashSet<&str> = messages[idx + 1..]
        .iter()
        .filter(|m| m.role == "tool")
        .filter_map(|m| m.tool_call_id.as_deref())
        .collect();
    Some(
        messages[idx]
            .tool_calls
            .iter()
            .filter(|c| !answered.contains(c.id.as_str()))
            .cloned()
            .collect(),
    )
}
