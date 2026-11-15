use std::sync::Arc;
use std::sync::Arc as StdArc;

use agent24_models::router::ModelRouter;
use tokio_util::sync::CancellationToken;

/// Build the D3 Guardian when the operator opts in with `A24_GUARDIAN=1`.
///
/// **Default OFF.** Letting a model auto-approve tool calls is a deliberate
/// operator choice, never a silent default — with no guardian every gated call
/// goes to a human exactly as before.
///
/// When on, risk is assessed by a LOCAL-ONLY model through the same [`ModelRouter`]
/// (the payload never leaves the device). `A24_GUARDIAN_ALWAYS_REVIEW` is a
/// comma-separated list of tool kinds that always require a human regardless of
/// the model's verdict; it defaults to `exec`, because `shell_exec` is arbitrary
/// code execution and deserves a human by default even with the guardian on.
pub(super) fn build_guardian(
    router: &Arc<ModelRouter>,
) -> Option<StdArc<agent24_policy::guardian::Guardian>> {
    if !guardian_enabled(std::env::var("A24_GUARDIAN").ok().as_deref()) {
        return None;
    }
    let always_review =
        parse_always_review(std::env::var("A24_GUARDIAN_ALWAYS_REVIEW").ok().as_deref());
    let assessor = StdArc::new(agent24_policy::guardian::ModelRiskAssessor::new(
        Arc::clone(router),
    ));
    tracing::info!(
        "guardian enabled (always-review kinds: {})",
        always_review.join(",")
    );
    Some(StdArc::new(
        agent24_policy::guardian::Guardian::new(assessor).always_review(always_review),
    ))
}

/// Opt-in only: absent, empty, or anything other than `1`/`true` leaves the
/// guardian OFF (fail-safe — a typo must never silently enable auto-approval).
pub(super) fn guardian_enabled(raw: Option<&str>) -> bool {
    raw.is_some_and(|v| {
        let v = v.trim();
        v == "1" || v.eq_ignore_ascii_case("true")
    })
}

/// Parse the always-review kind list, defaulting to `exec`. An explicitly empty
/// value yields an empty list (the operator deliberately allows every kind to be
/// considered for auto-approval).
pub(super) fn parse_always_review(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or("exec")
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The logical user a local daemon's memory belongs to.
///
/// Agent24 is one person's 24/7 agent, so there is exactly one — and inventing an
/// identity system to have a nicer-looking constant would be worse than naming
/// the assumption. It matters here only because it is one half of a module's
/// memory partition key, and because that key is VERSIONED: when real user
/// identity arrives it comes from the kernel, and the catalog in
/// [`crate::os_memory`] is what lets the existing partitions be reattributed
/// rather than guessed at.
pub(super) const LOCAL_USER: &str = "local";

/// Open the memory base — the KV store the D1 session memory and the domain-OS
/// memory partitions both live in. Returns `None` (memory off) if it cannot be
/// opened: a degraded daemon is better than one that won't start.
///
/// ONE store, handed to both. `KvStore` is `Clone` over a shared pool, so this is
/// one database and one connection pool rather than two competing for the same
/// file — and the two uses are separated by their owner keys, not by their
/// handles (see [`crate::os_memory`]).
pub(super) async fn open_memory_base(ephemeral: bool) -> Option<agent24_memory::KvStore> {
    let kv = if ephemeral {
        agent24_memory::KvStore::open_memory().await
    } else {
        let dir = agent24_protocol::state_file::state_dir()?;
        agent24_memory::KvStore::open(&dir.join("memory.db")).await
    };
    match kv {
        Ok(kv) => Some(kv),
        Err(err) => {
            tracing::warn!(
                "memory base unavailable ({err}); sessions will not remember and no \
                 domain OS will be lent memory"
            );
            None
        }
    }
}

/// Pair the memory base with a router-backed summarizer for D1 session memory.
/// The session owner is the durable personal partition recorded by the OS
/// memory catalog, so replay and module memory use the same org identity.
pub(super) async fn session_memory(
    kv: agent24_memory::KvStore,
    router: &Arc<ModelRouter>,
    shutdown: &CancellationToken,
) -> Result<agent24_agent::SessionMemory, String> {
    let org = crate::os_memory::OrgId::from_store(
        kv.ensure_org_for_user(LOCAL_USER)
            .await
            .map_err(|err| err.to_string())?,
    );
    let owner = crate::os_memory::OsMemoryCatalog::default()
        .ensure_personal_recorded(&org, LOCAL_USER, &kv)
        .await?;

    Ok(agent24_agent::SessionMemory::new(
        kv,
        StdArc::new(agent24_agent::RouterSummarizer::new(
            Arc::clone(router),
            shutdown.clone(),
        )),
    )
    .with_owner(owner))
}
