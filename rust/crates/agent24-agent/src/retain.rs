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
) -> agent24_memory::Result<()> {
    let id = checksum(&format!("{owner}{object}"));
    let candidate = Candidate::new(
        id,
        Scope::owner(owner),
        "user",
        "said_to_remember",
        json!(object),
        Origin {
            source: "explicit_remember".into(),
            trust: Trust::UserSaid,
        },
    )
    .with_evidence(vec![evidence])
    .remember();

    kv.write_gate().propose(vec![candidate]).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::explicit_remember;

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
}
