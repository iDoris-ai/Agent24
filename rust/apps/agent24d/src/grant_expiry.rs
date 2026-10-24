//! K1-6a-G §2.5 (A5b): a grant that expires is told to its module, as a
//! revocation is — at the grant's own expiry instant, with nobody's request
//! to hang it on. One task sleeps until the earliest expiry ahead, wakes
//! early on any decision written, and records each module's answer so a
//! grant is told once; a restart tells what expired while it was down.

use std::time::Duration;

use chrono::Utc;
use tokio_util::sync::CancellationToken;

use crate::consent_routes::RevocationNotice;
use crate::module_tool_runtime::RevocationStop;

/// The longest the task sleeps: an expiry a clock jump moved is told this
/// late at most.
const LONGEST_SLEEP: Duration = Duration::from_secs(300);
/// How soon a round that could not read or record is tried again.
const RETRY: Duration = Duration::from_secs(30);

pub async fn run(store: agent24_store::Store, notice: RevocationNotice, cancel: CancellationToken) {
    let mut changes = store.subscribe_module_consent_changes();
    loop {
        let sleep = tell_due(&store, &notice).await;
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

/// Tells every module whose grant is due, at once, and records each
/// answer; gives how long to sleep before the next round.
async fn tell_due(store: &agent24_store::Store, notice: &RevocationNotice) -> Duration {
    let now = Utc::now();
    let (due, next) = match store.due_expiry_notices(&now.to_rfc3339()).await {
        Ok(found) => found,
        Err(err) => {
            tracing::error!(error = %err, "expired grants could not be read");
            return RETRY;
        }
    };
    let told = futures::future::join_all(due.into_iter().map(|grant| async move {
        let stop = notice(
            grant.module.clone(),
            grant.op.clone(),
            grant.expires_at.clone(),
        )
        .await;
        record(store, &grant, stop).await
    }))
    .await;
    if told.contains(&false) {
        return RETRY;
    }
    next.and_then(|at| agent24_store::consent_instant_millis(&at))
        .map_or(LONGEST_SLEEP, |at| {
            let ahead = u64::try_from(at - now.timestamp_millis()).unwrap_or(0);
            // Woken just past the instant: at the instant itself it is
            // still valid.
            Duration::from_millis(ahead + 1).min(LONGEST_SLEEP)
        })
}

/// Records the answer to `grant`'s expiry; `false` if it could not be,
/// so the grant is told again next round.
async fn record(
    store: &agent24_store::Store,
    grant: &agent24_store::ModuleConsentRecord,
    stop: RevocationStop,
) -> bool {
    let recorded = store
        .record_expiry_stop(&grant.module, &grant.op, grant.generation, stop.word())
        .await;
    match recorded {
        // A newer decision replaced the grant: its own expiry is its own.
        Ok(false) => return true,
        Ok(true) => {}
        Err(err) => {
            tracing::error!(module = grant.module, error = %err, "an expiry's stop could not be recorded");
            return false;
        }
    }
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
        }
    }
    true
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

    /// K1-6a-G §2.5 (A5b): a grant is told to its module once it expires,
    /// with its tool and its expiry instant, and only once; an answer the
    /// module did not confirm is recorded and audited as an expiry.
    #[tokio::test]
    async fn an_expiring_grant_is_told_once() {
        let store = agent24_store::Store::open_memory().await.unwrap();
        let expires = (Utc::now() + chrono::Duration::milliseconds(300)).to_rfc3339();
        store
            .grant_module_consent(&summary("t"), &Utc::now().to_rfc3339(), &expires)
            .await
            .unwrap();
        let told = Arc::new(Mutex::new(Vec::<(String, String, String)>::new()));
        let notice: RevocationNotice = {
            let told = Arc::clone(&told);
            Arc::new(move |module: String, op: String, at: String| {
                told.lock().unwrap().push((module, op, at));
                Box::pin(async { RevocationStop::Unconfirmed })
                    as futures::future::BoxFuture<'static, _>
            })
        };
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run(store.clone(), notice, cancel.clone()));
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
            assert_eq!((told[0].0.as_str(), told[0].1.as_str()), ("m", "t"));
            assert_eq!(
                agent24_store::consent_instant_millis(&told[0].2),
                agent24_store::consent_instant_millis(&expires)
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
}
