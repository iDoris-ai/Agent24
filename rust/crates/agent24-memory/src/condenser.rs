//! MD-1: context condensers — a BOUNDED projection over an UNBOUNDED history
//! that never deletes the source (SPEC-MD-ME §2/§3, ADR-028 "condenser = view
//! delta, not deletion").
//!
//! A [`Condenser`] reads the full message history and returns a
//! [`ContextProjection`]: the bounded view to feed the model, plus the
//! view-delta ([`ContextProjection::folded`]) recording which source messages
//! were hidden from the verbatim view — never dropped. The raw history stays
//! authoritative (MD-2's `EventStore`); condensing it is a pure, reproducible
//! projection. This is the OpenHands model, verified in its source
//! (`event/condenser.py`: a `Condensation` carries `forgotten_event_ids` +
//! summary + offset, replayed by `View.from_events`).
//!
//! Contrast with [`crate::session::CanonicalSession`], which folds old messages
//! into a lossy running summary and DROPS them from `recent`. That loses the
//! verbatim originals; a condenser must not. MD-2 will back these condensers
//! with a durable event log so the projection is a view, not the truth.

use agent24_models::Msg;
use async_trait::async_trait;

use crate::session::Summarizer;

/// One rendered piece of context, linked back to the source messages it stands
/// for (indices into the input history; these become event ids in MD-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFragment {
    pub msg: Msg,
    /// The source-history indices this fragment represents. A verbatim message
    /// carries its own index; a summary fragment carries every folded index it
    /// stands in for (provenance).
    pub source: Vec<usize>,
    /// Why this fragment is in the view: `"recent"` (verbatim tail) or
    /// `"summary"` (folded head).
    pub reason: &'static str,
}

/// A best-effort-bounded view of a history.
///
/// The view TARGETS `budget_tokens` but is deliberately NOT hard-capped — two
/// documented overruns are allowed (review #113 B4):
/// 1. **Tool-safety.** An assistant `tool_calls` turn and the `tool_result`
///    answering it are indivisible, so the newest such pair is kept whole even
///    when it alone exceeds the budget (see [`tail_start`]).
/// 2. **Summary overhead.** A summary fragment ([`LlmSummaryCondenser`]) sits ON
///    TOP of the verbatim tail. The condenser RESERVES budget for it, but an
///    oversized summarizer can still push the total past `budget_tokens`.
///
/// So a caller wiring this into a real context window must treat `budget_tokens`
/// as a target, read the honest [`ContextProjection::tokens_estimated`], and not
/// assume a hard cap.
///
/// **No-loss invariant** (checked by [`ContextProjection::covers`] and the
/// tests): every source index `0..history.len()` appears EXACTLY once across
/// `fragments[*].source` ∪ `folded`. The projection hides or represents
/// messages; it never loses them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextProjection {
    pub fragments: Vec<ContextFragment>,
    /// Source indices compacted OUT of the verbatim view and NOT carried by a
    /// summary fragment — i.e. hidden. (A summary that stands in for the head
    /// keeps those indices in its fragment's `source` instead.)
    pub folded: Vec<usize>,
    pub tokens_estimated: usize,
}

impl ContextProjection {
    /// The messages to feed the model, in order.
    pub fn messages(&self) -> Vec<Msg> {
        self.fragments.iter().map(|f| f.msg.clone()).collect()
    }

    /// True iff the no-loss invariant holds for a history of length `n`: the
    /// union of every fragment's `source` and `folded` is exactly `0..n`, with
    /// no index missing and none appearing twice.
    pub fn covers(&self, n: usize) -> bool {
        let mut all: Vec<usize> = self
            .fragments
            .iter()
            .flat_map(|f| f.source.iter().copied())
            .chain(self.folded.iter().copied())
            .collect();
        all.sort_unstable();
        all.len() == n && all.iter().copied().eq(0..n)
    }
}

/// Token budgeting seam (goose's `TokenEstimator` split): swap in a real
/// tokenizer later without touching a condenser.
pub trait TokenEstimator: Send + Sync {
    fn estimate(&self, msgs: &[Msg]) -> usize;
}

/// A deterministic ~4-chars-per-token heuristic over role + content + tool-call
/// name/args. No dependency, reproducible — good enough for budgeting and for
/// the spike's regression corpus; a real tokenizer is a drop-in later.
///
/// TODO(MD-2b Low#3): this is a one-sided UNDER-estimate — it ignores
/// `tool_call_id` and per-message framing overhead, so it never over-counts.
/// Combined with the best-effort summary overhead that skews budgets small; a
/// real tokenizer with per-message framing closes it.
#[derive(Debug, Clone, Copy, Default)]
pub struct CharTokenEstimator;

impl TokenEstimator for CharTokenEstimator {
    fn estimate(&self, msgs: &[Msg]) -> usize {
        let chars: usize = msgs
            .iter()
            .map(|m| {
                m.role.len()
                    + m.content.as_deref().map(str::len).unwrap_or(0)
                    + m.tool_calls
                        .iter()
                        .map(|t| t.name.len() + t.arguments.len())
                        .sum::<usize>()
            })
            .sum();
        chars.div_ceil(4)
    }
}

/// Turns an unbounded history into a [`ContextProjection`] that TARGETS a token
/// budget — best-effort, not a hard cap (see [`ContextProjection`] for the two
/// documented overruns). MUST NOT mutate or drop the input — it returns a VIEW;
/// the source stays authoritative.
#[async_trait]
pub trait Condenser: Send + Sync {
    async fn condense(
        &self,
        history: &[Msg],
        budget_tokens: usize,
    ) -> std::result::Result<ContextProjection, String>;
}

/// The start index of the largest verbatim tail `history[start..]` that fits
/// `budget`, adjusted so the tail never BEGINS with an orphaned tool result.
///
/// A `role:"tool"` message split from the assistant `tool_calls` turn it answers
/// is an invalid conversation every OpenAI-compatible provider rejects (the same
/// guard `CanonicalSession::append` enforces). We always keep at least the
/// newest message (a projection with an empty verbatim view but a budget below
/// one message is still valid — everything is folded).
fn tail_start(history: &[Msg], est: &dyn TokenEstimator, budget: usize) -> usize {
    let n = history.len();
    if n == 0 {
        return 0;
    }
    // Grow the tail one older message at a time while it still fits the budget,
    // but never below the single newest message.
    let mut start = n;
    while start > 0 {
        let cand = start - 1;
        if start < n && est.estimate(&history[cand..]) > budget {
            break;
        }
        start = cand;
    }
    // Don't begin the tail on an orphaned tool result. Advancing past leading
    // tool messages is fine UNLESS it would eat the ENTIRE tail (reach `n` → an
    // empty view). That empty case is exactly the agent-loop moment when the
    // newest message is a `tool_result`, so instead of emptying the model input
    // we RETREAT to pull in the assistant turn those results answer — the same
    // "never below the newest message" escape hatch the loop above encodes
    // (review #113 B1: the advance previously contradicted that guarantee).
    let mut adv = start;
    while adv < n && history[adv].role == "tool" {
        adv += 1;
    }
    if adv < n {
        start = adv;
    } else {
        // Retreat past the leading tool run to include its assistant turn. A
        // history that is ALL tool results has no assistant to attach to, so
        // `start` lands at 0 (a leading orphan): that degenerate input is out of
        // contract (a real conversation never starts with a tool result).
        while start > 0 && history[start].role == "tool" {
            start -= 1;
        }
    }
    start
}

/// Deterministic baseline condenser: keep the newest messages that fit the
/// budget verbatim, FOLD (hide) the older head. No LLM, fully reproducible — the
/// spike's control against which the summarizing condenser is measured.
#[derive(Debug, Clone, Copy)]
pub struct RecentWindowCondenser<E = CharTokenEstimator> {
    pub est: E,
}

// Concrete Default (not derived) so `RecentWindowCondenser::default()` resolves
// the estimator to `CharTokenEstimator` instead of leaving `E` ambiguous.
impl Default for RecentWindowCondenser<CharTokenEstimator> {
    fn default() -> Self {
        Self {
            est: CharTokenEstimator,
        }
    }
}

#[async_trait]
impl<E: TokenEstimator> Condenser for RecentWindowCondenser<E> {
    async fn condense(
        &self,
        history: &[Msg],
        budget_tokens: usize,
    ) -> std::result::Result<ContextProjection, String> {
        let start = tail_start(history, &self.est, budget_tokens);
        let fragments: Vec<ContextFragment> = history[start..]
            .iter()
            .enumerate()
            .map(|(i, m)| ContextFragment {
                msg: m.clone(),
                source: vec![start + i],
                reason: "recent",
            })
            .collect();
        let tokens = self.est.estimate(&history[start..]);
        Ok(ContextProjection {
            fragments,
            folded: (0..start).collect(),
            tokens_estimated: tokens,
        })
    }
}

/// Head-summary + verbatim tail: fold the head into an LLM summary (via
/// [`Summarizer`]) and keep the recent tail verbatim. The summary fragment's
/// `source` carries every folded head index, so the no-loss invariant holds and
/// `folded` stays empty — the head is REPRESENTED, not lost. With nothing to
/// summarize it degrades to a pure recent window.
///
/// TODO(MD-2b Low#1): there is no `pinned` seam for a system prompt that must
/// always survive condensing. Before MD-2b wires this to a real context window,
/// either add one or make explicit that pinned instructions are the caller's
/// responsibility (reviewer's Low #1).
pub struct LlmSummaryCondenser<'a, E = CharTokenEstimator> {
    pub summarizer: &'a dyn Summarizer,
    pub est: E,
}

impl<'a, E: TokenEstimator> LlmSummaryCondenser<'a, E> {
    pub fn new(summarizer: &'a dyn Summarizer, est: E) -> Self {
        Self { summarizer, est }
    }
}

#[async_trait]
impl<E: TokenEstimator> Condenser for LlmSummaryCondenser<'_, E> {
    async fn condense(
        &self,
        history: &[Msg],
        budget_tokens: usize,
    ) -> std::result::Result<ContextProjection, String> {
        // Reserve part of the budget for the summary fragment so the verbatim
        // tail does not eat the whole window and leave the summary as pure
        // overrun (review #113 B4). A summary COMPRESSES the head, so a quarter
        // of the budget is a generous target. This is a RESERVE, not a cap: an
        // oversized summarizer can still overrun, which is exactly why the
        // contract is best-effort (see `ContextProjection` docs) and is pinned by
        // `llm_summary_reserve_is_best_effort_not_hard_bounded`.
        let reserve = budget_tokens / 4;
        let tail_budget = budget_tokens.saturating_sub(reserve);
        let start = tail_start(history, &self.est, tail_budget);
        let mut fragments = Vec::with_capacity(history.len() - start + 1);
        if start > 0 {
            // TODO(MD-2b Low#2): thread the prior running summary in here instead
            // of `None` so re-condensing composes incrementally rather than
            // re-summarizing the whole head each time (reviewer's Low #2).
            let summary = self.summarizer.summarize(None, &history[0..start]).await?;
            fragments.push(ContextFragment {
                msg: Msg {
                    role: "system".to_owned(),
                    content: Some(format!("Summary of earlier conversation:\n{summary}")),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
                source: (0..start).collect(),
                reason: "summary",
            });
        }
        for (i, m) in history[start..].iter().enumerate() {
            fragments.push(ContextFragment {
                msg: m.clone(),
                source: vec![start + i],
                reason: "recent",
            });
        }
        let msgs: Vec<Msg> = fragments.iter().map(|f| f.msg.clone()).collect();
        let tokens = self.est.estimate(&msgs);
        Ok(ContextProjection {
            fragments,
            folded: vec![],
            tokens_estimated: tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_models::ToolCallRequest;
    use std::sync::Mutex;

    fn user(n: usize) -> Msg {
        Msg::user(format!("message {n} with some padding text"))
    }
    fn history(n: usize) -> Vec<Msg> {
        (0..n).map(user).collect()
    }

    struct MockSummarizer {
        calls: Mutex<usize>,
    }
    #[async_trait]
    impl Summarizer for MockSummarizer {
        async fn summarize(
            &self,
            _prior: Option<&str>,
            messages: &[Msg],
        ) -> std::result::Result<String, String> {
            *self.calls.lock().unwrap() += 1;
            Ok(format!("folded {} msgs", messages.len()))
        }
    }

    /// A realistic-magnitude summarizer. `MockSummarizer` returns a handful of
    /// chars ("folded N msgs"), far too small to ever trip the budget — so it
    /// cannot pin the best-effort contract. This one returns a summary the size
    /// of a real LLM compaction.
    struct BigSummarizer;
    #[async_trait]
    impl Summarizer for BigSummarizer {
        async fn summarize(
            &self,
            _prior: Option<&str>,
            _messages: &[Msg],
        ) -> std::result::Result<String, String> {
            Ok("summary sentence. ".repeat(60)) // ~1080 chars ≈ 270 tokens
        }
    }

    #[tokio::test]
    async fn recent_window_keeps_tail_folds_head_and_loses_nothing() {
        let h = history(20);
        let c = RecentWindowCondenser::default();
        // Budget for roughly the newest few messages.
        let budget = CharTokenEstimator.estimate(&h[17..]);
        let p = c.condense(&h, budget).await.unwrap();
        assert!(!p.fragments.is_empty());
        assert!(p.tokens_estimated <= budget || p.fragments.len() == 1);
        // Every message accounted for exactly once (view-delta, no loss).
        assert!(p.covers(h.len()), "no-loss invariant: {p:?}");
        // The verbatim view is a contiguous newest suffix.
        let kept: Vec<usize> = p.fragments.iter().flat_map(|f| f.source.clone()).collect();
        assert_eq!(kept, (kept[0]..h.len()).collect::<Vec<_>>());
        assert_eq!(p.folded, (0..kept[0]).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn recent_window_is_deterministic_and_pure() {
        let h = history(30);
        let c = RecentWindowCondenser::default();
        let a = c.condense(&h, 40).await.unwrap();
        let b = c.condense(&h, 40).await.unwrap();
        assert_eq!(a, b, "same input+budget must give an identical projection");
        // The condenser never mutates the input history.
        assert_eq!(h, history(30));
    }

    #[tokio::test]
    async fn tail_never_orphans_a_tool_result_and_never_empties_the_view() {
        // user, assistant(tool_calls), tool_result — the newest message is a
        // tool_result, exactly the agent-loop moment before the next model call.
        let h = vec![
            user(0),
            Msg::assistant(
                None,
                vec![ToolCallRequest {
                    id: "c1".to_owned(),
                    name: "shell_exec".to_owned(),
                    arguments: "{}".to_owned(),
                }],
            ),
            Msg::tool_result("c1", "output"),
        ];
        let c = RecentWindowCondenser::default();
        // Across EVERY budget the view must be non-empty (B1: it used to empty
        // out) AND start with the assistant, not the orphaned tool result. At a
        // tiny budget the tool-safe pair is kept even though it exceeds the
        // budget — tool-safety wins over the budget (B2: assert this POSITIVELY,
        // not the old vacuous `assert_ne!(None, tool)` at budget=1).
        for budget in [0usize, 1, 5, 8, 12, 40] {
            let p = c.condense(&h, budget).await.unwrap();
            assert!(
                !p.fragments.is_empty(),
                "empty view at budget={budget}: {p:?}"
            );
            let roles: Vec<&str> = p.fragments.iter().map(|f| f.msg.role.as_str()).collect();
            assert_ne!(
                roles.first(),
                Some(&"tool"),
                "orphan at budget={budget}: {roles:?}"
            );
            // The tool_result and its assistant are always kept together.
            assert!(
                roles.ends_with(&["assistant", "tool"]),
                "pair split at budget={budget}: {roles:?}"
            );
            assert!(p.covers(h.len()));
        }
    }

    #[tokio::test]
    async fn recent_window_always_keeps_the_newest_message() {
        // The SEMANTIC guarantee `covers()` cannot see (B3: covers() only checks
        // bookkeeping — folded ∪ sources == 0..n holds even for an empty view).
        // For any non-empty history the newest source index must be in the
        // verbatim view, never folded away.
        let c = RecentWindowCondenser::default();
        for n in [1usize, 2, 7, 30] {
            let h = history(n);
            for budget in [0usize, 1, 5, 50] {
                let p = c.condense(&h, budget).await.unwrap();
                let kept: Vec<usize> = p.fragments.iter().flat_map(|f| f.source.clone()).collect();
                assert!(
                    kept.contains(&(n - 1)),
                    "newest message (idx {}) dropped at n={n} budget={budget}: {p:?}",
                    n - 1
                );
                assert!(!p.folded.contains(&(n - 1)));
            }
        }
    }

    #[tokio::test]
    async fn all_tool_history_is_out_of_contract_but_does_not_panic() {
        // Degenerate input (a conversation never starts with a tool result):
        // documented behavior is a leading orphan at index 0, no loss, no panic.
        let h = vec![Msg::tool_result("c", "a"), Msg::tool_result("c", "b")];
        let c = RecentWindowCondenser::default();
        let p = c.condense(&h, 1).await.unwrap();
        assert!(p.covers(h.len()));
    }

    #[tokio::test]
    async fn llm_summary_represents_head_and_keeps_tail_no_loss() {
        let h = history(20);
        let sum = MockSummarizer {
            calls: Mutex::new(0),
        };
        let c = LlmSummaryCondenser::new(&sum, CharTokenEstimator);
        let budget = CharTokenEstimator.estimate(&h[16..]);
        let p = c.condense(&h, budget).await.unwrap();
        // A summary fragment stands in for the folded head; `folded` is empty
        // because the head is represented (provenance), not dropped.
        assert_eq!(p.fragments[0].reason, "summary");
        assert!(
            p.fragments[0]
                .msg
                .content
                .as_deref()
                .unwrap()
                .contains("folded")
        );
        assert!(p.folded.is_empty());
        assert!(p.covers(h.len()), "no-loss invariant: {p:?}");
        assert_eq!(*sum.calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn llm_summary_degrades_to_recent_when_nothing_to_fold() {
        let h = history(3);
        let sum = MockSummarizer {
            calls: Mutex::new(0),
        };
        let c = LlmSummaryCondenser::new(&sum, CharTokenEstimator);
        // Budget large enough for everything → no head to summarize.
        let p = c.condense(&h, 100_000).await.unwrap();
        assert!(p.fragments.iter().all(|f| f.reason == "recent"));
        assert_eq!(
            *sum.calls.lock().unwrap(),
            0,
            "no summarize call when nothing folds"
        );
        assert!(p.covers(h.len()));
    }

    #[tokio::test]
    async fn no_loss_invariant_holds_across_sizes_and_budgets() {
        let sum = MockSummarizer {
            calls: Mutex::new(0),
        };
        let recent = RecentWindowCondenser::default();
        let summ = LlmSummaryCondenser::new(&sum, CharTokenEstimator);
        for n in [0usize, 1, 2, 5, 13, 50] {
            let h = history(n);
            for budget in [0usize, 1, 5, 20, 100, 100_000] {
                let a = recent.condense(&h, budget).await.unwrap();
                assert!(a.covers(n), "recent n={n} budget={budget}: {a:?}");
                let b = summ.condense(&h, budget).await.unwrap();
                assert!(b.covers(n), "summary n={n} budget={budget}: {b:?}");
            }
        }
    }

    #[tokio::test]
    async fn llm_summary_reserve_is_best_effort_not_hard_bounded() {
        // B4: the summary is overhead ON TOP of the verbatim tail. Reservation
        // shrinks the tail so it does not eat the whole window, but a big
        // summarizer still overruns — the contract is best-effort. This test
        // pins that so a reader of the "budget" docs is never misled into
        // assuming a hard cap (the whole B4 finding).
        let h = history(40);
        let sum = BigSummarizer;
        let c = LlmSummaryCondenser::new(&sum, CharTokenEstimator);
        let budget = 40usize;
        let p = c.condense(&h, budget).await.unwrap();

        // No loss, whatever the budget math does.
        assert!(p.covers(h.len()), "no-loss: {p:?}");
        assert_eq!(p.fragments[0].reason, "summary");

        // Reservation worked: the VERBATIM TAIL alone respects the reduced
        // (reserved) budget and did NOT consume the whole window. (The
        // single-newest-message floor is the only allowed exception.)
        let recent: Vec<Msg> = p
            .fragments
            .iter()
            .filter(|f| f.reason == "recent")
            .map(|f| f.msg.clone())
            .collect();
        let tail_tokens = CharTokenEstimator.estimate(&recent);
        let reserve = budget / 4;
        assert!(
            tail_tokens <= budget - reserve || recent.len() == 1,
            "tail ({tail_tokens}) should respect the reserved budget {}",
            budget - reserve
        );

        // But the TOTAL exceeds the budget because the summary is bigger than
        // its reserve — the documented best-effort overrun. If this ever becomes
        // <= budget, the "not a hard cap" caveat in the docs is stale and should
        // be revisited.
        assert!(
            p.tokens_estimated > budget,
            "a big summarizer must overrun to prove best-effort, got {}",
            p.tokens_estimated
        );
    }

    #[tokio::test]
    async fn empty_history_projects_empty() {
        let c = RecentWindowCondenser::default();
        let p = c.condense(&[], 100).await.unwrap();
        assert!(p.fragments.is_empty() && p.folded.is_empty());
        assert!(p.covers(0));
    }
}
