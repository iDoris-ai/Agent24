//! K1-6a-G §2.5 (A5b): a grant that expires is told to its module, as a
//! revocation is — at the grant's own expiry instant, with nobody's request
//! to hang it on. One task sleeps until the earliest expiry ahead, wakes
//! early on any decision written, and records each module's answer so a
//! grant is told once; a restart tells what expired while it was down.
//!
//! §2.5.1 (A5c-2): the same task tells the ends written to the outbox that
//! no one answered — a replacement's or an epoch's at once, a revocation's
//! once its route had its chance — each once, with the references it ended.

use std::time::Duration;

use chrono::Utc;
use futures::FutureExt;
use tokio_util::sync::CancellationToken;

use crate::consent_routes::RevocationNotice;
use crate::module_tool_runtime::{GrantEnd, RevocationStop};

/// The longest the task sleeps: an expiry a clock jump moved is told this
/// late at most.
const LONGEST_SLEEP: Duration = Duration::from_secs(300);
/// How soon a round that could not read or record is tried again.
const RETRY: Duration = Duration::from_secs(30);

/// Whether a module is on the grants protocol (K1-6a-G §2.6): it stops by
/// reference, so a `replaced` or `epoch` end may be told to it (#1202 G6).
pub type OnGrantsProtocol = std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync>;

pub async fn run(
    store: agent24_store::Store,
    notice: RevocationNotice,
    on_grants_protocol: OnGrantsProtocol,
    cancel: CancellationToken,
) {
    let mut changes = store.subscribe_module_consent_changes();
    loop {
        // A round that panics ends that round, not the task: every expiry
        // and end after it would go untold (#1190 Low).
        let round = std::panic::AssertUnwindSafe(tell_due(&store, &notice, &on_grants_protocol));
        let sleep = round.catch_unwind().await.unwrap_or_else(|_| {
            tracing::error!("a round of grant-end notices panicked; tried again later");
            RETRY
        });
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(sleep) => {}
            // A decision written, or missed (`Lagged`): read again.
            changed = changes.recv() => {
                if matches!(changed, Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                    return;
                }
            }
        }
    }
}

/// Tells every module whose grant is due, and every end not answered, at
/// once, and records each answer; gives how long to sleep before the next
/// round. A `replaced` or `epoch` end of a module not on the grants
/// protocol is not told: it does not stop by reference, so it would stop
/// work a newer grant let in, or the whole module (#1202 G6). It is
/// recorded `unconfirmed`, never as stopped, and a later revocation
/// carries its references.
async fn tell_due(
    store: &agent24_store::Store,
    notice: &RevocationNotice,
    on_grants_protocol: &OnGrantsProtocol,
) -> Duration {
    let now = Utc::now().to_rfc3339();
    // Each read on its own: an outbox that cannot be read holds back no
    // expiry, nor the other way round.
    let expiries = store
        .due_expiry_notices(&now)
        .await
        .inspect_err(|err| tracing::error!(error = %err, "expired grants could not be read"))
        .ok();
    let outbox = store
        .pending_grant_end_notices(&now)
        .await
        .inspect_err(|err| tracing::error!(error = %err, "grant ends could not be read"))
        .ok();
    let unread = expiries.is_none() || outbox.is_none();
    let (due, next) = expiries.unwrap_or_default();
    let (ends, next_end) = outbox.unwrap_or_default();
    let expired = due.into_iter().map(|grant| async move {
        let end = GrantEnd {
            module: grant.module.clone(),
            operation: grant.op.clone(),
            ended_at: grant.expires_at.clone(),
            reason: "expired",
            authorization_refs: Some(vec![grant.authorization_ref()]),
        };
        let stop = tell(notice, end).await;
        record(store, &grant, stop).await
    });
    let ended = ends.into_iter().map(|end| async move {
        let by_reference_only = end.reason != agent24_store::GrantEndReason::Revoked;
        if by_reference_only && !on_grants_protocol(&end.module) {
            return record_end(store, &end, RevocationStop::Unconfirmed).await;
        }
        let told = GrantEnd {
            module: end.module.clone(),
            operation: end.op.clone(),
            ended_at: end.ended_at.clone(),
            reason: end.reason.word(),
            authorization_refs: Some(end.authorization_refs.clone()),
        };
        let stop = tell(notice, told).await;
        record_end(store, &end, stop).await
    });
    let (expired, ended) = futures::future::join(
        futures::future::join_all(expired),
        futures::future::join_all(ended),
    )
    .await;
    // From when the round ended: telling took up to the notice bound.
    let until_next = next
        .and_then(|at| agent24_store::consent_instant_millis(&at))
        .into_iter()
        .chain(next_end)
        .min()
        .map_or(LONGEST_SLEEP, |at| {
            let ahead = u64::try_from(at - Utc::now().timestamp_millis()).unwrap_or(0);
            // Woken just past the instant: at the instant itself it is
            // still valid.
            Duration::from_millis(ahead + 1).min(LONGEST_SLEEP)
        });
    if unread || expired.contains(&false) || ended.contains(&false) {
        return until_next.min(RETRY);
    }
    until_next
}

/// Tells `end` and gives the module's answer; a notice that panics is
/// one that could not be confirmed, recorded and audited as such, and
/// leaves the other modules' notices of the round (#1190 Low).
async fn tell(notice: &RevocationNotice, end: GrantEnd) -> RevocationStop {
    let module = end.module.clone();
    std::panic::AssertUnwindSafe(async { notice(end).await })
        .catch_unwind()
        .await
        .unwrap_or_else(|_| {
            tracing::error!(
                module,
                "telling a grant's end panicked; recorded unconfirmed"
            );
            RevocationStop::Unconfirmed
        })
}

/// Records the answer to `end`, as [`record`] does an expiry's; a
/// revocation's also on the revocation, which the list reads. `false` if
/// the audit or the row's answer could not be written, so it is told
/// again next round.
async fn record_end(
    store: &agent24_store::Store,
    end: &agent24_store::GrantEndNotice,
    stop: RevocationStop,
) -> bool {
    let tool = Some(end.op.as_str()).filter(|op| *op != "*");
    let detail = serde_json::json!({
        "module": end.module,
        "tool": tool,
        "reason": end.reason.word(),
    });
    if stop == RevocationStop::Unconfirmed
        && let Err(err) = store
            .append_audit(
                &Utc::now().to_rfc3339(),
                "host",
                "module_consent.stop_unconfirmed",
                &detail,
            )
            .await
    {
        tracing::error!(module = end.module, error = %err, "an unconfirmed stop could not be audited");
        return false;
    }
    // The row's first answer stands, and only it reaches the revocation,
    // which the list reads: a route that answered first keeps its own.
    match store.record_grant_end_stop(end.id, stop.into()).await {
        Ok(false) => return true,
        Ok(true) => {}
        Err(err) => {
            tracing::error!(module = end.module, error = %err, "a grant end's stop could not be recorded");
            return false;
        }
    }
    if end.reason == agent24_store::GrantEndReason::Revoked
        && let Err(err) = store
            .record_revocation_stop(&end.module, tool, end.generation, stop.word())
            .await
    {
        // Answered: the list reads this revocation unconfirmed, audited so,
        // as the route does a stop it cannot record.
        tracing::error!(module = end.module, error = %err, "a revocation's stop could not be recorded");
        if stop != RevocationStop::Unconfirmed
            && let Err(err) = store
                .append_audit(
                    &Utc::now().to_rfc3339(),
                    "host",
                    "module_consent.stop_unconfirmed",
                    &detail,
                )
                .await
        {
            tracing::error!(module = end.module, error = %err, "an unconfirmed stop could not be audited");
        }
    }
    true
}

/// Records the answer to `grant`'s expiry, an unconfirmed one audited
/// first so none recorded goes unaudited; `false` if either could not be
/// written, so the grant is told again next round.
async fn record(
    store: &agent24_store::Store,
    grant: &agent24_store::ModuleConsentRecord,
    stop: RevocationStop,
) -> bool {
    if stop == RevocationStop::Unconfirmed {
        let detail = serde_json::json!({
            "module": grant.module,
            "tool": grant.op,
            "reason": "expired",
        });
        if let Err(err) = store
            .append_audit(
                &Utc::now().to_rfc3339(),
                "host",
                "module_consent.stop_unconfirmed",
                &detail,
            )
            .await
        {
            tracing::error!(module = grant.module, error = %err, "an unconfirmed stop could not be audited");
            return false;
        }
    }
    let recorded = store
        .record_expiry_stop(&grant.module, &grant.op, grant.generation, stop.word())
        .await;
    match recorded {
        // Recorded, or a newer decision replaced the grant: its own expiry
        // is its own.
        Ok(_) => true,
        Err(err) => {
            tracing::error!(module = grant.module, error = %err, "an expiry's stop could not be recorded");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_store::{ConsentSource, HostRiskLevel, ToolPermissionSummary};
    use std::sync::{Arc, Mutex};

    fn summary(op: &str) -> ToolPermissionSummary {
        ToolPermissionSummary::new(
            "m",
            op,
            "1.0.0",
            ConsentSource::FirstParty,
            Some("docs".to_owned()),
            None,
            None,
            HostRiskLevel::Low,
        )
    }

    /// Every module on the grants protocol.
    fn on_protocol() -> OnGrantsProtocol {
        Arc::new(|_: &str| true)
    }

    /// A notice that answers `stop` and keeps what it was told.
    fn telling(stop: RevocationStop) -> (Arc<Mutex<Vec<GrantEnd>>>, RevocationNotice) {
        let told = Arc::new(Mutex::new(Vec::new()));
        let notice: RevocationNotice = {
            let told = Arc::clone(&told);
            Arc::new(move |end| {
                told.lock().unwrap().push(end);
                Box::pin(async move { stop }) as futures::future::BoxFuture<'static, _>
            })
        };
        (told, notice)
    }

    /// Runs the task until `told` has `count` notices, and stops it.
    async fn run_until_told(
        store: &agent24_store::Store,
        notice: RevocationNotice,
        told: &Mutex<Vec<GrantEnd>>,
        count: usize,
    ) {
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run(store.clone(), notice, on_protocol(), cancel.clone()));
        tokio::time::timeout(Duration::from_secs(10), async {
            while told.lock().unwrap().len() < count {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the ends were never told");
        // Each answer is recorded once its notice returns.
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the task outlived its cancel")
            .unwrap();
    }

    /// #1202 G6: a `replaced` or `epoch` end of a module not on the grants
    /// protocol is not told, but recorded `unconfirmed` and audited as such;
    /// a revocation's still is.
    #[tokio::test]
    async fn a_module_off_the_grants_protocol_is_told_no_replacement_nor_epoch() {
        let store = agent24_store::Store::open_memory().await.unwrap();
        let (at, until) = ("2026-10-24T00:00:00Z", "2099-01-01T00:00:00Z");
        store
            .grant_module_consent(&summary("t"), at, until)
            .await
            .unwrap();
        store
            .grant_module_consent(&summary("t"), at, until)
            .await
            .unwrap();
        store.advance_module_enable_epoch("m").await.unwrap();
        store
            .revoke_module_consent("n", None, &Utc::now().to_rfc3339())
            .await
            .unwrap();
        let (told, notice) = telling(RevocationStop::Confirmed);
        let cancel = CancellationToken::new();
        let off: OnGrantsProtocol = Arc::new(|_: &str| false);
        let task = tokio::spawn(run(store.clone(), notice, off, cancel.clone()));
        tokio::time::timeout(Duration::from_secs(10), async {
            while !store
                .pending_grant_end_notices("2099-01-01T00:00:00Z")
                .await
                .unwrap()
                .0
                .is_empty()
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the ends were never answered");
        cancel.cancel();
        task.await.unwrap();
        let told: Vec<_> = told
            .lock()
            .unwrap()
            .iter()
            .map(|end| (end.module.clone(), end.reason))
            .collect();
        assert_eq!(told, vec![("n".to_owned(), "revoked")]);
        let mut audited: Vec<String> = store
            .list_audit()
            .await
            .unwrap()
            .iter()
            .filter(|e| e.action == "module_consent.stop_unconfirmed")
            .map(|e| e.detail["reason"].as_str().unwrap().to_owned())
            .collect();
        audited.sort();
        assert_eq!(audited, ["epoch", "replaced"]);
    }

    /// K1-6a-G §2.5.1, cases 19, 20 and 22: ends no route answered are told
    /// with the references they ended — a replacement of a grant past its
    /// expiry but not yet told, an epoch's at once, a revocation's only
    /// once its route had its chance — and each answer is recorded, a
    /// revocation's on the revocation too, an unconfirmed one audited.
    #[tokio::test]
    async fn ends_no_route_answered_are_told_with_their_references() {
        let store = agent24_store::Store::open_memory().await.unwrap();
        let earlier = (Utc::now() - chrono::Duration::seconds(120)).to_rfc3339();
        let past = (Utc::now() - chrono::Duration::seconds(60)).to_rfc3339();
        let until = (Utc::now() + chrono::Duration::days(1)).to_rfc3339();
        // Expired a minute ago, replaced before the expiry was told.
        let lapsed = store
            .grant_module_consent(&summary("t"), &earlier, &past)
            .await
            .unwrap();
        store
            .deny_module_consent(&summary("t"), &Utc::now().to_rfc3339(), &until)
            .await
            .unwrap();
        // Revoked a minute ago, its route gone before it told the module.
        let revoked = store
            .grant_module_consent(&summary("u"), &earlier, &until)
            .await
            .unwrap();
        store
            .revoke_module_consent_tracked("m", Some("u"), &past)
            .await
            .unwrap();
        // Revoked just now: its route still has its chance.
        let fresh = store
            .grant_module_consent(&summary("v"), &Utc::now().to_rfc3339(), &until)
            .await
            .unwrap();
        store
            .revoke_module_consent_tracked("m", Some("v"), &Utc::now().to_rfc3339())
            .await
            .unwrap();
        let (told, notice) = telling(RevocationStop::Unconfirmed);
        run_until_told(&store, notice, &told, 2).await;
        let ends = |told: &Mutex<Vec<GrantEnd>>| {
            let mut ends: Vec<_> = told
                .lock()
                .unwrap()
                .iter()
                .map(|end| {
                    (
                        end.operation.clone(),
                        end.reason,
                        end.authorization_refs.clone(),
                    )
                })
                .collect();
            ends.sort();
            ends
        };
        assert_eq!(
            ends(&told),
            [
                (
                    "t".to_owned(),
                    "replaced",
                    Some(vec![lapsed.authorization_ref()])
                ),
                (
                    "u".to_owned(),
                    "revoked",
                    Some(vec![revoked.authorization_ref()])
                ),
            ]
        );
        assert!(told.lock().unwrap().iter().all(|end| {
            end.authorization_refs
                .as_ref()
                .is_some_and(|refs| !refs.contains(&fresh.authorization_ref()))
        }));
        assert_eq!(
            store
                .revocation_stop_state("m", "u")
                .await
                .unwrap()
                .as_deref(),
            Some("unconfirmed")
        );
        let audited = store.list_audit().await.unwrap();
        let reasons = |reason: &str| {
            audited
                .iter()
                .filter(|e| {
                    e.action == "module_consent.stop_unconfirmed" && e.detail["reason"] == reason
                })
                .count()
        };
        assert_eq!((reasons("replaced"), reasons("revoked")), (1, 1));
        // Answered: not told again. An epoch's end is told at once.
        let current = store
            .grant_module_consent(&summary("w"), &Utc::now().to_rfc3339(), &until)
            .await
            .unwrap();
        store.advance_module_enable_epoch("m").await.unwrap();
        let (told, notice) = telling(RevocationStop::Confirmed);
        run_until_told(&store, notice, &told, 1).await;
        assert_eq!(
            ends(&told),
            [(
                "*".to_owned(),
                "epoch",
                Some(vec![current.authorization_ref()])
            )]
        );
        let (pending, _) = store.pending_grant_end_notices(&until).await.unwrap();
        assert_eq!(
            pending.len(),
            1,
            "only the fresh revocation is left: {pending:?}"
        );
        assert_eq!(pending[0].op, "v");
    }

    /// A route that answers the row while the notifier tells it keeps its
    /// answer: the row's first answer stands, on the revocation too.
    #[tokio::test]
    async fn a_route_s_answer_stands_over_the_notifier_s() {
        let store = agent24_store::Store::open_memory().await.unwrap();
        let revoked_at = (Utc::now() - chrono::Duration::seconds(60)).to_rfc3339();
        let generation = store
            .revoke_module_consent_tracked("m", Some("t"), &revoked_at)
            .await
            .unwrap();
        let told = Arc::new(Mutex::new(Vec::new()));
        let notice: RevocationNotice = {
            let (store, told) = (store.clone(), Arc::clone(&told));
            Arc::new(move |end| {
                let (store, told) = (store.clone(), Arc::clone(&told));
                Box::pin(async move {
                    // The route answers first.
                    let row = store.grant_end_notice(generation).await.unwrap().unwrap();
                    store
                        .record_grant_end_stop(row.id, agent24_store::GrantEndStop::Confirmed)
                        .await
                        .unwrap();
                    store
                        .record_revocation_stop("m", Some("t"), generation, "confirmed")
                        .await
                        .unwrap();
                    told.lock().unwrap().push(end);
                    RevocationStop::NotRunning
                }) as futures::future::BoxFuture<'static, _>
            })
        };
        run_until_told(&store, notice, &told, 1).await;
        assert_eq!(
            store
                .revocation_stop_state("m", "t")
                .await
                .unwrap()
                .as_deref(),
            Some("confirmed")
        );
    }

    /// A revocation its route did not tell is told as soon as its route's
    /// chance has passed, with no decision written to wake the task.
    #[tokio::test]
    async fn a_revocation_is_told_once_its_route_s_chance_passed() {
        let store = agent24_store::Store::open_memory().await.unwrap();
        let until = (Utc::now() + chrono::Duration::days(1)).to_rfc3339();
        let granted = store
            .grant_module_consent(&summary("t"), &Utc::now().to_rfc3339(), &until)
            .await
            .unwrap();
        let revoked_at = (Utc::now() - chrono::Duration::milliseconds(29_500)).to_rfc3339();
        store
            .revoke_module_consent_tracked("m", Some("t"), &revoked_at)
            .await
            .unwrap();
        let (told, notice) = telling(RevocationStop::Confirmed);
        run_until_told(&store, notice, &told, 1).await;
        assert_eq!(
            told.lock().unwrap()[0].authorization_refs,
            Some(vec![granted.authorization_ref()])
        );
    }

    /// K1-6a-G §2.5 (A5b): a grant is told to its module once it expires,
    /// with its tool, its expiry instant and its reference (§2.5.1), and
    /// only once; an answer the module did not confirm is recorded and
    /// audited as an expiry.
    #[tokio::test]
    async fn an_expiring_grant_is_told_once() {
        let store = agent24_store::Store::open_memory().await.unwrap();
        let expires = (Utc::now() + chrono::Duration::milliseconds(300)).to_rfc3339();
        let granted = store
            .grant_module_consent(&summary("t"), &Utc::now().to_rfc3339(), &expires)
            .await
            .unwrap();
        let (told, notice) = telling(RevocationStop::Unconfirmed);
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run(store.clone(), notice, on_protocol(), cancel.clone()));
        tokio::time::timeout(Duration::from_secs(10), async {
            while store.expiry_stop_state("m", "t").await.unwrap().is_none() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the expiry was never told");
        assert_eq!(
            store.expiry_stop_state("m", "t").await.unwrap().as_deref(),
            Some("unconfirmed")
        );
        // A decision written wakes the task; the told grant is not told again.
        store
            .deny_module_consent(&summary("u"), &Utc::now().to_rfc3339(), &expires)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        {
            let told = told.lock().unwrap();
            assert_eq!(told.len(), 1, "told once, a refusal never");
            assert_eq!(
                (told[0].module.as_str(), told[0].operation.as_str()),
                ("m", "t")
            );
            assert_eq!(
                agent24_store::consent_instant_millis(&told[0].ended_at),
                agent24_store::consent_instant_millis(&expires)
            );
            assert_eq!(told[0].reason, "expired");
            assert_eq!(
                told[0].authorization_refs,
                Some(vec![granted.authorization_ref()])
            );
        }
        let audited = store.list_audit().await.unwrap();
        assert!(
            audited
                .iter()
                .any(|e| e.action == "module_consent.stop_unconfirmed"
                    && e.detail["reason"] == "expired"
                    && e.detail["tool"] == "t")
        );
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the task outlived its cancel")
            .unwrap();
    }

    /// #1190 Low: a notice that panics is recorded unconfirmed, audited so,
    /// and the other modules of the round are told all the same.
    #[tokio::test]
    async fn a_notice_that_panics_is_unconfirmed_and_the_others_are_told() {
        let store = agent24_store::Store::open_memory().await.unwrap();
        for module in ["a", "b"] {
            store
                .revoke_module_consent(module, None, &Utc::now().to_rfc3339())
                .await
                .unwrap();
        }
        let told = Arc::new(Mutex::new(Vec::new()));
        let notice: RevocationNotice = {
            let told = Arc::clone(&told);
            Arc::new(move |end: GrantEnd| {
                assert_ne!(end.module, "a", "a's notice panics");
                told.lock().unwrap().push(end);
                Box::pin(async { RevocationStop::Confirmed })
                    as futures::future::BoxFuture<'static, _>
            })
        };
        let pending = tell_due(&store, &notice, &on_protocol()).await;
        assert!(pending <= LONGEST_SLEEP);
        let told: Vec<_> = told
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.module.clone())
            .collect();
        assert_eq!(told, ["b"]);
        assert!(
            store
                .pending_grant_end_notices("2099-01-01T00:00:00Z")
                .await
                .unwrap()
                .0
                .is_empty(),
            "both answered"
        );
        let audited: Vec<_> = store
            .list_audit()
            .await
            .unwrap()
            .iter()
            .filter(|e| e.action == "module_consent.stop_unconfirmed")
            .map(|e| e.detail["module"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(audited, ["a"]);
    }

    /// #1190 Low: a round that panics outside any notice ends that round,
    /// not the task: the next change wakes it, and it tells what is due.
    #[tokio::test]
    async fn the_task_outlives_a_round_that_panics() {
        let store = agent24_store::Store::open_memory().await.unwrap();
        let (at, until) = ("2026-10-24T00:00:00Z", "2099-01-01T00:00:00Z");
        store
            .grant_module_consent(&summary("t"), at, until)
            .await
            .unwrap();
        store.advance_module_enable_epoch("m").await.unwrap();
        let rounds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let first_panics: OnGrantsProtocol = {
            let rounds = Arc::clone(&rounds);
            Arc::new(move |_: &str| {
                let n = rounds.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                assert_ne!(n, 0, "the first round panics");
                true
            })
        };
        let (told, notice) = telling(RevocationStop::Confirmed);
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run(store.clone(), notice, first_panics, cancel.clone()));
        tokio::time::timeout(Duration::from_secs(10), async {
            while rounds.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the first round never ran");
        // A change written wakes the task, if it is still there.
        store
            .revoke_module_consent("z", None, &Utc::now().to_rfc3339())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !told.lock().unwrap().iter().any(|e| e.reason == "epoch") {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the epoch's end was never told");
        cancel.cancel();
        task.await.unwrap();
    }
}
