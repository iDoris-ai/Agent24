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
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        // Compact once 40 verbatim messages pile up, keeping the last 20.
        Self {
            max_recent: 40,
            keep_recent: 20,
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
        }
    }
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
        // Fold everything except the last `keep_recent` into the summary.
        let overflow = self.recent.len() - policy.keep_recent;
        let older: Vec<Msg> = self.recent.drain(0..overflow).collect();
        let new_summary = summarizer
            .summarize(self.summary.as_deref(), &older)
            .await
            .map_err(MemoryError::Summarizer)?;
        self.summary = Some(new_summary);
        self.compacted_count += older.len();
        Ok(())
    }

    /// Persist to the KV store under the `session` namespace.
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
        };
        s.append(user(0), policy, &FailingSummarizer).await.unwrap();
        s.append(user(1), policy, &FailingSummarizer).await.unwrap();
        // third message triggers compaction → summarizer error propagates
        let err = s
            .append(user(2), policy, &FailingSummarizer)
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::Summarizer(_)), "{err}");
    }
}
