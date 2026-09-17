//! `_a24/approval/gate` / `_a24/approval/advise` / `_a24/approval/status` —
//! the out-of-process wire methods for T7b/ME-3e. See
//! `docs/design/T7b-ME3e-approvals.md`, decision 4.
//!
//! Registered UNCONDITIONALLY on every out-of-process connection
//! (`crate::domain`'s `MethodsFor`), exactly like `_a24/events/emit`
//! (`crate::events_emit`) — capability gating happens INSIDE `call()`, not by
//! conditionally registering the method, so `forbidden` (a handler exists,
//! this call isn't allowed) stays distinct from `-32601` (no such method at
//! all).

use std::sync::Arc;

use agent24_domain::{Capability, Grants};
use agent24_os_proto::drain::{ApprovalCallbackRefused, Generation};
use agent24_os_proto::rpc::{CallFuture, ErrorKind, Handler, RpcError};
use agent24_protocol::{ApprovalAnswer, ApprovalRequestError, ModuleApprovalKind};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::module_approval_broker::{ModuleApprovalBroker, validate_gate_action};

/// Wire params for `gate`/`advise` (design doc decision 4) — the SUBMIT
/// shape. `deny_unknown_fields` with an explicit `_meta` (SPEC's `_meta`
/// requirement, Codex round 4 High 6): a legitimate `_meta` must not be
/// rejected as an unknown field, but nothing else may sneak in beside it.
///
/// NO `#[derive(Debug)]` (judgement 19: `approval_token` must never reach a
/// `Debug` output) — a hand-written impl redacts it instead, so a future
/// `{params:?}` added anywhere in this crate cannot leak the secret by
/// accident the way a derived impl would.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalSubmitParams {
    action: String,
    #[serde(default)]
    target: Option<String>,
    payload: Value,
    request_id: String,
    approval_token: String,
    #[serde(default)]
    _meta: Option<Map<String, Value>>,
}

impl std::fmt::Debug for ApprovalSubmitParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalSubmitParams")
            .field("action", &self.action)
            .field("target", &self.target)
            .field("payload", &self.payload)
            .field("request_id", &self.request_id)
            .field("approval_token", &"<redacted>")
            .field("_meta", &self._meta)
            .finish()
    }
}

/// Wire params for `status` (design doc decision 4) — the QUERY shape.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalStatusParams {
    approval_id: String,
    #[serde(default)]
    _meta: Option<Map<String, Value>>,
}

fn answer_json(answer: &ApprovalAnswer) -> Value {
    // `ApprovalAnswer` already carries exactly the wire shape design doc
    // decision 4 specifies (`{approval_id, kind, binding, decision}`) — no
    // hand-rolled `json!` to keep in sync with it separately.
    serde_json::to_value(answer).unwrap_or_else(|_| Value::Object(Map::new()))
}

fn refused_error(refused: ApprovalCallbackRefused) -> RpcError {
    match refused {
        ApprovalCallbackRefused::NotReady => RpcError::application(
            ErrorKind::NotReady,
            "the module has not finished its handshake yet",
        ),
        ApprovalCallbackRefused::Revoked => {
            RpcError::application(ErrorKind::Revoked, "this generation has been revoked")
        }
        ApprovalCallbackRefused::TokenInvalid => RpcError::application(
            ErrorKind::TokenInvalid,
            "request_id/approval_token did not admit this submission",
        ),
    }
}

/// Mapping shared by `gate`/`advise`/`status` wherever `ModuleApprovalBroker`
/// or the in-process backend can fail (design doc decisions 2/4/6).
fn request_error(err: ApprovalRequestError) -> RpcError {
    match err {
        // Only reachable for `gate` (decision 4, step 2) — SPEC §6.1's
        // wording is literal about reusing `Forbidden`, not a more precise
        // kind (design doc decision 4).
        ApprovalRequestError::ActionNotInClosedSet => {
            RpcError::application(ErrorKind::Forbidden, "action not in the closed set")
        }
        // T7c/ME-3e: "in the closed set, bad arguments" — distinct from the
        // above on the wire (design doc "闭集匹配"): `-32602`/invalid params,
        // never `forbidden`.
        ApprovalRequestError::InvalidTarget(msg) => RpcError::invalid_params(msg),
        ApprovalRequestError::NotFound => {
            RpcError::application(ErrorKind::NotFound, "approval not found")
        }
        ApprovalRequestError::BackendUnavailable(msg) => {
            // Wire boundary: the daemon's existing internal-error kind, not
            // an application `kind` (design doc decision 6) — and the
            // message must not leak storage internals to the module (SPEC
            // §3: "error.data 里不得出现内核内部路径、SQL、token").
            let _ = msg;
            RpcError::internal("the approval backend is temporarily unavailable")
        }
    }
}

/// `_a24/approval/gate` and `_a24/approval/advise` share this handler,
/// parameterized by `kind` — the two differ only in that `Gate` always hits
/// the (empty) closed set BEFORE the token is ever consumed (design doc
/// decision 4, step 2; judgement 16b).
pub struct ApprovalSubmitHandler {
    pub generation: Arc<Generation>,
    pub module: String,
    pub granted: Grants,
    pub kind: ModuleApprovalKind,
    pub broker: Arc<ModuleApprovalBroker>,
}

impl Handler for ApprovalSubmitHandler {
    fn check_params(&self, params: &Value) -> Result<(), String> {
        serde_json::from_value::<ApprovalSubmitParams>(params.clone())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn call(&self, params: Value) -> CallFuture {
        let parsed = serde_json::from_value::<ApprovalSubmitParams>(params);
        let granted = self.granted.clone();
        let generation = self.generation.clone();
        let module = self.module.clone();
        let kind = self.kind;
        let broker = self.broker.clone();
        Box::pin(async move {
            // `check_params` already proved this deserializes; a failure
            // here is a kernel bug, not a caller error.
            let mut parsed = parsed.map_err(|e| {
                RpcError::internal(format!(
                    "params for module {module:?} were valid at check_params but not at \
                     call(): {e}"
                ))
            })?;

            // Step 1 (decision 4): capability check. No grant, no quota
            // charged, no record touched — never even reaches the token or
            // the closed set.
            if !granted.has(Capability::Approval) {
                return Err(RpcError::application(
                    ErrorKind::Forbidden,
                    "the `approval` capability was not granted to this module",
                ));
            }

            // Step 2: for `Gate`, the closed-set check runs BEFORE token
            // consumption (Codex round 5 High 1 / judgement 16b) — a `gate`
            // call that is always going to be `forbidden` must not burn the
            // token a subsequent `advise` with the same request_id would
            // still need. T7c/ME-3e: on success, `parsed.target` is
            // OVERWRITTEN with `validate_gate_action`'s canonicalized form —
            // the raw string the module sent is never what gets persisted
            // (design doc "闭集匹配"); the in-process path
            // (`crate::domain::PolicyApprovalBackend`) calls this exact same
            // function, so the two paths cannot disagree (judgement 16).
            if kind == ModuleApprovalKind::Gate {
                match validate_gate_action(&parsed.action, parsed.target.as_deref()) {
                    Ok(canonical) => {
                        parsed.action = canonical.action;
                        parsed.target = Some(canonical.target);
                    }
                    Err(err) => return Err(request_error(err)),
                }
            }

            // Step 3: idempotent lookup — a resubmit with the same
            // `(module, request_id, kind)` returns the existing row as-is,
            // never re-validating the token or creating a second row
            // (judgement 16a).
            match broker
                .find_existing(&module, &parsed.request_id, kind)
                .await
            {
                Ok(Some(existing)) => return Ok(answer_json(&existing)),
                Ok(None) => {}
                Err(err) => return Err(request_error(err)),
            }

            // Step 4: token admission — ONLY reached when no existing row
            // was found, and only once per (module, request_id, kind).
            if let Err(refused) =
                generation.admit_approval_callback(&parsed.request_id, &parsed.approval_token)
            {
                return Err(refused_error(refused));
            }

            // Step 5: insert + push `module-approval.required`.
            let answer = broker
                .insert(
                    &module,
                    &parsed.request_id,
                    kind,
                    parsed.action,
                    parsed.target,
                    parsed.payload,
                )
                .await
                .map_err(request_error)?;

            // Step 6: return. T7c/ME-3e: `gate` CAN reach this line now, for
            // `schedule_callback` — step 2 only returns early for an action
            // outside the closed set or a bad `target`.
            Ok(answer_json(&answer))
        })
    }
}

/// `_a24/approval/status` — the QUERY method. Deliberately does not touch
/// `Generation`/the drain state machine at all (design doc "现状" 2): this is
/// an independent read, not a re-check of whether the original proxied
/// request is still alive.
pub struct ApprovalStatusHandler {
    pub module: String,
    pub granted: Grants,
    pub broker: Arc<ModuleApprovalBroker>,
}

impl Handler for ApprovalStatusHandler {
    fn check_params(&self, params: &Value) -> Result<(), String> {
        serde_json::from_value::<ApprovalStatusParams>(params.clone())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn call(&self, params: Value) -> CallFuture {
        let parsed = serde_json::from_value::<ApprovalStatusParams>(params);
        let granted = self.granted.clone();
        let module = self.module.clone();
        let broker = self.broker.clone();
        Box::pin(async move {
            let parsed = parsed.map_err(|e| {
                RpcError::internal(format!(
                    "params for module {module:?} were valid at check_params but not at \
                     call(): {e}"
                ))
            })?;

            // Step 1 (decision 4, "status" processing order): capability
            // check — v5 missed this (Codex round 5 High 3): without it, a
            // module whose `approval` grant was later revoked could still
            // read old approvals by a remembered id.
            if !granted.has(Capability::Approval) {
                return Err(RpcError::application(
                    ErrorKind::Forbidden,
                    "the `approval` capability was not granted to this module",
                ));
            }

            let answer = broker
                .status(&module, &parsed.approval_id)
                .await
                .map_err(request_error)?;
            Ok(answer_json(&answer))
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_os_proto::rpc::code;
    use serde_json::json;

    async fn test_broker() -> Arc<ModuleApprovalBroker> {
        ModuleApprovalBroker::new(
            agent24_store::Store::open_memory().await.unwrap(),
            crate::events::EventsHub::default(),
        )
    }

    fn running_generation() -> Arc<Generation> {
        let g = Generation::serving_at("/tmp/does-not-need-to-exist".into());
        assert!(g.ready(), "a freshly-serving generation must become Ready");
        g
    }

    /// A running generation with `"req-1"`/`"secret-1"` (the pair
    /// [`good_params`] uses) already admitted via `admit_request` — exactly
    /// what the PROXY does in production before a module ever gets a chance
    /// to call back. The returned `InFlight` MUST be kept alive (bound to a
    /// variable, not `_`) for as long as the test calls a submit handler
    /// with that pair: dropping it removes the id from `in_flight`.
    fn generation_with_good_params_admitted() -> (Arc<Generation>, agent24_os_proto::drain::InFlight)
    {
        let g = running_generation();
        let in_flight = admit(&g, "req-1", "secret-1");
        (g, in_flight)
    }

    fn admit(g: &Arc<Generation>, id: &str, token: &str) -> agent24_os_proto::drain::InFlight {
        use sha2::Digest;
        let hash: [u8; 32] = sha2::Sha256::digest(token.as_bytes()).into();
        g.admit_request(
            id.to_owned(),
            hash,
            std::time::Instant::now(),
            std::time::Duration::from_secs(30),
        )
        .unwrap()
    }

    fn granted(has_approval: bool) -> Grants {
        if has_approval {
            agent24_domain::Grants::granting(&[Capability::Approval], &[Capability::Approval])
        } else {
            agent24_domain::Grants::granting(&[], &[Capability::Approval])
        }
    }

    fn submit_handler(
        generation: Arc<Generation>,
        has_approval: bool,
        kind: ModuleApprovalKind,
        broker: Arc<ModuleApprovalBroker>,
    ) -> ApprovalSubmitHandler {
        ApprovalSubmitHandler {
            generation,
            module: "probe".to_owned(),
            granted: granted(has_approval),
            kind,
            broker,
        }
    }

    fn status_handler(
        has_approval: bool,
        broker: Arc<ModuleApprovalBroker>,
    ) -> ApprovalStatusHandler {
        ApprovalStatusHandler {
            module: "probe".to_owned(),
            granted: granted(has_approval),
            broker,
        }
    }

    fn good_params() -> Value {
        json!({
            "action": "send_email",
            "target": "ops@example.com",
            "payload": {"body": "hi"},
            "request_id": "req-1",
            "approval_token": "secret-1",
        })
    }

    // ── judgement 1: capability gating, all three methods ─────────────────

    #[tokio::test]
    async fn ungranted_module_is_forbidden_on_gate_advise_and_status_and_builds_nothing() {
        let broker = test_broker().await;
        for kind in [ModuleApprovalKind::Gate, ModuleApprovalKind::Advise] {
            let h = submit_handler(running_generation(), false, kind, broker.clone());
            let err = h.call(good_params()).await.unwrap_err();
            assert_eq!(err.kind, Some(ErrorKind::Forbidden));
        }
        let status = status_handler(false, broker.clone());
        let err = status
            .call(json!({"approval_id": "does-not-matter"}))
            .await
            .unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::Forbidden));

        assert!(
            broker.list(None).await.unwrap().is_empty(),
            "an ungranted module's calls must not build any record"
        );
    }

    #[tokio::test]
    async fn granted_module_can_advise_and_query_status_gate_stays_forbidden() {
        let broker = test_broker().await;
        // Positive control for `advise`.
        let (g, _in_flight) = generation_with_good_params_admitted();
        let advise = submit_handler(g, true, ModuleApprovalKind::Advise, broker.clone());
        let answer = advise.call(good_params()).await.unwrap();
        assert_eq!(answer["decision"], "pending");
        assert_eq!(answer["kind"], "advise");
        assert_eq!(answer["binding"], false);

        let status = status_handler(true, broker.clone());
        let looked_up = status
            .call(json!({"approval_id": answer["approval_id"]}))
            .await
            .unwrap();
        assert_eq!(looked_up["decision"], "pending");

        // Judgement 8: `gate` is forbidden even for a GRANTED module, this
        // round — the empty closed set, not a missing capability.
        let gate = submit_handler(
            running_generation(),
            true,
            ModuleApprovalKind::Gate,
            broker.clone(),
        );
        let err = gate.call(good_params()).await.unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::Forbidden));
        assert!(
            broker
                .list(None)
                .await
                .unwrap()
                .iter()
                .all(|a| a.kind == ModuleApprovalKind::Advise),
            "gate must never build a record, this round"
        );
    }

    // ── judgement 2a/2b: unknown fields vs. `_meta` ───────────────────────

    #[tokio::test]
    async fn an_unknown_field_outside_meta_is_rejected_at_check_params() {
        let h = submit_handler(
            running_generation(),
            true,
            ModuleApprovalKind::Advise,
            test_broker().await,
        );
        let mut params = good_params();
        params["sneaky"] = json!("nope");
        let err = h.check_params(&params).expect_err("unknown field");
        assert!(err.contains("sneaky") || err.contains("unknown"), "{err}");
    }

    #[tokio::test]
    async fn a_legitimate_meta_field_passes_through_harmlessly() {
        let broker = test_broker().await;
        let (g, _in_flight) = generation_with_good_params_admitted();
        let h = submit_handler(g, true, ModuleApprovalKind::Advise, broker);
        let mut params = good_params();
        // Sneaking same-named keys inside `_meta` must not influence anything.
        params["_meta"] = json!({"module": "sneaky", "request_id": "not-the-real-one"});
        h.check_params(&params).expect("_meta is a declared field");
        let answer = h.call(params).await.unwrap();
        assert_eq!(answer["decision"], "pending");
    }

    // ── judgement 9: submit never blocks on a human decision ──────────────

    #[tokio::test]
    async fn submit_completes_fast_not_waiting_on_any_human() {
        let broker = test_broker().await;
        let (g, _in_flight) = generation_with_good_params_admitted();
        let h = submit_handler(g, true, ModuleApprovalKind::Advise, broker);
        let start = std::time::Instant::now();
        h.call(good_params()).await.unwrap();
        // Generous on purpose (design doc judgement 9): the claim is
        // "database-write latency", not "wait for a person" — 1s is well
        // under both the 30s RPC/proxy timeout and the 300s legacy approval
        // default, so it cannot be satisfied by accident by either of those.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "submit took {:?}, which looks like it waited for something",
            start.elapsed()
        );
    }

    // ── judgement 16a/16b: idempotent resubmit; gate doesn't burn the token ──

    #[tokio::test]
    async fn resubmitting_the_same_request_id_and_token_is_idempotent_at_the_wire() {
        let broker = test_broker().await;
        let (g, _in_flight) = generation_with_good_params_admitted();
        let h = submit_handler(g, true, ModuleApprovalKind::Advise, broker.clone());
        let first = h.call(good_params()).await.unwrap();
        let second = h.call(good_params()).await.unwrap();
        assert_eq!(first["approval_id"], second["approval_id"]);
        assert_eq!(broker.list(None).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn gate_does_not_consume_the_token_a_following_advise_still_succeeds() {
        let broker = test_broker().await;
        let (g, _in_flight) = generation_with_good_params_admitted();
        let gate = submit_handler(g.clone(), true, ModuleApprovalKind::Gate, broker.clone());
        let err = gate.call(good_params()).await.unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::Forbidden));

        // Same `{request_id, approval_token}`, now for `advise` — must still
        // succeed: `gate`'s closed-set rejection happens strictly before any
        // token admission (judgement 16b).
        let advise = submit_handler(g, true, ModuleApprovalKind::Advise, broker.clone());
        let answer = advise.call(good_params()).await.unwrap();
        assert_eq!(answer["decision"], "pending");
    }

    #[tokio::test]
    async fn a_wrong_token_against_a_live_request_id_is_token_invalid() {
        let broker = test_broker().await;
        // "req-1" IS admitted (with "secret-1"), so this exercises the
        // hash-mismatch branch specifically — not merely "id never admitted".
        let (g, _in_flight) = generation_with_good_params_admitted();
        let h = submit_handler(g, true, ModuleApprovalKind::Advise, broker);
        let mut params = good_params();
        params["approval_token"] = json!("wrong-token");
        let err = h.call(params).await.unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::TokenInvalid));
    }

    #[tokio::test]
    async fn an_id_that_was_never_admitted_is_also_token_invalid() {
        // Judgement 5/6's shape at the wire: an id the proxy never admitted
        // (or already finished) reads identically to a wrong token — the
        // error matrix deliberately does not distinguish them.
        let broker = test_broker().await;
        let h = submit_handler(
            running_generation(),
            true,
            ModuleApprovalKind::Advise,
            broker,
        );
        let err = h.call(good_params()).await.unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::TokenInvalid));
    }

    // ── judgement 19: the token never appears in an error message ─────────

    #[tokio::test]
    async fn a_check_params_failure_on_an_unrelated_field_never_dumps_the_real_token() {
        // The concrete risk (design doc judgement 19) is dumping the WHOLE
        // params object on an unrelated failure, which would include a
        // real, correctly-typed token the caller already trusted the
        // channel with. Deliberately break a DIFFERENT field (`payload`
        // missing) while `approval_token` is a normal, valid string, and
        // check that string never appears in the resulting message.
        let h = submit_handler(
            running_generation(),
            true,
            ModuleApprovalKind::Advise,
            test_broker().await,
        );
        let mut params = good_params();
        params["approval_token"] = json!("a-real-looking-secret-value");
        params.as_object_mut().unwrap().remove("payload");
        let err = h
            .check_params(&params)
            .expect_err("payload is required and now missing");
        assert!(
            !err.contains("a-real-looking-secret-value"),
            "the check_params error must not dump the whole params object: {err}"
        );
    }

    #[test]
    fn approval_submit_params_debug_output_redacts_the_token() {
        let parsed: super::ApprovalSubmitParams = serde_json::from_value(good_params()).unwrap();
        let debug = format!("{parsed:?}");
        assert!(!debug.contains("secret-1"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
    }

    // ── judgement 11/12 (wire `status`): cross-module / nonexistent ids ────

    #[tokio::test]
    async fn status_treats_cross_module_and_nonexistent_ids_identically() {
        let broker = test_broker().await;
        let (g, _in_flight) = generation_with_good_params_admitted();
        let advise = submit_handler(g, true, ModuleApprovalKind::Advise, broker.clone());
        let answer = advise.call(good_params()).await.unwrap();

        let other_module_status = ApprovalStatusHandler {
            module: "someone-else".to_owned(),
            granted: granted(true),
            broker: broker.clone(),
        };
        let cross = other_module_status
            .call(json!({"approval_id": answer["approval_id"]}))
            .await
            .unwrap_err();
        let missing = status_handler(true, broker)
            .call(json!({"approval_id": "totally-made-up"}))
            .await
            .unwrap_err();
        assert_eq!(cross.kind, Some(ErrorKind::NotFound));
        assert_eq!(missing.kind, Some(ErrorKind::NotFound));
        assert_eq!(cross.code, missing.code);
    }

    #[tokio::test]
    async fn ungranted_plus_malformed_params_is_invalid_params_via_dispatch() {
        // Mirrors `events_emit`'s judgement 16: `check_params` runs before
        // `call()`, so an ungranted module's malformed submission is
        // `-32602`, not `forbidden` — capability gating never gets a chance
        // to fire on a request `dispatch()` already refused.
        let h = Arc::new(submit_handler(
            running_generation(),
            false,
            ModuleApprovalKind::Advise,
            test_broker().await,
        ));
        let methods = agent24_os_proto::rpc::Methods::none().with("_a24/approval/advise", h);
        let frame = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": "1",
            "method": "_a24/approval/advise",
            "params": {"action": "a", "payload": {}, "sneaky": true},
        }))
        .unwrap();
        let agent24_os_proto::rpc::Dispatch::Respond(r) =
            agent24_os_proto::rpc::dispatch(&frame, &methods, &|_| false)
        else {
            panic!("check_params must reject this before call() ever runs");
        };
        let err = r.outcome.unwrap_err();
        assert_eq!(err.code, code::INVALID_PARAMS);
    }

    // ── T7c/ME-3e: `gate`'s first closed-set entry, `schedule_callback` ────

    fn schedule_callback_params(target: Option<&str>) -> Value {
        let mut params = json!({
            "action": "schedule_callback",
            "payload": {},
            "request_id": "req-1",
            "approval_token": "secret-1",
        });
        if let Some(t) = target {
            params["target"] = json!(t);
        }
        params
    }

    #[tokio::test]
    async fn gate_accepts_schedule_callback_and_stores_the_canonicalized_target() {
        let broker = test_broker().await;
        let (g, _in_flight) = generation_with_good_params_admitted();
        let h = submit_handler(g, true, ModuleApprovalKind::Gate, broker.clone());
        let raw_target = "2026-01-01T08:00:00.5+08:00"; // epoch-equal to 2026-01-01T00:00:00Z
        let answer = h
            .call(schedule_callback_params(Some(raw_target)))
            .await
            .unwrap();
        assert_eq!(answer["decision"], "pending");
        assert_eq!(answer["kind"], "gate");
        assert_eq!(
            answer["binding"], true,
            "a Gate submission that reaches insert() must be binding (judgement 11)"
        );
        assert_eq!(answer["executed_at"], serde_json::Value::Null);

        // Judgement 16 (wire half): the value actually stored is EXACTLY
        // what calling the shared `validate_gate_action` on the same input
        // produces — proving the wire path did not roll its own
        // canonicalization.
        let expected = crate::module_approval_broker::validate_gate_action(
            "schedule_callback",
            Some(raw_target),
        )
        .unwrap();
        let stored = broker
            .get(answer["approval_id"].as_str().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.target.as_deref(), Some(expected.target.as_str()));
    }

    #[tokio::test]
    async fn gate_schedule_callback_without_a_target_is_invalid_params_not_forbidden() {
        // Judgement 12: missing `target` is `-32602`, never `forbidden`.
        let broker = test_broker().await;
        let (g, _in_flight) = generation_with_good_params_admitted();
        let h = submit_handler(g, true, ModuleApprovalKind::Gate, broker.clone());
        let err = h.call(schedule_callback_params(None)).await.unwrap_err();
        assert_eq!(err.code, code::INVALID_PARAMS);
        assert!(
            broker.list(None).await.unwrap().is_empty(),
            "an invalid target must not create a record"
        );
    }

    #[tokio::test]
    async fn gate_schedule_callback_with_an_unparseable_target_is_invalid_params() {
        let broker = test_broker().await;
        let (g, _in_flight) = generation_with_good_params_admitted();
        let h = submit_handler(g, true, ModuleApprovalKind::Gate, broker.clone());
        let err = h
            .call(schedule_callback_params(Some("whenever")))
            .await
            .unwrap_err();
        assert_eq!(err.code, code::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn gate_still_forbids_every_other_action_even_with_a_valid_target() {
        // Judgement 7: a valid `target` does not smuggle an unrelated
        // `action` into the closed set.
        let broker = test_broker().await;
        let (g, _in_flight) = generation_with_good_params_admitted();
        let h = submit_handler(g, true, ModuleApprovalKind::Gate, broker.clone());
        let mut params = schedule_callback_params(Some("2026-01-01T00:00:00Z"));
        params["action"] = json!("transfer_funds");
        let err = h.call(params).await.unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::Forbidden));
    }
}
