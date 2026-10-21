//! The resources a module tool call may touch (K1-06a §2, on `ab/kernel`):
//! the kernel's `authorized_resources`, read once per call and never kept.
//!
//! Each entry is `<kind>:<id>`; the only kind this module grants is `doc`,
//! its id the stored document id (`doc:doc_01K…`). An empty list grants
//! nothing, and an absent one is empty. Any other shape refuses the whole
//! call (`permission_denied`) before storage is read: it is a host fault,
//! not a missing resource. Jobs and extractions are never listed: each is
//! reachable through the document it belongs to (§2.3).
//!
//! Outside the scope reads exactly as absent (§2.4): the caller answers with
//! the service's own not-found error, never naming the id.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::error::ApiError;
use crate::state::Storage;

/// The kernel's code for a call the host did not authorize (kernel code
/// table, `agent24-domain/src/tool.rs`).
pub const PERMISSION_DENIED: &str = "permission_denied";

/// At most this many entries per call (K1-06a §2.1).
pub const MAX_ENTRIES: usize = 256;

/// The one root kind this module grants.
pub const DOC_KIND: &str = "doc";

/// The documents one call may touch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    documents: BTreeSet<String>,
}

/// `authorized_resources` is not a list this module can read: refuse the
/// whole call.
#[derive(Debug, PartialEq, Eq)]
pub struct Malformed;

impl Scope {
    /// Reads `authorized_resources` as the kernel sent it: `None` when the
    /// field is absent.
    ///
    /// # Errors
    ///
    /// [`Malformed`] unless it is an array of at most [`MAX_ENTRIES`]
    /// strings, each `<kind>:<id>` with `kind` matching
    /// `[a-z][a-z0-9_]{0,31}`, `id` matching `[A-Za-z0-9_-]{1,128}` (split at
    /// the first `:`, case-sensitive) and `kind` equal to [`DOC_KIND`].
    /// `null` is malformed, not absent.
    pub fn parse(raw: Option<&Value>) -> Result<Self, Malformed> {
        let Some(raw) = raw else {
            return Ok(Self::default());
        };
        let Value::Array(entries) = raw else {
            return Err(Malformed);
        };
        if entries.len() > MAX_ENTRIES {
            return Err(Malformed);
        }
        let mut documents = BTreeSet::new();
        for entry in entries {
            let Value::String(entry) = entry else {
                return Err(Malformed);
            };
            let Some((kind, id)) = entry.split_once(':') else {
                return Err(Malformed);
            };
            // Split at the first `:`; a later `:` fails the id check.
            if !is_entry_kind(kind) || kind != DOC_KIND || !is_entry_id(id) {
                return Err(Malformed);
            }
            documents.insert(id.to_owned());
        }
        Ok(Self { documents })
    }

    /// Whether `id` (a stored document id) is in the scope.
    #[must_use]
    pub fn allows_document(&self, id: &str) -> bool {
        self.documents.contains(id)
    }

    /// The documents in the scope, in byte order.
    #[must_use]
    pub fn documents(&self) -> &BTreeSet<String> {
        &self.documents
    }
}

/// The document a job belongs to: `None` when there is no such job,
/// `Some(None)` when the job has no document.
///
/// # Errors
///
/// The storage error, as the service layer reports it.
pub(crate) async fn job_document(
    storage: &Storage,
    job_id: &str,
) -> Result<Option<Option<String>>, ApiError> {
    let row: Result<Option<Option<String>>, sqlx::Error> =
        sqlx::query_scalar("SELECT document_id FROM jobs WHERE id = ?1")
            .bind(job_id)
            .fetch_optional(storage.db.pool())
            .await;
    match row {
        Ok(found) => Ok(found),
        Err(e) => Err(storage.db_failure(e).await),
    }
}

/// The document an extraction belongs to: `None` when there is no such
/// extraction.
///
/// # Errors
///
/// The storage error, as the service layer reports it.
pub(crate) async fn extraction_document(
    storage: &Storage,
    extraction_id: &str,
) -> Result<Option<String>, ApiError> {
    let row: Result<Option<String>, sqlx::Error> =
        sqlx::query_scalar("SELECT document_id FROM extractions WHERE id = ?1")
            .bind(extraction_id)
            .fetch_optional(storage.db.pool())
            .await;
    match row {
        Ok(found) => Ok(found),
        Err(e) => Err(storage.db_failure(e).await),
    }
}

/// `[a-z][a-z0-9_]{0,31}`.
fn is_entry_kind(kind: &str) -> bool {
    let bytes = kind.as_bytes();
    (1..=32).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

/// `[A-Za-z0-9_-]{1,128}`, ASCII only, so a further `:` does not match.
fn is_entry_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (1..=128).contains(&bytes.len())
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::json;

    const A: &str = "doc_01K74Z3QJ8V5N2W9RTX6Y00001";

    #[test]
    fn absent_is_empty_and_grants_nothing() {
        let scope = Scope::parse(None).unwrap();
        assert!(scope.documents().is_empty());
        assert!(!scope.allows_document(A));
        assert_eq!(Scope::parse(Some(&json!([]))).unwrap(), scope);
    }

    #[test]
    fn doc_entries_grant_exactly_their_ids() {
        let raw = json!([format!("doc:{A}"), "doc:doc_x-1_Y", format!("doc:{A}")]);
        let scope = Scope::parse(Some(&raw)).unwrap();
        assert!(scope.allows_document(A));
        assert!(scope.allows_document("doc_x-1_Y"));
        assert!(!scope.allows_document(&A.to_lowercase()));
        assert!(!scope.allows_document("doc_01K74Z3QJ8V5N2W9RTX6Y0000"));
        assert_eq!(scope.documents().len(), 2);
        let max = format!("doc:{}", "a".repeat(128));
        assert!(Scope::parse(Some(&json!([max]))).is_ok());
        let kind = format!("doc:{A}");
        assert!(Scope::parse(Some(&json!(vec![kind; MAX_ENTRIES]))).is_ok());
    }

    #[test]
    fn any_other_shape_is_malformed() {
        let long_id = format!("doc:{}", "a".repeat(129));
        let long_kind = format!("{}:x", "a".repeat(33));
        let too_many: Vec<String> = (0..=MAX_ENTRIES).map(|n| format!("doc:d{n}")).collect();
        for raw in [
            json!(null),
            json!(format!("doc:{A}")),
            json!({ "doc": A }),
            json!([1]),
            json!([null]),
            json!(["doc:*"]),
            json!(["doc:"]),
            json!([":x"]),
            json!(["doc"]),
            json!(["Doc:x"]),
            json!(["doc:a b"]),
            json!(["doc:a:b"]),
            json!(["doc:é"]),
            json!(["1doc:x"]),
            json!(["job:x"]),
            json!(["doc_01"]),
            json!([long_id]),
            json!([long_kind]),
            json!(too_many),
        ] {
            assert_eq!(Scope::parse(Some(&raw)), Err(Malformed), "{raw}");
        }
    }
}
