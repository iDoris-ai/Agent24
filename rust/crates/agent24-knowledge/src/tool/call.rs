//! The call layer of `knowledge.search` and `knowledge.read` (KB-C1-05): the
//! arguments the model may pass, the backend the host implements, and what
//! the model reads back, result or refusal. The `Tool` wrappers live with
//! the host's run binding (agent24d); this layer has no kernel dependency.
//! A refusal is host-built like a result: closed codes and reasons and a
//! fixed sentence, never the server's own message or hint (READONLY R6.3).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::evidence::{render_read, render_search};
use crate::provider::state::{LocalReason, Refusal};
use crate::{
    ErrorCode, ErrorEnvelope, LocalCancelled, NonEmpty, PositiveInt, ReadRequest, ReadResponse,
    SearchRequest, SearchResponse,
};

/// What the model asks to search. Without `knowledge_base_ids` the backend
/// searches every knowledge base of the run's binding.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchQuery {
    pub query: String,
    pub knowledge_base_ids: Option<NonEmpty<String>>,
    pub max_results: Option<PositiveInt>,
}

/// What the model asks to read: a document page by page, or one chunk;
/// a citation read names the chunk and the `content_tag` its hit carried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadQuery {
    pub knowledge_id: String,
    pub chunk_id: Option<String>,
    pub content_tag: Option<String>,
    pub cursor: Option<String>,
}

/// Why a call came back without a response.
#[derive(Clone, Debug, PartialEq)]
pub enum Failure {
    /// The server's error envelope (READONLY §6).
    Envelope(ErrorEnvelope),
    /// Refused before sending (STATE §6).
    Local(Refusal),
    /// Invalidated or not accepted (STATE §6, READONLY §11.4).
    Cancelled(LocalCancelled),
    /// The run names no Knowledge binding (KB-HOST-01).
    Unbound,
    /// The binding the run was pinned to is no longer the active one.
    BindingStale,
}

/// The host side: admission, generation, credentials and transport, for the
/// run the call belongs to. Every request of one call, each page included,
/// is admitted and checked against the generation on its own.
#[async_trait]
pub trait KnowledgeBackend: Send + Sync {
    async fn search(
        &self,
        run_id: &str,
        query: SearchQuery,
    ) -> Result<(SearchRequest, SearchResponse), Failure>;

    async fn read(
        &self,
        run_id: &str,
        query: ReadQuery,
    ) -> Result<(ReadRequest, ReadResponse), Failure>;
}

/// A call that produced no evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Arguments the model may correct.
    Invalid(String),
    /// Local `cancelled`: it never reaches the model's context (STATE S5.7).
    Cancelled,
    /// The refusal the model reads.
    Failed(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: String,
    #[serde(default, deserialize_with = "crate::opt::deserialize")]
    knowledge_base_ids: Option<Vec<String>>,
    #[serde(default, deserialize_with = "crate::opt::deserialize")]
    max_results: Option<PositiveInt>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    knowledge_id: String,
    #[serde(default, deserialize_with = "crate::opt::deserialize")]
    chunk_id: Option<String>,
    #[serde(default, deserialize_with = "crate::opt::deserialize")]
    content_tag: Option<String>,
    #[serde(default, deserialize_with = "crate::opt::deserialize")]
    cursor: Option<String>,
}

/// An object schema with one required key and nothing beyond `properties`.
fn object(properties: Value, required: &str) -> Value {
    json!({ "type": "object", "properties": properties,
            "required": [required], "additionalProperties": false })
}

/// The JSON Schema advertised for `knowledge.search`.
pub fn search_parameters() -> Value {
    let text = json!({ "type": "string", "pattern": "\\S" });
    let ids = json!({ "type": "array", "minItems": 1, "items": text });
    let max = json!({ "type": "integer", "minimum": 1 });
    let properties = json!({ "query": text, "knowledge_base_ids": ids, "max_results": max });
    object(properties, "query")
}

/// The JSON Schema advertised for `knowledge.read`. A citation read passes
/// the `chunk_id` and `content_tag` of the hit it cites.
pub fn read_parameters() -> Value {
    let text = json!({ "type": "string", "pattern": "\\S" });
    let keys = ["knowledge_id", "chunk_id", "content_tag", "cursor"];
    let properties = keys
        .map(|k| (k.to_owned(), text.clone()))
        .into_iter()
        .collect();
    let mut schema = object(Value::Object(properties), "knowledge_id");
    schema["dependentRequired"] = json!({ "content_tag": ["chunk_id"] });
    schema
}

/// An empty or blank string is refused here, never sent on (READONLY R2.4).
fn filled(name: &str, value: String) -> Result<String, Refused> {
    if value.trim().is_empty() {
        return Err(Refused::Invalid(format!("{name} must not be empty")));
    }
    Ok(value)
}

fn args<T: for<'de> Deserialize<'de>>(input: &Map<String, Value>) -> Result<T, Refused> {
    serde_json::from_value(Value::Object(input.clone()))
        .map_err(|e| Refused::Invalid(format!("arguments: {e}")))
}

pub fn parse_search(input: &Map<String, Value>) -> Result<SearchQuery, Refused> {
    let a: SearchArgs = args(input)?;
    let knowledge_base_ids = match a.knowledge_base_ids {
        None => None,
        Some(ids) => {
            let ids = ids
                .into_iter()
                .map(|id| filled("knowledge_base_ids[]", id))
                .collect::<Result<Vec<_>, _>>()?;
            let ids = NonEmpty::try_from(ids)
                .map_err(|_| Refused::Invalid("knowledge_base_ids must not be empty".into()))?;
            Some(ids)
        }
    };
    Ok(SearchQuery {
        query: filled("query", a.query)?,
        knowledge_base_ids,
        max_results: a.max_results,
    })
}

pub fn parse_read(input: &Map<String, Value>) -> Result<ReadQuery, Refused> {
    let a: ReadArgs = args(input)?;
    let opt = |name, v: Option<String>| v.map(|v| filled(name, v)).transpose();
    let query = ReadQuery {
        knowledge_id: filled("knowledge_id", a.knowledge_id)?,
        chunk_id: opt("chunk_id", a.chunk_id)?,
        content_tag: opt("content_tag", a.content_tag)?,
        cursor: opt("cursor", a.cursor)?,
    };
    if query.content_tag.is_some() && query.chunk_id.is_none() {
        return Err(Refused::Invalid("content_tag needs chunk_id".into()));
    }
    Ok(query)
}

#[derive(Serialize)]
struct RefusalOut<'a> {
    kind: &'static str,
    outcome: &'static str,
    code: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
    retryable: bool,
    advice: &'static str,
}

/// The code, reason and retryable of the server's envelope, as the wire
/// spells them; the message and hint stay behind.
fn envelope_parts(envelope: &ErrorEnvelope) -> (String, Option<String>, bool) {
    let wire = serde_json::to_value(&envelope.error).unwrap_or_default();
    let text = |v: &Value| v.as_str().map(str::to_owned);
    let retryable = wire["details"]["retryable"].as_bool().unwrap_or(false);
    let code = text(&wire["code"]).unwrap_or_else(|| "internal".into());
    (code, text(&wire["details"]["reason"]), retryable)
}

fn server_advice(code: ErrorCode, reason: Option<&str>, retryable: bool) -> &'static str {
    use ErrorCode as C;
    match (code, reason) {
        (C::InvalidRequest, Some("mixed_score_space")) => {
            "所选知识库不能放在一次检索里，请分开检索。"
        }
        (C::InvalidRequest, _) => "知识库不接受这组参数，请调整后再试。",
        (C::Unauthorized, _) => "知识库凭据无效或已失效，本次无法读取。",
        (C::Forbidden, Some("empty_scope")) => "所选范围内没有可以访问的知识库。",
        (C::Forbidden, _) => "知识库不允许这项操作。",
        (C::NotFound, _) => "没有找到可以读取的这份文档。",
        (C::Stale, _) => "这段内容已经变化，请重新检索后再读取。",
        (C::Timeout, _) => "知识库没有在时限内完成，可以稍后重试。",
        (C::Unavailable, _) if retryable => "知识库暂时不可用，可以稍后重试。",
        (C::Unavailable, _) => "知识库当前无法提供这部分内容。",
        (C::Internal, _) => "知识库出现内部错误，本次无法读取。",
    }
}

fn local_advice(reason: LocalReason, retryable: bool) -> &'static str {
    use LocalReason as R;
    match reason {
        R::KnowledgeOff => "知识库功能已关闭，本次不可用。",
        R::LocalStorage => "本地设置尚未保存完成，暂时不能使用知识库。",
        R::ProfileInactive => "当前没有可用的知识库配置。",
        R::Incompatible => "知识库服务版本不兼容。",
        R::CredentialRejected => "知识库登录被拒绝，需要用户重新登录。",
        R::AccessRevoked => "知识库访问权限已被撤销。",
        R::InstanceUnidentified => "无法确认本机知识库实例。",
        R::NotReady => "知识库尚未就绪，可以稍后重试。",
        R::Dependency if retryable => "知识库依赖的服务暂时不可用，可以稍后重试。",
        R::Dependency => "知识库依赖的服务不可用，要等用户确认设置后才会重试。",
    }
}

const UNBOUND: &str = "这次对话没有绑定知识库。";
const STALE: &str = "这次对话所用的知识库配置已经变更，本轮无法使用。";

fn refusal(kind: &'static str, failure: Failure) -> Refused {
    let (code, reason, retryable, advice) = match failure {
        Failure::Cancelled(_) => return Refused::Cancelled,
        Failure::Envelope(envelope) => {
            let (code, reason, retryable) = envelope_parts(&envelope);
            let advice = server_advice(envelope.error.code(), reason.as_deref(), retryable);
            (code, reason, retryable, advice)
        }
        Failure::Local(Refusal { reason, retryable }) => (
            "unavailable".into(),
            Some(reason.as_str().into()),
            retryable,
            local_advice(reason, retryable),
        ),
        Failure::Unbound => ("knowledge_unbound".into(), None, false, UNBOUND),
        Failure::BindingStale => ("knowledge_binding_stale".into(), None, false, STALE),
    };
    let out = RefusalOut {
        kind,
        outcome: "refused",
        code: &code,
        reason: reason.as_deref(),
        retryable,
        advice,
    };
    Refused::Failed(serde_json::to_string(&out).unwrap_or_default())
}

pub async fn search(
    backend: &dyn KnowledgeBackend,
    run_id: &str,
    input: &Map<String, Value>,
) -> Result<String, Refused> {
    let query = parse_search(input)?;
    match backend.search(run_id, query).await {
        Ok((request, response)) => Ok(render_search(&request, &response)),
        Err(failure) => Err(refusal("knowledge.search", failure)),
    }
}

pub async fn read(
    backend: &dyn KnowledgeBackend,
    run_id: &str,
    input: &Map<String, Value>,
) -> Result<String, Refused> {
    let query = parse_read(input)?;
    match backend.read(run_id, query).await {
        Ok((request, response)) => Ok(render_read(&request, &response)),
        Err(failure) => Err(refusal("knowledge.read", failure)),
    }
}

#[cfg(test)]
mod tests;
