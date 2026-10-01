# Open Design Stacked-PR Review Playbook

Status: proposed for PRDaemon / independent reviewer adoption

Date: 2026-09-29

## Purpose

This document tells PRDaemon and other independent reviewers how to review the
Open Design development pull requests without accidentally reviewing the whole
integration ancestry as if it were one change.

It is intentionally separate from
`OPEN-DESIGN-INTEGRATION-LANDING-STRATEGY.md`:

- this document defines **how stacked development PRs are reviewed**;
- the landing strategy defines **how reviewed slices are reconstructed and
  landed incrementally onto current `main`**.

Review and landing are related, but they are not the same operation.

## Core rule: review the slice, not the accumulated stack

Each stacked PR is an atomic development slice. Review exactly the tree delta
between that PR's declared base and its exact head:

```text
<slice-base-sha>..<slice-head-sha>
```

Do not substitute any of these for the slice diff:

- `main..<child-head>`;
- `integration/open-design-*..<child-head>`;
- the complete ancestry of a child PR;
- a previous review of an older head SHA.

For every review, record both exact SHAs. A valid verdict is attached to that
pair only.

## Reviewer operating rules

1. Review stacked PRs **bottom-up**.
2. Verify the PR base branch and exact base/head SHAs before reading the diff.
3. Read only the base-to-head slice first. Inspect surrounding code only when
   needed to judge an invariant or dependency.
4. Treat a child PR as depending on the reviewed behavior of its parent, not as
   permission to ignore parent assumptions.
5. If a parent or child head changes, review the new exact head. An older
   `FINAL PASS` does not automatically carry forward.
6. Fixes on published branches use normal follow-up commits. Do not request a
   force-push, history rewrite, or rebase merely to make the stack prettier.
7. Do not merge or reconstruct a giant stack just to review it.
8. Keep review findings scoped to the current slice. If a finding belongs to a
   parent slice, identify the dependency explicitly and stop the child review
   until the parent contract is corrected.

## Size and independent-review rule

The Open Design integration uses this review-size discipline:

- target: roughly 200-300 changed lines;
- 301-500 changed lines: allowed only when the slice is still atomic and must
  receive **two independent exact-head reviews**;
- more than 500 changed lines: stop and request a split unless an exceptional
  reason is documented and approved.

Count the true base-to-head changed lines. Do not accept artificial line-count
reduction through compressed tests, `#[rustfmt::skip]`, generated formatting
tricks, or unrelated file movement.

For a 301-500 line slice, both reviewers must independently inspect the same
exact base/head pair. One review plus a summary of the other is not equivalent
to two independent reviews.

## Review order

### A. Prototype / product train

Review in this order:

1. #563 — M1 Creative `serve-web` prototype launcher
2. #564 — M1 embedded Creative `WebContentsView`
3. #565 — M2 minimal Agent24 ACP bridge

M3 is the fork-side Open Design runtime adapter and is not an Agent24 PR. Review
that change in the Open Design fork at its pinned revision.

M4 is an end-to-end acceptance gate, not a standalone Agent24 code PR. After
#563-#565 and the M3 fork adapter are reviewed together in the integration
product build, rerun the real product path:

```text
Agent24 Desktop
  -> embedded Open Design
  -> Agent24 runtime / agent24 acp
  -> real Session / Run
  -> artifact / preview
  -> second-turn edit continuity
```

Do not interpret M4 acceptance as evidence that M1/M2 code has already landed
on `main`.

### B. Workspace authority / M5 train

Review in this order:

1. #555 — attached-route authority matrix
2. #556 — Run / Session workspace identity propagation
3. #558 — trusted workspace host resolve seam
4. #559 — atomic workspace run admission
5. #560 — adversarial admission hardening
6. #561 — atomic terminal run-lease release
7. #562 — startup workspace orphan reconciliation
8. #566 — active workspace run-lease lookup / rehydration
9. #567 — runtime workspace authority activation

Important distinction: the development stack can be reviewed slice-by-slice
using each PR's declared exact base even though current `main` does not yet
contain all pre-#555 foundation. **Landing** #555+ into `main` still requires a
separate inventory and landing of the necessary pre-#555 foundation, as defined
by the main-landing strategy.

### C. Workspace authority / M6 train

Continue from the reviewed M5 head:

1. #568 — atomic `RunWorkspaceAuthoritySnapshot`
2. #570 — opaque pinned `WorkspaceHandle`
3. M6.2 — ToolContext workspace authority binding
4. later M6 slices — use-time fs/shell authority, approval/grant scope,
   audit/event identity, and dual-workspace isolation.

For newly opened M6 PRs, append them to this list only after their exact base,
head, scope, test evidence, and required reviewer count are known.

## What to check on every PR

### 1. Identity and topology

Verify and record:

- PR number and title;
- declared base branch;
- exact base SHA;
- exact head SHA;
- changed files and true changed-line count;
- whether the current head differs from the last reviewed head.

If the base branch shown by GitHub does not match the expected parent slice,
stop before reviewing behavior.

### 2. Scope integrity

Confirm that the diff contains one coherent slice and no unrelated ancestry or
cleanup. A child PR may naturally depend on its parent, but its review diff
must not silently absorb additional unrelated work.

If a necessary adaptation is new code, review it explicitly. Do not label it
"already reviewed" simply because a nearby parent behavior was reviewed.

### 3. Validation evidence

Check the focused tests that prove the slice contract and the relevant full
crate/package suite. Require formatting and lint/clippy checks where applicable.

Platform-sensitive changes must have the relevant platform CI. If CI fails in
an unrelated subsystem, classify the failure with evidence; do not patch an
unrelated subsystem into the current PR merely to make the status green.

### 4. Public API surface

For authority/security slices, explicitly inspect every newly public type,
constructor, accessor, serialization surface, debug representation, and error
path. The review should answer whether a caller can:

- forge authority;
- downgrade bound authority to an unbound/legacy state;
- expose a raw or canonical workspace path;
- reuse stale point-in-time authority after an approval wait or lifecycle
  transition;
- cross workspace, run, session, lease, root generation, or ownership
  boundaries.

### 5. Failure atomicity and fail-closed behavior

For store/runtime authority work, adversarially inspect:

- transaction boundaries;
- rollback on corruption or trigger failure;
- complete history versus active-row-only queries;
- missing/duplicate/released lease history;
- run/input/session workspace mismatch;
- stale generation/root binding;
- TTL and clock precision;
- terminal versus nonterminal transitions;
- restart/orphan behavior;
- restored approval execution.

Reject paths that turn an authority-validation error into silent legacy or
`None` behavior for a workspace-bound run.

## M6-specific adversarial focus

### #568 — atomic authority snapshot

Verify one read transaction observes a self-consistent run/session/lease/
workspace/allocation/root state, strict row decoding is fail-closed, current
time cannot move backward relative to recorded authority evidence, and no raw
workspace root becomes public API.

### #570 — pinned WorkspaceHandle

Verify the complete mint chain on the exact head:

```text
fresh S1
  -> derive managed locator
  -> exact root pin
  -> fresh S2
  -> require S1 == S2
  -> post-S2 binding revalidation
  -> opaque handle
```

The review must include millisecond-precision freshness/expiry behavior. Check
that `WorkspaceHandle` has no public constructor, serde, revealing `Debug`, raw
path getter, or cross-platform fallback that bypasses the pinning invariant.

### M6.2 — ToolContext workspace authority binding

Verify structural binding only. This slice should not yet widen into fs/shell
workspace-root consumption.

Check that:

- `ToolContext` cannot be externally constructed with an optional workspace
  authority that permits Bound -> None downgrade;
- legacy and bound states are explicit;
- `WorkspaceRunAuthority` carries immutable run + lease identity and mints a
  fresh handle by revalidating current authority;
- normal tool execution and restored approval execution use the same authority
  construction path;
- Explorer/subagent child contexts inherit the bound authority rather than
  rebuilding or dropping it;
- non-Unix or missing-service bound execution fails closed without preventing
  the daemon from starting for legacy workloads;
- debug/error output does not leak authority internals or paths.

## Parent changes and follow-up fixes

When review finds a defect:

1. report the finding against the exact reviewed head;
2. request the smallest normal follow-up commit on the same published branch;
3. do not request amend/rebase/force-push;
4. rerun the focused regression and required full validation;
5. review the **new exact head**;
6. record `FINAL PASS` only for that new head.

If the parent slice changes in a way that changes a child assumption, the child
must be revalidated against its new effective parent before the child's prior
review can be treated as current.

## Review roles

A slice may need more than one review angle. Keep the verdicts independent:

- **design/contract review** — API shape, dependency boundary, smallest safe
  slice;
- **security/adversarial review** — fail-closed behavior, authority isolation,
  corruption/race/TOCTOU paths;
- **platform review** — Unix/Windows/macOS/Linux conditional compilation and
  lifecycle behavior;
- **product/E2E review** — positive path on the integration build.

For 301-500 line authority/security slices, two independent exact-head reviews
are required even when CI is green.

## PRDaemon verdict format

For each review, report a short machine- and human-readable record:

```text
PR: #<number>
Base: <exact-base-sha>
Head: <exact-head-sha>
Changed: <files>, <insertions>/<deletions> (<true changed lines>)
Review focus: <contract/security/platform/product>
Validation checked: <tests/CI>
Verdict: FINAL PASS | FINDING

Findings:
- <severity> <file/area>: <concrete invariant violation and smallest fix>
```

Do not output `FINAL PASS` if the exact head changed while the review was in
progress.

## Stop conditions

PRDaemon should stop the current review and ask for correction or clarification
when any of these is true:

- the PR's actual base is not the expected parent slice;
- the claimed head SHA differs from the current remote head;
- the diff includes unrelated ancestry or broad extra scope;
- the slice is over 500 changed lines without an explicitly approved exception;
- a 301-500 line slice lacks the required second independent exact-head review;
- required focused/full validation is missing;
- authority-sensitive platform CI is missing or genuinely failing in touched
  code;
- a workspace-bound path silently falls back to legacy/unbound authority;
- a parent finding invalidates an assumption used by the child;
- landing adaptation is being presented as patch-equivalent without being
  reviewed as new code.

An unrelated, evidenced baseline/flake failure may be recorded separately and
need not force unrelated code into the slice.

## Relationship to main landing

After PRDaemon has a valid exact-head review record, use
`OPEN-DESIGN-INTEGRATION-LANDING-STRATEGY.md` for the separate main-landing
process:

- create a current-main landing branch;
- bring over one reviewed slice;
- verify patch/tree equivalence;
- treat current-main adaptations as new code;
- rerun validation on the landing head;
- merge one landing PR at a time;
- merge updated `main` back into the integration branch normally and rerun the
  cross-stack product checks.

Never merge the long-lived Open Design integration branch wholesale into
`main` merely because all child PRs were reviewed.

## Summary for PRDaemon

Use the stacked PRs as the development source of truth. Review them bottom-up,
one exact base/head slice at a time. Keep the product train and workspace
authority train explicit. Invalidate old verdicts when heads change. Require
two independent exact-head reviews for 301-500 line slices. Ask for normal
follow-up commits for findings. Then hand the reviewed slice to the separate
main-landing playbook instead of merging the development stack directly.
