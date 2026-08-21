//! MD-1c: the eval harness — load a LongMemEval-format corpus into the durable
//! event log and measure a condenser BASELINE, so later MD-x cannot silently
//! regress it (SPEC-MD-ME §3 MD-1 acceptance: "LongMemEval 装载跑通"; the eval
//! gate of §0).
//!
//! LongMemEval (github.com/xiaowu0162/LongMemEval) poses a question over a
//! "haystack" of many prior chat sessions, with the answer-bearing turn(s) marked
//! `has_answer`. This module:
//! 1. parses that JSON ([`parse_cases`] / [`load_cases_from_file`]),
//! 2. ingests a case's haystack into the [`EventLog`] as `message` events, one
//!    per turn, session-scoped ([`ingest_case`]), and
//! 3. replays + condenses and reports whether the answer survived the view
//!    ([`run_case`]).
//!
//! **This is a baseline, not a score to celebrate.** A recent-window condenser
//! can only surface the NEWEST turns, so for a deep answer `answer_in_view` is
//! expected to be FALSE — that is precisely the number MD-3's retriever must beat
//! (the same boundary [`crate::replay`] documents). What MD-1 delivers here is
//! the harness and a reproducible baseline, so a regression is visible; it does
//! not claim deep recall the condenser structurally cannot do.
//!
//! The full dataset is large and not vendored. Point [`load_cases_from_file`] at
//! a downloaded `longmemeval_*.json`; the tests run an embedded miniature in the
//! same schema so "装载跑通" is exercised in CI without the download.

use std::path::Path;

use agent24_models::Msg;
use serde::Deserialize;

use crate::condenser::Condenser;
use crate::event::{EventLog, EventQuery, EventStore, Origin, Scope, Trust};
use crate::replay::{message_event, replay_history};
use crate::{MemoryError, Result};

/// One turn of a haystack session. Unknown fields are ignored (serde default), so
/// the real dataset's extra keys do not break the loader.
#[derive(Debug, Clone, Deserialize)]
pub struct LongMemEvalTurn {
    pub role: String,
    pub content: String,
    /// LongMemEval marks the turn(s) that contain the answer.
    #[serde(default)]
    pub has_answer: bool,
}

/// One LongMemEval question with its haystack of prior sessions.
#[derive(Debug, Clone, Deserialize)]
pub struct LongMemEvalCase {
    pub question_id: String,
    #[serde(default)]
    pub question_type: String,
    pub question: String,
    #[serde(default)]
    pub answer: String,
    #[serde(default)]
    pub question_date: String,
    /// The haystack: a list of sessions, each a list of turns.
    pub haystack_sessions: Vec<Vec<LongMemEvalTurn>>,
    /// Session ids, positionally aligned with `haystack_sessions`. When absent or
    /// short, [`ingest_case`] synthesizes `sess-{i}`.
    #[serde(default)]
    pub haystack_session_ids: Vec<String>,
    #[serde(default)]
    pub haystack_dates: Vec<String>,
    #[serde(default)]
    pub answer_session_ids: Vec<String>,
}

impl LongMemEvalCase {
    /// The contents of every answer-bearing turn (what a correct recall must
    /// surface).
    pub fn answer_turns(&self) -> Vec<&str> {
        self.haystack_sessions
            .iter()
            .flatten()
            .filter(|t| t.has_answer)
            .map(|t| t.content.as_str())
            .collect()
    }

    /// The FLAT indices (across all sessions, in ingest order) of the
    /// answer-bearing turns. [`ingest_case`] appends turns in exactly this
    /// flattened `(session, turn)` order, and replay returns them in seq order,
    /// so a turn's flat index equals its index in the condenser's history — which
    /// is what [`crate::condenser::ContextFragment::source`] refers to. Judging
    /// recall by these indices (not substring matching) is exact.
    pub fn answer_turn_indices(&self) -> Vec<usize> {
        self.haystack_sessions
            .iter()
            .flatten()
            .enumerate()
            .filter(|(_, t)| t.has_answer)
            .map(|(i, _)| i)
            .collect()
    }
}

/// Parse a LongMemEval JSON array of cases.
pub fn parse_cases(json: &str) -> Result<Vec<LongMemEvalCase>> {
    Ok(serde_json::from_str(json)?)
}

/// Load LongMemEval cases from a downloaded dataset file.
pub fn load_cases_from_file(path: &Path) -> Result<Vec<LongMemEvalCase>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| MemoryError::Io(format!("read {}: {e}", path.display())))?;
    parse_cases(&text)
}

/// Map a haystack role to a [`Trust`]: a user turn is what the user said, an
/// assistant turn is model output; anything else is treated as system.
fn trust_for(role: &str) -> Trust {
    match role {
        "user" => Trust::UserSaid,
        "assistant" => Trust::Model,
        _ => Trust::System,
    }
}

/// Persist a case's haystack into `log` under `owner`: one `message` event per
/// turn, each scoped to its session so an owner-only replay reconstructs the full
/// haystack while a session-scoped replay isolates one conversation.
///
/// Event ids are POSITION-stable (`lme-{question_id}-{session}-{turn}` — no
/// content component) and the loader is the single writer, so re-ingesting the
/// SAME corpus is idempotent. Re-ingesting a CORRECTED variant at the same
/// position does NOT overwrite: it hits the append idempotency guard and errors
/// as a `Conflict` (same id, different payload — #114 B2's loud-not-silent
/// behavior). To load a corrected corpus, use a fresh `owner` or clear the DB.
/// Returns the number of events appended (new + idempotent replays alike).
pub async fn ingest_case(log: &EventLog, owner: &str, case: &LongMemEvalCase) -> Result<usize> {
    let mut count = 0usize;
    for (si, session) in case.haystack_sessions.iter().enumerate() {
        let session_id = case
            .haystack_session_ids
            .get(si)
            .cloned()
            .unwrap_or_else(|| format!("sess-{si}"));
        for (ti, turn) in session.iter().enumerate() {
            let msg = match turn.role.as_str() {
                "assistant" => Msg::assistant(Some(turn.content.clone()), vec![]),
                "user" => Msg::user(turn.content.clone()),
                other => Msg {
                    role: other.to_owned(),
                    content: Some(turn.content.clone()),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
            };
            let scope = Scope::owner(owner).with_session(&session_id);
            let origin = Origin {
                source: "longmemeval".to_owned(),
                trust: trust_for(&turn.role),
            };
            let id = format!("lme-{}-{si}-{ti}", case.question_id);
            log.append(&message_event(id, scope, &msg, origin)?).await?;
            count += 1;
        }
    }
    Ok(count)
}

/// The baseline outcome of one case under one condenser + budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalOutcome {
    pub question_id: String,
    /// Turns recovered from the log.
    pub total_turns: usize,
    /// Turns kept verbatim in the condenser's view.
    pub kept_turns: usize,
    /// Whether an answer-bearing turn's content is present in the verbatim view.
    /// For a recent-window condenser over a deep answer this is expected FALSE —
    /// the baseline MD-3 must improve on.
    pub answer_in_view: bool,
    /// Whether replay + condense lost nothing (`covers`). MUST stay true.
    pub lossless: bool,
}

/// Ingest is assumed already done (call [`ingest_case`] first). Replays `owner`'s
/// history, condenses under `budget`, and reports the baseline for `case`.
pub async fn run_case(
    log: &EventLog,
    owner: &str,
    case: &LongMemEvalCase,
    condenser: &dyn Condenser,
    budget: usize,
) -> Result<EvalOutcome> {
    let replayed = replay_history(log, &EventQuery::owner(owner)).await?;
    let projection = condenser
        .condense(&replayed.messages, budget)
        .await
        .map_err(MemoryError::Condenser)?;

    // Judge recall by SOURCE INDEX, not substring: a deep answer that happens to
    // be a substring of a recent turn must NOT count as recalled, or the baseline
    // this PR freezes would OVER-report and hide a future regression (review #117
    // B1). `fragments[*].source` are the history indices the model actually sees;
    // `answer_turn_indices()` are the answer turns' indices in that same history.
    let in_view: std::collections::HashSet<usize> = projection
        .fragments
        .iter()
        .flat_map(|f| f.source.iter().copied())
        .collect();
    let answer_in_view = case
        .answer_turn_indices()
        .iter()
        .any(|i| in_view.contains(i));

    Ok(EvalOutcome {
        question_id: case.question_id.clone(),
        total_turns: replayed.len(),
        kept_turns: projection.fragments.len(),
        answer_in_view,
        lossless: projection.covers(replayed.len()),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::KvStore;
    use crate::condenser::{CharTokenEstimator, RecentWindowCondenser, TokenEstimator};

    /// A miniature corpus in the LongMemEval schema (including an extra unknown
    /// key `metadata` to prove the loader tolerates the real dataset's shape).
    /// Case `recent`: the answer is in the NEWEST turn. Case `deep`: the answer is
    /// in the OLDEST session, behind lots of chatter.
    const FIXTURE: &str = r#"[
      {
        "question_id": "recent",
        "question_type": "single-session-user",
        "question": "what is my dog's name?",
        "answer": "Rex",
        "metadata": {"ignored": true},
        "haystack_session_ids": ["s0"],
        "haystack_sessions": [
          [
            {"role": "user", "content": "hello there"},
            {"role": "assistant", "content": "hi!"},
            {"role": "user", "content": "my dog is named Rex", "has_answer": true}
          ]
        ]
      },
      {
        "question_id": "deep",
        "question_type": "multi-session",
        "question": "where do I work?",
        "answer": "Acme",
        "haystack_session_ids": ["s0", "s1"],
        "haystack_sessions": [
          [
            {"role": "user", "content": "I work at Acme Corp", "has_answer": true},
            {"role": "assistant", "content": "noted"}
          ],
          [
            {"role": "user", "content": "chatter one"},
            {"role": "assistant", "content": "ok one"},
            {"role": "user", "content": "chatter two"},
            {"role": "assistant", "content": "ok two"}
          ]
        ]
      }
    ]"#;

    fn case(cases: &[LongMemEvalCase], id: &str) -> LongMemEvalCase {
        cases.iter().find(|c| c.question_id == id).unwrap().clone()
    }

    #[test]
    fn parses_the_schema_including_unknown_fields() {
        let cases = parse_cases(FIXTURE).unwrap();
        assert_eq!(cases.len(), 2);
        let recent = case(&cases, "recent");
        assert_eq!(recent.answer_turns(), vec!["my dog is named Rex"]);
        assert_eq!(recent.haystack_sessions.len(), 1);
        assert_eq!(case(&cases, "deep").haystack_sessions.len(), 2);
    }

    #[tokio::test]
    async fn ingest_is_idempotent_and_counts_every_turn() {
        let store = KvStore::open_memory().await.unwrap();
        let log = store.events();
        let deep = case(&parse_cases(FIXTURE).unwrap(), "deep");
        // 2 + 4 = 6 turns.
        assert_eq!(ingest_case(&log, "u1", &deep).await.unwrap(), 6);
        // Re-ingest (same content-stable ids) → still 6 events in the log.
        ingest_case(&log, "u1", &deep).await.unwrap();
        let replayed = replay_history(&log, &EventQuery::owner("u1"))
            .await
            .unwrap();
        assert_eq!(replayed.len(), 6, "idempotent: no duplication");
    }

    #[tokio::test]
    async fn end_to_end_recent_answer_is_recalled_deep_answer_is_the_baseline() {
        // 装载跑通: load → ingest → replay → condense → measure, end to end.
        let store = KvStore::open_memory().await.unwrap();
        let log = store.events();
        let cases = parse_cases(FIXTURE).unwrap();
        let condenser = RecentWindowCondenser::default();

        // The RECENT case: answer is the newest turn, so a tight budget keeps it.
        let recent = case(&cases, "recent");
        ingest_case(&log, "recent-owner", &recent).await.unwrap();
        let out = run_case(&log, "recent-owner", &recent, &condenser, 1)
            .await
            .unwrap();
        assert!(
            out.answer_in_view,
            "newest answer must be recalled: {out:?}"
        );
        assert!(out.lossless);

        // The DEEP case: answer is in the oldest session. A recent-window
        // condenser at a tight budget CANNOT surface it — the honest baseline
        // (MD-3's retriever is what will move this to true). Still lossless.
        let deep = case(&cases, "deep");
        ingest_case(&log, "deep-owner", &deep).await.unwrap();
        let budget = CharTokenEstimator.estimate(&[Msg::user("chatter two")]);
        let out = run_case(&log, "deep-owner", &deep, &condenser, budget)
            .await
            .unwrap();
        assert!(
            !out.answer_in_view,
            "recent window cannot reach a deep answer — baseline is FALSE: {out:?}"
        );
        assert!(
            out.lossless,
            "no loss even when the answer is folded: {out:?}"
        );
        assert_eq!(out.total_turns, 6);
    }

    #[tokio::test]
    async fn deep_answer_that_is_a_substring_of_a_recent_turn_is_not_falsely_recalled() {
        // B1: the answer "Paris" is the OLDEST (deep) turn; a RECENT turn happens
        // to contain "Paris" as a substring. Substring matching would wrongly
        // report the deep answer as recalled (over-reporting the baseline). Index
        // matching must report FALSE — the deep turn really is folded away.
        let json = r#"[{
          "question_id": "sub",
          "question": "where?",
          "answer": "Paris",
          "haystack_sessions": [[
            {"role": "user", "content": "Paris", "has_answer": true},
            {"role": "assistant", "content": "noted"},
            {"role": "user", "content": "I love Paris in the spring"}
          ]]
        }]"#;
        let store = KvStore::open_memory().await.unwrap();
        let log = store.events();
        let c = case(&parse_cases(json).unwrap(), "sub");
        ingest_case(&log, "u1", &c).await.unwrap();
        let condenser = RecentWindowCondenser::default();
        let out = run_case(&log, "u1", &c, &condenser, 1).await.unwrap();
        // Only the newest turn survives at budget=1; the deep "Paris" is folded.
        assert!(
            !out.answer_in_view,
            "substring overlap must not report the folded deep answer: {out:?}"
        );
        assert!(out.lossless);
    }

    #[test]
    fn load_from_missing_file_is_an_io_error_not_a_panic() {
        let err = load_cases_from_file(Path::new("/nonexistent/longmemeval.json")).unwrap_err();
        assert!(matches!(err, MemoryError::Io(_)), "{err}");
    }
}
