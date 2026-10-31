//! The daemon's [`KnowledgeBackend`] (#1086, thin wiring for 11-11; decision
//! 2026-10-30-m1-thin-wiring): a run's Knowledge call goes to the binding the
//! run was pinned to (`crate::knowledge_host`), under the endpoint and C of
//! that binding's current activation, over [`HttpTransport`].
//!
//! The crate's Provider (`agent24_knowledge::provider`) is not wired yet; until
//! it is, the registry's activation number is the generation, and a call is
//! dropped as `cancelled` when the activation or C it was sent under is gone by
//! the time the answer is back: a deactivation, a revoke or a new push in
//! between (STATE S4.8). The answer must also echo the request's id and
//! generation, and stay inside the scope asked. Nothing here logs; a failure carries a fixed code, never the
//! endpoint, C or the server's own text.

// `Failure` is the error type of `KnowledgeBackend`, which the helpers return.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent24_knowledge::provider::state::{LocalReason, Refusal};
use agent24_knowledge::tool::call::{Failure, KnowledgeBackend, ReadQuery, SearchQuery};
use agent24_knowledge::{
    Budget, CancelledCode, CancelledDetails, CancelledError, ChunkExpectedRevision, ConstBool,
    Credential as WireCredential, ErrorCode, ErrorEnvelope, HttpTransport, HttpTransportConfig,
    KnowledgeOperation, KnowledgeTransport, LocalCancelled, NonEmpty, Page, PositiveInt,
    ProtocolVersion, ReadRequest, ReadResponse, ReadTarget, Scope, SearchRequest, SearchResponse,
    TransportError,
};
use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::capabilities::unix_now;
use crate::knowledge_host::{
    BindingNow, Credential, KnowledgeHostRegistry, KnowledgeRefusal, RunBinding,
};

/// What the server is given to answer in, and the transport's own bound on
/// top of it for the answer to arrive.
const DEADLINE_MS: u64 = 15_000;
const TRANSPORT_SLACK: Duration = Duration::from_secs(2);
const MAX_CONCURRENCY: u64 = 4;
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The `knowledge_binding_id` a run names in its stored input.
#[async_trait]
pub trait RunSelector: Send + Sync {
    async fn knowledge_binding_id(&self, run_id: &str) -> Option<String>;
}

#[async_trait]
impl RunSelector for agent24_store::Store {
    async fn knowledge_binding_id(&self, run_id: &str) -> Option<String> {
        self.get_run(run_id)
            .await
            .ok()
            .flatten()?
            .input
            .knowledge_binding_id
    }
}

/// Builds the transport for one activation and C; a seam for tests.
pub type Connect =
    dyn Fn(&str, WireCredential) -> Option<Arc<dyn KnowledgeTransport>> + Send + Sync;

/// The transport of one binding, for the activation and C it was built with.
struct Cached {
    generation: u64,
    credential_seq: u64,
    transport: Arc<dyn KnowledgeTransport>,
}

pub struct DaemonKnowledgeBackend {
    registry: Arc<KnowledgeHostRegistry>,
    runs: Arc<dyn RunSelector>,
    connect: Box<Connect>,
    transports: Mutex<HashMap<(String, String), Cached>>,
}

fn http_transport(
    base_url: &str,
    credential: WireCredential,
) -> Option<Arc<dyn KnowledgeTransport>> {
    let config = HttpTransportConfig {
        base_url: base_url.to_owned(),
        credential,
        max_response_bytes: MAX_RESPONSE_BYTES,
        connect_timeout: CONNECT_TIMEOUT,
    };
    let transport: Arc<dyn KnowledgeTransport> = Arc::new(HttpTransport::new(config).ok()?);
    Some(transport)
}

fn local(reason: LocalReason, retryable: bool) -> Failure {
    Failure::Local(Refusal { reason, retryable })
}

/// A transport ready for one call, the number of the C it carries and the
/// binding's knowledge bases.
type Ready = (Arc<dyn KnowledgeTransport>, u64, Vec<String>);

/// The call as sent: where it went and under which activation and C.
struct Sent {
    pin: RunBinding,
    generation: u64,
    credential_seq: u64,
}

impl DaemonKnowledgeBackend {
    pub fn new(registry: Arc<KnowledgeHostRegistry>, runs: Arc<dyn RunSelector>) -> Self {
        Self::with_connect(registry, runs, Box::new(http_transport))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_connect(
        registry: Arc<KnowledgeHostRegistry>,
        runs: Arc<dyn RunSelector>,
        connect: Box<Connect>,
    ) -> Self {
        Self {
            registry,
            runs,
            connect,
            transports: Mutex::new(HashMap::new()),
        }
    }

    /// The run's binding as it is now, or why the call cannot go out.
    async fn binding(&self, run_id: &str) -> Result<(RunBinding, BindingNow), Failure> {
        let selector = self.runs.knowledge_binding_id(run_id).await;
        let pin = self
            .registry
            .resolve_run(run_id, selector.as_deref())
            .map_err(|refusal| match refusal {
                KnowledgeRefusal::Unbound => Failure::Unbound,
                KnowledgeRefusal::Stale => Failure::BindingStale,
            })?;
        let now = self
            .registry
            .binding_now(&pin.host, &pin.binding, unix_now())
            .filter(|now| now.generation == pin.generation)
            .ok_or(Failure::BindingStale)?;
        Ok((pin, now))
    }

    /// The transport for this activation and C, built anew once either moved,
    /// with the C's number and the binding's knowledge bases.
    fn transport(&self, pin: &RunBinding, now: &BindingNow) -> Result<Ready, Failure> {
        let endpoint = now
            .endpoint
            .as_ref()
            .ok_or(local(LocalReason::ProfileInactive, false))?;
        // C is pushed shortly after the activation and refreshed by the host.
        let current = now
            .credential
            .as_ref()
            .ok_or(local(LocalReason::NotReady, true))?;
        let key = (pin.host.clone(), pin.binding.clone());
        let mut cache = self
            .transports
            .lock()
            .map_err(|_| local(LocalReason::NotReady, true))?;
        if let Some(cached) = cache.get(&key)
            && cached.generation == now.generation
            && cached.credential_seq == current.seq
        {
            return Ok((
                Arc::clone(&cached.transport),
                current.seq,
                endpoint.knowledge_base_ids.clone(),
            ));
        }
        cache.remove(&key);
        let credential = match &current.credential {
            Credential::Bearer(token) => WireCredential::Bearer(token.clone()),
            Credential::ApiKey(key) => WireCredential::ApiKey(key.clone()),
        };
        let transport = (self.connect)(&endpoint.base_url, credential)
            .ok_or(local(LocalReason::Incompatible, false))?;
        cache.insert(
            key,
            Cached {
                generation: now.generation,
                credential_seq: current.seq,
                transport: Arc::clone(&transport),
            },
        );
        Ok((transport, current.seq, endpoint.knowledge_base_ids.clone()))
    }

    /// Sends `request`; the answer counts only if the activation and C it
    /// went out under are still the run's, and it echoes `request_id` and the
    /// generation.
    async fn exchange<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        op: KnowledgeOperation,
        transport: &dyn KnowledgeTransport,
        sent: Sent,
        request_id: &str,
        request: &Req,
        echo: impl Fn(&Resp) -> (&str, u64),
    ) -> Result<Resp, Failure> {
        let body =
            serde_json::to_vec(request).map_err(|_| local(LocalReason::Incompatible, false))?;
        let deadline = Duration::from_millis(DEADLINE_MS) + TRANSPORT_SLACK;
        // A change to the binding while the call is out abandons it there
        // and then (S4.5): the transport's future is dropped.
        let answer = tokio::select! {
            answer = transport.send(op, body, deadline) => answer,
            () = self.until_moved(&sent, request_id) => {
                return Err(self.still_current(&sent, request_id).err()
                    .unwrap_or_else(|| cancelled(request_id, sent.generation, 0)));
            }
        };
        self.still_current(&sent, request_id)?;
        let answer = answer.map_err(|e| match e {
            TransportError::Timeout | TransportError::Unreachable => {
                local(LocalReason::Dependency, true)
            }
            TransportError::ResponseTooLarge | TransportError::Other(_) => {
                local(LocalReason::Incompatible, false)
            }
        })?;
        if answer.status != 200 {
            let envelope: ErrorEnvelope = serde_json::from_slice(&answer.body)
                .map_err(|_| local(LocalReason::Incompatible, false))?;
            // Never another request's echoes; `null` only for the codes that
            // may fail before the body is read (R6.1), paired by this
            // exchange instead (R7.3).
            let may_be_null = matches!(
                envelope.error.code(),
                ErrorCode::InvalidRequest | ErrorCode::Unauthorized | ErrorCode::Internal
            );
            let details = serde_json::to_value(&envelope.error).unwrap_or_default();
            let details = &details["details"];
            let id_ok = match details["request_id"].as_str() {
                Some(id) => id == request_id,
                None => may_be_null,
            };
            let generation_ok = match details["generation"].as_u64() {
                Some(generation) => generation == sent.generation,
                None => may_be_null,
            };
            if !(id_ok && generation_ok) {
                return Err(cancelled(request_id, sent.generation, sent.generation));
            }
            return Err(Failure::Envelope(envelope));
        }
        let response: Resp = serde_json::from_slice(&answer.body)
            .map_err(|_| local(LocalReason::Incompatible, false))?;
        let (id, generation) = echo(&response);
        if id != request_id || generation != sent.generation {
            return Err(cancelled(request_id, sent.generation, sent.generation));
        }
        Ok(response)
    }

    /// Returns once the activation or C a call went out under is gone.
    async fn until_moved(&self, sent: &Sent, request_id: &str) {
        loop {
            let notified = self.registry.changed().notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.still_current(sent, request_id).is_err() {
                return;
            }
            notified.await;
        }
    }

    /// Whether the activation and C a call went out under are still there.
    fn still_current(&self, sent: &Sent, request_id: &str) -> Result<(), Failure> {
        let now = self
            .registry
            .binding_now(&sent.pin.host, &sent.pin.binding, unix_now());
        let current_generation = now.as_ref().map_or(0, |now| now.generation);
        let same = now.as_ref().is_some_and(|now| {
            now.generation == sent.generation
                && now
                    .credential
                    .as_ref()
                    .is_some_and(|current| current.seq == sent.credential_seq)
        });
        if same {
            Ok(())
        } else {
            Err(cancelled(request_id, sent.generation, current_generation))
        }
    }
}

fn cancelled(request_id: &str, generation: u64, current_generation: u64) -> Failure {
    Failure::Cancelled(LocalCancelled {
        error: CancelledError {
            code: CancelledCode::Cancelled,
            message: "The Knowledge binding changed while the call was out".into(),
            hint: None,
            details: CancelledDetails {
                retryable: ConstBool,
                request_id: request_id.to_owned(),
                generation,
                current_generation,
            },
        },
    })
}

/// No knowledge base the model named is the binding's: read back as an empty
/// scope from the server would (READONLY R2.5, R2.6), and nothing is sent.
fn empty_scope(request_id: &str, generation: u64) -> Failure {
    let envelope = serde_json::json!({ "error": {
        "code": "forbidden", "message": "No knowledge base in scope",
        "details": { "retryable": false, "request_id": request_id,
                     "generation": generation, "reason": "empty_scope" } } });
    match serde_json::from_value(envelope) {
        Ok(envelope) => Failure::Envelope(envelope),
        Err(_) => local(LocalReason::Incompatible, false),
    }
}

fn positive(n: u64) -> PositiveInt {
    PositiveInt::new(n).unwrap_or(PositiveInt::MIN)
}

#[async_trait]
impl KnowledgeBackend for DaemonKnowledgeBackend {
    async fn search(
        &self,
        run_id: &str,
        query: SearchQuery,
    ) -> Result<(SearchRequest, SearchResponse), Failure> {
        let (pin, now) = self.binding(run_id).await?;
        let (transport, credential_seq, allowed) = self.transport(&pin, &now)?;
        let generation = now.generation;
        let request_id = uuid::Uuid::new_v4().to_string();
        let ids = match &query.knowledge_base_ids {
            None => allowed,
            Some(named) => named
                .as_slice()
                .iter()
                .filter(|id| allowed.contains(id))
                .cloned()
                .collect(),
        };
        let Ok(knowledge_base_ids) = NonEmpty::try_from(ids) else {
            return Err(empty_scope(&request_id, generation));
        };
        let request = SearchRequest {
            protocol_version: ProtocolVersion,
            request_id: request_id.clone(),
            generation,
            query: query.query,
            scope: Scope { knowledge_base_ids },
            budget: Budget {
                deadline_ms: positive(DEADLINE_MS),
                max_results: query.max_results,
                max_concurrency: positive(MAX_CONCURRENCY),
            },
        };
        let sent = Sent {
            pin,
            generation,
            credential_seq,
        };
        let response = self
            .exchange(
                KnowledgeOperation::Search,
                transport.as_ref(),
                sent,
                &request_id,
                &request,
                |r: &SearchResponse| (r.request_id.as_str(), r.generation),
            )
            .await?;
        // What comes back stays inside what was asked (R2.1, R3.8).
        let asked = request.scope.knowledge_base_ids.as_slice();
        let effective = response.effective_scope.knowledge_base_ids.as_slice();
        let failed = response
            .partial_failures
            .as_ref()
            .map_or(&[][..], NonEmpty::as_slice);
        let inside = effective.iter().all(|id| asked.contains(id))
            && response
                .hits
                .iter()
                .all(|hit| effective.contains(&hit.knowledge_base_id))
            && failed.iter().all(|f| asked.contains(&f.knowledge_base_id));
        if !inside {
            return Err(local(LocalReason::Incompatible, false));
        }
        Ok((request, response))
    }

    async fn read(
        &self,
        run_id: &str,
        query: ReadQuery,
    ) -> Result<(ReadRequest, ReadResponse), Failure> {
        let (pin, now) = self.binding(run_id).await?;
        let (transport, credential_seq, allowed) = self.transport(&pin, &now)?;
        let generation = now.generation;
        let request_id = uuid::Uuid::new_v4().to_string();
        // A document outside the binding's knowledge bases is the server's to
        // refuse: it answers `not_found` for one outside the scope (R2.6).
        let Ok(knowledge_base_ids) = NonEmpty::try_from(allowed) else {
            return Err(empty_scope(&request_id, generation));
        };
        let target = match query.chunk_id {
            Some(chunk_id) => ReadTarget::Chunk {
                chunk_id,
                expected_revision: query.content_tag.map(|content_tag| ChunkExpectedRevision {
                    content_tag,
                    source_revision: None,
                    index_build_id: None,
                    chunk_revision: None,
                }),
            },
            None => ReadTarget::Document {
                expected_revision: None,
            },
        };
        let request = ReadRequest {
            protocol_version: ProtocolVersion,
            request_id: request_id.clone(),
            generation,
            scope: Scope { knowledge_base_ids },
            knowledge_id: query.knowledge_id,
            target,
            page: Some(Page {
                cursor: query.cursor,
                limit: None,
            }),
            max_bytes: None,
            deadline_ms: Some(positive(DEADLINE_MS)),
        };
        let sent = Sent {
            pin,
            generation,
            credential_seq,
        };
        let response = self
            .exchange(
                KnowledgeOperation::Read,
                transport.as_ref(),
                sent,
                &request_id,
                &request,
                |r: &ReadResponse| (r.request_id.as_str(), r.generation),
            )
            .await?;
        let asked = request.scope.knowledge_base_ids.as_slice();
        let effective = response.effective_scope.knowledge_base_ids.as_slice();
        if !effective.iter().all(|id| asked.contains(id)) {
            return Err(local(LocalReason::Incompatible, false));
        }
        // The document asked for, and with `chunk_id` that chunk alone
        // (READONLY R4.3): what a run read is told from its calls' arguments
        // (`knowledge_tools::run_knowledge`), so an answer about another one
        // is refused rather than shown.
        let other_chunk = match &request.target {
            ReadTarget::Chunk { chunk_id, .. } => {
                !matches!(response.segments.as_slice(), [s] if s.chunk_id == *chunk_id)
            }
            ReadTarget::Document { .. } => false,
        };
        if response.knowledge_id != request.knowledge_id || other_chunk {
            return Err(local(LocalReason::Incompatible, false));
        }
        Ok((request, response))
    }
}

#[cfg(test)]
mod tests;
