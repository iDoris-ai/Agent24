# D1-2a — L1 `Unavailable` failure-layer contract (proposal only)

Status: proposal for PR-Daemon review; this task authorizes only the documentation PR described in the dated authorization record in `AGENTS.md`, not a production code change.
Baseline: Agent24 `ab/decide` at `37f9fde8fb0f6ee74485cf3b75e52e6de012d692`; D1 plan `docs/agent/PLAN-DECIDE-D1.md`.

## Objective

Make an unavailable cascade layer observable without misreporting an earlier floor result as the failed layer or as a successful final decision. The service must preserve the distinction between (a) a real floor result and its source and (b) the layer whose backend was unavailable. This slice is limited to L1; it does not implement answer validation (L2), trace/log shape (L3), thresholds (L4), or product call-site wiring.

## Proposed contract

1. `Unavailable` identifies the exact decision/backend layer whose required backend was unavailable. The representation must be structured and serializable; do not encode the layer only in prose. Confirm whether `BackendKind` uniquely identifies a cascade stage; if duplicate kinds are possible, use a stable stage identity rather than a model/display name.
2. Existing source/answers for an earlier floor remain attributable to that actual floor. Do not overwrite the floor's source with the unavailable layer, promote it to `Decided`, or treat partial floor answers as a complete decision. Preserve current `Decision.backend` semantics (`floor_backend.unwrap_or(failed_kind)`) unless an approved contract explicitly changes them; the new field must disambiguate, not silently redefine this field.
3. An unavailable result is terminal for the current cascade attempt: once a layer reports `Unavailable`, the service does not continue to a later layer or silently select another model/backend.
4. With no floor result, preserve the empty/unknown answer state while still identifying the failed layer. With a floor result, preserve that floor exactly and separately identify the failed layer.
5. The exact public enum/field name and serde compatibility strategy are review items. `Outcome`/`Decision` currently derive `Serialize` only. The proposal must state the JSON/API compatibility impact of adding layer identity to `Unavailable`; do not claim byte-for-byte T0 compatibility without evidence. Do not repurpose `backend` or infer the failure layer from model names.

## Scope and invariants

- In scope: `rust/crates/agent24-decide/src/service.rs`, `types.rs`, and focused fake-backend unit tests only, after approval of this contract.
- Out of scope: agent/memory/recall/module integration, ONNX/model downloads, hardware tier selection, model directory changes, threshold policy, log schema, network/cloud fallback, A-class safety/authz, and all K1 product gates.
- Preserve T0 and existing fail-closed behavior. Do not let a model result authorize writes, egress, ownership, active-state, capabilities, or other A-class actions.
- No code, tests, or API modification before the interface proposal obtains PR-Daemon `APPROVED` on its exact head. The user authorized creating, committing, pushing, and opening this proposal-only PR for that review; this does not authorize implementation, merging, or release.

## Test-first acceptance matrix for a later implementation

Use deterministic fake backends; no model assets, network, timing thresholds, or hardware assumptions.

| Case | Setup | Required observable result |
|---|---|---|
| First attempted layer unavailable | No prior floor; that backend returns `Unavailable` | Result remains unavailable, has no invented answers/floor, names exactly the failed layer; no later backend is called. |
| Earlier floor then deeper layer unavailable | Rule/encoder floor is returned, next required backend is unavailable | Preserve the floor's original source and answers; separately name the unavailable layer; do not return `Decided`; no later backend is called. |
| Empty floor | Earlier layer yields no answer, next required backend is unavailable | Preserve empty floor semantics and identify the failed layer; do not synthesize an answer. |
| Exact call boundary | Fake layers count invocations | Each layer through the failure is called at most once; all later layers have zero calls. |
| Serialization compatibility | Snapshot the new unavailable JSON shape and an existing legacy outcome shape | New payload exposes layer identity; unaffected legacy variants keep their existing JSON shape. Do not add deserialization support solely for this task (`Outcome`/`Decision` currently derive `Serialize`, not `Deserialize`); if an actual consumer needs old-payload decoding, route that separately. |

Before implementation, add the acceptance tests and record that the relevant test fails on the pre-fix baseline. Then make the smallest change and run the focused crate tests plus applicable Rust gates serially on the laptop/shared Cargo target; do not run workspace Cargo gates concurrently.

## Review and completion gates

- Independent reviewer must challenge ambiguity around “floor”, “required layer”, serialized compatibility, and whether all unavailable paths are terminal.
- PR-Daemon must approve the exact interface head before implementation proceeds; approval for a stale head is invalid. This PR is solely for proposal review; no implementation is part of its acceptance.
- Keep any implementation PR under the Agent24 production-code size budget and target `ab/decide`; release remains human-owned.
- Acceptance is the review of this proposal and its test matrix only. It is not D1 completion, runtime integration, K1 clearance, or release approval.

## Evidence basis

At the stated baseline, `service.rs` preserves the latest non-empty floor when a later backend is unavailable and sets `Decision.backend` to `floor_backend.unwrap_or(kind)`; `Outcome::Unavailable` carries only a reason, so the failed layer is not separately represented when a floor exists. The authoritative D1 plan assigns L2 validation and L4 thresholds to the service and marks D1-2 high-risk.
