# Documenting — Design & Delivery Framework

> **Owner:** David Xu  
> **Project:** Agent24 / iDoris OfficeSuite  
> **Status:** Working baseline  
> **Updated:** 2026-10-05

## 1. Purpose

This document is the top-level planning and collaboration framework for the **Documenting** capability owned primarily by David Xu.

It is intended to be the stable entry point for later GitHub Issues, Projects, milestones, ADRs, implementation PRs, progress tracking, dependency management, and team communication.

This document deliberately focuses on David's scope. Related systems are described only where they are dependencies or integration boundaries.

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
- document atomic operations;
- Agent24 capability/kernel integration for document operations;
- import/read/process/edit/review/render/export user flows;
- document-side source and citation UX;
- document-side conflict and version handling;
- output/artifact adaptation;
- end-to-end document acceptance tests.

The scope must not be reduced to a frontend page.

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

## 5. Explicit Non-Scope / Ownership Boundaries

### KnowledgeBase / MediaBase

The KB/Media provider owns or supplies the agreed lower-level capabilities such as:

- ingestion backend;
- parsing/OCR jobs;
- chunking/indexing/embedding;
- retrieval/RAG/reranking;
- source/citation data;
- knowledge-side ACL enforcement;
- media processing and storage semantics.

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
- audit/control-plane responsibilities.

Documenting must not create a second Agent loop, authentication system, approval engine, or generic workflow engine.

### Workspace

Workspace composes multi-step application workflows. It may consume Documenting, but Workspace is **not a prerequisite for Documenting to exist**.

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

## 8. Citation Boundary

WeKnora / KB should provide evidence metadata such as resource, revision, source and verifiable location.

Documenting is responsible for making that evidence useful in the document workflow:

- show the citation;
- open the correct revision;
- navigate to the correct page/block/range when available;
- highlight the source where possible;
- expose stale/missing precision honestly.

**Never fabricate a page or source location.**

## 9. Current Prerequisite Engineering Work

David has also been asked to work on a prerequisite Agent24 integration path:

> **Generic Workspace → preserve OpenDesign → load WeKnora**

This is important enabling work, but it does **not** redefine David as the long-term owner of all Workspace, WeKnora, KB or Media responsibilities.

### 9.1 Target Workspace architecture

```text
                 Agent24
                    |
           Generic Workspace Host
          /          |           \
         v           v            v
  OpenDesign      WeKnora     OpenCreator
    Adapter        Adapter       Adapter
       |             |             |
       v             v             v
 OpenDesign       WeKnora      OpenCreator
```

Application-specific behavior belongs in adapters. The generic host should not accumulate application-specific dispatch such as `if app == WeKnora`.

### 9.2 Prerequisite implementation sequence

1. **Establish OD baseline** — build/run current Agent24 and record the real OpenDesign golden paths.
2. **Define generic Workspace contract** — manifest, instance, adapter, call context, result and events.
3. **Wrap OD through a compatibility adapter** — prove old and new paths are behaviorally equivalent.
4. **Integrate WeKnora** — initially prefer trusted adapter → WeKnora REST rather than assuming current Agent24 can directly consume WeKnora HTTP MCP.
5. **Keep WeKnora as an independent upstream-tracking fork** — minimize changes to its core knowledge algorithms.

## 10. WeKnora Fork Principle

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

## 11. High-Level Delivery Path

This is a dependency-oriented framework, **not yet a sprint schedule**.

### Phase A — Workspace foundation (prerequisite)

**Goal:** establish a reusable Agent24 Workspace host without regressing OD.

**Deliverables:**

- reproducible OD baseline/golden paths;
- generic Workspace contract;
- registry/host path;
- OD compatibility adapter;
- OD regression evidence;
- migration/rollback path.

### Phase B — Knowledge Workspace (prerequisite/dependency)

**Goal:** Agent24 can safely load and call the selected WeKnora fork.

**Deliverables relevant to Documenting:**

- upstream-tracking WeKnora fork;
- WeKnora adapter;
- Knowledge Workspace entry;
- search/source integration;
- citation preservation;
- identity/scope mapping;
- explicit health/capability states.

Long-term ownership of all WeKnora/KB operations is not implied by this phase.

### Phase C — Documenting foundation

**Goal:** Documenting exists as an Agent24-default callable business capability.

**Deliverables:**

- Document capability contract;
- stable document identity;
- revision semantics;
- source/artifact model;
- read/render boundary;
- WeKnora resource/revision mapping;
- Agent24 capability registration/integration;
- baseline acceptance fixtures.

### Phase D — Read & understand

**Goal:** a user or Agent can reliably consume real documents.

**Deliverables:**

- import/read path;
- preview/navigation;
- search and exact source jump;
- selected summarize/extract/translate/compare operations;
- citation UX;
- visible parse/index/error states.

### Phase E — Edit & review

**Goal:** AI output becomes reviewable document change rather than untracked generated text.

**Deliverables:**

- draft creation;
- targeted edits;
- undo or equivalent safe revision behavior;
- diff;
- accept/reject;
- conflict detection;
- creation of a new revision.

### Phase F — Render & deliver

**Goal:** the result is a real deliverable document.

**Deliverables:**

- layout preview;
- preflight checks;
- selected fixed/editable exports;
- conversion warnings;
- reopen/visual validation;
- attachment/package assembly where required;
- controlled delivery handoff to Agent24/connectors.

## 12. Dependencies and Team Inputs

| Dependency | Needed by Documenting | Owner boundary |
|---|---|---|
| Agent24 Core | capability registration, trusted caller context, permissions, approvals, run/cancel/model/tool access | Agent24 |
| WeKnora / KB | ingest/status, search, source, citation, ACL, revision mapping, context where applicable | KB/Knowledge |
| Document engine | rendering, structured edit, layout, revision-safe mutation, export | TBD / Documenting integration |
| OpenDesign/OpenCreator | context/artifact/run/cancel handoff | Workspace/Creative |
| Connectors | actual email/Drive/filesystem/business-system action + receipt | connector/Agent24 |
| Product samples | representative real workflows and sanitized files | team/product |

## 13. Open Decisions

These are intentionally **TBD** and must not be silently frozen by implementation convenience.

1. **Editable source of truth** — Documenting-managed, KB-managed immutable source, or external document system?
2. **Primary content model** — Markdown-first, DOCX-first, internal structured model, or adapters?
3. **DOCX fidelity level** — basic import/export vs comments/track-changes/headers/fields/complex tables/round-trip fidelity.
4. **PDF scope** — reading, OCR, form filling, page overlay, page operations and true content editing are separate capabilities.
5. **Collaboration depth** — single-user, async review, multi-user revision or real-time co-editing.
6. **Document engine/editor choice** — should follow representative document tasks, not precede them.
7. **First P0 workflow** — which real document task becomes the first end-to-end acceptance path?

## 14. Progress-Control Framework

GitHub Project should track execution; this document remains the stable design/ownership baseline.

Recommended lifecycle:

```text
Research → Contract → Prototype → Integrated → Tested → Released
```

Recommended issue dimensions:

- **Area:** workspace / knowledge / document-core / read / ai-action / edit / review / render / export / integration
- **Dependency:** agent24 / weknora / opendesign / opencreator / kb-media / connector
- **Priority:** P0 / P1 / Later
- **State:** planned / in-progress / blocked / review / done

Every implementation issue should link back to the relevant section of this document and identify its external dependencies explicitly.

## 15. Definition of Done

Documenting is not “done” because a UI exists or an LLM can answer questions about a PDF.

For an agreed P0 document workflow, completion requires evidence that:

- the original/source document is preserved or reliably referenced;
- document identity and revision are stable;
- the selected document can be opened and navigated;
- AI-derived facts can be checked against evidence;
- unsupported/failed parsing is visible rather than hidden;
- edits are reviewable and do not silently overwrite a changed baseline;
- a new revision can be saved and recovered;
- selected export formats actually reopen and pass the agreed fidelity checks;
- KB indexing state is distinguishable from document-save state;
- permission and delivery actions respect Agent24 control;
- failure/retry does not duplicate destructive or delivery side effects;
- the workflow works through Agent24 capability integration, not only through a bespoke demo UI.

## 16. Source Baseline

This working baseline is derived from the current project direction and the following design/research sources:

- iDoris product / OfficeSuite positioning;
- T005 Documenting report;
- T005 user scenarios & atomic capabilities;
- T005 David Xu handoff;
- T005 Agent24 kernel integration study;
- T006 Document ↔ KnowledgeBase/MediaBase service boundaries;
- T006 WeKnora Workspace development plan;
- Agent24 current architecture/ADR/code observations;
- WeKnora upstream capability documentation.

Where these sources contain research proposals rather than approved engineering decisions, this document keeps the corresponding item marked **proposed** or **TBD**.

---

## 17. Immediate Next Checkpoint

Before converting this framework into a detailed GitHub Project plan, confirm:

1. the current Agent24 branch/build/run baseline and OD golden path;
2. which subset of the Generic Workspace / WeKnora prerequisite work is assigned to David;
3. the actual KB/WeKnora test endpoint and responsibility boundary;
4. one representative P0 Documenting workflow and sanitized sample documents;
5. the initial Documenting ↔ Agent24 ↔ KB contract assumptions.

After these are confirmed, create milestones/issues from Sections 11–14 rather than expanding scope directly in implementation PRs.
