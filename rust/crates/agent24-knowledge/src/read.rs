//! `read.schema.json`: `knowledge.read` request and success response (§4).
//! Errors use [`crate::ErrorEnvelope`].

use crate::common::{
    ChunkRevision, ChunkType, Display, EffectiveScope, Generation, PositiveInt, ProtocolVersion,
    RequestScope,
};
use serde::{Deserialize, Serialize};

/// `common.schema.json#/$defs/doc_revision` (R4.3, R5.3); only read uses it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocRevision {
    pub source_revision: String,
    pub index_build_id: String,
}

/// `$defs/chunk_expected_revision` (R4.3, r9): `content_tag` is required and
/// decides the match; the other parts are informational. `chunk_revision` may be
/// `null` (a hit's value sent back as is) and then encodes as no key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkExpectedRevision {
    pub content_tag: String,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub index_build_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_revision: Option<ChunkRevision>,
}

/// What is read (R4.3). The schema's `if chunk_id` / `then` / `else` picks the
/// `expected_revision` shape; here it is the variant, so a chunk read cannot
/// carry a document pair and vice versa.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadTarget {
    Document {
        expected_revision: Option<DocRevision>,
    },
    Chunk {
        chunk_id: String,
        expected_revision: Option<ChunkExpectedRevision>,
    },
}

/// `request.page` (R4.1): `cursor` is required, `null` on the first page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    #[serde(deserialize_with = "crate::nullable")]
    pub cursor: Option<String>,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub limit: Option<PositiveInt>,
}

/// `$defs/request`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawReadRequest", into = "RawReadRequest")]
pub struct ReadRequest {
    pub protocol_version: ProtocolVersion,
    pub request_id: String,
    pub generation: Generation,
    pub scope: RequestScope,
    pub knowledge_id: String,
    pub target: ReadTarget,
    pub page: Option<Page>,
    pub max_bytes: Option<PositiveInt>,
    pub deadline_ms: Option<PositiveInt>,
}

/// Either `expected_revision` shape; which one is allowed depends on `chunk_id`.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum RawExpected {
    Chunk(ChunkExpectedRevision),
    Doc(DocRevision),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReadRequest {
    protocol_version: ProtocolVersion,
    request_id: String,
    generation: Generation,
    scope: RequestScope,
    knowledge_id: String,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    chunk_id: Option<String>,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    expected_revision: Option<RawExpected>,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    page: Option<Page>,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    max_bytes: Option<PositiveInt>,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    deadline_ms: Option<PositiveInt>,
}

impl TryFrom<RawReadRequest> for ReadRequest {
    type Error = &'static str;
    fn try_from(r: RawReadRequest) -> Result<Self, Self::Error> {
        let target = match (r.chunk_id, r.expected_revision) {
            (Some(chunk_id), None) => ReadTarget::Chunk {
                chunk_id,
                expected_revision: None,
            },
            (Some(chunk_id), Some(RawExpected::Chunk(e))) => ReadTarget::Chunk {
                chunk_id,
                expected_revision: Some(e),
            },
            (None, None) => ReadTarget::Document {
                expected_revision: None,
            },
            (None, Some(RawExpected::Doc(e))) => ReadTarget::Document {
                expected_revision: Some(e),
            },
            (Some(_), Some(RawExpected::Doc(_))) => {
                return Err("expected_revision: a chunk read needs content_tag");
            }
            (None, Some(RawExpected::Chunk(_))) => {
                return Err("expected_revision: a document read carries only the document pair");
            }
        };
        Ok(Self {
            protocol_version: r.protocol_version,
            request_id: r.request_id,
            generation: r.generation,
            scope: r.scope,
            knowledge_id: r.knowledge_id,
            target,
            page: r.page,
            max_bytes: r.max_bytes,
            deadline_ms: r.deadline_ms,
        })
    }
}

impl From<ReadRequest> for RawReadRequest {
    fn from(q: ReadRequest) -> Self {
        let (chunk_id, expected_revision) = match q.target {
            ReadTarget::Document { expected_revision } => {
                (None, expected_revision.map(RawExpected::Doc))
            }
            ReadTarget::Chunk {
                chunk_id,
                expected_revision,
            } => (Some(chunk_id), expected_revision.map(RawExpected::Chunk)),
        };
        Self {
            protocol_version: q.protocol_version,
            request_id: q.request_id,
            generation: q.generation,
            scope: q.scope,
            knowledge_id: q.knowledge_id,
            chunk_id,
            expected_revision,
            page: q.page,
            max_bytes: q.max_bytes,
            deadline_ms: q.deadline_ms,
        }
    }
}

/// `$defs/response` (R4.2–R4.5). `revision` is the document pair only;
/// `chunk_revision` lives in each segment. `truncated: true` ⇒ `next_cursor`
/// being a string is a cross-field rule left to the schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadResponse {
    pub protocol_version: ProtocolVersion,
    pub request_id: String,
    pub generation: Generation,
    pub effective_scope: EffectiveScope,
    pub knowledge_id: String,
    pub revision: DocRevision,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub display: Option<Display>,
    pub segments: Vec<Segment>,
    pub truncated: bool,
    #[serde(deserialize_with = "crate::nullable")]
    pub next_cursor: Option<String>,
}

/// `$defs/segment` (R4.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    pub chunk_id: String,
    pub chunk_type: ChunkType,
    pub chunk_revision: ChunkRevision,
    pub content_tag: String,
    pub text: String,
}
