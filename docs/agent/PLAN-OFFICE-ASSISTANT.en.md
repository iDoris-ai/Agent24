# PLAN-OFFICE-ASSISTANT — OfficeAssistant Product Plan (Draft)

> **Translation status**: machine-drafted English translation of [`PLAN-OFFICE-ASSISTANT.md`](PLAN-OFFICE-ASSISTANT.md); native review pending. Where this translation and the Chinese version conflict, **the Chinese version prevails**. Terminology follows [`research/T007-office-assistant-report.md`](research/T007-office-assistant-report.md); see [`OFFICE-ASSISTANT-TRANSLATION-NOTES.md`](OFFICE-ASSISTANT-TRANSLATION-NOTES.md) (Chinese) for the glossary.

> Status: **planning draft, pending review**. Written 2026-10-16, based on branch `docs/office-assistant-plan-20261016` (based on that day's main, tip `92f9f0cf`).
> This document **is only a plan**: no item in it has been implemented, acceptance-tested, or registered as a project in [`tasks.md`](tasks.md).
>
> - Single source of truth (capabilities/interaction/permissions/acceptance): [`research/T007-office-assistant-report.md`](research/T007-office-assistant-report.md) (hereafter "the report", v0.2, 2026-10-16). The Chinese version `report.zh-CN.md` referenced on line 3 of the report is not in this worktree; this document cites sections of the English version.
> - Technical design: [`OFFICE-ASSISTANT-TECH-DESIGN.en.md`](OFFICE-ASSISTANT-TECH-DESIGN.en.md); intern brief (English): [`OFFICE-ASSISTANT-INTERN-BRIEF.md`](OFFICE-ASSISTANT-INTERN-BRIEF.md); index: [`OFFICE-ASSISTANT-INDEX.md`](OFFICE-ASSISTANT-INDEX.md) (Chinese only; the Chinese version prevails).
> - **This plan does not change mainline priorities**: the main track still follows the rulings in [`COMPONENT-ROADMAP.md`](COMPONENT-ROADMAP.md) §3 and [`tasks.md`](tasks.md) (see §0.2). This plan requires jason's explicit approval before work begins; until approved it is only "a plan available for review".

---

## 0. Preconditions and Constraints (Know These Before Reading)

### 0.1 Four hard boundaries for this topic

1. **Agent24 is the sole execution control plane.** Runs, tool approvals, audit, and workspace permissions belong to the kernel; OfficeAssistant must not open a separate execution channel that bypasses the kernel (report §1 "System relationships" table, Agent24 row; §5 "Inherited safety and reliability requirements" item 1).
2. **Do not introduce a second general-purpose Agent framework**, and do not add a database merely because the historical prototype mentioned LangGraph/Postgres (report §4 "Recommended trade-offs").
3. **Do not require Sin90/Cos72 to be loaded first.** Basic office capabilities must not presuppose a domain OS (report §1 "Structure already confirmed by the author" and the paragraph after it).
4. **A PR that changes observable behavior must name the affected laws**, and keep code, tests, and laws consistent; law changes must be in a separate PR ([`../laws/README.md`](../laws/README.md) "closing the loop with PRs").

### 0.2 Relationship to the mainline and K1 (checked against repository files)

| Fact | Basis | Implication for this plan |
|---|---|---|
| The 2026-10-03 ruling set the main track = C3 P0 (M1); M1 was merged into main on 2026-10-07 (#720); on the roadmap C3 P1 "immediately follows P0 and is the next stop of the main track" | `COMPONENT-ROADMAP.md` §3 table, "B (mid-term)" row; `tasks.md` lines 21–25, line 372 | This topic **must not crowd out** C3 P1's review and merge cadence |
| `tasks.md` also records "first item after M1 = DM-SPIKE" (2026-10-06) | `tasks.md` line 24 | The statement "the current main track is C3 P1" comes from the author verbally; in the repository files the ordering of C3 P1 and DM-SPIKE has **not been verified against the latest ruling**. Both take priority over this topic, which does not affect this plan's conclusions |
| K1 (consent gate, module tool audit, source classification, LocalOnly egress narrowing) has been merged into main with #1022 (squash `56264ef9`) | `K1-RELEASE-NOTES-20261016.md` lines 1–35 | OfficeAssistant must be built on top of K1 and must not invent its own authorization/audit |
| But the K1 ledger in `tasks.md` still says `PLANNED` / `BLOCKED` | `tasks.md` lines 42–56 | The ledger lags behind the merge facts; the release notes and code prevail. This document does not modify the ledger |
| After K1 merged: **production code has no route that grants module consent**; `CloudAuthorized` has no production write point; `http_fetch`/`shell_exec` have no unified egress decision yet (6b.3 of ADR-K1-02 §6 not done) | `K1-RELEASE-NOTES-20261016.md` lines 27, 31, 33 | **Real external writes in M2 are blocked by kernel prerequisites** (see §4.3 "Dependencies"); M1 is unaffected |
| The source tag for user input is fixed as `LocalOnly`; the main conversation, `/api/v1/chat`, explorer, and compaction do not fall back to remote | Same, line 31, lines 149–158 | Minutes extraction in M1 can only use local models; when a local model is unavailable the task must fail explicitly (consistent with the requirement in report §4, paragraph 3) |

### 0.3 Matters this document does not decide on the author's behalf

The decisions that report §7 explicitly leaves to the author are **only listed here as a list of decisions pending (§7)**, with no answers presupposed: who the first version serves, the first real connector (email or Notion) with its account and test destination, the form of the entry point, whether community bulk messaging/LINE automatic replies are included, and how to balance individual confirmation against bounded delegation.

### 0.4 Decisions already taken (author, 2026-10-16)

| # | Decision | Meaning and impact |
|---|---|---|
| **D-A** | **Hang point is option B**: create a first-party **out-of-process domain-OS module `office`** (isomorphic to `rust/apps/agent24-documents/`) to hold the business state | Business records (workflow/plan versions, artifact versions, human edits, SOPs, receipt projections, mock connectors) live in the `office` module's own `data_dir`; **they do not enter the kernel store, and no store migration is introduced**. Precedent: `docs/documenting/adr/ADR-DOC-01-placement-and-integration.md` ruled the same way for Documenting (Accepted) |
| **D-B** | **Actions with external effects stay in the kernel**; a new kernel gate **OA-K** sits between the module and the kernel | OA-K accepts `(tool, exact input, request_id)`; the kernel runs the full pipeline for it (consent → per-call human review → egress gate → timed execution → audit) and returns a typed result plus a receipt. **The kernel never retries automatically.** Detailed design: [`OA-K-KERNEL-GATE-DESIGN.md`](OA-K-KERNEL-GATE-DESIGN.md); it needs an ADR and should be **folded into #735** (Documenting and Cos72 need it too) |
| **D-C** | OA-K is **not** specific to OfficeAssistant | Its ADR and implementation therefore must not sit on the OA timeline; **OA-M2 waits for it** |

**Three consequential adjustments:**
- `OA-M1-04` moves from "inside the kernel store, including migration" to "**the `office` module's own `data_dir`**" (risk drops from High to Medium; it no longer triggers PR-Daemon's mandatory review of "storage migrations").
- `OA-M1-05` changes from "wire the extraction step into a run" to "**go through the `Models` callback** (local model, LocalOnly)"; wiring it into a run instead would need a separate argument (today the kernel only exposes model-driven tool invocation).
- `OA-M2` gains a third prerequisite: **OA-K must have landed** (previously only the consent-grant entry point and the egress gate).

---

## 1. Product Position and Boundaries

### 1.1 Position

OfficeAssistant is **the assistant capability layer within Agent24's basic office suite**: it translates user intent into inspectable plans, drafts, actions, and receipts, reusing Agent24's execution, tools, approvals, audit, and memory (report "Conclusion" paragraph 1; §1 "Structure already confirmed by the author"). Its responsibility is first defined as "understand tasks, organize steps, present confirmations, and consolidate results" (report §1 paragraph 2).

Whether it ultimately becomes a menu label, module name, or separate directory remains open according to the report (same paragraph of report §1); this plan is structured as "one thin business orchestration layer + several connector tools" and presupposes no naming.

### 1.2 What it does not do (in the first version and within this plan's three milestones)

Based on report §2 "What the first iteration does not take on" and the §1 table:

- No new email client, Notion-style collaborative editor, CRM, or complete project-management product; artifacts are returned to existing tools wherever possible.
- No bulk sending enabled by default, no unattended external sending, no cross-organization data sharing, no automatic payments, no high-risk decisions.
- A single "agree" is not implicitly turned into a recurring task or ongoing authorization; recurring tasks and ongoing authorization are designed separately.
- Sin90/Cos72 are not required to be loaded first; Cos72 can supply organizational semantics and Sin90 can supply personal context, both as optional integrations, and cross-scope access requires authorization (report §1 table, Sin90/Cos72 row).
- No dependence on or initiation of Hyphae/Web3 integration; no expansion of Open Design/OpenCreator development (report §1 table, corresponding rows; "Conclusion" paragraph 3).
- Application Workspace is not made a prerequisite for foundational tool calls, nor a second approval authority (report §1 table, Application Workspace row).
- "Assistant" is not given a name that conflates it with the L2 filesystem workspace or the Cos72 community workbench (report §1 "Distinct meanings of Workspace"; the opening note of the C4 section in `COMPONENT-ROADMAP.md`).

---

## 2. Initial Requirements Draft

### 2.1 Two formulations side by side

| | Author's original page promise | Precise formulation given by the report |
|---|---|---|
| Text | "Hand off the repetitive work. Before each task is completed, it will wait for your approval." (report "How the original page plan is incorporated", sentence 1, S21) | "The assistant first prepares results for review. External sending, remote changes, or expansion of access are performed under the confirmation for that action, or an explicitly granted scope that remains valid; anything outside that scope requires another question. Reading, organizing, and drafting already requested by the user need not interrupt them at every internal step. Reading itself remains subject to access rights and constraints on where the material may be processed." (report, same section, sentences 2–4; quoted from the English original) |
| Nature | A user-facing **statement of trust** | A testable **interaction and permission contract** |
| Use | Home page and introduction-page copy; may be retained as historical planning | Basis for product behavior, acceptance cases, and PR review |

**Relationship between the two**: the former is a plain-language summary of the latter, not an additional promise. Wherever the two appear to conflict (for example, "each task" read as requiring a dialog for every low-risk internal step, or read as meaning previously granted scope ceases to apply), the latter prevails; a public release must include the latter's explanation to avoid misreading (last two sentences of the same section of the report).

### 2.2 Phase semantics: preparation → review → confirmation → execution → receipt

| Phase | What the user sees | What the system guarantees | Source in report |
|---|---|---|---|
| **Preparation** | Which materials will be read, what will be produced, which steps will write or send | The plan has a fixed version; new scope, new destinations, or material content changes trigger reassessment of authorization | §3 "Seven semantics", Plan row |
| **Review** | Drafts/minutes/action items each with provenance; items lacking evidence are marked "needs confirmation"; editable | Human edits are preserved and form a new version; commitments not expressed in the source must not be filled in | §2 item 1; §6 R1 |
| **Confirmation** | This step's account, exact destination, final payload/diff, affected scope; approve/edit/reject/stop (choices supplied by the server) | Approval is bound to **this one action** and the **final payload version**; expiration, rejection, or disconnection never count as permission | §3 Approval row; §5 item 5 |
| **Execution** | Running/awaiting confirmation/partially complete/blocked/finished; can be cancelled at any time | Each step has an action_id and attempt records; cancellation stops only future work and does not claim to undo external effects that already occurred; retries query first, then act | §3 Action/Cancel/Retry/Status rows |
| **Receipt** | Artifact links/versions, executed steps, failed or unknown steps, follow-up actions | Approval, tool return, provider acceptance, and actual delivery are recorded separately; simulated results are clearly labeled | §3 Receipt row; §6 negative-case matrix, last row |

The "preparation" phase still presupposes that the user has requested it and has access rights to the materials; it does not mean automatically reading an entire mailbox, all Notion pages, or every LINE conversation (report §2, first paragraph after the table).

### 2.3 Explicit "not in the first version" list

- Real external account integration (none at all throughout M1; M2 connects only **one** connector selected by the author).
- Bulk messaging, automatic replies, proactive pushes, broadcasts (design only in M3, and requiring separate authorization; report §5 table, LINE and community bulk messaging rows).
- Recurring tasks, long-term synchronization, ongoing authorization (report §2 "What the first iteration does not take on").
- Implementation of bounded delegation (M3 does design and an acceptance plan only; report §5 "Balance convenience and safety" paragraph 2 explicitly states that the first MVP still uses individual confirmations).
- Voice entry point, OS-shared materials, multi-person collaboration (M3 only evaluates entry conditions; report §6 R3).
- Authorized cloud processing (K1 has no `CloudAuthorized` production entry point yet; see §0.2).
- An end-to-end redaction promise (iDoris not wired up, non-LLM egress guard not complete; report §5 item 4).

---

## 3. Milestone Overview

| Milestone | Corresponding report phase | One-sentence goal | External accounts | Prerequisites |
|---|---|---|---|---|
| **OA-M1 Local-loop skeleton** | R0 + R1 | Synthetic/authorized transcript → minutes + action items → item-by-item provenance → human edits preserved → local delivery/receipt | None; mock connector | jason approves the project; does not crowd out the mainline |
| **OA-M2 One real connector** | R2 | The email or Notion connector selected by the author: read-only → remote draft → **one** real write under explicit authorization | One; test account and destination specified by the author | OA-M1 DoD; §7 decision items D1–D3; kernel egress/grant prerequisites (§4.3) |
| **OA-M3 Reuse and boundary expansion** | R3 + report §5 bounded delegation | The same action/approval/receipt structure carries a second workflow; bounded delegation design and acceptance plan; entry-condition evaluation for extensions | Carried over from M2, no new ones (unless decided otherwise) | OA-M2 DoD |

Milestone IDs use the `OA-M*` prefix to avoid confusion with the repository's existing M1 (memory) and M2 (`PLAN-M2.md`).

---

## 4. Milestone Details

### 4.1 OA-M1 Local-loop skeleton

**Goal**: without connecting any real external account, prove that the structure of the chain "understanding → draft → human edits → confirmation boundary → delivery/receipt" and the seven semantics can be observed and tested (report "How the original page plan…" paragraph 4; §6 R0/R1).

**In scope**
- Input: synthetic meeting transcripts; or authorized, de-identified samples (report §6 R0). Text transcripts are accepted directly; no voice pipeline (report §1 table, AgentEar row).
- Output: minutes, decisions, action items (each with source_quote/material location); missing owners/dates are marked "needs confirmation"; commitments not expressed in the source must not be filled in (report §2 item 1; §6 R1).
- Human edits: an edit produces a new artifact version, and later steps are based on the edited version; regeneration must not overwrite human edits (report §6 R1 "preservation of human edits").
- Delivery: local delivery (local artifact files/versions) and **mock connector** delivery; mock results are labeled "simulated" in the UI and receipts (report §6 negative-case matrix, last row).
- A minimal observable implementation of the seven semantics (plan/action/approval/cancel/retry/status/receipt), with status following the kernel's authoritative snapshot (report §3).
- Cancellation points: during reading of materials, while waiting for the model, while waiting for approval, before the write boundary; plus state recovery after restart (report §6 R1, last sentence).
- Models: local models only; fail explicitly when no local model is available (report §4 paragraph 3; K1 release notes lines 31, 149–158).

**Out of scope**: any real accounts and credentials; remote drafts; real email/Notion/LINE APIs; bounded delegation; bulk messaging; voice; Sin90/Cos72 integration; cloud models.

**Features to deliver**

| ID | Feature | Description |
|---|---|---|
| F1.1 | Synthetic samples and gold-standard set | ≥1 reproducible input + expected minutes/action items + source locations + actual output (report §6 R1); 3–5 recommended, covering counterexamples (see acceptance) |
| F1.2 | State mapping and list of new fields | Map the seven semantics onto the existing `RunStatus` (`Queued/Running/AwaitingApproval/Completed/Failed/Cancelled`, `agent24-protocol/src/types.rs:381-388`) and newly added workflow-layer projections; produce a list of new fields (report §6 R0) |
| F1.3 | Minutes + action-item extraction (with provenance) | Each item carries source_quote/location; missing fields are "needs confirmation", not guesses |
| F1.4 | Artifact versions and preservation of human edits | Artifact records carry version/hash; edits produce new versions |
| F1.5 | Actions and confirmation cards | One confirmation card per delivery action: destination, final payload/diff, simulated/real label; approval bound to the payload version |
| F1.6 | Mock connector + receipts | Receipts distinguish "approved / tool returned / provider accepted / delivered"; a mock can only reach "simulated acceptance" |
| F1.7 | Cancellation, retry, disconnection recovery | Cancellation lists completed actions; retry redoes only incomplete steps; after disconnection, read the authoritative snapshot |
| F1.8 | Task details view (minimal) | Of the three views in report §3, "task details" comes first; "start a task" can be minimal; "connections and permissions" in M1 only shows "no external services connected" |

**Sub-task breakdown** (each task PR defaults to ≤300 lines, see `AGENTS.md` "branch rules"; items with state machines first get a one-page semantics note, see `tasks.md` line 288)

| Sub-task | Content | Suggested owner | Risk level |
|---|---|---|---|
| OA-M1-01 | One-page semantics note: the seven semantics × mapping to existing run/approval shapes, new fields, list of things that must not be claimed | Drafted by planner, reviewed by jason | Design (review before writing code) |
| OA-M1-02 | Synthetic transcripts and gold standard (at least one each in Thai/English/Chinese, see §5) | Primarily the intern | Low (pure data; must be labeled "synthetic") |
| OA-M1-03 | Extraction evaluation script: item-by-item comparison of omissions, unsupported commitments, "needs confirmation" labeling, provenance accuracy | Intern + planner review | Low |
| OA-M1-04 | workflow/action/artifact/receipt records (**in the `office` module's own `data_dir`**, not the kernel store; no migration — see §0.4 D-A) | Core developer | Medium (no storage migration) |
| OA-M1-05 | Extraction step goes through the `Models` callback (local model, LocalOnly; not wired into a run — see §0.4 D-A) | Core developer | Medium |
| OA-M1-06 | Confirmation card + payload version binding (mock actions) | Core developer | **High** (approval semantics) |
| OA-M1-07 | Mock connector + layered receipts | Core developer; intern writes negative-case tests | Medium |
| OA-M1-08 | Negative-case tests for cancellation/retry/restart recovery | Intern writes cases, core developer fixes | Medium |
| OA-M1-09 | Task details UI (trilingual copy; Thai proofread by the intern) | Frontend developer + intern for copy | Medium |
| OA-M1-10 | Demonstration script and acceptance record (per-scenario pass/fail + evidence + reproduction steps) | Executed by the intern, accepted by jason | Low |

**Acceptance criteria** (OA-M1 is complete only when all are satisfied; report §6 R0/R1 and the negative-case matrix)

1. A reviewer can answer three questions from the UI alone: what will happen, when external effects will occur, and what will remain if we stop now (report §6 R0 success criterion).
2. Item-by-item comparison against the gold standard: omissions, unsupported commitments, and loss of human edits are **all zero-tolerance items**, reported separately and not mixed into an average success rate (report §6, paragraph after the negative-case matrix).
3. "Do not fill in what was not expressed": for action items in the samples where owners/dates are deliberately omitted, the output must be "needs confirmation"; no model-inferred names or dates may appear.
4. "Instructions in materials exceeding authorization are only material": when the samples embed a sentence such as "send these minutes to everyone", the system must not expand the sending scope (report §6 negative-case matrix, row 7).
5. On rejection, approval expiration, or no approval channel, no (simulated) external action occurs (row 1).
6. If the destination or payload changes after approval, the old approval does not permit it (row 2).
7. Cancellation during execution: later steps stop, and (simulated) effects that already occurred are listed truthfully (row 5).
8. After a restart or WS disconnection, recover from authoritative state and do not send again based on an old card (row 6).
9. Local model unavailable: the task fails/waits and does not switch to remote (row 8).
10. Mock returns success: clearly labeled as a demonstration result; no real writes or delivery reported (row 14).
11. Every scenario has pass/fail, evidence, and reproduction steps (report §6, paragraph after the negative-case matrix).

**Risks and dependencies**
- Dependencies: jason's approval of the project and resources; a local model being available (oMLX/Ollama loopback, K1 release notes lines 255–260); K1's approval and audit shapes (ADR-K1-04 §2.1).
- Risks: extraction quality is limited by local models — exposed by the gold standard and evaluation script, not substituted by an attractive page (report §6 R1); new storage migrations cannot be rolled back (the sqlx behavior described in K1 release notes lines 89–121 applies equally); competition with C3 P1 for review bandwidth — all PRs in this topic yield to the mainline.

### 4.2 OA-M2 One real connector

**Goal**: on the **one** connector selected by the author (email or Notion, report §6 R2; this document does not choose on the author's behalf), go through "read-only → remote draft → one real write under explicit authorization", proving that idempotency, reconciliation, receipts, and revocation hold against a real service.

**In scope**
- Read-only: read only the threads/pages specified by the author; the read scope is **disclosed** on the confirmation card (not stored as standing authorization; see §6 L-APPR-7).
- Remote draft: saving a draft remotely is itself a remote write; it is confirmed separately and not confused with a local preview (report §2, first paragraph after the table; §5 table, email row).
- One real write: send one email to a test recipient specified by the author, or write to one test page specified by the author; **done only once**, with an authorization record, idempotency key, and receipt.
- Idempotency and reconciliation: generate a business idempotency key before writing; on timeout or unclear result, query the remote first, then decide whether to retry (report §3 Retry row; §6 negative case row 3).
- Receipts: distinguish "approved / tool returned / provider accepted / result needs verification / verified"; email provider acceptance does not mean the recipient has received the message (report §2 table, email row).
- Confirmation does not imply long-term synchronization: one approval covers only this one write (report §3 "A user flow suitable for review" paragraph 3).
- Revocation/expiration: connections and authorizations can be revoked in the "connections and permissions" view; after expiration or revocation, in-flight actions stop before the outbound boundary (ADR-K1-03 §3).
- A payload change after approval invalidates it (report §5 item 5).
- In the Notion case: show the **effective inherited scope** (parent-page authorization covers child pages); after revocation both retrieval and writing are invalidated (report §5 table, Notion row and the paragraph after it; §6 negative case row 13).

**Out of scope**: a second connector; bulk messaging; automatic replies; long-term synchronization; bounded delegation; LINE; production data and real customer materials; cloud models processing body content.

**Features to deliver**

| ID | Feature |
|---|---|
| F2.1 | Connector selection record and integration-method ADR (API/MCP/controlled adapter; credential types and their least-privilege capabilities; report §4 "Connectors may use…" and the §5 Notion paragraph) |
| F2.2 | "Connections and permissions" view: account, accessible resources, permitted actions, expiration and revocation |
| F2.3 | Read-only path (disclosed read scope) |
| F2.4 | Remote-draft action (separately confirmed) |
| F2.5 | One real write: confirmation card + payload version binding + idempotency key + query before acting |
| F2.6 | Layered receipts, the "result needs verification" status, and reconciliation queries |
| F2.7 | In-flight handling of revocation/expiration/disconnection |
| F2.8 | Test-account isolation scheme and credential custody (held only by jason) |

**Sub-task breakdown**

| Sub-task | Content | Owner | Risk level |
|---|---|---|---|
| OA-M2-00 | Author decides D1–D3 (§7) | jason | Gate |
| OA-M2-01 | Kernel prerequisite check: status of the module consent grant entry point and the egress decision (ADR-K1-02 6b.3), and the minimal dependency slice for this topic (see "Dependencies" below) | Planner + core developer | Design |
| OA-M2-02 | Connector integration ADR + list of affected laws (§6) | Drafted by planner, decided by jason | Design |
| OA-M2-03 | Read-only connector (test account) | Core developer | **High** (credentials/egress) |
| OA-M2-04 | Remote draft + confirmation card | Core developer | **High** |
| OA-M2-05 | Idempotency key + reconciliation query + one real write | Core developer | **High** |
| OA-M2-06 | Negative cases for revocation/expiration/payload change | Intern writes cases (against mock/record-replay); execution on the real account by jason or the core developer | Medium |
| OA-M2-07 | Acceptance of the real write (once) and evidence archiving | Executed by jason, or authorized by jason in person | Gate |

**Acceptance criteria** (report §6 R2: actual account scope, controlled test materials, result queries, deduplication evidence)

1. The three actions — read-only, remote draft, real write — are distinguishable in the UI, each with its own confirmation.
2. The real write happens exactly once: artificially create "remote success, local timeout"; the system queries first and then decides, **producing no second email/second page** (negative case row 3).
3. First step succeeds, second step fails: preserve the successful receipt and retry only the necessary steps (negative case row 4).
4. Changing recipients/page/attachments/body after approval: the old approval does not permit it (negative case row 2).
5. After revocation or expiration: new actions are rejected, and in-flight actions stop before the outbound boundary; content already delivered is not claimed to be retrievable (ADR-K1-03 §3; report §5 item 7).
6. Access to unauthorized pages/paths is rejected; permissions are not inferred from a workspace with the same name (negative case row 9). For Notion, verify the display of inherited scope and its invalidation after revocation (negative case row 13).
7. Outbound paths that bypass the privacy/egress gate: the test fails and that path is prevented from being enabled (negative case row 10).
8. In receipts, "provider accepted" and "verified" are separate; any result that cannot be verified is shown as "result needs verification".
9. The PR description names the affected laws (§6), with evidence pointing to tests.

**Risks and dependencies**
- **Kernel prerequisites (critical)**: after K1 merged, production code has no module consent grant route, `CloudAuthorized` has no production write point, and `http_fetch`/`shell_exec` have no unified egress decision (K1 release notes lines 27, 31, 33). ADR-K1-02 §2.4 item 2 lists "email/IM/external APIs" as LocalOnly egress gates, and user input defaults to `LocalOnly`. Therefore **sending minutes to email or Notion is, under the existing contract, an egress requiring "material × purpose × destination" authorization** (ADR-K1-02 §2.3). The real write in OA-M2 needs at least: (a) a host-controlled grant entry point (release notes line 59: the first PR that adds a grant path to production code must handle F1, F3, and F6 at the same time); (b) wiring that connector path into the egress decision. Both belong to the K1 track/C5 A-3b; whether to bring forward a minimal slice for OA-M2 **must be decided by jason (§7 D7)**. This plan does not decide it on its own, nor use it to change mainline priorities.
- Credentials are held only by jason; the intern has no contact with them (§5).
- Platform risk: provider API behavior, quotas, and permission models are governed by official documentation and integration acceptance testing; the words "official MCP" are not acceptance (report §4 "Connectors may use…").
- Irreversibility: sent email cannot be recalled; prefer remote drafts and versioned writes (report §5 item 7).

### 4.3 OA-M3 Reuse and boundary expansion

**Goal**: prove that the action/approval/receipt abstraction is reusable; complete the design and acceptance plan for bounded delegation; give entry conditions for extensions.

**In scope**
1. **Reuse for a second workflow**: carry email drafts or document action items on the same action/approval/receipt structure (report §6 R3). **Criterion: if the second workflow requires rewriting the approval or recovery mechanisms, the abstraction is deemed to have failed**, and work returns to design rather than piling on features.
2. **Bounded delegation design and acceptance**: the user explicitly selects tools, accounts, destinations/audiences, action categories, material/processing scope, validity period, and usage limits, and can pause or revoke at any time; the interface explains which authorization is being used now, why this step does not need another question, and where its result can be found; expiration, revocation, out-of-scope actions, or substantive changes to the final payload always pause execution, without silently switching channels (report §5 "Balance convenience and safety"). M3 delivers **a design document + a counterexample acceptance plan + the required law-change proposals**; whether to proceed to implementation is decided separately (same section of the report, paragraph 2: the first MVP still uses individual confirmations; ongoing authorization is a separate later design and acceptance item).
3. **LINE**: the default remains "suggested replies, manual copying"; connecting an Official Account to receive messages does not authorize sending; automatic replies/proactive pushes/broadcasts require separate authorization (report §2 item 5; §5 table, LINE row). M3 only verifies, on a mock, the behavior after a reply token expires: no blind retry, no switch to push without authorization, explain the restriction (report §5 LINE paragraph; §6 negative case row 12).
4. **Entry-condition evaluation for extensions**: voice (AgentEar as a transcript source), OS-shared materials (Cos72/Sin90), multi-person collaboration — one page of "entry conditions" each: which kernel/memory/organizational capabilities they depend on (e.g. `COMPONENT-ROADMAP.md` C3 P3, C11 F9), and which laws must be established first.

**Out of scope**: production implementation of bounded delegation (unless decided otherwise); real bulk messaging; real LINE sending; a second real connector (unless decided otherwise).

**Features to deliver**

| ID | Feature |
|---|---|
| F3.1 | Second workflow (email draft or document action items) running on the existing structure, with negative cases at the same level as OA-M1 |
| F3.2 | Abstraction reuse report: which shared code was changed and why; whether the "abstraction failure" criterion was triggered |
| F3.3 | Bounded delegation design document + counterexample acceptance matrix + conflict analysis against L-APPR-4/5/6/7 and law-change proposals (separate PR) |
| F3.4 | LINE suggested replies (manual copying) and mock negative cases for reply-token expiration |
| F3.5 | Entry-condition evaluation for voice / OS-shared materials / multi-person collaboration |

**Sub-task breakdown**

| Sub-task | Owner | Risk level |
|---|---|---|
| OA-M3-01 Semantics note for the second workflow (reuse mapping) | Planner | Design |
| OA-M3-02 Implementation of the second workflow | Core developer | Medium/High (depending on whether approval is touched) |
| OA-M3-03 Gold standard and negative cases for the second workflow | Intern | Low |
| OA-M3-04 Bounded delegation design + law proposals | Planner, decided by jason | Design (laws must be a separate PR) |
| OA-M3-05 LINE suggested replies (Thai scenario samples and copy) | Primarily the intern, reviewed by core developer | Low/Medium |
| OA-M3-06 Entry-condition evaluation for extensions | Planner + intern research | Low |

**Acceptance criteria**
1. The second workflow passes a negative-case set at the same level as OA-M1, with zero rewrites of approval/recovery mechanisms (verifiable from the F3.2 report).
2. The bounded delegation design includes: a list of dimensions, pause/revoke semantics, an item-by-item comparison against existing laws, and a counterexample matrix (covering at least report §6 negative case row 11); reviewed by jason.
3. The LINE default path makes no calls to any sending API; behavior in the reply-token expiration scenario conforms to the report §5 LINE paragraph.
4. Each extension has "entry conditions" and "reasons they are not currently met".

**Risks and dependencies**: bounded delegation is in tension with the current L-APPR-4 (no broad session authorization for external), L-APPR-5 (standing authorization narrowed to exact targets), L-APPR-6 (standing authorization attached to a schedule), and L-APPR-7 (read permissions are not stored), and must go through the law process first; multi-person collaboration depends on the C11 F9 community subset (`COMPONENT-ROADMAP.md` C11, wave C).

---

## 5. People and Collaboration Model

### 5.1 Roles

| Role | Responsibilities |
|---|---|
| **Author (jason)** | Decides all items in §7; holds and keeps custody of all real accounts and credentials; decides test destinations; merge and release gates; real-device acceptance; approves this topic as a project and the start of each milestone |
| **Planner (the role of this document's author)** | Drafts semantics notes, ADRs, and lists of affected laws; defines sub-tasks and acceptance plans; reviews intern output |
| **Core developer** | Kernel/store/approval/connector implementation; all high-risk PRs |
| **Thai intern** | Primarily OA-M1 (see below) |
| **PR-Daemon** | The sole external reviewer; high-risk PRs require its APPROVE |

### 5.2 What the Thai intern takes on in each milestone

- **OA-M1 (primary)**: synthetic transcripts and gold standard (including Thai/English, and meetings mixing Chinese, English, and Thai); extraction evaluation script and item-by-item comparison reports; writing negative-case test cases; demonstration script and acceptance records; proofreading Thai UI copy; feedback on "how local work habits affect priorities" (see the question list in the English brief).
- **OA-M2 (supporting)**: writing negative cases against mock/record-replay; reading connector documentation and taking notes on "platform behavior differences"; **no contact with real accounts**.
- **OA-M3 (supporting)**: gold standard for the second workflow; Thai scenario samples and copy for LINE suggested replies; local user-research notes for extensions.

### 5.3 Things the author must decide

Connector selection; real accounts and test destinations; the timing of any real external write; all merges and releases other than merges into the integration branch; law changes; whether bounded delegation proceeds to implementation; whether to bring forward a kernel slice for OA-M2 (§7).

### 5.4 Things the intern must not do alone

- Any real outbound action (sending email, writing to Notion, LINE sending), including "just testing it".
- Touching, generating, storing, or forwarding any credentials, tokens, API keys, or OAuth authorizations.
- Using production data, real customer materials, or real meeting recordings or transcripts (unless authorized in writing by the author, de-identified, and supplied by the author).
- Merging any PR, creating tags, releasing, changing rulesets, or changing the integration branch.
- Modifying `docs/laws/`, ADRs, or kernel approval/audit/authorization code (may raise questions and write tests, but not change these independently).
- Running any task on the Mac mini (`AGENTS.md` "milestone direction branch workflow", last item).

### 5.5 Collaboration conventions

English is the collaboration language; one fixed weekly sync (suggested) + an asynchronous question list; "when in doubt, ask first — do not guess"; intern output goes through pre-pr-check and PR-Daemon review like any other PR (see the English brief for details).

---

## 6. Progress and Gates

### 6.1 Integration with existing repository gates

| Gate | Source | How this topic uses it |
|---|---|---|
| Integration branch | `AGENTS.md` "milestone direction branch workflow" | Suggested integration branch `ab/office-assistant`, task branches `ab/office-assistant-NN-<short-name>`; **a new direction must first be added to the ruleset `ab-integration`** — whether to open this track is decided by jason (§7 D8) |
| Absorb main weekly | Same, hard constraint 1 | Use merge, not rebase |
| Task PR ≤300 lines | `AGENTS.md` "branch rules", "PR size" | If exceeded, the PR body must explain why it cannot be split |
| Local gates | `AGENTS.md` "gates" | Rust: fmt/clippy/test; if `apps/` changed, run pnpm typecheck/lint/test; if `protocol/` changed, run `lint:openapi` and `gen:api` with no diff |
| pre-pr-check | `HANDOFF-ORCA-2026-10-08.md` line 44 | Before opening a PR run `pre-pr-check.sh --base origin/<base>`, and respond item by item by number in the PR body |
| PR-Daemon | `AGENTS.md` "review and merge thresholds" | Ordinary task PRs: CI fully green + pre-pr-check responses + local gates are enough to merge into the integration branch; **PRs touching security/permissions/authorization, data-write semantics, storage migrations, or external protocols require PR-Daemon APPROVE** (in this topic, OA-M1-04/06 and OA-M2-03..05 all fall into this category). Approval applies to the PR's exact head; pushing commits after approval invalidates the approval (a team convention recorded in collaboration memory; no written clause was found in the repository files; listed as "not verified") |
| Release PR | Same | `ab/office-assistant → main` requires PR-Daemon APPROVE + jason's real-device acceptance; opened and reviewed by humans |
| Law loop | `../laws/README.md` | PRs that change observable behavior name the affected laws; law changes are in a separate PR, not in the same PR as the implementation (part three of the K1 release notes records a remediation after this rule was violated once; this should not be repeated) |
| Semantics note before state machines | `tasks.md` line 288 | OA-M1-01 and OA-M3-01 are reviewed before code is written |

### 6.2 Laws expected to be affected (implementation PRs must name them item by item)

| Law | Relationship |
|---|---|
| L-APPR-4 | When a connector's outbound action has risk `external`, `approve_for_session` must not appear (`agent24-policy/src/lib.rs` `decisions_for`) |
| L-APPR-5 | Any "don't ask again" can only be narrowed to an exact target; if bounded delegation needs wider dimensions, the law must be changed first |
| L-APPR-6 | If there are recurring tasks in the future, standing authorization is attached to the schedule |
| L-APPR-7 | **Read permissions are only disclosed on the consent card and not stored.** The report's §5 table, Notion row — "already authorized reading need not be reconfirmed every time" — is in tension with this: until the law changes, OA-M2 can only do "read disclosure within the scope of this task" and must not persist read authorization |
| L-CTX-2 | Materials injected into context (transcripts, email threads, pages) must be nameable: canonical source, hash, size limit |
| L-CTX-4 | The system's own run records/audit are not evidence of completion — "verified" in receipts must come from an independent query of remote state, not from local logs |
| L-MEM-1/2/7 | If OfficeAssistant writes or recalls memory, the scope is injected by the kernel and isolation is not overstated |
| To be established: PRIVACY (L-PRV) | `docs/laws/PRIVACY.md` **does not exist** as of this check; the privacy law that C5 A-3 planned to freeze has not landed. OA-M2 egress must comply with ADR-K1-02, but cannot claim protection by a privacy law |

### 6.3 DoD for each milestone and "do not claim until met" red lines

**OA-M1 DoD**: §4.1 acceptance items 1–11 all pass, with evidence archived; all high-risk PRs have PR-Daemon APPROVE; the release PR has jason's real-device acceptance.
Red lines: must not claim "can already send email/write to Notion", "already connected to any external service", "the assistant can execute autonomously", or "privacy protection is in place"; must not write mock receipts up as delivery; must not present an average success rate as the quality conclusion while hiding any safety counterexample.

**OA-M2 DoD**: §4.2 acceptance items 1–9 all pass; the real write happened exactly once with deduplication evidence; kernel prerequisites satisfied in the way jason decides.
Red lines: must not claim "email/Notion integration is supported" (can only say "one controlled write was completed on one test account"); must not claim long-term synchronization, batching, or automation; must not write "provider accepted" up as "the other party has received it"; must not claim that revocation can retrieve delivered content.

**OA-M3 DoD**: §4.3 acceptance items 1–4 all pass.
Red lines: until implemented, must not claim support for bounded delegation, automatic replies, bulk messaging, voice, or multi-person collaboration; if the second workflow triggers the "abstraction failure" criterion, must not claim the abstraction has been established.

**General red lines**: a plan must not be written up as complete; `tasks.md` is registered only after the project is approved, with truthful status.

---

## 7. Decisions Pending (Decided by the Author; This Document Presupposes No Answers)

| # | Question | Source | Impact | Latest needed by |
|---|---|---|---|---|
| D1 | Who is the first version for: personal use, or internal collaboration at 4seas? | Report §7 item 1 | Scope of shared permissions; whether C11 organizational capabilities are needed | Before OA-M1 starts (affects sample design); at the latest before OA-M2 starts |
| D2 | First real connector: email or Notion? | Report §7 item 2; §6 R2 | All of OA-M2 | Before OA-M2 starts |
| D3 | Which account, and which exact test destination? | Same | Credential custody and isolation | Before OA-M2-03 |
| D4 | Entry-point form: an "Assistant" entry on the basic home page, or routing through a unified input box? | Report §7 item 3 | UI projection layer; both views can share the same interaction contract | Before OA-M1-09 (can be validated first with the task details page) |
| D5 | How to balance individual confirmation by default with later bounded delegation? | Report §7 item 4 | Design direction of OA-M3 | Before OA-M3 starts |
| D6 | Are community bulk messaging and automatic LINE sending part of the initial release, or do they remain drafts/manual copying at first? | Same | Scope of the initial release | Before the first public release |
| D7 | Kernel prerequisites for OA-M2 (grant entry point, connector egress decision): wait for the K1 track at its own pace, or bring forward a minimal slice for this topic? | This document §4.2 "Dependencies"; K1 release notes lines 27/31/33/59 | Whether OA-M2 can start; must not be used to change mainline priorities | Before OA-M2 starts |
| D8 | Whether to open the `ab/office-assistant` direction and the corresponding ruleset; the intern's GitHub permission scope | `AGENTS.md` | Collaboration foundation | Before OA-M1 starts |
| D9 | When this topic starts (relative to C3 P1 / DM-SPIKE) | §0.2 | Scheduling | At project approval |

---

## 8. Not Verified and Known Inconsistencies

- The report's implementation descriptions are based on the fixed source snapshot of 2026-10-04 (Agent24 `54cec44`); the report states that it did not perform a new code audit on 2026-10-16 (report line 5). For K1, this document relies on `K1-RELEASE-NOTES-20261016.md` and the current code.
- "The current main track is C3 P1": the repository files only confirm that M1 has been merged and that P1 immediately follows P0, while also recording DM-SPIKE as the first item after M1; the latest ordering is **not verified**.
- The status of the K1 ledger in `tasks.md` is inconsistent with the merge facts (§0.2).
- No written clause was found in the repository files for PR-Daemon's "APPROVE applies to the exact head; pushing commits after approval invalidates it".
- The report's descriptions of the Components repository (five Skills, workspace server) were not checked in this repository (they are not in this repository).
- LINE and Notion platform behavior (reply token, inherited scope, quotas) is cited from the official documentation listed in the report; this document did not recheck it.
