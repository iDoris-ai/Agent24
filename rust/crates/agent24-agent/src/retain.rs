//! Rule based extraction for explicit user requests to remember something.

use agent24_memory::{
    KvStore,
    artifact::checksum,
    event::{EventId, Origin, Scope, Trust},
    writer::{Candidate, MemoryWriter},
};
use serde_json::json;

/// Put a phrase before its shorter prefix so the more specific form wins.
const REMEMBER_PREFIXES: &[&str] = &["请记住", "remember that ", "记住", "remember "];

/// M1-T10 review H1: injected alongside recall, at run start, when
/// [`explicit_remember`] matches the prompt but personal memory is paused —
/// so the model does not go on to tell the user it remembered something it
/// did not. The actual write is independently gated too ([`persist`]'s own
/// early check): this is belt-and-braces, not the only thing stopping it.
pub(super) const PAUSED_WRITE_NOTICE: &str = "记忆已暂停：本条不会被记住，请如实告知用户";

/// Return only the user's explicitly requested text. The model response is not
/// an input to this parser.
pub(super) fn explicit_remember(prompt: &str) -> Option<&str> {
    if prompt.trim_end() == "remember that" {
        return None;
    }
    let body = REMEMBER_PREFIXES
        .iter()
        .find_map(|prefix| prompt.strip_prefix(prefix))?;
    let body = body.trim();
    (!body.is_empty()).then_some(body)
}

/// Persist a user statement with the frozen SHA-256(owner ‖ object) identity.
pub(super) async fn persist(
    kv: &KvStore,
    owner: &str,
    object: &str,
    evidence: EventId,
    origin: Origin,
) -> agent24_memory::Result<()> {
    // Only a direct user run can authorize a qualified personal assertion.
    // Validate the propagated origin here so future callers cannot
    // accidentally turn model or scheduled content into user memory.
    if origin.trust != Trust::UserSaid {
        return Ok(());
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

    kv.write_gate().propose(vec![candidate]).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

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
        ];

        for (prompt, expected) in cases {
            assert_eq!(explicit_remember(prompt), expected, "prompt: {prompt:?}");
        }
    }

    #[tokio::test]
    async fn model_origin_cannot_persist_a_qualified_remember_assertion() {
        let kv = KvStore::open_memory().await.unwrap();
        persist(
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
        persist(
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

        let beliefs = kv
            .assertions()
            .beliefs_as_of(&BeliefQuery::owner("owner"))
            .await
            .unwrap();
        assert!(beliefs.is_empty(), "paused memory must reject the write");

        // Negative control: the SAME statement, for an owner who never
        // paused, is written — proving the gate (not something else) is what
        // rejected the paused owner's write above.
        persist(
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
        let beliefs2 = kv
            .assertions()
            .beliefs_as_of(&BeliefQuery::owner("owner2"))
            .await
            .unwrap();
        assert_eq!(beliefs2.len(), 1, "an un-paused owner's write still lands");
    }
}
