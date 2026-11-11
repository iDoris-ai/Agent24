//! Inbound ingest and startup reconcile (X11-2c).
//!
//! `persist_new_run` commits its own transaction, so a link written afterwards
//! can outlive a crash as a run with no `inbound_runs` row. This path does not
//! call it. [`agent24_store::Store::insert_session_run_for_inbound`] commits
//! the session, the run, and `inbound_runs` together. The window after that
//! commit and before `run_state` leaves `starting` is what
//! [`reconcile_inbound_runs`] closes: a link with a live run becomes
//! `started`; a link whose run has ended becomes `done`; `starting` with no
//! link becomes `interrupted` and is not started again.
//!
//! 评审 N1：没有 run 级墙钟超时。`MAX_ITERATIONS`
//! （`agent24-agent/src/lib.rs:62`，循环在同文件约 2751 行）只限制模型
//! 循环次数；一次卡住的 `complete()` 不会自己结束。工具调用各自有 timeout，
//! 盖不住卡在模型请求上的 run。取消该 run 后，入站行变为 `done`，同一
//! actor 的下一条才能再起。
//!
//! X11-2e / X11-2g call [`InboundService`]. This slice has no HTTP route, so
//! the ingest types are unused in a non-test build.

#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "X11-2e and X11-2g call InboundService; this slice wires startup reconcile only"
    )
)]

use std::sync::Arc;

use agent24_agent::{InboundOrigin, InboundRunStart, RunManager};
use agent24_core::util::ulid;
use agent24_protocol::{RunCreate, RunMode};
use agent24_store::{NewInboundEvent, Store, StoreError, format_inbound_timestamp};
use chrono::{DateTime, Duration, Utc};

use super::admit::{
    AcceptMode, Addressed, AdmitInput, AllowlistEntry, Category, Disposition, Source, admit,
    policy_version,
};

/// §3.4 placeholder, fixed here: 3 messages per sender per minute.
pub(crate) const INBOUND_RATE_LIMIT: u32 = 3;
/// Sliding window for [`INBOUND_RATE_LIMIT`].
pub(crate) const INBOUND_RATE_WINDOW_SECS: i64 = 60;
/// §3.4 placeholder, fixed here: 10 minutes either side of now.
pub(crate) const INBOUND_MAX_AGE_SECS: u64 = 10 * 60;
/// External text cap, in Unicode scalar values. The note sits after the cap.
pub(crate) const INBOUND_TEXT_MAX_CHARS: usize = 4000;

const TRUNCATION_NOTE: &str = "\n[truncated, original exceeded 4000 chars]";

pub(crate) struct InboundService {
    store: Store,
    runs: Arc<RunManager>,
}

/// One external message. `source` / `category` / `addressed` are the wire words.
#[derive(Debug, Clone)]
pub(crate) struct InboundMessage {
    pub id: String,
    pub source: String,
    pub category: String,
    pub recipient_identity: Option<String>,
    pub thread_key: Option<String>,
    pub actor: Option<String>,
    pub human: bool,
    pub addressed: String,
    pub at: String,
    pub text: Option<String>,
    pub meta: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IngestOutcome {
    Duplicate,
    Ignored {
        reason: String,
    },
    Started {
        run_id: String,
    },
    /// `run_id` is `None` when the link row is already gone. An empty string
    /// is not a run id.
    AlreadyLinked {
        run_id: Option<String>,
    },
}

#[derive(Debug)]
#[allow(dead_code, reason = "error payloads are for X11-2e and X11-2g callers")]
pub(crate) enum IngestError {
    Invalid(String),
    /// `source` is scheduler or self, or `category` is internal.
    /// Not inserted and not admitted. Holds in release builds.
    NotInbound,
    Store(StoreError),
    Start(String),
}

impl From<StoreError> for IngestError {
    fn from(err: StoreError) -> Self {
        Self::Store(err)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InboundStartupReconcile {
    pub started: u64,
    pub done: u64,
    pub interrupted: u64,
}

impl InboundService {
    pub(crate) fn new(store: Store, runs: Arc<RunManager>) -> Self {
        Self { store, runs }
    }

    /// Admit, insert, and (only for `explicit`) start one run.
    pub(crate) async fn ingest(
        &self,
        message: InboundMessage,
    ) -> Result<IngestOutcome, IngestError> {
        // Not `debug_assert`: release builds must not fall through into admit.
        if message.source == Source::Scheduler.as_str()
            || message.source == Source::Self_.as_str()
            || message.category == Category::Internal.as_str()
        {
            return Err(IngestError::NotInbound);
        }
        let prepared = prepare(&message)?;
        let now = Utc::now();
        let received_at = format_inbound_timestamp(now);
        let decision = self.decide(&prepared, now).await?;
        let explicit = decision.disposition == Disposition::Explicit;
        // Refuse before the insert: a stored `starting` row with nothing to run
        // would hold this actor's in-flight slot until the next startup.
        if explicit && prepared.text.as_deref().is_none_or(str::is_empty) {
            return Err(IngestError::Invalid(format!(
                "event {} has no text",
                prepared.id
            )));
        }
        let row = NewInboundEvent {
            id: prepared.id.clone(),
            source: prepared.source.as_str().to_owned(),
            category: prepared.category.as_str().to_owned(),
            recipient_identity: prepared.recipient_identity.clone(),
            thread_key: prepared.thread_key.clone(),
            actor: Some(prepared.actor.clone()),
            human: prepared.human,
            addressed: prepared.addressed.as_str().to_owned(),
            at: prepared.at.clone(),
            received_at,
            text: prepared.text.clone(),
            meta: prepared.meta.clone(),
            disposition: decision.disposition.as_str().to_owned(),
            disposition_reason: decision.reason.map(|reason| reason.as_str().to_owned()),
            policy_version: Some(decision.policy_version),
            run_state: explicit.then(|| "starting".to_owned()),
        };
        match self.store.insert_inbound_event(&row).await? {
            agent24_store::InboundInsert::Duplicate => Ok(IngestOutcome::Duplicate),
            agent24_store::InboundInsert::Inflight => {
                let mut limited = row;
                limited.disposition = Disposition::Ignore.as_str().to_owned();
                limited.disposition_reason = Some("rate_limited".to_owned());
                limited.run_state = None;
                match self.store.insert_inbound_event(&limited).await? {
                    agent24_store::InboundInsert::Inserted => Ok(IngestOutcome::Ignored {
                        reason: "rate_limited".to_owned(),
                    }),
                    // Two inflight rewrites of the same id: the loser finds
                    // the row the winner just stored. That is a duplicate,
                    // not a start failure.
                    agent24_store::InboundInsert::Duplicate => Ok(IngestOutcome::Duplicate),
                    other => Err(IngestError::Start(format!(
                        "rate_limited rewrite for {} was {other:?}",
                        limited.id
                    ))),
                }
            }
            agent24_store::InboundInsert::Inserted if explicit => self.dispatch(&prepared.id).await,
            agent24_store::InboundInsert::Inserted => Ok(IngestOutcome::Ignored {
                reason: decision
                    .reason
                    .map(|reason| reason.as_str().to_owned())
                    .unwrap_or_else(|| "ignore".to_owned()),
            }),
        }
    }

    /// Start the run for an event that is already stored as `explicit`.
    ///
    /// A second call commits nothing: `inbound_runs.event_id` is unique inside
    /// the same transaction as the run.
    pub(crate) async fn dispatch(&self, event_id: &str) -> Result<IngestOutcome, IngestError> {
        let event = self
            .store
            .get_inbound_event(event_id)
            .await?
            .ok_or_else(|| IngestError::Invalid(format!("unknown inbound event {event_id}")))?;
        if event.disposition != Disposition::Explicit.as_str() {
            return Err(IngestError::Invalid(format!(
                "event {event_id} is {}, not explicit",
                event.disposition
            )));
        }
        // The session channel is the source name. It has to be one of
        // `RunManager::INBOUND_SOURCE_CHANNELS` (the same set as the route
        // allowlist). A name outside that set would be stored as the account
        // owner (#1813 R2 M1). A fresh id every call: reusing the linked
        // session fails the session insert before `AlreadyLinked`.
        let channel = event.source;
        if !agent24_agent::RunManager::INBOUND_SOURCE_CHANNELS.contains(&channel.as_str()) {
            return Err(IngestError::Invalid(format!(
                "inbound source {channel} is not a session channel"
            )));
        }
        // `None` is a missing body. `unwrap_or_default` used to start a run
        // whose prompt was empty.
        let Some(prompt) = event.text else {
            return Err(IngestError::Invalid(format!(
                "event {event_id} has no text"
            )));
        };
        let create = RunCreate {
            idoris_headroom_confirmed: false,
            session_id: Some(format!("sess_{}", ulid())),
            workspace_id: None,
            prompt,
            model_override: None,
            mode: RunMode::Normal,
            knowledge_binding_id: None,
        };
        let origin = InboundOrigin {
            event_id: event.id,
            channel,
        };
        match self.runs.start_run_inbound(create, origin).await {
            Ok(InboundRunStart::Started(run)) => Ok(IngestOutcome::Started { run_id: run.id }),
            Ok(InboundRunStart::AlreadyLinked) => Ok(already_linked_outcome(
                self.store.inbound_run_id_for_event(event_id).await?,
            )),
            Ok(InboundRunStart::NotStarting) => Err(IngestError::Invalid(format!(
                "event {event_id} is not starting"
            ))),
            Err(err) => {
                // The event is already `starting` and holds the in-flight
                // index. The link commits in the same transaction as the run,
                // so a start error has no `inbound_runs` row. `start_failed`
                // refuses to move a row that does have one.
                if let Err(mark_err) = self
                    .store
                    .mark_inbound_run_state(event_id, "start_failed")
                    .await
                {
                    tracing::warn!(
                        event_id,
                        error = %mark_err,
                        "inbound start_failed was not recorded"
                    );
                }
                Err(IngestError::Start(err.to_string()))
            }
        }
    }

    async fn decide(
        &self,
        prepared: &Prepared,
        now: DateTime<Utc>,
    ) -> Result<Decided, IngestError> {
        let mode = match self.store.inbound_accept_mode().await? {
            Some(raw) => parse_mode(&raw)?,
            None => AcceptMode::Observe,
        };
        let mut entries = Vec::new();
        for entry in self.store.list_inbound_allowlist().await? {
            let Some(source) = parse_source(&entry.source) else {
                return Err(IngestError::Invalid(format!(
                    "allowlist source {} is not a known source",
                    entry.source
                )));
            };
            entries.push(AllowlistEntry {
                source,
                actor: entry.actor,
            });
        }
        let version = policy_version(&entries, mode);
        let allowlisted = self
            .store
            .inbound_actor_allowlisted(prepared.source.as_str(), &prepared.actor)
            .await?;
        let activated = match self
            .store
            .inbound_watermark_activated_at(prepared.source.as_str())
            .await?
        {
            Some(stamp) => parse_unix("watermark", &stamp)?,
            // No activation row: every event is before the watermark (H5).
            None => i64::MAX,
        };
        let since = format_inbound_timestamp(now - Duration::seconds(INBOUND_RATE_WINDOW_SECS));
        let window = self
            .store
            .count_inbound_window(prepared.source.as_str(), &prepared.actor, &since)
            .await?;
        let window_count = u32::try_from(window.max(0)).unwrap_or(u32::MAX);
        let decision = admit(&AdmitInput {
            source: prepared.source,
            actor: prepared.actor.clone(),
            category: prepared.category,
            human: prepared.human,
            addressed: prepared.addressed,
            allowlisted,
            event_at_unix_secs: parse_unix("at", &prepared.at)?,
            activated_unix_secs: activated,
            max_age_secs: INBOUND_MAX_AGE_SECS,
            now_unix_secs: now.timestamp(),
            window_count,
            rate_limit: INBOUND_RATE_LIMIT,
        });
        Ok(Decided {
            disposition: decision.disposition,
            reason: decision.reason,
            policy_version: version,
        })
    }
}

struct Prepared {
    id: String,
    source: Source,
    category: Category,
    recipient_identity: Option<String>,
    thread_key: Option<String>,
    actor: String,
    human: bool,
    addressed: Addressed,
    at: String,
    text: Option<String>,
    meta: Option<String>,
}

struct Decided {
    disposition: Disposition,
    reason: Option<super::admit::Reason>,
    policy_version: String,
}

fn prepare(message: &InboundMessage) -> Result<Prepared, IngestError> {
    if message.id.is_empty() {
        return Err(IngestError::Invalid("id is empty".to_owned()));
    }
    let actor = message
        .actor
        .clone()
        .filter(|actor| !actor.is_empty())
        .ok_or_else(|| IngestError::Invalid("actor is empty".to_owned()))?;
    let source = parse_source(&message.source)
        .ok_or_else(|| IngestError::Invalid(format!("unknown source {}", message.source)))?;
    let category = parse_category(&message.category)
        .ok_or_else(|| IngestError::Invalid(format!("unknown category {}", message.category)))?;
    let addressed = parse_addressed(&message.addressed)
        .ok_or_else(|| IngestError::Invalid(format!("unknown addressed {}", message.addressed)))?;
    parse_unix("at", &message.at)?;
    Ok(Prepared {
        id: message.id.clone(),
        source,
        category,
        recipient_identity: message.recipient_identity.clone(),
        thread_key: message.thread_key.clone(),
        actor,
        human: message.human,
        addressed,
        at: message.at.clone(),
        text: bound_text(message.text.clone()),
        meta: message.meta.clone(),
    })
}

fn bound_text(text: Option<String>) -> Option<String> {
    let text = text?;
    if text.chars().count() <= INBOUND_TEXT_MAX_CHARS {
        return Some(text);
    }
    let mut bounded: String = text.chars().take(INBOUND_TEXT_MAX_CHARS).collect();
    bounded.push_str(TRUNCATION_NOTE);
    Some(bounded)
}

fn parse_unix(field: &str, stamp: &str) -> Result<i64, IngestError> {
    let Some(parsed) = DateTime::parse_from_rfc3339(stamp).ok() else {
        return Err(IngestError::Invalid(format!(
            "{field} must be YYYY-MM-DDTHH:MM:SSZ, got {stamp}"
        )));
    };
    if format_inbound_timestamp(parsed.with_timezone(&Utc)) != stamp {
        return Err(IngestError::Invalid(format!(
            "{field} must be YYYY-MM-DDTHH:MM:SSZ, got {stamp}"
        )));
    }
    Ok(parsed.timestamp())
}

fn parse_source(raw: &str) -> Option<Source> {
    Some(match raw {
        "hyphae" => Source::Hyphae,
        "wechat" => Source::Wechat,
        "remote" => Source::Remote,
        "scheduler" => Source::Scheduler,
        "self" => Source::Self_,
        _ => return None,
    })
}

fn parse_category(raw: &str) -> Option<Category> {
    Some(match raw {
        "direct" => Category::Direct,
        "group" => Category::Group,
        "group_mention" => Category::GroupMention,
        "stranger" => Category::Stranger,
        "dev_bridge" => Category::DevBridge,
        "protocol" => Category::Protocol,
        "internal" => Category::Internal,
        _ => return None,
    })
}

fn parse_addressed(raw: &str) -> Option<Addressed> {
    Some(match raw {
        "none" => Addressed::None,
        "mention" => Addressed::Mention,
        "direct" => Addressed::Direct,
        _ => return None,
    })
}

/// `None` stays `None`. Filling it with `""` would name a run that is not there.
fn already_linked_outcome(run_id: Option<String>) -> IngestOutcome {
    IngestOutcome::AlreadyLinked { run_id }
}

fn parse_mode(raw: &str) -> Result<AcceptMode, IngestError> {
    match raw {
        "off" => Ok(AcceptMode::Off),
        "observe" => Ok(AcceptMode::Observe),
        "auto" => Ok(AcceptMode::Auto),
        other => Err(IngestError::Invalid(format!("unknown accept_mode {other}"))),
    }
}

/// Startup pass. Call it after orphan sweeps so a swept run is already terminal.
///
/// - `starting` with a link and a live run → `started` (no second run)
/// - `starting` with a link and a terminal run → `done`
/// - `starting` with no link → `interrupted`
/// - `started` whose run has ended → `done`
/// - `started` with no link → `done`. Deleting `runs` cascades `inbound_runs`
///   and leaves the event `started`, so the in-flight index still holds the
///   sender. Nothing is left to resume. `done` is the edge that already
///   releases a `started` row (the same word as a run that reached a terminal
///   status). `interrupted` stays the word for a `starting` row that never
///   linked.
pub(crate) async fn reconcile_inbound_runs(
    store: &Store,
) -> Result<InboundStartupReconcile, StoreError> {
    let mut report = InboundStartupReconcile {
        started: 0,
        done: 0,
        interrupted: 0,
    };
    let split = store.inbound_reconcile().await?;
    for id in split.unlinked {
        if store.mark_inbound_run_state(&id, "interrupted").await? {
            report.interrupted += 1;
        }
    }
    for id in split.linked {
        match store.inbound_linked_run_status(&id).await? {
            Some(status) if run_status_is_terminal(&status) => {
                if store.mark_inbound_done_by_event(&id).await? {
                    report.done += 1;
                }
            }
            Some(_) => {
                if store.mark_inbound_started(&id).await? {
                    report.started += 1;
                }
            }
            None => {
                if store.mark_inbound_run_state(&id, "interrupted").await? {
                    report.interrupted += 1;
                }
            }
        }
    }
    for id in store.inbound_started_terminal_event_ids().await? {
        if store.mark_inbound_done_by_event(&id).await? {
            report.done += 1;
        }
    }
    // See the function docs: a deleted run cascades the link and leaves
    // `started` holding the in-flight slot. `done` releases it.
    for id in store.inbound_started_unlinked_event_ids().await? {
        if store.mark_inbound_run_state(&id, "done").await? {
            report.done += 1;
        }
    }
    Ok(report)
}

fn run_status_is_terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Arc;
    use std::time::Duration as StdDuration;

    use agent24_agent::RunManager;
    use agent24_models::router::{ModelRouter, Tier};
    use agent24_models::{CompletionRequest, CompletionResponse, ModelError, ModelProvider, Msg};
    use agent24_protocol::RunStatus;
    use agent24_store::{
        InboundInsert, NewInboundEvent, SourceClassification, SourceKind, Store,
        format_inbound_timestamp,
    };
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use chrono::Duration;
    use chrono::Utc;
    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    use super::*;

    const SECRET: &str = "SECRET-inbound-7f3a";

    struct Fixed;

    #[async_trait]
    impl ModelProvider for Fixed {
        fn name(&self) -> &str {
            "fixed"
        }
        async fn complete(
            &self,
            _req: &CompletionRequest,
            _cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            Ok(CompletionResponse {
                message: Msg::assistant(Some("pong".to_owned()), vec![]),
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
            _cancel: &CancellationToken,
        ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
            Ok(vec![])
        }
    }

    struct Recording(Mutex<Vec<String>>);

    #[async_trait]
    impl ModelProvider for Recording {
        fn name(&self) -> &str {
            "recording"
        }
        async fn complete(
            &self,
            req: &CompletionRequest,
            _cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            let mut seen = self.0.lock().await;
            for msg in &req.messages {
                if let Some(content) = &msg.content {
                    seen.push(content.clone());
                }
            }
            drop(seen);
            Ok(CompletionResponse {
                message: Msg::assistant(Some("pong".to_owned()), vec![]),
                usage: agent24_protocol::Usage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                    cost_usd: 0.0,
                },
                model_id: None,
            })
        }
        async fn models(
            &self,
            _cancel: &CancellationToken,
        ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
            Ok(vec![])
        }
    }

    struct Hanging;

    #[async_trait]
    impl ModelProvider for Hanging {
        fn name(&self) -> &str {
            "hanging"
        }
        async fn complete(
            &self,
            _req: &CompletionRequest,
            cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            cancel.cancelled().await;
            Err(ModelError::Cancelled)
        }
        async fn models(
            &self,
            _cancel: &CancellationToken,
        ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
            Ok(vec![])
        }
    }

    struct Noop;

    impl agent24_agent::EventSink for Noop {
        fn emit(&self, _body: agent24_protocol::EventBody) {}
    }

    async fn open_service(provider: Arc<dyn ModelProvider>) -> (InboundService, Store) {
        let store = Store::open_memory().await.unwrap();
        let runs = RunManager::new(
            store.clone(),
            Arc::new(ModelRouter::with_defaults(vec![(provider, Tier::Local)])),
            Arc::new(agent24_tools::ToolRegistry::new()),
            Arc::new(Noop),
            CancellationToken::new(),
        );
        (InboundService::new(store.clone(), runs), store)
    }

    fn stamp(seconds_ago: i64) -> String {
        format_inbound_timestamp(Utc::now() - Duration::seconds(seconds_ago))
    }

    async fn allow(store: &Store, source: &str, actor: &str) {
        sqlx::query("INSERT INTO inbound_allowlist (source, actor, added_at) VALUES (?, ?, ?)")
            .bind(source)
            .bind(actor)
            .bind(stamp(0))
            .execute(agent24_store::test_hooks::pool(store))
            .await
            .unwrap();
    }

    async fn watermark(store: &Store, source: &str, activated_at: &str) {
        // max_age_secs = 1 on purpose. Admission uses INBOUND_MAX_AGE_SECS, not this column.
        sqlx::query(
            "INSERT INTO inbound_source_watermark (source, activated_at, max_age_secs) VALUES (?, ?, 1)",
        )
        .bind(source)
        .bind(activated_at)
        .execute(agent24_store::test_hooks::pool(store))
        .await
        .unwrap();
    }

    fn message(id: &str, actor: &str, at: &str, text: &str) -> InboundMessage {
        InboundMessage {
            id: id.to_owned(),
            source: "hyphae".to_owned(),
            category: "direct".to_owned(),
            recipient_identity: Some("me".to_owned()),
            thread_key: None,
            actor: Some(actor.to_owned()),
            human: true,
            addressed: "direct".to_owned(),
            at: at.to_owned(),
            text: Some(text.to_owned()),
            meta: None,
        }
    }

    async fn run_count(store: &Store) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(agent24_store::test_hooks::pool(store))
            .await
            .unwrap()
    }

    async fn wait_terminal(store: &Store, id: &str) {
        for _ in 0..100 {
            let run = store.get_run(id).await.unwrap().unwrap();
            if agent24_core::run_is_terminal(run.status) {
                return;
            }
            tokio::time::sleep(StdDuration::from_millis(20)).await;
        }
        panic!("run {id} never finished");
    }

    #[tokio::test]
    async fn explicit_starts_one_run_tagged_unknown_and_a_replay_does_not() {
        let (svc, store) = open_service(Arc::new(Fixed)).await;
        allow(&store, "hyphae", "npub1").await;
        watermark(&store, "hyphae", &stamp(120)).await;
        let text = format!("hello {SECRET}");
        let first = svc
            .ingest(message("hyphae:m1", "npub1", &stamp(30), &text))
            .await
            .unwrap();
        let IngestOutcome::Started { run_id } = first else {
            panic!("expected a run, got {first:?}");
        };
        assert_eq!(run_count(&store).await, 1);
        assert_eq!(
            store
                .inbound_run_id_for_event("hyphae:m1")
                .await
                .unwrap()
                .as_deref(),
            Some(run_id.as_str())
        );
        let again = svc
            .ingest(message("hyphae:m1", "npub1", &stamp(30), &text))
            .await
            .unwrap();
        assert_eq!(again, IngestOutcome::Duplicate);
        assert_eq!(run_count(&store).await, 1);

        wait_terminal(&store, &run_id).await;
        let tags = store.list_run_source_tags(&run_id).await.unwrap();
        let user = tags
            .iter()
            .find(|tag| tag.source.kind == SourceKind::UserInput)
            .unwrap();
        assert_eq!(user.source.classification, SourceClassification::Unknown);
        assert!(user.source.source_id.starts_with("inbound_external:"));
        let event = store.get_inbound_event("hyphae:m1").await.unwrap().unwrap();
        assert_eq!(event.run_state.as_deref(), Some("done"));
        let session_id = store
            .get_run(&run_id)
            .await
            .unwrap()
            .unwrap()
            .session_id
            .unwrap();
        let session = store.get_session(&session_id).await.unwrap().unwrap();
        assert_eq!(session.channel, "hyphae");

        let mut state = crate::server::tests::state().await;
        state.store = store.clone();
        let (status, body) = peer_get(&state, &format!("/api/v1/peer/runs/{run_id}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("\"withheld\":true"), "{body}");
        assert!(!body.contains(SECRET), "{body}");
        let (status, list) = peer_get(&state, "/api/v1/peer/runs").await;
        assert_eq!(status, StatusCode::OK, "{list}");
        assert!(!list.contains(SECRET), "{list}");
    }

    async fn peer_get(state: &crate::server::AppState, path: &str) -> (StatusCode, String) {
        let res = crate::server::build_router_with_modules(state.clone(), axum::Router::new())
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
            .await
            .unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn full_external_text_enters_the_prompt_and_overlong_text_is_marked() {
        let seen = Arc::new(Recording(Mutex::new(Vec::new())));
        let (svc, store) = open_service(Arc::clone(&seen) as Arc<dyn ModelProvider>).await;
        allow(&store, "hyphae", "npub1").await;
        watermark(&store, "hyphae", &stamp(120)).await;
        let body = format!("{SECRET}{}", "x".repeat(20));
        let started = svc
            .ingest(message("hyphae:full", "npub1", &stamp(30), &body))
            .await
            .unwrap();
        let IngestOutcome::Started { run_id } = started else {
            panic!("{started:?}");
        };
        wait_terminal(&store, &run_id).await;
        let prompts = seen.0.lock().await.clone();
        assert!(
            prompts.iter().any(|prompt| prompt.contains(&body)),
            "full text missing from {prompts:?}"
        );

        let overlong = format!("{}TAIL-MARKER-XYZ", "a".repeat(INBOUND_TEXT_MAX_CHARS));
        // The first run has ended, so the in-flight slot is free. The window
        // still has room (limit 3).
        let second = svc
            .ingest(message("hyphae:long", "npub1", &stamp(30), &overlong))
            .await
            .unwrap();
        let IngestOutcome::Started { run_id: long_id } = second else {
            panic!("{second:?}");
        };
        let stored = store
            .get_inbound_event("hyphae:long")
            .await
            .unwrap()
            .unwrap();
        let text = stored.text.unwrap();
        assert!(!text.contains("TAIL-MARKER-XYZ"), "{text}");
        assert!(text.contains("truncated"), "{text}");
        assert_eq!(
            text.chars().take(INBOUND_TEXT_MAX_CHARS).count(),
            INBOUND_TEXT_MAX_CHARS
        );
        wait_terminal(&store, &long_id).await;
        let prompts = seen.0.lock().await.clone();
        assert!(
            prompts.iter().any(|prompt| prompt.contains("truncated")),
            "{prompts:?}"
        );
        assert!(
            prompts
                .iter()
                .all(|prompt| !prompt.contains("TAIL-MARKER-XYZ")),
            "{prompts:?}"
        );
    }

    #[tokio::test]
    async fn dispatching_one_event_twice_starts_one_run() {
        let (svc, store) = open_service(Arc::new(Hanging)).await;
        allow(&store, "hyphae", "npub1").await;
        watermark(&store, "hyphae", &stamp(120)).await;
        let inserted = svc
            .ingest(message("hyphae:twice", "npub1", &stamp(30), "once"))
            .await
            .unwrap();
        // ingest already dispatched. Call dispatch again, and also a direct
        // second pair on a row that ingest has not started: insert via the
        // store, then dispatch twice.
        assert!(matches!(inserted, IngestOutcome::Started { .. }));
        let second = svc.dispatch("hyphae:twice").await.unwrap();
        assert!(matches!(second, IngestOutcome::AlreadyLinked { .. }));
        assert_eq!(run_count(&store).await, 1);

        // npub1 still has the hanging run in flight, so another explicit row
        // for that actor is Inflight. Park a different actor and dispatch it
        // twice without going through insert dedup.
        let parked = NewInboundEvent {
            id: "hyphae:parked".to_owned(),
            source: "hyphae".to_owned(),
            category: "direct".to_owned(),
            recipient_identity: None,
            thread_key: None,
            actor: Some("npub-parked".to_owned()),
            human: true,
            addressed: "direct".to_owned(),
            at: stamp(30),
            received_at: stamp(0),
            text: Some(format!("parked {SECRET}")),
            meta: None,
            disposition: "explicit".to_owned(),
            disposition_reason: None,
            policy_version: Some("p".to_owned()),
            run_state: Some("starting".to_owned()),
        };
        assert_eq!(
            store.insert_inbound_event(&parked).await.unwrap(),
            InboundInsert::Inserted
        );
        let (left, right) =
            tokio::join!(svc.dispatch("hyphae:parked"), svc.dispatch("hyphae:parked"));
        let mut outcomes = [left.unwrap(), right.unwrap()];
        outcomes.sort_by_key(|outcome| match outcome {
            IngestOutcome::Started { .. } => 0,
            IngestOutcome::AlreadyLinked { .. } => 1,
            other => panic!("unexpected {other:?}"),
        });
        assert!(matches!(outcomes[0], IngestOutcome::Started { .. }));
        assert!(matches!(outcomes[1], IngestOutcome::AlreadyLinked { .. }));
        let parked_runs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM inbound_runs WHERE event_id = 'hyphae:parked'",
        )
        .fetch_one(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
        assert_eq!(parked_runs, 1);
        assert_eq!(run_count(&store).await, 2);
    }

    #[tokio::test]
    async fn an_inbound_run_opens_its_own_session_and_redelivery_is_already_linked() {
        // Mutation: reuse `sess_existing` (or the first session id) on the
        // second dispatch. The session insert fails before the event unique
        // index, so the second call is an error instead of AlreadyLinked.
        let (svc, store) = open_service(Arc::new(Hanging)).await;
        let now = stamp(0);
        sqlx::query(
            "INSERT INTO sessions (id, title, channel, created_at, updated_at) \
             VALUES ('sess_existing', 'owner', 'desktop', ?, ?)",
        )
        .bind(&now)
        .bind(&now)
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
        allow(&store, "hyphae", "npub1").await;
        watermark(&store, "hyphae", &stamp(120)).await;
        let started = svc
            .ingest(message("hyphae:fresh", "npub1", &stamp(30), "hello"))
            .await
            .unwrap();
        let IngestOutcome::Started { run_id } = started else {
            panic!("{started:?}");
        };
        let session_id = store
            .get_run(&run_id)
            .await
            .unwrap()
            .unwrap()
            .session_id
            .unwrap();
        assert_ne!(session_id, "sess_existing");
        assert_eq!(
            store
                .get_session(&session_id)
                .await
                .unwrap()
                .unwrap()
                .channel,
            "hyphae"
        );
        let again = svc.dispatch("hyphae:fresh").await.unwrap();
        assert_eq!(
            again,
            IngestOutcome::AlreadyLinked {
                run_id: Some(run_id.clone())
            }
        );
        assert_eq!(run_count(&store).await, 1);
        let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        assert_eq!(sessions, 2);
        assert_eq!(
            store
                .get_run(&run_id)
                .await
                .unwrap()
                .unwrap()
                .session_id
                .as_deref(),
            Some(session_id.as_str())
        );
    }

    #[tokio::test]
    async fn each_external_source_starts_a_run_on_that_channel() {
        // Mutation: hard-code channel "desktop" and every session channel
        // fails the set check below.
        let (svc, store) = open_service(Arc::new(Fixed)).await;
        for source in agent24_agent::RunManager::INBOUND_SOURCE_CHANNELS {
            allow(&store, source, "npub1").await;
            watermark(&store, source, &stamp(120)).await;
            let mut msg = message(&format!("{source}:one"), "npub1", &stamp(30), "hi");
            msg.source = (*source).to_owned();
            let started = svc.ingest(msg).await.unwrap();
            let IngestOutcome::Started { run_id } = started else {
                panic!("{source}: {started:?}");
            };
            let session_id = store
                .get_run(&run_id)
                .await
                .unwrap()
                .unwrap()
                .session_id
                .unwrap();
            let channel = store
                .get_session(&session_id)
                .await
                .unwrap()
                .unwrap()
                .channel;
            assert_eq!(channel, *source);
            assert!(
                agent24_agent::RunManager::INBOUND_SOURCE_CHANNELS.contains(&channel.as_str()),
                "{channel}"
            );
        }
    }

    #[tokio::test]
    async fn a_source_outside_the_channel_set_does_not_start() {
        // Mutation: drop the channel-set check and this row starts a run
        // whose session channel is `desktop`.
        let (svc, store) = open_service(Arc::new(Fixed)).await;
        let parked = NewInboundEvent {
            id: "desktop:1".to_owned(),
            source: "desktop".to_owned(),
            category: "direct".to_owned(),
            recipient_identity: None,
            thread_key: None,
            actor: Some("npub1".to_owned()),
            human: true,
            addressed: "direct".to_owned(),
            at: stamp(30),
            received_at: stamp(0),
            text: Some("hi".to_owned()),
            meta: None,
            disposition: "explicit".to_owned(),
            disposition_reason: None,
            policy_version: Some("p".to_owned()),
            run_state: Some("starting".to_owned()),
        };
        assert_eq!(
            store.insert_inbound_event(&parked).await.unwrap(),
            InboundInsert::Inserted
        );
        let err = svc.dispatch("desktop:1").await.unwrap_err();
        assert!(
            matches!(err, IngestError::Invalid(ref message) if message.contains("not a session channel")),
            "{err:?}"
        );
        assert_eq!(run_count(&store).await, 0);
        let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        assert_eq!(sessions, 0);
    }

    #[tokio::test]
    async fn off_list_and_stale_and_internal_start_nothing() {
        let (svc, store) = open_service(Arc::new(Fixed)).await;
        watermark(&store, "hyphae", &stamp(60)).await;
        let off = svc
            .ingest(message("hyphae:off", "stranger", &stamp(30), SECRET))
            .await
            .unwrap();
        assert_eq!(
            off,
            IngestOutcome::Ignored {
                reason: "not_allowlisted".to_owned()
            }
        );
        let stale = svc
            .ingest(message("hyphae:old", "stranger", &stamp(120), SECRET))
            .await
            .unwrap();
        // Before the watermark (activated 60s ago). Protocol row 2 beats the allowlist.
        assert_eq!(
            stale,
            IngestOutcome::Ignored {
                reason: "stale".to_owned()
            }
        );
        assert_eq!(run_count(&store).await, 0);
        let err = svc
            .ingest(InboundMessage {
                source: "scheduler".to_owned(),
                category: "direct".to_owned(),
                ..message("scheduler:1", "job", &stamp(0), "tick")
            })
            .await
            .unwrap_err();
        assert!(matches!(err, IngestError::NotInbound), "{err:?}");
        let err = svc
            .ingest(InboundMessage {
                source: "self".to_owned(),
                ..message("self:1", "me", &stamp(0), "wake")
            })
            .await
            .unwrap_err();
        assert!(matches!(err, IngestError::NotInbound), "{err:?}");
        let err = svc
            .ingest(InboundMessage {
                category: "internal".to_owned(),
                ..message("hyphae:int", "npub1", &stamp(0), "inside")
            })
            .await
            .unwrap_err();
        assert!(matches!(err, IngestError::NotInbound), "{err:?}");
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inbound_events")
            .fetch_one(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        // The two ignored external rows only. scheduler / self / internal wrote nothing.
        assert_eq!(rows, 2);
        assert_eq!(run_count(&store).await, 0);
    }

    #[tokio::test]
    async fn dispatch_refuses_a_row_that_is_not_explicit() {
        // Mutation: drop the disposition guard and this ignore row starts a run.
        let (svc, store) = open_service(Arc::new(Fixed)).await;
        let ignored = NewInboundEvent {
            id: "hyphae:ignored".to_owned(),
            source: "hyphae".to_owned(),
            category: "direct".to_owned(),
            recipient_identity: None,
            thread_key: None,
            actor: Some("npub1".to_owned()),
            human: true,
            addressed: "direct".to_owned(),
            at: stamp(30),
            received_at: stamp(0),
            text: Some("no".to_owned()),
            meta: None,
            disposition: "ignore".to_owned(),
            disposition_reason: Some("not_allowlisted".to_owned()),
            policy_version: None,
            run_state: None,
        };
        assert_eq!(
            store.insert_inbound_event(&ignored).await.unwrap(),
            InboundInsert::Inserted
        );
        let err = svc.dispatch("hyphae:ignored").await.unwrap_err();
        assert!(
            matches!(err, IngestError::Invalid(ref message) if message.contains("not explicit")),
            "{err:?}"
        );
        assert_eq!(run_count(&store).await, 0);
        assert_eq!(
            store
                .get_inbound_event("hyphae:ignored")
                .await
                .unwrap()
                .unwrap()
                .disposition,
            "ignore"
        );
    }

    #[tokio::test]
    async fn dispatch_does_not_restart_an_interrupted_or_start_failed_row() {
        // Mutation: drop the `run_state = 'starting'` check in
        // `insert_session_run_for_inbound` and both calls start a run.
        let (svc, store) = open_service(Arc::new(Fixed)).await;
        for (id, actor, state) in [
            ("hyphae:cut", "npub-cut", "interrupted"),
            ("hyphae:broke", "npub-broke", "start_failed"),
        ] {
            let row = NewInboundEvent {
                id: id.to_owned(),
                source: "hyphae".to_owned(),
                category: "direct".to_owned(),
                recipient_identity: None,
                thread_key: None,
                actor: Some(actor.to_owned()),
                human: true,
                addressed: "direct".to_owned(),
                at: stamp(30),
                received_at: stamp(0),
                text: Some("again".to_owned()),
                meta: None,
                disposition: "explicit".to_owned(),
                disposition_reason: None,
                policy_version: Some("p".to_owned()),
                run_state: Some(state.to_owned()),
            };
            assert_eq!(
                store.insert_inbound_event(&row).await.unwrap(),
                InboundInsert::Inserted
            );
            let err = svc.dispatch(id).await.unwrap_err();
            assert!(
                matches!(err, IngestError::Invalid(ref message) if message.contains("not starting")),
                "{err:?}"
            );
            assert_eq!(state_of(&store, id).await, state);
        }
        assert_eq!(run_count(&store).await, 0);
        let links: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inbound_runs")
            .fetch_one(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        assert_eq!(links, 0);
    }

    #[tokio::test]
    async fn ingest_refuses_an_explicit_message_with_no_text_before_storing_it() {
        // Mutation: drop the pre-insert guard in `ingest` and the row is stored
        // as `starting`, holding npub1's in-flight slot until the next startup.
        let (svc, store) = open_service(Arc::new(Fixed)).await;
        allow(&store, "hyphae", "npub1").await;
        watermark(&store, "hyphae", &stamp(120)).await;
        let err = svc
            .ingest(message("hyphae:empty", "npub1", &stamp(30), ""))
            .await
            .unwrap_err();
        assert!(
            matches!(err, IngestError::Invalid(ref message) if message.contains("no text")),
            "{err:?}"
        );
        assert!(
            store
                .get_inbound_event("hyphae:empty")
                .await
                .unwrap()
                .is_none()
        );
        let next = svc
            .ingest(message("hyphae:after", "npub1", &stamp(31), "hi"))
            .await
            .unwrap();
        assert!(matches!(next, IngestOutcome::Started { .. }), "{next:?}");
    }

    #[tokio::test]
    async fn dispatch_refuses_an_explicit_row_with_no_text() {
        // Mutation: drop the `event.text` guard and this starts a run whose
        // prompt is empty.
        let (svc, store) = open_service(Arc::new(Fixed)).await;
        let row = NewInboundEvent {
            id: "hyphae:blank".to_owned(),
            source: "hyphae".to_owned(),
            category: "direct".to_owned(),
            recipient_identity: None,
            thread_key: None,
            actor: Some("npub-blank".to_owned()),
            human: true,
            addressed: "direct".to_owned(),
            at: stamp(30),
            received_at: stamp(0),
            text: None,
            meta: None,
            disposition: "explicit".to_owned(),
            disposition_reason: None,
            policy_version: Some("p".to_owned()),
            run_state: Some("starting".to_owned()),
        };
        assert_eq!(
            store.insert_inbound_event(&row).await.unwrap(),
            InboundInsert::Inserted
        );
        let err = svc.dispatch("hyphae:blank").await.unwrap_err();
        assert!(
            matches!(err, IngestError::Invalid(ref message) if message.contains("no text")),
            "{err:?}"
        );
        assert_eq!(run_count(&store).await, 0);
        assert_eq!(state_of(&store, "hyphae:blank").await, "starting");
    }

    #[tokio::test]
    async fn an_already_linked_outcome_keeps_a_missing_run_id_absent() {
        // The follow-up read found no `inbound_runs` row. Mutation:
        // `unwrap_or_default` makes this `Some("")`.
        assert_eq!(
            already_linked_outcome(None),
            IngestOutcome::AlreadyLinked { run_id: None }
        );
    }

    #[tokio::test]
    async fn dispatch_of_a_linked_terminal_row_names_that_run() {
        // The link is still there, so the id is `Some`. A second dispatch
        // does not insert another run.
        let (svc, store) = open_service(Arc::new(Fixed)).await;
        let row = NewInboundEvent {
            id: "hyphae:named".to_owned(),
            source: "hyphae".to_owned(),
            category: "direct".to_owned(),
            recipient_identity: None,
            thread_key: None,
            actor: Some("npub-named".to_owned()),
            human: true,
            addressed: "direct".to_owned(),
            at: stamp(30),
            received_at: stamp(0),
            text: Some("hi".to_owned()),
            meta: None,
            disposition: "explicit".to_owned(),
            disposition_reason: None,
            policy_version: Some("p".to_owned()),
            run_state: Some("started".to_owned()),
        };
        assert_eq!(
            store.insert_inbound_event(&row).await.unwrap(),
            InboundInsert::Inserted
        );
        insert_run(&store, "run_named", "completed").await;
        store
            .insert_inbound_run("run_named", "hyphae:named")
            .await
            .unwrap();
        let again = svc.dispatch("hyphae:named").await.unwrap();
        assert_eq!(
            again,
            IngestOutcome::AlreadyLinked {
                run_id: Some("run_named".to_owned())
            }
        );
        assert_eq!(run_count(&store).await, 1);
    }

    #[tokio::test]
    async fn a_bad_allowlist_source_refuses_the_ingest() {
        // Mutation: skip the bad allowlist row instead of returning, and the
        // hyphae entry still admits this message.
        let (svc, store) = open_service(Arc::new(Fixed)).await;
        allow(&store, "hyphae", "npub1").await;
        allow(&store, "carrier-pigeon", "npub1").await;
        watermark(&store, "hyphae", &stamp(120)).await;
        let err = svc
            .ingest(message("hyphae:bad-list", "npub1", &stamp(30), "hi"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, IngestError::Invalid(ref message) if message.contains("allowlist source")),
            "{err:?}"
        );
        assert_eq!(event_count(&store).await, 0);
        assert_eq!(run_count(&store).await, 0);
    }

    #[tokio::test]
    async fn an_empty_message_id_is_refused() {
        // Mutation: drop the empty-id guard and this is no longer
        // Invalid("id is empty"). The row is inserted, then start fails
        // because the event id is empty.
        let (svc, store) = open_service(Arc::new(Fixed)).await;
        allow(&store, "hyphae", "npub1").await;
        watermark(&store, "hyphae", &stamp(120)).await;
        let err = svc
            .ingest(message("", "npub1", &stamp(30), "hi"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, IngestError::Invalid(ref message) if message == "id is empty"),
            "{err:?}"
        );
        assert_eq!(event_count(&store).await, 0);
        assert_eq!(run_count(&store).await, 0);
    }

    #[tokio::test]
    async fn a_non_canonical_or_unparseable_timestamp_is_refused() {
        // Mutation: accept a failed parse, or skip the canonical check, and
        // these calls are no longer IngestError::Invalid from prepare.
        let (svc, store) = open_service(Arc::new(Fixed)).await;
        allow(&store, "hyphae", "npub1").await;
        watermark(&store, "hyphae", &stamp(120)).await;
        for (id, at) in [
            ("hyphae:bad-at", "not-a-time"),
            ("hyphae:offset", "2026-11-11T00:00:00+00:00"),
            ("hyphae:fraction", "2026-11-11T00:00:00.000Z"),
        ] {
            let err = svc
                .ingest(message(id, "npub1", at, "hi"))
                .await
                .unwrap_err();
            assert!(
                matches!(err, IngestError::Invalid(ref message) if message.contains(at)),
                "{at}: {err:?}"
            );
        }
        assert_eq!(event_count(&store).await, 0);
        assert_eq!(run_count(&store).await, 0);
    }

    async fn event_count(store: &Store) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM inbound_events")
            .fetch_one(agent24_store::test_hooks::pool(store))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn startup_reconcile_marks_a_linked_live_run_started_and_an_unlinked_one_interrupted() {
        let store = Store::open_memory().await.unwrap();
        insert_event(&store, "hyphae:live", "npub1", "starting").await;
        insert_event(&store, "hyphae:lost", "npub2", "starting").await;
        insert_event(&store, "hyphae:swept", "npub3", "starting").await;
        insert_event(&store, "hyphae:ended", "npub4", "started").await;
        insert_run(&store, "run_live", "awaiting_approval").await;
        insert_run(&store, "run_swept", "queued").await;
        insert_run(&store, "run_ended", "completed").await;
        sqlx::query(
            "INSERT INTO approvals (id, run_id, tool_call_id, kind, summary, payload, \
             available_decisions, status, expires_at, created_at) \
             VALUES ('apr_live', 'run_live', 'tc', 'exec', 's', '{}', '[]', 'pending', \
             '2099-01-01T00:00:00Z', '2026-11-11T00:00:00Z')",
        )
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
        store
            .insert_inbound_run("run_live", "hyphae:live")
            .await
            .unwrap();
        store
            .insert_inbound_run("run_swept", "hyphae:swept")
            .await
            .unwrap();
        store
            .insert_inbound_run("run_ended", "hyphae:ended")
            .await
            .unwrap();

        let swept = store
            .sweep_orphan_runs("2026-11-11T00:00:02Z")
            .await
            .unwrap();
        assert_eq!(swept, 1, "only the queued run is an orphan");
        let report = reconcile_inbound_runs(&store).await.unwrap();
        assert_eq!(report.started, 1);
        assert_eq!(report.interrupted, 1);
        assert_eq!(report.done, 2);
        assert_eq!(run_count(&store).await, 3);
        assert_eq!(state_of(&store, "hyphae:live").await, "started");
        assert_eq!(state_of(&store, "hyphae:lost").await, "interrupted");
        assert_eq!(state_of(&store, "hyphae:swept").await, "done");
        assert_eq!(state_of(&store, "hyphae:ended").await, "done");
        let live = store.get_run("run_live").await.unwrap().unwrap();
        assert_eq!(live.status, RunStatus::AwaitingApproval);
        let again = reconcile_inbound_runs(&store).await.unwrap();
        assert_eq!(again.started, 0);
        assert_eq!(again.done, 0);
        assert_eq!(again.interrupted, 0);
        assert_eq!(run_count(&store).await, 3);
    }

    #[tokio::test]
    async fn reconcile_closes_a_started_row_whose_run_was_deleted() {
        let (svc, store) = open_service(Arc::new(Fixed)).await;
        allow(&store, "hyphae", "npub1").await;
        watermark(&store, "hyphae", &stamp(120)).await;
        insert_event(&store, "hyphae:gone", "npub1", "started").await;
        insert_run(&store, "run_gone", "completed").await;
        store
            .insert_inbound_run("run_gone", "hyphae:gone")
            .await
            .unwrap();
        sqlx::query("DELETE FROM runs WHERE id = 'run_gone'")
            .execute(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        assert!(
            store
                .inbound_run_id_for_event("hyphae:gone")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(state_of(&store, "hyphae:gone").await, "started");

        let blocked = svc
            .ingest(message("hyphae:blocked", "npub1", &stamp(20), "held"))
            .await
            .unwrap();
        assert_eq!(
            blocked,
            IngestOutcome::Ignored {
                reason: "rate_limited".to_owned()
            }
        );

        let report = reconcile_inbound_runs(&store).await.unwrap();
        assert!(report.done >= 1, "{report:?}");
        assert_eq!(state_of(&store, "hyphae:gone").await, "done");
        let next = svc
            .ingest(message("hyphae:after-delete", "npub1", &stamp(20), "free"))
            .await
            .unwrap();
        assert!(matches!(next, IngestOutcome::Started { .. }), "{next:?}");
    }

    async fn insert_event(store: &Store, id: &str, actor: &str, run_state: &str) {
        let mut event = NewInboundEvent {
            id: id.to_owned(),
            source: "hyphae".to_owned(),
            category: "direct".to_owned(),
            recipient_identity: None,
            thread_key: None,
            actor: Some(actor.to_owned()),
            human: true,
            addressed: "direct".to_owned(),
            at: "2026-11-11T00:00:00Z".to_owned(),
            received_at: "2026-11-11T00:00:01Z".to_owned(),
            text: Some("t".to_owned()),
            meta: None,
            disposition: "explicit".to_owned(),
            disposition_reason: None,
            policy_version: Some("p".to_owned()),
            run_state: Some(run_state.to_owned()),
        };
        let _ = &mut event;
        assert_eq!(
            store.insert_inbound_event(&event).await.unwrap(),
            agent24_store::InboundInsert::Inserted
        );
    }

    async fn insert_run(store: &Store, id: &str, status: &str) {
        sqlx::query(
            "INSERT INTO runs (id, status, input, usage, created_at) \
             VALUES (?, ?, \
             '{\"prompt\":\"x\",\"workspace_id\":null,\"knowledge_binding_id\":null}', \
             '{\"prompt_tokens\":0,\"completion_tokens\":0,\"total_tokens\":0,\"cost_usd\":0.0}', \
             '2026-11-11T00:00:00Z')",
        )
        .bind(id)
        .bind(status)
        .execute(agent24_store::test_hooks::pool(store))
        .await
        .unwrap();
    }

    async fn state_of(store: &Store, id: &str) -> String {
        store
            .get_inbound_event(id)
            .await
            .unwrap()
            .unwrap()
            .run_state
            .unwrap()
    }

    async fn wait_inbound_done(store: &Store, id: &str) {
        for _ in 0..100 {
            if state_of(store, id).await == "done" {
                return;
            }
            tokio::time::sleep(StdDuration::from_millis(20)).await;
        }
        panic!("inbound {id} stayed {}", state_of(store, id).await);
    }

    #[tokio::test]
    async fn concurrent_ingests_for_one_actor_start_one_run_and_cancel_frees_the_next() {
        let (store, _dir) = wal_store().await;
        let runs = RunManager::new(
            store.clone(),
            Arc::new(ModelRouter::with_defaults(vec![(
                Arc::new(Hanging) as Arc<dyn ModelProvider>,
                Tier::Local,
            )])),
            Arc::new(agent24_tools::ToolRegistry::new()),
            Arc::new(Noop),
            CancellationToken::new(),
        );
        let svc = InboundService::new(store.clone(), Arc::clone(&runs));
        allow(&store, "hyphae", "npub1").await;
        watermark(&store, "hyphae", &stamp(120)).await;
        let left = message("hyphae:left", "npub1", &stamp(20), "left");
        let right = message("hyphae:right", "npub1", &stamp(20), "right");
        let (a, b) = tokio::join!(svc.ingest(left), svc.ingest(right));
        let mut outcomes = [a.unwrap(), b.unwrap()];
        outcomes.sort_by_key(|outcome| match outcome {
            IngestOutcome::Started { .. } => 0,
            IngestOutcome::Ignored { reason } if reason == "rate_limited" => 1,
            other => panic!("unexpected {other:?}"),
        });
        let IngestOutcome::Started { run_id } = outcomes[0].clone() else {
            panic!("{:?}", outcomes[0]);
        };
        assert_eq!(
            outcomes[1],
            IngestOutcome::Ignored {
                reason: "rate_limited".to_owned()
            }
        );
        assert_eq!(run_count(&store).await, 1);
        let winner: String = sqlx::query_scalar("SELECT event_id FROM inbound_runs")
            .fetch_one(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();

        runs.cancel_run(&run_id).await.unwrap();
        wait_terminal(&store, &run_id).await;
        // 生产上终态事件与把入站行写成 done 之间有很短窗口：run 行先到终态，
        // note_inbound_terminal 随后才落库。轮询，避免读到尚未收敛的 started。
        wait_inbound_done(&store, &winner).await;
        // The pair is one explicit plus one ignore. The window cap is 3, so
        // the same actor's next message is the positive control: it starts.
        let freed = svc
            .ingest(message("hyphae:after", "npub1", &stamp(20), "after"))
            .await
            .unwrap();
        assert!(matches!(freed, IngestOutcome::Started { .. }), "{freed:?}");
        assert_eq!(run_count(&store).await, 2);
    }

    #[tokio::test]
    async fn a_failed_start_releases_the_inflight_slot_for_the_same_actor() {
        let (svc, store) = open_service(Arc::new(Fixed)).await;
        allow(&store, "hyphae", "npub1").await;
        watermark(&store, "hyphae", &stamp(120)).await;
        sqlx::raw_sql(
            "CREATE TRIGGER fail_inbound_run_insert BEFORE INSERT ON runs \
             BEGIN SELECT RAISE(ABORT, 'blocked'); END;",
        )
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();

        let err = svc
            .ingest(message("hyphae:fail", "npub1", &stamp(20), "first"))
            .await
            .unwrap_err();
        assert!(matches!(err, IngestError::Start(_)), "{err:?}");
        assert_eq!(state_of(&store, "hyphae:fail").await, "start_failed");
        assert!(
            store
                .inbound_run_id_for_event("hyphae:fail")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(run_count(&store).await, 0);

        sqlx::query("DROP TRIGGER fail_inbound_run_insert")
            .execute(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        let next = svc
            .ingest(message("hyphae:after-fail", "npub1", &stamp(20), "second"))
            .await
            .unwrap();
        assert!(matches!(next, IngestOutcome::Started { .. }), "{next:?}");
    }

    #[tokio::test]
    async fn concurrent_rate_limited_rewrites_of_one_id_are_a_duplicate() {
        let (store, _dir) = wal_store().await;
        let runs = RunManager::new(
            store.clone(),
            Arc::new(ModelRouter::with_defaults(vec![(
                Arc::new(Fixed) as Arc<dyn ModelProvider>,
                Tier::Local,
            )])),
            Arc::new(agent24_tools::ToolRegistry::new()),
            Arc::new(Noop),
            CancellationToken::new(),
        );
        allow(&store, "hyphae", "npub1").await;
        watermark(&store, "hyphae", &stamp(120)).await;
        insert_event(&store, "hyphae:occupy", "npub1", "starting").await;

        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let left_svc = InboundService::new(store.clone(), Arc::clone(&runs));
        let right_svc = InboundService::new(store.clone(), runs);
        let left_gate = Arc::clone(&barrier);
        let right_gate = barrier;
        let (left, right) = tokio::join!(
            async move {
                left_gate.wait().await;
                left_svc
                    .ingest(message("hyphae:same", "npub1", &stamp(20), "left"))
                    .await
            },
            async move {
                right_gate.wait().await;
                right_svc
                    .ingest(message("hyphae:same", "npub1", &stamp(20), "right"))
                    .await
            },
        );
        let mut outcomes = vec![left.unwrap(), right.unwrap()];
        outcomes.sort_by_key(|outcome| match outcome {
            IngestOutcome::Ignored { reason } if reason == "rate_limited" => 0,
            IngestOutcome::Duplicate => 1,
            other => panic!("unexpected {other:?}"),
        });
        assert_eq!(
            outcomes,
            vec![
                IngestOutcome::Ignored {
                    reason: "rate_limited".to_owned()
                },
                IngestOutcome::Duplicate,
            ]
        );
        let stored = store
            .get_inbound_event("hyphae:same")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.disposition, "ignore");
        assert_eq!(stored.disposition_reason.as_deref(), Some("rate_limited"));
    }

    async fn wal_store() -> (Store, tempfile::TempDir) {
        use std::str::FromStr;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.db");
        let options =
            sqlx::sqlite::SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
                .unwrap()
                .create_if_missing(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .busy_timeout(StdDuration::from_secs(5))
                .foreign_keys(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::migrate!("../../crates/agent24-store/migrations")
            .run(&pool)
            .await
            .unwrap();
        (agent24_store::test_hooks::from_pool(pool), dir)
    }
}
