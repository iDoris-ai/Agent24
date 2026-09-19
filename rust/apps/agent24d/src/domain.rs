//! The kernel's domain-OS mounter (ME-1b; ADR-029).
//!
//! This is the half of the kernel↔domain-OS boundary that lives in the kernel:
//! it takes a CATALOGUE of [`Installed`] descriptors — names and builders, not
//! constructed modules — decides which of them should run, builds those, gives
//! each a directory and a [`KernelCtx`], and nests its routes under a namespace
//! derived from its identity. The mounting LOGIC has no module-specific branch — that is the ME-1
//! acceptance — and the tests below mount fake modules rather than Sin90, so the
//! property cannot quietly become "the mounter happens to work for Sin90". As of
//! ME-1b-b this file names no module at all: Sin90 mounts through exactly this
//! path, and the only place in the kernel that says "sin90" is `serve`, which has
//! to name the OS it installs. (Fake modules give regression evidence, not proof
//! that no special case exists.)
//!
//! Five rules the CONTRACT cannot enforce on its own, which therefore live here:
//!
//! 1. **Duplicate names are refused.** `DomainOsManifest::validate` checks ONE
//!    manifest for self-consistency; it cannot see the others. Two modules named
//!    `sin90` would collide on the directory, the route namespace AND the event
//!    module at once, so the name is reserved BEFORE the store is opened or any
//!    route is mounted — a later failure must not leave a half-mounted twin.
//! 2. **A module runs the way its entry says, and only that way.** A compiled-in
//!    entry ([`Build::InProcess`]) whose manifest declares an out-of-process
//!    provider is refused — once constructed; a package on disk
//!    ([`Build::Package`]) must declare one, and is started as its own process
//!    under a supervisor with the kernel's proxy in front (SUP-4). A DISABLED
//!    entry is never constructed or started; see the ordering note below.
//! 3. **A failed `open_store` degrades that module ONLY.** The kernel nests its
//!    OWN 503 router under the namespace rather than the module's — a module
//!    whose store is gone is exactly the one least able to answer correctly, and
//!    `open_store` returning `Err` cannot stop it from handing back handlers that
//!    answer 200.
//! 4. **Modules mount BEFORE kernel auth.** An axum layer applies only to routes
//!    already on the router, so nesting after `.layer(auth)` would leave every
//!    module route unauthenticated. [`mount_all`] returns a router for the CALLER
//!    to fold in before its auth layer, and
//!    [`crate::server::build_router_with_modules`] does exactly that;
//!    `module_routes_are_behind_kernel_auth` in `server.rs` is the regression,
//!    verified by mutation (swap the two lines and it goes 401 → 200).
//! 5. **A name that is already a kernel route segment is refused.** axum PANICS on
//!    an exact route overlap, so a module called `health` would kill the daemon at
//!    startup rather than lose a routing contest — see [`RESERVED_KERNEL_SEGMENTS`].
//!
//! The pass runs `identity → admission → registry policy → construction → mount`,
//! and the position of CONSTRUCTION is load-bearing: a module the user switched off
//! is never built, which is what lets them switch off one whose constructor is
//! breaking the daemon. The cost is that manifest-derived admission (the
//! out-of-process transport) cannot run for a module that was never constructed —
//! such an entry reports `Disabled`, which is true, and is refused the moment it is
//! enabled.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use agent24_domain::{Capability, DomainModule, EventBroadcast, EventSink, Grants, KernelCtx};
use agent24_protocol::EventBody;
use axum::Router;

/// What the kernel is willing to lend a domain OS today. A module may REQUEST
/// more in its manifest; [`Grants::granting`] intersects, so asking gains
/// nothing. The list grows as `KernelCtx` gains handles — `Models` and
/// `Scheduler` are deliberately absent because there is nothing to hand out
/// yet, and granting a capability with no handle would be a lie.
///
/// `Memory` joined the list in F1, when `KernelCtx::memory` gained a handle to
/// give: a module that asks for it gets a view of the shared memory base scoped
/// to ITS OWN partition (see [`crate::os_memory`]). Before that handle existed,
/// two domain OSes under one user shared that base.
///
/// `Approval` joined in T7b/ME-3e, when `KernelCtx::approval` gained a handle
/// (`ApprovalRequester`, backed by [`PolicyApprovalBackend`] below).
const KERNEL_GRANTS: &[Capability] =
    &[Capability::Events, Capability::Memory, Capability::Approval];

/// What an out-of-process module may be granted. Narrower than
/// [`KERNEL_GRANTS`] on purpose — see `docs/design/T7a-ME3e-grants-and-events.md`
/// §1 for why this list exists at all.
/// `Approval` joined in T7b/ME-3e, alongside the wire handlers in
/// `crate::approval_callback` that give it a real handler.
///
/// `Memory` joined in T8.5c-W-mount. Out-of-process access to the shared
/// memory base goes through the OOP wire surface (`_a24/memory/private/*`,
/// T8.5c-W-wire), gated by [`crate::os_memory::MemoryEntitlement`] rather
/// than by this list alone — a module may be in this grant set and still
/// receive `MemoryEntitlement::NONE` (its partition could not be recorded,
/// or the daemon has no connection-admission budget to offer — see
/// `MemoryLease::admission()`). This list only says the kernel is WILLING to
/// consider lending memory to an out-of-process module; whether it actually
/// can is decided per mount.
const KERNEL_OOP_GRANTS: &[Capability] =
    &[Capability::Events, Capability::Approval, Capability::Memory];

/// Names a module may not take, because the kernel already serves
/// `/api/v1/<segment>` and axum PANICS on an exact route overlap:
///
/// ```text
/// Overlapping method route. Handler for `GET /api/v1/health` already exists
/// ```
///
/// A module called `health` with a route at `/` would therefore kill the daemon
/// AT STARTUP — a third-party domain OS could brick the process by choosing a
/// name. Refusing it here turns that into one refused module and a running
/// daemon. (A FALLBACK-ONLY nested router does not shadow kernel routes —
/// verified: a degraded module named `runs` leaves `/api/v1/runs/{id}` answering
/// from the kernel. A module with an explicit STATIC route can still outrank a
/// dynamic kernel one, e.g. `/api/v1/runs/foo` over `/api/v1/runs/{id}`; the
/// reservation below is what prevents that, not axum's precedence rules.)
///
/// This list must track [`crate::server::build_router_with_modules`], and
/// `reserved_segments_match_the_kernel_routes_exactly` fails if it drifts EITHER
/// way — a missing entry reappears as a startup panic, a stale one silently
/// refuses a legitimate module. That test scans literal `/api/v1/<segment>`
/// strings in that function's source, so it catches how routes are written here
/// today; it is a heuristic, not a proof (see its own doc for what it cannot
/// see).
///
/// The structural alternative is to namespace domain OSes under `/api/v1/os/<name>`,
/// which makes the collision unrepresentable rather than checked. That is the
/// better shape, but the namespace is fixed by ADR-029 / SPEC-MD-ME §2, so it is
/// raised there rather than changed here.
const RESERVED_KERNEL_SEGMENTS: &[&str] = &[
    "approvals",
    "chat",
    "events",
    "health",
    // T7b/ME-3e: `/api/v1/module-approvals`.
    "module-approvals",
    "models",
    // ME-2b's own registry endpoint. A domain OS named `os` would collide with it
    // — and the set-equality test below is what forced this entry the moment the
    // route was added, rather than leaving it to be discovered by whoever shipped
    // that module.
    "os",
    "runs",
    "schedules",
    "sessions",
    "shutdown",
    "standing-grants",
    "tool-overrides",
    "tools",
    "usage",
];

/// Adapts the daemon's WS hub to the contract's transport. Only the kernel builds
/// one of these, which is what makes a module's sink reach real subscribers —
/// see the trust model in `agent24_domain`.
struct HubBroadcast(crate::events::EventsHub);

impl EventBroadcast for HubBroadcast {
    fn send(&self, body: EventBody) {
        self.0.broadcast(body);
    }
}

/// Adapts [`crate::module_approval_broker::ModuleApprovalBroker`] to the
/// contract's `ApprovalBackend` trait (T7b/ME-3e design doc, decision 6) —
/// the daemon-side counterpart of [`HubBroadcast`] above. Only the kernel
/// builds one of these, for a module it granted [`Capability::Approval`].
struct PolicyApprovalBackend(Arc<crate::module_approval_broker::ModuleApprovalBroker>);

impl agent24_domain::ApprovalBackend for PolicyApprovalBackend {
    fn submit<'a>(
        &'a self,
        module: &'a str,
        kind: agent24_protocol::ModuleApprovalKind,
        action: String,
        target: Option<String>,
        payload: serde_json::Value,
    ) -> agent24_domain::ApprovalFuture<
        'a,
        std::result::Result<
            agent24_protocol::ApprovalAnswer,
            agent24_protocol::ApprovalRequestError,
        >,
    > {
        Box::pin(async move {
            // `gate` runs through the SAME validation the wire handler uses
            // (`crate::approval_callback`, T7c/ME-3e design doc "闭集匹配") —
            // one free function, so the two paths cannot disagree about what
            // is in the closed set OR about how `target` gets canonicalized
            // (judgement 16). On success, `target` is REPLACED with the
            // canonicalized form — never the raw string the module passed —
            // exactly like the wire path does.
            let (action, target) = if kind == agent24_protocol::ModuleApprovalKind::Gate {
                let canonical = crate::module_approval_broker::validate_gate_action(
                    &action,
                    target.as_deref(),
                )?;
                (canonical.action, Some(canonical.target))
            } else {
                (action, target)
            };
            // No token to admit here (T7b design doc decision 6): an
            // in-process module IS the daemon, so there is no callback
            // channel/`Generation` to check against — `submit` composes the
            // idempotency lookup and the insert directly.
            self.0
                .submit(module, &mint_request_id(), kind, action, target, payload)
                .await
        })
    }

    fn status<'a>(
        &'a self,
        module: &'a str,
        approval_id: &'a str,
    ) -> agent24_domain::ApprovalFuture<
        'a,
        std::result::Result<
            agent24_protocol::ApprovalAnswer,
            agent24_protocol::ApprovalRequestError,
        >,
    > {
        Box::pin(self.0.status(module, approval_id))
    }
}

/// A fresh `request_id` for an in-process submission (T7b/ME-3e). Unlike the
/// out-of-process wire path, an in-process module has no proxied HTTP
/// request to correlate with — a Rust function call cannot lose its response
/// in transit the way a JSON-RPC round trip over a socket can, so there is no
/// retry scenario for `(module, request_id, kind)` idempotency to protect
/// against here. A fresh id per call is therefore correct: it simply means
/// the idempotency lookup this crate's `submit` performs will never find a
/// match for an in-process caller, which is the right behavior (every
/// in-process `submit` call is a distinct request).
fn mint_request_id() -> String {
    agent24_core::util::ulid()
}

/// What the kernel needs in order to lend a module a memory partition: the
/// authenticated user, and the base to lend from.
///
/// Bundled so `mount_all` cannot be handed one without the other, and so the
/// whole capability is `Option` at the call site — a daemon whose memory base
/// failed to open lends nothing rather than half a handle.
pub struct MemoryLease {
    pub user: String,
    /// The org the user is acting in, resolved ONCE at startup.
    ///
    /// Resolved rather than derived, and held rather than re-read: it is an
    /// input to every partition key, so a lease whose org could change between
    /// two mounts would hand two modules keys from different orgs in one run.
    pub org: crate::os_memory::OrgId,
    pub kv: agent24_memory::KvStore,
}

impl MemoryLease {
    /// Resolve the org for `user`, then bring any partition still on F1's key
    /// format onto its (org, space) identity.
    ///
    /// Returns `None` if the org cannot be resolved — a daemon that cannot say
    /// which org it is acting in lends no memory at all, rather than lending
    /// partitions under a guessed one. The re-key sweep is best-effort by
    /// contrast: it reports per-partition failures and leaves those rows on v1,
    /// which the catalog's UNIQUE `(org_id, space_id)` then turns into a refused
    /// capability for that module rather than a silently fresh partition.
    pub async fn open(user: &str, kv: agent24_memory::KvStore) -> Option<Self> {
        let org = match kv.ensure_org_for_user(user).await {
            Ok(id) => crate::os_memory::OrgId::from_store(id),
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "could not resolve the org for the authenticated user; withholding \
                     the memory capability from every module rather than keying \
                     partitions under a guessed org"
                );
                return None;
            }
        };
        match crate::os_memory::OsMemoryCatalog::migrate_legacy_partitions(&kv).await {
            Ok(0) => {}
            Ok(n) => tracing::info!("re-keyed {n} memory partition(s) from v1 onto (org, space)"),
            Err(e) => tracing::error!(error = %e, "the v1 partition sweep could not run"),
        }
        Some(Self {
            user: user.to_owned(),
            org,
            kv,
        })
    }
    /// T8.5c-W-mount decision 4: the daemon-level OOP connection-admission
    /// permit, or `None` if this lease's store has no headroom to offer one
    /// (the ephemeral `:memory:` pool — its one connection is already needed
    /// in-process, so `max_connections - 1 = 0` leaves nothing to lend; see
    /// `agent24_memory::KvStore::oop_admission`'s doc). Reads `self.kv`
    /// fresh every call rather than caching: there is then no second copy of
    /// this value that could ever disagree with what `self.kv` actually is.
    pub fn admission(&self) -> Option<Arc<tokio::sync::Semaphore>> {
        self.kv.oop_admission()
    }

    /// Ensure `manifest`'s partition is durably recorded — or NOTHING, if it
    /// could not be. Does not by itself confirm this run mounted it; the
    /// caller must call [`crate::os_memory::OsMemoryCatalog::mark_mounted`]
    /// once it has confirmed the mount actually succeeded (T8.5c-W-mount
    /// decision 5).
    ///
    /// The catalog write is a precondition, not bookkeeping done afterwards. A
    /// partition the kernel lends but never records is orphaned data: rows under
    /// a NUL-containing owner key that no later export, erase or key-version
    /// migration can attribute to a user or a module. Refusing the capability
    /// costs the module its memory for this run; lending it anyway costs the
    /// user their ability to find the data again.
    async fn lend(
        &self,
        manifest: &agent24_domain::DomainOsManifest,
        catalogue: &crate::os_memory::OsMemoryCatalog,
    ) -> Option<(
        Arc<crate::os_memory::OsScopedMemory>,
        crate::os_memory::OsMemoryPartition,
    )> {
        match catalogue
            .ensure_recorded(&self.org, &self.user, manifest, &self.kv)
            .await
        {
            Ok(partition) => Some((
                Arc::new(crate::os_memory::OsScopedMemory::new(&partition, &self.kv)),
                partition,
            )),
            Err(e) => {
                tracing::error!(
                    module = manifest.name(),
                    error = %e,
                    "could not record the module's memory partition; withholding the \
                     memory capability rather than creating rows nothing can attribute"
                );
                None
            }
        }
    }
}

/// What happened to one module, for logging and for `agent24 os` later. Kept
/// separate from the router because a mount that FAILED is still a fact the
/// operator needs, not something to swallow into a 503 nobody reads — and a
/// REFUSED module has no route at all, so a 503 could not carry it either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountReport {
    pub name: String,
    pub namespace: String,
    /// The module's version, always the CATALOGUE's copy — which exists before any
    /// module does. A constructed module whose manifest states a different version
    /// is refused rather than mounted, so for anything that IS mounted the two
    /// agree and this is not a second answer.
    ///
    /// Held here rather than in a name-keyed side table: two modules claiming one
    /// name would otherwise have the loser reported with the winner's version, and
    /// ME-3 makes that collision real.
    pub version: String,
    /// What the registry said about this module WHEN THE DAEMON STARTED.
    ///
    /// `agent24 os` needs "has the config changed since we mounted?", and that is
    /// not the same question as "is it running?". Comparing config-now against
    /// RUNNING conflates a pending toggle with a module that is enabled and simply
    /// unhealthy — the first is fixed by a restart, the second is not.
    ///
    /// `None` means the registry gave no usable answer at startup: unparseable, OR
    /// parseable but semantically rejected (an unknown-disabled entry). There is
    /// nothing to compare against, so the view reports a pending change only once
    /// the registry is USABLE again — a file that is still broken cannot be applied
    /// by restarting, and saying otherwise sent the user to restart into the same
    /// degradation.
    pub enabled_at_start: Option<bool>,
    pub outcome: MountOutcome,
    /// Capabilities a LIVE module holds: the intersection of what its manifest
    /// asked for and [`KERNEL_GRANTS`].
    ///
    /// Empty for every outcome other than [`MountOutcome::Mounted`], because no
    /// `KernelCtx` was handed over — for a disabled entry the manifest was never
    /// even read, and for one that failed to open its store the grants were
    /// computed but never given. Empty therefore means "holds nothing", which is
    /// true in every one of those cases.
    pub granted: Vec<String>,
    /// Whether the module's declared `requires_models` are actually there
    /// (ME-2's "缺资源明确报错").
    pub resources: ResourceStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountOutcome {
    /// Routes are live.
    Mounted,
    /// The user switched it off in `os.json`. Its namespace answers 503, NOT 404:
    /// "this is off" and "this does not exist" send an operator to different
    /// places, and only one of them is true.
    Disabled,
    /// Namespace serves 503, from the KERNEL rather than the module: its directory
    /// could not be prepared, its store failed to open, or the registry itself was
    /// unreadable so we refused to guess whether the user wanted it. The wire
    /// distinguishes the last case (`registry_invalid`) from the first two
    /// (`module_unavailable`), because they send an operator to different files.
    Degraded(String),
    /// Not mounted at all — NO routes, not even 503 ones: a duplicate name, a
    /// kernel-reserved name, or a manifest that disagrees with how its entry is
    /// provided (a compiled-in module declaring an out-of-process provider, a
    /// package on disk declaring an in-process crate).
    Refused(String),
}

/// Whether a module's declared resources are present.
///
/// Deliberately NOT a mount outcome. A module whose model is missing still
/// mounts: most of its surface works, the model matters only when a request
/// actually needs it, and refusing to mount would turn a partial limitation into
/// a total outage. The point is that the user can SEE it — in the startup log and
/// in `agent24 os` — instead of discovering it as a confusing failure later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceStatus {
    /// The module never got far enough to be worth checking — refused, disabled,
    /// or blocked by an unreadable registry. Distinct from [`Self::Satisfied`]
    /// because "we did not look" and "we looked and it was fine" are different
    /// claims, and only one of them should appear in `agent24 os`.
    NotChecked,
    /// Nothing declared, or everything declared is present.
    Satisfied,
    /// These declared models were not found.
    MissingModels(Vec<String>),
    /// The check could not run — typically no provider answered. **Not** the same
    /// as "missing": reporting an unreachable provider as a missing model would
    /// send the user hunting for a model they already have.
    Unknown(String),
}

/// Answers "is this model available?" for the mount-time resource check.
///
/// A trait rather than a direct `ModelRouter` call so the mounter stays testable
/// without a router, and — more importantly — so the ENUMERATION happens once,
/// outside the per-module loop. `ModelRouter::models` queries every provider over
/// the network; doing that per module would multiply startup latency by the
/// module count and make a slow provider look like a module problem.
pub trait ModelInventory: Send + Sync {
    /// Every model id currently on offer, or why that could not be determined.
    fn available(&self) -> std::result::Result<&[String], String>;
}

fn check_resources(inv: &dyn ModelInventory, required: &[String]) -> ResourceStatus {
    if required.is_empty() {
        return ResourceStatus::Satisfied;
    }
    match inv.available() {
        Err(why) => ResourceStatus::Unknown(why),
        Ok(have) => {
            let missing: Vec<String> = required
                .iter()
                .filter(|r| !have.iter().any(|h| h == *r))
                .cloned()
                .collect();
            if missing.is_empty() {
                ResourceStatus::Satisfied
            } else {
                ResourceStatus::MissingModels(missing)
            }
        }
    }
}

/// Create a module's directory, DECLINING an existing symlink at that path.
///
/// Declining means the module degrades to a 503 namespace
/// ([`MountOutcome::Degraded`]), not [`MountOutcome::Refused`] — its storage is
/// unavailable, which is the same class of problem as a store that will not open.
///
/// **This is a check, not a guarantee, and the difference matters.** It catches
/// the case that actually happens — `~/.agent24/os/cos72` symlinked at Sin90's
/// directory, so two domain OSes share one store — instead of silently
/// contaminating them. It does NOT make the path traversal
/// symlink-safe: an ancestor of `root` may be a link, and the check is inherently
/// TOCTOU-prone (the path can change between the `symlink_metadata` and the
/// `create_dir_all`). Real isolation needs `openat`-style directory handles, which
/// is tracked in `improvement/` rather than half-claimed here. The contract's
/// wording was corrected to match what this actually does.
///
/// `spawn_blocking` because this runs on a Tokio worker: startup is not hot, but a
/// module directory on a slow or network filesystem should not stall the runtime.
async fn prepare_dir(dir: &Path) -> std::result::Result<(), String> {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        match std::fs::symlink_metadata(&dir) {
            // A symlink at the module's own directory. Windows junctions are
            // name-surrogate reparse points and report as symlinks here, which is
            // the right answer — they redirect traversal exactly like unix links.
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!(
                    "module directory {} is a symlink; declining it rather than \
                     risk sharing another module's store",
                    dir.display()
                ));
            }
            // A real entry already there, or nothing there yet: `create_dir_all`
            // sorts both out (idempotent for a directory, an error for a file).
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            // Anything else — PermissionDenied, a broken mount — must NOT fall
            // through: continuing would silently SKIP the symlink check on exactly
            // the path we could not inspect.
            Err(e) => {
                return Err(format!(
                    "cannot inspect module directory {}: {e}",
                    dir.display()
                ));
            }
        }
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))
    })
    .await
    .unwrap_or_else(|e| Err(format!("directory preparation task failed: {e}")))
}

/// Nest the KERNEL's 503 under `namespace`, for a module that could not be
/// brought up.
///
/// `nest` covers the namespace root and its descendants but NOT the bare trailing
/// slash — matchit's `{*rest}` does not match an empty segment — so that gets its
/// A store or directory failure specifically — a registry failure uses
/// [`registry_invalid_namespace`], because the two send an operator to different
/// files. Both go through [`nest_503`], which is where the trailing-slash and
/// all-methods rules live so they cannot drift between the callers.
fn degraded_namespace(app: Router, namespace: &str, module: &str) -> Router {
    let m = module.to_owned();
    nest_503(app, namespace, move || {
        agent24_domain::http::module_unavailable(&m)
    })
}

/// The namespace response when the REGISTRY itself could not be read.
///
/// Its own code, not `module_unavailable`: nothing is wrong with the module, and
/// sending an operator to look at it would waste their time. The one thing a
/// client can act on here is that `os.json` is broken, so the wire says exactly
/// that — a message that only reached the log would be invisible to a CLI daemon
/// whose stderr is discarded.
fn registry_invalid_namespace(app: Router, namespace: &str, why: &str) -> Router {
    let why = why.to_owned();
    nest_503(app, namespace, move || {
        agent24_domain::http::error_response(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "registry_invalid",
            &format!("no domain OS is mounted: {why}"),
        )
    })
}

/// Nest a fixed 503 over a whole namespace.
///
/// `nest` covers the namespace root and its descendants but NOT the bare trailing
/// slash — matchit's `{*rest}` does not match an empty segment — so that gets its
/// own route, and `any` rather than `get` because a namespace that is down must
/// answer the same way to every method. One implementation for all three callers
/// (degraded, disabled, registry-invalid) so that rule cannot drift between them.
fn nest_503<F>(app: Router, namespace: &str, body: F) -> Router
where
    F: Fn() -> axum::response::Response + Clone + Send + Sync + 'static,
{
    let handler = move || {
        let body = body.clone();
        async move { body() }
    };
    app.nest(namespace, Router::new().fallback(handler.clone()))
        .route(&format!("{namespace}/"), axum::routing::any(handler))
}

/// Nest the "switched off" 503 under `namespace`.
///
/// A KERNEL response with its own code, not the module's and not
/// [`agent24_domain::http::module_unavailable`]: "you turned this off" and
/// "something broke" need different actions, and collapsing them sends an
/// operator hunting for a fault that does not exist. 503 rather than 404 because
/// the feature exists and the routes are real — it is simply not running.
///
/// It lives here rather than in the shared contract because it is kernel POLICY:
/// no module ever serves it, and it names the kernel's own `os.json`.
fn disabled_namespace(app: Router, namespace: &str, module: &str) -> Router {
    let m = module.to_owned();
    nest_503(app, namespace, move || {
        agent24_domain::http::error_response(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "module_disabled",
            &format!("the {m} module is disabled in os.json"),
        )
    })
}

/// One entry in what this build PROVIDES — named WITHOUT being constructed.
///
/// The identity has to exist before construction, or the kernel cannot answer
/// "what is installed?" for a module it decided not to build. That matters in
/// exactly the case where it matters most: a module whose constructor RETURNS AN
/// ERROR must still have a name for `agent24 os disable` to act on, and a module
/// that is switched off must appear in the list — one that only showed up once it
/// was already on could never be turned on.
///
/// A constructor that PANICS is not contained: it takes the daemon down before any
/// of this runs. Containing that needs a process boundary (ME-3), and claiming
/// otherwise here would be the kind of promise this contract keeps getting wrong.
pub struct Installed {
    pub name: String,
    pub version: String,
    /// How the module comes to exist. Acted on ONLY after IDENTITY admission
    /// (name validity, duplicates, kernel-reserved names) and registry policy
    /// have said it should run — so a switched-off or badly-named entry is never
    /// constructed, and never started.
    pub build: Build,
}

/// Where a module comes from.
pub enum Build {
    /// Compiled in: build it by calling this. Manifest-derived admission
    /// necessarily happens after, because the manifest does not exist until the
    /// module does.
    #[allow(clippy::type_complexity)]
    InProcess(Box<dyn Fn() -> std::result::Result<Arc<dyn DomainModule>, String> + Send + Sync>),
    /// A package found on disk, run as its own process (ME-3, SUP-4). Its
    /// manifest was read and validated at discovery.
    Package(Box<Package>),
}

/// A discovered package: what a supervisor needs to start it.
pub struct Package {
    pub manifest: agent24_domain::DomainOsManifest,
    /// Where the manifest was read from; the spawn command resolves under it.
    pub dir: std::path::PathBuf,
    /// `sha256:<hex>` of the manifest's bytes — what the module must report in
    /// its handshake.
    pub digest: String,
}

/// What the daemon lends every out-of-process module: where their callback
/// sockets live, how to start them, and how long to wait for them.
pub struct ProcessHost {
    pub callback_dir: Arc<agent24_os_proto::endpoint::CallbackDir>,
    pub trampoline: agent24_os_proto::launch::Trampoline,
    pub timings: agent24_os_proto::supervisor::Timings,
    /// Every module started, owned from the moment it starts — so a shutdown
    /// that begins while later packages are still being mounted stops the
    /// earlier ones too — and closed by the shutdown, after which nothing
    /// starts (review of SUP-4, rounds 2 and 3).
    pub supervisors: Arc<Supervisors>,
}

/// The daemon's supervised modules: a list the shutdown closes and takes.
/// Starting a module happens under its lock, so "has the shutdown begun?" and
/// "start and register it" are one step — no module starts after
/// [`Supervisors::close`], and none started before it escapes the list it
/// returns (review of SUP-4, round 3). A module stopped by `os disable` leaves
/// the list under the same lock, its stop kept for the shutdown to wait for
/// (SUP-5).
#[derive(Default)]
pub struct Supervisors(std::sync::Mutex<Registry>);

struct Registry {
    /// `None` once the shutdown has closed the list.
    running: Option<Vec<Supervised>>,
    /// The stops `os disable` began and the shutdown has not taken yet.
    disabling: Vec<Disabling>,
    /// Every module `os disable` has asked to stop since this daemon started
    /// — so the list and a later disable can see how far that has got.
    disabled: std::collections::HashMap<String, Disabled>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            running: Some(Vec::new()),
            disabling: Vec::new(),
            disabled: std::collections::HashMap::new(),
        }
    }
}

/// A module `os disable` asked to stop: its proxy slot, to see whether it
/// still admits requests, and its supervisor's status, to see whether the
/// stop failed.
#[derive(Clone)]
pub struct Disabled {
    pub current: Arc<agent24_os_proto::drain::Current>,
    pub status: tokio::sync::watch::Receiver<agent24_os_proto::supervisor::Status>,
}

/// A stop `os disable` began: which module, what is known about the stop
/// (taken before the stop was asked for — SHUT-1b), and the task that waits
/// for it.
pub struct Disabling {
    pub name: String,
    pub record: agent24_os_proto::stop_record::StopRecordHandle,
    pub task: tokio::task::JoinHandle<()>,
}

/// What [`Supervisors::close`] hands the shutdown: every module still running,
/// and every stop a disable began and had not finished, for it to wait for
/// (and to describe in its summary: a disable that finished before is only
/// history, told in the log when it ended).
#[derive(Default)]
pub struct Closed {
    pub running: Vec<Supervised>,
    pub disabling: Vec<Disabling>,
}

impl Closed {
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.running.is_empty() && self.disabling.is_empty()
    }
}

impl Supervisors {
    fn lock(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether the shutdown has closed the list.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.lock().running.is_none()
    }

    /// Run `start` and keep what it starts — unless the list is closed, in
    /// which case `start` does not run and this returns `None`. `start` runs
    /// under the lock and must not block (starting a supervisor only spawns
    /// its task).
    pub fn start_with<E>(
        &self,
        start: impl FnOnce() -> std::result::Result<Supervised, E>,
    ) -> Option<std::result::Result<(), E>> {
        let mut registry = self.lock();
        let list = registry.running.as_mut()?;
        Some(start().map(|s| list.push(s)))
    }

    /// The live status of each module in the list, by name — for `agent24 os
    /// list` (see `AppState::module_status`).
    #[must_use]
    pub fn statuses(
        &self,
    ) -> std::collections::HashMap<
        String,
        tokio::sync::watch::Receiver<agent24_os_proto::supervisor::Status>,
    > {
        self.lock()
            .running
            .iter()
            .flatten()
            .map(|s| (s.name.clone(), s.handle.subscribe()))
            .collect()
    }

    /// Stop one module while the daemon runs (SUP-5, hot disable): take it
    /// out of the list, ask its supervisor to drain for up to `drain` and
    /// stop, and keep that stop for the shutdown — which kills it once
    /// `cut_off` resolves, and waits for it. The stop is asked for before this
    /// returns (see [`SupervisorHandle::drain_and_stop`]), all under the lock
    /// [`Supervisors::close`] takes: a module is stopped by the shutdown or by
    /// the disable, never by both, and never by neither.
    ///
    /// The module's proxy slot, to see its generation refuse new work; `None`
    /// if nothing by that name is running (a compiled-in module, a package
    /// not started at mount, one already disabled) or the shutdown has closed
    /// the list, and so already owns it.
    ///
    /// [`SupervisorHandle::drain_and_stop`]: agent24_os_proto::supervisor::SupervisorHandle::drain_and_stop
    pub fn disable(
        &self,
        name: &str,
        drain: std::time::Duration,
        cut_off: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Option<Disabled> {
        let mut registry = self.lock();
        let list = registry.running.as_mut()?;
        let i = list.iter().position(|s| s.name == name)?;
        let Supervised {
            name,
            handle,
            current,
        } = list.swap_remove(i);
        let disabled = Disabled {
            current,
            status: handle.subscribe(),
        };
        registry.disabled.insert(name.clone(), disabled.clone());
        let record = handle.stop_record();
        let entry = name.clone();
        let stop = handle.drain_and_stop_unless(drain, cut_off);
        let task = tokio::spawn(async move {
            match stop.await {
                Ok(()) => tracing::info!("domain OS {name:?} disabled and stopped"),
                Err(e) => {
                    tracing::error!(
                        "domain OS {name:?} was disabled but did not stop cleanly: {e}"
                    );
                }
            }
        });
        registry.disabling.retain(|d| !d.task.is_finished());
        registry.disabling.push(Disabling {
            name: entry,
            record,
            task,
        });
        Some(disabled)
    }

    /// Whether `os disable` has asked to stop `name` since this daemon
    /// started — stopped, or still stopping.
    #[cfg(test)]
    pub fn is_disabled(&self, name: &str) -> bool {
        self.lock().disabled.contains_key(name)
    }

    /// A module an earlier `os disable` asked to stop.
    #[must_use]
    pub fn disabled_slot(&self, name: &str) -> Option<Disabled> {
        self.lock().disabled.get(name).cloned()
    }

    /// Close the list and take everything in it — and the stops disables
    /// began. Later starts and disables are refused.
    pub fn close(&self) -> Closed {
        let mut registry = self.lock();
        Closed {
            running: registry.running.take().unwrap_or_default(),
            disabling: std::mem::take(&mut registry.disabling)
                .into_iter()
                .filter(|d| !d.task.is_finished())
                .collect(),
        }
    }
}

/// A module started under a supervisor. The daemon stops each before it exits
/// (see `server::stop_supervisors`); dropping one kills its module without
/// grace.
pub struct Supervised {
    pub name: String,
    pub handle: agent24_os_proto::supervisor::SupervisorHandle,
    /// Its proxy slot: which generation the proxy sends requests to.
    pub current: Arc<agent24_os_proto::drain::Current>,
}

/// Mount everything in `catalogue` under `root`, returning the combined router
/// and one report per entry.
///
/// **One pass, over the CATALOGUE rather than over constructed modules.** An
/// earlier version split the two — the caller constructed the enabled ones, called
/// this, and appended reports for the rest — and that quietly broke every global
/// invariant this loop maintains: a skipped entry did not claim its name (so two
/// disabled twins both "succeeded" and their namespaces could collide), it did not
/// pass admission (so a disabled module named `health` was reported Disabled rather
/// than Refused), and it was absent from `provided`, so a legitimate
/// `sin90: false` looked like a typo for a module the build does not have. The
/// order below is the whole design:
///
/// ```text
/// identity → admission → registry policy → construction → mount
/// ```
///
/// The returned router is NOT authenticated — the caller must fold it into the
/// kernel router before applying the auth layer (rule 4 above). It is a
/// `Router<()>`: each module has already bound its own state. `registry` decides
/// which entries are active (ME-2); `inventory` answers the declared-resource
/// check once for the whole pass.
#[allow(clippy::too_many_arguments)]
pub async fn mount_all(
    catalogue: &[Installed],
    root: &Path,
    events: &crate::events::EventsHub,
    registry: std::result::Result<&crate::os_config::OsConfig, &str>,
    inventory: &dyn ModelInventory,
    memory: Option<&MemoryLease>,
    host: std::result::Result<&ProcessHost, &str>,
    // T7b/ME-3e: threaded to `mount_package` for the OOP `MethodsFor`
    // closure AND used here for the in-process `PolicyApprovalBackend` —
    // both need the SAME broker `AppState` holds, so `module-approval.*`
    // events land on the one WS hub clients are subscribed to.
    approval_broker: &Arc<crate::module_approval_broker::ModuleApprovalBroker>,
) -> (Router, Vec<MountReport>, crate::os_memory::OsMemoryCatalog) {
    let mut app = Router::new();
    let mut reports = Vec::new();
    // Recorded for every module that is HANDED a partition, so a future export or
    // erase path has an explicit list instead of prefix-matching storage keys.
    let mut partitions = crate::os_memory::OsMemoryCatalog::default();
    let mut claimed: BTreeSet<String> = BTreeSet::new();

    // A name in the file that no build provides is a typo, and the two halves of
    // that mistake are NOT equally dangerous.
    //
    // `sin09: {"enabled": false}` looks exactly like a working config and leaves
    // `sin90` RUNNING — a config mistake that silently keeps something on, which is
    // the same failure that justifies rejecting malformed JSON. So it is treated
    // the same way: every module degrades to a 503 naming the bad entry, rather
    // than a warning in a log the user is not reading. An unknown ENABLED entry is
    // harmless by comparison (it asks for something absent, and nothing happens),
    // so that stays a warning.
    //
    // `provided` is the CATALOGUE, not the constructed subset — otherwise
    // disabling a module would make its own entry look like a typo for something
    // this build does not have.
    let provided: BTreeSet<&str> = catalogue.iter().map(|e| e.name.as_str()).collect();
    let mut registry = registry;
    let unknown_disabled: Vec<String> = registry
        .into_iter()
        .flat_map(|c| c.unknown_disabled(&provided))
        .map(str::to_owned)
        .collect();
    let typo_reason;
    if !unknown_disabled.is_empty() {
        typo_reason = format!(
            "os.json disables {unknown_disabled:?}, which this build does not \
             provide — so the module you meant to switch off is still running. \
             Delete that entry, or set \"default\": \"disabled\" to use an allow-list"
        );
        tracing::error!("{typo_reason}");
        registry = Err(typo_reason.as_str());
    }
    for named in registry.into_iter().flat_map(|c| c.named()) {
        if !provided.contains(named) {
            tracing::warn!(
                "os.json names a domain OS {named:?} that this build does not \
                 provide; the entry has no effect (typo?)"
            );
        }
    }

    for entry in catalogue {
        let name = entry.name.clone();
        let version = entry.version.clone();
        let namespace = agent24_domain::DomainOsManifest::declared_namespace(&name);
        let enabled_at_start = registry.ok().map(|c| c.is_enabled(&name));

        let refuse = |why: String, reports: &mut Vec<MountReport>| {
            tracing::error!("domain OS {name:?} not mounted: {why}");
            reports.push(MountReport {
                name: name.clone(),
                namespace: namespace.clone(),
                version: version.clone(),
                enabled_at_start,
                outcome: MountOutcome::Refused(why),
                // Nothing was handed a `KernelCtx`, so it holds nothing. (For a
                // pre-construction refusal no manifest was read either; for a
                // post-construction one the grants were computable but never given.
                // Both hold nothing, which is what this field means.)
                granted: Vec::new(),
                resources: ResourceStatus::NotChecked,
            });
        };

        // IDENTITY. The catalogue names a module before any manifest exists, so the
        // name has to satisfy the SAME rule a manifest's would — it becomes a URL
        // segment and a directory either way, and an unvalidated one could mount a
        // namespace no manifest would ever be allowed to claim.
        if version.trim().is_empty() {
            refuse(
                "the catalogue entry has an empty version; a manifest could not \
                 declare one, and this identity is reported as if it had"
                    .to_owned(),
                &mut reports,
            );
            continue;
        }
        if !agent24_domain::is_valid_module_name(&name) {
            refuse(
                format!(
                    "the catalogue entry {name:?} is not a usable module name; it \
                     cannot be routed or given a directory"
                ),
                &mut reports,
            );
            continue;
        }
        // Claim the name before anything observable, so a duplicate can
        // never open a store, build routes or leave a namespace behind. The claim
        // is held for the whole pass even by a REFUSED entry: a name rejected once
        // stays rejected rather than being handed to the next asker. (The loop is
        // sequential — there is no race to lose; the point is ORDER, so that a
        // later rejection never has to undo work.)
        if !claimed.insert(name.clone()) {
            refuse(
                format!("another module already claims the name {name:?}"),
                &mut reports,
            );
            continue;
        }

        // ADMISSION, before policy. A name that could never be mounted is refused
        // whether or not the user switched it on, because "disabled" would
        // otherwise CONCEAL it: turning it back on would fail in a way the earlier
        // report gave no hint of. It also stops two truths at once — an entry named
        // `health` reported Disabled while `/api/v1/health` answered 200.
        if RESERVED_KERNEL_SEGMENTS.contains(&name.as_str()) {
            refuse(
                format!(
                    "the name {name:?} is a kernel route segment; mounting it would \
                     panic the daemon on an overlapping route"
                ),
                &mut reports,
            );
            continue;
        }

        // POLICY: an unreadable registry, then the user's switch. Neither
        // CONSTRUCTS anything — which is what lets a user switch off a module whose
        // constructor is what breaks the daemon.
        //
        // An unreadable `os.json` degrades every admissible entry rather than
        // mounting it. Falling back to defaults would mount something the user had
        // switched off; mounting nothing would answer 404, which reads as "this
        // feature is gone". A 503 naming the config is the only answer that is both
        // safe and legible.
        if let Err(why) = registry {
            let reason = format!("os.json could not be read ({why}); refusing to guess");
            tracing::error!("domain OS {name:?}: {reason}");
            app = registry_invalid_namespace(app, &namespace, why);
            reports.push(MountReport {
                name,
                namespace,
                version,
                enabled_at_start,
                outcome: MountOutcome::Degraded(reason),
                granted: Vec::new(),
                resources: ResourceStatus::NotChecked,
            });
            continue;
        }
        if !registry.is_ok_and(|c| c.is_enabled(&name)) {
            tracing::info!("domain OS {name:?} is disabled in os.json; {namespace}/* will 503");
            app = disabled_namespace(app, &namespace, &name);
            reports.push(MountReport {
                name,
                namespace,
                version,
                enabled_at_start,
                outcome: MountOutcome::Disabled,
                granted: Vec::new(),
                resources: ResourceStatus::NotChecked,
            });
            continue;
        }

        // A PACKAGE is started, not constructed: its own path from here.
        let build = match &entry.build {
            Build::Package(package) => {
                let (next, report) = mount_package(
                    app,
                    package,
                    MountTarget {
                        name,
                        namespace,
                        version,
                        enabled_at_start,
                    },
                    root,
                    events,
                    inventory,
                    memory,
                    &mut partitions,
                    host,
                    approval_broker,
                )
                .await;
                app = next;
                reports.push(report);
                continue;
            }
            Build::InProcess(build) => build,
        };

        // CONSTRUCTION. A failure here degrades this entry and nothing else — and
        // it still has a name and a namespace, so `agent24 os disable` can reach it.
        let module = match build() {
            Ok(m) => m,
            Err(why) => {
                let reason = format!("could not be constructed: {why}");
                tracing::error!("domain OS {name:?} {reason}");
                app = degraded_namespace(app, &namespace, &name);
                reports.push(MountReport {
                    name,
                    namespace,
                    version,
                    enabled_at_start,
                    outcome: MountOutcome::Degraded(reason),
                    granted: Vec::new(),
                    resources: ResourceStatus::NotChecked,
                });
                continue;
            }
        };

        let manifest = module.manifest();
        // The catalogue named it before it existed, so the two can disagree — and a
        // module whose manifest claims a DIFFERENT name would be routed under one
        // identity while emitting events under another.
        if manifest.name() != name {
            refuse(
                format!(
                    "the catalogue lists it as {name:?} but its manifest says {:?}",
                    manifest.name()
                ),
                &mut reports,
            );
            continue;
        }
        // The VERSION too: the catalogue's copy is what `agent24 os` reports for an
        // entry that was never constructed, so a mismatch would have the API state
        // one version while the running module is another.
        if manifest.version() != version {
            refuse(
                format!(
                    "the catalogue lists {name:?} at version {version:?} but its \
                     manifest says {:?}",
                    manifest.version()
                ),
                &mut reports,
            );
            continue;
        }
        // A compiled-in entry is mounted in process, so its manifest must say
        // so. Out-of-process modules come from packages (`Build::Package`).
        if !manifest.is_mountable_in_process() {
            refuse(
                "a compiled-in module's manifest declares an out-of-process \
                 provider; out-of-process modules are installed as packages"
                    .to_owned(),
                &mut reports,
            );
            continue;
        }

        let granted = Grants::granting(manifest.kernel_capabilities(), KERNEL_GRANTS);
        let granted_names: Vec<String> = granted.iter().map(|c| c.as_str().to_owned()).collect();

        // The module's directory is DERIVED from its validated name, never from
        // the string its manifest declared.
        let dir = manifest.data_dir_under(root);
        // Two distinct failures, reported distinctly: an unwritable or symlinked
        // module directory is NOT "the store failed to open", and telling an
        // operator the wrong one sends them to the wrong file.
        let opened = match prepare_dir(&dir).await {
            Ok(()) => module
                .open_store(&dir)
                .await
                .map_err(|e| format!("store failed to open: {e}")),
            Err(e) => Err(e),
        };

        if let Err(why) = opened {
            // Degrade THIS module: the kernel's own 503, not the module's.
            tracing::error!("domain OS {name:?} unavailable ({why}); {namespace}/* will 503");
            app = degraded_namespace(app, &namespace, &name);
            reports.push(MountReport {
                name,
                namespace,
                version,
                enabled_at_start,
                outcome: MountOutcome::Degraded(why),
                // Empty because no `KernelCtx` was ever handed over: these would
                // have been its grants, but it never got them. `granted` means what
                // a LIVE module holds, and nothing else.
                granted: Vec::new(),
                resources: ResourceStatus::NotChecked,
            });
            continue;
        }

        // Only NOW — an admitted, enabled module whose store actually opened — is
        // its declared-resource status a fact worth reporting. Checking earlier
        // meant a module that then failed to open still carried `MissingModels`,
        // and the startup log said "it is mounted" about something that was not.
        let resources = check_resources(inventory, manifest.requires_models());

        // The sink's EXISTENCE is the capability: build one only when events were
        // actually granted, and name it from the MANIFEST, never a local string.
        let sink = granted.has(Capability::Events).then(|| {
            EventSink::new(
                manifest,
                Arc::new(HubBroadcast(events.clone())) as Arc<dyn EventBroadcast>,
            )
        });
        // Same shape for memory: the HANDLE is the capability. A module that did
        // not ask for it, or that the kernel has no base to lend, gets `None` —
        // not an unusable object it has to remember to check.
        // Carries `lease` alongside the lend result (rather than
        // re-deriving it from `memory` afterwards) so the `mark_mounted`
        // call below needs no `Option::expect` — `scoped_lend` being `Some`
        // is then a type-level guarantee that a lease is right there with it.
        let scoped_lend = match (granted.has(Capability::Memory), memory) {
            (true, Some(lease)) => lease
                .lend(manifest, &partitions)
                .await
                .map(|(s, p)| (s, p, lease)),
            _ => None,
        };
        let scoped = scoped_lend.as_ref().map(|(s, ..)| s.clone());
        // Same shape again for approval (T7b/ME-3e): the requester's
        // EXISTENCE is the capability, built only when granted, and it uses
        // the SAME `ModuleApprovalBroker` as every other mount path (proxy
        // wire, REST) — one table, one set of WS pushes.
        let approval = granted.has(Capability::Approval).then(|| {
            agent24_domain::ApprovalRequester::new(
                manifest,
                Arc::new(PolicyApprovalBackend(approval_broker.clone()))
                    as Arc<dyn agent24_domain::ApprovalBackend>,
            )
        });
        // `granted` must name what the module ACTUALLY holds, not what policy
        // would have allowed — the invariant #134 established for every other
        // capability. A lease refused because its partition could not be recorded
        // is a withheld capability, so it must not still be reported as granted.
        let granted_names: Vec<String> = granted_names
            .into_iter()
            .filter(|c| c != Capability::Memory.as_str() || scoped.is_some())
            .collect();
        let ctx: Arc<dyn KernelCtx> = Arc::new(crate::os_memory::MemoryCtx {
            sink,
            memory: scoped,
            approval,
        });

        tracing::info!("domain OS {name:?} mounted at {namespace} (grants: {granted_names:?})");
        app = app.nest(&namespace, module.routes(ctx));
        // T8.5c-W-mount decision 5 (H1): only mark this partition "mounted"
        // now that the route is actually nested — not right after `lend()`
        // succeeded, which is what let a mount that failed AFTER `lend()`
        // still get recorded as "just active" in the durable catalog.
        if let Some((_, partition, lease)) = scoped_lend {
            partitions
                .mark_mounted(partition, &lease.kv, &agent24_memory::SystemClock)
                .await;
        }
        reports.push(MountReport {
            name,
            namespace,
            version,
            enabled_at_start,
            outcome: MountOutcome::Mounted,
            granted: granted_names,
            resources,
        });
    }

    (app, reports, partitions)
}

/// The identity `mount_all` already admitted, for the entry being mounted.
struct MountTarget {
    name: String,
    namespace: String,
    version: String,
    enabled_at_start: Option<bool>,
}

/// Mount one admitted, enabled package: start it under a supervisor — kept in
/// `host.supervisors` — and put the kernel's proxy in front of it. Returns the
/// router and the report.
///
/// The same outcomes as a compiled-in module, for the same reasons: a manifest
/// that disagrees with its entry is Refused (no routes); a directory that cannot
/// be prepared, or a daemon that cannot start processes at all, is Degraded
/// (the kernel's 503). A package that is started is Mounted from the kernel's
/// side — its namespace answers `503 module_not_ready` until the module's
/// handshake, and after a crash while it restarts; the supervisor's status says
/// which.
#[allow(clippy::too_many_arguments)]
async fn mount_package(
    app: Router,
    package: &Package,
    target: MountTarget,
    root: &Path,
    events: &crate::events::EventsHub,
    inventory: &dyn ModelInventory,
    memory: Option<&MemoryLease>,
    partitions: &mut crate::os_memory::OsMemoryCatalog,
    host: std::result::Result<&ProcessHost, &str>,
    approval_broker: &Arc<crate::module_approval_broker::ModuleApprovalBroker>,
) -> (Router, MountReport) {
    let MountTarget {
        name,
        namespace,
        version,
        enabled_at_start,
    } = target;
    // Unchanged (Codex round 2 High 2): `Refused`/`Degraded` never had a
    // `KernelCtx` handed over, so `granted` stays `Vec::new()` for both — only
    // the `Mounted` branch, built separately below, carries `granted_names`.
    let report = |outcome: MountOutcome, resources: ResourceStatus| MountReport {
        name: name.clone(),
        namespace: namespace.clone(),
        version: version.clone(),
        enabled_at_start,
        outcome,
        granted: Vec::new(),
        resources,
    };
    let manifest = &package.manifest;
    let refused = |why: String| {
        tracing::error!("domain OS {name:?} not mounted: {why}");
        report(MountOutcome::Refused(why), ResourceStatus::NotChecked)
    };
    // The catalogue entry was made from this manifest, so these agree unless
    // the entry was built by hand — checked anyway, as for a compiled-in module.
    if manifest.name() != name || manifest.version() != version {
        return (
            app,
            refused(format!(
                "the catalogue lists {name:?} v{version} but its manifest says {:?} v{}",
                manifest.name(),
                manifest.version()
            )),
        );
    }
    let Some(command) = manifest
        .spawn()
        .filter(|_| !manifest.is_mountable_in_process())
    else {
        return (
            app,
            refused(
                "a package on disk must declare an out-of-process provider with a \
                 spawn command; in-process modules are compiled in"
                    .to_owned(),
            ),
        );
    };
    let degraded = |app: Router, why: String| {
        tracing::error!("domain OS {name:?} unavailable ({why}); {namespace}/* will 503");
        (
            degraded_namespace(app, &namespace, &name),
            report(MountOutcome::Degraded(why), ResourceStatus::NotChecked),
        )
    };
    let host = match host {
        Ok(h) => h,
        Err(why) => {
            return degraded(
                app,
                format!("this daemon cannot start out-of-process modules: {why}"),
            );
        }
    };
    const SHUTTING_DOWN: &str = "the daemon is shutting down";
    if host.supervisors.is_closed() {
        return degraded(app, SHUTTING_DOWN.to_owned());
    }
    let data_dir = manifest.data_dir_under(root);
    if let Err(why) = prepare_dir(&data_dir).await {
        return degraded(app, why);
    }
    let resources = check_resources(inventory, manifest.requires_models());

    // T7a/ME-3e: computed once, before `supervise` — `Offer` and the
    // `MethodsFor` closure both need it, and it must be the SAME `Grants` that
    // ends up in `MountReport.granted` below (judgement 10's consistency).
    let granted = Grants::granting(manifest.kernel_capabilities(), KERNEL_OOP_GRANTS);
    let broadcast: Arc<dyn EventBroadcast> = Arc::new(HubBroadcast(events.clone()));
    let event_sink = granted
        .has(Capability::Events)
        .then(|| Arc::new(EventSink::new(manifest, broadcast)));
    // T8.5c-W-mount decision 4: only try to lend when this lease's store has
    // OOP admission budget to offer (`admission()` is `None` for the
    // ephemeral pool) — two different reasons to end up with nothing
    // (`memory` is `None`, or it is `Some` but has no admission), same
    // observable result either way: `memory_lend` is `None`.
    // Carries `lease` alongside the result (rather than re-deriving it from
    // `memory` where `mark_mounted` is called below) so that call needs no
    // `Option::expect` — `memory_lend` being `Some` is a type-level
    // guarantee that a lease is right there with it.
    let memory_lend: Option<(
        Arc<crate::os_memory::OsScopedMemory>,
        crate::os_memory::OsMemoryPartition,
        Arc<tokio::sync::Semaphore>,
        &MemoryLease,
    )> = if granted.has(Capability::Memory) {
        match memory.and_then(|lease| lease.admission().map(|admission| (lease, admission))) {
            Some((lease, admission)) => lease
                .lend(manifest, partitions)
                .await
                .map(|(scoped, partition)| (scoped, partition, admission, lease)),
            None => None,
        }
    } else {
        None
    };
    let memory_entitlement = crate::os_memory::build_private_memory_entitlement(
        memory_lend
            .as_ref()
            .map(|(scoped, _partition, admission, _lease)| (scoped.clone(), admission.clone())),
    );
    let memory_grant = crate::os_memory::memory_grant_name(&memory_entitlement);
    // `granted` must name what the module ACTUALLY holds (invariant #134),
    // so a `granted.has(Capability::Memory)` that did not turn into a real
    // handle (`lend()` failed, or ephemeral has no admission) must not
    // appear here — the same rule the in-process path already applies via
    // `scoped.is_some()`.
    let granted_names: Vec<String> = granted
        .iter()
        .map(|c| c.as_str().to_owned())
        .filter(|c| c != Capability::Memory.as_str() || memory_grant.is_some())
        .collect();
    // T7b/ME-3e: additive, not if/else (design doc §"现状" 1) — a module
    // granted ONLY `approval` (not `events`) must still get a non-empty
    // `Offer`, which an if/else between the two prefixes could never produce.
    let mut provides = Vec::new();
    if granted.has(Capability::Events) {
        provides.push("_a24/events/".to_owned());
    }
    if granted.has(Capability::Approval) {
        provides.push("_a24/approval/".to_owned());
    }
    if memory_grant.is_some() {
        provides.push("_a24/memory/private/".to_owned());
    }
    let offer = agent24_os_proto::initialize::Offer { provides };
    let methods_for: agent24_os_proto::supervisor::MethodsFor = {
        let name = name.clone();
        let granted = granted.clone();
        let event_sink = event_sink.clone();
        let approval_broker = approval_broker.clone();
        Arc::new(
            move |generation: &Arc<agent24_os_proto::drain::Generation>| {
                // A fresh bucket every time this closure runs — once per
                // generation, i.e. once per (re)start. Building it outside the
                // closure and cloning the `Arc` in would let a restarted module
                // inherit whatever quota the previous generation had already
                // spent (Codex round 3 Medium 2).
                let limiter = Arc::new(crate::events_emit::RateLimiter::new(
                    crate::events_emit::EVENTS_RATE_CAPACITY,
                    crate::events_emit::EVENTS_RATE_REFILL_PER_SEC,
                ));
                // T7b/ME-3e: the three approval methods are registered
                // UNCONDITIONALLY, exactly like `_a24/events/emit` above —
                // capability gating happens INSIDE each handler's `call()`,
                // not by conditionally registering the method (design doc
                // decision 4).
                agent24_os_proto::rpc::Methods::none()
                    .with(
                        "_a24/events/emit",
                        Arc::new(crate::events_emit::EventsEmitHandler {
                            generation: generation.clone(),
                            name: name.clone(),
                            granted: granted.clone(),
                            sink: event_sink.clone(),
                            limiter,
                        }),
                    )
                    .with(
                        "_a24/approval/gate",
                        Arc::new(crate::approval_callback::ApprovalSubmitHandler {
                            generation: generation.clone(),
                            module: name.clone(),
                            granted: granted.clone(),
                            kind: agent24_protocol::ModuleApprovalKind::Gate,
                            broker: approval_broker.clone(),
                        }),
                    )
                    .with(
                        "_a24/approval/advise",
                        Arc::new(crate::approval_callback::ApprovalSubmitHandler {
                            generation: generation.clone(),
                            module: name.clone(),
                            granted: granted.clone(),
                            kind: agent24_protocol::ModuleApprovalKind::Advise,
                            broker: approval_broker.clone(),
                        }),
                    )
                    .with(
                        "_a24/approval/status",
                        Arc::new(crate::approval_callback::ApprovalStatusHandler {
                            module: name.clone(),
                            granted: granted.clone(),
                            broker: approval_broker.clone(),
                        }),
                    )
            },
        )
    };

    let current =
        agent24_os_proto::drain::Current::new(agent24_os_proto::drain::Generation::starting());
    let spec = agent24_os_proto::supervisor::ModuleSpec {
        name: name.clone(),
        command: command.clone(),
        package_dir: package.dir.clone(),
        data_dir,
        manifest_digest: package.digest.clone(),
        trampoline: host.trampoline.clone(),
    };
    // FU-61: re-checked right before every restart, not just at this mount —
    // `agent24-os-proto` stays unaware of `agent24-os-packages`'s on-disk
    // format (`ModuleSpec` already treats `package_dir`/`manifest_digest` as
    // opaque values), so the check itself lives here and travels in as an
    // opaque closure.
    let package_check: agent24_os_proto::supervisor::PackageCheck = Arc::new(|spec| {
        agent24_os_packages::discovery::recheck(&spec.package_dir, &spec.manifest_digest).err()
    });
    // Started and registered in one step, unless the shutdown has closed the
    // list — which it may have while the directory was being prepared.
    let started = host.supervisors.start_with(|| {
        agent24_os_proto::supervisor::supervise(
            spec,
            host.callback_dir.clone(),
            current.clone(),
            methods_for,
            offer,
            host.timings,
            package_check,
        )
        .map(|handle| Supervised {
            name: name.clone(),
            handle,
            current: current.clone(),
        })
    });
    match started {
        None => return degraded(app, SHUTTING_DOWN.to_owned()),
        // A fresh slot, held by nobody: not expected. Still not a mounted module.
        Some(Err(held)) => return degraded(app, held.to_string()),
        Some(Ok(())) => {}
    }
    // T8.5c-W-mount decision 5 (H1): only now — supervisor slot occupied,
    // child task spawned — is this partition really "mounted" in the sense
    // that lets the durable catalog advance `last_seen_at`. The two failure
    // branches above return before reaching here, so a `lend()` that
    // succeeded but whose module then failed to start leaves the durable
    // identity row recorded (from `ensure_recorded`) but NOT marked active.
    if let Some((_, partition, _, lease)) = memory_lend {
        partitions
            .mark_mounted(partition, &lease.kv, &agent24_memory::SystemClock)
            .await;
    }
    tracing::info!(
        "domain OS {name:?} started from {} and proxied at {namespace} (grants: {granted_names:?})",
        package.dir.display()
    );
    let app = agent24_os_proto::proxy::mount(app, &namespace, current);
    // Only THIS branch — a module actually started — carries `granted_names`;
    // `report`'s own default (used by `refused`/`degraded` above) stays
    // `Vec::new()` (Codex round 2 High 2).
    (
        app,
        MountReport {
            name,
            namespace,
            version,
            enabled_at_start,
            outcome: MountOutcome::Mounted,
            granted: granted_names,
            resources,
        },
    )
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_domain::{DomainOsManifest, Result as DomainResult};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// Defaults for the ME-2 knobs, so the mount tests keep testing MOUNTING.
    /// The registry and resource cases have their own tests below.
    fn all_enabled() -> crate::os_config::OsConfig {
        crate::os_config::OsConfig::default()
    }

    /// T7b/ME-3e: a throwaway broker for tests that don't care about approval
    /// behavior — built on the SAME hub the test passes as `mount_all`'s
    /// `events`, so a test that DOES look at broadcast events sees module
    /// approval ones on the same receiver.
    async fn test_approval_broker(
        hub: &crate::events::EventsHub,
    ) -> Arc<crate::module_approval_broker::ModuleApprovalBroker> {
        crate::module_approval_broker::ModuleApprovalBroker::new(
            agent24_store::Store::open_memory().await.unwrap(),
            hub.clone(),
        )
    }

    /// A model catalogue under the test's control.
    struct TestModels(std::result::Result<Vec<String>, String>);
    impl ModelInventory for TestModels {
        fn available(&self) -> std::result::Result<&[String], String> {
            match &self.0 {
                Ok(v) => Ok(v),
                Err(e) => Err(e.clone()),
            }
        }
    }
    fn no_models() -> TestModels {
        TestModels(Ok(Vec::new()))
    }

    /// A catalogue entry that hands back an already-built fake. Construction is
    /// what the mounter now controls, so tests that are about MOUNTING supply a
    /// builder that always succeeds; the construction-failure cases build their own.
    fn entry<M: DomainModule + 'static>(m: Arc<M>) -> Installed {
        let name = m.manifest().name().to_owned();
        let version = m.manifest().version().to_owned();
        Installed {
            name,
            version,
            build: Build::InProcess(Box::new(move || Ok(m.clone() as Arc<dyn DomainModule>))),
        }
    }

    /// A catalogue entry whose builder FAILS — the case that used to vanish from
    /// the reports entirely.
    fn broken_entry(name: &str, why: &str) -> Installed {
        let why = why.to_owned();
        Installed {
            name: name.to_owned(),
            version: "0.0.0".to_owned(),
            build: Build::InProcess(Box::new(move || Err(why.clone()))),
        }
    }

    /// `mount_all` with the ME-2 knobs defaulted.
    async fn mount(
        catalogue: &[Installed],
        root: &Path,
        hub: &crate::events::EventsHub,
    ) -> (Router, Vec<MountReport>) {
        let (app, reports, _) = mount_all(
            catalogue,
            root,
            hub,
            Ok(&all_enabled()),
            &no_models(),
            None,
            Err("no process host in this test"),
            &test_approval_broker(hub).await,
        )
        .await;
        (app, reports)
    }

    fn manifest_yaml(name: &str, kind: &str) -> String {
        // ME-3b-3: an out-of-process module must declare how to start it, and an
        // in-process one must not. Both halves are refused at parse time, so the
        // fixture cannot simply always include a spawn block.
        let spawn = if kind == "out_of_process_provider" {
            "spawn:\n  command: bin/mod\n"
        } else {
            ""
        };
        format!(
            "name: {name}\nversion: \"0.1.0\"\nroute_namespace: /api/v1/{name}\n\
             event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
             kernel_capabilities: [events]\nimpl_kind: {kind}\n{spawn}"
        )
    }

    /// A stand-in domain OS, deliberately NOT Sin90: if these tests used the real
    /// module, a mounter that special-cased Sin90 would pass them all. Using a name
    /// the production code has never seen turns "the mounter has no module-specific
    /// branch" into something a test can regress on — evidence, not proof.
    struct FakeModule {
        manifest: DomainOsManifest,
        fail_open: bool,
        opened_in: std::sync::Mutex<Option<std::path::PathBuf>>,
        /// The context the mounter handed over, so a test can check WHAT was lent
        /// rather than only that mounting succeeded.
        ctx: std::sync::Mutex<Option<Arc<dyn KernelCtx>>>,
        /// Whether the mounter ever ASKED this module for routes. A module that
        /// failed to open must never be asked — otherwise "the kernel serves the
        /// 503" would be indistinguishable from "the module's routes happen not to
        /// be reachable".
        routes_built: std::sync::atomic::AtomicUsize,
    }

    impl FakeModule {
        fn new(name: &str) -> Arc<Self> {
            Self::with(name, "in_process_crate", false)
        }
        fn with(name: &str, kind: &str, fail_open: bool) -> Arc<Self> {
            Self::from_yaml(&manifest_yaml(name, kind), fail_open)
        }
        fn from_yaml(yaml: &str, fail_open: bool) -> Arc<Self> {
            Arc::new(Self {
                manifest: DomainOsManifest::from_yaml(yaml).unwrap(),
                fail_open,
                opened_in: std::sync::Mutex::new(None),
                ctx: std::sync::Mutex::new(None),
                routes_built: std::sync::atomic::AtomicUsize::new(0),
            })
        }
        fn routes_built(&self) -> usize {
            self.routes_built.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn opened(&self) -> Option<std::path::PathBuf> {
            self.opened_in.lock().unwrap().clone()
        }
        fn ctx(&self) -> Option<Arc<dyn KernelCtx>> {
            self.ctx.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl DomainModule for FakeModule {
        fn manifest(&self) -> &DomainOsManifest {
            &self.manifest
        }
        async fn open_store(&self, dir: &Path) -> DomainResult<()> {
            *self.opened_in.lock().unwrap() = Some(dir.to_path_buf());
            if self.fail_open {
                return Err(agent24_domain::DomainError::Store("boom".into()));
            }
            Ok(())
        }
        fn routes(&self, ctx: Arc<dyn KernelCtx>) -> Router {
            self.routes_built
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *self.ctx.lock().unwrap() = Some(ctx.clone());
            let name = self.manifest.name().to_owned();
            Router::new().route(
                "/ping",
                axum::routing::get(move || {
                    let has_events = ctx.events().is_some();
                    let module = ctx.events().map(|s| s.module().to_owned());
                    // Emit through the sink so the test can observe attribution.
                    if let Some(sink) = ctx.events() {
                        let _ = sink.emit("ping.served", serde_json::Map::new());
                    }
                    async move {
                        axum::Json(serde_json::json!({
                            "name": name, "events": has_events, "sink_module": module
                        }))
                    }
                }),
            )
        }
    }

    async fn body_json(r: axum::response::Response) -> serde_json::Value {
        let b = axum::body::to_bytes(r.into_body(), 64 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&b).unwrap()
    }

    async fn get(app: &Router, uri: &str) -> axum::response::Response {
        app.clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_module_mounts_under_the_namespace_its_manifest_derives() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let m = FakeModule::new("zzquux");
        let (app, reports) = mount(&[entry(m.clone())], tmp.path(), &hub).await;

        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].outcome, MountOutcome::Mounted);
        assert_eq!(reports[0].namespace, "/api/v1/zzquux");

        // Nested under the namespace the MANIFEST derives, not one the kernel spells.
        let r = get(&app, "/api/v1/zzquux/ping").await;
        assert_eq!(r.status(), StatusCode::OK);
        let j = body_json(r).await;
        assert_eq!(j["name"], "zzquux");
        // The store was opened in the name-derived directory.
        assert_eq!(m.opened().unwrap(), tmp.path().join("zzquux"));
    }

    #[tokio::test]
    async fn each_module_gets_its_own_namespace_directory_and_sink_name() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let a = FakeModule::new("aaa");
        let b = FakeModule::new("bbb");
        let (app, reports) = mount(&[entry(a.clone()), entry(b.clone())], tmp.path(), &hub).await;
        assert!(reports.iter().all(|r| r.outcome == MountOutcome::Mounted));

        assert_eq!(
            body_json(get(&app, "/api/v1/aaa/ping").await).await["sink_module"],
            "aaa"
        );
        assert_eq!(
            body_json(get(&app, "/api/v1/bbb/ping").await).await["sink_module"],
            "bbb"
        );
        assert_ne!(
            a.opened(),
            b.opened(),
            "two modules must never share a store directory"
        );
        // An unknown path inside a mounted namespace is a plain 404 — the module
        // supplied no fallback, so nothing swallows it. (This does NOT show that
        // one module cannot answer on the other's namespace; the distinct
        // `sink_module` values above are what show they stayed separate.)
        assert_eq!(
            get(&app, "/api/v1/aaa/nope").await.status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn a_duplicate_name_is_refused_without_touching_its_store() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let first = FakeModule::new("dup");
        let second = FakeModule::new("dup");
        let (app, reports) = mount(
            &[entry(first.clone()), entry(second.clone())],
            tmp.path(),
            &hub,
        )
        .await;

        assert_eq!(reports[0].outcome, MountOutcome::Mounted);
        assert!(matches!(reports[1].outcome, MountOutcome::Refused(_)));
        // Reserved BEFORE anything module-side runs: the loser was never asked to
        // open a store OR to build routes. (It cannot prove no mkdir happened —
        // the WINNER creates that same directory — which is why the assertion is
        // about the module's own entry points, not the filesystem.)
        assert!(
            second.opened().is_none(),
            "a refused module must not have opened a store"
        );
        assert_eq!(
            second.routes_built(),
            0,
            "a refused module must never be asked for routes"
        );
        // The namespace still serves the FIRST module.
        assert_eq!(get(&app, "/api/v1/dup/ping").await.status(), StatusCode::OK);
    }

    // ── SUP-4: packages on disk, started under a supervisor ──────────────

    /// A package module in Python: it serves HTTP on the listener the kernel
    /// hands it (fd `A24_LISTEN_FD`), computes its manifest digest the way
    /// SPEC §3 says — `sha256:` + hex of the manifest file's bytes — reports it
    /// in `initialize`, and exits when its callback connection ends (D1).
    const PACKAGE_MODULE: &str = r#"import hashlib, json, os, socket, threading
name = os.environ["A24_MODULE_NAME"]
with open("domain-os.yml", "rb") as f:
    digest = "sha256:" + hashlib.sha256(f.read()).hexdigest()
listener = socket.socket(fileno=int(os.environ["A24_LISTEN_FD"]))
def serve():
    while True:
        conn, _ = listener.accept()
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = conn.recv(4096)
            if not chunk:
                break
            head += chunk
        body = ("hello from " + name).encode()
        conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: %d\r\n\r\n%s" % (len(body), body))
        conn.close()
threading.Thread(target=serve, daemon=True).start()
cb = socket.socket(socket.AF_UNIX)
cb.connect(os.environ["A24_CALLBACK_SOCK"])
req = {"jsonrpc": "2.0", "id": "1", "method": "initialize", "params": {
    "protocol_versions": {"min": 1, "max": 1000}, "module": name,
    "manifest_digest": digest, "auth_token": os.environ["A24_HANDSHAKE_TOKEN"],
    "capabilities": []}}
cb.sendall((json.dumps(req) + "\n").encode())
f = cb.makefile("rb")
f.readline()
while f.readline():
    pass
"#;

    /// T7a/ME-3e (judgement 10): a package module that, after handshaking,
    /// reads back whether its `initialize` result's `offer` covers
    /// `_a24/events/`, then makes ONE real `_a24/events/emit` call over the
    /// callback socket and records the raw JSON-RPC response — so the Rust
    /// test can assert on the real wire behaviour rather than a Rust-side
    /// simulation of it. No HTTP listener is served; this probe is only
    /// about the callback channel.
    const EVENTS_PROBE_MODULE: &str = r#"import hashlib, json, os, socket, threading
name = os.environ["A24_MODULE_NAME"]
with open("domain-os.yml", "rb") as f:
    digest = "sha256:" + hashlib.sha256(f.read()).hexdigest()
listener = socket.socket(fileno=int(os.environ["A24_LISTEN_FD"]))
def serve():
    while True:
        conn, _ = listener.accept()
        conn.close()
threading.Thread(target=serve, daemon=True).start()
cb = socket.socket(socket.AF_UNIX)
cb.connect(os.environ["A24_CALLBACK_SOCK"])
req = {"jsonrpc": "2.0", "id": "1", "method": "initialize", "params": {
    "protocol_versions": {"min": 1, "max": 1000}, "module": name,
    "manifest_digest": digest, "auth_token": os.environ["A24_HANDSHAKE_TOKEN"],
    "capabilities": []}}
cb.sendall((json.dumps(req) + "\n").encode())
f = cb.makefile("rb")
init_resp = json.loads(f.readline())
provides = init_resp.get("result", {}).get("offer", {}).get("provides", [])
offers_events = any("_a24/events/emit".startswith(p) for p in provides)
emit_req = {"jsonrpc": "2.0", "id": "2", "method": "_a24/events/emit",
            "params": {"kind": "task.transitioned", "payload": {"x": 1}}}
cb.sendall((json.dumps(emit_req) + "\n").encode())
emit_resp = json.loads(f.readline())
with open("probe.json", "w") as out:
    json.dump({"offers_events": offers_events, "emit_response": emit_resp}, out)
while f.readline():
    pass
"#;

    /// Write a package for `name` under `packages`: a manifest whose spawn
    /// command runs [`PACKAGE_MODULE`]. The module learns its name from an
    /// argument-free environment, so it is written into the script.
    fn write_package(packages: &Path, name: &str) {
        write_package_with(packages, name, "[]", PACKAGE_MODULE);
    }

    /// Like [`write_package`], but with a caller-chosen `kernel_capabilities`
    /// YAML list and script — for T7a/ME-3e tests that need a module to
    /// declare `events` (or an unrecognised capability).
    fn write_package_with(packages: &Path, name: &str, kernel_capabilities: &str, script: &str) {
        let dir = packages.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("domain-os.yml"),
            format!(
                "name: {name}\nversion: \"0.1.0\"\nroute_namespace: /api/v1/{name}\n\
                 event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
                 kernel_capabilities: {kernel_capabilities}\nimpl_kind: out_of_process_provider\n\
                 spawn:\n  command: python3\n  args: [\"-I\", \"-S\", \"mod.py\"]\n"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("mod.py"),
            script.replace("os.environ[\"A24_MODULE_NAME\"]", &format!("{name:?}")),
        )
        .unwrap();
    }

    /// The catalogue entries `serve` would make of `packages`.
    fn discovered(packages: &Path) -> Vec<Installed> {
        agent24_os_packages::discovery::scan(packages)
            .found
            .into_iter()
            .map(|d| Installed {
                name: d.manifest.name().to_owned(),
                version: d.manifest.version().to_owned(),
                build: Build::Package(Box::new(Package {
                    manifest: d.manifest,
                    dir: d.dir,
                    digest: d.digest,
                })),
            })
            .collect()
    }

    /// A process host for tests: a callback directory under a short path, and
    /// a shell trampoline that execs the module (the daemon binary is the
    /// real one, and is not available to a unit test).
    fn test_host(tmp: &Path) -> ProcessHost {
        use std::os::unix::fs::PermissionsExt;
        let trampoline = tmp.join("trampoline.sh");
        std::fs::write(
            &trampoline,
            "#!/bin/sh\nwhile [ \"$1\" != \"--a24-exec-module\" ]; do shift; done\nshift\nexec \"$@\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&trampoline, std::fs::Permissions::from_mode(0o755)).unwrap();
        ProcessHost {
            callback_dir: Arc::new(agent24_os_proto::endpoint::CallbackDir::create(tmp).unwrap()),
            trampoline: agent24_os_proto::launch::Trampoline {
                program: trampoline,
                args: Vec::new(),
            },
            timings: agent24_os_proto::supervisor::Timings {
                stop_grace: std::time::Duration::from_secs(1),
                ..agent24_os_proto::supervisor::Timings::default()
            },
            supervisors: Arc::new(Supervisors::default()),
        }
    }

    /// SUP-4, the real path: a package found on disk is started under a
    /// supervisor, completes its handshake — with the digest of its own
    /// manifest — and answers through the kernel's proxy at its namespace; its
    /// data directory is prepared; and it stops cleanly. (The successor of the
    /// old tripwire: out-of-process modules now mount. The CLI's "applies at
    /// the next daemon start" still holds — a toggle still acts only at start.)
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_package_is_started_and_proxied() {
        let tmp = tempfile::Builder::new()
            .prefix("a24")
            .tempdir_in("/tmp")
            .unwrap();
        let packages = tmp.path().join("packages");
        write_package(&packages, "remote");
        let host = test_host(tmp.path());
        let hub = crate::events::EventsHub::default();
        let root = tmp.path().join("os");
        let (app, reports, _) = mount_all(
            &discovered(&packages),
            &root,
            &hub,
            Ok(&all_enabled()),
            &no_models(),
            None,
            Ok(&host),
            &test_approval_broker(&hub).await,
        )
        .await;
        assert_eq!(
            reports[0].outcome,
            MountOutcome::Mounted,
            "{:?}",
            reports[0]
        );
        assert!(
            root.join("remote").is_dir(),
            "its data directory was not prepared"
        );
        assert_eq!(
            host.supervisors.lock().running.as_ref().map(Vec::len),
            Some(1)
        );

        // `module_not_ready` until the handshake, then the module's own answer.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let body = loop {
            let res = get(&app, "/api/v1/remote/hi").await;
            if res.status() == StatusCode::OK {
                let bytes = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
                break String::from_utf8_lossy(&bytes).into_owned();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the module never became ready"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        assert_eq!(body, "hello from remote");

        for s in host.supervisors.close().running {
            s.handle.stop().await.expect("a clean stop");
        }
    }

    /// Once the daemon's shutdown has begun — a SIGTERM during startup — no
    /// further package is started: each reports why, and nothing is left for
    /// the shutdown to stop (review of SUP-4, round 2).
    #[tokio::test]
    async fn no_package_is_started_once_shutdown_has_begun() {
        let tmp = tempfile::Builder::new()
            .prefix("a24")
            .tempdir_in("/tmp")
            .unwrap();
        let packages = tmp.path().join("packages");
        write_package(&packages, "remote");
        let host = test_host(tmp.path());
        assert!(host.supervisors.close().is_empty());
        let hub = crate::events::EventsHub::default();
        let (_, reports, _) = mount_all(
            &discovered(&packages),
            &tmp.path().join("os"),
            &hub,
            Ok(&all_enabled()),
            &no_models(),
            None,
            Ok(&host),
            &test_approval_broker(&hub).await,
        )
        .await;
        match &reports[0].outcome {
            MountOutcome::Degraded(why) => assert!(why.contains("shutting down"), "{why}"),
            other => panic!("expected Degraded, got {other:?}"),
        }
        assert!(
            host.supervisors.close().is_empty(),
            "a package was started during the shutdown"
        );
    }

    // ── T7a/ME-3e: out-of-process capability grants + `_a24/events/emit` ──

    /// Judgement 9: `Capability::parse` already rejects an unrecognised
    /// capability name at manifest-parse time — confirmed here at the NEW
    /// consumption point (an out-of-process package's manifest), not because
    /// the behaviour is new, but because `mount_package` never used to read
    /// `kernel_capabilities` at all, so nothing had exercised this combination
    /// before. There is no "partially granted" outcome: the whole module is
    /// refused before it ever reaches the catalogue.
    #[test]
    fn a_package_declaring_an_unknown_capability_never_reaches_the_catalogue() {
        let tmp = tempfile::tempdir().unwrap();
        let packages = tmp.path().join("packages");
        write_package_with(&packages, "typo", "[events, telepthy]", PACKAGE_MODULE);
        let scan = agent24_os_packages::discovery::scan(&packages);
        assert!(
            scan.found.is_empty(),
            "a manifest with an unknown capability must not be mountable"
        );
        assert_eq!(scan.refused.len(), 1);
        assert!(
            scan.refused[0].why.contains("telepthy"),
            "{}",
            scan.refused[0].why
        );
    }

    /// T7b/ME-3e (judgement 17): like [`EVENTS_PROBE_MODULE`], but for
    /// `_a24/approval/status` — a QUERY method that needs no live
    /// `request_id`/`approval_token` pair (unlike `gate`/`advise`, which are
    /// tied to a proxied HTTP request this probe never makes; see the design
    /// doc "现状" 2 on why `status` is independent of that machinery). Good
    /// enough to prove offer/grant/real-call agree on `approval` without
    /// building a probe that also serves HTTP.
    const APPROVAL_PROBE_MODULE: &str = r#"import hashlib, json, os, socket, threading
name = os.environ["A24_MODULE_NAME"]
with open("domain-os.yml", "rb") as f:
    digest = "sha256:" + hashlib.sha256(f.read()).hexdigest()
listener = socket.socket(fileno=int(os.environ["A24_LISTEN_FD"]))
def serve():
    while True:
        conn, _ = listener.accept()
        conn.close()
threading.Thread(target=serve, daemon=True).start()
cb = socket.socket(socket.AF_UNIX)
cb.connect(os.environ["A24_CALLBACK_SOCK"])
req = {"jsonrpc": "2.0", "id": "1", "method": "initialize", "params": {
    "protocol_versions": {"min": 1, "max": 1000}, "module": name,
    "manifest_digest": digest, "auth_token": os.environ["A24_HANDSHAKE_TOKEN"],
    "capabilities": []}}
cb.sendall((json.dumps(req) + "\n").encode())
f = cb.makefile("rb")
init_resp = json.loads(f.readline())
provides = init_resp.get("result", {}).get("offer", {}).get("provides", [])
offers_approval = any("_a24/approval/status".startswith(p) for p in provides)
status_req = {"jsonrpc": "2.0", "id": "2", "method": "_a24/approval/status",
              "params": {"approval_id": "does-not-exist"}}
cb.sendall((json.dumps(status_req) + "\n").encode())
status_resp = json.loads(f.readline())
with open("probe.json", "w") as out:
    json.dump({"offers_approval": offers_approval, "status_response": status_resp}, out)
while f.readline():
    pass
"#;

    /// T7b/ME-3e (judgement 17): the additive `Offer` rule specifically — a
    /// module granted ONLY `approval` (not `events`) must still see
    /// `_a24/approval/` in its offer. An if/else between the two prefixes
    /// (what T7a shipped) could never produce this: it would pick one
    /// prefix or the other, never both independently.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn approval_only_grant_still_offers_the_approval_prefix_and_the_real_call_agrees() {
        for (name, capabilities, expect_granted) in
            [("granted", "[approval]", true), ("ungranted", "[]", false)]
        {
            let tmp = tempfile::Builder::new()
                .prefix("a24")
                .tempdir_in("/tmp")
                .unwrap();
            let packages = tmp.path().join("packages");
            write_package_with(&packages, name, capabilities, APPROVAL_PROBE_MODULE);
            let host = test_host(tmp.path());
            let hub = crate::events::EventsHub::default();
            let (_, reports, _) = mount_all(
                &discovered(&packages),
                &tmp.path().join("os"),
                &hub,
                Ok(&all_enabled()),
                &no_models(),
                None,
                Ok(&host),
                &test_approval_broker(&hub).await,
            )
            .await;
            assert_eq!(
                reports[0].outcome,
                MountOutcome::Mounted,
                "{:?}",
                reports[0]
            );
            assert_eq!(
                reports[0].granted,
                if expect_granted {
                    vec!["approval".to_owned()]
                } else {
                    Vec::new()
                },
                "MountReport.granted for {name:?} — not `events`, proving Offer is additive \
                 by capability, not by an if/else"
            );

            let probe_path = packages.join(name).join("probe.json");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            let probe: serde_json::Value = loop {
                if let Ok(bytes) = std::fs::read(&probe_path) {
                    break serde_json::from_slice(&bytes).unwrap();
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the module never wrote its probe"
                );
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            };

            assert_eq!(
                probe["offers_approval"].as_bool(),
                Some(expect_granted),
                "the real handshake's offer for {name:?} must include `_a24/approval/` \
                 with NO `events` grant at all"
            );
            let status_response = &probe["status_response"];
            if expect_granted {
                assert_eq!(
                    status_response["error"]["data"]["kind"].as_str(),
                    Some("not_found"),
                    "a granted module's real status call on an unknown id: {status_response}"
                );
            } else {
                assert_eq!(
                    status_response["error"]["data"]["kind"].as_str(),
                    Some("forbidden"),
                    "an ungranted module's real call: {status_response}"
                );
            }

            for s in host.supervisors.close().running {
                s.handle.stop().await.expect("a clean stop");
            }
        }
    }

    /// Judgement 10, the three-way consistency check, over a REAL package
    /// process and a REAL `initialize` handshake (not a Rust-side simulation
    /// of the wire): a module granted `events` gets `MountReport.granted ==
    /// ["events"]`, its handshake's `offer.provides("_a24/events/emit")` is
    /// true, and a real `_a24/events/emit` call over its callback socket
    /// succeeds with `{}`. A module granted nothing gets the negative of all
    /// three: `granted == []`, no offer, and the same call comes back
    /// `forbidden` — proving the method exists (it is NOT `-32601`) but this
    /// module cannot use it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn granted_and_offer_and_the_real_call_agree_for_both_outcomes() {
        for (name, capabilities, expect_granted) in
            [("granted", "[events]", true), ("ungranted", "[]", false)]
        {
            let tmp = tempfile::Builder::new()
                .prefix("a24")
                .tempdir_in("/tmp")
                .unwrap();
            let packages = tmp.path().join("packages");
            write_package_with(&packages, name, capabilities, EVENTS_PROBE_MODULE);
            let host = test_host(tmp.path());
            let hub = crate::events::EventsHub::default();
            let mut events = hub.subscribe();
            let (_, reports, _) = mount_all(
                &discovered(&packages),
                &tmp.path().join("os"),
                &hub,
                Ok(&all_enabled()),
                &no_models(),
                None,
                Ok(&host),
                &test_approval_broker(&hub).await,
            )
            .await;
            assert_eq!(
                reports[0].outcome,
                MountOutcome::Mounted,
                "{:?}",
                reports[0]
            );
            assert_eq!(
                reports[0].granted,
                if expect_granted {
                    vec!["events".to_owned()]
                } else {
                    Vec::new()
                },
                "MountReport.granted for {name:?}"
            );

            let probe_path = packages.join(name).join("probe.json");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            let probe: serde_json::Value = loop {
                if let Ok(bytes) = std::fs::read(&probe_path) {
                    break serde_json::from_slice(&bytes).unwrap();
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the module never wrote its probe"
                );
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            };

            assert_eq!(
                probe["offers_events"].as_bool(),
                Some(expect_granted),
                "the real handshake's offer for {name:?}"
            );
            let emit_response = &probe["emit_response"];
            if expect_granted {
                assert_eq!(
                    emit_response["result"],
                    serde_json::json!({}),
                    "a granted module's real call must succeed with exactly {{}}: {emit_response}"
                );
                let (_, body) =
                    tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
                        .await
                        .expect("the kernel's WS hub must see the event")
                        .unwrap();
                match body {
                    agent24_protocol::EventBody::Module(m) => {
                        assert_eq!(m.module, name);
                        assert_eq!(m.kind, "task.transitioned");
                    }
                    other => panic!("expected a module event, got {other:?}"),
                }
            } else {
                assert_eq!(
                    emit_response["error"]["data"]["kind"].as_str(),
                    Some("forbidden"),
                    "an ungranted module's real call: {emit_response}"
                );
            }

            for s in host.supervisors.close().running {
                s.handle.stop().await.expect("a clean stop");
            }
        }
    }

    /// A full `AppState` with one package, `remote`, started and Running —
    /// for tests that need a real HTTP route (`stop_now_os`, `patch_os`)
    /// rather than just `Supervisors` directly. Builds on `running_package`'s
    /// same recipe but also wires `os_reports`/`module_status`/`supervisors`
    /// into the state the same way `server::run` does after its own mount
    /// pass (`server.rs`, around `state.os_reports = Arc::new(reports)`).
    pub(crate) async fn running_state(tmp: &Path) -> crate::server::AppState {
        let packages = tmp.join("packages");
        write_package(&packages, "remote");
        let host = test_host(tmp);
        let hub = crate::events::EventsHub::default();
        let (_, reports, _) = mount_all(
            &discovered(&packages),
            &tmp.join("os"),
            &hub,
            Ok(&all_enabled()),
            &no_models(),
            None,
            Ok(&host),
            &test_approval_broker(&hub).await,
        )
        .await;
        let mut status = host.supervisors.statuses().remove("remote").unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            status.wait_for(|s| *s == agent24_os_proto::supervisor::Status::Running),
        )
        .await
        .expect("the package never ran")
        .unwrap();
        let mut state = crate::server::tests::state().await;
        state.module_status = Arc::new(host.supervisors.statuses());
        state.os_reports = Arc::new(reports);
        state.supervisors = Some(host.supervisors.clone());
        state
    }

    /// Like [`write_package`], but the module crashes right after its
    /// handshake instead of serving — for FU-61's real-`recheck` test, which
    /// needs an actual restart cycle. Computes its digest from its own
    /// `domain-os.yml` (CWD is the package directory), the same way
    /// [`PACKAGE_MODULE`] does, so the very first run's handshake succeeds
    /// against whatever the daemon's `recheck`-backed check expects.
    fn write_crashing_package(packages: &Path, name: &str) {
        let dir = packages.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("domain-os.yml"),
            format!(
                "name: {name}\nversion: \"0.1.0\"\nroute_namespace: /api/v1/{name}\n\
                 event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
                 kernel_capabilities: []\nimpl_kind: out_of_process_provider\n\
                 spawn:\n  command: python3\n  args: [\"-I\", \"-S\", \"mod.py\"]\n"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("mod.py"),
            format!(
                r#"import hashlib, json, os, socket
name = {name:?}
with open("domain-os.yml", "rb") as f:
    digest = "sha256:" + hashlib.sha256(f.read()).hexdigest()
cb = socket.socket(socket.AF_UNIX)
cb.connect(os.environ["A24_CALLBACK_SOCK"])
req = {{"jsonrpc": "2.0", "id": "1", "method": "initialize", "params": {{
    "protocol_versions": {{"min": 1, "max": 1000}}, "module": name,
    "manifest_digest": digest, "auth_token": os.environ["A24_HANDSHAKE_TOKEN"],
    "capabilities": []}}}}
cb.sendall((json.dumps(req) + "\n").encode())
f = cb.makefile("rb")
f.readline()
raise SystemExit(3)
"#
            ),
        )
        .unwrap();
    }

    /// FU-61: the REAL `discovery::recheck`-backed `PackageCheck` built at
    /// `mount()` (not a synthetic one, unlike `agent24-os-proto`'s own unit
    /// tests) genuinely stops a crash-looping module once its manifest
    /// changes during backoff — and does so without disturbing an unrelated
    /// module mounted alongside it (round-1 code-review Medium 3: the
    /// closure construction at `mount()` and the injection point in
    /// `run_loop` had never been exercised together end to end).
    #[tokio::test]
    async fn a_manifest_change_during_backoff_reports_package_changed_via_the_real_check() {
        let tmp = tempfile::Builder::new()
            .prefix("a24")
            .tempdir_in("/tmp")
            .unwrap();
        let packages = tmp.path().join("packages");
        write_crashing_package(&packages, "crashy");
        write_package(&packages, "remote");
        // A generous backoff so the test can safely act inside the window —
        // the same style already used by `agent24-os-proto`'s own timing
        // tests for the same reason.
        let mut host = test_host(tmp.path());
        host.timings.backoff_base = std::time::Duration::from_secs(3);
        let hub = crate::events::EventsHub::default();
        let (_, reports, _) = mount_all(
            &discovered(&packages),
            &tmp.path().join("os"),
            &hub,
            Ok(&all_enabled()),
            &no_models(),
            None,
            Ok(&host),
            &test_approval_broker(&hub).await,
        )
        .await;

        let mut crashy_status = host.supervisors.statuses().remove("crashy").unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            crashy_status.wait_for(|s| {
                matches!(
                    s,
                    agent24_os_proto::supervisor::Status::Backoff { failures: 1, .. }
                )
            }),
        )
        .await
        .expect("the module never crashed into backoff")
        .unwrap();

        // Change the manifest during the backoff window — the package
        // directory itself stays, so an unguarded restart would spawn
        // successfully and only fail much later, at the handshake.
        std::fs::write(
            packages.join("crashy").join("domain-os.yml"),
            "name: crashy\nversion: \"0.2.0\"\nroute_namespace: /api/v1/crashy\n\
             event_module: crashy\ndata_dir: ~/.agent24/os/crashy/\n\
             kernel_capabilities: []\nimpl_kind: out_of_process_provider\n\
             spawn:\n  command: python3\n  args: [\"-I\", \"-S\", \"mod.py\"]\n",
        )
        .unwrap();

        let status = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            crashy_status.wait_for(|s| {
                matches!(
                    s,
                    agent24_os_proto::supervisor::Status::PackageChanged { .. }
                        | agent24_os_proto::supervisor::Status::Backoff { failures: 2, .. }
                )
            }),
        )
        .await
        .expect("never settled")
        .unwrap()
        .clone();
        let agent24_os_proto::supervisor::Status::PackageChanged { reason } = &status else {
            panic!("restarted against the replaced manifest instead of stopping: {status:?}");
        };
        assert!(reason.contains("manifest changed"), "{reason}");
        // The rendered hint text (naming the one recovery that actually
        // works) is pinned separately, at the unit level, by
        // `os_routes::tests::a_package_changed_module_is_told_to_restart_
        // the_daemon_not_disable_enable` — this test's job is only to prove
        // the REAL `recheck`-backed closure reaches `PackageChanged` at all.
        assert!(reports.iter().any(|r| r.name == "crashy"));

        // The unrelated module mounted alongside it is completely unaffected
        // — this test's closure never touches `os.json`, so there is
        // nothing here that COULD degrade `remote`, but the assertion is
        // cheap insurance against a future change that routes `recheck`
        // failures through shared state.
        let mut remote_status = host.supervisors.statuses().remove("remote").unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            remote_status.wait_for(|s| *s == agent24_os_proto::supervisor::Status::Running),
        )
        .await
        .expect("the unrelated module never ran")
        .unwrap();

        for s in host.supervisors.close().running {
            s.handle.stop().await.expect("a clean stop");
        }
    }

    /// A host with one package, `remote`, started and Running.
    pub(crate) async fn running_package(tmp: &Path) -> ProcessHost {
        let packages = tmp.join("packages");
        write_package(&packages, "remote");
        let host = test_host(tmp);
        let hub = crate::events::EventsHub::default();
        let _ = mount_all(
            &discovered(&packages),
            &tmp.join("os"),
            &hub,
            Ok(&all_enabled()),
            &no_models(),
            None,
            Ok(&host),
            &test_approval_broker(&hub).await,
        )
        .await;
        let mut status = host.supervisors.statuses().remove("remote").unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            status.wait_for(|s| *s == agent24_os_proto::supervisor::Status::Running),
        )
        .await
        .expect("the package never ran")
        .unwrap();
        host
    }

    /// A disable and the shutdown racing for the same module, on two threads
    /// at once: whichever wins, the module is handed to the shutdown exactly
    /// once — as a module to stop, or as a disable's stop to wait for — never
    /// both and never neither (SUP-5).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_disable_racing_the_shutdown_hands_the_module_over_exactly_once() {
        for _ in 0..5 {
            let tmp = tempfile::Builder::new()
                .prefix("a24")
                .tempdir_in("/tmp")
                .unwrap();
            let host = Arc::new(running_package(tmp.path()).await);
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let disable = {
                let (host, barrier) = (host.clone(), barrier.clone());
                let rt = tokio::runtime::Handle::current();
                std::thread::spawn(move || {
                    let _rt = rt.enter();
                    barrier.wait();
                    host.supervisors
                        .disable(
                            "remote",
                            std::time::Duration::from_secs(10),
                            std::future::pending(),
                        )
                        .is_some()
                })
            };
            let close = {
                let (host, barrier) = (host.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    host.supervisors.close()
                })
            };
            let disabled = disable.join().unwrap();
            let closed = close.join().unwrap();
            assert_eq!(
                closed.running.len() + closed.disabling.len(),
                1,
                "handed over twice or not at all"
            );
            assert_eq!(disabled, closed.disabling.len() == 1);
            for s in closed.running {
                s.handle.stop().await.expect("a clean stop");
            }
            for stop in closed.disabling {
                stop.task.await.unwrap();
            }
        }
    }

    /// `disable` takes one module out of the list and stops it — once — and
    /// hands that stop to the shutdown rather than the module itself, so
    /// the shutdown waits for it and does not stop it twice (SUP-5).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_module_is_stopped_by_the_disable_or_the_shutdown_never_both() {
        let tmp = tempfile::Builder::new()
            .prefix("a24")
            .tempdir_in("/tmp")
            .unwrap();
        let packages = tmp.path().join("packages");
        write_package(&packages, "remote");
        let host = test_host(tmp.path());
        let hub = crate::events::EventsHub::default();
        let _ = mount_all(
            &discovered(&packages),
            &tmp.path().join("os"),
            &hub,
            Ok(&all_enabled()),
            &no_models(),
            None,
            Ok(&host),
            &test_approval_broker(&hub).await,
        )
        .await;
        let drain = std::time::Duration::from_secs(10);
        let never = std::future::pending::<()>;
        assert!(host.supervisors.disable("nope", drain, never()).is_none());
        assert!(!host.supervisors.is_disabled("remote"));
        let current = host
            .supervisors
            .disable("remote", drain, never())
            .expect("the running module")
            .current;
        assert!(host.supervisors.is_disabled("remote"));
        assert!(
            host.supervisors.disable("remote", drain, never()).is_none(),
            "disabled twice"
        );
        let closed = host.supervisors.close();
        assert!(closed.running.is_empty(), "the shutdown got it too");
        assert_eq!(
            closed.disabling.len(),
            1,
            "the disable's stop, not waited for"
        );
        for stop in closed.disabling {
            stop.task.await.unwrap();
        }
        assert_eq!(
            current.get().state(),
            agent24_os_proto::drain::DrainState::Revoked
        );
        assert!(
            host.supervisors.disable("remote", drain, never()).is_none(),
            "a disable after the shutdown began"
        );
    }

    /// A disabled package is never started — the same rule as a compiled-in
    /// module that is never constructed — and its namespace says so.
    #[tokio::test]
    async fn a_disabled_package_is_never_started() {
        let tmp = tempfile::Builder::new()
            .prefix("a24")
            .tempdir_in("/tmp")
            .unwrap();
        let packages = tmp.path().join("packages");
        write_package(&packages, "remote");
        let host = test_host(tmp.path());
        let hub = crate::events::EventsHub::default();
        let cfg = config_from(r#"{"domainOs": {"remote": {"enabled": false}}}"#);
        let (app, reports, _) = mount_all(
            &discovered(&packages),
            &tmp.path().join("os"),
            &hub,
            Ok(&cfg),
            &no_models(),
            None,
            Ok(&host),
            &test_approval_broker(&hub).await,
        )
        .await;
        assert_eq!(reports[0].outcome, MountOutcome::Disabled);
        assert!(
            host.supervisors.close().is_empty(),
            "a disabled package was started"
        );
        assert_eq!(
            body_json(get(&app, "/api/v1/remote/hi").await).await["error"]["code"],
            "module_disabled"
        );
    }

    /// A daemon that cannot start processes still mounts everything else; each
    /// package degrades to the kernel's 503, with the reason in its report.
    #[tokio::test]
    async fn without_a_process_host_a_package_degrades_with_the_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let packages = tmp.path().join("packages");
        write_package(&packages, "remote");
        let hub = crate::events::EventsHub::default();
        let (app, reports, _) = mount_all(
            &discovered(&packages),
            &tmp.path().join("os"),
            &hub,
            Ok(&all_enabled()),
            &no_models(),
            None,
            Err("the callback directory is not ours"),
            &test_approval_broker(&hub).await,
        )
        .await;
        match &reports[0].outcome {
            MountOutcome::Degraded(why) => assert!(why.contains("not ours"), "{why}"),
            other => panic!("expected Degraded, got {other:?}"),
        }
        assert_eq!(
            get(&app, "/api/v1/remote/hi").await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    /// A COMPILED-IN entry whose manifest declares an out-of-process provider
    /// is refused, not half-mounted: out-of-process modules are packages
    /// (`Build::Package`, and the real path above), and an in-process type
    /// claiming otherwise is a build mistake. (This used to be the tripwire for
    /// the "takes effect at the next daemon start" promises; since SUP-4 that
    /// is `a_package_is_started_and_proxied`, and the promises still hold — see
    /// the note in `agent24-cli`'s `OsAction`.)
    #[tokio::test]
    async fn a_compiled_in_module_declaring_out_of_process_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let m = FakeModule::with("remote", "out_of_process_provider", false);
        let (app, reports) = mount(&[entry(m.clone())], tmp.path(), &hub).await;

        assert!(matches!(reports[0].outcome, MountOutcome::Refused(_)));
        assert!(m.opened().is_none());
        assert_eq!(m.routes_built(), 0);
        assert!(
            !tmp.path().join("remote").exists(),
            "a refused module must not even have a directory created for it"
        );
        assert_eq!(
            get(&app, "/api/v1/remote/ping").await.status(),
            StatusCode::NOT_FOUND,
            "a refused module must have NO routes, not 503 ones"
        );
    }

    #[tokio::test]
    async fn a_failed_store_degrades_only_that_module() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let bad = FakeModule::with("broken", "in_process_crate", true);
        let good = FakeModule::new("healthy");
        let (app, reports) = mount(&[entry(bad), entry(good)], tmp.path(), &hub).await;

        assert!(matches!(reports[0].outcome, MountOutcome::Degraded(_)));
        assert_eq!(reports[1].outcome, MountOutcome::Mounted);

        // Every path under the broken namespace 503s with the v1 envelope — and
        // it is the KERNEL's 503, so it is served even though the module's own
        // handlers were never mounted.
        for path in ["/api/v1/broken/ping", "/api/v1/broken/anything/else"] {
            let r = get(&app, path).await;
            assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
            let j = body_json(r).await;
            assert_eq!(j["error"]["code"], "module_unavailable");
            assert!(j["error"]["message"].as_str().unwrap().contains("broken"));
        }
        // The healthy module is untouched.
        assert_eq!(
            get(&app, "/api/v1/healthy/ping").await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn a_modules_events_reach_the_hub_stamped_with_its_own_name() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let mut rx = hub.subscribe();
        let m = FakeModule::new("emitter");
        let (app, _) = mount(&[entry(m)], tmp.path(), &hub).await;

        assert_eq!(
            get(&app, "/api/v1/emitter/ping").await.status(),
            StatusCode::OK
        );
        let (_, body) = rx.try_recv().expect("the module's event reached the hub");
        match body {
            EventBody::Module(p) => {
                assert_eq!(p.module, "emitter");
                assert_eq!(p.kind, "ping.served");
            }
            other => panic!("expected a Module event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_name_that_collides_with_a_kernel_route_is_refused_not_panicked() {
        // Without this refusal, `merge` panics with "Overlapping method route" and
        // the daemon never starts — a third-party domain OS could brick the
        // process by choosing a name. The module here registers `/` under
        // `/api/v1/health`, which is exactly the colliding shape.
        struct RootRouteModule(DomainOsManifest);
        #[async_trait::async_trait]
        impl DomainModule for RootRouteModule {
            fn manifest(&self) -> &DomainOsManifest {
                &self.0
            }
            async fn open_store(&self, _dir: &Path) -> DomainResult<()> {
                Ok(())
            }
            fn routes(&self, _ctx: Arc<dyn KernelCtx>) -> Router {
                Router::new().route("/", axum::routing::get(|| async { "module" }))
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let m = Arc::new(RootRouteModule(
            DomainOsManifest::from_yaml(&manifest_yaml("health", "in_process_crate")).unwrap(),
        ));
        let (modules, reports) = mount(&[entry(m)], tmp.path(), &hub).await;
        assert!(
            matches!(reports[0].outcome, MountOutcome::Refused(_)),
            "got {:?}",
            reports[0].outcome
        );
        // And the router it produced still merges into the kernel's without panicking.
        let kernel: Router =
            Router::new().route("/api/v1/health", axum::routing::get(|| async { "k" }));
        let merged = kernel.merge(modules);
        assert_eq!(
            get(&merged, "/api/v1/health").await.status(),
            StatusCode::OK
        );
    }

    /// Pin `RESERVED_KERNEL_SEGMENTS` against the kernel router's own source, in
    /// BOTH directions.
    ///
    /// A missing entry reappears as a startup panic. A STALE entry is just as bad
    /// and much quieter — and this is not hypothetical: when ME-1b-b deleted
    /// Sin90's seven hardcoded routes, `"sin90"` left in this list would have made
    /// the kernel refuse to mount its own first domain OS, and a one-directional
    /// "every kernel segment is reserved" check would have stayed green through
    /// it. The equality assertion is what forced that entry out.
    ///
    /// **It is a heuristic, and its limits are the point of saying so.** It scans
    /// literal `"/api/v1/<seg>` strings inside one function's source, so a route
    /// built from a `const`, a `format!`, a macro, or a helper that returns a
    /// router would be invisible to it — as would a route registered outside this
    /// function. It catches the way routes are actually written here today; it is
    /// not a proof. The durable fix is a router builder that records each segment
    /// as it registers it, which is worth doing when the first of those forms
    /// appears.
    #[test]
    fn reserved_segments_match_the_kernel_routes_exactly() {
        let src = include_str!("server.rs");
        let start = src
            .find("pub fn build_router_with_modules")
            .expect("build_router_with_modules must exist");
        // The function ends at the first column-zero `}` — every brace inside it is
        // indented by rustfmt.
        let body = &src[start..];
        let end = body
            .find("\n}\n")
            .expect("function must be brace-terminated");
        let body = &body[..end];

        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for (i, _) in body.match_indices("\"/api/v1/") {
            let rest = &body[i + "\"/api/v1/".len()..];
            let seg = rest
                .split(['/', '"', '{'])
                .next()
                .unwrap_or("")
                .trim_end_matches('/');
            if !seg.is_empty() {
                seen.insert(seg);
            }
        }
        let reserved: BTreeSet<&str> = RESERVED_KERNEL_SEGMENTS.iter().copied().collect();

        // Checked FIRST: if the scan broke, every reservation would otherwise be
        // reported as stale and bury the real cause.
        assert!(
            seen.len() >= 10,
            "the scan found only {} distinct kernel segments; it has gone \
             vacuous — did the routes move out of build_router_with_modules?",
            seen.len()
        );

        let missing: Vec<_> = seen.difference(&reserved).collect();
        assert!(
            missing.is_empty(),
            "kernel route segments NOT reserved — a module could claim one and \
             panic the daemon at startup: {missing:?}"
        );
        let stale: Vec<_> = reserved.difference(&seen).collect();
        assert!(
            stale.is_empty(),
            "reserved segments the kernel no longer routes — these now REFUSE a \
             legitimate module for no reason (this is the ME-1b-b trap: delete \
             Sin90's hardcoded routes and its entry must go too): {stale:?}"
        );
    }

    #[tokio::test]
    async fn a_degraded_namespace_503s_on_root_slash_descendants_and_common_methods() {
        // The 503 must cover the namespace ROOT, its trailing slash, arbitrary
        // descendants, and every method — a degraded module that answered 404 on
        // POST would read as "no such endpoint" and send a client rewriting a
        // request that was fine.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let bad = FakeModule::with("broken", "in_process_crate", true);
        let (modules, _) = mount(&[entry(bad.clone())], tmp.path(), &hub).await;

        // Merged into the FULL kernel router, so the kernel's own fallback and the
        // nested one are both in play and we learn which wins where.
        let st = crate::server::tests::state().await;
        let token = st.token.to_string();
        let app = crate::server::build_router_with_modules(st, modules);

        for method in ["GET", "POST", "PATCH", "DELETE", "PUT", "HEAD", "OPTIONS"] {
            for path in [
                "/api/v1/broken",
                "/api/v1/broken/", // the bare trailing slash: matchit's
                // `{*rest}` does not match an empty
                // segment, so this needs its own route
                "/api/v1/broken//", // double slash -> non-empty rest
                "/api/v1/broken/ping",
                "/api/v1/broken/deep/nested/path",
                "/api/v1/broken/ping?x=1", // query strings are not part of matching
            ] {
                let r = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method(method)
                            .uri(path)
                            .header("authorization", format!("Bearer {token}"))
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    r.status(),
                    StatusCode::SERVICE_UNAVAILABLE,
                    "{method} {path}"
                );
                // HEAD carries no body by definition; every other method must
                // carry the v1 envelope so a client can tell WHY it is 503.
                if method != "HEAD" {
                    assert_eq!(
                        body_json(r).await["error"]["code"],
                        "module_unavailable",
                        "{method} {path}"
                    );
                }
            }
        }

        // The degraded namespace is still BEHIND auth — a 503 that leaked without
        // a token would be a (small) unauthenticated surface.
        let r = get(&app, "/api/v1/broken/ping").await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

        // And the module was never asked for routes: the kernel serves this 503.
        assert_eq!(bad.routes_built(), 0);

        // The kernel itself is untouched.
        let r = get(&app, "/api/v1/health").await;
        assert_eq!(r.status(), StatusCode::OK);
    }

    /// `#[cfg(unix)]` on the whole test, not an early `return` inside it: a test
    /// that silently returns on another platform reports "passed" while proving
    /// nothing. Absent is honest; green-but-vacuous is not.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_module_directory_degrades_without_opening_the_store() {
        // The contamination this whole boundary exists to prevent: two module
        // directories resolving to one place. A check, not a guarantee (an ancestor
        // symlink still passes) — but it catches the case that occurs.
        let tmp = tempfile::tempdir().unwrap();
        let victim = tmp.path().join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::os::unix::fs::symlink(&victim, tmp.path().join("linked")).unwrap();

        let hub = crate::events::EventsHub::default();
        let m = FakeModule::new("linked");
        let (app, reports) = mount(&[entry(m.clone())], tmp.path(), &hub).await;

        match &reports[0].outcome {
            MountOutcome::Degraded(why) => assert!(why.contains("symlink"), "{why}"),
            other => panic!("a symlinked module directory must degrade, got {other:?}"),
        }
        assert!(
            m.opened().is_none(),
            "the module must not have been handed a symlinked directory"
        );
        assert_eq!(
            get(&app, "/api/v1/linked/ping").await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn a_module_that_never_asked_for_events_gets_no_sink() {
        // The capability is the HANDLE: a module that did not request events must
        // see `ctx.events() == None`, not an unusable sink.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let mut rx = hub.subscribe();
        let yaml = manifest_yaml("quiet", "in_process_crate")
            .replace("kernel_capabilities: [events]\n", "");
        let m = FakeModule::from_yaml(&yaml, false);
        let (app, reports) = mount(&[entry(m)], tmp.path(), &hub).await;

        assert_eq!(reports[0].outcome, MountOutcome::Mounted);
        assert!(reports[0].granted.is_empty());
        let j = body_json(get(&app, "/api/v1/quiet/ping").await).await;
        assert_eq!(j["events"], false, "no grant means no handle at all");
        assert_eq!(j["sink_module"], serde_json::Value::Null);
        assert!(rx.try_recv().is_err(), "and nothing reached the hub");
    }

    // ── T7b/ME-3e: in-process `ApprovalRequester` (judgement 18/18a) ──────

    #[tokio::test]
    async fn a_module_that_never_asked_for_approval_gets_no_requester() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let m = FakeModule::new("quiet2"); // manifest_yaml's default: [events] only
        let (_, reports) = mount(&[entry(m.clone())], tmp.path(), &hub).await;
        assert_eq!(reports[0].outcome, MountOutcome::Mounted);
        let ctx = m.ctx().expect("routes() was called, so ctx was recorded");
        assert!(
            ctx.approval().is_none(),
            "no grant means no handle at all, same as events/memory"
        );
    }

    #[tokio::test]
    async fn a_granted_modules_requester_submits_and_queries_for_real() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let yaml = manifest_yaml("approver", "in_process_crate").replace(
            "kernel_capabilities: [events]",
            "kernel_capabilities: [approval]",
        );
        let m = FakeModule::from_yaml(&yaml, false);
        let (_, reports) = mount(&[entry(m.clone())], tmp.path(), &hub).await;
        assert_eq!(reports[0].outcome, MountOutcome::Mounted);
        assert_eq!(reports[0].granted, vec!["approval".to_owned()]);

        let ctx = m.ctx().expect("routes() was called, so ctx was recorded");
        let approval = ctx.approval().expect("approval was granted");

        // Judgement 18: a real submit/status round trip through the SAME
        // path the wire handler's in-process counterpart uses
        // (`PolicyApprovalBackend` → `ModuleApprovalBroker`).
        let submitted = approval
            .submit(
                agent24_protocol::ModuleApprovalKind::Advise,
                "send_email",
                Some("ops@example.com".to_owned()),
                serde_json::json!({"body": "hi"}),
            )
            .await
            .unwrap();
        assert_eq!(
            submitted.decision,
            agent24_protocol::ModuleApprovalDecision::Pending
        );
        // Judgement 18a: `kind`/`binding` are read from the record, not
        // assumed — an `Advise` result must never look like a `Gate` one.
        assert_eq!(submitted.kind, agent24_protocol::ModuleApprovalKind::Advise);
        assert!(!submitted.binding);

        let queried = approval.status(&submitted.approval_id).await.unwrap();
        assert_eq!(
            queried.decision,
            agent24_protocol::ModuleApprovalDecision::Pending
        );
        assert_eq!(queried.kind, agent24_protocol::ModuleApprovalKind::Advise);
        assert!(!queried.binding);
    }

    #[tokio::test]
    async fn a_granted_modules_gate_submission_errs_immediately_without_touching_the_broker() {
        // Judgement 8, the in-process half: `ApprovalRequester::submit(Gate,
        // ...)` returns `Err(ActionNotInClosedSet)` directly — it never even
        // reaches `ModuleApprovalBroker`, so no record is ever created.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let yaml = manifest_yaml("gater", "in_process_crate").replace(
            "kernel_capabilities: [events]",
            "kernel_capabilities: [approval]",
        );
        let m = FakeModule::from_yaml(&yaml, false);
        let (_, reports) = mount(&[entry(m.clone())], tmp.path(), &hub).await;
        assert_eq!(reports[0].outcome, MountOutcome::Mounted);
        let ctx = m.ctx().expect("routes() was called, so ctx was recorded");
        let approval = ctx.approval().expect("approval was granted");

        let err = approval
            .submit(
                agent24_protocol::ModuleApprovalKind::Gate,
                "transfer_funds",
                None,
                serde_json::json!({}),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            agent24_protocol::ApprovalRequestError::ActionNotInClosedSet
        ));
    }

    // ── T7c/ME-3e: in-process `schedule_callback` (judgement 1/11/12/16) ───

    #[tokio::test]
    async fn a_granted_modules_gate_schedule_callback_succeeds_and_canonicalizes_target() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        // Built directly (not via the `mount` helper, which swallows its own
        // throwaway broker) so this test can inspect the ACTUAL row the
        // broker persisted afterwards — a bare `validate_gate_action(...)`
        // comparison alone would pass even if `PolicyApprovalBackend`
        // silently stored the module's raw, uncanonicalized string instead
        // of the canonicalized one (Codex review, criterion 16).
        let broker = test_approval_broker(&hub).await;
        let yaml = manifest_yaml("gater2", "in_process_crate").replace(
            "kernel_capabilities: [events]",
            "kernel_capabilities: [approval]",
        );
        let m = FakeModule::from_yaml(&yaml, false);
        let (_, reports, _) = mount_all(
            &[entry(m.clone())],
            tmp.path(),
            &hub,
            Ok(&all_enabled()),
            &no_models(),
            None,
            Err("no process host in this test"),
            &broker,
        )
        .await;
        assert_eq!(reports[0].outcome, MountOutcome::Mounted);
        let ctx = m.ctx().expect("routes() was called, so ctx was recorded");
        let approval = ctx.approval().expect("approval was granted");

        let raw_target = "2026-01-01T08:00:00.5+08:00"; // epoch-equal to 2026-01-01T00:00:00Z
        let submitted = approval
            .submit(
                agent24_protocol::ModuleApprovalKind::Gate,
                "schedule_callback",
                Some(raw_target.to_owned()),
                serde_json::json!({}),
            )
            .await
            .unwrap();
        assert_eq!(
            submitted.decision,
            agent24_protocol::ModuleApprovalDecision::Pending
        );
        assert!(
            submitted.binding,
            "a Gate row must be binding (judgement 11)"
        );
        assert_eq!(submitted.executed_at, None);

        // Judgement 16: fetch the row the broker ACTUALLY persisted and
        // assert its stored `target` — not just the validator's return
        // value in isolation — equals the canonicalized form, proving
        // `PolicyApprovalBackend` really did replace the module's raw
        // string with `CanonicalGateAction.target` before writing it.
        let stored = broker
            .get(&submitted.approval_id)
            .await
            .unwrap()
            .expect("the row PolicyApprovalBackend inserted must be readable back");
        assert_eq!(
            stored.target.as_deref(),
            Some("2026-01-01T00:00:00Z"),
            "the STORED target must be the canonicalized form, not the module's raw string"
        );
    }

    #[tokio::test]
    async fn a_granted_modules_gate_schedule_callback_without_a_target_is_invalid_target() {
        // Judgement 12, in-process half: missing `target` is `InvalidTarget`,
        // never `ActionNotInClosedSet` — the action IS in the closed set.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let yaml = manifest_yaml("gater3", "in_process_crate").replace(
            "kernel_capabilities: [events]",
            "kernel_capabilities: [approval]",
        );
        let m = FakeModule::from_yaml(&yaml, false);
        let (_, reports) = mount(&[entry(m.clone())], tmp.path(), &hub).await;
        assert_eq!(reports[0].outcome, MountOutcome::Mounted);
        let ctx = m.ctx().expect("routes() was called, so ctx was recorded");
        let approval = ctx.approval().expect("approval was granted");

        let err = approval
            .submit(
                agent24_protocol::ModuleApprovalKind::Gate,
                "schedule_callback",
                None,
                serde_json::json!({}),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            agent24_protocol::ApprovalRequestError::InvalidTarget(_)
        ));
    }

    // ---------- F1: memory partitions handed out by the MOUNTER ----------

    #[tokio::test]
    async fn the_mounter_lends_each_module_its_own_memory_partition() {
        // The unit tests in `os_memory` prove two handles are isolated. This proves
        // the MOUNTER actually hands out such handles — that the capability is
        // wired, keyed per module, and recorded in the catalog.
        use agent24_domain::memory::Remember;

        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let lease = MemoryLease::open("alice", kv.clone()).await.unwrap();

        // Both modules ask for memory in their manifests.
        let want_mem = |name: &str| {
            let yaml = manifest_yaml(name, "in_process_crate").replace(
                "kernel_capabilities: [events]",
                "kernel_capabilities: [memory]",
            );
            FakeModule::from_yaml(&yaml, false)
        };
        let a = want_mem("alpha");
        let b = want_mem("beta");
        let (_, reports, partitions) = mount_all(
            &[entry(a.clone()), entry(b.clone())],
            tmp.path(),
            &hub,
            Ok(&all_enabled()),
            &no_models(),
            Some(&lease),
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;

        assert!(reports.iter().all(|r| r.outcome == MountOutcome::Mounted));
        assert!(
            reports
                .iter()
                .all(|r| r.granted == vec!["memory".to_owned()])
        );

        // Two partitions, recorded — so a future export/erase path has a list
        // rather than a prefix match over keys containing NUL.
        assert_eq!(partitions.partitions().len(), 2);
        // From the DURABLE table, which is what a later export/erase path reads —
        // not from this run's inventory, which cannot see a disabled or renamed
        // module's leftovers.
        let rows = crate::os_memory::OsMemoryCatalog::durable_for_org(&lease.kv, &lease.org)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_ne!(
            rows[0].owner_key, rows[1].owner_key,
            "one partition per module, not per user"
        );

        // And the handles the mounter actually built are isolated from each other.
        let ctx_a = a.ctx().unwrap();
        let ctx_b = b.ctx().unwrap();
        let ma = ctx_a.memory().unwrap();
        let mb = ctx_b.memory().unwrap();
        ma.remember(Remember::new("note", serde_json::Map::new()))
            .await
            .unwrap();
        assert_eq!(ma.recent(10).await.unwrap().len(), 1);
        assert!(
            mb.recent(10).await.unwrap().is_empty(),
            "the other module must see nothing the mounter gave the first one"
        );
    }

    #[tokio::test]
    async fn a_module_that_did_not_ask_for_memory_gets_no_handle() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let lease = MemoryLease::open("alice", kv).await.unwrap();
        // `manifest_yaml` asks for events only.
        let m = FakeModule::new("quiet");
        let (_, reports, partitions) = mount_all(
            &[entry(m.clone())],
            tmp.path(),
            &hub,
            Ok(&all_enabled()),
            &no_models(),
            Some(&lease),
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;
        assert!(m.ctx().unwrap().memory().is_none());
        assert!(
            partitions.partitions().is_empty(),
            "and no partition was recorded for a module that never got one"
        );
        assert!(
            !reports[0].granted.contains(&"memory".to_owned()),
            "and the report must not claim a capability the module does not hold"
        );
    }

    #[tokio::test]
    async fn no_memory_base_means_no_handle_rather_than_a_broken_one() {
        // A daemon whose memory base failed to open lends nothing. The alternative
        // — a handle that errors on every call — would make every module carry a
        // failure path for a capability it was told it had.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let yaml = manifest_yaml("hungry", "in_process_crate").replace(
            "kernel_capabilities: [events]",
            "kernel_capabilities: [memory]",
        );
        let m = FakeModule::from_yaml(&yaml, false);
        let (_, reports, partitions) = mount_all(
            &[entry(m.clone())],
            tmp.path(),
            &hub,
            Ok(&all_enabled()),
            &no_models(),
            None,
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;
        assert_eq!(reports[0].outcome, MountOutcome::Mounted, "it still mounts");
        assert!(m.ctx().unwrap().memory().is_none());
        assert!(partitions.partitions().is_empty());
        assert!(
            !reports[0].granted.contains(&"memory".to_owned()),
            "a module that asked for memory and got none must not be REPORTED as \
             holding it — `granted` names what a live module holds (#134)"
        );
    }

    #[tokio::test]
    async fn a_partition_that_cannot_be_recorded_is_not_lent() {
        // The precondition, end to end. Lending a partition the catalog could not
        // record creates rows under a NUL-containing owner key that no later
        // export, erase or key-version migration can attribute to anyone.
        //
        // The failure is provoked the way it could really happen: the durable row
        // for this module's key already exists, recorded against a DIFFERENT
        // identity, so `record_os_partition` refuses to re-attribute it.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let yaml = manifest_yaml("hungry", "in_process_crate").replace(
            "kernel_capabilities: [events]",
            "kernel_capabilities: [memory]",
        );
        let m = FakeModule::from_yaml(&yaml, false);
        // Resolved here so the poisoned row is recorded against the SAME org the
        // lease will resolve a moment later — otherwise the mount would derive a
        // different key and sail past the conflict this test exists to provoke.
        let org =
            crate::os_memory::OrgId::from_store(kv.ensure_org_for_user("alice").await.unwrap());
        let key = crate::os_memory::partition_key(
            &org,
            &crate::os_memory::SpaceId::module_private("hungry"),
        );
        kv.record_os_partition(agent24_memory::OsPartitionIdentity {
            owner_key: &key,
            key_version: "v0-from-an-older-kernel",
            org_id: org.as_str(),
            space_id: "os:hungry",
            user: "alice",
            module: "hungry",
        })
        .await
        .unwrap();

        let lease = MemoryLease::open("alice", kv).await.unwrap();
        let (_, reports, partitions) = mount_all(
            &[entry(m.clone())],
            tmp.path(),
            &hub,
            Ok(&all_enabled()),
            &no_models(),
            Some(&lease),
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;

        // It still mounts — a bookkeeping failure must not take down a module's
        // HTTP surface — but without the capability, and saying so.
        assert_eq!(reports[0].outcome, MountOutcome::Mounted);
        assert!(
            m.ctx().unwrap().memory().is_none(),
            "no handle, rather than one writing rows nothing can attribute"
        );
        assert!(!reports[0].granted.contains(&"memory".to_owned()));
        assert!(partitions.partitions().is_empty());
    }

    // ---------- T8.5c-W-mount: OOP memory entitlement (C1/C2/H1/H3/M2) ----------

    #[tokio::test]
    async fn memory_grant_name_reflects_only_whether_a_real_handle_exists() {
        // §5.3 unit-level judgement: the rule both `granted_names`/`provides`
        // filtering in `mount_package` rely on, in isolation from any mount.
        assert_eq!(
            crate::os_memory::memory_grant_name(&crate::os_memory::MemoryEntitlement::NONE),
            None
        );
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let with_handle = crate::os_memory::build_private_memory_entitlement(Some((
            Arc::new(crate::os_memory::OsScopedMemory::new(
                &crate::os_memory::OsMemoryPartition {
                    key: "k".to_owned(),
                    org: crate::os_memory::OrgId::from_store("org"),
                    space: crate::os_memory::SpaceId::module_private("m"),
                    user: "alice".to_owned(),
                    module: "m".to_owned(),
                },
                &kv,
            )),
            Arc::new(tokio::sync::Semaphore::new(4)),
        )));
        assert_eq!(
            crate::os_memory::memory_grant_name(&with_handle),
            Some("memory")
        );
    }

    #[tokio::test]
    async fn build_private_memory_entitlement_preserves_admission_and_creates_a_fresh_limiter() {
        // §4.1/§7.1 judgement 4, unit level: given the same admission `Arc`
        // twice, the function must hand it back unchanged (not clone into a
        // new object) — and must build a NEW `RateLimiter` every call, never
        // a daemon-level singleton.
        let admission = Arc::new(tokio::sync::Semaphore::new(4));
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let a = crate::os_memory::build_private_memory_entitlement(Some((
            Arc::new(crate::os_memory::OsScopedMemory::new(
                &crate::os_memory::OsMemoryPartition {
                    key: "k-a".to_owned(),
                    org: crate::os_memory::OrgId::from_store("org"),
                    space: crate::os_memory::SpaceId::module_private("a"),
                    user: "alice".to_owned(),
                    module: "a".to_owned(),
                },
                &kv,
            )),
            admission.clone(),
        )));
        let b = crate::os_memory::build_private_memory_entitlement(Some((
            Arc::new(crate::os_memory::OsScopedMemory::new(
                &crate::os_memory::OsMemoryPartition {
                    key: "k-b".to_owned(),
                    org: crate::os_memory::OrgId::from_store("org"),
                    space: crate::os_memory::SpaceId::module_private("b"),
                    user: "alice".to_owned(),
                    module: "b".to_owned(),
                },
                &kv,
            )),
            admission.clone(),
        )));
        let ha = a.private_handle().unwrap();
        let hb = b.private_handle().unwrap();
        assert!(
            Arc::ptr_eq(&ha.admission, &hb.admission),
            "the same admission Arc handed in twice must come back unchanged"
        );
        assert!(
            !Arc::ptr_eq(&ha.limiter, &hb.limiter),
            "each call must build its OWN RateLimiter, never share a daemon-level one"
        );
    }

    #[test]
    fn two_modules_rate_limiters_are_behaviourally_isolated_not_just_different_pointers() {
        // §7.1 judgement 4's negative control: a pointer-inequality check alone
        // cannot rule out two `RateLimiter`s sharing internal state. This drains
        // module A's limiter to empty and asserts module B's is unaffected —
        // using `RateLimiter::with_clock` with a clock that never advances, so
        // the number of calls needed to exhaust the budget is a constant, not a
        // race against real refill (same pattern as `os_memory_page`'s
        // `frozen_limiter`).
        struct FrozenClock(std::time::Instant);
        impl crate::events_emit::Clock for FrozenClock {
            fn now(&self) -> std::time::Instant {
                self.0
            }
        }
        let frozen = std::time::Instant::now();
        let a =
            crate::events_emit::RateLimiter::with_clock(1.0, 0.0, Arc::new(FrozenClock(frozen)));
        let b =
            crate::events_emit::RateLimiter::with_clock(1.0, 0.0, Arc::new(FrozenClock(frozen)));
        assert!(a.try_acquire(), "A's first call spends its only token");
        assert!(
            !a.try_acquire(),
            "A must now be exhausted (frozen clock: no refill)"
        );
        assert!(
            b.try_acquire(),
            "B must be a completely separate budget, unaffected by A's exhaustion"
        );
    }

    #[tokio::test]
    async fn a_file_backed_kv_stores_real_oop_admission_permit_count_is_exactly_max_minus_one() {
        // §7.1 judgement 4, Low fix (round 4): the permit count must come from
        // a REAL `KvStore::open`, not from re-deriving the same formula the
        // test would also use to construct a fake — that would only prove the
        // test agrees with itself, not that `KvStore::open` computed it right.
        let tmp = tempfile::tempdir().unwrap();
        let kv = agent24_memory::KvStore::open(&tmp.path().join("m.db"))
            .await
            .unwrap();
        let admission = kv.oop_admission().expect("file-backed must have admission");
        assert_eq!(
            admission.available_permits(),
            (agent24_memory::KVSTORE_MAX_CONNECTIONS - 1) as usize
        );
        assert!(
            agent24_memory::KvStore::open_memory()
                .await
                .unwrap()
                .oop_admission()
                .is_none(),
            "ephemeral must never construct an admission permit, not even a zero-capacity one"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_oop_module_that_asks_for_memory_is_granted_it_over_a_file_backed_lease() {
        // ★ C1 positive judgement, integration level: a REAL out-of-process
        // package, a REAL supervisor start, a file-backed (non-ephemeral)
        // lease. Whether `Offer` itself carries the value over the real
        // handshake is T8.5c-W-wire's job (§3.3's acceptance gap) — this
        // proves the mount layer's own return values (`MountReport`,
        // `OsMemoryCatalog`) are correct.
        let tmp = tempfile::Builder::new()
            .prefix("a24")
            .tempdir_in("/tmp")
            .unwrap();
        let packages = tmp.path().join("packages");
        write_package_with(&packages, "hungry-oop", "[memory]", PACKAGE_MODULE);
        let host = test_host(tmp.path());
        let hub = crate::events::EventsHub::default();
        let kv = agent24_memory::KvStore::open(&tmp.path().join("mem.db"))
            .await
            .unwrap();
        let lease = MemoryLease::open("alice", kv).await.unwrap();
        let (_, reports, partitions) = mount_all(
            &discovered(&packages),
            &tmp.path().join("os"),
            &hub,
            Ok(&all_enabled()),
            &no_models(),
            Some(&lease),
            Ok(&host),
            &test_approval_broker(&hub).await,
        )
        .await;
        assert_eq!(
            reports[0].outcome,
            MountOutcome::Mounted,
            "{:?}",
            reports[0]
        );
        assert_eq!(reports[0].granted, vec!["memory".to_owned()]);
        assert_eq!(
            partitions.partitions().len(),
            1,
            "a real successful mount must confirm the partition (mark_mounted), \
             not just record it (ensure_recorded)"
        );

        for s in host.supervisors.close().running {
            s.handle.stop().await.expect("a clean stop");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_oop_module_whose_partition_cannot_be_recorded_mounts_without_memory() {
        // C1's negative control, integration level, provoked the way it could
        // really happen (like `a_partition_that_cannot_be_recorded_is_not_lent`
        // above, but through the OOP path): the durable identity row already
        // exists under a DIFFERENT identity, so `ensure_recorded` refuses it.
        let tmp = tempfile::Builder::new()
            .prefix("a24")
            .tempdir_in("/tmp")
            .unwrap();
        let packages = tmp.path().join("packages");
        write_package_with(&packages, "hungry-oop", "[memory]", PACKAGE_MODULE);
        let host = test_host(tmp.path());
        let hub = crate::events::EventsHub::default();
        let kv = agent24_memory::KvStore::open(&tmp.path().join("mem.db"))
            .await
            .unwrap();
        let org =
            crate::os_memory::OrgId::from_store(kv.ensure_org_for_user("alice").await.unwrap());
        let key = crate::os_memory::partition_key(
            &org,
            &crate::os_memory::SpaceId::module_private("hungry-oop"),
        );
        kv.record_os_partition(agent24_memory::OsPartitionIdentity {
            owner_key: &key,
            key_version: "v0-from-an-older-kernel",
            org_id: org.as_str(),
            space_id: "os:hungry-oop",
            user: "alice",
            module: "hungry-oop",
        })
        .await
        .unwrap();
        let lease = MemoryLease::open("alice", kv).await.unwrap();
        let (_, reports, partitions) = mount_all(
            &discovered(&packages),
            &tmp.path().join("os"),
            &hub,
            Ok(&all_enabled()),
            &no_models(),
            Some(&lease),
            Ok(&host),
            &test_approval_broker(&hub).await,
        )
        .await;

        assert_eq!(
            reports[0].outcome,
            MountOutcome::Mounted,
            "a bookkeeping failure must not take down the module's process"
        );
        assert!(!reports[0].granted.contains(&"memory".to_owned()));
        assert!(partitions.partitions().is_empty());

        for s in host.supervisors.close().running {
            s.handle.stop().await.expect("a clean stop");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ephemeral_withholds_oop_memory_but_not_in_process_memory() {
        // ★ C2 boundary judgement: ephemeral withholds the OOP capability
        // (no admission budget to offer — decision 4) while an in-process
        // module under the SAME lease is completely unaffected (positive
        // control — proves this is an admission-budget boundary, not
        // ephemeral turning memory off altogether).
        let tmp = tempfile::Builder::new()
            .prefix("a24")
            .tempdir_in("/tmp")
            .unwrap();
        let packages = tmp.path().join("packages");
        write_package_with(&packages, "hungry-oop", "[memory]", PACKAGE_MODULE);
        let host = test_host(tmp.path());
        let hub = crate::events::EventsHub::default();
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        assert!(
            kv.oop_admission().is_none(),
            "sanity: the ephemeral pool must never carry an admission permit"
        );
        let lease = MemoryLease::open("alice", kv).await.unwrap();

        let in_process_wants_memory = {
            let yaml = manifest_yaml("hungry-in-process", "in_process_crate").replace(
                "kernel_capabilities: [events]",
                "kernel_capabilities: [memory]",
            );
            FakeModule::from_yaml(&yaml, false)
        };
        let mut catalogue = discovered(&packages);
        catalogue.push(entry(in_process_wants_memory.clone()));

        let (_, reports, _) = mount_all(
            &catalogue,
            tmp.path(),
            &hub,
            Ok(&all_enabled()),
            &no_models(),
            Some(&lease),
            Ok(&host),
            &test_approval_broker(&hub).await,
        )
        .await;

        let oop_report = reports.iter().find(|r| r.name == "hungry-oop").unwrap();
        assert_eq!(oop_report.outcome, MountOutcome::Mounted);
        assert!(
            !oop_report.granted.contains(&"memory".to_owned()),
            "ephemeral must withhold the OOP memory capability"
        );
        assert!(
            in_process_wants_memory.ctx().unwrap().memory().is_some(),
            "ephemeral must NOT withhold in-process memory — only the OOP \
             admission budget is absent, decision 4's whole point"
        );

        for s in host.supervisors.close().running {
            s.handle.stop().await.expect("a clean stop");
        }
    }

    // ---------- ME-2: the registry ----------

    fn config_from(json: &str) -> crate::os_config::OsConfig {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("os.json");
        std::fs::write(&p, json).unwrap();
        crate::os_config::OsConfig::load(&p).unwrap()
    }

    #[tokio::test]
    async fn a_disabled_module_503s_instead_of_vanishing() {
        // 503 with a DISTINCT code, not 404: "you turned this off" and "no such
        // feature" send an operator to different places, and only one is true.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let m = FakeModule::new("offswitch");
        let cfg = config_from(r#"{"domainOs": {"offswitch": {"enabled": false}}}"#);
        let (app, reports, _) = mount_all(
            &[entry(m.clone())],
            tmp.path(),
            &hub,
            Ok(&cfg),
            &no_models(),
            None,
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;

        assert_eq!(reports[0].outcome, MountOutcome::Disabled);
        // Nothing module-side ran: no store opened, no routes built, no directory.
        assert!(m.opened().is_none());
        assert_eq!(m.routes_built(), 0);
        assert!(!tmp.path().join("offswitch").exists());

        for method in ["GET", "POST", "PATCH", "DELETE", "HEAD"] {
            for path in [
                "/api/v1/offswitch",
                "/api/v1/offswitch/",
                "/api/v1/offswitch/ping",
                "/api/v1/offswitch/deep/path",
            ] {
                let r = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method(method)
                            .uri(path)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    r.status(),
                    StatusCode::SERVICE_UNAVAILABLE,
                    "{method} {path}"
                );
                if method != "HEAD" {
                    assert_eq!(
                        body_json(r).await["error"]["code"],
                        "module_disabled",
                        "{method} {path}"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn a_disabled_namespace_is_still_behind_kernel_auth() {
        // Merged into the REAL kernel router: a 503 that leaked without a token
        // would be a (small) unauthenticated surface, and it is the kind of thing
        // that only shows up once the pieces are assembled.
        let st = crate::server::tests::state().await;
        let token = st.token.to_string();
        let tmp = tempfile::tempdir().unwrap();
        let cfg = config_from(r#"{"domainOs": {"offswitch": {"enabled": false}}}"#);
        let (modules, _, _) = mount_all(
            &[entry(FakeModule::new("offswitch"))],
            tmp.path(),
            &st.events,
            Ok(&cfg),
            &no_models(),
            None,
            Err("no process host in this test"),
            &test_approval_broker(&st.events).await,
        )
        .await;
        let app = crate::server::build_router_with_modules(st, modules);

        assert_eq!(
            get(&app, "/api/v1/offswitch/ping").await.status(),
            StatusCode::UNAUTHORIZED
        );
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/offswitch/ping")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_json(r).await["error"]["code"], "module_disabled");
        // And the kernel is untouched.
        assert_eq!(get(&app, "/api/v1/health").await.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_disabled_module_still_holds_its_name() {
        // Otherwise switching Sin90 off would let a second module quietly take the
        // `sin90` namespace — and its data directory.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let off = FakeModule::new("shared");
        let squatter = FakeModule::new("shared");
        let cfg = config_from(r#"{"domainOs": {"shared": {"enabled": false}}}"#);
        let (app, reports, _) = mount_all(
            &[entry(off), entry(squatter.clone())],
            tmp.path(),
            &hub,
            Ok(&cfg),
            &no_models(),
            None,
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;

        assert_eq!(reports[0].outcome, MountOutcome::Disabled);
        assert!(matches!(reports[1].outcome, MountOutcome::Refused(_)));
        assert_eq!(squatter.routes_built(), 0);
        // The namespace answers "disabled", not the squatter's routes.
        assert_eq!(
            body_json(get(&app, "/api/v1/shared/ping").await).await["error"]["code"],
            "module_disabled"
        );
    }

    #[tokio::test]
    async fn a_registry_error_degrades_every_admissible_module_rather_than_hiding_them() {
        // Three candidate behaviours, and only one is both safe and legible:
        //   - fall back to defaults  → MOUNTS something the user disabled;
        //   - mount nothing          → 404, which reads as "this feature is gone"
        //                              and sends the user looking in the wrong place;
        //   - degrade every module   → 503 that names the config. ← this one.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let a = FakeModule::new("alpha");
        let b = FakeModule::new("beta");
        let (app, reports, _) = mount_all(
            &[entry(a.clone()), entry(b.clone())],
            tmp.path(),
            &hub,
            Err("os.json is not valid: expected value at line 1"),
            &no_models(),
            None,
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;

        assert_eq!(reports.len(), 2);
        for r in &reports {
            match &r.outcome {
                MountOutcome::Degraded(why) => assert!(why.contains("os.json"), "{why}"),
                other => panic!("expected Degraded, got {other:?}"),
            }
        }
        // Nothing module-side ran, and no directories were made: we do not know
        // which of these the user wanted, so we touch none of them.
        assert!(a.opened().is_none() && b.opened().is_none());
        assert_eq!(a.routes_built() + b.routes_built(), 0);
        assert!(!tmp.path().join("alpha").exists());
        assert!(!tmp.path().join("beta").exists());
        assert!(
            reports
                .iter()
                .all(|r| r.resources == ResourceStatus::NotChecked),
            "a module that never ran has no resource facts"
        );

        // The namespaces answer 503 — and with a code that points at the CONFIG,
        // not at the modules, which are fine. Sending an operator to debug a module
        // over a JSON typo would waste their time.
        for path in ["/api/v1/alpha/ping", "/api/v1/beta/", "/api/v1/beta"] {
            let r = get(&app, path).await;
            assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
            let j = body_json(r).await;
            assert_eq!(j["error"]["code"], "registry_invalid", "{path}");
            assert!(
                j["error"]["message"].as_str().unwrap().contains("os.json"),
                "the wire must name the config, not just the log: {j}"
            );
        }
    }

    #[tokio::test]
    async fn a_module_whose_store_failed_reports_no_resource_facts() {
        // The check used to run before `open_store`, so a module that then failed
        // still carried `MissingModels` — and the startup log said "it is mounted"
        // about something that was not.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let yaml = format!(
            "{}requires_models: [absent-model]\n",
            manifest_yaml("broken", "in_process_crate")
        );
        let m = FakeModule::from_yaml(&yaml, true);
        let (_, reports, _) = mount_all(
            &[entry(m)],
            tmp.path(),
            &hub,
            Ok(&all_enabled()),
            &TestModels(Ok(vec!["something-else".to_owned()])),
            None,
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;
        assert!(matches!(reports[0].outcome, MountOutcome::Degraded(_)));
        assert_eq!(
            reports[0].resources,
            ResourceStatus::NotChecked,
            "a module that never came up must not be reported as missing a model"
        );
    }

    #[tokio::test]
    async fn a_typod_disable_is_harmless_under_deny_by_default() {
        // The danger only exists when unlisted means ENABLED. Under an allow-list a
        // misspelled name leaves the real module unlisted and therefore already
        // off — the user's intent is satisfied, and failing the whole registry over
        // a harmless tombstone would be worse than the bug.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let cfg = config_from(
            r#"{"default": "disabled",
                "domainOs": {"sin09": {"enabled": false}, "sin90": {"enabled": true}}}"#,
        );
        let (app, reports, _) = mount_all(
            &[entry(FakeModule::new("sin90"))],
            tmp.path(),
            &hub,
            Ok(&cfg),
            &no_models(),
            None,
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;
        assert_eq!(
            reports[0].outcome,
            MountOutcome::Mounted,
            "a stale entry must not take the whole registry down here"
        );
        assert_eq!(
            get(&app, "/api/v1/sin90/ping").await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn a_module_absent_from_the_config_is_enabled() {
        // The upgrade path. Defaulting to disabled would switch Sin90 off for every
        // existing user the moment os.json was introduced.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        // `someone-else` is a REAL module here, so disabling it is a legitimate
        // config rather than the typo case guarded above.
        let cfg = config_from(r#"{"domainOs": {"someone-else": {"enabled": false}}}"#);
        let (app, reports, _) = mount_all(
            &[
                entry(FakeModule::new("newcomer")),
                entry(FakeModule::new("someone-else")),
            ],
            tmp.path(),
            &hub,
            Ok(&cfg),
            &no_models(),
            None,
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;
        assert_eq!(
            reports[0].outcome,
            MountOutcome::Mounted,
            "a module the file never mentions runs"
        );
        assert_eq!(reports[1].outcome, MountOutcome::Disabled);
        assert_eq!(
            get(&app, "/api/v1/newcomer/ping").await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn a_typod_disable_does_not_leave_the_real_module_running() {
        // `sin09: false` is indistinguishable from a working config, and the module
        // the user meant to switch off keeps serving. That is the same
        // "a mistake silently keeps something on" failure that justifies rejecting
        // malformed JSON, so it gets the same answer: everything 503s, naming the
        // bad entry. An unknown ENABLED entry stays a warning — it asks for
        // something absent and nothing happens.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let real = FakeModule::new("sin90");
        let cfg = config_from(r#"{"domainOs": {"sin09": {"enabled": false}}}"#);
        let (app, reports, _) = mount_all(
            &[entry(real.clone())],
            tmp.path(),
            &hub,
            Ok(&cfg),
            &no_models(),
            None,
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;

        match &reports[0].outcome {
            MountOutcome::Degraded(why) => assert!(why.contains("sin09"), "{why}"),
            other => panic!("a typo'd disable must not leave it Mounted, got {other:?}"),
        }
        assert_eq!(
            get(&app, "/api/v1/sin90/ping").await.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "the module the user MEANT to disable must not still be serving"
        );
        assert_eq!(real.routes_built(), 0);

        // The harmless half: an unknown ENABLED entry changes nothing.
        let ok_cfg = config_from(r#"{"domainOs": {"sin09": {"enabled": true}}}"#);
        let (app, reports, _) = mount_all(
            &[entry(FakeModule::new("sin90"))],
            tmp.path(),
            &hub,
            Ok(&ok_cfg),
            &no_models(),
            None,
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;
        assert_eq!(reports[0].outcome, MountOutcome::Mounted);
        assert_eq!(
            get(&app, "/api/v1/sin90/ping").await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn deny_by_default_runs_only_what_is_listed() {
        // Listing today's modules as false cannot protect a user from one a future
        // build adds — and ME-3 makes that a real exposure. An allow-list has to be
        // expressible.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let cfg =
            config_from(r#"{"default": "disabled", "domainOs": {"wanted": {"enabled": true}}}"#);
        let (app, reports, _) = mount_all(
            &[
                entry(FakeModule::new("wanted")),
                entry(FakeModule::new("unlisted")),
            ],
            tmp.path(),
            &hub,
            Ok(&cfg),
            &no_models(),
            None,
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;
        assert_eq!(reports[0].outcome, MountOutcome::Mounted);
        assert_eq!(
            reports[1].outcome,
            MountOutcome::Disabled,
            "an unlisted module must NOT run under deny-by-default"
        );
        assert_eq!(
            get(&app, "/api/v1/wanted/ping").await.status(),
            StatusCode::OK
        );
        assert_eq!(
            body_json(get(&app, "/api/v1/unlisted/ping").await).await["error"]["code"],
            "module_disabled"
        );
    }

    #[tokio::test]
    async fn identity_admission_beats_disabled_but_manifest_admission_cannot() {
        // There are TWO kinds of admission, and only one can run before the user's
        // switch:
        //
        // - IDENTITY admission (duplicate name, kernel-reserved name) needs only
        //   the catalogue, so it runs first and beats `disabled`. Otherwise a module
        //   named `health` would report Disabled while `/api/v1/health` answered
        //   200 — two truths at once — and re-enabling it would fail in a way the
        //   earlier report gave no hint of.
        // - MANIFEST admission (the out-of-process transport) needs the manifest,
        //   which needs CONSTRUCTION. A disabled module is deliberately never
        //   constructed — that is what lets a user switch off a module whose
        //   constructor is breaking the daemon — so its transport is simply not
        //   known yet, and `Disabled` is the truthful report: the reason it is not
        //   running is the user's own setting. It is refused the moment it is
        //   enabled, which `an_out_of_process_manifest_is_refused_not_half_mounted`
        //   covers.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let cfg = config_from(
            r#"{"domainOs": {"health": {"enabled": false}, "remote": {"enabled": false}}}"#,
        );
        let (_, reports, _) = mount_all(
            &[
                entry(FakeModule::new("health")),
                entry(FakeModule::with("remote", "out_of_process_provider", false)),
            ],
            tmp.path(),
            &hub,
            Ok(&cfg),
            &no_models(),
            None,
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;
        assert!(
            matches!(reports[0].outcome, MountOutcome::Refused(_)),
            "a kernel-reserved NAME is knowable without constructing, so it is \
             refused even when disabled; got {:?}",
            reports[0].outcome
        );
        assert_eq!(
            reports[1].outcome,
            MountOutcome::Disabled,
            "an out-of-process TRANSPORT is only knowable from the manifest, which \
             a disabled module is never constructed to provide"
        );
        for r in &reports {
            assert_eq!(
                r.resources,
                ResourceStatus::NotChecked,
                "a module that never ran has no resource facts"
            );
            assert!(
                r.granted.is_empty(),
                "nor any capability facts — its manifest was never read"
            );
        }
    }

    #[tokio::test]
    async fn a_disabled_module_is_never_constructed() {
        // The bootstrapping fix, tested where it actually lives. If construction
        // ran first, a module whose constructor crashes the daemon could never be
        // switched off — the tool for switching it off needs the daemon.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let built = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = built.clone();
        let cat = vec![Installed {
            name: "crashy".to_owned(),
            version: "0.1.0".to_owned(),
            build: Build::InProcess(Box::new(move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err("would have taken the daemon down".to_owned())
            })),
        }];
        let cfg = config_from(r#"{"domainOs": {"crashy": {"enabled": false}}}"#);
        let (app, reports, _) = mount_all(
            &cat,
            tmp.path(),
            &hub,
            Ok(&cfg),
            &no_models(),
            None,
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;

        assert_eq!(reports[0].outcome, MountOutcome::Disabled);
        assert_eq!(
            built.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a switched-off module must not be constructed at all"
        );
        assert_eq!(
            body_json(get(&app, "/api/v1/crashy/ping").await).await["error"]["code"],
            "module_disabled"
        );
    }

    #[tokio::test]
    async fn an_unreadable_registry_constructs_nothing() {
        // We do not know what the user wanted, so we build nothing — same reason.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let built = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = built.clone();
        let cat = vec![Installed {
            name: "any".to_owned(),
            version: "0.1.0".to_owned(),
            build: Build::InProcess(Box::new(move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err("never reached".to_owned())
            })),
        }];
        let (app, reports, _) = mount_all(
            &cat,
            tmp.path(),
            &hub,
            Err("os.json is not valid"),
            &no_models(),
            None,
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;

        assert!(matches!(reports[0].outcome, MountOutcome::Degraded(_)));
        assert_eq!(built.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            body_json(get(&app, "/api/v1/any/ping").await).await["error"]["code"],
            "registry_invalid"
        );
    }

    #[tokio::test]
    async fn a_module_that_fails_to_construct_still_has_a_name_and_a_namespace() {
        // It used to vanish from the reports entirely, so `agent24 os disable` was
        // refused for a name that "did not exist" — in exactly the situation where
        // a user most needs to switch it off.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let (app, reports) = mount(
            &[broken_entry("brokenbuild", "manifest invalid")],
            tmp.path(),
            &hub,
        )
        .await;

        assert_eq!(reports[0].name, "brokenbuild");
        assert_eq!(reports[0].namespace, "/api/v1/brokenbuild");
        match &reports[0].outcome {
            MountOutcome::Degraded(why) => {
                assert!(why.contains("could not be constructed"), "{why}");
                assert!(why.contains("manifest invalid"), "{why}");
            }
            other => panic!("expected Degraded, got {other:?}"),
        }
        assert_eq!(
            get(&app, "/api/v1/brokenbuild/ping").await.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "and its namespace answers 503, not 404"
        );
    }

    #[tokio::test]
    async fn two_disabled_twins_still_collide_on_their_name() {
        // Skipped entries used to be appended AFTER `mount_all`, so they never
        // claimed a name: two disabled twins both "succeeded" and their namespaces
        // could register overlapping routes.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let cfg = config_from(r#"{"domainOs": {"twin": {"enabled": false}}}"#);
        let (app, reports, _) = mount_all(
            &[
                entry(FakeModule::new("twin")),
                entry(FakeModule::new("twin")),
            ],
            tmp.path(),
            &hub,
            Ok(&cfg),
            &no_models(),
            None,
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;

        assert_eq!(reports[0].outcome, MountOutcome::Disabled);
        assert!(
            matches!(reports[1].outcome, MountOutcome::Refused(_)),
            "the second twin must lose on NAME, not be reported Disabled too: {:?}",
            reports[1].outcome
        );
        // And exactly one namespace was registered — a second would have panicked
        // on an overlapping route, which is why this test builds the router at all.
        assert_eq!(
            get(&app, "/api/v1/twin/ping").await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn disabling_one_module_does_not_make_its_own_entry_look_like_a_typo() {
        // `provided` used to be the CONSTRUCTED subset, so switching Sin90 off
        // removed it from that set and its own `sin90: false` entry was then read
        // as a typo for a module the build does not have — degrading every OTHER
        // module. With one module the damage was invisible; with two it is not.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let cfg = config_from(r#"{"domainOs": {"alpha": {"enabled": false}}}"#);
        let (app, reports, _) = mount_all(
            &[
                entry(FakeModule::new("alpha")),
                entry(FakeModule::new("beta")),
            ],
            tmp.path(),
            &hub,
            Ok(&cfg),
            &no_models(),
            None,
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;

        assert_eq!(reports[0].outcome, MountOutcome::Disabled);
        assert_eq!(
            reports[1].outcome,
            MountOutcome::Mounted,
            "the OTHER module must be unaffected: {:?}",
            reports[1].outcome
        );
        assert_eq!(
            get(&app, "/api/v1/beta/ping").await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn an_invalid_catalogue_name_is_refused_without_construction() {
        // The catalogue names a module before any manifest exists, so nothing else
        // would have checked it — and the name becomes a URL segment and a
        // directory either way. An entry called `bad/name` would otherwise reach
        // `disabled_namespace` and try to mount a route no manifest could claim.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        for bad in ["bad/name", "", "Health", "../escape"] {
            let cat = vec![Installed {
                name: bad.to_owned(),
                version: "0.1.0".to_owned(),
                build: Build::InProcess(Box::new(|| panic!("must never be constructed"))),
            }];
            let (_, reports) = mount(&cat, tmp.path(), &hub).await;
            match &reports[0].outcome {
                MountOutcome::Refused(why) => {
                    assert!(why.contains("not a usable module name"), "{bad:?}: {why}")
                }
                other => panic!("{bad:?} must be refused, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_manifest_that_disagrees_with_its_catalogue_entry_is_refused() {
        // The catalogue and the manifest are two sources for one identity, so they
        // can diverge. A name mismatch would route the module under one identity
        // while it emitted events under another; a version mismatch would have
        // `agent24 os` state one version while another was running.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();

        let m = FakeModule::new("truthful");
        let wrong_name = Installed {
            name: "claimed".to_owned(),
            version: m.manifest().version().to_owned(),
            build: Build::InProcess(Box::new(move || Ok(m.clone() as Arc<dyn DomainModule>))),
        };
        let (_, reports) = mount(&[wrong_name], tmp.path(), &hub).await;
        match &reports[0].outcome {
            MountOutcome::Refused(why) => assert!(why.contains("manifest says"), "{why}"),
            other => panic!("expected Refused, got {other:?}"),
        }

        let m = FakeModule::new("truthful");
        let wrong_version = Installed {
            name: "truthful".to_owned(),
            version: "9.9.9".to_owned(),
            build: Build::InProcess(Box::new(move || Ok(m.clone() as Arc<dyn DomainModule>))),
        };
        let (_, reports) = mount(&[wrong_version], tmp.path(), &hub).await;
        match &reports[0].outcome {
            MountOutcome::Refused(why) => assert!(why.contains("version"), "{why}"),
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_module_that_failed_to_open_its_store_holds_no_grants() {
        // `granted` means what a LIVE module holds. This one got as far as having
        // its grants computed and then never received a KernelCtx, so reporting
        // them would say it holds capabilities it does not.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let bad = FakeModule::with("brokenstore", "in_process_crate", true);
        let (_, reports) = mount(&[entry(bad)], tmp.path(), &hub).await;
        assert!(matches!(reports[0].outcome, MountOutcome::Degraded(_)));
        assert!(reports[0].granted.is_empty());
    }

    // ---------- ME-2: declared resources ----------

    #[tokio::test]
    async fn a_missing_declared_model_is_reported_but_still_mounts() {
        // Reporting, not refusing. Most of a module's surface does not touch the
        // model; refusing to mount would turn a partial limitation into a total
        // outage, and the user would lose the working parts too.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        // APPENDED, not substituted: the base manifest has no `requires_models`
        // line at all, so a `.replace` is a silent no-op — which is exactly what
        // the first run of this test caught, by reporting Satisfied.
        let yaml = format!(
            "{}requires_models: [ornith-9b, absent-model]\n",
            manifest_yaml("hungry", "in_process_crate")
        );
        let m = FakeModule::from_yaml(&yaml, false);
        let inv = TestModels(Ok(vec!["ornith-9b".to_owned()]));
        let (app, reports, _) = mount_all(
            &[entry(m)],
            tmp.path(),
            &hub,
            Ok(&all_enabled()),
            &inv,
            None,
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;

        assert_eq!(reports[0].outcome, MountOutcome::Mounted);
        assert_eq!(
            reports[0].resources,
            ResourceStatus::MissingModels(vec!["absent-model".to_owned()]),
            "only the ACTUALLY missing one is named"
        );
        assert_eq!(
            get(&app, "/api/v1/hungry/ping").await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn an_unreachable_provider_is_not_reported_as_a_missing_model() {
        // "Install this model" and "start your provider" are different
        // instructions. Collapsing them sends the user hunting for a model they
        // already have.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let yaml = format!(
            "{}requires_models: [ornith-9b]\n",
            manifest_yaml("hungry", "in_process_crate")
        );
        let m = FakeModule::from_yaml(&yaml, false);
        let inv = TestModels(Err("provider timed out".to_owned()));
        let (_, reports, _) = mount_all(
            &[entry(m)],
            tmp.path(),
            &hub,
            Ok(&all_enabled()),
            &inv,
            None,
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;
        match &reports[0].resources {
            ResourceStatus::Unknown(why) => assert!(why.contains("timed out"), "{why}"),
            other => panic!("an unreachable provider must be Unknown, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_module_declaring_no_models_is_satisfied_even_with_no_providers() {
        // The common case must not be noisy: a module that needs nothing is fine
        // on a daemon with no providers at all.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let m = FakeModule::new("frugal");
        let (_, reports, _) = mount_all(
            &[entry(m)],
            tmp.path(),
            &hub,
            Ok(&all_enabled()),
            &TestModels(Err("nothing configured".to_owned())),
            None,
            Err("no process host in this test"),
            &test_approval_broker(&hub).await,
        )
        .await;
        assert_eq!(reports[0].resources, ResourceStatus::Satisfied);
    }

    #[tokio::test]
    async fn a_capability_the_kernel_cannot_serve_is_not_granted() {
        // The manifest asks for `scheduler`, which KERNEL_GRANTS does not include
        // because there is no handle to hand out yet. Granting it would be a lie.
        //
        // This used to use `memory` — until F1 gave memory a real handle, at which
        // point the kernel COULD serve it and the test was asserting the opposite
        // of the truth. The example has to be a capability that is still
        // unimplemented, or the test stops meaning anything.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let yaml = manifest_yaml("greedy", "in_process_crate").replace(
            "kernel_capabilities: [events]",
            "kernel_capabilities: [events, scheduler]",
        );
        let m = FakeModule::from_yaml(&yaml, false);
        let (_, reports) = mount(&[entry(m)], tmp.path(), &hub).await;
        assert_eq!(reports[0].granted, vec!["events".to_owned()]);
    }

    /// Judgement 21 (T7b/ME-3e): `RESERVED_KERNEL_SEGMENTS` really does
    /// block a module literally named `module-approvals` — the REST
    /// endpoints this design added — the same way it already blocks
    /// `health`/`approvals`/etc. Without this entry, such a module would
    /// panic the daemon at startup on the overlapping route rather than
    /// being refused.
    #[tokio::test]
    async fn module_approvals_is_a_reserved_kernel_segment() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let m = FakeModule::new("module-approvals");
        let (_, reports) = mount(&[entry(m)], tmp.path(), &hub).await;
        assert!(
            matches!(reports[0].outcome, MountOutcome::Refused(_)),
            "{:?}",
            reports[0]
        );
    }
}
