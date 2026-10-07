//! Rule based extraction for explicit user requests to remember something.
//!
//! **Known boundary (M1-T10 review R4, non-blocking, recorded for P1):** the
//! personal-memory pause switch gates exactly two things today — this
//! module's [`persist`] (new assertion writes) and
//! `agent24_agent::SessionMemory::recall` (cross-session recall). It does
//! NOT stop the raw conversation (`mem_events`) from being journaled while
//! paused — that is correct today (nothing currently extracts facts from raw
//! events; those two gates cover M1's entire surface), but a future P1
//! auto-extraction pass over `mem_events` would need to either skip events
//! recorded while paused or accept that they are fair game, since nothing
//! marks them as "recorded while paused" today.

use agent24_memory::{
    KvStore,
    artifact::checksum,
    event::{EventId, Origin, Scope, Trust},
    writer::{Candidate, MemoryWriter, WriteDecision},
};
use serde_json::json;

/// fix683 (PR #683 review): the live result of [`persist`] — a tri-state
/// replacing the old `Result<()>` that returned the SAME `Ok(())` whether
/// the assertion was actually committed or skipped because personal memory
/// was paused at commit time. Callers (ultimately `/api/v1/chat`'s
/// `memory_receipt`) must derive their answer from THIS, never from a
/// pre-model-call pause snapshot — pause can be toggled while the model is
/// still generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetainOutcome {
    /// The assertion is durably persisted (including an idempotent reuse of
    /// an identical already-current commit, or a reassert after forget).
    Saved,
    /// The personal-memory pause switch was found active inside the write
    /// transaction; nothing was persisted.
    SkippedPaused,
    /// This call was never going to write anything, independent of pause:
    /// either the prompt was not an explicit remember, or the origin is not
    /// a trusted `UserSaid` provenance.
    NotApplicable,
}

/// M1-T10 review H1: injected alongside recall, at run start, when
/// [`explicit_remember`] matches the prompt but personal memory is paused —
/// so the model does not go on to tell the user it remembered something it
/// did not. The actual write is independently gated too ([`persist`]'s own
/// early check): this is belt-and-braces, not the only thing stopping it.
pub(super) const PAUSED_WRITE_NOTICE: &str = "记忆已暂停：本条不会被记住，请如实告知用户";

/// Check if the extracted content looks like a question sentence (should be rejected).
/// Returns true if content ends with question particles/marks or starts with 了.
fn is_question_sentence(text: &str) -> bool {
    let trimmed = text.trim();

    // Check if ends with question particles or marks
    if trimmed.ends_with('吗')
        || trimmed.ends_with('么')
        || trimmed.ends_with('呢')
        || trimmed.ends_with('？')
        || trimmed.ends_with('?')
    {
        return true;
    }

    // Check if ends with sentence-final negation/doubt phrases
    if trimmed.ends_with("了吗")
        || trimmed.ends_with("了没")
        || trimmed.ends_with("了没有")
        || trimmed.ends_with("没有")
    {
        return true;
    }

    // Check if content starts with 了 (e.g., after verb stripping)
    if trimmed.starts_with('了') {
        return true;
    }

    false
}

/// Return only the user's explicitly requested text. The model response is not
/// an input to this parser.
pub(super) fn explicit_remember(prompt: &str) -> Option<&str> {
    const ADDRESS_PREFIXES_CN: &[&str] = &[
        "你帮我",
        "请帮我",
        "请你",
        "麻烦你",
        "帮我",
        "请",
        "麻烦",
        "你",
    ];
    const REMEMBER_VERBS_CN: &[&str] = &["记一下", "记住", "记好", "记着"];
    const SEPARATORS: &[char] = &[' ', '　', '，', ',', '：', ':', '、'];
    const END_PUNCT: &[char] = &['。', '.', '！', '!'];

    let prompt_trimmed = prompt.trim_end();
    if prompt_trimmed == "remember that" {
        return None;
    }

    let mut remaining = prompt;

    // Try to strip optional Chinese address prefixes
    for prefix in ADDRESS_PREFIXES_CN {
        if remaining.starts_with(prefix) {
            remaining = &remaining[prefix.len()..];
            break;
        }
    }

    // For English: try to strip "please " (case-insensitive) but only if followed by
    // "remember that " to avoid matching "please remember" alone
    if remaining == prompt {
        // Only try if we haven't matched a Chinese prefix
        let lower = remaining.to_lowercase();
        if lower.starts_with("please ")
            && lower[7..].starts_with("remember that ")
            && let Some(after_please) = strip_prefix_insensitive(remaining, "please ")
        {
            remaining = after_please;
        }
    }

    remaining = remaining.trim();

    // Try to match Chinese remember verbs
    for verb in REMEMBER_VERBS_CN {
        if let Some(after_verb) = remaining.strip_prefix(verb) {
            // Check if the verb is immediately followed by 了 or 吗 (these are questions)
            // Also skip if followed by 没 but NOT 没有 (e.g., "记住没" is question, but "记住没有..." is content)
            if after_verb.starts_with("了")
                || after_verb.starts_with("吗")
                || (after_verb.starts_with("没") && !after_verb.starts_with("没有"))
            {
                continue;
            }

            let body = after_verb.trim_start_matches(SEPARATORS);
            let body = body.trim();

            if !body.is_empty() {
                let body = body.trim_end_matches(END_PUNCT);
                if !body.is_empty() {
                    // Reject if it looks like a question sentence
                    if is_question_sentence(body) {
                        return None;
                    }
                    return Some(body);
                }
            }
            return None;
        }
    }

    // Try to match English "remember that " (case-insensitive)
    if let Some(body) = strip_prefix_insensitive(remaining, "remember that ") {
        let body = body.trim();
        if !body.is_empty() {
            let body = body.trim_end_matches(END_PUNCT);
            if !body.is_empty() {
                // Reject if it looks like a question sentence
                if is_question_sentence(body) {
                    return None;
                }
                return Some(body);
            }
        }
        return None;
    }

    // Try to match English "remember " (case-insensitive)
    if let Some(body) = strip_prefix_insensitive(remaining, "remember ") {
        let body = body.trim();
        if !body.is_empty() {
            let body = body.trim_end_matches(END_PUNCT);
            if !body.is_empty() {
                // Reject if it looks like a question sentence
                if is_question_sentence(body) {
                    return None;
                }
                return Some(body);
            }
        }
        return None;
    }

    None
}

/// Helper function to strip prefix with case-insensitive matching.
fn strip_prefix_insensitive<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let s_lower = s.to_lowercase();
    let prefix_lower = prefix.to_lowercase();
    if s_lower.starts_with(&prefix_lower) {
        let char_count = prefix.chars().count();
        let mut char_iter = s.chars();
        for _ in 0..char_count {
            char_iter.next();
        }
        Some(char_iter.as_str())
    } else {
        None
    }
}

/// Persist a user statement with the frozen SHA-256(owner ‖ object) identity.
/// fix683: returns the LIVE [`RetainOutcome`] from the write-gate's decision
/// — not a bare `Ok(())` that could mean either "committed" or "rolled back
/// because paused". `propose()` decides per-candidate at commit time, inside
/// the same transaction the pause re-check runs in; this maps that decision
/// 1:1 instead of discarding it (the bug this fix closes: the old code threw
/// the `Vec<WriteDecision>` away and always reported success).
pub(super) async fn persist(
    kv: &KvStore,
    owner: &str,
    object: &str,
    evidence: EventId,
    origin: Origin,
) -> agent24_memory::Result<RetainOutcome> {
    // Only a direct user run can authorize a qualified personal assertion.
    // Validate the propagated origin here so future callers cannot
    // accidentally turn model or scheduled content into user memory.
    if origin.trust != Trust::UserSaid {
        return Ok(RetainOutcome::NotApplicable);
    }
    // M1-T10 review M2: the personal-memory pause switch used to be checked
    // HERE, before the candidate was even built — outside any transaction,
    // so a PUT disabling memory between this check and the write landing
    // could still let the write through (TOCTOU). The check now lives
    // inside `WriteGate::commit_with_audit`'s own `BEGIN IMMEDIATE`
    // transaction (`agent24_memory::writer`), atomic with the write it
    // gates — this is still the retain entry point's only call into the
    // write path, so the gate is still "one early check at retain", just
    // one transaction deeper.
    let id = checksum(&format!("{owner}{object}"));
    let candidate = Candidate::new(
        id,
        Scope::owner(owner),
        "user",
        "said_to_remember",
        json!(object),
        origin,
    )
    .with_evidence(vec![evidence])
    .remember();

    let decisions = kv.write_gate().propose(vec![candidate]).await?;
    Ok(match decisions.into_iter().next() {
        Some(WriteDecision::Committed(_)) => RetainOutcome::Saved,
        Some(WriteDecision::SkippedPaused(_)) => RetainOutcome::SkippedPaused,
        // Unreachable today: `WriteGate::policy` always maps `UserSaid` +
        // `explicit_remember` (with non-empty evidence, which this
        // candidate always has) to `Outcome::Commit`, never Hold/Reject.
        // Handled explicitly rather than folded into `Saved` (PR #683
        // review point 4) so a future policy change cannot silently start
        // reporting a non-write as a successful remember.
        Some(WriteDecision::Held(_)) | Some(WriteDecision::Rejected { .. }) | None => {
            tracing::error!(
                owner,
                "explicit remember produced an unexpected write decision; \
                 policy should always Commit or skip-paused for UserSaid+remember"
            );
            RetainOutcome::NotApplicable
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::RetainOutcome;
    use super::explicit_remember;
    use super::persist;
    use agent24_memory::event::{Origin, Trust};
    use agent24_memory::{
        KvStore,
        assertion::{AssertionStore, BeliefQuery},
    };

    #[test]
    fn recognizes_only_the_supported_leading_forms() {
        let cases = [
            // Existing cases
            ("记住我对花生过敏", Some("我对花生过敏")),
            ("请记住 我住在上海", Some("我住在上海")),
            (
                "remember my birthday is May 2",
                Some("my birthday is May 2"),
            ),
            ("remember that I prefer tea", Some("I prefer tea")),
            ("remember thatched roof", Some("thatched roof")),
            ("remember that", None),
            ("remember ", None),
            ("记住   ", None),
            ("please remember my birthday", None),
            ("I remember that I prefer tea", None),
            ("rememberable facts", None),
            // New positive cases
            ("你记住，我对花生过敏。", Some("我对花生过敏")),
            ("请你记住：我住在上海", Some("我住在上海")),
            ("帮我记一下 明天下午三点开会", Some("明天下午三点开会")),
            ("麻烦记住我不吃辣", Some("我不吃辣")),
            ("Please remember that my dog is Max", Some("my dog is Max")),
            ("记住没有人会来接你", Some("没有人会来接你")),
            ("你帮我记住我不吃香菜", Some("我不吃香菜")),
            ("请帮我记住下周二开会", Some("下周二开会")),
            // Negative cases: questions should NOT be remembered
            ("你记住我吗", None),
            ("你记住这件事吗", None),
            ("你记住他的名字了吗", None),
            ("记住了没有", None),
            ("你记住了吗？", None),
            // More question cases
            ("你还记得我对什么过敏吗", None),
            ("我记住了", None),
            ("记住了吗", None),
            ("你记住了没有", None),
            ("do you remember my name", None),
            ("remembered", None),
        ];

        for (prompt, expected) in cases {
            assert_eq!(explicit_remember(prompt), expected, "prompt: {prompt:?}");
        }
    }

    #[tokio::test]
    async fn model_origin_cannot_persist_a_qualified_remember_assertion() {
        let kv = KvStore::open_memory().await.unwrap();
        let outcome = persist(
            &kv,
            "owner",
            "I am allergic to peanuts",
            "event-1".into(),
            Origin {
                source: "scheduler".into(),
                trust: Trust::Model,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            RetainOutcome::NotApplicable,
            "a non-UserSaid origin must never report Saved"
        );

        let beliefs = kv
            .assertions()
            .beliefs_as_of(&BeliefQuery::owner("owner"))
            .await
            .unwrap();
        assert!(beliefs.is_empty());
    }

    #[tokio::test]
    async fn paused_memory_rejects_a_new_write() {
        let kv = KvStore::open_memory().await.unwrap();
        kv.set_memory_enabled("owner", false).await.unwrap();
        let outcome = persist(
            &kv,
            "owner",
            "I am allergic to peanuts",
            "event-1".into(),
            Origin {
                source: "test".into(),
                trust: Trust::UserSaid,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            RetainOutcome::SkippedPaused,
            "fix683: the live result must say SkippedPaused, not look like Saved"
        );

        let beliefs = kv
            .assertions()
            .beliefs_as_of(&BeliefQuery::owner("owner"))
            .await
            .unwrap();
        assert!(beliefs.is_empty(), "paused memory must reject the write");

        // Negative control: the SAME statement, for an owner who never
        // paused, is written — proving the gate (not something else) is what
        // rejected the paused owner's write above.
        let outcome2 = persist(
            &kv,
            "owner2",
            "I am allergic to peanuts",
            "event-2".into(),
            Origin {
                source: "test".into(),
                trust: Trust::UserSaid,
            },
        )
        .await
        .unwrap();
        assert_eq!(outcome2, RetainOutcome::Saved);
        let beliefs2 = kv
            .assertions()
            .beliefs_as_of(&BeliefQuery::owner("owner2"))
            .await
            .unwrap();
        assert_eq!(beliefs2.len(), 1, "an un-paused owner's write still lands");
    }
}
