//! Kernel assistant callback. Reuses the desktop chat and bound-run paths.
use std::sync::Arc;
use std::time::Duration;

use agent24_domain::{Capability, Grants};
use agent24_os_proto::drain::Generation;
use agent24_os_proto::rpc::{CallFuture, ErrorKind, Handler, RpcError};
use agent24_protocol::{ChatMessage, ChatRequest, RunCreate, RunStatus, SessionCreate};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::server::AppState;

const TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    text: String,
    conversation_id: String,
    #[serde(default = "default_lang")]
    lang: String,
    request_id: Option<String>,
}

fn default_lang() -> String {
    "zh-CN".into()
}

impl Params {
    fn parse(value: Value) -> Result<Self, String> {
        let p: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        if p.text.trim().is_empty() || p.text.len() > 4000 {
            return Err("text must contain 1..=4000 bytes and not be blank".into());
        }
        if p.conversation_id.is_empty()
            || p.conversation_id.len() > 64
            || !p
                .conversation_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err("invalid conversation_id".into());
        }
        if !["zh-CN", "en-US", "th-TH"].contains(&p.lang.as_str()) {
            return Err("invalid lang".into());
        }
        if p.request_id.as_ref().is_some_and(|s| s.len() > 128) {
            return Err("request_id exceeds 128 bytes".into());
        }
        Ok(p)
    }

    fn system(&self) -> String {
        format!(
            "回答会被朗读：用 {} 回答，口语、简短（一般不超过 3 句），不要 Markdown、列表或代码。",
            self.lang
        )
    }
}

/// Only callback-owned handles: no module supervisors, attach registries or
/// model usage senders whose lifetime must end when grants are released.
#[derive(Clone)]
pub(crate) struct AssistantCallbackDeps {
    chat: crate::routes::ChatContext,
    store: agent24_store::Store,
    runs: Arc<agent24_agent::RunManager>,
    events: crate::events::EventsHub,
    knowledge_host: Arc<agent24_knowledge::host::KnowledgeHostRegistry>,
    shutdown: crate::server::Shutdown,
    admission: Arc<crate::model_callback::ModelAdmission>,
}

impl From<&AppState> for AssistantCallbackDeps {
    fn from(state: &AppState) -> Self {
        Self {
            chat: crate::routes::ChatContext::from(state),
            store: state.store.clone(),
            runs: state.runs.clone(),
            events: state.events.clone(),
            knowledge_host: state.knowledge_host.clone(),
            shutdown: state.shutdown.clone(),
            admission: state.assistant_admission.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct AssistantGrant {
    module: String,
    state: AssistantCallbackDeps,
}

pub(crate) fn assistant_grant(
    module: &str,
    granted: &Grants,
    state: Option<&AssistantCallbackDeps>,
) -> Option<AssistantGrant> {
    if !granted.has(Capability::Assistant) {
        return None;
    }
    Some(AssistantGrant {
        module: module.into(),
        state: state?.clone(),
    })
}

struct CancelRunOnDrop {
    runs: Arc<agent24_agent::RunManager>,
    id: Option<String>,
}

impl CancelRunOnDrop {
    fn disarm(&mut self) {
        self.id = None;
    }
}

impl Drop for CancelRunOnDrop {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let runs = self.runs.clone();
            tokio::spawn(async move {
                if let Err(error) = runs.cancel_run(&id).await {
                    tracing::warn!(run_id = %id, %error, "assistant run cancellation failed");
                }
            });
        }
    }
}

pub(crate) struct AssistantChatHandler {
    pub generation: Arc<Generation>,
    pub grant: Option<AssistantGrant>,
}

fn timeout() -> RpcError {
    RpcError::application(ErrorKind::Timeout, "assistant call exceeded its deadline")
}

fn internal() -> RpcError {
    RpcError::internal("assistant storage failed")
}

fn truncate(mut text: String) -> String {
    let mut end = text.len().min(8000);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}

// Chat's response and errors stay owned by its shared implementation.
async fn response_value(response: axum::response::Response) -> Result<Value, RpcError> {
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .map_err(|_| internal())?;
    let value: Value = serde_json::from_slice(&body).map_err(|_| internal())?;
    if status.is_success() {
        return Ok(value);
    }
    if value["error"]["code"]
        .as_str()
        .is_some_and(|c| matches!(c, "idoris_unavailable" | "provider_unavailable"))
    {
        return Err(RpcError::application(
            ErrorKind::Unavailable,
            value["error"]["message"]
                .as_str()
                .unwrap_or("model unavailable"),
        ));
    }
    Err(internal())
}

impl Handler for AssistantChatHandler {
    fn check_params(&self, params: &Value) -> Result<(), String> {
        Params::parse(params.clone()).map(|_| ())
    }

    fn call_timeout(&self) -> Option<Duration> {
        // Leave time for cancellation after our own total deadline fires.
        Some(TIMEOUT + Duration::from_secs(1))
    }

    fn call(&self, params: Value) -> CallFuture {
        let parsed = Params::parse(params);
        let grant = self.grant.clone();
        let generation = self.generation.clone();
        Box::pin(async move {
            let p = parsed.map_err(RpcError::invalid_params)?;
            let grant = grant.ok_or_else(|| {
                RpcError::application(ErrorKind::Forbidden, "assistant was not granted")
            })?;
            if crate::kernel_consent::live_denies(&grant.module, "assistant") {
                return Err(RpcError::application(
                    ErrorKind::Forbidden,
                    "assistant was revoked",
                ));
            }
            // request_id is correlation only, unlike model/complete's parent request id.
            generation
                .admit_callback_bound(None)
                .map_err(crate::events_emit::refused_error)?;
            let state = &grant.state;
            let _admitted = state.admission.try_admit(&grant.module).ok_or_else(|| {
                RpcError::application(ErrorKind::Busy, "assistant call already in flight")
            })?;
            let cancel = state.shutdown.child_token();
            let _cancel_on_drop = cancel.clone().drop_guard();
            let work = async {
                let session_id = match state
                    .store
                    .assistant_session(&grant.module, &p.conversation_id)
                    .await
                    .map_err(|_| internal())?
                {
                    Some(id) => id,
                    None => {
                        let title = match p.lang.as_str() {
                            "en-US" => "Voice conversation",
                            "th-TH" => "สนทนาด้วยเสียง",
                            _ => "语音对话",
                        };
                        let session = crate::runs::new_session(
                            &state.store,
                            SessionCreate {
                                title: title.into(),
                                channel: "desktop".into(),
                                workspace_id: None,
                            },
                        )
                        .await
                        .map_err(|_| internal())?;
                        state
                            .store
                            .link_assistant_session(&grant.module, &p.conversation_id, &session.id)
                            .await
                            .map_err(|_| internal())?;
                        session.id
                    }
                };
                let system = p.system();
                let (text, run_id) = if let Some(binding) = state
                    .knowledge_host
                    .active_assistant_binding()
                    .map_err(|_| {
                        RpcError::application(ErrorKind::Unavailable, "Knowledge host unavailable")
                    })? {
                    let binding = crate::knowledge_routes::bind_run_for_host(
                        &state.store,
                        &state.knowledge_host,
                        binding.host,
                        "default",
                        Some(&session_id),
                    )
                    .await
                    .map_err(|refused| match refused {
                        crate::knowledge_routes::Refused::SourcesUnavailable => internal(),
                        _ => RpcError::application(
                            ErrorKind::Unavailable,
                            "Knowledge binding unavailable",
                        ),
                    })?;
                    let create: RunCreate = serde_json::from_value(json!({
                        "prompt":p.text, "session_id":session_id, "knowledge_binding_id":"default"
                    }))
                    .map_err(|_| internal())?;
                    let mut events = state.events.subscribe();
                    let run = crate::runs::start_bound_run(
                        &state.runs,
                        &state.knowledge_host,
                        None,
                        create,
                        Some(binding),
                        Some(system),
                    )
                    .await
                    .map_err(|_| internal())?;
                    let mut cancel_run = CancelRunOnDrop {
                        runs: state.runs.clone(),
                        id: Some(run.id.clone()),
                    };
                    let mut poll = tokio::time::interval(Duration::from_millis(100));
                    let text = loop {
                        tokio::select! {
                            event = events.recv() => {
                                match event {
                                    Ok((_, agent24_protocol::EventBody::RunCompleted(done)))
                                        if done.run_id == run.id => {
                                        if done.memory_persisted == Some(false) { return Err(internal()); }
                                        cancel_run.disarm();
                                        break done.output.text;
                                    }
                                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return Err(internal()),
                                }
                            }
                            _ = poll.tick() => {}
                        }
                        let run = state
                            .store
                            .get_run(&run.id)
                            .await
                            .map_err(|_| internal())?
                            .ok_or_else(internal)?;
                        match run.status {
                            RunStatus::Completed => {
                                let output = run.output.ok_or_else(internal)?;
                                cancel_run.disarm();
                                break output.text;
                            }
                            RunStatus::AwaitingApproval => {
                                cancel_run.disarm();
                                return Err(RpcError::application(
                                    ErrorKind::Unavailable,
                                    "请在 Agent24 桌面确认",
                                )
                                .with_data("reason", json!("awaiting_approval"))
                                .with_data("run_id", json!(run.id))
                                .with_data("session_id", json!(session_id)));
                            }
                            RunStatus::Failed => {
                                return Err(match run.error {
                                    Some(error)
                                        if matches!(
                                            error.code.as_str(),
                                            "provider_unavailable"
                                                | "idoris_headroom_confirmation_required"
                                                | "idoris_supervisor_busy"
                                        ) =>
                                    {
                                        RpcError::application(ErrorKind::Unavailable, error.message)
                                    }
                                    Some(error) if error.code == "provider_error" => {
                                        RpcError::application(
                                            ErrorKind::Unavailable,
                                            "model provider failed",
                                        )
                                    }
                                    _ => internal(),
                                });
                            }
                            RunStatus::Cancelled => {
                                return Err(RpcError::application(
                                    ErrorKind::Cancelled,
                                    "assistant run cancelled",
                                ));
                            }
                            _ => {}
                        }
                    };
                    (text, run.id)
                } else {
                    let run_id = format!("run_{}", agent24_core::util::ulid());
                    let response = crate::routes::chat_response(
                        state.chat.clone(),
                        ChatRequest {
                            messages: vec![ChatMessage {
                                role: "user".into(),
                                content: p.text.clone(),
                            }],
                            session_id: Some(session_id.clone()),
                            model: None,
                            idoris_headroom_confirmed: false,
                        },
                        run_id.clone(),
                        cancel.clone(),
                        Some(system),
                    )
                    .await;
                    let value = response_value(response).await?;
                    if matches!(
                        value["memory_receipt"].as_str(),
                        Some("not_saved" | "failed" | "partial")
                    ) {
                        return Err(internal());
                    }
                    (
                        value["message"]["content"]
                            .as_str()
                            .ok_or_else(internal)?
                            .to_owned(),
                        run_id,
                    )
                };
                Ok(
                    json!({"text":truncate(text), "session_id":session_id, "run_id":run_id, "request_id":p.request_id}),
                )
            };
            tokio::time::timeout(TIMEOUT, work)
                .await
                .map_err(|_| timeout())?
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use agent24_models::router::{ModelRouter, Tier};
    use agent24_models::{CompletionRequest, CompletionResponse, ModelError, ModelProvider, Msg};
    use agent24_os_proto::rpc::{Dispatch, code};
    use std::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    fn params() -> Value {
        json!({"text":"hello", "conversation_id":"voice-1", "request_id":"correlation"})
    }

    #[test]
    fn validates_params() {
        assert!(Params::parse(params()).is_ok());
        for value in [
            json!({"conversation_id":"v"}),
            json!({"text":"  \n", "conversation_id":"v"}),
            json!({"text":"x".repeat(4001), "conversation_id":"v"}),
            json!({"text":"hello", "conversation_id":"../v"}),
            json!({"text":"hello", "conversation_id":""}),
            json!({"text":"hello", "conversation_id":"x".repeat(65)}),
            json!({"text":"hello", "conversation_id":"v", "extra":true}),
            json!({"text":"hello", "conversation_id":"v", "lang":"fr"}),
            json!({"text":"hello", "conversation_id":"v", "request_id":"x".repeat(129)}),
        ] {
            assert!(Params::parse(value.clone()).is_err(), "{value}");
        }
        assert!(Params::parse(json!({"text":"x".repeat(4000), "conversation_id":"a_-Z9"})).is_ok());
    }

    #[test]
    fn truncation_preserves_utf8() {
        assert_eq!(truncate("ok".into()), "ok");
        let value = truncate("好".repeat(3000));
        assert_eq!(value.len(), 7998);
        assert_eq!(truncate(format!("{}好", "x".repeat(7999))).len(), 7999);
        assert_eq!(truncate("x".repeat(8000)).len(), 8000);
    }

    struct Provider {
        seen: Mutex<Vec<CompletionRequest>>,
        hang: bool,
        entered: tokio::sync::Notify,
    }
    #[async_trait::async_trait]
    impl ModelProvider for Provider {
        fn name(&self) -> &str {
            "assistant-test"
        }
        async fn complete(
            &self,
            request: &CompletionRequest,
            cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            self.seen.lock().unwrap().push(request.clone());
            self.entered.notify_one();
            if self.hang {
                cancel.cancelled().await;
                return Err(ModelError::Cancelled);
            }
            Ok(CompletionResponse {
                message: Msg::assistant(Some("好".repeat(3000)), vec![]),
                usage: agent24_protocol::Usage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    cost_usd: 0.0,
                },
                model_id: None,
            })
        }
        async fn models(
            &self,
            _: &CancellationToken,
        ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
            Ok(vec![])
        }
    }

    async fn fixture(hang: bool) -> (AppState, Arc<Provider>) {
        let provider = Arc::new(Provider {
            seen: Mutex::new(vec![]),
            hang,
            entered: tokio::sync::Notify::new(),
        });
        let router = Arc::new(ModelRouter::with_defaults(vec![(
            provider.clone() as Arc<dyn ModelProvider>,
            Tier::Local,
        )]));
        (
            crate::server::tests::state_with_router(router).await,
            provider,
        )
    }

    fn running() -> Arc<Generation> {
        let generation = Generation::attached();
        assert!(generation.ready());
        generation
    }

    fn handler(state: &AppState) -> AssistantChatHandler {
        AssistantChatHandler {
            generation: running(),
            grant: Some(AssistantGrant {
                module: "agentear".into(),
                state: AssistantCallbackDeps::from(state),
            }),
        }
    }

    #[tokio::test]
    async fn unauthorized_is_forbidden() {
        let h = AssistantChatHandler {
            generation: running(),
            grant: None,
        };
        assert_eq!(
            h.call(params()).await.unwrap_err().kind,
            Some(ErrorKind::Forbidden)
        );
    }

    #[tokio::test]
    async fn assistant_without_manifest_grant_is_forbidden_with_deps() {
        use agent24_attach::attach_registry::AttachGrantBuilder;
        let (state, provider) = fixture(false).await;
        let manifest = agent24_domain::DomainOsManifest::from_yaml("name: agentear\nversion: '1'\nroute_namespace: /api/v1/agentear\nevent_module: agentear\ndata_dir: ~/.agent24/os/agentear/\nimpl_kind: attached_process\nkernel_capabilities: []\n").unwrap();
        let builder = crate::attach_grant::DaemonAttachGrantBuilder {
            scheduler: state.scheduler.clone(),
            models: None,
            assistant: Some(AssistantCallbackDeps::from(&state)),
            approval_broker: state.module_approval_broker.clone(),
            events: state.events.clone(),
        };
        let error = attached_call(&builder.build("agentear", &manifest), params())
            .await
            .unwrap_err();
        assert_eq!(error.kind, Some(ErrorKind::Forbidden));
        assert!(provider.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_revoked_assistant_grant_forbids_the_next_call() {
        let (state, provider) = fixture(false).await;
        let mut h = handler(&state);
        let module = "kc-assistant-revoke";
        h.grant.as_mut().unwrap().module = module.into();
        let until = chrono::Utc::now() + chrono::Duration::days(30);
        crate::kernel_consent::track(module, ["assistant"], until);
        assert!(h.call(params()).await.is_ok());
        crate::kernel_consent::track(module, std::iter::empty::<&str>(), until);
        assert_eq!(
            h.call(params()).await.unwrap_err().kind,
            Some(ErrorKind::Forbidden)
        );
        assert_eq!(provider.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn busy_is_shared_across_handlers_and_released() {
        let (state, _) = fixture(false).await;
        let slot = state.assistant_admission.try_admit("agentear").unwrap();
        assert_eq!(
            handler(&state).call(params()).await.unwrap_err().kind,
            Some(ErrorKind::Busy)
        );
        drop(slot);
        assert!(handler(&state).call(params()).await.is_ok());
    }

    #[tokio::test]
    async fn timeout_releases_admission() {
        let (state, provider) = fixture(true).await;
        let task = tokio::spawn(handler(&state).call(params()));
        tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
            .await
            .unwrap();
        tokio::time::pause();
        tokio::time::advance(TIMEOUT).await;
        tokio::time::resume();
        assert_eq!(
            task.await.unwrap().unwrap_err().kind,
            Some(ErrorKind::Timeout)
        );
        assert!(state.assistant_admission.try_admit("agentear").is_some());
    }

    struct Summarizer;
    #[async_trait::async_trait]
    impl agent24_memory::session::Summarizer for Summarizer {
        async fn summarize(&self, _: Option<&str>, _: &[Msg]) -> Result<String, String> {
            Ok("summary".into())
        }
    }

    async fn attached_call(
        grant: &agent24_attach::attach_registry::AttachGrant,
        value: Value,
    ) -> Result<Value, RpcError> {
        let methods = (grant.methods_for)(&running());
        let frame = serde_json::to_vec(
            &json!({"jsonrpc":"2.0", "id":"1", "method":"_a24/assistant/chat", "params":value}),
        )
        .unwrap();
        match agent24_os_proto::rpc::dispatch(&frame, &methods, &|_| false) {
            Dispatch::Call {
                handler, params, ..
            } => handler.call(params).await,
            Dispatch::Respond(r) => r.outcome,
            _ => panic!("unexpected dispatch"),
        }
    }

    #[tokio::test]
    async fn attached_module_reuses_persistent_session_and_records_two_chat_turns() {
        use agent24_attach::attach_registry::AttachGrantBuilder;
        let (mut state, provider) = fixture(false).await;
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        state.runs = agent24_agent::RunManager::with_memory(
            state.store.clone(),
            state.router.clone(),
            state.tools.clone(),
            Arc::new(state.events.clone()),
            state.shutdown.child_token(),
            Some(
                agent24_agent::SessionMemory::new(kv.clone(), Arc::new(Summarizer))
                    .with_owner("assistant-test".into()),
            ),
        );
        let manifest = agent24_domain::DomainOsManifest::from_yaml("name: agentear\nversion: '1'\nroute_namespace: /api/v1/agentear\nevent_module: agentear\ndata_dir: ~/.agent24/os/agentear/\nimpl_kind: attached_process\nkernel_capabilities: [assistant]\n").unwrap();
        let builder = crate::attach_grant::DaemonAttachGrantBuilder {
            scheduler: state.scheduler.clone(),
            models: None,
            assistant: Some(AssistantCallbackDeps::from(&state)),
            approval_broker: state.module_approval_broker.clone(),
            events: state.events.clone(),
        };
        let grant = builder.build("agentear", &manifest);
        assert_eq!(grant.provides, vec!["_a24/assistant/"]);
        assert_eq!(grant.kernel_grants, vec!["assistant"]);
        let first = attached_call(&grant, params()).await.unwrap();
        // New admission/MethodsFor represents a reconnect; mapping is read from SQLite.
        let second = attached_call(
            &builder.build("agentear", &manifest),
            json!({"text":"second turn", "conversation_id":"voice-1"}),
        )
        .await
        .unwrap();
        assert_eq!(first["session_id"], second["session_id"]);
        assert_eq!(first["request_id"], "correlation");
        assert_eq!(first["text"].as_str().unwrap().len(), 7998);
        let sid = first["session_id"].as_str().unwrap();
        let view = kv
            .session_log()
            .load_view("assistant-test", sid)
            .await
            .unwrap();
        assert_eq!(view.tail.len(), 4);
        assert_eq!(view.tail[0].1.content.as_deref(), Some("hello"));
        assert_eq!(view.tail[2].1.content.as_deref(), Some("second turn"));
        assert_eq!(state.store.list_sessions().await.unwrap().len(), 1);
        assert!(
            state.store.list_runs(None).await.unwrap().is_empty(),
            "inactive Knowledge uses chat"
        );
        {
            let seen = provider.seen.lock().unwrap();
            assert_eq!(seen.len(), 2);
            assert_eq!(seen[0].messages[0].role, "system");
            assert!(
                seen[0].messages[0]
                    .content
                    .as_ref()
                    .unwrap()
                    .contains("zh-CN")
            );
            assert!(seen[0].model.is_none());
            // Boundary validation is exercised through dispatch, not only direct parse.
        }
        let invalid = attached_call(&grant, json!({"conversation_id":"v"}))
            .await
            .unwrap_err();
        assert_eq!(invalid.code, code::INVALID_PARAMS);
    }
    fn activate_default(state: &AppState) {
        state
            .knowledge_host
            .apply(
                "shell:ai.auraai.agent24",
                "default",
                1,
                agent24_knowledge::host::Change::Activate,
            )
            .unwrap();
    }

    #[tokio::test]
    async fn active_knowledge_uses_bound_run_and_persists_system_instruction() {
        let (state, _) = fixture(false).await;
        activate_default(&state);
        let response = handler(&state).call(params()).await.unwrap();
        let run = state
            .store
            .get_run(response["run_id"].as_str().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(run.input.knowledge_binding_id.as_deref(), Some("default"));
        assert_eq!(run.status, RunStatus::Completed);
        assert_eq!(run.session_id.as_deref(), response["session_id"].as_str());
        let messages = state.store.list_run_messages(&run.id).await.unwrap();
        assert_eq!(messages[0].role, "system");
        assert!(
            messages[0]
                .content
                .as_ref()
                .unwrap()
                .contains("回答会被朗读")
        );
        assert_eq!(response["text"].as_str().unwrap().len(), 7998);
    }

    #[tokio::test]
    async fn aborting_knowledge_call_cancels_run_and_releases_admission() {
        let (state, provider) = fixture(true).await;
        activate_default(&state);
        let task = tokio::spawn(handler(&state).call(params()));
        tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
            .await
            .unwrap();
        let run = state.store.list_runs(None).await.unwrap().pop().unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if state.store.get_run(&run.id).await.unwrap().unwrap().status
                    == RunStatus::Cancelled
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("aborted call must cancel its bound run");
        assert!(state.assistant_admission.try_admit("agentear").is_some());
    }

    #[tokio::test]
    async fn knowledge_timeout_cancels_run() {
        let (state, provider) = fixture(true).await;
        activate_default(&state);
        let task = tokio::spawn(handler(&state).call(params()));
        tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
            .await
            .unwrap();
        let run = state.store.list_runs(None).await.unwrap().pop().unwrap();
        tokio::time::pause();
        tokio::time::advance(TIMEOUT).await;
        tokio::time::resume();
        assert_eq!(
            task.await.unwrap().unwrap_err().kind,
            Some(ErrorKind::Timeout)
        );
        for _ in 0..100 {
            if state.store.get_run(&run.id).await.unwrap().unwrap().status == RunStatus::Cancelled {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out run did not cancel");
    }

    #[tokio::test]
    async fn awaiting_approval_returns_without_cancelling_run() {
        let (state, provider) = fixture(true).await;
        activate_default(&state);
        let task = tokio::spawn(handler(&state).call(params()));
        tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
            .await
            .unwrap();
        let run = state.store.list_runs(None).await.unwrap().pop().unwrap();
        state
            .store
            .transition_run(
                &run.id,
                RunStatus::AwaitingApproval,
                agent24_store::RunPatch::default(),
            )
            .await
            .unwrap();
        let error = task.await.unwrap().unwrap_err();
        assert_eq!(error.kind, Some(ErrorKind::Unavailable));
        assert_eq!(error.data.unwrap()["reason"], "awaiting_approval");
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            state.store.get_run(&run.id).await.unwrap().unwrap().status,
            RunStatus::AwaitingApproval
        );
        assert!(state.assistant_admission.try_admit("agentear").is_some());
        state.runs.cancel_run(&run.id).await.unwrap();
    }

    #[tokio::test]
    async fn knowledge_status_survives_event_flood_and_lag_without_completion_event() {
        for status in [
            RunStatus::AwaitingApproval,
            RunStatus::Failed,
            RunStatus::Completed,
        ] {
            let (state, provider) = fixture(true).await;
            activate_default(&state);
            let task = tokio::spawn(handler(&state).call(params()));
            tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
                .await
                .unwrap();
            let run = state.store.list_runs(None).await.unwrap().pop().unwrap();
            let event =
                agent24_protocol::EventBody::RunCancelled(agent24_protocol::RunCancelledPayload {
                    run_id: "unrelated".into(),
                });
            // Overflow the receiver before yielding, then keep events arriving
            // faster than the old 50 ms sleep. No RunCompleted is emitted.
            for _ in 0..5000 {
                state.events.broadcast(event.clone());
            }
            let hub = state.events.clone();
            let flood = tokio::spawn(async move {
                loop {
                    hub.broadcast(event.clone());
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            });
            state
                .store
                .transition_run(
                    &run.id,
                    status,
                    agent24_store::RunPatch {
                        output: (status == RunStatus::Completed).then(|| {
                            agent24_protocol::RunOutput {
                                text: "stored answer".into(),
                            }
                        }),
                        error: (status == RunStatus::Failed).then(|| agent24_protocol::ErrorBody {
                            code: "provider_unavailable".into(),
                            message: "provider offline".into(),
                            hint: None,
                            details: None,
                        }),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let result = tokio::time::timeout(Duration::from_secs(5), task).await;
            flood.abort();
            let response = result
                .expect("event traffic must not starve status checks")
                .unwrap();
            match status {
                RunStatus::Completed => assert_eq!(response.unwrap()["text"], "stored answer"),
                RunStatus::Failed => {
                    let error = response.unwrap_err();
                    assert_eq!(error.kind, Some(ErrorKind::Unavailable));
                    assert_eq!(error.message, "provider offline");
                }
                RunStatus::AwaitingApproval => {
                    let error = response.unwrap_err();
                    assert_eq!(error.kind, Some(ErrorKind::Unavailable));
                    assert_eq!(error.data.unwrap()["reason"], "awaiting_approval");
                }
                _ => unreachable!(),
            }
            // Stop the hanging provider even for manually terminal store rows.
            state.shutdown.token().cancel();
        }
    }

    #[tokio::test]
    async fn mapping_survives_database_reopen_and_is_scoped_to_module() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("assistant.sqlite");
        let (mut state, _) = fixture(false).await;
        state.store = agent24_store::Store::open(&path).await.unwrap();
        let session = crate::runs::new_session(
            &state.store,
            SessionCreate {
                title: "语音对话".into(),
                channel: "desktop".into(),
                workspace_id: None,
            },
        )
        .await
        .unwrap();
        state
            .store
            .link_assistant_session("agentear", "c1", &session.id)
            .await
            .unwrap();
        drop(state);
        let reopened = agent24_store::Store::open(&path).await.unwrap();
        assert_eq!(
            reopened
                .assistant_session("agentear", "c1")
                .await
                .unwrap()
                .as_deref(),
            Some(session.id.as_str())
        );
        assert!(
            reopened
                .assistant_session("other", "c1")
                .await
                .unwrap()
                .is_none()
        );
    }
}
