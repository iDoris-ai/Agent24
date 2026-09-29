# Open Design Integration and Main-Landing Strategy

Status: proposed for adoption

Date: 2026-09-29

## Goal

Keep Open Design development fast and continuously runnable without eventually
creating one very large pull request into `main`.

The integration work has two different needs:

1. A continuously usable branch where M4, M5, M6, and later slices can coexist,
   be packaged, and be exercised end-to-end.
2. Small, independently reviewable changes entering `main` with narrow blast
   radius, clear rollback points, and no unrelated integration history.

Those needs should be handled by different branch roles.

## Decision

Use two tracks in parallel.

### Track A — Integration / product validation

Maintain one long-lived branch for real product integration and test builds:

`integration/open-design-product`

The current temporary preview branch may continue to be used while this branch
is prepared, but it is not itself the preferred main-landing vehicle.

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
open one large `integration/open-design-product -> main` PR.

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
review small while M6+ is being built.

Review and close a stack bottom-up. Do not merge a child slice before its base
slice has either:

1. landed in the integration branch, or
2. landed in `main` and the child has been refreshed by a normal merge.

No force-push / rebase is required for already-published stacks.

## Current recommended landing order

The exact order can be adjusted when a dependency audit proves a slice is
independent, but the default order is:

### Product proof / M4

1. #563 — Creative `serve-web` launcher
2. #564 — embedded Creative `WebContentsView`
3. #565 — minimal Agent24 ACP bridge

These three should first be proven together on the integration branch and by a
real packaged preview. Their main landing should still be three small PRs (or
two only if the final diff is genuinely tiny and review remains clear), not one
combined M4 PR.

### Workspace authority foundation / M5

4. #555 — attached-route authority matrix, if the landing dependency audit
   confirms it is required by #556 on current `main`
5. #556 — Run / Session workspace identity contract
6. #558 — trusted workspace host resolve seam
7. #559 — atomic workspace run admission
8. #560 — adversarial admission coverage
9. #561 — workspace-aware terminal transition + exact lease release
10. #562 — startup orphan reconciliation
11. #566 — active run-lease lookup / rehydration
12. #567 — runtime workspace authority activation

The M5 development milestone is considered complete when these behaviors are
green on the integration branch. Main landing remains incremental in the order
above.

### M6

13. #568 — atomic `RunWorkspaceAuthoritySnapshot`
14. M6.1b — pinned opaque `WorkspaceHandle`
15. later M6 slices — ToolContext authority binding and use-time validation

M6 should continue development on top of the reviewed M5/M6 stack even while
the earlier landing PRs are being reviewed for `main`.

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
4. Merge that reviewed slice into `integration/open-design-product` with a
   normal merge and produce/update the runnable preview.
5. Continue immediately with the next M6+ slice; integration development does
   not wait for main landing.
6. In parallel, take the oldest fully reviewed dependency-ready slice and make
   a clean `land/open-design-*` branch from latest `main`.
7. Verify patch equivalence, rerun CI, review, and merge that small PR to main.
8. Repeat from the updated `main`.

This creates a conveyor belt rather than a final big-bang merge.

## What not to do

- Do not create one final multi-thousand-line Open Design PR to `main`.
- Do not rebase / force-push already-published reviewed stacks solely to change
  ancestry.
- Do not merge an existing integration branch into `main` if it carries
  unrelated history.
- Do not bypass exact-head review merely because the patch was already tested
  on the integration branch.
- Do not stop M6 development while older M4/M5 landing PRs wait for human
  review.

## Immediate adoption steps

1. Keep the current runnable preview branch for product validation while the
   permanent integration branch is established.
2. Fix the current Creative loading regression on the preview branch and prove
   the real packaged path again.
3. Create/refresh `integration/open-design-product` from current `main` and add
   reviewed Open Design slices to it without importing unrelated ancestry.
4. Start main landing with the smallest dependency-ready M4 slice.
5. Continue M6.1b development in parallel.

