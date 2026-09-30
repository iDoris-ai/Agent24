# Open Design Integration and Main-Landing Strategy

Status: adopted; refreshed for current execution

Date: 2026-09-30

## Goal

Keep Open Design development fast and continuously runnable without eventually
creating one very large pull request into `main`.

The integration work has two different needs:

1. A continuously usable branch where M4, M5, M6, and later slices can coexist,
   be packaged, and be exercised end-to-end.
2. Small, independently reviewable changes entering `main` with narrow blast
   radius, clear rollback points, and no unrelated integration history.

Those needs should be handled by different branch roles.

## Current milestone reality

The integration/product track has advanced substantially since this strategy
was first drafted:

- **M1-M4** prototype integration is complete. The embedded Creative shell,
  minimal ACP bridge, runtime adapter, and packaged positive-path acceptance
  were proven together before hardening continued.
- **M5** workspace authority activation is complete on the integration branch.
- **M6** workspace-bound execution/authority hardening is complete, including
  opaque pinned workspace authority, fresh revalidation, filesystem/shell
  isolation, approval/run ownership, and multi-workspace isolation.
- **M7 Recovery & Reliability is complete on the integration branch.** The
  durable approval/recovery train was landed bottom-up: #579-#584, #586,
  #592, #593, #595, and #596 are merged into the integration branch. The
  branch was then normal-merged with current `main` and is 0 commits behind it.

The development PRs listed in the older M1-M6 sections below are therefore no
longer "waiting to be reviewed" development work: the relevant slices through
#596 have been merged into
`integration/open-design-main-sync-wave20`. They are **not** thereby landed
wholesale into Agent24 `main`.

This distinction is important: development/recovery completion on the
integration branch and controlled landing into `main` are separate tracks.
The main-landing plan still starts with the smallest dependency-ready
prototype/foundation slices and preserves patch/tree equivalence; it must not
merge the long-lived integration branch wholesale.

Current status claims in this document are snapshots, not merge authorization.
Before acting on any numbered PR, verify its exact current base/head, CI, and
latest review verdict according to
`docs/design/OPEN-DESIGN-PR-REVIEW-PLAYBOOK.md`.

## Decision

Use two tracks in parallel.

### Track A — Integration / product validation

Maintain one long-lived branch for real product integration and test builds.
The current active branch is:

`integration/open-design-main-sync-wave20`

It remains a staging/development composition branch, not the preferred
main-landing vehicle. A future rename or replacement with a cleaner permanent
integration branch does not change this two-track policy.

This branch is allowed to contain multiple already-reviewed Open Design slices
at once so that we can continuously verify:

- Agent24 Desktop -> Creative Workspace rendering;
- Open Design `od daemon start --serve-web` lifecycle;
- ACP-over-stdio bridge;
- Agent24 Session / Run creation;
- workspace admission and lease lifecycle;
- authority snapshots and later pinned workspace handles;
- packaged macOS / Windows / Linux behavior as those paths become available.

The integration branch should regularly merge the latest `main` with ordinary
merge commits. Do not rebase or force-push published Open Design branches.

### Track B — Small landing PRs into `main`

Do **not** wait until the complete Open Design program is finished and then
open one large integration-branch-to-`main` PR.

Instead, once a slice has passed its exact-head review and CI, create a clean
landing branch from the latest `main`, bring over only that slice's reviewed
patch, rerun its required validation, and open a small PR directly to `main`.

Recommended naming:

`land/open-design-<slice-name>`

Example:

`land/open-design-creative-launcher`

After that PR merges, create the next landing branch from the **new** `main`.
This keeps every main PR small and naturally linearizes dependencies.

## Why not merge the current stacked branches directly to `main`?

The current Open Design development stacks were based on
`integration/open-design-main-sync-wave20`, which has substantial history that
is not equivalent to the current `main` history. Merging those published stack
branches directly can therefore pull unrelated integration ancestry into a
main PR even when the Open Design patch itself is small.

At the time this strategy was written:

- `main` was `bb1945d6f538c897311bca504623f2191a2e6c48`;
- `integration/open-design-main-sync-wave20` was
  `d62f65ad97c2c68a9ed8aff99407685b5046b28d`;
- their merge-base was `32072b02a3c71d50b7b841ad538e3c14073878d3`;
- the integration branch was about 321 commits ahead and 1 commit behind
  `main`;
- the whole integration-to-main tree diff was roughly 38k lines across 133
  files.

That is useful staging history, but it is not an acceptable unit of review for
main landing.

The published branches remain valuable as the reviewed source of truth. They
should not be rebased or force-rewritten merely to make main landing prettier.
Instead, construct separate landing branches.

## Patch-equivalence rule

A landing PR is not trusted merely because it was cherry-picked from a reviewed
branch. Before merge, verify that the landing patch is equivalent to the
reviewed slice.

For a one-commit slice, compare patch identity or equivalent diffs.

For a multi-commit slice, compare the complete base-to-head tree diff of the
reviewed slice with the complete base-to-head tree diff of the landing branch.

Then rerun the slice's required tests, formatting, clippy/lint, and CI on the
landing head.

If dependency adaptation is required because `main` has moved, treat the
adaptation as new code: keep it small, test it explicitly, and review the new
landing head rather than claiming it is identical to the old reviewed head.

## Merge policy for stacked development PRs

Development PRs may continue to use stacked bases because that keeps each
review small while M7+ is being built.

Review and close a stack bottom-up. Do not merge a child slice before its base
slice has either:

1. landed in the integration branch, or
2. landed in `main` and the child has been refreshed by a normal merge.

No force-push / rebase is required for already-published stacks.

## Current recommended landing order

The exact order can be adjusted when a dependency audit proves a slice is
independent, but the default order is:

### M1-M4 prototype train

1. #563 — **M1** Creative `serve-web` launcher
2. #564 — **M1** embedded Creative `WebContentsView`
3. #565 — **M2** minimal Agent24 ACP bridge
4. M3 fork-side runtime adapter — verify the pinned Open Design fork revision
   used by the integration branch; this is not an Agent24-main PR
5. M4 acceptance — rerun the packaged end-to-end prototype after #563-#565
   landing equivalents are green on current `main`

These three have already been proven together on the integration branch and by
the product prototype. Their main landing should still be three small PRs (or
two only if the final diff is genuinely tiny and review remains clear), not one
combined prototype PR.

### Workspace authority foundation / M5

Before #555 can land to `main`, audit and land the required **pre-#555
foundation** from wave20 as its own original atomic slices. In particular this
includes the capability authority/policy substrate, workspace
schema/allocation/registry/service substrate, and required sidecar/runtime
foundation that #555–#558 assume already exists. Do not hide this prerequisite
inside the #555 landing PR.

6. #555 — attached-route authority matrix, if the landing dependency audit
   confirms it is required by #556 on current `main`
7. #556 — Run / Session workspace identity contract
8. #558 — trusted workspace host resolve seam
9. #559 — atomic workspace run admission
10. #560 — adversarial admission coverage
11. #561 — workspace-aware terminal transition + exact lease release
12. #562 — startup orphan reconciliation
13. #566 — active run-lease lookup / rehydration
14. #567 — runtime workspace authority activation

The M5 development milestone is complete on the integration branch. Main
landing remains incremental in the order above.

### M6

15. #568 — atomic `RunWorkspaceAuthoritySnapshot`
16. M6.1b — pinned opaque `WorkspaceHandle`
17. later M6 slices — ToolContext authority binding, use-time revalidation,
    workspace filesystem/shell enforcement, approval ownership, and isolation

M6 is complete on the integration branch. Its main landing should still be
performed as reviewed atomic landing slices rather than as one combined M6 PR.

### M7 Recovery & Reliability

Continue the same conveyor-belt policy for recovery slices. The M7 development
sequence includes the recovery clock and durable approval/recovery work (#579
onward). At the latest 2026-09-30 refresh, #579-#584, #586, #592, #593, #595,
and #596 are merged into the integration branch. The final integration sync is
at `6d0b8b6` and is 0 commits behind current `main`; M7 has no remaining active
code slice on this branch.

Do not encode a stale "approved/blocked" label for each M7 PR into this
strategy. The operational source of truth is the exact current base/head plus
the latest exact-head review required by the PR review playbook.

## Integration branch acceptance gate

Before a slice is selected for main landing, the integration branch should
prove the positive path that the slice is intended to support. For M4–M6 this
means, as applicable:

- Desktop production build succeeds;
- packaged preview starts a real Agent24 daemon;
- Creative view loads a real Open Design web application;
- ACP creates a real Agent24 Session / Run;
- the same creative session supports a second turn;
- workspace-bound paths pass the relevant admission / lease / recovery tests;
- no raw filesystem path is promoted into public authority APIs.

Integration failures are fixed on a small reviewable development branch first,
then merged into the integration branch. Do not patch `main` only to make the
preview work.

## Main-landing acceptance gate

Each landing PR to `main` must have:

- one clearly named slice;
- a current-main base;
- documented dependency on previously landed slices only;
- patch-equivalence evidence to the reviewed development slice, or an explicit
  note that adaptation was required;
- focused tests and relevant full-crate/package tests;
- formatting/lint/clippy checks;
- platform CI required for that slice;
- a rollback boundary that does not require reverting later unrelated work.

For security-sensitive or authority slices above the normal review-size target,
retain the existing rule of two independent exact-head reviews.

## Recommended daily workflow

1. Develop the next small slice on a stacked feature branch.
2. Run focused + full relevant validation.
3. Obtain required exact-head review(s).
4. Merge that reviewed slice into the active integration/staging branch with a
   normal merge and produce/update the runnable preview.
5. Continue immediately with the next M7+ slice; integration development does
   not wait for main landing.
6. In parallel, take the oldest fully reviewed dependency-ready slice and make
   a clean `land/open-design-*` branch from latest `main`.
7. Verify patch equivalence, rerun CI, review, and merge that small PR to main.
8. Repeat from the updated `main`.

This creates a conveyor belt rather than a final big-bang merge.

## PLDM / main-reviewer execution checklist

This section is the operational instruction for the person reviewing and
merging Open Design work into Agent24 `main`.

### Phase 0 — approve the process

1. Review this document PR first.
2. Confirm the two-lane policy: staging/integration is for composition and
   product testing; `main` receives only small landing PRs.
3. Do **not** merge `integration/open-design-main-sync-wave20` or the current
   preview branch wholesale into `main`.
4. Do **not** ask the existing published stacks to be rebased/force-pushed.

### Phase 1 — inventory the missing foundation

1. Compare current `main` with the merge-base of the Open Design stack.
2. Identify only the pre-#555 capability/workspace/runtime slices that are
   actually required by #555/#556/#558.
3. For each required foundation slice, create or reuse a small landing PR from
   latest `main` and review it independently.
4. Merge those foundation PRs before attempting the dependent M5 authority
   train.

### Phase 2 — land the prototype train first

For #563, #564, and #565, in that order:

1. Create `land/open-design-<slice>` from the latest `main`.
2. Apply only that reviewed slice's delta.
3. Verify patch/tree equivalence against the reviewed development PR.
4. If adaptation to current `main` is needed, call it out explicitly and review
   the adapted landing head as new code.
5. Run the slice's focused tests plus the relevant full desktop/CLI/daemon
   validation.
6. Merge the small PR to `main` only after CI/review is green.
7. Create the next landing branch from the newly updated `main`.

After #563-#565 are landed, rerun the M4 packaged E2E gate on `main` plus the
pinned Open Design fork revision. This proves that the prototype still works
after clean main landing.

### Phase 3 — land M5 bottom-up

Once the required pre-#555 foundation is in `main`, process:

`#555 -> #556 -> #558 -> #559 -> #560 -> #561 -> #562 -> #566 -> #567`

For every item, use the same clean `land/open-design-*` procedure rather than
merging the old stacked branch directly.

### Phase 4 — land M6 and later slices continuously

After #567 is in `main`, continue the same conveyor belt:

`#568 -> M6.1b WorkspaceHandle -> later M6 slices -> M7 -> M8 -> M9 -> M10`

Do not wait for M10 and then create one final large PR.

### After every landing tranche

1. Merge the new `main` back into the long-lived Open Design staging branch by
   a normal merge commit.
2. Rebuild the packaged preview and rerun the relevant cross-stack E2E path.
3. Continue development from the staging/development stack while the next
   small landing PR is under human review.

### Stop conditions

PLDM should stop the landing train at the current slice if any of these occurs:

- the landing diff contains unrelated wave20 history;
- patch-equivalence cannot be demonstrated and the adaptation is not separately
  reviewed;
- required parent/foundation behavior is absent from current `main`;
- focused or required platform CI is red;
- the landing PR is growing into a multi-slice change that no longer has a
  narrow rollback boundary.

## What not to do

- Do not create one final multi-thousand-line Open Design PR to `main`.
- Do not rebase / force-push already-published reviewed stacks solely to change
  ancestry.
- Do not merge an existing integration branch into `main` if it carries
  unrelated history.
- Do not bypass exact-head review merely because the patch was already tested
  on the integration branch.
- Do not stop M7+ development while older M4/M5/M6 landing PRs wait for human
  review.

## Immediate adoption steps

1. Keep `integration/open-design-main-sync-wave20` as the current composition
   branch while controlled main landing proceeds.
2. Preserve the proven packaged M4 positive path as a regression gate while
   recovery/productization work continues.
3. Merge current `main` into the integration/staging branch only with normal
   merge commits when needed; do not rebase/force published stacks.
4. Audit the pre-#555 wave20 foundation and turn its required pieces into a
   concrete main-landing queue of small PRs.
5. Start main landing with the smallest dependency-ready M4 slice while the
   foundation queue is prepared in parallel.
6. After each main landing tranche, merge the new `main` back into staging and
   rerun cross-stack CI.
7. Continue M7 Recovery & Reliability in parallel, then M8-M10
   productization/release work.
