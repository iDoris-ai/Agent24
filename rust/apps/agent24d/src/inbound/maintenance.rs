//! X11-2d: the background task that expires inbound message text after
//! [`INBOUND_TEXT_RETENTION_DAYS`] days (INBOUND-ADMISSION-PLAN §3.2, §7-3).
//!
//! Same shape as `grant_expiry`: one task, one pass at start, then one pass
//! per interval, ending on the daemon-wide token. What a pass clears and why
//! is in `agent24_store::inbound_scrub`; this file orders the two databases.
//!
//! Order matters. Session memory is in `memory.db`, the rest in the run
//! store, and no transaction spans both. A pass first purges the memory of the
//! sessions the store names, then runs the store transaction that stamps the
//! events. A crash between the two leaves the events unstamped, so the next
//! pass lists the same sessions again and the purge (a delete) repeats as a
//! no-op. The reverse order would lose the list.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use agent24_memory::session_log::SessionPurge;
use agent24_store::{InboundScrubReport, Store, format_inbound_timestamp};
use chrono::{DateTime, Utc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// How long an inbound message body is kept, counted from `received_at`.
pub(crate) const INBOUND_TEXT_RETENTION_DAYS: i64 = 180;
/// Time between passes after the one at start.
pub(crate) const SCRUB_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// Events cleared per transaction; a pass loops until a batch comes back short.
const BATCH: u32 = 500;

/// Time source, replaceable in tests.
pub(crate) type NowFn = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// Purges one session's memory (the inbound part of it).
///
/// Production goes through the run manager so the purge takes the session's
/// own lock (`SessionMemory::purge_session`): a turn write that outlived its
/// cancelled caller still holds that lock, and the purge must queue behind it.
pub(crate) trait SessionPurger: Send + Sync {
    fn purge<'a>(
        &'a self,
        session: &'a str,
        not_after: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<SessionPurge, String>> + Send + 'a>>;
}

impl SessionPurger for agent24_agent::RunManager {
    fn purge<'a>(
        &'a self,
        session: &'a str,
        not_after: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<SessionPurge, String>> + Send + 'a>> {
        Box::pin(async move {
            self.purge_session_memory(session, not_after)
                .await
                .ok_or_else(|| "no session memory configured".to_owned())?
                .map_err(|e| e.to_string())
        })
    }
}

/// The session-memory partition to purge; `None` where there is none.
pub(crate) type MemoryTarget = Arc<dyn SessionPurger>;

/// What one pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PassReport {
    pub store: InboundScrubReport,
    pub memory_sessions: u64,
    pub memory_events: u64,
    /// Sessions whose memory was kept because it holds events newer than the
    /// inbound runs (the user carried on in them).
    pub memory_kept_newer: u64,
}

impl PassReport {
    pub(crate) fn is_empty(&self) -> bool {
        self.store.is_empty()
            && self.memory_sessions == 0
            && self.memory_events == 0
            && self.memory_kept_newer == 0
    }
}

/// One pass at `now`. Fails before the store transaction if the memory purge
/// fails, so nothing is stamped whose memory copy still exists.
///
/// `memory_matters` is false only for an ephemeral daemon, which has no
/// `memory.db` to leave text in; a persistent daemon whose memory partition
/// failed to initialise passes true with `memory == None`.
pub(crate) async fn run_pass(
    store: &Store,
    memory: Option<&MemoryTarget>,
    memory_matters: bool,
    now: DateTime<Utc>,
) -> Result<PassReport, String> {
    run_pass_with(store, memory, memory_matters, now, || async {}).await
}

/// [`run_pass`] with a hook that runs between listing the sessions and the
/// store transaction, so a test can put a run into the gap.
async fn run_pass_with<F, Fut>(
    store: &Store,
    memory: Option<&MemoryTarget>,
    memory_matters: bool,
    now: DateTime<Utc>,
    after_list: F,
) -> Result<PassReport, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let cutoff =
        format_inbound_timestamp(now - chrono::Duration::days(INBOUND_TEXT_RETENTION_DAYS));
    let stamp = format_inbound_timestamp(now);
    let mut report = PassReport::default();
    // Sessions whose memory this pass dealt with. The store transaction
    // refuses to stamp an event whose exclusive session is not in here, so a
    // run that ended after the list was read cannot get its event stamped
    // ahead of its memory purge.
    let mut handled: Vec<String> = Vec::new();

    if let Some(target) = memory {
        let sessions = store
            .inbound_scrub_sessions(&cutoff)
            .await
            .map_err(|e| format!("listing expired inbound sessions: {e}"))?;
        after_list().await;
        for session in sessions {
            // Re-ask right before the delete: a run that joined the session
            // since the list was read (the user carried on in the UI) keeps
            // its memory. The window is narrowed, not closed: the two
            // databases share no transaction.
            if !store
                .inbound_scrub_session_pending(&cutoff, &session.id)
                .await
                .map_err(|e| format!("re-checking inbound session {}: {e}", session.id))?
            {
                continue;
            }
            let purge = target
                .purge(&session.id, &session.memory_not_after)
                .await
                .map_err(|e| format!("purging memory of session {}: {e}", session.id))?;
            // A session the user carried on in keeps its memory (the runs'
            // copies still go and the event is stamped, as for a shared
            // session), so it counts as handled all the same.
            if purge.kept_newer {
                report.memory_kept_newer += 1;
            } else if !purge.is_empty() {
                report.memory_sessions += 1;
                report.memory_events += purge.events;
            }
            handled.push(session.id);
        }
    } else {
        after_list().await;
    }

    // Without a memory partition nothing is handled, so events in exclusive
    // sessions stay unstamped (stamping would drop the session from every
    // later pass and strand its text in memory.db). They are cleared on a
    // later pass once memory is available, which means the next daemon start
    // with the partition initialised; the 6-hour tick cannot bring it back
    // within one process. Events in shared sessions or with no session are
    // cleared as usual. An ephemeral daemon has no memory.db, so it passes
    // `None` and clears everything.
    let constraint = (memory.is_some() || memory_matters).then_some(handled.as_slice());
    loop {
        let batch = store
            .scrub_expired_inbound(&cutoff, &stamp, BATCH, constraint)
            .await
            .map_err(|e| format!("scrubbing expired inbound text: {e}"))?;
        report.store.events += batch.events;
        report.store.runs += batch.runs;
        report.store.run_messages += batch.run_messages;
        report.store.tool_calls += batch.tool_calls;
        report.store.approvals += batch.approvals;
        report.store.sessions += batch.sessions;
        // Every batch counts the same standing backlog; keep the latest.
        report.store.deferred = batch.deferred;
        if batch.events < u64::from(BATCH) {
            if batch.deferred > 0 {
                tracing::warn!(
                    deferred = batch.deferred,
                    "expired inbound events left unstamped until their session memory is handled (memory partition missing, or a run ended mid-pass)"
                );
            }
            return Ok(report);
        }
    }
}

/// Start the task: a pass now, then one every `interval`, until `cancel`.
pub(crate) fn spawn(
    store: Store,
    memory: Option<MemoryTarget>,
    memory_matters: bool,
    now: NowFn,
    interval: Duration,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            // A pass is racing shutdown rather than blocking it: dropped
            // mid-way, it rolls back or repeats as a no-op next start.
            tokio::select! {
                () = cancel.cancelled() => return,
                result = run_pass(&store, memory.as_ref(), memory_matters, now()) => match result {
                    Ok(report) if report.is_empty() => {}
                    Ok(report) => tracing::info!(?report, "inbound text expiry pass"),
                    Err(err) => tracing::warn!(error = %err, "inbound text expiry pass failed; will retry"),
                },
            }
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(interval) => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::atomic::{AtomicUsize, Ordering};

    use agent24_memory::KvStore;
    use agent24_memory::event::{Origin, Trust};
    use agent24_models::Msg;
    use agent24_store::{INBOUND_SCRUB_PLACEHOLDER, NewInboundEvent};
    use chrono::TimeZone;

    use super::*;

    const OWNER: &str = "owner-1";

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap()
    }

    fn days_ago(days: i64) -> String {
        format_inbound_timestamp(now() - chrono::Duration::days(days))
    }

    /// Test purger over a bare memory base (no run manager, no session lock).
    struct KvPurger {
        kv: KvStore,
        owner: String,
    }

    impl SessionPurger for KvPurger {
        fn purge<'a>(
            &'a self,
            session: &'a str,
            not_after: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<SessionPurge, String>> + Send + 'a>> {
            Box::pin(async move {
                self.kv
                    .session_log()
                    .purge_session(&self.owner, session, not_after)
                    .await
                    .map_err(|e| e.to_string())
            })
        }
    }

    struct Fixture {
        store: Store,
        kv: KvStore,
    }

    impl Fixture {
        async fn new() -> Self {
            Self {
                store: Store::open_memory().await.unwrap(),
                kv: KvStore::open_memory().await.unwrap(),
            }
        }

        fn target(&self) -> MemoryTarget {
            Arc::new(KvPurger {
                kv: self.kv.clone(),
                owner: OWNER.to_owned(),
            })
        }

        fn pool(&self) -> &sqlx::SqlitePool {
            agent24_store::test_hooks::pool(&self.store)
        }

        /// A session with one run, its message, tool call, approval, and
        /// one turn of session memory. `inbound_age_days = Some(n)` also
        /// stores an inbound event received n days ago, linked to the run.
        async fn seed(
            &self,
            tag: &str,
            session: &str,
            status: &str,
            inbound_age_days: Option<i64>,
        ) {
            let pool = self.pool();
            sqlx::query(
                "INSERT OR IGNORE INTO sessions (id, title, channel, created_at, updated_at) \
                 VALUES (?, ?, 'hyphae', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            )
            .bind(session)
            .bind(format!("title-{tag}"))
            .execute(pool)
            .await
            .unwrap();
            let run = format!("run-{tag}");
            sqlx::query(
                "INSERT INTO runs (id, session_id, status, input, output, error, usage, created_at, ended_at) \
                 VALUES (?, ?, ?, ?, ?, ?, '{}', '2026-01-01T00:00:00Z', ?)",
            )
            .bind(&run)
            .bind(session)
            .bind(status)
            .bind(format!(
                r#"{{"prompt":"secret-{tag}","workspace_id":null,"mode":"normal","knowledge_binding_id":null}}"#
            ))
            .bind(format!(r#"{{"text":"answer-{tag}"}}"#))
            .bind(format!(r#"{{"code":"internal","message":"err-{tag}"}}"#))
            // The run's own memory write happens just before this instant.
            .bind(format_inbound_timestamp(Utc::now()))
            .execute(pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO run_messages (run_id, seq, role, content, tool_calls, created_at) \
                 VALUES (?, 0, 'user', ?, '[]', '2026-01-01T00:00:00Z'), \
                        (?, 1, 'assistant', NULL, ?, '2026-01-01T00:00:00Z')",
            )
            .bind(&run)
            .bind(format!("snapshot-{tag}"))
            .bind(&run)
            .bind(format!(
                r#"[{{"id":"c-{tag}","name":"x","arguments":"leak-{tag}"}}]"#
            ))
            .execute(pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO tool_calls (id, run_id, tool, input, status, output_summary, started_at) \
                 VALUES (?, ?, 'x', ?, 'completed', ?, '2026-01-01T00:00:00Z')",
            )
            .bind(format!("tc-{tag}"))
            .bind(&run)
            .bind(format!(r#"{{"q":"leak-{tag}"}}"#))
            .bind(format!("summary-{tag}"))
            .execute(pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO approvals (id, run_id, tool_call_id, kind, summary, payload, \
                 available_decisions, status, expires_at, created_at) \
                 VALUES (?, ?, ?, 'tool', ?, ?, '[]', 'pending', '2027-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            )
            .bind(format!("ap-{tag}"))
            .bind(&run)
            .bind(format!("tc-{tag}"))
            .bind(format!("approve-{tag}"))
            .bind(format!(r#"{{"cmd":"leak-{tag}"}}"#))
            .execute(pool)
            .await
            .unwrap();
            let turn = (self.memory_texts(session).await.len() / 2) as u64;
            self.kv
                .session_log()
                .append_turn(
                    OWNER,
                    session,
                    turn,
                    &Msg::user(format!("memory-{tag}")),
                    Origin {
                        source: "test".into(),
                        trust: Trust::UserSaid,
                    },
                    &Msg::assistant(Some(format!("memory-answer-{tag}")), vec![]),
                    Origin {
                        source: "test".into(),
                        trust: Trust::Model,
                    },
                )
                .await
                .unwrap();
            if let Some(age) = inbound_age_days {
                let id = format!("hyphae:{tag}");
                let outcome = self
                    .store
                    .insert_inbound_event(&NewInboundEvent {
                        id: id.clone(),
                        source: "hyphae".into(),
                        category: "direct".into(),
                        recipient_identity: None,
                        thread_key: None,
                        actor: Some(format!("actor-{tag}")),
                        human: true,
                        addressed: "direct".into(),
                        at: days_ago(age),
                        received_at: days_ago(age),
                        text: Some(format!("secret-{tag}")),
                        meta: Some(format!("meta-{tag}")),
                        disposition: "explicit".into(),
                        disposition_reason: None,
                        policy_version: None,
                        run_state: Some("done".into()),
                    })
                    .await
                    .unwrap();
                assert_eq!(outcome, agent24_store::InboundInsert::Inserted);
                self.store.insert_inbound_run(&run, &id).await.unwrap();
            }
        }

        /// Every copy of `tag`'s text, as one comparable string.
        async fn copies(&self, tag: &str, session: &str) -> String {
            let pool = self.pool();
            let run = format!("run-{tag}");
            let one = |sql: &'static str, bind: String| async move {
                sqlx::query_scalar::<_, Option<String>>(sql)
                    .bind(bind)
                    .fetch_all(pool)
                    .await
                    .unwrap()
            };
            let mut out = Vec::new();
            out.push(format!(
                "{:?}",
                one(
                    "SELECT input || '|' || output || '|' || error FROM runs WHERE id = ?",
                    run.clone()
                )
                .await
            ));
            out.push(format!(
                "{:?}",
                one(
                    "SELECT content || '/' || tool_calls FROM run_messages WHERE run_id = ? ORDER BY seq",
                    run.clone()
                )
                .await
            ));
            out.push(format!(
                "{:?}",
                one(
                    "SELECT input || '|' || output_summary FROM tool_calls WHERE run_id = ?",
                    run.clone()
                )
                .await
            ));
            out.push(format!(
                "{:?}",
                one(
                    "SELECT summary || '|' || payload FROM approvals WHERE run_id = ?",
                    run
                )
                .await
            ));
            out.push(format!(
                "{:?}",
                one(
                    "SELECT title FROM sessions WHERE id = ?",
                    session.to_owned()
                )
                .await
            ));
            out.push(format!("{:?}", self.memory_texts(session).await));
            out.join("\n")
        }

        async fn memory_texts(&self, session: &str) -> Vec<String> {
            let view = self
                .kv
                .session_log()
                .load_view(OWNER, session)
                .await
                .unwrap();
            view.tail
                .iter()
                .map(|(_, m)| format!("{}:{}", m.role, m.content.clone().unwrap_or_default()))
                .collect()
        }

        async fn event_text(&self, tag: &str) -> (Option<String>, Option<String>, bool) {
            let ev = self
                .store
                .get_inbound_event(&format!("hyphae:{tag}"))
                .await
                .unwrap()
                .unwrap();
            (ev.text, ev.meta, ev.text_scrubbed_at.is_some())
        }
    }

    #[tokio::test]
    async fn expiry_clears_old_copies_and_leaves_young_and_plain_runs_untouched() {
        let fx = Fixture::new().await;
        fx.seed("old", "s-old", "completed", Some(181)).await;
        fx.seed("young", "s-young", "completed", Some(179)).await;
        fx.seed("plain", "s-plain", "completed", None).await;
        let young_before = fx.copies("young", "s-young").await;
        let plain_before = fx.copies("plain", "s-plain").await;
        assert!(young_before.contains("secret-young") && young_before.contains("memory-young"));
        assert!(fx.copies("old", "s-old").await.contains("memory-old"));

        let report = run_pass(&fx.store, Some(&fx.target()), true, now())
            .await
            .unwrap();
        assert_eq!(report.store.events, 1);
        assert_eq!(report.store.runs, 1);
        assert_eq!(report.store.run_messages, 2);
        assert_eq!(report.store.tool_calls, 1);
        assert_eq!(report.store.approvals, 1);
        assert_eq!(report.store.sessions, 1);
        assert_eq!(report.memory_sessions, 1);
        assert_eq!(report.memory_events, 2);

        // The 181-day event and every copy of it.
        assert_eq!(fx.event_text("old").await, (None, None, true));
        let old = fx.copies("old", "s-old").await;
        assert!(old.contains(INBOUND_SCRUB_PLACEHOLDER), "{old}");
        for leak in [
            "secret-old",
            "snapshot-old",
            "leak-old",
            "summary-old",
            "answer-old",
            "approve-old",
            "title-old",
            "memory-old",
        ] {
            assert!(!old.contains(leak), "{leak} survived: {old}");
        }
        // Rows still exist, so foreign keys and recovery lookups hold.
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE id = 'run-old'")
            .fetch_one(fx.pool())
            .await
            .unwrap();
        assert_eq!(rows, 1);
        let kept = fx
            .store
            .get_inbound_event("hyphae:old")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(kept.actor.as_deref(), Some("actor-old"));
        assert_eq!(kept.disposition, "explicit");

        // Positive controls: 179 days and a plain run are byte-for-byte as before.
        assert_eq!(fx.copies("young", "s-young").await, young_before);
        assert_eq!(fx.copies("plain", "s-plain").await, plain_before);
        let (text, meta, scrubbed) = fx.event_text("young").await;
        assert_eq!(text.as_deref(), Some("secret-young"));
        assert_eq!(meta.as_deref(), Some("meta-young"));
        assert!(!scrubbed);

        // Second pass changes nothing.
        let again = run_pass(&fx.store, Some(&fx.target()), true, now())
            .await
            .unwrap();
        assert!(again.is_empty(), "{again:?}");
        assert_eq!(fx.copies("young", "s-young").await, young_before);
    }

    #[tokio::test]
    async fn a_live_run_defers_its_event_until_it_is_terminal() {
        let fx = Fixture::new().await;
        fx.seed("live", "s-live", "running", Some(200)).await;
        let before = fx.copies("live", "s-live").await;
        let report = run_pass(&fx.store, Some(&fx.target()), true, now())
            .await
            .unwrap();
        assert!(report.is_empty(), "{report:?}");
        assert_eq!(fx.copies("live", "s-live").await, before);
        assert!(!fx.event_text("live").await.2);

        sqlx::query("UPDATE runs SET status = 'completed' WHERE id = 'run-live'")
            .execute(fx.pool())
            .await
            .unwrap();
        let report = run_pass(&fx.store, Some(&fx.target()), true, now())
            .await
            .unwrap();
        assert_eq!(report.store.events, 1);
        assert!(!fx.copies("live", "s-live").await.contains("secret-live"));
    }

    #[tokio::test]
    async fn a_session_shared_with_a_plain_run_keeps_its_memory() {
        // Memory cannot be split per run, so a session that also holds a
        // non-inbound run is never purged. The run copies still go.
        let fx = Fixture::new().await;
        fx.seed("shared", "s-shared", "completed", Some(200)).await;
        fx.seed("other", "s-shared", "completed", None).await;
        let report = run_pass(&fx.store, Some(&fx.target()), true, now())
            .await
            .unwrap();
        assert_eq!(report.store.events, 1);
        assert_eq!(report.memory_sessions, 0);
        assert_eq!(report.store.sessions, 0);
        let texts = fx.memory_texts("s-shared").await;
        assert!(
            texts.iter().any(|t| t.contains("memory-shared")),
            "{texts:?}"
        );
        let title: String = sqlx::query_scalar("SELECT title FROM sessions WHERE id = 's-shared'")
            .fetch_one(fx.pool())
            .await
            .unwrap();
        assert_ne!(title, INBOUND_SCRUB_PLACEHOLDER);
        let own: String = sqlx::query_scalar("SELECT input FROM runs WHERE id = 'run-other'")
            .fetch_one(fx.pool())
            .await
            .unwrap();
        assert!(own.contains("secret-other"));
    }

    #[tokio::test]
    async fn a_chat_turn_written_after_the_run_keeps_the_sessions_memory() {
        // `/chat` writes a user turn into a caller-named session without a
        // run. The inbound runs' own memory is older than that turn, so the
        // session is kept whole (the user's conversation), while the run
        // copies are still cleared and the event stamped.
        let fx = Fixture::new().await;
        fx.seed("talk", "s-talk", "completed", Some(181)).await;
        // The inbound run ended long ago; the user chats now.
        sqlx::query("UPDATE runs SET ended_at = ? WHERE id = 'run-talk'")
            .bind(format_inbound_timestamp(
                Utc::now() - chrono::Duration::seconds(30),
            ))
            .execute(fx.pool())
            .await
            .unwrap();
        fx.kv
            .session_log()
            .append_turn(
                OWNER,
                "s-talk",
                1,
                &Msg::user("my-own-chat-question"),
                Origin {
                    source: "chat".into(),
                    trust: Trust::UserSaid,
                },
                &Msg::assistant(Some("my-own-chat-answer".into()), vec![]),
                Origin {
                    source: "agent_loop".into(),
                    trust: Trust::Model,
                },
            )
            .await
            .unwrap();

        let report = run_pass(&fx.store, Some(&fx.target()), true, now())
            .await
            .unwrap();
        assert_eq!(report.memory_kept_newer, 1, "{report:?}");
        assert_eq!(report.memory_sessions, 0);
        // A kept session counts as handled: its event is stamped, not deferred
        // on every later pass.
        assert_eq!(report.store.deferred, 0, "{report:?}");
        let memory = fx.memory_texts("s-talk").await.join("|");
        assert!(memory.contains("my-own-chat-question"), "{memory}");
        assert!(memory.contains("memory-talk"), "kept whole: {memory}");
        // The run's copies are cleared and the event is stamped.
        assert!(fx.event_text("talk").await.2);
        assert!(!fx.copies("talk", "s-talk").await.contains("secret-talk"));
        // Positive control is `expiry_clears_old_copies...`: without a later
        // turn the whole session's memory goes.
    }

    #[tokio::test]
    async fn a_chat_turn_written_while_the_run_was_running_keeps_the_memory() {
        // The user chats into the session while the inbound run is parked
        // (say, awaiting approval); the run ends hours later. The turn is
        // OLDER than the run's end, so only its origin (`chat`) marks it.
        let fx = Fixture::new().await;
        fx.seed("busy", "s-busy", "completed", Some(181)).await;
        fx.kv
            .session_log()
            .append_turn(
                OWNER,
                "s-busy",
                1,
                &Msg::user("chat-while-running"),
                Origin {
                    source: "chat".into(),
                    trust: Trust::UserSaid,
                },
                &Msg::assistant(Some("chat-answer".into()), vec![]),
                Origin {
                    source: "agent_loop".into(),
                    trust: Trust::Model,
                },
            )
            .await
            .unwrap();
        // The run ended well after both writes.
        sqlx::query("UPDATE runs SET ended_at = ? WHERE id = 'run-busy'")
            .bind(format_inbound_timestamp(
                Utc::now() + chrono::Duration::hours(3),
            ))
            .execute(fx.pool())
            .await
            .unwrap();

        let report = run_pass(&fx.store, Some(&fx.target()), true, now())
            .await
            .unwrap();
        assert_eq!(report.memory_kept_newer, 1, "{report:?}");
        assert_eq!(report.store.deferred, 0);
        let memory = fx.memory_texts("s-busy").await.join("|");
        assert!(memory.contains("chat-while-running"), "{memory}");
        assert!(fx.event_text("busy").await.2);
        assert!(!fx.copies("busy", "s-busy").await.contains("secret-busy"));
        // Positive control: `expiry_clears_old_copies...` (no chat turn).
    }

    #[tokio::test]
    async fn without_a_memory_partition_session_events_are_not_stamped() {
        // Persistent daemon whose memory init failed: an event whose run has a
        // session must stay unstamped (its memory.db copy is out of reach and
        // a stamp would hide the session from every later pass). An event
        // with no session is still cleared.
        let fx = Fixture::new().await;
        fx.seed("sess", "s-sess", "completed", Some(181)).await;
        fx.seed("bare", "s-bare", "completed", Some(181)).await;
        sqlx::query("UPDATE runs SET session_id = NULL WHERE id = 'run-bare'")
            .execute(fx.pool())
            .await
            .unwrap();
        let before = fx.copies("sess", "s-sess").await;

        let report = run_pass(&fx.store, None, true, now()).await.unwrap();
        assert_eq!(report.store.events, 1, "{report:?}");
        assert_eq!(report.store.deferred, 1);
        assert_eq!(
            fx.event_text("sess").await,
            (Some("secret-sess".into()), Some("meta-sess".into()), false)
        );
        assert_eq!(fx.copies("sess", "s-sess").await, before);
        assert_eq!(fx.event_text("bare").await, (None, None, true));

        // Memory comes back: the same event is now cleared, memory included.
        let report = run_pass(&fx.store, Some(&fx.target()), true, now())
            .await
            .unwrap();
        assert_eq!(report.store.events, 1);
        assert_eq!(report.memory_sessions, 1);
        assert!(fx.event_text("sess").await.2);
        assert!(!fx.copies("sess", "s-sess").await.contains("memory-sess"));
    }

    #[tokio::test]
    async fn a_run_ending_between_the_list_and_the_stamp_does_not_strand_memory() {
        // The session is not listed (its run is live), the run then ends, and
        // the store pass must not stamp the event ahead of the memory purge.
        let fx = Fixture::new().await;
        fx.seed("race", "s-race", "running", Some(200)).await;
        let report = run_pass_with(&fx.store, Some(&fx.target()), true, now(), || async {
            sqlx::query("UPDATE runs SET status = 'completed' WHERE id = 'run-race'")
                .execute(fx.pool())
                .await
                .unwrap();
        })
        .await
        .unwrap();
        assert_eq!(report.store.events, 0, "{report:?}");
        assert_eq!(report.store.deferred, 1);
        assert!(!fx.event_text("race").await.2);
        assert!(fx.copies("race", "s-race").await.contains("memory-race"));

        // The next pass lists the session, purges its memory, then stamps.
        let report = run_pass(&fx.store, Some(&fx.target()), true, now())
            .await
            .unwrap();
        assert_eq!(report.store.events, 1);
        assert_eq!(report.memory_sessions, 1);
        assert!(fx.event_text("race").await.2);
        assert!(!fx.copies("race", "s-race").await.contains("memory-race"));
    }

    #[tokio::test]
    async fn an_ephemeral_daemon_without_memory_still_scrubs_the_store() {
        let fx = Fixture::new().await;
        fx.seed("old", "s-old", "completed", Some(181)).await;
        let report = run_pass(&fx.store, None, false, now()).await.unwrap();
        assert_eq!(report.store.events, 1);
        assert_eq!(report.store.deferred, 0);
        assert_eq!(report.memory_sessions, 0);
    }

    #[tokio::test]
    async fn runs_a_pass_at_start_then_periodically_and_stops_on_cancel() {
        let fx = Fixture::new().await;
        fx.seed("old", "s-old", "completed", Some(181)).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let clock: NowFn = {
            let calls = Arc::clone(&calls);
            Arc::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                now()
            })
        };

        // A long interval: only the start-up pass can have happened.
        let cancel = CancellationToken::new();
        let handle = spawn(
            fx.store.clone(),
            Some(fx.target()),
            true,
            Arc::clone(&clock),
            Duration::from_secs(3600),
            cancel.clone(),
        );
        wait_until(|| calls.load(Ordering::SeqCst) >= 1).await;
        wait_until_async(|| async { fx.event_text("old").await.2 }).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("task ends on cancel")
            .unwrap();

        // A short interval: it keeps going, then stops.
        calls.store(0, Ordering::SeqCst);
        let cancel = CancellationToken::new();
        let handle = spawn(
            fx.store.clone(),
            None,
            false,
            clock,
            Duration::from_millis(10),
            cancel.clone(),
        );
        wait_until(|| calls.load(Ordering::SeqCst) >= 3).await;
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("task ends on cancel")
            .unwrap();
        let stopped_at = calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(calls.load(Ordering::SeqCst), stopped_at);
    }

    #[tokio::test]
    async fn an_already_cancelled_token_ends_the_task_without_hanging() {
        let fx = Fixture::new().await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let handle = spawn(
            fx.store.clone(),
            None,
            false,
            Arc::new(now),
            Duration::from_secs(3600),
            cancel,
        );
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("task ends")
            .unwrap();
    }

    async fn wait_until(cond: impl Fn() -> bool) {
        for _ in 0..500 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition not reached");
    }

    async fn wait_until_async<F, Fut>(cond: F)
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        for _ in 0..500 {
            if cond().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition not reached");
    }
}
