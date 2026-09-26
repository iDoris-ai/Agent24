//! `_a24/memory/private/{remember,recall,recent}` — the three JSON-RPC
//! `Handler`s an out-of-process module actually calls to use the memory
//! capability it may have been granted at mount time (T8.5c-W-wire / ME-3d).
//! See `docs/design/T8.5c-W-wire.md` (v5, frozen).
//!
//! `_a24/memory/scoped/*` is deliberately **not** registered anywhere in this
//! module or `crate::domain` — see the design doc's decision W6. That is the
//! whole implementation of W6: three method names this crate never puts into
//! a `Methods` value, so `dispatch()`'s existing "no such method" path
//! produces `-32601` for them, matching
//! `docs/specs/SPEC-ME3-OUT-OF-PROCESS.md`'s decision 2 ("未实现时返回稳定的
//! `method not found`，不是 fallback 到 `private/*`").

use std::sync::Arc;

use agent24_domain::memory::Remember;
use agent24_os_proto::drain::Generation;
use agent24_os_proto::rpc::{CallFuture, ErrorKind, Handler, RpcError};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::events_emit::refused_error;
use crate::os_memory::MemoryEntitlement;
use crate::os_memory_page::{MemoryRpcError, Needle};

/// `_a24/memory/private/remember`'s params. `deny_unknown_fields`:
/// `SPEC-ME3-OUT-OF-PROCESS.md:185` MUST — `private/*` accepts no lease field
/// at all, so a request carrying `lease`/`space`/`scope` must fail at
/// `check_params`, before `Handler::call()` ever runs, not be silently
/// ignored. `_meta` is the one deliberately permissive escape hatch
/// (ADR-031): it is never read by anything in this module, so a value placed
/// there — including `org`/`space`/`lease` — has no effect on this call,
/// which is the judgement §5.1's negative test pins.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryRememberParams {
    kind: String,
    body: Map<String, Value>,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    _meta: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryRecallParams {
    query: String,
    page_size: usize,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    _meta: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryRecentParams {
    page_size: usize,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    _meta: Option<Map<String, Value>>,
}

/// Shared by all three handlers, one built fresh every time this
/// generation's `MethodsFor` closure runs — same shape as
/// `EventsEmitHandler`. `entitlement` carries `T8.5c-W-mount`'s
/// `PrivateMemoryHandle` (memory/limiter/admission) inside it whenever this
/// module was actually granted one; `entitlement.private_handle()` is the
/// only place any of the three `call()` implementations decide `forbidden`
/// (mount design §2.1) — never `Capability::Memory` on its own, which mount
/// decision 2 explicitly does not promise a usable handle.
pub struct RememberHandler {
    pub generation: Arc<Generation>,
    pub entitlement: MemoryEntitlement,
}

pub struct RecallHandler {
    pub generation: Arc<Generation>,
    pub entitlement: MemoryEntitlement,
}

pub struct RecentHandler {
    pub generation: Arc<Generation>,
    pub entitlement: MemoryEntitlement,
}

/// `entitlement.private_handle()` is `None` — the forbidden response every
/// one of the three handlers gives, worded identically because the caller
/// cannot distinguish "capability withheld" from "lend() failed" from
/// "ephemeral admission absent" (mount design §2.1's table), and should not
/// be able to.
fn forbidden() -> RpcError {
    RpcError::application(
        ErrorKind::Forbidden,
        "this module was not granted a private memory handle",
    )
}

impl Handler for RememberHandler {
    fn check_params(&self, params: &Value) -> Result<(), String> {
        serde_json::from_value::<MemoryRememberParams>(params.clone())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn call(&self, params: Value) -> CallFuture {
        let parsed = serde_json::from_value::<MemoryRememberParams>(params);
        let entitlement = self.entitlement.clone();
        let generation = self.generation.clone();
        Box::pin(async move {
            let parsed = parsed.map_err(|e| {
                RpcError::internal(format!(
                    "params valid at check_params but not at call(): {e}"
                ))
            })?;

            let Some(handle) = entitlement.private_handle() else {
                return Err(forbidden());
            };

            let lifecycle = match generation.admit_callback_bound(parsed.request_id.as_deref()) {
                Ok(lifecycle) => lifecycle,
                Err(refused) => return Err(refused_error(refused)),
            };

            let what = Remember {
                kind: parsed.kind,
                body: parsed.body,
            };
            let remembered = handle
                .memory
                .remember_checked(
                    lifecycle,
                    handle.limiter.clone(),
                    handle.admission.clone(),
                    what,
                )
                .await
                .map_err(MemoryRpcError::into_rpc_error)?;

            serde_json::to_value(remembered)
                .map_err(|e| RpcError::internal(format!("result not serialisable: {e}")))
        })
    }
}

impl Handler for RecallHandler {
    fn check_params(&self, params: &Value) -> Result<(), String> {
        serde_json::from_value::<MemoryRecallParams>(params.clone())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn call(&self, params: Value) -> CallFuture {
        let parsed = serde_json::from_value::<MemoryRecallParams>(params);
        let entitlement = self.entitlement.clone();
        let generation = self.generation.clone();
        Box::pin(async move {
            let parsed = parsed.map_err(|e| {
                RpcError::internal(format!(
                    "params valid at check_params but not at call(): {e}"
                ))
            })?;

            let Some(handle) = entitlement.private_handle() else {
                return Err(forbidden());
            };

            let lifecycle = match generation.admit_callback_bound(parsed.request_id.as_deref()) {
                Ok(lifecycle) => lifecycle,
                Err(refused) => return Err(refused_error(refused)),
            };

            let needle = Needle::normalize(&parsed.query);
            let page = handle
                .memory
                .recall_page(
                    lifecycle,
                    handle.limiter.clone(),
                    handle.admission.clone(),
                    &needle,
                    parsed.page_size,
                    parsed.cursor.as_deref(),
                )
                .await
                .map_err(MemoryRpcError::into_rpc_error)?;

            serde_json::to_value(page)
                .map_err(|e| RpcError::internal(format!("result not serialisable: {e}")))
        })
    }
}

impl Handler for RecentHandler {
    fn check_params(&self, params: &Value) -> Result<(), String> {
        serde_json::from_value::<MemoryRecentParams>(params.clone())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn call(&self, params: Value) -> CallFuture {
        let parsed = serde_json::from_value::<MemoryRecentParams>(params);
        let entitlement = self.entitlement.clone();
        let generation = self.generation.clone();
        Box::pin(async move {
            let parsed = parsed.map_err(|e| {
                RpcError::internal(format!(
                    "params valid at check_params but not at call(): {e}"
                ))
            })?;

            let Some(handle) = entitlement.private_handle() else {
                return Err(forbidden());
            };

            let lifecycle = match generation.admit_callback_bound(parsed.request_id.as_deref()) {
                Ok(lifecycle) => lifecycle,
                Err(refused) => return Err(refused_error(refused)),
            };

            let page = handle
                .memory
                .recent_page(
                    lifecycle,
                    handle.limiter.clone(),
                    handle.admission.clone(),
                    parsed.page_size,
                    parsed.cursor.as_deref(),
                )
                .await
                .map_err(MemoryRpcError::into_rpc_error)?;

            serde_json::to_value(page)
                .map_err(|e| RpcError::internal(format!("result not serialisable: {e}")))
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_domain::DomainOsManifest;
    use agent24_os_proto::drain::Generation;
    use agent24_os_proto::rpc::{Dispatch, Methods, code};
    use serde_json::json;
    use tokio::sync::Semaphore;

    use crate::os_memory::{
        OrgId, OsMemoryCatalog, OsScopedMemory, build_private_memory_entitlement,
    };
    use crate::os_memory_page::MEMORY_MAX_PAGE_SIZE;

    fn manifest(name: &str) -> DomainOsManifest {
        DomainOsManifest::from_yaml(&format!(
            "name: {name}\nversion: \"0.1.0\"\nroute_namespace: /api/v1/{name}\n\
             event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
             kernel_capabilities: [memory]\nimpl_kind: in_process_crate\n"
        ))
        .unwrap()
    }

    async fn org_of(kv: &agent24_memory::KvStore, user: &str) -> OrgId {
        OrgId::from_store(kv.ensure_org_for_user(user).await.unwrap())
    }

    /// A real, mounted `MemoryEntitlement` — same construction path
    /// `mount_package` uses (T8.5c-W-mount decision 3/4), backed by a real
    /// `OsScopedMemory` over an in-memory `KvStore`. Not a mock: the three
    /// Handlers below drive this through their real `call()`, all the way
    /// to a real `EventLog`.
    async fn granted_entitlement(
        kv: &agent24_memory::KvStore,
        user: &str,
        name: &str,
    ) -> MemoryEntitlement {
        let cat = OsMemoryCatalog::default();
        let org = org_of(kv, user).await;
        let p = cat
            .ensure_recorded(&org, user, &manifest(name), kv)
            .await
            .unwrap();
        let memory = Arc::new(OsScopedMemory::new(&p, kv));
        build_private_memory_entitlement(Some((memory, Arc::new(Semaphore::new(4)))))
    }

    fn running_generation() -> Arc<Generation> {
        let g = Generation::serving_at("/tmp/does-not-need-to-exist".into());
        assert!(g.ready(), "a freshly-serving generation must become Ready");
        g
    }

    // ── judgement 1: `deny_unknown_fields` rejects any lease field ──────

    #[test]
    fn a_lease_field_on_any_of_the_three_private_methods_is_rejected_at_check_params() {
        let g = running_generation();
        let remember = RememberHandler {
            generation: g.clone(),
            entitlement: MemoryEntitlement::NONE,
        };
        let recall = RecallHandler {
            generation: g.clone(),
            entitlement: MemoryEntitlement::NONE,
        };
        let recent = RecentHandler {
            generation: g,
            entitlement: MemoryEntitlement::NONE,
        };

        for field in ["lease", "space", "scope"] {
            let mut remember_params = json!({"kind": "note", "body": {}});
            remember_params[field] = json!("sneaky");
            assert!(
                remember.check_params(&remember_params).is_err(),
                "remember must reject a {field:?} field — SPEC-ME3-OUT-OF-PROCESS.md:185 MUST"
            );

            let mut recall_params = json!({"query": "", "page_size": 1});
            recall_params[field] = json!("sneaky");
            assert!(
                recall.check_params(&recall_params).is_err(),
                "recall must reject {field:?}"
            );

            let mut recent_params = json!({"page_size": 1});
            recent_params[field] = json!("sneaky");
            assert!(
                recent.check_params(&recent_params).is_err(),
                "recent must reject {field:?}"
            );
        }
    }

    // ── judgement 2: `_meta` is accepted and inert ──────────────────────

    #[tokio::test]
    async fn a_legitimate_meta_field_is_accepted_and_has_no_effect_on_the_call() {
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let entitlement = granted_entitlement(&kv, "alice", "sin90").await;
        let h = RememberHandler {
            generation: running_generation(),
            entitlement,
        };

        let clean = json!({"kind": "note", "body": {}});
        assert!(h.check_params(&clean).is_ok());
        let clean_result = h.call(clean).await.unwrap();

        // Negative control: `_meta` carrying exactly the fields a lease
        // would use must produce the SAME outcome as no `_meta` at all —
        // nothing in `RememberHandler::call` ever reads `parsed._meta`.
        let with_meta = json!({
            "kind": "note", "body": {},
            "_meta": {"org": "attacker-org", "space": "attacker-space", "lease": "forged"},
        });
        assert!(
            h.check_params(&with_meta).is_ok(),
            "_meta's own contents must stay unchecked"
        );
        let with_meta_result = h.call(with_meta).await.unwrap();

        // Both calls wrote a real, distinct memory (same partition) — the
        // shape of the two responses (not their exact ids/timestamps) must
        // agree: both succeeded, neither was rejected or redirected.
        assert!(clean_result.get("id").is_some());
        assert!(with_meta_result.get("id").is_some());
    }

    // ── judgement 4 (W6): `scoped/*` is never registered → `-32601` ─────

    #[tokio::test]
    async fn scoped_methods_are_never_registered_private_methods_are() {
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let entitlement = granted_entitlement(&kv, "alice", "sin90").await;
        let g = running_generation();
        let methods = Methods::none()
            .with(
                "_a24/memory/private/remember",
                Arc::new(RememberHandler {
                    generation: g.clone(),
                    entitlement: entitlement.clone(),
                }),
            )
            .with(
                "_a24/memory/private/recall",
                Arc::new(RecallHandler {
                    generation: g.clone(),
                    entitlement: entitlement.clone(),
                }),
            )
            .with(
                "_a24/memory/private/recent",
                Arc::new(RecentHandler {
                    generation: g,
                    entitlement,
                }),
            );

        for method in [
            "_a24/memory/scoped/remember",
            "_a24/memory/scoped/recall",
            "_a24/memory/scoped/recent",
            "_a24/memory/scoped/forget", // never even a planned name
        ] {
            let frame = serde_json::to_vec(&json!({
                "jsonrpc": "2.0", "id": "1", "method": method, "params": {},
            }))
            .unwrap();
            let Dispatch::Respond(r) =
                agent24_os_proto::rpc::dispatch(&frame, &methods, &|_| false)
            else {
                panic!("{method}: an unregistered method must never reach a Handler::call()");
            };
            assert_eq!(
                r.outcome.unwrap_err().code,
                code::METHOD_NOT_FOUND,
                "{method} must be -32601, not a registered-but-forbidden handler"
            );
        }

        // Positive control: the three PRIVATE names really are registered.
        for method in [
            "_a24/memory/private/remember",
            "_a24/memory/private/recall",
            "_a24/memory/private/recent",
        ] {
            let frame = serde_json::to_vec(&json!({
                "jsonrpc": "2.0", "id": "1", "method": method, "params": {},
            }))
            .unwrap();
            let Dispatch::Respond(r) =
                agent24_os_proto::rpc::dispatch(&frame, &methods, &|_| false)
            else {
                continue; // a Call variant means it WAS found — also fine
            };
            assert_ne!(
                r.outcome.unwrap_err().code,
                code::METHOD_NOT_FOUND,
                "{method} is registered — a malformed empty `params` must fail at \
                 check_params (-32602), never -32601"
            );
        }
    }

    // ── judgement 5: forbidden vs. a real success, on live entitlement ──

    #[tokio::test]
    async fn no_handle_is_forbidden_a_real_handle_succeeds() {
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();

        let ungranted = RememberHandler {
            generation: running_generation(),
            entitlement: MemoryEntitlement::NONE,
        };
        let err = ungranted
            .call(json!({"kind": "note", "body": {}}))
            .await
            .unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::Forbidden));

        let granted = RememberHandler {
            generation: running_generation(),
            entitlement: granted_entitlement(&kv, "alice", "sin90").await,
        };
        let ok = granted
            .call(json!({"kind": "note", "body": {}}))
            .await
            .unwrap();
        assert!(
            ok.get("id").is_some(),
            "a real handle must really write a memory: {ok}"
        );
    }

    // ── judgement 6 (partial — Revoked's own state machine is covered by
    // agent24-os-proto::drain's admit_callback_bound tests; a Revoked
    // Generation cannot be constructed from this crate, `revoke()` is
    // pub(crate) to agent24-os-proto, same limitation `events_emit.rs`'s
    // own draining tests already document) ──

    #[tokio::test]
    async fn all_three_handlers_reject_a_draining_call_with_no_request_id_and_admit_one_with_a_live_id()
     {
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let entitlement = granted_entitlement(&kv, "alice", "sin90").await;
        // Admitted while still Running — `begin_drain` stops admitting NEW
        // requests, it does not evict ones already in flight.
        let g = running_generation();
        let live = g
            .admit_request(
                "r1".to_owned(),
                [0u8; 32],
                std::time::Instant::now(),
                std::time::Duration::from_secs(30),
            )
            .unwrap();
        assert!(g.begin_drain(
            std::time::Instant::now(),
            std::time::Duration::from_secs(10)
        ));

        // Codex review round 1 (M2): the frozen design requires this matrix
        // for EACH of the three handlers independently, not just
        // `RememberHandler` — a copy-paste regression in `RecallHandler`'s
        // or `RecentHandler`'s `call()` (e.g. forgetting to wire
        // `admit_callback_bound` at all) must fail a test named after it.
        let cases: [(
            &str,
            serde_json::Value,
            serde_json::Value,
            serde_json::Value,
        ); 3] = [
            (
                "remember",
                json!({"kind": "note", "body": {}}),
                json!({"kind": "note", "body": {}, "request_id": "ghost"}),
                json!({"kind": "note", "body": {}, "request_id": "r1"}),
            ),
            (
                "recall",
                json!({"query": "", "page_size": 1}),
                json!({"query": "", "page_size": 1, "request_id": "ghost"}),
                json!({"query": "", "page_size": 1, "request_id": "r1"}),
            ),
            (
                "recent",
                json!({"page_size": 1}),
                json!({"page_size": 1, "request_id": "ghost"}),
                json!({"page_size": 1, "request_id": "r1"}),
            ),
        ];
        for (label, params_no_id, params_unknown_id, params_live_id) in cases {
            // No request_id at all: must refuse — no fallback to "just run
            // it anyway".
            let err = match label {
                "remember" => {
                    RememberHandler {
                        generation: g.clone(),
                        entitlement: entitlement.clone(),
                    }
                    .call(params_no_id.clone())
                    .await
                }
                "recall" => {
                    RecallHandler {
                        generation: g.clone(),
                        entitlement: entitlement.clone(),
                    }
                    .call(params_no_id.clone())
                    .await
                }
                _ => {
                    RecentHandler {
                        generation: g.clone(),
                        entitlement: entitlement.clone(),
                    }
                    .call(params_no_id.clone())
                    .await
                }
            }
            .unwrap_err();
            assert_eq!(
                err.kind,
                Some(ErrorKind::Draining),
                "{label}: no request_id"
            );

            // An id that is not in flight: same refusal (both fold into
            // `ErrorKind::Draining` — `DrainingWithoutRequest` vs
            // `DrainingUnknownRequest` are distinguished at the
            // `CallbackRefused` level, not the wire level).
            let err = match label {
                "remember" => {
                    RememberHandler {
                        generation: g.clone(),
                        entitlement: entitlement.clone(),
                    }
                    .call(params_unknown_id.clone())
                    .await
                }
                "recall" => {
                    RecallHandler {
                        generation: g.clone(),
                        entitlement: entitlement.clone(),
                    }
                    .call(params_unknown_id.clone())
                    .await
                }
                _ => {
                    RecentHandler {
                        generation: g.clone(),
                        entitlement: entitlement.clone(),
                    }
                    .call(params_unknown_id.clone())
                    .await
                }
            }
            .unwrap_err();
            assert_eq!(
                err.kind,
                Some(ErrorKind::Draining),
                "{label}: unknown request_id"
            );

            // A real, still-in-flight request_id: draining must still admit
            // it (background work bound to a live request is exactly what
            // draining keeps serving).
            let ok = match label {
                "remember" => {
                    RememberHandler {
                        generation: g.clone(),
                        entitlement: entitlement.clone(),
                    }
                    .call(params_live_id.clone())
                    .await
                }
                "recall" => {
                    RecallHandler {
                        generation: g.clone(),
                        entitlement: entitlement.clone(),
                    }
                    .call(params_live_id.clone())
                    .await
                }
                _ => {
                    RecentHandler {
                        generation: g.clone(),
                        entitlement: entitlement.clone(),
                    }
                    .call(params_live_id.clone())
                    .await
                }
            }
            .unwrap_or_else(|e| panic!("{label}: live request_id must be admitted: {e:?}"));
            assert!(
                ok.is_object(),
                "{label}: a genuinely admitted call must really execute and return a result"
            );
        }
        drop(live);
    }

    // ── judgement 10: invalid params must NOT be over-sanitized ─────────

    #[tokio::test]
    async fn an_out_of_range_page_size_states_the_real_bound_not_a_generic_message() {
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let entitlement = granted_entitlement(&kv, "alice", "sin90").await;
        let h = RecallHandler {
            generation: running_generation(),
            entitlement,
        };
        let err = h
            .call(json!({"query": "", "page_size": MEMORY_MAX_PAGE_SIZE + 1}))
            .await
            .unwrap_err();
        assert_eq!(err.code, code::INVALID_PARAMS);
        assert!(
            err.message.contains(&MEMORY_MAX_PAGE_SIZE.to_string()),
            "the caller needs the real bound to fix their call, not a blanket \
             'invalid input': got {:?}",
            err.message
        );
    }

    // ── J-S7: SDK wire parity (ME4-S3 §6) ───────────────────────────────
    //
    // The SDK's `MemoryClient` runs against `agent24_os_sdk::testing::
    // fake_kernel`; the fake kernel's peer hands the raw params straight to
    // THIS module's real `RememberHandler`/`RecallHandler::call` (the same
    // construction `granted_entitlement` gives the tests above), and the
    // handler's own result is fed back for the SDK to parse.

    async fn respond_rpc_result(
        peer: &mut agent24_os_sdk::testing::FakePeer,
        req: &Value,
        result: Result<Value, RpcError>,
    ) {
        match result {
            Ok(v) => agent24_os_sdk::testing::respond(peer, req, v).await,
            Err(e) => {
                let mut data = e.data.clone().unwrap_or_default();
                if let Some(kind) = e.kind {
                    data.insert("kind".to_owned(), Value::String(kind.as_str().to_owned()));
                }
                if data.is_empty() {
                    agent24_os_sdk::testing::respond_error(
                        peer,
                        req,
                        i64::from(e.code),
                        "",
                        &e.message,
                    )
                    .await;
                } else {
                    agent24_os_sdk::testing::respond_error_with_data(
                        peer,
                        req,
                        i64::from(e.code),
                        &e.message,
                        Value::Object(data),
                    )
                    .await;
                }
            }
        }
    }

    #[tokio::test]
    async fn sdk_wire_parity_memory_remember_then_recall() {
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let entitlement = granted_entitlement(&kv, "alice", "sin90").await;
        let remember_handler = RememberHandler {
            generation: running_generation(),
            entitlement: entitlement.clone(),
        };
        let recall_handler = RecallHandler {
            generation: running_generation(),
            entitlement,
        };

        let (conn, mut peer) =
            agent24_os_sdk::testing::fake_kernel(vec!["_a24/memory/private/".to_owned()]).await;
        let client = agent24_os_sdk::MemoryClient::new(&conn).expect("offer covers memory");
        let mut body = Map::new();
        body.insert("text".to_owned(), json!("hello from the SDK"));
        let request_id = agent24_os_sdk::RequestId::for_test("req-1");

        let (remembered, ()) =
            tokio::join!(client.remember("note", body, Some(&request_id)), async {
                let req = agent24_os_sdk::testing::read_request(&mut peer).await;
                let result = remember_handler.call(req["params"].clone()).await;
                respond_rpc_result(&mut peer, &req, result).await;
            });
        let remembered = remembered.expect("SDK remember must succeed against the real handler");

        let (recalled, ()) =
            tokio::join!(client.recall("hello", 10, None, Some(&request_id)), async {
                let req = agent24_os_sdk::testing::read_request(&mut peer).await;
                let result = recall_handler.call(req["params"].clone()).await;
                respond_rpc_result(&mut peer, &req, result).await;
            });
        let page = recalled.expect("SDK recall must succeed against the real handler");
        assert!(
            page.items.iter().any(|it| it.id == remembered.id),
            "the memory just written must be recallable through the same real handler"
        );
    }
}

#[cfg(test)]
mod real_resource_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::pin::Pin;
    use std::str::FromStr as _;
    use std::time::{Duration, Instant};

    use serde_json::json;
    use sqlx::Connection as _;

    use agent24_domain::memory::ScopedMemory as _;

    use super::*;
    use crate::os_memory::{
        OrgId, OsMemoryCatalog, OsScopedMemory, build_private_memory_entitlement,
    };

    fn manifest(name: &str) -> agent24_domain::DomainOsManifest {
        agent24_domain::DomainOsManifest::from_yaml(&format!(
            "name: {name}\nversion: \"0.1.0\"\nroute_namespace: /api/v1/{name}\n\
             event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
             kernel_capabilities: [memory]\nimpl_kind: in_process_crate\n"
        ))
        .unwrap()
    }

    async fn org_of(kv: &agent24_memory::KvStore, user: &str) -> OrgId {
        OrgId::from_store(kv.ensure_org_for_user(user).await.unwrap())
    }

    /// A `MemoryEntitlement` built with the SAME real `Arc<Semaphore>` a real
    /// mount would hand it — `kv.oop_admission()`, not a hand-rolled
    /// `Semaphore` — so "the daemon's one shared permit" in these tests is
    /// literally the same value T8.5c-W-mount's own construction path
    /// (`server.rs`/`KvStore::open`) produces, not a stand-in.
    async fn granted_entitlement_real_admission(
        kv: &agent24_memory::KvStore,
        user: &str,
        name: &str,
    ) -> MemoryEntitlement {
        let cat = OsMemoryCatalog::default();
        let org = org_of(kv, user).await;
        let p = cat
            .ensure_recorded(&org, user, &manifest(name), kv)
            .await
            .unwrap();
        let memory = Arc::new(OsScopedMemory::new(&p, kv));
        let admission = kv
            .oop_admission()
            .expect("a file-backed KvStore must carry real admission (T8.5c-W-mount decision 4)");
        build_private_memory_entitlement(Some((memory, admission)))
    }

    fn running_generation() -> Arc<Generation> {
        let g = Generation::serving_at("/tmp/does-not-need-to-exist".into());
        assert!(g.ready());
        g
    }

    struct NotifyFirstPending<F> {
        inner: F,
        notify: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl<F: std::future::Future + Unpin> std::future::Future for NotifyFirstPending<F> {
        type Output = F::Output;
        fn poll(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            match Pin::new(&mut self.inner).poll(cx) {
                std::task::Poll::Pending => {
                    if let Some(tx) = self.notify.take() {
                        let _ = tx.send(());
                    }
                    std::task::Poll::Pending
                }
                ready => ready,
            }
        }
    }

    /// Same technique `os_memory.rs`'s own judgement 9b test uses: return
    /// only once `fut` has genuinely been polled to `Pending` at least once
    /// — the caller's next assertion is checking a fact that already
    /// happened, not racing a `sleep` against the scheduler.
    async fn spawn_and_confirm_blocked<T: Send + 'static>(
        fut: Pin<Box<dyn std::future::Future<Output = T> + Send>>,
    ) -> tokio::task::JoinHandle<T> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(NotifyFirstPending {
            inner: fut,
            notify: Some(tx),
        });
        rx.await
            .expect("the call must have blocked at least once before finishing this fast");
        task
    }

    /// A real external writer, holding a real SQLite write lock via `BEGIN
    /// IMMEDIATE` — the same lock-contention source
    /// `os_memory.rs`'s own `judgement_9b_real_sqlite_connections_...` test
    /// uses. Callers must finish all of THEIR OWN setup writes against `kv`
    /// (e.g. `ensure_recorded`, which needs a write) before calling this —
    /// taking the lock first deadlocks a caller's own setup against itself
    /// (`SQLITE_BUSY`/`database is locked`, not a hang: this pool's
    /// `busy_timeout` is 5s, so a caller ordering it wrong sees a fast,
    /// loud failure rather than a slow one).
    async fn take_external_writer_lock(
        db_path: &std::path::Path,
    ) -> sqlx::sqlite::SqliteConnection {
        let mut lock_conn = sqlx::sqlite::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::from_str(&format!(
                "sqlite://{}",
                db_path.display()
            ))
            .unwrap(),
        )
        .await
        .unwrap();
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut lock_conn)
            .await
            .unwrap();
        lock_conn
    }

    /// ★ Judgement 7 (mount §7.2/§11, P judgement 9b's model — mandatory, not
    /// optional): two DIFFERENT real modules' `Handler::call()` really do
    /// share ONE daemon-level admission permit. The cross-module WIRING
    /// itself (`mount_package`'s OOP branch handing every module the
    /// identical `Arc<Semaphore>`) is what T8.5c-W-mount's own judgement 4
    /// already proved — this test's job is narrower and specific to what
    /// THIS document adds: prove `Handler::call()` itself really acquires
    /// and respects that one real permit end to end (real JSON params → real
    /// entitlement check → real `admit_callback_bound` → real
    /// `OsScopedMemory` → a real, file-backed SQLite pool), for two
    /// genuinely different modules, not just that the OsScopedMemory-level
    /// primitives do (already shown by `os_memory.rs`'s own judgement 9b).
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn judgement_7_two_real_modules_handler_call_shares_one_daemon_admission_permit() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("m.db");
        let kv = agent24_memory::KvStore::open(&db_path).await.unwrap();

        // Setup writes (`ensure_recorded`) happen BEFORE the external lock is
        // taken — see `take_external_writer_lock`'s doc comment.
        let module_a = granted_entitlement_real_admission(&kv, "alice", "sin90").await;
        let module_b = granted_entitlement_real_admission(&kv, "alice", "cos72").await;
        let mut lock_conn = take_external_writer_lock(&db_path).await;

        // `max_connections(5) - 1 = 4`: exactly headroom design §6.5's MUST
        // contract requires — real, not asserted by hand.
        assert_eq!(kv.oop_admission().unwrap().available_permits(), 4);

        // Module A occupies all 4 real permits via 4 real
        // `Handler::call()`s, each genuinely blocked on the external writer
        // lock (not a stand-in: this really holds a pool connection AND an
        // admission permit at once).
        let mut write_tasks = Vec::new();
        for _ in 0..4u32 {
            let h = RememberHandler {
                generation: running_generation(),
                entitlement: module_a.clone(),
            };
            write_tasks.push(
                spawn_and_confirm_blocked(Box::pin(async move {
                    h.call(json!({"kind": "note", "body": {}})).await
                }))
                .await,
            );
        }

        // Headroom: a real in-process read (bypasses admission entirely —
        // `ScopedMemory::recall`, not the OOP `recall_page`) still succeeds
        // — the write lock does not block reads under WAL, and admission
        // being fully occupied does not touch this path at all. Reuses
        // module A's already-`ensure_recorded`ed partition — a fresh
        // `ensure_recorded` call here would itself be a write, and the
        // external lock is already held by this point.
        assert!(
            module_a
                .private_handle()
                .unwrap()
                .memory
                .recall("", 1)
                .await
                .is_ok(),
            "an in-process read must not be starved by 4 occupied OOP admission \
             permits or by the external write lock"
        );

        // Module B — a DIFFERENT module — probes with a READ
        // (`recall_page`, not `remember_checked`): a read never touches the
        // write lock, so if it blocks, the ONLY thing it can be blocked on
        // is `Semaphore::acquire_owned()` itself — i.e. the SAME permit
        // module A just exhausted. If B had an independent `Semaphore`,
        // this call would succeed immediately (pool has a free connection,
        // reads don't need the write lock).
        let probe = RecallHandler {
            generation: running_generation(),
            entitlement: module_b.clone(),
        };
        let probe_task = spawn_and_confirm_blocked(Box::pin(async move {
            probe.call(json!({"query": "", "page_size": 1})).await
        }))
        .await;
        assert!(
            !probe_task.is_finished(),
            "module B's real Handler::call() must be blocked on the SHARED \
             admission permit — if it finished, B is not sharing module A's \
             semaphore"
        );

        sqlx::query("ROLLBACK")
            .execute(&mut lock_conn)
            .await
            .unwrap();
        for t in write_tasks {
            t.await.unwrap().unwrap();
        }
        let result = tokio::time::timeout(Duration::from_secs(5), probe_task)
            .await
            .expect("module B's call must complete once a permit frees up")
            .unwrap();
        assert!(result.is_ok(), "{result:?}");

        // Codex review round 1 (M1): the frozen criterion names BOTH read
        // methods (`recall_page`/`recent_page`), not just one — a second
        // round, same technique, with `RecentHandler` this time.
        let mut lock_conn = take_external_writer_lock(&db_path).await;
        let mut write_tasks = Vec::new();
        for _ in 0..4u32 {
            let h = RememberHandler {
                generation: running_generation(),
                entitlement: module_a.clone(),
            };
            write_tasks.push(
                spawn_and_confirm_blocked(Box::pin(async move {
                    h.call(json!({"kind": "note", "body": {}})).await
                }))
                .await,
            );
        }
        let recent_probe = RecentHandler {
            generation: running_generation(),
            entitlement: module_b.clone(),
        };
        let recent_probe_task = spawn_and_confirm_blocked(Box::pin(async move {
            recent_probe.call(json!({"page_size": 1})).await
        }))
        .await;
        assert!(
            !recent_probe_task.is_finished(),
            "module B's RecentHandler must also be blocked on the shared permit"
        );
        sqlx::query("ROLLBACK")
            .execute(&mut lock_conn)
            .await
            .unwrap();
        for t in write_tasks {
            t.await.unwrap().unwrap();
        }
        let recent_result = tokio::time::timeout(Duration::from_secs(5), recent_probe_task)
            .await
            .expect("must complete once a permit frees up")
            .unwrap();
        assert!(recent_result.is_ok(), "{recent_result:?}");
    }

    /// Judgement 8a: a real `Handler::call()` genuinely queued on the
    /// exhausted shared admission permit (not merely "about to call
    /// `acquire_owned()`" — `spawn_and_confirm_blocked` proves it already
    /// has) is cancelled when the real request it is bound to ends —
    /// exactly the point this document exists to prove: this chain has
    /// never, before T8.5c-W-wire, had a real async business operation to
    /// drive it (mount/P/T8.5a all note `_a24/events/emit`'s `sink.emit`
    /// resolves on its first poll and so never even reaches this code path).
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn judgement_8a_a_queued_call_is_cancelled_when_its_bound_request_ends() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("m.db");
        let kv = agent24_memory::KvStore::open(&db_path).await.unwrap();
        let entitlement = granted_entitlement_real_admission(&kv, "alice", "sin90").await;
        let mut lock_conn = take_external_writer_lock(&db_path).await;

        let mut write_tasks = Vec::new();
        for _ in 0..4u32 {
            let h = RememberHandler {
                generation: running_generation(),
                entitlement: entitlement.clone(),
            };
            write_tasks.push(
                spawn_and_confirm_blocked(Box::pin(async move {
                    h.call(json!({"kind": "note", "body": {}})).await
                }))
                .await,
            );
        }

        assert_eq!(
            kv.oop_admission().unwrap().available_permits(),
            0,
            "all 4 real permits must be occupied before the 5th call is even attempted"
        );

        let g = running_generation();
        let live = g
            .admit_request(
                "r1".to_owned(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30), // generous — this test cancels via `finish`, not the budget
            )
            .unwrap();
        // Codex review round 1 (M3): the frozen design's judgement 8a/8b
        // specify `recall_page`, not `remember_checked` — a queued READ,
        // not a queued write, is the case that had no coverage at all.
        let fifth = RecallHandler {
            generation: g,
            entitlement: entitlement.clone(),
        };
        let fifth_task = spawn_and_confirm_blocked(Box::pin(async move {
            fifth
                .call(json!({"query": "", "page_size": 1, "request_id": "r1"}))
                .await
        }))
        .await;
        assert!(
            !fifth_task.is_finished(),
            "the 5th call must genuinely be queued on the exhausted permit \
             before this test ends its bound request"
        );

        let _ = live.finish(); // ends the request while the call is still queued
        let err = tokio::time::timeout(Duration::from_secs(5), fifth_task)
            .await
            .expect("must resolve once its bound request ends, not hang")
            .unwrap()
            .unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::Timeout));
        assert!(
            err.message.contains("ended"),
            "must say the REQUEST ended, distinct from a budget running out: {:?}",
            err.message
        );

        sqlx::query("ROLLBACK")
            .execute(&mut lock_conn)
            .await
            .unwrap();
        for t in write_tasks {
            let _ = t.await;
        }
    }

    /// Judgement 8b: the other cancellation branch — a real queued call
    /// whose bound request's time budget runs out (not ended, still live)
    /// is cut off the same way, with a distinguishable message.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn judgement_8b_a_queued_call_is_cancelled_when_its_time_budget_is_exhausted() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("m.db");
        let kv = agent24_memory::KvStore::open(&db_path).await.unwrap();
        let entitlement = granted_entitlement_real_admission(&kv, "alice", "sin90").await;
        let mut lock_conn = take_external_writer_lock(&db_path).await;

        let mut write_tasks = Vec::new();
        for _ in 0..4u32 {
            let h = RememberHandler {
                generation: running_generation(),
                entitlement: entitlement.clone(),
            };
            write_tasks.push(
                spawn_and_confirm_blocked(Box::pin(async move {
                    h.call(json!({"kind": "note", "body": {}})).await
                }))
                .await,
            );
        }

        assert_eq!(
            kv.oop_admission().unwrap().available_permits(),
            0,
            "all 4 real permits must be occupied before the 5th call is even attempted"
        );

        let g = running_generation();
        // A budget far shorter than the pool's 5s `busy_timeout` (judgement
        // 7's setup, same constant) — the queue wait WILL outlast it. 300ms,
        // not 50ms (Codex review round 2, Low): `spawn_and_confirm_blocked`
        // must observe a real `Pending` poll BEFORE this budget elapses, and
        // on a loaded CI runner 50ms was tight enough to risk a scheduling
        // delay racing the budget itself, which would fail loud rather than
        // silently pass — still comfortably under the 5s busy_timeout, so it
        // does not risk the filler tasks timing out first.
        let _live = g
            .admit_request(
                "r1".to_owned(),
                [0u8; 32],
                Instant::now(),
                Duration::from_millis(300),
            )
            .unwrap();
        // Codex review round 1 (M3): use `RecallHandler` (matches the
        // frozen design's judgement 8b) AND confirm the call has genuinely
        // been polled to `Pending` at least once (`spawn_and_confirm_blocked`)
        // BEFORE letting the 50ms budget run out — awaiting it directly
        // cannot distinguish "it queued, then the budget expired" from "the
        // budget was already gone before this even reached `acquire_owned`".
        let fifth = RecallHandler {
            generation: g,
            entitlement: entitlement.clone(),
        };
        let fifth_task = spawn_and_confirm_blocked(Box::pin(async move {
            fifth
                .call(json!({"query": "", "page_size": 1, "request_id": "r1"}))
                .await
        }))
        .await;
        let err = tokio::time::timeout(Duration::from_secs(5), fifth_task)
            .await
            .expect("must resolve once its budget is exhausted, not hang")
            .unwrap()
            .unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::Timeout));
        assert!(
            err.message.contains("budget"),
            "must say the BUDGET was exhausted, distinct from the request \
             having ended: {:?}",
            err.message
        );

        sqlx::query("ROLLBACK")
            .execute(&mut lock_conn)
            .await
            .unwrap();
        for t in write_tasks {
            let _ = t.await;
        }
    }
}
