//! The kernel's domain-OS mounter (ME-1b; ADR-029).
//!
//! This is the half of the kernel↔domain-OS boundary that lives in the kernel:
//! it takes a list of [`DomainModule`]s, gives each one a directory and a
//! [`KernelCtx`], and nests its routes under a namespace DERIVED from its
//! manifest. The mounting LOGIC has no module-specific branch — that is the ME-1
//! acceptance — and the tests below mount fake modules rather than Sin90, so the
//! property cannot quietly become "the mounter happens to work for Sin90". (The
//! file does name `sin90` once, in [`RESERVED_KERNEL_SEGMENTS`], because the
//! kernel still hardcodes those routes; that entry disappears in ME-1b-b. Fake
//! modules give regression evidence, not proof that no special case exists.)
//!
//! Five rules the CONTRACT cannot enforce on its own, which therefore live here:
//!
//! 1. **Duplicate names are refused.** `DomainOsManifest::validate` checks ONE
//!    manifest for self-consistency; it cannot see the others. Two modules named
//!    `sin90` would collide on the directory, the route namespace AND the event
//!    module at once, so the name is reserved BEFORE the store is opened or any
//!    route is mounted — a later failure must not leave a half-mounted twin.
//! 2. **Out-of-process manifests are refused.** ME-3's transport does not exist;
//!    half-mounting a config we cannot honor is worse than refusing it.
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

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use agent24_domain::{Capability, DomainModule, EventBroadcast, EventSink, Grants, KernelCtx};
use agent24_protocol::EventBody;
use axum::Router;

/// What the kernel is willing to lend a domain OS today. A module may REQUEST
/// more in its manifest; [`Grants::granting`] intersects, so asking gains
/// nothing. The list grows as `KernelCtx` gains handles — `Models`, `Scheduler`,
/// `Policy` and `Memory` are deliberately absent because there is nothing to hand
/// out yet, and granting a capability with no handle would be a lie.
const KERNEL_GRANTS: &[Capability] = &[Capability::Events];

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
    "models",
    "runs",
    "schedules",
    "sessions",
    "shutdown",
    // TEMPORARY, and the set-equality test is what enforces it. Sin90's seven
    // routes are still HARDCODED in the kernel, which makes `sin90` a kernel
    // segment like any other — a module claiming it today would panic startup.
    // ME-1b-b deletes those routes; at that moment this entry becomes STALE and
    // `reserved_segments_match_the_kernel_routes_exactly` FAILS until it is
    // removed. Without that direction of the check, the kernel would quietly
    // refuse to mount its own first domain OS.
    "sin90",
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

/// The kernel context handed to one module.
struct DaemonCtx {
    sink: Option<EventSink>,
}

impl KernelCtx for DaemonCtx {
    fn events(&self) -> Option<&EventSink> {
        self.sink.as_ref()
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
    pub outcome: MountOutcome,
    /// What the kernel actually granted (the intersection of what the module
    /// asked for and [`KERNEL_GRANTS`]), as capability names.
    pub granted: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountOutcome {
    /// Routes are live.
    Mounted,
    /// Namespace serves 503: the module's directory could not be prepared, or
    /// its store failed to open. The kernel serves that 503, not the module.
    Degraded(String),
    /// Not mounted at all — NO routes, not even 503 ones: a duplicate name, a
    /// kernel-reserved name, or an out-of-process manifest.
    Refused(String),
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

/// Mount every module under `root`, returning the combined router and one report
/// per module.
///
/// The returned router is NOT authenticated — the caller must fold it into the
/// kernel router before applying the auth layer (rule 4 above). It is a
/// `Router<()>`: each module has already bound its own state.
pub async fn mount_all(
    modules: &[Arc<dyn DomainModule>],
    root: &Path,
    events: &crate::events::EventsHub,
) -> (Router, Vec<MountReport>) {
    let mut app = Router::new();
    let mut reports = Vec::new();
    let mut claimed: BTreeSet<String> = BTreeSet::new();

    for module in modules {
        let manifest = module.manifest();
        let name = manifest.name().to_owned();
        let namespace = manifest.route_namespace();
        let granted = Grants::granting(manifest.kernel_capabilities(), KERNEL_GRANTS);
        let granted_names: Vec<String> = granted.iter().map(|c| c.as_str().to_owned()).collect();

        let refuse = |why: String, reports: &mut Vec<MountReport>| {
            tracing::error!("domain OS {name:?} not mounted: {why}");
            reports.push(MountReport {
                name: name.clone(),
                namespace: namespace.clone(),
                outcome: MountOutcome::Refused(why),
                granted: granted_names.clone(),
            });
        };

        // Claim the name before `open_store`, route construction, or any
        // filesystem work, so a duplicate cannot open a store or leave routes
        // behind. (`manifest()` above already ran — it is the module's identity
        // and there is nothing to claim without it.) (This
        // loop is sequential — there is no race to lose; the point is ORDER, so
        // that a later rejection never has to undo work.) The claim is held for
        // the whole pass even by a REFUSED module: a name that was rejected once
        // stays rejected, rather than being silently handed to the next module
        // that asks for it.
        if !claimed.insert(name.clone()) {
            refuse(
                format!("another module already claims the name {name:?}"),
                &mut reports,
            );
            continue;
        }
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
        if !manifest.is_mountable_in_process() {
            refuse(
                "manifest declares an out-of-process provider; that transport does \
                 not exist yet (ME-3)"
                    .to_owned(),
                &mut reports,
            );
            continue;
        }

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
            let unavailable = {
                let m = name.clone();
                move || {
                    let m = m.clone();
                    async move { agent24_domain::http::module_unavailable(&m) }
                }
            };
            // `nest` covers the namespace root and its descendants, but NOT the
            // bare trailing slash: matchit's `{*rest}` does not match an empty
            // segment, so `/api/v1/<name>/` would fall through to the kernel's 404
            // — telling a client "no such endpoint" when the truth is "this module
            // is down". Route it explicitly. (`any`, not `get`: a degraded module
            // must answer the same way to every method, or a POST reads as a
            // missing endpoint and sends the caller rewriting a fine request.)
            app = app
                .nest(&namespace, Router::new().fallback(unavailable.clone()))
                .route(&format!("{namespace}/"), axum::routing::any(unavailable));
            reports.push(MountReport {
                name,
                namespace,
                outcome: MountOutcome::Degraded(why),
                granted: granted_names,
            });
            continue;
        }

        // The sink's EXISTENCE is the capability: build one only when events were
        // actually granted, and name it from the MANIFEST, never a local string.
        let sink = granted.has(Capability::Events).then(|| {
            EventSink::new(
                manifest,
                Arc::new(HubBroadcast(events.clone())) as Arc<dyn EventBroadcast>,
            )
        });
        let ctx: Arc<dyn KernelCtx> = Arc::new(DaemonCtx { sink });

        tracing::info!("domain OS {name:?} mounted at {namespace} (grants: {granted_names:?})");
        app = app.nest(&namespace, module.routes(ctx));
        reports.push(MountReport {
            name,
            namespace,
            outcome: MountOutcome::Mounted,
            granted: granted_names,
        });
    }

    (app, reports)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_domain::{DomainOsManifest, Result as DomainResult};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn manifest_yaml(name: &str, kind: &str) -> String {
        format!(
            "name: {name}\nversion: \"0.1.0\"\nroute_namespace: /api/v1/{name}\n\
             event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
             kernel_capabilities: [events]\nimpl_kind: {kind}\n"
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
                routes_built: std::sync::atomic::AtomicUsize::new(0),
            })
        }
        fn routes_built(&self) -> usize {
            self.routes_built.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn opened(&self) -> Option<std::path::PathBuf> {
            self.opened_in.lock().unwrap().clone()
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
        let (app, reports) =
            mount_all(&[m.clone() as Arc<dyn DomainModule>], tmp.path(), &hub).await;

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
        let (app, reports) = mount_all(
            &[
                a.clone() as Arc<dyn DomainModule>,
                b.clone() as Arc<dyn DomainModule>,
            ],
            tmp.path(),
            &hub,
        )
        .await;
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
        let (app, reports) = mount_all(
            &[
                first.clone() as Arc<dyn DomainModule>,
                second.clone() as Arc<dyn DomainModule>,
            ],
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

    #[tokio::test]
    async fn an_out_of_process_manifest_is_refused_not_half_mounted() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let m = FakeModule::with("remote", "out_of_process_provider", false);
        let (app, reports) =
            mount_all(&[m.clone() as Arc<dyn DomainModule>], tmp.path(), &hub).await;

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
        let (app, reports) = mount_all(
            &[bad as Arc<dyn DomainModule>, good as Arc<dyn DomainModule>],
            tmp.path(),
            &hub,
        )
        .await;

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
        let (app, _) = mount_all(&[m as Arc<dyn DomainModule>], tmp.path(), &hub).await;

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
        let (modules, reports) = mount_all(&[m as Arc<dyn DomainModule>], tmp.path(), &hub).await;
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
    /// and much quieter: when ME-1b-b deletes Sin90's seven hardcoded routes,
    /// `"sin90"` left in this list would make the kernel refuse to mount its own
    /// first domain OS — and a one-directional "every kernel segment is reserved"
    /// check would stay green through it. So this asserts SET EQUALITY, which
    /// makes that deletion fail here until the entry is removed too.
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
        let (modules, _) =
            mount_all(&[bad.clone() as Arc<dyn DomainModule>], tmp.path(), &hub).await;

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
        let (app, reports) =
            mount_all(&[m.clone() as Arc<dyn DomainModule>], tmp.path(), &hub).await;

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
        let (app, reports) = mount_all(&[m as Arc<dyn DomainModule>], tmp.path(), &hub).await;

        assert_eq!(reports[0].outcome, MountOutcome::Mounted);
        assert!(reports[0].granted.is_empty());
        let j = body_json(get(&app, "/api/v1/quiet/ping").await).await;
        assert_eq!(j["events"], false, "no grant means no handle at all");
        assert_eq!(j["sink_module"], serde_json::Value::Null);
        assert!(rx.try_recv().is_err(), "and nothing reached the hub");
    }

    #[tokio::test]
    async fn a_capability_the_kernel_cannot_serve_is_not_granted() {
        // The manifest asks for `memory`, which KERNEL_GRANTS does not include
        // because there is no handle to hand out yet. Granting it would be a lie.
        let tmp = tempfile::tempdir().unwrap();
        let hub = crate::events::EventsHub::default();
        let yaml = manifest_yaml("greedy", "in_process_crate").replace(
            "kernel_capabilities: [events]",
            "kernel_capabilities: [events, memory]",
        );
        let m = FakeModule::from_yaml(&yaml, false);
        let (_, reports) = mount_all(&[m as Arc<dyn DomainModule>], tmp.path(), &hub).await;
        assert_eq!(reports[0].granted, vec!["events".to_owned()]);
    }
}
