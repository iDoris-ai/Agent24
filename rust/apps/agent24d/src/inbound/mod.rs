//! Inbound HTTP surface (X11-2f), admission (X11-2b), text expiry (X11-2d)
//! and ingest (X11-2c).
//!
//! `admit` is the pure policy hash and the §3.3 table. `routes` serves HTTP.
//! `service` ingests, starts runs, and reconciles at startup. `policy_version`
//! is already called from `routes`. The development wechat POST calls
//! `InboundService::ingest`, which calls `admit`, so the table types are live
//! in the bin.

mod admit;
mod maintenance;
mod routes;
mod service;

pub(crate) use routes::{
    get_allowlist, get_events, get_policy, post_wechat_event, put_allowlist, put_policy,
    seed_dev_allowlist,
};
#[allow(
    unused_imports,
    reason = "X11-2e and X11-2g call InboundService; this slice wires startup reconcile only"
)]
pub(crate) use service::InboundService;
pub(crate) use service::reconcile_inbound_runs;

use agent24_agent::RunManager;
use agent24_store::Store;
use tokio_util::sync::CancellationToken;

/// Start the 180-day text expiry task with the production clock and period.
///
/// `runs` is the run manager when session memory is configured; the purge goes
/// through it so it takes the session lock. `None` means no memory partition.
pub(crate) fn spawn_text_expiry(
    store: &Store,
    runs: Option<std::sync::Arc<RunManager>>,
    ephemeral: bool,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    maintenance::spawn(
        store.clone(),
        runs.map(|r| r as maintenance::MemoryTarget),
        // A persistent daemon with no memory partition (init failed) must not
        // stamp events whose sessions it cannot purge; only an ephemeral one,
        // which has no memory.db, may.
        !ephemeral,
        std::sync::Arc::new(chrono::Utc::now),
        maintenance::SCRUB_INTERVAL,
        cancel,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::time::Duration;

    use agent24_store::NewInboundEvent;

    use super::*;

    /// An expired event whose run has a session, and one whose run has none.
    async fn seed(store: &Store) {
        let pool = agent24_store::test_hooks::pool(store);
        sqlx::query(
            "INSERT INTO sessions (id, title, channel, created_at, updated_at) \
             VALUES ('s-1', 't', 'hyphae', '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap();
        for (tag, session) in [("sess", Some("s-1")), ("bare", None)] {
            sqlx::query(
                "INSERT INTO runs (id, session_id, status, input, usage, created_at) \
                 VALUES (?, ?, 'completed', '{\"prompt\":\"p\"}', '{}', '2020-01-01T00:00:00Z')",
            )
            .bind(format!("run-{tag}"))
            .bind(session)
            .execute(pool)
            .await
            .unwrap();
            let _ = store
                .insert_inbound_event(&NewInboundEvent {
                    id: format!("hyphae:{tag}"),
                    source: "hyphae".into(),
                    category: "direct".into(),
                    recipient_identity: None,
                    thread_key: None,
                    actor: Some(format!("a-{tag}")),
                    human: true,
                    addressed: "direct".into(),
                    at: "2020-01-01T00:00:00Z".into(),
                    received_at: "2020-01-01T00:00:00Z".into(),
                    text: Some("t".into()),
                    meta: None,
                    disposition: "explicit".into(),
                    disposition_reason: None,
                    policy_version: None,
                    run_state: Some("done".into()),
                })
                .await
                .unwrap();
            store
                .insert_inbound_run(&format!("run-{tag}"), &format!("hyphae:{tag}"))
                .await
                .unwrap();
        }
    }

    async fn stamped(store: &Store, tag: &str) -> bool {
        store
            .get_inbound_event(&format!("hyphae:{tag}"))
            .await
            .unwrap()
            .unwrap()
            .text_scrubbed_at
            .is_some()
    }

    /// Runs the production entry point once and returns whether the session
    /// event was stamped. The session-less event is the "pass has finished"
    /// signal.
    async fn session_event_stamped_after_first_pass(ephemeral: bool) -> bool {
        let store = Store::open_memory().await.unwrap();
        seed(&store).await;
        let cancel = CancellationToken::new();
        let handle = spawn_text_expiry(&store, None, ephemeral, cancel.clone());
        for _ in 0..500 {
            if stamped(&store, "bare").await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(stamped(&store, "bare").await, "the pass never ran");
        cancel.cancel();
        handle.await.unwrap();
        stamped(&store, "sess").await
    }

    #[tokio::test]
    async fn a_persistent_daemon_without_memory_leaves_session_events_unstamped() {
        // Mutation: pass `ephemeral` through unnegated and this goes red.
        assert!(!session_event_stamped_after_first_pass(false).await);
    }

    #[tokio::test]
    async fn an_ephemeral_daemon_clears_session_events_too() {
        assert!(session_event_stamped_after_first_pass(true).await);
    }
}
