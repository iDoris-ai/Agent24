//! Canonical session — a conversation that stays a BOUNDED prompt no matter
//! how long the chat runs.
//!
//! As messages accumulate, once the verbatim tail exceeds `max_recent` the
//! oldest messages are folded into a running LLM summary (via the
//! [`Summarizer`] trait — real impl uses the model gateway; tests use a mock),
//! keeping only the most recent `keep_recent` turns verbatim. The context fed
//! to the model is therefore always: `[system: prior summary] + recent tail`.

use agent24_models::Msg;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{KvStore, MemoryError, Result};

/// Folds a run of older messages into a compact natural-language summary.
/// `prior` is the existing summary (if any) so summaries compose rather than
/// lose history.
#[async_trait]
pub trait Summarizer: Send + Sync {
    async fn summarize(
        &self,
        prior: Option<&str>,
        messages: &[Msg],
    ) -> std::result::Result<String, String>;
}

/// Compaction policy. `max_recent` is the trigger; once the verbatim tail
/// exceeds it, everything older than the last `keep_recent` is compacted.
#[derive(Debug, Clone, Copy)]
pub struct CompactionPolicy {
    pub max_recent: usize,
    pub keep_recent: usize,
    /// Hard cap on the rolling summary length (chars). A misbehaving
    /// summarizer that keeps appending can't grow the prompt/KV row without
    /// bound — the summary is truncated to this after each fold (review D1).
    pub max_summary_chars: usize,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        // Compact once 40 verbatim messages pile up, keeping the last 20.
        Self {
            max_recent: 40,
            keep_recent: 20,
            max_summary_chars: 4000,
        }
    }
}

impl CompactionPolicy {
    /// Guard against a nonsensical policy (keep_recent >= max_recent would
    /// never converge). Clamps keep_recent below max_recent.
    fn normalized(self) -> Self {
        let max_recent = self.max_recent.max(1);
        let keep_recent = self.keep_recent.min(max_recent.saturating_sub(1));
        Self {
            max_recent,
            keep_recent,
            max_summary_chars: self.max_summary_chars.max(1),
        }
    }
}

/// Truncate a summary to at most `max` chars on a char boundary, marking it.
fn cap_summary(summary: String, max: usize) -> String {
    if summary.chars().count() <= max {
        return summary;
    }
    let kept: String = summary.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// A session's compacted conversation state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CanonicalSession {
    pub session_id: String,
    /// Rolling summary of everything older than `recent`; None until the first
    /// compaction.
    pub summary: Option<String>,
    /// The most recent messages, kept verbatim.
    pub recent: Vec<Msg>,
    /// How many messages have been folded into `summary` (for observability).
    pub compacted_count: usize,
}

impl CanonicalSession {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            summary: None,
            recent: Vec::new(),
            compacted_count: 0,
        }
    }

    /// The bounded context to feed the model: the prior summary as a system
    /// message (when present) followed by the verbatim recent tail.
    pub fn context(&self) -> Vec<Msg> {
        let mut out = Vec::with_capacity(self.recent.len() + 1);
        if let Some(summary) = &self.summary {
            out.push(Msg {
                role: "system".to_owned(),
                content: Some(format!("Summary of earlier conversation:\n{summary}")),
                tool_calls: vec![],
                tool_call_id: None,
            });
        }
        out.extend(self.recent.iter().cloned());
        out
    }

    /// Append a message, compacting if the tail grew past the policy's trigger.
    /// Compaction summarizes the oldest overflow messages into `summary`
    /// (folding in any prior summary) and drops them from `recent`.
    ///
    /// No-loss guarantee: the fold is computed WITHOUT mutating `recent`, the
    /// summarizer is awaited, and only on success are the older messages
    /// dropped and the summary committed. If the summarizer errors, `recent`
    /// is left intact (the appended message included) and the next append
    /// retries compaction — no message is ever lost to a failed summary
    /// (review D1).
    pub async fn append(
        &mut self,
        msg: Msg,
        policy: CompactionPolicy,
        summarizer: &dyn Summarizer,
    ) -> Result<()> {
        self.recent.push(msg);
        let policy = policy.normalized();
        if self.recent.len() <= policy.max_recent {
            return Ok(());
        }
        // Everything except the last `keep_recent` folds into the summary.
        let mut overflow = self.recent.len() - policy.keep_recent;
        // A `role: "tool"` result must never be split from the assistant
        // `tool_calls` turn it answers — a kept tail starting with an orphaned
        // tool message is an invalid conversation every OpenAI-compatible
        // provider rejects (review D1). Advance the boundary to fold any
        // leading tool-result messages of the kept tail together with their
        // (already-folded) assistant turn, until the tail starts on a
        // non-tool message.
        while overflow < self.recent.len() && self.recent[overflow].role == "tool" {
            overflow += 1;
        }
        // Borrow (not drain) so a summarizer error leaves state untouched.
        let older = &self.recent[0..overflow];
        let new_summary = summarizer
            .summarize(self.summary.as_deref(), older)
            .await
            .map_err(MemoryError::Summarizer)?;
        // Commit only after success.
        let folded = overflow;
        self.recent.drain(0..overflow);
        self.summary = Some(cap_summary(new_summary, policy.max_summary_chars));
        self.compacted_count += folded;
        Ok(())
    }

    /// Persist to the KV store under the `session` namespace.
    ///
    /// Last-writer-wins: callers MUST serialize mutations of a single session
    /// (load → append → save under a per-session lock). The current agent
    /// drives a session's runs sequentially, satisfying this; a future
    /// concurrent-runs design would need optimistic concurrency here (review
    /// D1, deferred to the wiring in D2+).
    pub async fn save(&self, kv: &KvStore) -> Result<()> {
        kv.put("session", &self.session_id, self).await
    }

    /// Load from the KV store; `None` if the session was never saved.
    pub async fn load(kv: &KvStore, session_id: &str) -> Result<Option<Self>> {
        kv.fetch("session", session_id).await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::sync::Mutex;

    /// Records every summarize call; returns a deterministic summary that
    /// encodes how many messages it folded and whether a prior existed.
    struct MockSummarizer {
        calls: Mutex<usize>,
    }

    impl MockSummarizer {
        fn new() -> Self {
            Self {
                calls: Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl Summarizer for MockSummarizer {
        async fn summarize(
            &self,
            prior: Option<&str>,
            messages: &[Msg],
        ) -> std::result::Result<String, String> {
            *self.calls.lock().unwrap() += 1;
            Ok(format!(
                "{}folded {} msgs",
                prior.map(|p| format!("{p}; ")).unwrap_or_default(),
                messages.len()
            ))
        }
    }

    fn user(n: usize) -> Msg {
        Msg::user(format!("message {n}"))
    }

    #[tokio::test]
    async fn no_compaction_below_threshold() {
        let mut s = CanonicalSession::new("sess_1");
        let sum = MockSummarizer::new();
        let policy = CompactionPolicy {
            max_recent: 5,
            keep_recent: 2,
            max_summary_chars: 4000,
        };
        for i in 0..5 {
            s.append(user(i), policy, &sum).await.unwrap();
        }
        assert_eq!(s.recent.len(), 5);
        assert_eq!(s.summary, None);
        assert_eq!(*sum.calls.lock().unwrap(), 0);
        // context has no summary prefix
        assert_eq!(s.context().len(), 5);
    }

    #[tokio::test]
    async fn compacts_oldest_when_tail_overflows() {
        let mut s = CanonicalSession::new("sess_1");
        let sum = MockSummarizer::new();
        let policy = CompactionPolicy {
            max_recent: 5,
            keep_recent: 2,
            max_summary_chars: 4000,
        };
        // 6th message triggers compaction: overflow = 6 - 2 = 4 folded, 2 kept
        for i in 0..6 {
            s.append(user(i), policy, &sum).await.unwrap();
        }
        assert_eq!(s.recent.len(), 2);
        assert_eq!(s.compacted_count, 4);
        assert_eq!(s.summary.as_deref(), Some("folded 4 msgs"));
        assert_eq!(*sum.calls.lock().unwrap(), 1);
        // the kept tail is the two newest
        assert_eq!(s.recent[0].content.as_deref(), Some("message 4"));
        assert_eq!(s.recent[1].content.as_deref(), Some("message 5"));
        // context = system summary + 2 recent
        let ctx = s.context();
        assert_eq!(ctx.len(), 3);
        assert_eq!(ctx[0].role, "system");
        assert!(ctx[0].content.as_deref().unwrap().contains("folded 4 msgs"));
    }

    #[tokio::test]
    async fn repeated_compaction_folds_prior_summary() {
        let mut s = CanonicalSession::new("sess_1");
        let sum = MockSummarizer::new();
        let policy = CompactionPolicy {
            max_recent: 4,
            keep_recent: 2,
            max_summary_chars: 4000,
        };
        // 10 messages → multiple compactions, each folding the prior summary
        for i in 0..10 {
            s.append(user(i), policy, &sum).await.unwrap();
        }
        assert!(*sum.calls.lock().unwrap() >= 2);
        // prior summary is carried forward (mock prefixes it)
        assert!(s.summary.as_deref().unwrap().contains("folded"));
        assert!(s.summary.as_deref().unwrap().contains(';')); // prior folded in
        // the verbatim tail never exceeds the trigger
        assert!(s.recent.len() <= 4);
        // every message is accounted for: compacted + recent == total appended
        assert_eq!(s.compacted_count + s.recent.len(), 10);
    }

    #[tokio::test]
    async fn degenerate_policy_is_normalized_not_looping() {
        let mut s = CanonicalSession::new("sess_1");
        let sum = MockSummarizer::new();
        // keep_recent >= max_recent would never converge; normalized clamps it
        let policy = CompactionPolicy {
            max_recent: 3,
            keep_recent: 9,
            max_summary_chars: 4000,
        };
        for i in 0..5 {
            s.append(user(i), policy, &sum).await.unwrap();
        }
        // normalized to max_recent=3, keep_recent=2: compaction happened (no
        // infinite loop) and the tail stays bounded by max_recent
        assert!(s.compacted_count > 0);
        assert!(s.recent.len() <= 3);
    }

    #[tokio::test]
    async fn save_and_load_via_kv() {
        let kv = KvStore::open_memory().await.unwrap();
        let mut s = CanonicalSession::new("sess_1");
        let sum = MockSummarizer::new();
        for i in 0..3 {
            s.append(user(i), CompactionPolicy::default(), &sum)
                .await
                .unwrap();
        }
        s.save(&kv).await.unwrap();
        let loaded = CanonicalSession::load(&kv, "sess_1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded, s);
        assert_eq!(CanonicalSession::load(&kv, "nope").await.unwrap(), None);
    }

    #[tokio::test]
    async fn summarizer_error_surfaces() {
        struct FailingSummarizer;
        #[async_trait]
        impl Summarizer for FailingSummarizer {
            async fn summarize(
                &self,
                _prior: Option<&str>,
                _messages: &[Msg],
            ) -> std::result::Result<String, String> {
                Err("model unavailable".to_owned())
            }
        }
        let mut s = CanonicalSession::new("sess_1");
        let policy = CompactionPolicy {
            max_recent: 2,
            keep_recent: 1,
            max_summary_chars: 4000,
        };
        s.append(user(0), policy, &FailingSummarizer).await.unwrap();
        s.append(user(1), policy, &FailingSummarizer).await.unwrap();
        // third message triggers compaction → summarizer error propagates
        let err = s
            .append(user(2), policy, &FailingSummarizer)
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::Summarizer(_)), "{err}");
        // NO-LOSS: recent untouched — all 3 messages present, nothing dropped
        // by the failed fold (review D1 blocker)
        assert_eq!(s.recent.len(), 3);
        assert_eq!(s.summary, None);
        assert_eq!(s.compacted_count, 0);
        assert_eq!(s.recent[0].content.as_deref(), Some("message 0"));
        assert_eq!(s.recent[2].content.as_deref(), Some("message 2"));
    }

    #[tokio::test]
    async fn summary_is_capped_to_the_policy_bound() {
        // A summarizer that returns a huge string must not grow the prompt
        // without bound — cap_summary truncates it (review D1 major).
        struct HugeSummarizer;
        #[async_trait]
        impl Summarizer for HugeSummarizer {
            async fn summarize(
                &self,
                _prior: Option<&str>,
                _messages: &[Msg],
            ) -> std::result::Result<String, String> {
                Ok("x".repeat(10_000))
            }
        }
        let mut s = CanonicalSession::new("sess_1");
        let policy = CompactionPolicy {
            max_recent: 2,
            keep_recent: 1,
            max_summary_chars: 100,
        };
        for i in 0..3 {
            s.append(user(i), policy, &HugeSummarizer).await.unwrap();
        }
        let summary = s.summary.as_deref().unwrap();
        assert!(summary.chars().count() <= 100, "summary not capped");
        assert!(summary.ends_with('…'));
    }

    #[tokio::test]
    async fn compaction_never_orphans_a_tool_result() {
        // The exact repro (review D1 High): user → assistant(tool_calls) →
        // tool_result, with a boundary that would split the pair. The kept
        // tail must never start with a `role: "tool"` message (which would be
        // an orphaned tool result an OpenAI-compatible provider rejects).
        let mut s = CanonicalSession::new("sess_1");
        let sum = MockSummarizer::new();
        let policy = CompactionPolicy {
            max_recent: 2,
            keep_recent: 1,
            max_summary_chars: 4000,
        };
        s.append(user(0), policy, &sum).await.unwrap();
        s.append(
            Msg::assistant(
                None,
                vec![agent24_models::ToolCallRequest {
                    id: "call_1".to_owned(),
                    name: "shell_exec".to_owned(),
                    arguments: "{}".to_owned(),
                }],
            ),
            policy,
            &sum,
        )
        .await
        .unwrap();
        // this triggers compaction; the naive boundary would keep the tool
        // result alone
        s.append(Msg::tool_result("call_1", "output"), policy, &sum)
            .await
            .unwrap();
        // the tool result was folded with its assistant, not stranded
        assert!(
            s.recent.first().map(|m| m.role.as_str()) != Some("tool"),
            "kept tail must not start with an orphaned tool result: {:?}",
            s.recent
        );
        // context() likewise never begins its verbatim part with a bare tool msg
        let ctx = s.context();
        let first_verbatim = ctx.iter().find(|m| m.role != "system");
        assert!(first_verbatim.map(|m| m.role.as_str()) != Some("tool"));
    }

    #[tokio::test]
    async fn tool_call_group_kept_together_when_it_fits() {
        // When keep_recent is large enough, the whole tool-call group stays in
        // the verbatim tail — no spurious over-folding.
        let mut s = CanonicalSession::new("sess_1");
        let sum = MockSummarizer::new();
        let policy = CompactionPolicy {
            max_recent: 4,
            keep_recent: 3,
            max_summary_chars: 4000,
        };
        s.append(user(0), policy, &sum).await.unwrap();
        s.append(user(1), policy, &sum).await.unwrap();
        s.append(
            Msg::assistant(
                None,
                vec![agent24_models::ToolCallRequest {
                    id: "c".to_owned(),
                    name: "t".to_owned(),
                    arguments: "{}".to_owned(),
                }],
            ),
            policy,
            &sum,
        )
        .await
        .unwrap();
        s.append(Msg::tool_result("c", "out"), policy, &sum)
            .await
            .unwrap();
        // 5th append triggers compaction with keep_recent=3: the assistant +
        // tool_result pair is within the kept tail, never split
        s.append(user(4), policy, &sum).await.unwrap();
        assert_ne!(s.recent.first().map(|m| m.role.as_str()), Some("tool"));
    }

    #[tokio::test]
    async fn msg_serde_roundtrips_tool_calls_and_results() {
        // Persisting a session requires Msg to serialize losslessly, including
        // an assistant tool-call turn and a tool-result turn (review D1 minor).
        let assistant = Msg::assistant(
            Some("calling a tool".to_owned()),
            vec![agent24_models::ToolCallRequest {
                id: "call_1".to_owned(),
                name: "shell_exec".to_owned(),
                arguments: "{\"argv\":[\"ls\"]}".to_owned(),
            }],
        );
        let tool_result = Msg::tool_result("call_1", "file listing");
        let json = serde_json::to_string(&vec![assistant.clone(), tool_result.clone()]).unwrap();
        let back: Vec<Msg> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, vec![assistant, tool_result]);
        assert_eq!(back[0].tool_calls[0].name, "shell_exec");
        assert_eq!(back[1].tool_call_id.as_deref(), Some("call_1"));
    }
}
