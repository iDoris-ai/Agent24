//! `documents.compare`: what an Agent is told about a comparison. Read-only.
//! The answer is the module's own `changes` (never the model's), the first
//! few with their text clipped, so it stays within a small model's prompt
//! budget; the rest are for the documents page's comparison view.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use super::route::{CompareRequest, Target, run};
use super::{BlockRef, Change};
use crate::engine::Requester;
use crate::error::ApiError;
use crate::state::{AppState, Storage};

/// Changes shown to the model, and characters of each side's text.
const SHOWN: usize = 10;
const CLIP: usize = 120;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Args {
    pub document_id: String,
    /// Absent: `document_id`'s own versions are compared (then `from_revision`
    /// and `to_revision` are its two versions; with this, they pin each document).
    pub other_document_id: Option<String>,
    pub from_revision: Option<i64>,
    pub to_revision: Option<i64>,
}

fn clip(text: &str) -> String {
    match text.char_indices().nth(CLIP) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

fn at(r: &BlockRef) -> String {
    match r {
        BlockRef::Paragraph { paragraph_index } => format!("第 {} 段", paragraph_index + 1),
        BlockRef::Block { page, .. } => format!("第 {page} 页"),
    }
}

fn shown(c: &Change) -> Value {
    let side = |s: &Option<super::Side>| {
        s.as_ref()
            .map(|s| json!({"at": at(&s.block_ref), "text": clip(&s.text)}))
    };
    json!({"id": c.id, "kind": c.kind, "left": side(&c.left), "right": side(&c.right)})
}

pub async fn call(
    state: &AppState,
    storage: &Arc<Storage>,
    args: Args,
    by: Requester,
) -> Result<Value, ApiError> {
    if args.from_revision.is_some_and(|r| r < 1) || args.to_revision.is_some_and(|r| r < 1) {
        return Err(ApiError::invalid_request("a revision is 1 or more"));
    }
    let (left, right) = match args.other_document_id {
        Some(other) => (
            Target::new(args.document_id, args.from_revision),
            Target::new(other, args.to_revision),
        ),
        None => {
            let head = crate::versions::list(storage, &args.document_id)
                .await?
                .head_revision;
            let to = args.to_revision.unwrap_or(head);
            let from = args.from_revision.unwrap_or(to - 1);
            if from < 1 {
                return Ok(json!({
                    "reason": "single_version",
                    "message": "这份文档只有一个版本，没有可以对比的上一个版本。请这样告诉用户。",
                }));
            }
            (
                Target::new(args.document_id.clone(), Some(from)),
                Target::new(args.document_id, Some(to)),
            )
        }
    };
    let c = run(state, storage, CompareRequest::new(left, right, SHOWN), by).await?;
    let (from_revision, to_revision) = c.revisions();
    Ok(json!({
        "from_revision": from_revision,
        "to_revision": to_revision,
        "identical": c.identical,
        "only_text": true,
        "summary": c.summary,
        "shown": c.changes.len(),
        "more": c.truncated,
        "changes": c.changes.iter().map(shown).collect::<Vec<_>>(),
        "note": "只比较文字。只能引用 changes 里列出的改动（id 是它们的编号）；summary 是全部改动的数量，没列出的改动请告诉用户到文档页的对比视图里看；不要自己补充任何改动。",
    }))
}
