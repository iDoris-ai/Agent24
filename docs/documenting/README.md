# Documenting — Design & Delivery Framework

> **Owner:** David Xu  
> **Project:** Agent24 / iDoris OfficeSuite  
> **Status:** Working baseline — revised after review (scope/ownership baseline; contracts and acceptance being completed before implementation)  
> **Updated:** 2026-10-07

## 1. Purpose

This document is the top-level planning and collaboration framework for the **Documenting** capability owned primarily by David Xu.

It is intended to be the stable entry point for later GitHub Issues, Projects, milestones, ADRs, implementation PRs, progress tracking, dependency management, and team communication.

This document deliberately focuses on David's scope. Related systems are described only where they are dependencies or integration boundaries.

**How to read this revision.** The 2026-10-05 review concluded that the framework is a sound scope, ownership and collaboration baseline, but not yet an executable, acceptable development plan. This revision keeps the framework and adds:

- the product form: a first-party built-in Agent24 feature with kernel tools, a native UI and replaceable engines, not loaded through a Workspace (§2.1–§2.3);
- three parallel workstreams instead of a serial Workspace → KB → Documenting chain (§12, §14);
- a DocumentService contract skeleton and Agent24 default registration (§10);
- a knowledge-context policy (§9);
- cross-scenario behavioural constraints derived from the 13 T005 scenarios (§11);
- first vertical slices (§15) and release hard gates (§18).

The 2026-10-07 design review on PR #685 led to further changes:

- knowledge context **default ON with a persistent Off** is now treated as a settled product direction, not an open decision (§9);
- three distinct terms: product Workspace, file working directory and knowledge space (§2.3);
- knowledge dependency is classified by whether an operation retrieves knowledge the user did not explicitly provide (§9.1);
- the T006 context-inheritance and safety contract is adopted as a normative dependency (§9.3);
- a single commit point for revisions (§10);
- pre-implementation gates (§22.1).

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

### 2.1 Product form

**Adopted direction (2026-10-06):**

> Documenting is a first-party, default document feature of Agent24. It consists of kernel document tools, a native document UI, and replaceable processing engines. The first release is developed in the Agent24 repository and merged to main. It does not depend on the generic Workspace. Whether to adopt a standalone editing application is decided separately, based on reuse benefit.

What this means in practice:

- **Built-in, same repo.** Development happens on an Agent24 feature branch and merges into `main`. Documenting is not a separate app that users install, log into or manage.
- **Three deliverable parts:**
  1. **Document capability** — read, parse, create, edit, review, save revisions, export, with tests. Implemented as Rust kernel document tools registered with Agent24 (§10).
  2. **Document UI** — native Electron/React/TypeScript pages: file list, preview/edit, change diff, revision and export entry points.
  3. **Engine adapters** — reuse existing editing, OCR and conversion engines behind adapters. Documenting does not build an Office suite from scratch.
- **Process model is not product shape.** OCR, conversion and similar jobs may run in separate background processes. That is a runtime choice only. Likewise, “DocumentService” (§10) is an interface boundary, not a requirement for separate deployment.
- **One library.** The document page connects to the unified material/knowledge library (§5, §7) instead of creating another store. “One library” means a single entry point and stable document identity. It does not rule out the draft/revision mapping metadata that Documenting needs. Where the editable authority and saved revisions live is settled by §17 #1 and §22.1.

Still open: the editor and content model (§17 #2, #3, #6). The direction above fixes the form, not those choices.

### 2.2 Two entry paths, one capability

Both paths call the **same** registered document operations. Neither is a privileged shortcut.

**A. Menu → native document page** (recommended interaction, not yet implemented):

1. The user clicks **Documents** in the left navigation. Agent24's own document page opens on the right, with recent files, import and new.
2. The user selects a file, and a preview or editor opens. Scanned material shows the original plus recognition status. Formats that cannot be edited say so explicitly.
3. The user edits directly, or selects a passage and asks e.g. “rewrite this”. Agent24 calls the document operations and produces a change proposal.
4. The user reviews the diff and accepts or rejects it. The result is saved as a new revision, and the original revision is kept.
5. The user clicks **Export**. Preflight runs, then a PDF or a supported editable format is produced. Sending it to someone is a separate action that needs confirmation (§11.2).

**B. Conversation → agent tools** (no menu click):

- In chat, the user says e.g. “read this file and extract the deadlines”. The agent calls the document tools directly.
- When viewing or review is needed, the agent opens the corresponding file in the document page.

### 2.3 Terminology: “Workspace”

Following T006 §3.1, this document uses three distinct terms. Matching names in Agent24 or upstream WeKnora must not be treated as the same thing.

- **Product Workspace** — the product entry for an application or a multi-step workflow, such as Open Design, Open Creator or the Knowledge Workspace.
  - It may be hosted natively or adapt an external application.
  - The generic contract keeps runtime kind (`desktop-sidecar | service`), UI kind (`embedded-web | host-native | none`) and transport (`acp | rest | other-reviewed`) independent.
  - Open Design currently uses a managed sidecar plus an isolated view (`docs/open-design-workspace/design/ADR-001-INTEGRATION-BOUNDARIES.md`). That is one way of hosting, not the definition.
- **File working directory** — the kernel workspace `a24.workspace.v1`, a registered, pinned scratch directory that a run is bound to (`ADR-002-WORKSPACE-CONTRACT.md`).
  - Documenting may use it for temporary drafts and export files, like any other kernel consumer.
  - It is not an identity or organization boundary.
  - It is never the only location of a saved revision (§22.1).
- **Knowledge space** — the personal / organization / project / KB scope of material and its ACL. It is separate from both of the above.

**Documenting is not loaded through a product Workspace.** It is a first-party feature with its own native page (§2.1). Having a UI does not make something a Workspace.

The relationship runs in two directions:

- **Workspaces as callers.** Other product Workspaces and multi-step workflows may call Documenting operations, the same way the chat and the document page do.
- **Documenting inside a Workspace.** Documenting may need a Workspace adapter only if the team adopts a full external document-editing application (§17 #11). Even then, the base document tools must remain callable by the agent without that application being open.

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

David's primary scope is the **Documenting business capability**, delivered as the three parts in §2.1 (kernel document tools, native document UI, engine adapters), including:

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

A product Workspace (§2.3) is the entry for an application or multi-step workflow, hosted natively or adapting an external application. It may call Documenting, but Workspace is **not a prerequisite for Documenting to exist**. Documenting is not loaded through a Workspace, and its delivery is not gated on the generic Workspace host (§12). Generic Workspace work is separate platform work.

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
   Chat / Agent            Documents page (native UI)
          \                      /
           v                    v
                    Agent24
       identity / permission / agent / approval
                       |
                       v
          Documenting (kernel tools)
     read / create / edit / review / version
          render / export / package
                       |
          +------------+------------+
          |                         |
          v                         v
       WeKnora                Document engine
 parse / search / RAG          edit / render
 citation / knowledge            export
  (default ON, §9)
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

## 9. Knowledge Context (default ON)

**Settled product direction** (T006 §1 and §7.1, pinned in §21):

- The Knowledge Workspace is **ON by default** for new installs, with a clear and **persistent Off** switch.
- On upgrade, an existing user's explicit Off choice is kept.
- Before each task, the host automatically attaches the personal context and the context of the current active organization, as far as the user's permissions allow.
- Default ON does **not** mean reading all material, being configured and ready, or permission to send data off the device.

Still open (§17 #8): who may override the setting, the override rules across org / personal / task, and the concrete interfaces.

Knowledge is a dependency of some Documenting operations, not of Documenting as a whole.

### 9.1 Operation classes

Operations are classified by **whether they need to retrieve knowledge the user did not explicitly provide**. The operation's name and the number of documents involved do not decide the class.

| Class | Examples | Behaviour when knowledge is Off or unavailable |
|---|---|---|
| Knowledge-free | import, open/render, find text inside a given document, edit, diff, revision, preflight, export; QA, compare or extract across documents the user explicitly provided | Works, as long as document storage itself is available (see below). |
| Knowledge-optional | summarize or draft where personal/org background would help but is not required | Runs on the provided material only. The result states that no knowledge context was attached (`skipped` or `failed`, never reported as `empty`). |
| Knowledge-required | retrieve material that was not explicitly provided: search across knowledge spaces, “find the latest policy”, answers that must rest on organization rules | Pauses with an explicit `knowledge_disabled` / `knowledge_unavailable` state. Never silently degrades into an answer without evidence. |

Knowledge being Off and the original-document storage being unreachable are **two different states**. In the second case import/edit/export cannot be promised unconditionally. See §22.1.

### 9.2 Assembly rules

- **One policy for every entry point:** direct Agent24 call, chat, Assistant, document page, and Workspaces acting as callers. No entry point may bypass it.
- **The host assembles context.** A host context-policy hook does the assembly; it does not wait for the model to choose to call a tool.
- **Identity comes from the host.** The host injects the trusted principal and the explicit active organization. The model may supply a query or task intent, but it cannot widen the allowlist.
- **Results record their context:**
  - the context state: `context_attached / empty / skipped / failed`;
  - which packs, resources and revisions were consulted;
  - the index state at the time (§7).
- **Service states are kept distinct:** `registered / loading / ready / degraded / unavailable / disabled`. Entry visible, service alive, identity usable and index ready are four different facts.

### 9.3 Normative dependency: T006 §7

Documenting does not write a second, partial set of context rules. **T006 §7 is normative** for context inheritance, Off semantics and privacy. Implementation acceptance must demonstrate at least the following:

- **Inheritance.** A fresh run, conversation continuation, resume, approval continuation and delegated subtasks all go through the same context policy. Subtasks inherit only the pieces they need, as restricted references.
- **Off.**
  - Off is persisted.
  - Switching it off cancels in-flight knowledge queries, discards late results, clears reusable ContextPack caches, and stops prefetch/retry started for the feature.
  - No tool may bypass Off.
  - Re-enabling re-checks identity, permissions and readiness, then rebuilds packs. Old tokens and stale caches are never revived.
  - Content already sent to a model or written into an artifact cannot be withdrawn, and the UI says so.
- **LocalOnly.** LocalOnly covers parsing/OCR, embedding, rerank, model and plugin egress. There is no automatic fallback to cloud services. If the policy cannot be proven for a path, that path fails closed.
- **Source content is evidence, not instruction.** Document or KB content never grants execution authority and never overrides host rules.

These are acceptance requirements for the implementation. They are not preconditions for merging this design document.

## 10. DocumentService Contract & Agent24 Default Registration (proposed skeleton)

“Capability integration” is not sufficient as a deliverable. Phase 1 (§14) must produce this contract as a reviewed ADR before implementation of the operations it covers.

### 10.1 Operations (initial list)

| Operation | Risk class | Knowledge class |
|---|---|---|
| `document.import` / `document.get` / `document.list` | read / create-record | free |
| `document.render` / `document.read_range` | read | free |
| `document.find` (inside given documents) | read | free |
| `document.search` (across knowledge spaces) | read | required |
| `document.ask` / `summarize` / `extract` / `compare` / `translate` | read | free on explicitly provided documents; optional or required per §9.1 |
| `document.draft.create` | mutate-draft | optional |
| `document.change.propose` | mutate-draft | optional |
| `document.change.review` (accept/reject) | mutate-draft (records decisions; creates no revision) | free |
| `document.revision.list` / `get` | read | free |
| `document.revision.commit` | **commit — the single commit point** | free |
| `document.revision.restore` | commit (creates a new revision through `revision.commit`) | free |
| `document.preflight` | read | free |
| `document.export` | create-artifact | free |
| `document.package.assemble` | create-artifact | free |
| `document.delivery.prepare` | handoff (side effect executed by Agent24) | free |

**Single commit point.**

- Accepting or rejecting suggestions only updates the working-draft state. `document.revision.commit` is the only operation that creates a revision. Restore also goes through it and produces a new revision; it never rewrites history.
- “Accept and save” in the menu path (§2.2 A, step 4) is a UI composition: review decisions followed by **one** commit.
- The idempotency key of that commit is bound to the document, the `base_revision` and the reviewed change set. A retry therefore yields the same revision, not a second one.
- The ADR defines this precisely.

### 10.2 Per-operation contract fields

Every operation must specify:

- **inputs / outputs**, including document id and revision;
- **identity source** — always the Agent24 trusted caller context, never model-supplied arguments;
- **risk class** and whether Agent24 approval is required. The business labels above (read / mutate-draft / commit / create-artifact / handoff) must map onto Agent24's actual kernel risk, permission and approval categories. The ADR provides the mapping table. Documenting does not invent its own permission tiers;
- **concurrency** — mutating operations require `base_revision`; a mismatch returns `revision_conflict` rather than overwriting;
- **typed errors** — at least `unsupported_format`, `parse_failed`, `partial_parse`, `knowledge_disabled`, `knowledge_unavailable`, `stale_index`, `revision_conflict`, `permission_denied`, `cancelled`;
- **job semantics** — long-running operations return a job id with status, progress, cancellation and resume/retry behaviour;
- **idempotency** — commit, artifact and handoff operations accept an idempotency key, so a retry never duplicates a revision, package or delivery.

### 10.3 Default registration

- Documenting registers with Agent24 as a default capability at startup, without loading Sin90/Cos72, a Workspace or a UI.
- Agent24 can discover the operations, their risk classes and their availability (for example engine present, knowledge context enabled and ready).
- UI, Assistant and Workspace call the same registered operations. A bespoke demo-UI path does not count as integration.
- Direct edits typed in the document page go through the same controlled business entry as agent calls, with the same identity, `base_revision`, commit point and audit trail. There is no UI-only write path.

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

- **Line D** is David's mainline: getting ordinary users' document tasks working end to end. It does not wait for Line W. Knowledge-free operations (§9.1) do not wait for Line K.
- **Line W** is independent platform work, not a prerequisite for Line D. It covers the generic Workspace host, the OD compatibility adapter and the WeKnora Workspace entry. Documenting is not delivered through it (§2.3). Any part temporarily assigned to David is listed explicitly with an exit condition (a handover owner and date). It is not added to §4.1.
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

- feature branch in Agent24 with Rust kernel document tools and a TS document page skeleton (§2.1);
- DocumentService contract ADR (§10) and Agent24 default registration;
- **Documents** navigation entry and the conversation entry path, both calling the same operations (§2.2);
- stable document identity and revision semantics;
- source/artifact model with input binding (§7.1);
- read/render boundary;
- knowledge context integration point: classification and recorded context state (§9);
- **early end-to-end format check:** use a Chinese notice and a sanitized template to validate read, edit of a specified region, revision save, and real reopen + layout check of PDF and the selected editable format. This exposes delivery problems in D1, not after an editor is built in D4 (§22.1);
- baseline acceptance fixtures and sample set (§18.2).

Needs: Agent24 registration, trusted caller context, and decisions §17 #1–#3 (the first formats and fidelity scope are frozen before the engine choice). Does **not** need Line W or Line K.

**D2 — Read & understand**

- import/read path; document page with recent files, import and new; preview/navigation, including original + recognition status for scans;
- selected summarize/extract/translate/compare operations;
- citation UX, exact source jump, visible parse/index/error states.

Needs: Line K only for operations that retrieve knowledge the user did not explicitly provide, and for KB-backed citation (§9.1). Reading, finding, QA and comparison over explicitly provided documents, whether one or several, run without it.

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

Independent platform work; not on Documenting's critical path. Reproducible OD baseline and golden paths → generic Workspace contract → OD compatibility adapter with regression evidence and a rollback path → WeKnora Workspace entry. Start with a trusted adapter calling WeKnora REST, rather than assuming Agent24 can consume WeKnora HTTP MCP directly.

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
| Document engines (edit / OCR / conversion) | rendering, structured edit, layout, revision-safe mutation, OCR, export — reused existing engines behind adapters (§2.1) | D1 (render), D3–D4 | engine choice TBD; adapters owned by Documenting |
| Agent24 desktop shell | navigation entry, native page hosting, open-file-from-chat | D1 | Agent24 desktop |
| OpenDesign/OpenCreator | context/artifact/run/cancel handoff | Line W | Workspace/Creative |
| Connectors | actual email/Drive/filesystem/business-system action + receipt | D4 | connector/Agent24 |
| Media | transcription for meeting material (S07) | later | Media |
| Product samples | representative real workflows and sanitized files (§18.2) | D1 | team/product |

## 17. Open Decisions

These are intentionally **TBD** and must not be silently frozen by implementation convenience. Each lists the phase it blocks.

| # | Decision | Blocks |
|---|---|---|
| 1 | **Editable source of truth** — Documenting-managed, KB-managed immutable source, or external document system? Also covers persistence/recovery and the promotion of temporary artifacts (§22.1). | D1 — must be resolved before any persistent data is written |
| 2 | **Primary content model** — Markdown-first, DOCX-first, internal structured model, or adapters? | D1 |
| 3 | **First formats and fidelity scope** — which formats ship first, and for DOCX: basic import/export vs comments/track-changes/headers/fields/complex tables/round-trip. Frozen before the engine choice (#6). | D1 format check, D3–D4 |
| 4 | **PDF scope** — reading, OCR, form filling (interactive vs flat overlay), page operations and true content editing are separate capabilities. | D2, form slice |
| 5 | **Collaboration depth** — single-user, async review, multi-user revision or real-time co-editing. | D3 |
| 6 | **Document engine/editor choice** — follows representative tasks and the frozen format scope (#3), not the other way round. | D3 |
| 7 | **First slices** — confirm V1/V2 and their scenario mapping (§15). | D1 fixtures |
| 8 | **Knowledge setting overrides** — who may override the setting, the override rules across org / personal / task, and the concrete interfaces. The default itself is settled: ON, with a persistent Off (§9). | D2 knowledge-optional/required operations |
| 9 | **Line W assignment** — which parts, if any, are temporarily David's, the handover owner and the exit date (§12). | Line W start (not Line D) |
| 10 | **Roadmap conflicts** — how conflicts between this plan and existing Agent24/T006 roadmaps are resolved and recorded. | planning |
| 11 | **External editing application** — whether to adopt a full standalone editor hosted as a product Workspace. Decided by reuse benefit. Base tools stay callable without it (§2.3). | D3 engine choice |

## 18. Release Hard Gates & Test Samples

### 18.1 Hard gates (a slice cannot be called released without these)

- Chinese and multilingual content (mixed scripts, vertical/CJK layout where in scope) passes read, edit and export.
- The format fidelity matrix for the declared formats passes reopen and visual checks.
- Hidden content (comments, tracked changes, metadata, hidden text, old template residue such as names, dates and headers) is cleaned or reported before export/delivery.
- Attachment versions and recipients in a package match the approved revision and confirmed targets.
- Batch isolation holds (§11.9).
- Permission revoked mid-task, cancellation and network loss leave a consistent state: no half-committed revision and no duplicated delivery.
- Knowledge Off/unavailable behaves as in §9.1, and the T006 §7 inheritance, Off and LocalOnly requirements listed in §9.3 are demonstrated.

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
- the 2026-10-05 applicability review of this document;
- the 2026-10-06 product-form clarification with the design repository (§2.1–§2.3);
- Agent24 `docs/open-design-workspace/design/ADR-001` and `ADR-002` (Workspace terminology);
- the 2026-10-07 design review on PR #685 (knowledge default ON, terminology, retrieval-based classification, commit point, pre-implementation gates).

Pinned references:

| Source | Ref |
|---|---|
| Agent24 design (reviewed version) | `iDoris-ai/Agent24` `bb76d8d3221516cac0fb3e4af2185dc5c627f753` (branch `docs/documenting-design`) |
| Agent24 source baseline | `54cec44a7410532e8a864ea90860f2d442c06ba2` |
| Research baseline (T001–T008) | `jhfnetboy/researcher` `9e374e1d8cd5b3415a1c5ee81029eb5aef187fde` |
| T006 WeKnora Workspace plan (normative for §9) | `jhfnetboy/researcher` `c7ab2129dbf1ae3af2150adfded9aa1a449889dd` — `topics/T006-knowledge-media-base/subtopics/weknora-workspace-development-plan.md` §1, §3.1, §4, §7 |

Where these sources contain research proposals rather than approved engineering decisions, this document keeps the corresponding item marked **proposed** or **TBD**. Historical implementation observations in research documents are not treated as current run results.

## 22. Immediate Next Checkpoint

Before implementation, and before converting this framework into a detailed GitHub Project plan, confirm:

1. decisions §17 #1, #2, #3, #7, which block D1;
2. generic Workspace is tracked as independent platform work with its own owner, and any temporary David assignment has an exit (§17 #9);
3. the KB/WeKnora test endpoint and the Line K service contract owner;
4. the sanitized sample set and gold labels for V1/V2 (§18.2);
5. review of the DocumentService contract skeleton (§10) as an ADR draft.

Allowed before these are confirmed: baseline verification, sample preparation, contract/capability probing, and small independent PoCs. Not allowed: committing to “complete Documenting delivered”, multi-user secure rollout, or fixed timelines.

After confirmation, create milestones/issues from §12–§19 rather than expanding scope directly in implementation PRs.

### 22.1 Pre-implementation gates

The following must be frozen before the corresponding implementation starts. They do **not** block merging this framework, and merging the framework does not mean the product is complete.

1. **Content and persistence authority.**
   - Define the single editable authority, persistent save/recover, and the rules for promoting temporary artifacts to saved revisions (§17 #1).
   - A scratch file working directory is never the only location of a saved revision.
   - “One library” means a unified entry point and stable identity. Draft/revision mapping metadata is allowed.
   - Knowledge retrieval being Off and the original-document storage being unreachable are different states. The latter cannot unconditionally promise import/edit/export.
2. **Formats and engines.**
   - Freeze the first formats and fidelity scope (§17 #3) before choosing an engine (§17 #6).
   - In D1, a Chinese notice and a sanitized template validate the path end to end (§14 D1).
   - All 13 scenarios are not required at once.
3. **Commit and permission contract.** The DocumentService ADR defines:
   - the single commit point and its idempotency (§10.1);
   - the read-only operations split from commit/restore;
   - the mapping of business risk labels to Agent24 kernel risk/permission categories;
   - one controlled business entry shared by UI direct edits and agent calls (§10.3).
4. **Context and safety contract.** The T006 §7 requirements in §9.3 are part of the implementation acceptance for any knowledge-optional or knowledge-required operation.

---

## Appendix A — Scenario Traceability (S01–S13)

All 13 scenarios are currently **partial**: the direction is present, but acceptance is not yet defined. The table records where each gap is now addressed. Closing a row requires a test in the slice or a gate that uses it.

As implementation starts, each row (and each capability group A1–A12) is extended into a full trace chain:

```text
scenario/capability ID → contract (ADR section) → test ID → owner/phase → passing evidence
```

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
