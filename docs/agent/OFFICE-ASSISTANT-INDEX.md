# OfficeAssistant Documentation Index

English | [简体中文](OFFICE-ASSISTANT-INDEX.zh-CN.md)

> Created 2026-10-16. All documents are planning drafts; nothing has been implemented or registered as a project in [`tasks.md`](tasks.md).

| Document | Language | Contents |
|---|---|---|
| [`research/T007-office-assistant-report.md`](research/T007-office-assistant-report.md) | English | Research report (single source of truth): five workflows, seven semantic requirements, four outbound capabilities requiring authorization, R0–R3, and the negative-case matrix |
| [`PLAN-OFFICE-ASSISTANT.md`](PLAN-OFFICE-ASSISTANT.md) | Chinese | Main plan: positioning and boundaries, initial requirements, OA-M1/M2/M3, team responsibilities, gates and red lines, and decisions pending |
| [`PLAN-OFFICE-ASSISTANT.en.md`](PLAN-OFFICE-ASSISTANT.en.md) | English (machine-drafted, native review pending) | English version of the main plan; the Chinese version prevails in case of conflict |
| [`OFFICE-ASSISTANT-TECH-DESIGN.md`](OFFICE-ASSISTANT-TECH-DESIGN.md) | Chinese | Technical design: layers, data contracts, approval binding / idempotency / receipts / recovery / outbound actions, relationship to K1, tests, and risks |
| [`OFFICE-ASSISTANT-TECH-DESIGN.en.md`](OFFICE-ASSISTANT-TECH-DESIGN.en.md) | English (machine-drafted, native review pending) | English version of the technical design; the Chinese version prevails in case of conflict |
| [`OFFICE-ASSISTANT-DESIGN-DISCUSSION.md`](OFFICE-ASSISTANT-DESIGN-DISCUSSION.md) | Chinese | Design discussion: four judgments checked against the code, attachment points A/B/C, and the Opus 5.5 assessment in §5 (not an approval) |
| [`OFFICE-ASSISTANT-DESIGN-DISCUSSION.en.md`](OFFICE-ASSISTANT-DESIGN-DISCUSSION.en.md) | English (machine-drafted, native review pending) | English version of the design discussion (including §5); the Chinese version prevails in case of conflict |
| [`OFFICE-ASSISTANT-INTERN-BRIEF.md`](OFFICE-ASSISTANT-INTERN-BRIEF.md) | English | Brief and structured feedback template for the Thai intern |
| [`OFFICE-ASSISTANT-TRANSLATION-NOTES.md`](OFFICE-ASSISTANT-TRANSLATION-NOTES.md) | Chinese | Sources of the language versions, Chinese–English terminology, and conflict-resolution rules |
| [`research/T007-office-assistant-report.zh-CN.md`](research/T007-office-assistant-report.zh-CN.md) | Chinese | Official Chinese version of the research report (from jhfnetboy/researcher, sharing the same source as the English version) |
| [`OFFICE-ASSISTANT-INTERN-BRIEF.zh-CN.md`](OFFICE-ASSISTANT-INTERN-BRIEF.zh-CN.md) | Chinese | Chinese version of the intern brief (for the China-side team) |
| [`OFFICE-ASSISTANT-INTERN-BRIEF.th.md`](OFFICE-ASSISTANT-INTERN-BRIEF.th.md) | Thai (machine-drafted; corrections from the intern are welcome) | For the Thai intern; technical terms remain in English, with Thai in parentheses; the English original prevails if the three language versions conflict |
| [`OFFICE-ASSISTANT-INTERN-BRIEF-TRANSLATION-NOTES.md`](OFFICE-ASSISTANT-INTERN-BRIEF-TRANSLATION-NOTES.md) | Chinese | Sources and status of the Chinese and Thai translations of the brief, and technical terms intentionally kept in English |
| [`SKILL-FEATURE-PROPOSAL.md`](SKILL-FEATURE-PROPOSAL.md) | Chinese | **Separate topic**: current-state verification of global Skill capabilities (loader / distribution from three sources / external import / quick creation), three implementation slices, three hard constraints, and decisions pending; companion issue [#1037](https://github.com/iDoris-ai/Agent24/issues/1037) |
| [`OA-K-KERNEL-GATE-DESIGN.md`](OA-K-KERNEL-GATE-DESIGN.md) | Chinese | **Detailed design of the OA-K kernel gate** (the module submits `(tool, exact input, request_id)` → the kernel runs the full pipeline); includes a compatibility matrix for four extension surfaces, safety rationale, alternatives, and implementation slices |

> **Decisions already taken (author, 2026-10-16)**: attachment point **B** is selected: create a first-party out-of-process domain OS module, `office` (isomorphic to `agent24-documents`), to hold the business state; **actions with external effects still execute in the kernel**, with a new kernel gate, **OA-K**, between the two. See the main plan §0.4 for details.

Reading order: report “Conclusion” and §6 → main plan §0 and §7 (decisions pending) → technical design §4 and §5 → intern brief.
