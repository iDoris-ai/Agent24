//! ME4-S3 §2.4/§3.5 — `MemoryClient` (`_a24/memory/private/{remember,recall,
//! recent}`) and `remember_once` (§8 Q5/H3): `remember` is not idempotent on
//! the wire (every success mints a new id), so a caller that must write a
//! summary at most once per logical key needs the pre-check-by-recall
//! algorithm this module carries.

use std::sync::Arc;

use agent24_os_proto::module::Connection;
use serde::Deserialize;
use serde_json::{Map, Value};

use super::{Core, malformed_result, set_opt};
use crate::context::RequestId;
use crate::error::ClientError;

pub const REMEMBER: &str = "_a24/memory/private/remember";
pub const RECALL: &str = "_a24/memory/private/recall";
pub const RECENT: &str = "_a24/memory/private/recent";
pub(crate) const METHODS: [&str; 3] = [REMEMBER, RECALL, RECENT];
const OFFER_PREFIX: &str = "_a24/memory/private/";

/// The body field `remember_once` writes its dedup marker into — the same
/// key Sin90's pre-migration `reconciler.rs` used, so memories written before
/// and after a migration to this client still recognise each other.
pub const DEDUP_KEY_FIELD: &str = "dedup_key";
/// How many `recall` pages `remember_once` will turn before giving up and
/// answering [`RememberOnce::Inconclusive`] rather than guessing "absent".
pub const RECALL_PRECHECK_MAX_PAGES: usize = 10;
pub const RECALL_PRECHECK_PAGE_SIZE: usize = 50;

/// What the kernel stored (`_a24/memory/private/remember`'s result).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Remembered {
    pub id: String,
    pub at: String,
}

/// One result from a recall/recent page.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Recollection {
    pub id: String,
    pub kind: String,
    pub body: Map<String, Value>,
    pub at: String,
}

/// One page of `recall`/`recent`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RecallPage {
    pub items: Vec<Recollection>,
    pub cursor: Option<String>,
}

/// [`MemoryClient::remember_once`]'s outcome — three states, not two:
/// "found" and "created" are not the only possibilities, because the
/// pre-check itself can run out of budget without a definite answer.
#[derive(Debug, Clone, PartialEq)]
pub enum RememberOnce {
    /// No marker found in the scanned history; `remember` was called.
    Created { id: String, at: String },
    /// A memory whose `body.dedup_key` equals the marker already exists;
    /// nothing was sent.
    Found { id: String },
    /// [`RECALL_PRECHECK_MAX_PAGES`] pages were scanned and `cursor` was
    /// still set: genuinely unknown, NOT "absent" — nothing was sent.
    Inconclusive,
}

pub struct MemoryClient(Core);

impl MemoryClient {
    #[must_use]
    pub fn new(conn: &Arc<Connection>) -> Option<Self> {
        Core::new(conn, OFFER_PREFIX).map(Self)
    }

    /// NOT idempotent on the wire: every success mints a new id. See
    /// [`Self::remember_once`] for a caller that needs "at most once".
    ///
    /// # Errors
    /// See [`ClientError`].
    pub async fn remember(
        &self,
        kind: &str,
        body: Map<String, Value>,
        request_id: Option<&RequestId>,
    ) -> Result<Remembered, ClientError> {
        let mut params = Map::new();
        params.insert("kind".to_owned(), Value::String(kind.to_owned()));
        params.insert("body".to_owned(), Value::Object(body));
        set_opt(
            &mut params,
            "request_id",
            request_id.map(|id| Value::String(id.as_str().to_owned())),
        );
        let value = self.0.call(REMEMBER, Value::Object(params)).await?;
        serde_json::from_value(value).map_err(|e| malformed_result("remember result", e))
    }

    /// # Errors
    /// See [`ClientError`].
    pub async fn recall(
        &self,
        query: &str,
        page_size: usize,
        cursor: Option<&str>,
        request_id: Option<&RequestId>,
    ) -> Result<RecallPage, ClientError> {
        self.page(RECALL, Some(query), page_size, cursor, request_id)
            .await
    }

    /// # Errors
    /// See [`ClientError`].
    pub async fn recent(
        &self,
        page_size: usize,
        cursor: Option<&str>,
        request_id: Option<&RequestId>,
    ) -> Result<RecallPage, ClientError> {
        self.page(RECENT, None, page_size, cursor, request_id).await
    }

    async fn page(
        &self,
        method: &'static str,
        query: Option<&str>,
        page_size: usize,
        cursor: Option<&str>,
        request_id: Option<&RequestId>,
    ) -> Result<RecallPage, ClientError> {
        let mut params = Map::new();
        if let Some(query) = query {
            params.insert("query".to_owned(), Value::String(query.to_owned()));
        }
        params.insert("page_size".to_owned(), Value::from(page_size));
        set_opt(
            &mut params,
            "cursor",
            cursor.map(|c| Value::String(c.to_owned())),
        );
        set_opt(
            &mut params,
            "request_id",
            request_id.map(|id| Value::String(id.as_str().to_owned())),
        );
        let value = self.0.call(method, Value::Object(params)).await?;
        serde_json::from_value(value).map_err(|e| malformed_result("recall/recent page", e))
    }

    /// Write `body` at most once per `dedup_key` (§8 Q5/H3): pre-checks by
    /// `recall`ing `dedup_key` up to [`RECALL_PRECHECK_MAX_PAGES`] pages
    /// before falling back to `remember`.
    ///
    /// `body` must either omit [`DEDUP_KEY_FIELD`] or already carry the same
    /// value as `dedup_key` — a body that names a DIFFERENT dedup key is
    /// rejected locally, before any call is made, since sending it would
    /// silently orphan the pre-check.
    ///
    /// Not atomic: the pre-check and the write are not one transaction, so
    /// the caller must ensure at most one call for a given `dedup_key` is in
    /// flight at a time (§3.5 — Sin90's single outbox pump makes this true
    /// by construction; a caller with multiple writers needs its own
    /// serialization, e.g. one outbox row per `dedup_key` drained by a
    /// single pump task).
    ///
    /// # Errors
    /// [`ClientError::InvalidParams`] for the body/`dedup_key` mismatch
    /// above; otherwise whatever `recall`/`remember` returned.
    pub async fn remember_once(
        &self,
        kind: &str,
        dedup_key: &str,
        mut body: Map<String, Value>,
        request_id: Option<&RequestId>,
    ) -> Result<RememberOnce, ClientError> {
        if let Some(existing) = body.get(DEDUP_KEY_FIELD)
            && existing.as_str() != Some(dedup_key)
        {
            return Err(ClientError::InvalidParams(format!(
                "body.{DEDUP_KEY_FIELD} does not match dedup_key"
            )));
        }
        body.insert(
            DEDUP_KEY_FIELD.to_owned(),
            Value::String(dedup_key.to_owned()),
        );

        // B2 (external review of #516): the kernel's `recall` substring-
        // matches the query against `serde_json::to_string(&item.body)`
        // (`agent24d/src/os_memory_page.rs::item_matches_substring`) — the
        // JSON-escaped form of the body, not the raw field value. A
        // `dedup_key` containing `"`, `\`, or a control character never
        // appears verbatim in that escaped string, so the pre-check would
        // never find its own previous write and `remember_once` would
        // duplicate it forever. Escaping the query the same way the kernel
        // escapes the stored body fixes that; the exact-equality check below
        // still compares the RAW `dedup_key` against `body.dedup_key`
        // (`Recollection::body` is already parsed JSON, not the escaped
        // wire string), so that comparison is unaffected.
        let recall_query = json_escaped_for_substring_match(dedup_key);

        let mut cursor: Option<String> = None;
        for _ in 0..RECALL_PRECHECK_MAX_PAGES {
            let page = self
                .recall(
                    &recall_query,
                    RECALL_PRECHECK_PAGE_SIZE,
                    cursor.as_deref(),
                    request_id,
                )
                .await?;
            if let Some(item) = page.items.iter().find(|item| {
                item.body.get(DEDUP_KEY_FIELD).and_then(Value::as_str) == Some(dedup_key)
            }) {
                return Ok(RememberOnce::Found {
                    id: item.id.clone(),
                });
            }
            match page.cursor {
                Some(next) => cursor = Some(next),
                None => {
                    let remembered = self.remember(kind, body, request_id).await?;
                    return Ok(RememberOnce::Created {
                        id: remembered.id,
                        at: remembered.at,
                    });
                }
            }
        }
        Ok(RememberOnce::Inconclusive)
    }
}

/// `serde_json::to_string` on a `&str` always produces a quoted JSON string
/// literal (`"..."`), so stripping exactly one leading and one trailing byte
/// recovers just the escaped body — the same bytes that appear inside
/// `serde_json::to_string(&item.body)` on the kernel side for this field's
/// value. The `unwrap_or_else`/`unwrap_or` fallbacks below are unreachable
/// in practice (`String` -> JSON string never fails, and the output of that
/// serialization always starts and ends with `"`); they exist only so this
/// stays panic-free without reaching for `unwrap`/`expect` (denied outside
/// tests).
fn json_escaped_for_substring_match(key: &str) -> String {
    let quoted = serde_json::to_string(key).unwrap_or_else(|_| format!("\"{key}\""));
    quoted
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(&quoted)
        .to_owned()
}

#[cfg(all(test, feature = "test-util"))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;
    use crate::testing;

    #[tokio::test]
    async fn without_the_offer_prefix_new_returns_none() {
        let (conn, _peer) = testing::fake_kernel(vec![]).await;
        assert!(MemoryClient::new(&conn).is_none());
    }

    #[tokio::test]
    async fn remember_sends_kind_and_body_and_parses_the_result() {
        let (conn, mut peer) = testing::fake_kernel(vec![OFFER_PREFIX.to_owned()]).await;
        let client = MemoryClient::new(&conn).expect("offer covers memory");
        let mut body = Map::new();
        body.insert("text".to_owned(), json!("hello"));
        let call = tokio::spawn(async move { client.remember("note", body, None).await });
        let req = testing::read_request(&mut peer).await;
        assert_eq!(req["method"], REMEMBER);
        assert_eq!(req["params"]["kind"], "note");
        assert_eq!(req["params"]["body"]["text"], "hello");
        testing::respond(
            &mut peer,
            &req,
            json!({"id": "osmem:1", "at": "2026-01-01T00:00:00Z"}),
        )
        .await;
        let remembered = call.await.unwrap().unwrap();
        assert_eq!(remembered.id, "osmem:1");
    }

    fn page(items: Vec<Value>, cursor: Option<&str>) -> Value {
        json!({"items": items, "cursor": cursor})
    }

    fn item(id: &str, dedup_key: &str) -> Value {
        json!({"id": id, "kind": "review.summary", "body": {"dedup_key": dedup_key}, "at": "2026-01-01T00:00:00Z"})
    }

    // J-S18(a): a match on the third page stops the pre-check immediately —
    // exactly 3 `recall` calls, no `remember`.
    #[tokio::test]
    async fn remember_once_found_on_the_third_page_makes_exactly_three_recall_calls() {
        let (conn, mut peer) = testing::fake_kernel(vec![OFFER_PREFIX.to_owned()]).await;
        let client = MemoryClient::new(&conn).expect("offer covers memory");
        let call = tokio::spawn(async move {
            client
                .remember_once("review.summary", "r:1", Map::new(), None)
                .await
        });
        for page_no in 0..2 {
            let req = testing::read_request(&mut peer).await;
            assert_eq!(req["method"], RECALL);
            // A near-miss substring match ("r:10" contains "r:1") must NOT
            // count as found — only an exact `body.dedup_key` equality does.
            testing::respond(
                &mut peer,
                &req,
                page(
                    vec![item(&format!("decoy-{page_no}"), "r:10")],
                    Some("next"),
                ),
            )
            .await;
        }
        let req = testing::read_request(&mut peer).await;
        assert_eq!(req["method"], RECALL);
        testing::respond(
            &mut peer,
            &req,
            page(vec![item("osmem:found", "r:1")], Some("next")),
        )
        .await;
        let outcome = call.await.unwrap().unwrap();
        assert_eq!(
            outcome,
            RememberOnce::Found {
                id: "osmem:found".to_owned()
            }
        );
    }

    // J-S18(b): the recall cursor exhausts with no match -> `remember` runs.
    #[tokio::test]
    async fn remember_once_calls_remember_when_the_precheck_is_exhausted() {
        let (conn, mut peer) = testing::fake_kernel(vec![OFFER_PREFIX.to_owned()]).await;
        let client = MemoryClient::new(&conn).expect("offer covers memory");
        let call = tokio::spawn(async move {
            client
                .remember_once("review.summary", "r:2", Map::new(), None)
                .await
        });
        let req = testing::read_request(&mut peer).await;
        assert_eq!(req["method"], RECALL);
        testing::respond(&mut peer, &req, page(vec![], None)).await;
        let req = testing::read_request(&mut peer).await;
        assert_eq!(req["method"], REMEMBER);
        assert_eq!(req["params"]["body"]["dedup_key"], "r:2");
        testing::respond(
            &mut peer,
            &req,
            json!({"id": "osmem:new", "at": "2026-01-02T00:00:00Z"}),
        )
        .await;
        let outcome = call.await.unwrap().unwrap();
        assert_eq!(
            outcome,
            RememberOnce::Created {
                id: "osmem:new".to_owned(),
                at: "2026-01-02T00:00:00Z".to_owned()
            }
        );
    }

    // J-S18(c): all `RECALL_PRECHECK_MAX_PAGES` pages carry a cursor and no
    // match -> `Inconclusive`, exactly that many `recall` calls, no `remember`.
    #[tokio::test]
    async fn remember_once_is_inconclusive_after_max_pages_all_with_cursor() {
        let (conn, mut peer) = testing::fake_kernel(vec![OFFER_PREFIX.to_owned()]).await;
        let client = MemoryClient::new(&conn).expect("offer covers memory");
        let call = tokio::spawn(async move {
            client
                .remember_once("review.summary", "r:3", Map::new(), None)
                .await
        });
        for _ in 0..RECALL_PRECHECK_MAX_PAGES {
            let req = testing::read_request(&mut peer).await;
            assert_eq!(req["method"], RECALL);
            testing::respond(&mut peer, &req, page(vec![], Some("more"))).await;
        }
        let outcome = call.await.unwrap().unwrap();
        assert_eq!(outcome, RememberOnce::Inconclusive);
    }

    #[tokio::test]
    async fn remember_once_rejects_a_body_with_a_different_dedup_key_without_any_call() {
        let (conn, _peer) = testing::fake_kernel(vec![OFFER_PREFIX.to_owned()]).await;
        let client = MemoryClient::new(&conn).expect("offer covers memory");
        let mut body = Map::new();
        body.insert("dedup_key".to_owned(), json!("other"));
        let err = client
            .remember_once("review.summary", "r:4", body, None)
            .await
            .unwrap_err();
        assert!(matches!(err, ClientError::InvalidParams(_)));
    }
}
