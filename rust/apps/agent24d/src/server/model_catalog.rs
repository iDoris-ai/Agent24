use std::sync::Arc;
use std::time::Duration;

use agent24_models::router::ModelRouter;
use tokio_util::sync::CancellationToken;

/// The model ids on offer at startup, for the mount-time resource check.
///
/// Three properties, each of which cost a real bug to learn:
///
/// - **Enumerated once**, not per module: `ModelRouter::models` queries every
///   configured provider over the network, so per-module would multiply startup
///   latency by the module count and make one slow provider look like a module
///   fault.
/// - **Enumerated only when needed.** Zero times is better than once. Sin90
///   declares no models at all today, so an unconditional probe is pure startup
///   cost — and it lands after MCP's own 10s budget, inside the CLI's 15s ready
///   deadline. [`Self::skipped`] is what a daemon with nothing to check uses.
/// - **Completeness is tracked**, not inferred from emptiness. A partial failure
///   (one provider answers, another is down) still returns a non-empty union, so
///   a model served by the DOWN provider would be reported missing. "Install this
///   model" and "start your provider" are not the same instruction, and only one
///   of them would be right.
pub(super) struct ModelCatalog(std::result::Result<Vec<String>, String>);

impl ModelCatalog {
    /// No admissible module declares any model, so nothing was asked.
    pub(super) fn skipped() -> Self {
        Self(Err(
            "no module declares a model, so no provider was queried".to_owned(),
        ))
    }

    /// Ask every provider, bounded. A provider that hangs must not push the
    /// daemon past the CLI's ready deadline — a timed-out probe is `Unknown`,
    /// which is exactly the honest answer.
    pub(super) async fn probe(router: &Arc<ModelRouter>, cancel: &CancellationToken) -> Self {
        const BUDGET: Duration = Duration::from_secs(3);
        let inv = match tokio::time::timeout(BUDGET, router.models_detailed(&cancel.child_token()))
            .await
        {
            Ok(inv) => inv,
            Err(_) => {
                return Self(Err(format!(
                    "model enumeration exceeded {}s",
                    BUDGET.as_secs()
                )));
            }
        };
        // INCOMPLETE, not empty, is the disqualifier. An empty list from providers
        // that all answered honestly means the models really are absent; a
        // non-empty list from a partial sweep proves nothing about what is missing.
        // A hand-built router with no providers is vacuously complete. Production
        // `from_env` reports an iDoris failure instead, so "nothing is available"
        // and "iDoris is not connected" stay different answers.
        if !inv.is_complete() {
            return Self(Err(format!(
                "{} provider(s) did not answer: {}",
                inv.failures.len(),
                inv.failures.join("; ")
            )));
        }
        Self(Ok(inv.models.into_iter().map(|m| m.id).collect()))
    }
}

impl crate::domain::ModelInventory for ModelCatalog {
    fn available(&self) -> std::result::Result<&[String], String> {
        match &self.0 {
            Ok(v) => Ok(v),
            Err(e) => Err(e.clone()),
        }
    }
}
