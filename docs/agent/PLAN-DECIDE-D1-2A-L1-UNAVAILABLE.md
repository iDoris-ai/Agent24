# D1-2a — L1 `Unavailable` failure-layer contract (clarification proposal)

Status: documentation-only follow-up to merged PR #854. This document resolves the concrete stage-identity and serialized-shape decisions for a later implementation; it does not implement or authorize production code/tests by itself.
Baseline: `ab/decide` at `6e8929ca8e36b035dd043b829663c242e784ecde`, including PR #854. D1 plan: `docs/agent/PLAN-DECIDE-D1.md`.

## Objective

Make an unavailable cascade layer observable without misreporting an earlier floor result as the failed layer or as a successful final decision. Preserve the distinction between (a) a real floor result and its source and (b) the exact configured cascade entry whose backend was unavailable. This slice remains limited to L1; it does not implement answer validation (L2), trace/log shape (L3), thresholds (L4), or product call-site wiring.

## Contract decisions

1. **Exact failed-stage identity.** `BackendKind` identifies a backend class, not a unique cascade stage; repeated kinds are valid. Add two fields to the public `Outcome::Unavailable` variant:
   - `failed_stage_index: usize` — zero-based position in the `DecisionService` ordered `backends` vector for this decision attempt.
   - `failed_backend_kind: BackendKind` — the `kind()` of the backend at that position.

   The pair identifies the exact attempted entry even when two entries share a kind. The index is stable only for that service configuration/attempt; it is not a durable ID across configuration reorderings. Do not infer identity from model IDs, display names, or `Decision.backend`.
2. **Floor and `Decision.backend` remain unchanged.** If a non-empty floor exists, preserve its answers and keep `Decision.backend` set to that floor's `BackendKind`; report the failed layer separately in `Outcome::Unavailable`. With no floor, preserve empty answers and the existing `floor_backend.unwrap_or(failed_kind)` behavior. Never promote a floor to `Decided` or overwrite its source with the failed backend.
3. **Unavailable is terminal.** The failed configured entry is evaluated once; after it returns `Unavailable`, do not call later entries or select a fallback backend/model.
4. **Floor semantics.** Only a non-empty `NoConclusion.floor` replaces the remembered floor. An empty floor does not clear a prior non-empty floor; when multiple non-empty floors occur, retain the most recent one. Exercise these cases with deterministic fake backends.
5. **Scope.** Only the service result type and its focused fake-backend tests need the new identity. `BackendOutcome::Unavailable` remains the backend's reason-bearing input; the service adds the configured index and backend kind when it constructs public `Outcome::Unavailable`.

## Rust and JSON shape

The intended Rust shape is:

```rust
Outcome::Unavailable {
    reason: String,
    failed_stage_index: usize,
    failed_backend_kind: BackendKind,
}
```

For a two-entry chain where entry 0 left a Rule floor and entry 1 (Encoder) became unavailable, serialize the outcome as:

```json
{"kind":"unavailable","reason":"encoder unavailable","failed_stage_index":1,"failed_backend_kind":"encoder"}
```

`Decision.backend` remains `rule` in that example; it does not become `encoder`.

`Outcome` and `Decision` currently derive `Serialize`, not `Deserialize`. Adding these variant fields is an intentional Rust source/API and `Unavailable` JSON-shape change: construction/exhaustive matching of this variant must be updated, and old serialized `Unavailable` records do not gain new fields retroactively. Do not claim byte-for-byte compatibility for `Unavailable`. The `Decided` and `Abstain` JSON shapes, and all unaffected T0 behavior, remain unchanged. Do not add deserialization, log migration, or a broader schema-versioning system in this task; inventory actual in-repository consumers before implementation and route any external compatibility requirement separately.

## Test-first acceptance matrix for a later implementation

Use deterministic fake backends; no model assets, network, timing thresholds, or hardware assumptions. First add tests and record that the relevant assertions fail on the pre-fix baseline, then implement the smallest change.

| Case | Setup | Required observable result |
|---|---|---|
| First attempted layer unavailable | No prior floor; first backend returns `Unavailable` | Empty answers; exact index and kind reported; no later backend called. |
| Floor then unavailable | A backend leaves a non-empty floor; a later backend is unavailable | Preserve floor answers and `Decision.backend`; independently report the failing index/kind; outcome is not `Decided`; no later backend called. |
| Empty after floor | A non-empty floor is followed by an empty `NoConclusion`, then `Unavailable` | Retain the earlier non-empty floor and identify the final unavailable entry. |
| Multiple non-empty floors | Two fake backends leave non-empty floors, followed by `Unavailable` | Preserve the most recent non-empty floor and its source; identify the unavailable entry. |
| Duplicate backend kinds | Two configured entries return the same `BackendKind`; the second is unavailable | Report the second entry's index and kind, distinguishing it from the first. |
| Exact call boundary | Counting fake backends before, at, and after the failure | Prior entries called at most once, failed entry called exactly once, later entries called zero times. |
| Serialization | Snapshot `Unavailable`, `Decided`, and `Abstain` | `Unavailable` has the two new fields; unaffected legacy variants retain their existing JSON shape. No deserialization support is added. |

Before the implementation PR, confirm all constructors/matches and JSON consumers in the repository are updated. Run focused crate tests and applicable Rust gates serially on the laptop/shared Cargo target; do not run workspace Cargo gates concurrently.

## Review and completion gates

- This PR is only a clarification proposal. All Agent24 PRs must be reviewed by the external background PR-Daemon; internal model reviews do not substitute for that review.
- Implementation may proceed only after external PR-Daemon APPROVE on this exact clarification head. The implementation remains a separate task PR targeting `ab/decide`, test-first, within the production-code size budget.
- The proposal does not implement L2/L3/L4, product call-site wiring, agent/memory/recall integration, ONNX/model downloads, hardware tiers, thresholds, network/cloud fallback, A-class safety/authz, or K1 product gates.
- Acceptance is review of the clarified L1 contract and test matrix only. It is not D1 completion, runtime integration, or release approval.

## Evidence basis

At baseline `37f9fde8fb0f6ee74485cf3b75e52e6de012d692`, `service.rs` iterates an ordered `Vec<Arc<dyn DecisionBackend>>`, remembers the latest non-empty floor, sets `Decision.backend` to `floor_backend.unwrap_or(kind)`, and returns terminal `Outcome::Unavailable { reason }` without separately representing the failing entry. `BackendKind` is an enum returned by each backend's `kind()` and is not a unique identifier for a vector position. PR #854 was approved by the external PR-Daemon and merged as `6e8929ca8e36b035dd043b829663c242e784ecde`.
