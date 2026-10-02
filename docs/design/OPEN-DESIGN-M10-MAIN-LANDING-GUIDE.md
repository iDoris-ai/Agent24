# Open Design M10 Main-Landing Guide

Status: active M10 execution guide
Date: 2026-10-02
Target branch: `main`

This document is the operational companion to
`docs/design/OPEN-DESIGN-INTEGRATION-LANDING-STRATEGY.md`.
The strategy document defines *why* Agent24 uses separate integration and
main-landing tracks. This guide defines *how to finish M10* without turning the
already-reviewed Open Design history into one large merge.

## 1. M10 completion rule

M10 is **not complete** when one prototype PR reaches `main`.

M10 is complete only when all of the following are true:

1. the prototype landing train is in `main` as small reviewed PRs;
2. the M4 packaged E2E path is re-proven on `main` plus the pinned Open Design fork;
3. the minimum pre-#555 capability/workspace/runtime foundation is in `main`;
4. the M5 authority train is in `main` bottom-up;
5. the reviewed M6, M7, M8, and M9 slices needed by the shipped product are landed
   incrementally;
6. release packaging and cross-platform/security gates pass from current `main`;
7. no wholesale merge of
   `integration/open-design-main-sync-wave20` was used.

## 2. Frozen architecture constraints

Every landing must preserve these constraints:

- Agent24 is the Shell and authority owner.
- Open Design is the Creative Workspace.
- The initial bridge is ACP-over-stdio.
- Do not prematurely activate `run()`.
- Do not expose `canonical_root` as public authority.
- Do not construct `ToolContext` from an untrusted raw path.
- Creative has no host/admin authority.
- Workspace authority must remain opaque, pinned, and fail-closed where the
  reviewed integration design requires it.

A landing that violates one of these rules stops the conveyor even if its local
tests pass.

## 3. Live checkpoint

As of 2026-10-02:

| Item | State | Exact checkpoint |
| --- | --- | --- |
| Main CI race baseline | merged | #647 -> `4f79adfd505f9196f2eb196efc54df4e9e59a330` |
| #563 clean landing | merged | #643 -> `0b88049d48fc1de7c1319cf9118e84a62584a659` |
| #564 clean landing | active | #651 head `e55f920127d81b3d4ab0f9ea1a7cc81151eb9f09`, based on `main@af5862141d3d6a2658be7ea70f34ef43d055f430` |
| #565 clean landing | preflight only | historical reviewed head `fe4ae95dffcf0096168289ba4189415f3693a54a`; rerun the probe after #651's final main refresh before opening the landing PR |
| pre-#555 foundation inventory | complete | use F1-F6 below |
| M5+ main landing | not started | must wait for required foundation |

#643 was refreshed after #647 by a normal merge from current `main`, passed
fresh CI/review, and merged. Its net feature diff was exactly two files / +334
lines, and the two historical #563 blobs were:

- `creative-serve-web.ts`: `2676af28b8746c985f2916fb77c7ff7e8a957c6f`
- `creative-serve-web.test.ts`: `784073733ec1136f435ce1efec411294788e85c7`

Treat this table as a checkpoint, not permanent merge authorization. Before
every merge, re-read the PR exact head, CI, and review verdict.

## 4. Non-negotiable landing policy

For every Open Design landing PR:

1. start from the latest `origin/main`;
2. land one clearly named slice;
3. depend only on slices already present in `main`;
4. prove patch/tree equivalence to the reviewed historical slice;
5. if current-main adaptation is required, label it as new code and review it;
6. run focused tests plus relevant full validation;
7. run formatting/lint/clippy/typecheck as applicable;
8. require the relevant platform CI;
9. preserve a narrow rollback boundary;
10. merge normally; do not rebase or force-push published branches.

Security-sensitive authority slices and unusually large slices keep the existing
independent exact-head review rule. A slice above the normal size target must
not silently grow into a multi-feature PR.

## 5. Phase A — finish the prototype train

### A1. Land #643 / historical #563

Source slice: Creative serve-web launcher.

**Checkpoint: completed.** #643 merged as
`0b88049d48fc1de7c1319cf9118e84a62584a659`. Keep the checklist below as the
evidence standard for any future refresh/revert rather than reopening this
slice without a concrete regression.

Merge only when the refreshed #643 head has:

- exact two-file / +334 net diff against current `main`;
- the two historical blob hashes listed above;
- fresh CI on the refreshed exact head;
- valid exact-head approval/review.

Do not include the separate uncommitted launcher-hardening experiment from the
old local worktree in #643.

### A2. Land historical #564

Source slice: embedded Creative `WebContentsView`.

Current landing PR: #651. Exact-head landing review found two historical
lifecycle races around delayed first startup / replacement startup; the current
landing carries the narrow generation fence + ready-origin bounds guard derived
from later reviewed #616 rather than pulling the rest of #616 forward.

Use the **historical PR net patch**, not only commit `5209c77`.
The historical slice contains propagated ancestry; cherry-picking only its final
commit is not a valid reconstruction.

Expected landing scope:

- `apps/desktop/src/main/main.ts`
- `apps/desktop/src/main/preload.ts`
- `apps/desktop/src/renderer/App.tsx`
- `apps/desktop/src/renderer/pages/Creative.test.tsx`
- `apps/desktop/src/renderer/pages/Creative.tsx`
- `apps/desktop/src/renderer/styles.css`
- `apps/desktop/src/shared/ipc-types.ts`

Known current-main adaptations must preserve:

- DEP-A5 daemon behavior in `main.ts`;
- AUDIT-3 disabled capability-card styling in `styles.css`.

Those adaptations require fresh exact-head review.

### A3. Land historical #565

Source slice: minimal Agent24 ACP bridge.

Do **not** merge the historical feature tree wholesale. The feature branch is
not actually descended from the displayed #564 merge base.

Use either the GitHub PR net patch or only these feature commits:

`ca1c9a4 -> 8f69683 -> fe4ae95`

Preserve current-main behavior in `agent24-cli`:

- keep the current CLI package version;
- keep `rustix` / COMM dependencies;
- keep all current COMM commands;
- add only the ACP module/command/dispatch wiring;
- add `io-std` to Tokio features as required;
- keep `acp.rs` and `agent24d/src/events.rs` patch-equivalent where possible.

Do not add later `workspace_id: None` compatibility until workspace identity
actually exists in `main`.

## 6. Phase B — rerun the M4 packaged E2E gate

After the #563/#564/#565 landing equivalents are all green in `main`, prove
the real product path again using the pinned Open Design fork revision:

The Open Design compatibility contract is version `0.22.2`, but the integration
tracking branch `integration/agent24-open-design-v0.22.2` is mutable. Do **not**
treat the branch name or a developer's local checkout HEAD as an immutable pin.
Before running the M4 gate, fetch the fork, choose the exact reviewed commit,
record its full SHA in the validation evidence, and run the whole gate against
that SHA. If the exact fork SHA is not recorded, the M4 gate is incomplete.

1. build the production/packageable desktop;
2. start a real Agent24 daemon;
3. start the real Open Design web application;
4. render Creative inside Agent24 Desktop;
5. establish ACP-over-stdio;
6. create a real Agent24 Session and Run;
7. complete a second turn in the same creative session;
8. verify no host/admin authority is granted to Creative;
9. verify no raw filesystem path becomes public workspace authority.

A failure here is fixed in a small reviewable slice first. Do not patch around
it directly in an unrelated later landing.

## 7. Phase C — minimal pre-#555 foundation dependency groups

The historical pre-#555 integration history is much larger than the actual
dependency set. Do **not** copy that history wholesale. F1-F6 below are
**dependency groups, not six large PRs**. Preserve the listed small historical
slices where possible. Keep the normal <=300-line target; 301-500-line adapted
slices require two independent exact-head reviews, and >500-line landings should
be split unless a documented exception preserves a narrow rollback boundary.

### F1. Capability authority core

Land the reviewed capability authority work in small slices, preserving the
original sequence where dependencies require it:

`#229 -> #230 -> #231 -> #232 -> #233 -> #236 -> #237`

This supplies scoped claims, digest store/watch, mint/validate/revoke authority,
tests, and modular wiring. #226/#227/#228 are already represented in current
main and must not be re-landed.

### F2. Capability runtime and closed route policy

Then land:

`#238 -> #239 -> #240`

This supplies private host bootstrap transport, safe capability startup/AuthMode,
and the closed required-operation route policy required by the attached-route
authority matrix.

#241's mint/revoke HTTP API and #242's broader isolation-test slice are not part
of the minimum #555 dependency set unless a fresh current-main dependency audit
proves otherwise. If #555 needs only the historical reusable test helper, adapt
that helper locally and review the adaptation rather than importing unrelated
route/API scope.

### F3. Workspace contract and persistence skeleton

Land the minimum workspace contract/schema needed by #556/#558:

- #245/#246 WorkspaceId contract and validation;
- the reviewed final workspace schema derived from #248 and its hardening;
- #363 allocation journal schema;
- #372 dormant workspace bindings.

Use the final migration sequencing appropriate to current `main`:

- `0009_workspaces`
- `0010_workspace_allocations`
- `0011_workspace_bindings`

Do not blindly reuse early historical migration numbers; current `main`
already owns intervening migrations.

### F4. Workspace registry core

Preserve the reviewed registry/create API needed by #558 fixtures:

`#263 -> #264 -> #265 -> #266 -> #267 -> #270 -> #280 -> #286 -> #293`

GET/list projection-only ancestry is not automatically required. Omit unrelated
history when a dependency audit proves it is unnecessary.

### F5. Allocation read evidence

Land the minimum read-side allocation evidence:

`#371 -> #373`

Do not pull later reservation/materialization/commit/retention writers into this
tranche merely because they existed in the historical integration base.

### F6. Workspace filesystem/root proof

Land the workspace service/root primitives required by #558:

- #359 service crate skeleton;
- #418 POSIX root authority;
- #420 tests when retaining the reviewed proof;
- #458 pinned allocation root identity.

The historical #458 slice is large enough to deserve explicit exact-head size
and security review during adaptation.

## 8. Phase D — land M5 bottom-up

Only after the required F1-F6 prerequisites are present in `main`, process:

`#555 -> #556 -> #558 -> #559 -> #560 -> #561 -> #562 -> #566 -> #567`

Important dependency notes:

- #555 is not a compile prerequisite for #556; the order is retained for the
  reviewed architecture/landing history.
- #556 is not a direct compile prerequisite for #558; preserve the order unless
  a new dependency review intentionally changes it.
- do not pull `agent24-sidecar-host-protocol` into the pre-#555 tranche merely
  because it exists on integration; #555/#556/#558 do not reference it.

## 9. Phase E — M6 through M9 clean landings

After #567, continue the same procedure for the reviewed M6, M7, M8, and M9
slices. The lists below are **historical source trains for clean current-main
equivalents**, not instructions to merge old stack branches directly.

### M6 source train

`#568 -> #570 -> #573 -> #574 -> #575 -> #576 -> #577 -> #578`

### M7 source train

Start from the exact-parent order of #579/#580, then continue through the
recovery/reliability chain:

`#581 -> #582 -> #583 -> #584 -> #586`

Compatibility/default repairs include #591 and #603. The later recovery chain
continues:

`#592 -> #593 -> #595 -> #596 -> #631`

#605 is migration-test hygiene, not a feature milestone. Re-audit exact parents
before turning this list into landing branches.

### M8 source train

`#606 -> #615 -> #616 -> #623 -> #629 -> #632`

#604 is validation-baseline hygiene rather than the productization feature train.

### M9 source train

`#530 -> #531 -> #533 -> #535 -> #536 -> #537 -> #538 -> #539 -> #542 -> #545 -> #546 -> #547 -> #548 -> #549 -> #551 -> #553 -> #552 -> #554`

#644 is test-evidence repair, not a feature milestone.

Do not compress a whole milestone into one landing PR just because that
milestone is already closed on the integration branch.

For each slice:

- identify its exact reviewed historical base/head;
- identify the minimum current-main dependency;
- reconstruct only its net patch;
- preserve unrelated current-main changes;
- explicitly review any adaptation;
- run the slice's platform/security gates;
- merge from the new `main` before preparing the next dependent slice.

M9 ProcessKit/Windows work remains frozen to the reviewed pin unless a later
main adaptation proves a concrete reason to change it. ProcessKit/Windows CI is
evidence for those reviewed paths; it is **not by itself proof of complete
Windows product/distribution readiness**. Final release claims must also match
the current Deployment document, signed-artifact availability, and explicitly
supported release platforms.

## 10. Per-PR operator checklist

### Before constructing a landing

- [ ] `git fetch origin main`
- [ ] record current `origin/main` SHA
- [ ] record reviewed historical PR base/head
- [ ] verify all required parents already exist in `main`
- [ ] estimate final diff size before coding
- [ ] choose a fresh `land/open-design-<slice>` branch/worktree

### Before opening or refreshing the PR

- [ ] compare net patch/tree with the reviewed source
- [ ] document every current-main adaptation
- [ ] `git diff --check`
- [ ] run focused tests
- [ ] run relevant full crate/package tests
- [ ] run fmt/lint/clippy/typecheck as applicable
- [ ] push without force

### Before merge

- [ ] exact head has not changed since review
- [ ] required CI is green on that exact head
- [ ] review decision is valid for that exact head
- [ ] security/authority slices have the required independent review count
- [ ] PR has one rollback boundary and no unrelated integration history
- [ ] merge method is a normal repository-approved merge, never rebase/force

### Immediately after merge

- [ ] fetch and record new `origin/main`
- [ ] verify the merged tree contains only the expected slice
- [ ] start the next dependent landing from the new `main`
- [ ] normal-merge new `main` back into the integration/staging branch when
      that tranche needs cross-stack validation
- [ ] rerun the relevant packaged/cross-stack E2E gate

## 11. Stop conditions

Stop the current landing slice and repair it before continuing if:

- the diff contains unrelated wave20/integration history;
- patch equivalence cannot be shown and the adaptation lacks fresh review;
- a required foundation/API is absent from `main`;
- required focused tests or platform CI are red;
- an authority boundary becomes broader than the reviewed design;
- the PR grows beyond one reviewable rollback unit;
- migration numbering collides with current `main`;
- packaged E2E only works by bypassing the pinned/opaque authority model.

Do not stop the entire M10 program merely because one later PR waits for human
review. Continue preparing independent dependency-ready work.

## 12. Rollback rule

Every landing PR must be revertable without reverting unrelated later work.

If a regression is found after merge:

1. identify the smallest landing slice that introduced it;
2. stop dependent landings;
3. revert or repair that slice on top of current `main`;
4. rerun its focused and cross-stack gates;
5. resume the conveyor only after the repaired boundary is green.

Never solve rollback pressure by merging the long-lived integration branch.

## 13. Final M10 release gate

M10 can be closed only after a final current-main validation records:

- complete incremental landing ledger;
- packaged desktop production build;
- real Agent24 daemon lifecycle;
- real Open Design packaged resource discovery/startup;
- Creative render/navigation/session isolation;
- ACP Session/Run + second-turn E2E;
- workspace admission/lease/recovery/security paths;
- M9 cross-platform/Windows and ProcessKit gates where required for the
  explicitly supported artifact/platform set;
- no hidden Creative host/admin authority;
- no public `canonical_root` authority leak;
- no raw untrusted path -> `ToolContext` construction;
- release packaging/checklist passes, including any still-open Deployment
  blockers for signed/distributable artifacts;
- no unresolved release-blocking CI/review finding.

Only then mark the Open Design integration roadmap's M10 milestone complete.
