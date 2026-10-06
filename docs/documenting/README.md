# Documenting — Design & Delivery Framework

> **Owner:** David Xu  
> **Project:** Agent24 / iDoris OfficeSuite  
> **Status:** Working baseline — revised after review (scope/ownership baseline; contracts and acceptance being completed before implementation)  
> **Updated:** 2026-10-06

## 1. Purpose

This document is the top-level planning and collaboration framework for the **Documenting** capability owned primarily by David Xu.

It is intended to be the stable entry point for later GitHub Issues, Projects, milestones, ADRs, implementation PRs, progress tracking, dependency management, and team communication.

This document deliberately focuses on David's scope. Related systems are described only where they are dependencies or integration boundaries.

**How to read this revision.** The 2026-10-05 review concluded that the framework is a sound scope, ownership and collaboration baseline, but not yet an executable, acceptable development plan. This revision keeps the framework and adds:

- three parallel workstreams instead of a serial Workspace → KB → Documenting chain (§12, §14);
- a DocumentService contract skeleton and Agent24 default registration (§10);
- a knowledge-context ON/OFF policy (§9);
- cross-scenario behavioural constraints derived from the 13 T005 scenarios (§11);
- first vertical slices (§15) and release hard gates (§18).

Items not yet agreed by the team are marked **proposed** or **TBD**.

## 2. Product Positioning

**Confirmed direction:** Documenting is a default foundational document capability of the Agent24 OfficeSuite.

It must be discoverable and callable by Agent24 without requiring Sin90/Cos72, a product Workspace, or a particular UI to be loaded first.

The user problem is broader than “chat with a document”:

> Receive material → find trustworthy information → prepare or modify a usable document → review it → deliver it → be able to recover the exact delivered version later.

The core lifecycle is therefore:

```text
Receive → Read → Understand → Create/Edit → Review → Deliver → Version/Recover
```

The product is **not** defined by RAG, a vector database, or six AI buttons. Those are supporting capabilities inside a larger document workflow.

## 3. Mission

Documenting should enable Agent24 to reliably:

1. receive and identify documents;
2. render, read and navigate the original material;
3. find, extract and understand information with traceable evidence;
4. create and modify documents;
5. review AI/user changes before they become authoritative;
6. maintain stable document identity and revisions;
7. preview and validate layout before delivery;
8. export files that can actually be opened and used;
9. preserve source, citation, revision and artifact relationships;
10. expose these capabilities consistently to Agent24, Assistant, UI and Workspaces.

A successful LLM response is **not** by itself a successful document operation. The end result must be inspectable, editable where applicable, traceable, recoverable and deliverable.

## 4. Scope

### 4.1 David Xu primary responsibility

David's primary scope is the **Documenting business capability**, including:

- document business semantics;
- document identity and revision semantics;
- document atomic operations and the DocumentService contract (§10);
- Agent24 capability/kernel integration for document operations;
- import/read/process/edit/review/render/export user flows;
- document-side source and citation UX;
- document-side conflict and version handling;
- output/artifact adaptation;
- end-to-end document acceptance tests.

The scope must not be reduced to a frontend page.

Temporary Workspace / WeKnora integration work is tracked separately in §12 with its own scope and exit conditions. It does not extend this list.

### 4.2 Capability map

T005 groups the problem into A1–A12. These are capability groups, not necessarily UI buttons or microservices.

| ID | Capability | Documenting role | Primary dependency |
|---|---|---|---|
| A1 | Receive / identify / preserve | Core semantics + UX | storage / KB |
| A2 | Render / navigate | Core | document engine |
| A3 | Text / OCR / structure parsing | Integrate + review UX | WeKnora / parser |
| A4 | Search / locate / select | Integrate + document UX | WeKnora |
| A5 | Summarize / QA / extract / compare / translate | Own document operation semantics | WeKnora + Agent24/model |
| A6 | Structured edit / form fill | Core | document engine |
| A7 | Review / conflict detection | Core | revision model |
| A8 | Layout / preflight | Core | rendering engine |
| A9 | Convert / export | Core | export engine |
| A10 | Save / version / archive / recover | Core semantics | storage + KB mapping |
| A11 | Attachments / delivery package | Core document workflow | files/connectors |
| A12 | Permission / delivery handoff | Integration | Agent24 |

The largest Documenting-specific gaps are expected around **A2 and A6–A11**, rather than RAG itself.

Listing a capability group here is **not** acceptance coverage. Every group is currently framework-level; acceptance is defined only through the contract (§10), constraints (§11), slices (§15) and gates (§18).

## 5. Explicit Non-Scope / Ownership Boundaries

### KnowledgeBase / MediaBase

The KB/Media provider owns or supplies the agreed lower-level capabilities such as:

- ingestion backend;
- parsing/OCR jobs;
- chunking/indexing/embedding;
- retrieval/RAG/reranking;
- source/citation data;
- knowledge-side ACL enforcement;
- media processing and storage semantics (including meeting audio/video transcription).

Current planning uses **WeKnora** as the Knowledge Workspace / KB baseline. Documenting should consume this capability rather than create a second private knowledge store.

### Agent24 Core

Agent24 owns:

- trusted identity;
- permissions;
- approval;
- Agent runtime;
- task/run lifecycle;
- cancellation/retry;
- tool execution;
- model orchestration;
- audit/control-plane responsibilities;
- the actual execution of external side effects (send, submit, pay, share).

Documenting must not create a second Agent loop, authentication system, approval engine, or generic workflow engine.

### Workspace

Workspace composes multi-step application workflows. It may consume Documenting, but Workspace is **not a prerequisite for Documenting to exist**, and Documenting delivery is not gated on the generic Workspace host (§12).

### Spreadsheet / Creative / Connectors

Documenting should not grow into a weak clone of Excel, PowerPoint, email, or workflow products.

- formula/recalculation/pivot/data-analysis → spreadsheet capability;
- presentation/design/video → OpenDesign/OpenCreator or relevant creative capability;
- actual email/Drive/business-system delivery → connectors under Agent24 control.

## 6. Relationship: Agent24, Documenting and WeKnora

A useful responsibility model is:

> **Agent24:** Who can do what, and when?  
> **Documenting:** What happens to the document?  
> **WeKnora:** What knowledge exists in the documents?

```text
                    Agent24
       identity / permission / agent / approval
                       |
                       v
                 Documenting
     read / create / edit / review / version
          render / export / package
                       |
          +------------+------------+
          |                         |
          v                         v
       WeKnora                Document engine
 parse / search / RAG          edit / render
 citation / knowledge            export
   (optional per §9)
```

WeKnora can provide knowledge operations to Documenting while also being exposed as a full Knowledge Workspace through the generic Workspace host. These are two different product entry paths over related knowledge.

## 7. Document Revision vs Knowledge Index

Document revision and knowledge-index revision must not be conflated.

Example:

```text
document_id = contract-001
current document revision = 4

WeKnora resource = wk-abc
indexed source revision = 3
index build = 17
```

The product must be able to report that revision 4 exists while search still reflects revision 3. An old index must never be presented as if it represented the latest editable document.

A stable mapping is required between document identity/revision and KB backend identity/version.

### 7.1 Artifact ↔ input binding (proposed)

Every produced artifact (draft, export, delivery package) records the exact revisions of every input it was produced from, including:

- source documents and attachments;
- templates and their version;
- policies or reference material used;
- the review baseline and the approved revision.

A delivery package must bind to the approved revision, not to “latest”. If any bound input changes after approval, the package is stale and must be re-approved.

## 8. Citation Boundary

WeKnora / KB should provide evidence metadata such as resource, revision, source and verifiable location.

Documenting is responsible for making that evidence useful in the document workflow:

- show the citation;
- open the correct revision;
- navigate to the correct page/block/range when available;
- highlight the source where possible;
- expose stale/missing precision honestly.

**Never fabricate a page or source location.**

For extraction and comparison outputs (tables, field lists, figures), provenance is required **per value**, not only per paragraph or per answer. A value without a verifiable source is shown as unsourced, not silently trusted.

## 9. Knowledge Context ON/OFF (proposed)

Knowledge (KB/WeKnora) is a dependency of some Documenting operations, not of Documenting as a whole.

### 9.1 Operation classes

| Class | Examples | Behaviour when knowledge is OFF or unavailable |
|---|---|---|
| Knowledge-free | import, open/render, local text read, edit, diff, revision, preflight, export | Works normally. |
| Knowledge-optional | summarize/extract/translate a given document, draft from given material | Runs on the explicitly provided documents only, and says that no KB context was used. |
| Knowledge-required | search across a corpus, cross-document QA, “find the latest policy” | Pauses with an explicit `knowledge_disabled` / `knowledge_unavailable` state. Never silently degrades into an answer without evidence. |

### 9.2 Assembly rules

- The same policy applies to every entry point: direct Agent24 call, Assistant, UI and Workspace. No entry point may bypass it.
- The default (ON/OFF) and who may change it are Agent24 policy, set per org/workspace and overridable per task within that policy. The final default is **TBD** (§17).
- The task result records whether knowledge was used, which resources/revisions were consulted, and the index state at the time (§7).

## 10. DocumentService Contract & Agent24 Default Registration (proposed skeleton)

“Capability integration” is not sufficient as a deliverable. Phase 1 (§14) must produce this contract as a reviewed ADR before implementation of the operations it covers.

### 10.1 Operations (initial list)

| Operation | Risk class | Knowledge class |
|---|---|---|
| `document.import` / `document.get` / `document.list` | read / create-record | free |
| `document.render` / `document.read_range` | read | free |
| `document.search` | read | required |
| `document.ask` / `summarize` / `extract` / `compare` / `translate` | read | optional or required |
| `document.draft.create` | mutate-draft | optional |
| `document.change.propose` | mutate-draft | optional |
| `document.change.review` (accept/reject) | commit | free |
| `document.revision.commit` / `list` / `restore` | commit | free |
| `document.preflight` | read | free |
| `document.export` | create-artifact | free |
| `document.package.assemble` | create-artifact | free |
| `document.delivery.prepare` | handoff (side effect executed by Agent24) | free |

### 10.2 Per-operation contract fields

Every operation must specify:

- **inputs / outputs**, including document id and revision;
- **identity source** — always the Agent24 trusted caller context, never model-supplied arguments;
- **risk class** and whether Agent24 approval is required;
- **concurrency** — mutating operations require `base_revision`; a mismatch returns `revision_conflict` rather than overwriting;
- **typed errors** — at least `unsupported_format`, `parse_failed`, `partial_parse`, `knowledge_disabled`, `knowledge_unavailable`, `stale_index`, `revision_conflict`, `permission_denied`, `cancelled`;
- **job semantics** — long-running operations return a job id with status, progress, cancellation and resume/retry behaviour;
- **idempotency** — commit, artifact and handoff operations accept an idempotency key, so a retry never duplicates a revision, package or delivery.

### 10.3 Default registration

- Documenting registers with Agent24 as a default capability at startup, without loading Sin90/Cos72, a Workspace or a UI.
- Agent24 can discover the operations, their risk classes and their availability (for example engine present, knowledge ON/OFF and healthy).
- UI, Assistant and Workspace call the same registered operations. A bespoke demo-UI path does not count as integration.

## 11. Cross-Scenario Behavioural Constraints (proposed)

These constraints come from the gaps found across the 13 T005 scenarios (S01–S13, Appendix A). They apply to every operation and slice, not to one scenario.

1. **Faithfulness — never guess or inflate.** Do not guess identity or personal data. Polishing must not alter facts. Unconfirmed commitments stay marked “to be confirmed”. Do not widen promises beyond approved material. A missing value is empty, never zero. Negations, deadlines and amounts must be preserved exactly.
2. **Prepare ≠ execute.** Prepared, approved, submitted, sent and paid are distinct, visible states. Documenting may reach “prepared”. Execution goes through Agent24/connectors and returns a receipt or failure state.
3. **Pinned versions.** Templates, policies, review baselines and approved revisions are pinned (§7.1). Outputs never float to “latest”.
4. **Per-value provenance.** Extracted fields, table cells and attachment numbering trace to a source location, or are explicitly unsourced (§8).
5. **Recipients and receipts.** Delivery targets are exact and confirmed. Every delivery has a receipt or an explicit failure state. Batch-edit permission does not imply bulk-send permission.
6. **Separated authority.** Edit vs approve, prepare vs approve vs pay, and delete of a sole original each require distinct permissions, enforced by Agent24.
7. **Declared non-support.** Unaligned, unparsed or unsupported regions (for example interactive vs flat PDF fields, or complex layout) are shown explicitly and are never covered by a confident output.
8. **Gold-standard evaluation.** Critical fields (deadlines, negations, amounts, dates, IDs) are evaluated against labelled samples, not by impression.
9. **Batch isolation.** In batch operations each item is isolated: no cross-recipient content leakage, per-item preview, and partial failure/retry without duplicating completed items.

## 12. Workstreams & Ownership

The previous plan made the generic Workspace host (former Phase A) and the Knowledge Workspace (former Phase B) **serial prerequisites** of Documenting. In practice that would turn David into the de facto owner of Workspace and KB, regardless of any disclaimer. This revision replaces the serial chain with three workstreams.

```text
Line D  Documenting core   D1 → D2 → D3 → D4          owner: David Xu
Line W  Workspace integration (scoped, time-boxed)    owner: TBD; David's share explicit
Line K  KB service dependency (per operation)         owner: KB/Knowledge team
```

- **Line D** does not wait for Line W. Knowledge-free operations (§9.1) do not wait for Line K.
- **Line W** covers the generic Workspace host, the OD compatibility adapter and the WeKnora Workspace entry. Any part assigned to David is listed explicitly with an exit condition (a handover owner and date). It is not added to §4.1.
- **Line K** is consumed through an agreed service contract (ingest/status, search, source, citation, ACL, revision mapping). Documenting depends on it per operation (§14). It does not own or operate it.

## 13. WeKnora Fork Principle

The planned fork should continuously track Tencent upstream while keeping iDoris-specific changes thin.

Prefer changes around:

- host/base-path integration;
- navigation;
- identity bridge;
- health/capability discovery;
- configuration;
- stable adapter APIs.

Avoid unnecessary forks of:

- chunking;
- retrieval;
- embedding;
- reranking;
- RAG core;
- Wiki algorithms.

If satisfying requirements requires sustained invasive changes to those areas, treat that as architecture evidence and reassess rather than silently turning a thin fork into a divergent product.

Ownership of the fork follows Line K/Line W (§12), not Line D.

## 14. Delivery Path

This is a dependency-oriented framework, **not yet a sprint schedule**. Each phase lists what it needs from the other lines.

### Line D — Documenting core (owner: David)

**D1 — Foundation**

- DocumentService contract ADR (§10) and Agent24 default registration;
- stable document identity and revision semantics;
- source/artifact model with input binding (§7.1);
- read/render boundary;
- knowledge ON/OFF policy (§9);
- baseline acceptance fixtures and sample set (§18.2).

Needs: Agent24 registration, trusted caller context, and decisions §17 #1–#2. Does **not** need Line W or Line K.

**D2 — Read & understand**

- import/read path, preview/navigation;
- selected summarize/extract/translate/compare operations;
- citation UX, exact source jump, visible parse/index/error states.

Needs: Line K only for search, cross-document QA and KB-backed citation. Single-document operations run without it.

**D3 — Edit & review**

- draft creation, targeted edits, undo or equivalent safe revision behaviour;
- diff, accept/reject, conflict detection, creation of a new revision;
- suggestion ownership, separation of comments from body text, and edit/approve separation.

Needs: document engine decision (§17 #6), Agent24 approval.

**D4 — Render & deliver**

- layout preview, preflight checks, selected fixed/editable exports, conversion warnings;
- reopen/visual validation, hidden-content cleanup;
- attachment/package assembly bound to the approved revision;
- controlled delivery handoff to Agent24/connectors with receipt.

Needs: Agent24/connectors for execution and receipts.

### Line W — Workspace integration (formerly Phase A + Workspace part of Phase B)

Reproducible OD baseline and golden paths → generic Workspace contract → OD compatibility adapter with regression evidence and a rollback path → WeKnora Workspace entry. Start with a trusted adapter calling WeKnora REST, rather than assuming Agent24 can consume WeKnora HTTP MCP directly.

### Line K — KB service (formerly the service part of Phase B)

Upstream-tracking WeKnora fork, adapter/service contract, search/source integration, citation preservation, identity/scope mapping, explicit health/capability states, and document ↔ KB revision mapping (§7).

## 15. First Vertical Slices (proposed)

The first end-to-end acceptance is two vertical slices, not the whole capability map.

| Slice | Intent | Candidate scenarios | Key constraints (§11) |
|---|---|---|---|
| **V1 收件看懂 — Receive & understand** | Receive material, understand it with evidence, keep a recoverable result | S01, S06 (reading part) | 1, 4, 7, 8 |
| **V2 日常成稿 — Everyday drafting** | Produce a usable document from material/template, review it, export it | S03, S08 | 1, 3, 7 |

- **Form filling (S02)** is not in the first slices. Before it is promised, the supported form types and the manual-completion path for unsupported ones must be declared.
- If only one slice ships first, it is called “first slice”. It must not be described as covering the T005 first-release recommendation.
- The scenario mapping above is **TBD — to confirm with reviewer**.

## 16. Dependencies and Team Inputs

| Dependency | Needed by Documenting | Needed from phase | Owner |
|---|---|---|---|
| Agent24 Core | capability registration, trusted caller context, permissions, approvals, run/cancel/model/tool access | D1 | Agent24 |
| WeKnora / KB | ingest/status, search, source, citation, ACL, revision mapping | D2 (knowledge ops only) | KB/Knowledge |
| Document engine | rendering, structured edit, layout, revision-safe mutation, export | D1 (render), D3–D4 | TBD / Documenting integration |
| OpenDesign/OpenCreator | context/artifact/run/cancel handoff | Line W | Workspace/Creative |
| Connectors | actual email/Drive/filesystem/business-system action + receipt | D4 | connector/Agent24 |
| Media | transcription for meeting material (S07) | later | Media |
| Product samples | representative real workflows and sanitized files (§18.2) | D1 | team/product |

## 17. Open Decisions

These are intentionally **TBD** and must not be silently frozen by implementation convenience. Each lists the phase it blocks.

| # | Decision | Blocks |
|---|---|---|
| 1 | **Editable source of truth** — Documenting-managed, KB-managed immutable source, or external document system? | D1 |
| 2 | **Primary content model** — Markdown-first, DOCX-first, internal structured model, or adapters? | D1 |
| 3 | **DOCX fidelity level** — basic import/export vs comments/track-changes/headers/fields/complex tables/round-trip. | D3–D4 |
| 4 | **PDF scope** — reading, OCR, form filling (interactive vs flat overlay), page operations and true content editing are separate capabilities. | D2, form slice |
| 5 | **Collaboration depth** — single-user, async review, multi-user revision or real-time co-editing. | D3 |
| 6 | **Document engine/editor choice** — follows representative tasks, not the other way round. | D3 |
| 7 | **First slices** — confirm V1/V2 and their scenario mapping (§15). | D1 fixtures |
| 8 | **Knowledge default** — default ON or OFF, and who may change it (§9.2). | D1 |
| 9 | **Line W assignment** — which parts are David's, the handover owner and the exit date (§12). | Line W start |
| 10 | **Roadmap conflicts** — how conflicts between this plan and existing Agent24/T006 roadmaps are resolved and recorded. | planning |

## 18. Release Hard Gates & Test Samples

### 18.1 Hard gates (a slice cannot be called released without these)

- Chinese and multilingual content (mixed scripts, vertical/CJK layout where in scope) passes read, edit and export.
- The format fidelity matrix for the declared formats passes reopen and visual checks.
- Hidden content (comments, tracked changes, metadata, hidden text, old template residue such as names, dates and headers) is cleaned or reported before export/delivery.
- Attachment versions and recipients in a package match the approved revision and confirmed targets.
- Batch isolation holds (§11.9).
- Permission revoked mid-task, cancellation and network loss leave a consistent state: no half-committed revision and no duplicated delivery.
- Knowledge OFF/unavailable behaves as in §9.1.

### 18.2 Sample set

- Sanitized, representative documents per slice, including bad cases: scans, mixed language, complex tables, same-name different versions, and duplicates.
- Gold labels for critical fields (§11.8).
- Each sample pinned and versioned with the fixtures.

## 19. Progress-Control Framework

GitHub Project should track execution; this document remains the stable design/ownership baseline.

Recommended lifecycle:

```text
Research → Contract → Prototype → Integrated → Tested → Released
```

Recommended issue dimensions:

- **Line:** D / W / K
- **Area:** workspace / knowledge / document-core / read / ai-action / edit / review / render / export / integration
- **Dependency:** agent24 / weknora / opendesign / opencreator / kb-media / connector
- **Priority:** P0 / P1 / Later
- **State:** planned / in-progress / blocked / review / done

Every implementation issue should link back to the relevant section of this document and identify its external dependencies and owner explicitly.

## 20. Definition of Done

Documenting is not “done” because a UI exists or an LLM can answer questions about a PDF.

For an agreed slice (§15), completion requires evidence that:

- the original/source document is preserved or reliably referenced;
- document identity and revision are stable;
- the selected document can be opened and navigated;
- AI-derived facts can be checked against evidence, per value where applicable;
- unsupported/failed parsing is visible rather than hidden;
- edits are reviewable and do not silently overwrite a changed baseline;
- a new revision can be saved and recovered;
- artifacts record and honour their pinned inputs (§7.1);
- selected export formats actually reopen and pass the agreed fidelity checks;
- KB indexing state is distinguishable from document-save state;
- knowledge OFF/unavailable behaviour matches §9;
- permission and delivery actions respect Agent24 control, and prepare/execute states are distinct;
- failure/retry does not duplicate destructive or delivery side effects;
- the cross-scenario constraints (§11) relevant to the slice are tested;
- the release hard gates (§18.1) pass;
- the workflow works through Agent24 capability integration, not only through a bespoke demo UI.

## 21. Source Baseline

This working baseline is derived from the current project direction and the following design/research sources:

- iDoris product / OfficeSuite positioning;
- T005 Documenting report, user scenarios & atomic capabilities, David Xu handoff, Agent24 kernel integration study;
- T006 Document ↔ KnowledgeBase/MediaBase service boundaries and WeKnora Workspace development plan;
- T007 Assistant and T008 Media roadmap (for boundaries);
- Agent24 current architecture/ADR/code observations;
- WeKnora upstream capability documentation;
- the 2026-10-05 applicability review of this document.

Pinned references:

| Source | Ref |
|---|---|
| Agent24 design (reviewed version) | `iDoris-ai/Agent24` `bb76d8d3221516cac0fb3e4af2185dc5c627f753` (branch `docs/documenting-design`) |
| Agent24 source baseline | `54cec44a7410532e8a864ea90860f2d442c06ba2` |
| Research baseline (T001–T008) | `jhfnetboy/researcher` `9e374e1d8cd5b3415a1c5ee81029eb5aef187fde` |

Where these sources contain research proposals rather than approved engineering decisions, this document keeps the corresponding item marked **proposed** or **TBD**. Historical implementation observations in research documents are not treated as current run results.

## 22. Immediate Next Checkpoint

Before implementation, and before converting this framework into a detailed GitHub Project plan, confirm:

1. decisions §17 #1, #2, #7, #8, which block D1;
2. Line W assignment, handover owner and exit (§17 #9);
3. the KB/WeKnora test endpoint and the Line K service contract owner;
4. the sanitized sample set and gold labels for V1/V2 (§18.2);
5. review of the DocumentService contract skeleton (§10) as an ADR draft.

Allowed before these are confirmed: baseline verification, sample preparation, contract/capability probing, and small independent PoCs. Not allowed: committing to “complete Documenting delivered”, multi-user secure rollout, or fixed timelines.

After confirmation, create milestones/issues from §12–§19 rather than expanding scope directly in implementation PRs.

---

## Appendix A — Scenario Traceability (S01–S13)

All 13 scenarios are currently **partial**: the direction is present, but acceptance is not yet defined. The table records where each gap is now addressed. Closing a row requires a test in the slice or a gate that uses it.

| ID | Scenario | Gaps to close | Addressed by |
|---|---|---|---|
| S01 | Understand a notice/instructions | deadline/negation/amount gold labels, missing/conflict hints, saved result list, check against original page | §8, §11.1/4/8, V1 |
| S02 | Application/registration/repair forms | interactive vs flat PDF, ask for missing fields, never guess identity, checkbox/length/overlap, required attachments, signing handoff, prepare vs submit | §11.1/2/7, §15 (deferred), §17 #4 |
| S03 | CV / intro / cover letter | purpose and audience, template choice, verify real experience and contacts, facts unchanged by polishing | §11.1/3, V2 |
| S04 | Archive receipts/manuals/warranties | reversible classify/rename, same-name versions and duplicates, never delete a sole original, find by purchase fields, delete permission | §7.1, §11.6, §18.2 |
| S05 | Compare two versions | fixed inputs, paragraph/page alignment, separate text/number/format diffs, unaligned regions not guaranteed; edit diff ≠ cross-format compare | §10.1, §11.7 |
| S06 | Admin intake with many attachments | source access scope, existing reply versions, per-attachment list, exact recipients, receipt/failure, archive link | §11.2/5, V1, D4 |
| S07 | Meeting material & minutes | decision/discussion/open items, owner and date evidence, unconfirmed stays pending, attachment trace; transcription by Media | §5, §11.1/4, §16 |
| S08 | Template-based drafting | template version and replaceable-field whitelist, residue check, tables, editable + publish dual output | §7.1, §11.3, §18.1, V2 |
| S09 | Sales/support material response | approved material only, quote/exception check, no widened promises, package bound to approved version and recipient | §7.1, §11.1/2/5 |
| S10 | Supplier material summary | field schema, multi-row headers, currency/tax/unit/validity, empty ≠ zero, non-comparable items, per-value source, structured export | §8, §11.1/4, spreadsheet boundary §5 |
| S11 | Expense statement & receipt pack | duplicate receipts, amount/date gold labels, number ↔ attachment mapping, calculation handoff, prepare/approve/pay separation, finance data scope | §11.2/4/6/8 |
| S12 | Personalized onboarding material | per-person binding, pinned template/policy, per-recipient isolation and preview, partial failure/retry, batch edit ≠ bulk send | §7.1, §11.3/5/9 |
| S13 | Collaborative report revision | suggestion ownership, comments vs body, edit/approve separation, pinned review baseline and approved version, DOCX revision support scope | D3, §11.3/6, §17 #3/#5 |
