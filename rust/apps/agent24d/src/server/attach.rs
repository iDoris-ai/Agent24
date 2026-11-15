use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// A3-2b / Codex A3 follow-up (design §5.5): fills `cell` with `registry`,
/// then immediately re-checks `cancel`, revoking every live generation if a
/// shutdown raced ahead of the fill.
///
/// `serve`'s `stopping` task checks `cell` (a `OnceLock<Arc<AttachRegistry>>`)
/// exactly ONCE, the instant shutdown is requested, and only calls
/// `revoke_all()` if it finds the cell already filled. If shutdown instead
/// races ahead of the fill — `stopping`'s single check runs, finds the cell
/// empty, and gives up — nothing else would ever call `revoke_all()` for
/// that daemon: the fill that follows a moment later would be the LAST thing
/// to ever touch the registry, leaving its live generations/tokens (none
/// yet, this early, but hydration just before the fill may already have
/// populated `entries` with disk-registered modules) "live" forever, and the
/// attach listener started just after would keep accepting/handshaking
/// connections for a daemon that is meant to be going away. Re-checking
/// `cancel` immediately after the fill catches exactly that ordering.
/// `revoke_all()` is idempotent (see its own doc), so calling it here is a
/// harmless no-op on the OTHER ordering, where `stopping`'s own check
/// already found the cell filled and revoked everything itself — between the
/// two checks, every interleaving of "shutdown requested" vs "cell filled"
/// is covered by exactly one of them.
///
/// A free function — not inlined into `serve` — so the two
/// `stopping`-task-races-the-fill regression tests in this file's `tests`
/// module call this SAME code, instead of re-describing the logic next to
/// it (a reverse mutation removing the re-check below must turn those tests
/// red; see their doc comments).
pub(super) fn fill_attach_registry_and_recheck(
    cell: &std::sync::OnceLock<Arc<agent24_attach::attach_registry::AttachRegistry>>,
    registry: &Arc<agent24_attach::attach_registry::AttachRegistry>,
    cancel: &CancellationToken,
) {
    let _ = cell.set(Arc::clone(registry));
    if cancel.is_cancelled() {
        registry.revoke_all();
    }
}

const APPROVAL_RECOVERY_SCAN_INTERVAL: Duration = Duration::from_secs(10);

pub(super) fn spawn_approval_recovery_scan(
    broker: Arc<agent24_policy::ApprovalBroker>,
    runs: Arc<agent24_agent::RunManager>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    spawn_approval_recovery_scan_every(broker, runs, APPROVAL_RECOVERY_SCAN_INTERVAL, cancel)
}

pub(super) fn spawn_approval_recovery_scan_every(
    broker: Arc<agent24_policy::ApprovalBroker>,
    runs: Arc<agent24_agent::RunManager>,
    interval: Duration,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(interval) => {}
            }
            let now = agent24_core::util::now_iso8601();
            match broker.timeout_expired(&now).await {
                Ok(expired) if expired > 0 => {
                    tracing::warn!("timed out {expired} expired approval(s)");
                }
                Ok(_) => {}
                Err(err) => {
                    tracing::error!("expired approval scan failed; will retry: {err}");
                }
            }
            match runs.recover_timed_out_approval_runs().await {
                Ok(cancelled) if cancelled > 0 => {
                    tracing::warn!(
                        "cancelled {cancelled} token-less run(s) after approval timeout"
                    );
                }
                Ok(_) => {}
                Err(err) => {
                    tracing::error!("timed-out run recovery scan failed; will retry: {err}");
                }
            }
        }
    })
}
