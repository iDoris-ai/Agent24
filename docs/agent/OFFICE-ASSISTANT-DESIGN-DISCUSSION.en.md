# OfficeAssistant Design Discussion: Checking Your Four Judgments Against the Agent24 Code

> **Translation status**: machine-drafted English translation of [`OFFICE-ASSISTANT-DESIGN-DISCUSSION.md`](OFFICE-ASSISTANT-DESIGN-DISCUSSION.md); native review pending. Where this translation and the Chinese version conflict, **the Chinese version prevails**. Terminology follows [`research/T007-office-assistant-report.md`](research/T007-office-assistant-report.md); see [`OFFICE-ASSISTANT-TRANSLATION-NOTES.md`](OFFICE-ASSISTANT-TRANSLATION-NOTES.md) (Chinese) for the glossary. References of the form `PLAN-OFFICE-ASSISTANT.md:NNN` and `OFFICE-ASSISTANT-TECH-DESIGN.md:NN` keep the Chinese file paths on purpose: **those line numbers refer to the Chinese originals**, not to the `.en.md` translations.

Status: **discussion draft (assessed by Opus 5.5, see section 5)**. Date 2026-10-16. Author: the coordinator.
Comparison baseline: `origin/main` (`92f9f0cf`, including the K1 release). Every "code fact" gives a file/line number or command and can be rechecked item by item.

> ⚠️ **Two facts in sections 1 and 4 of this draft have been corrected by section 5; when citing, the latter prevails**:
> - **F9**: memory is not just D1 — M1 (#720, 2026-10-07) has been merged; the agent loop now consumes `SessionLog`/`EventStore` (the raw text of every turn is written into `mem_events`)/`AssertionStore`; what is still not consumed is retriever/consolidator/knowledge.
> - **F10**: LocalOnly **already** covers the four paths main conversation/`/api/v1/chat`/explorer/session compaction (after K1); this draft's claim that it is "not propagated" is outdated. What is really not done is the opposite direction (no production entry point writes `CloudAuthorized`) and the unified egress decision for `http_fetch`/`shell_exec`.
> - Two further judgments were too narrow: **F5** (the kernel already has the primitive closest to action/approval/receipt, `ModuleApproval`) and **F11** (the attachment point already has a precedent: Documenting's ADR-DOC-01 has already ruled that office capabilities use an out-of-process domain OS).

---

## 1. Your Four Judgments, Checked One by One

| # | What you said | Code facts | Verdict |
|---|---|---|---|
| 1 | It leans toward a **tool-calling engine** that calls many tools for **process-style chaining** of work scenarios | **The kernel already is this engine.** `rust/crates/agent24-tools/src/lib.rs:266` has `pub trait Tool`, `:305` has `ToolRegistry`; tools go through a fixed pipeline **normalize → capability allowlist → approval gate → execution with timeout**; `agent24-policy` is the approval broker + tool gate (Guardian is off by default and only on with `A24_GUARDIAN=1`); `agent24-agent` is the run manager + agent loop; **MCP external tools are wrapped as ordinary `Tool`s and go through the same approval pipeline** | ✅ **Right, but the ownership is backwards**: OfficeAssistant should not build another engine; it should **use this one** |
| 2 | You can design some **Skills to chain tool calls together** | Agent24 Skills are **not orchestration**: `docs/specs/SPEC-SKILL-DISTRIBUTION.md` (status "**design finalized, not implemented**", belongs to M5) defines **distribution and upgrading of instruction packages** — three sources bundled/distro/user, precedence user > distro > bundled, a same-name package replaces the whole with no field-level merging. Separately, `rust/crates/agent24-memory/src/knowledge.rs` is an **instruction layer** (CLAUDE.md-style layered instruction merging + trigger-based injection + a review-gated automatic memory inbox), and is also **not** a retrieval-style knowledge base | ⚠️ **Needs correction**: "using Skills to chain tools" has no counterpart in Agent24 |
| 3 | **Embedded on Agent24** | Agent24 has four kinds of extensions (`docs/ARCHITECTURE-LAYERS.md` L6): ① in-process DomainModule (🟡 production catalogue is empty), ② out-of-process domain OS (Sin90/Cos72 ✅), ③ attached modules (AgentEar ✅), ④ sidecar (Open Design); plus MCP (tool-level). The kernel state machines are only **Run / Approval / ToolCall** (`rust/crates/agent24-core/src/transitions.rs`) | ✅ **Right, but first choose "which face to attach to"** |
| 4 | Use Agent24's **context memory, knowledge base, and other shared services** | Memory: the agent loop **only consumes D1** (`KvStore` + `CanonicalSession`); higher layers such as `retriever`/`consolidator`/`knowledge` are **not yet consumed by the agent loop** (`ARCHITECTURE-LAYERS.md` L3 table, belongs to M1/P1). Routing: `TaskProfile::default()` is still passed by the main conversation/sub-agents/REST, **LocalOnly is not propagated**; the iDoris provider is not wired up (no `IDORIS_URL`). In the event schema `workflow` / `artifact` / `receipt` **appear 0 times** and `step` only 2 times → the fields in report §4 are **contracts still to be added**, not existing interfaces | ✅ **Direction right, maturity must be discounted** |

### A more accurate statement for each

1. **The tool engine already exists; do not rebuild it.** What your "process-style chaining" lacks is not a tool engine but **business-layer workflow state**:
   the kernel's Run/ToolCall state machines describe only "this one run/this one tool call"; **there is no business-semantic layer of workflow / step / action / artifact / receipt** (verified by command:
   in `git show origin/main:protocol/events.schema.json` the hit count for `workflow`/`artifact`/`receipt` is 0).
2. **"Skills chaining tools" should be split into two things**:
   - **Deterministic chaining → the workflow layer** (code/state machine). Office processes require "a fixed plan version, a per-step `action_id`, idempotency keys, separate receipts" (report §3);
     these are **testable contracts**, and written as instruction text they cannot be mechanically verified.
   - **Knowledge and rules for the model → Skills/instruction packages** (this is also the position in the report's §1 table: a Skill is "input/output rules, steps, quality constraints, and applicability conditions",
     and "**Text instructions alone neither grant permission nor prove that a connector is available**").
3. **Choose one of three attachment points (this is the technical decision this discussion most needs to settle)**:

   | Attachment point | Authorities reused | Cost | Relationship to "sole control plane" |
   |---|---|---|---|
   | A. **Thin business layer** in the composition root (new module inside `agent24d` + new REST/WS projections) | Directly reuses run/approval/audit/store | Requires touching the kernel composition root and protocol; kernel responsibilities widen | Most straightforward: no new channel |
   | B. **Out-of-process domain OS module** (os-proto callbacks, like Sin90/Cos72) | Via `KERNEL_OOP_GRANTS` (Events/Memory/Approval/Scheduler/Models) | Requires module packaging and supervision; callback surface narrower than kernel REST | Naturally isolated, but whether "business orchestration" belongs in a domain OS needs to be argued |
   | C. **Sidecar** (like Open Design) | Requires capability mode and route-level resource authorization — **not yet implemented today** | Heaviest, and under current capability mode basically unusable apart from `GET /api/v1/health` and `GET /api/v1/models` | Consistent with the target form of ADR-001/005, but **not something the first version can take** |
4. **The actual maturity of shared services** (determines what the first version can promise):
   - **Can be relied on**: D1 memory (KvStore/CanonicalSession, already used by the agent loop), approval/audit/cancellation/events, the tool pipeline, model routing (local tier).
   - **Cannot be relied on / not wired up**: higher-level memory (retriever/consolidator), retrieval-style KB (on the iDoris-Components side, visibility fixed to public, **not permission-aware**),
     LocalOnly propagation to the main conversation, iDoris egress redaction, resource-level authorization in capability mode.
   - Therefore items 2 and 4 of report §5 ("trusted local materials + mock connectors", **end-to-end redaction must not be promised**) are not conservatism; they are consistent with the current state of the code.

---

## 2. Design Constraints Derived from This (My Recommendations, Pending Opus Review)

1. **The kernel remains the sole control plane**: every external action of OfficeAssistant goes through the existing approval/consent gates;
   no separate execution channel may be opened because "this is a workflow" (report §5 item 1).
2. **OfficeAssistant = a thin workflow layer** (i.e. the report's §4 preferred "Agent24 kernel + a thin workflow layer"):
   what is added is **business state** (workflow / step / action / artifact / receipt), which **uses different IDs and explicit references** from the kernel's Run/Approval/ToolCall
   (the report already raised in §1 the requirement to separate `workflow_id` / `run_id` / `workspace_id` / `artifact_id`);
   **authoritative state is never duplicated**, and the frontend does not maintain a second set of approval results.
3. **The correct position of Skills**: human-readable SOPs / instruction packages (for model understanding and quality constraints);
   **they carry no authorization and no deterministic orchestration**; orchestration is driven by the workflow layer, and permissions are adjudicated by the kernel.
4. **Tighten first-version boundaries per report §5**: real external actions are confirmed individually; no unbounded ongoing authorization;
   mock and real must be distinguishable in the interface; "events are not proof of delivery" must be carried into acceptance (the negative-case matrix of report §6).

---

## 3. Things That Need to Be Added or Clarified (Candidate List for Later Slices)

| # | Item to be decided | Why it must be decided first |
|---|---|---|
| 1 | Where the four kinds of business records live (new tables / projections / events) and their relationship to the three kernel state machines | Decides whether an ADR is needed; decides how idempotency and recovery are done |
| 2 | Where "plan version + approval binding to the final payload" is implemented (whether kernel approval is enough, whether action-level approval is needed) | Report §3's "approval must not be silently reused" must rely on mechanism, not copy |
| 3 | Idempotency and reconciliation: external success + local timeout → query first; unknown outcomes must not be blindly retried | Two rows of the report's §6 negative-case matrix correspond directly |
| 4 | LocalOnly propagation and **governing outbound paths separately** (LLM / email / Notion / HTTP / MCP are different exits) | K1 has narrowed the main conversation to LocalOnly, but `TaskProfile::default()` is not propagated |
| 5 | The authorization surface usable in the first version: today it is a legacy full-authority bearer + incomplete capability mode | Decides the boundary of "trusted local materials + mock"; isolation cannot be promised externally |

---

## 4. Assessment Questions Left for Opus 5.5

1. Of the four verdicts above, **which did I get wrong, and which did I judge too narrowly**? Please point to the code or the report text.
2. Should the "thin workflow layer" live in **A (thin composition-root layer) / B (out-of-process domain OS) / C (sidecar)**? Give the reasoning for the trade-off and a recommendation for the first slice.
3. If the author insists on "using Skills to chain tools", what is the **closest form that is also correct**? Please give an executable alternative (e.g. Skill as SOP text + execution by the workflow layer).
4. Are these constraints consistent with the cut of the three milestones (M1 local loop / M2 one real connector / M3 reuse and bounded delegation)? How should it be adjusted?
5. What other risk points "where this sentence will mislead" are there that I did not point out?

---

## 5. Opus 5.5's Assessment (2026-10-16)

> Author of this section: Claude Opus 5.5 (`claude-opus-5-5`). Based on the first four sections of this discussion draft and the sources of truth named in the task, plus items found during checking:
> `docs/documenting/adr/ADR-DOC-01-placement-and-integration.md`, `docs/agent/K1-RELEASE-NOTES-20261016.md`,
> `rust/apps/agent24-documents/`, `rust/apps/agent24d/src/approval_callback.rs`. **This section is not an approval**, nor does it decide on the author's behalf.
> Checking baseline: this worktree's HEAD `92f9f0cf`, the same as the discussion draft's baseline. All line numbers below refer to this commit; anything not verified is noted.

### 5.1 Fact check (item by item)

| # | Claim in the discussion draft | Check result | Evidence |
|---|---|---|---|
| F1 | `agent24-tools/src/lib.rs:266` has `pub trait Tool`, `:305` has `ToolRegistry` | **Correction: wrong line numbers.** `pub trait Tool` is at `:389`, `pub struct ToolRegistry` at `:1063`. `:266` is the `Debug` impl of `ToolContext`, `:306` is `pub trait ApprovalGate` | `grep -n "pub trait Tool\|pub struct ToolRegistry" rust/crates/agent24-tools/src/lib.rs` |
| F2 | The fixed pipeline is normalize → capability allowlist → approval gate → execution with timeout | **Confirmed.** File header `:4`; the step comments inside `dispatch` are near `:1336` (1), `:1343` (2), `:1351` (3), `:1400` (4). After K1, module tools also pass an additional consent gate before entering the runtime (`ModuleTool`, from `:546`) | Same file |
| F3 | MCP external tools are wrapped as ordinary `Tool`s and go through the same pipeline | **Confirmed, with one addition:** MCP tools are uniformly assigned `RiskClass::External` | `rust/crates/agent24-mcp/src/lib.rs:269-273` |
| F4 | Guardian is off by default and only on with `A24_GUARDIAN=1` | **Confirmed** (only the environment variable name was confirmed; the switch logic was not read line by line) | `rust/apps/agent24d/src/server.rs` contains `A24_GUARDIAN` |
| F5 | The kernel state machines are only Run / Approval / ToolCall | **Too narrow.** `transitions.rs` also has `ScheduleHealth` (`:102-111`). More importantly, the kernel already has a **ready-made primitive closest to action/approval/receipt** that the discussion draft does not mention: **ModuleApproval** (`agent24-protocol/src/types.rs:590-640`, `agent24-store/migrations/0005_module_approvals.sql`). It carries a `request_id` idempotency key `UNIQUE(module, request_id, kind)`, a `payload_digest` computed by the kernel itself, `expires_at` → `timed_out` (fail-closed), and an `executed_at` that only means "the kernel executed it" | See left |
| F6 | In the event schema `workflow`/`artifact`/`receipt` are 0 and `step` is 2 | **Confirmed.** Both occurrences of `step` are in description text (`protocol/events.schema.json:506`, `:663`) and unrelated to business steps | `Grep "(?i)workflow\|artifact\|receipt\|step" protocol/events.schema.json` |
| F7 | Skills have no counterpart; `SPEC-SKILL-DISTRIBUTION` is "design finalized, not implemented" | **Confirmed, and more thoroughly than the discussion draft says:** a case-insensitive search for `skill` under `rust/` returns **0** hits; the kernel has no Skill loader at all. The five Skills mentioned in the report are in the iDoris-Components repository (report §2 [S03]–[S07]), not in Agent24 | `Grep -i skill rust/` → 0; `SPEC-SKILL-DISTRIBUTION.md:3` |
| F8 | `knowledge.rs` is an instruction layer, not a retrieval-style KB | **Confirmed** (`knowledge.rs:1-27`). Addition: the agent loop does not consume it; `KnowledgeBase` cannot be found by search in `agent24-agent/src` | Same as left |
| F9 | Memory: the agent loop **only consumes D1**; retriever/consolidator/knowledge are not consumed | **Correction (outdated).** M1 was merged into main on 2026-10-07 (#720, `tasks.md:24`). The agent loop now consumes `SessionLog`, `EventStore` (the raw text of every turn is written into `mem_events`), and `AssertionStore` (facts the user explicitly asked to "remember", injected as **data blocks** on recall) (`session_memory.rs:5-11`, `:17-30`; `retain.rs:1-12`; `lib.rs:724`). retriever/consolidator/knowledge are **still not** consumed, so that half holds. The `ARCHITECTURE-LAYERS.md` cited by the discussion draft was written on 2026-10-03, before #720 | See left |
| F10 | `TaskProfile::default()` is still passed by the main conversation/sub-agents/REST, **LocalOnly is not propagated** | **Correction (overturned by K1).** The main conversation run (`agent24-agent/src/lib.rs:1726`), `/api/v1/chat` (`agent24d/src/routes.rs:569`), explorer (`subagent.rs:231`), and session compaction (`lib.rs:240`) all go through `task_profile_for_run` / `merge_source_policy` (`lib.rs:49-56`, `:482-495`). The source tag for user input is fixed as `LocalOnly`, and when not found it is also treated as `LocalOnly`, so **all four paths are LocalOnly today, with remote fail-closed** (K1 release notes `:149-158`). What is really not yet done is the **opposite direction**: no production entry point can write `CloudAuthorized` (release notes `:31`), and `http_fetch`/`shell_exec` have no unified egress decision (`:33`). Row 4 of section 3, "K1 has narrowed the main conversation to LocalOnly, but `TaskProfile::default()` is not propagated", contradicts itself; the first half prevails | See left |
| F11 | Four kinds of extensions: ② out-of-process domain OS is Sin90/Cos72 | **Too narrow: the most direct precedent is missing.** This repository's `rust/apps/agent24-documents/` (Documenting, a first-party out-of-process domain OS; `rust/Cargo.toml:29` lists it as a workspace member). Its ADR-DOC-01 (Accepted, 2026-10-07) ruled on exactly question 2 of section 4 of this topic: "which face first-party office capabilities attach to". The conclusion is ②, and it **rejected** "kernel built-in proxy tools", on the grounds that the kernel holds no domain nouns, consistent with T11 (ADR-DOC-01 D1/D2/D4; `decision.md:1126`) | See left |
| F12 | `KERNEL_OOP_GRANTS` = Events/Memory/Approval/Scheduler/Models | **Confirmed** | `rust/apps/agent24d/src/domain.rs:116-122` |
| F13 | C (sidecar) is basically unusable under capability mode apart from health/models | **Confirmed** | `ARCHITECTURE-LAYERS.md:158` |

**Three further facts the discussion draft did not mention but which affect the judgment:**

- **There is no entry point for "non-model callers" to go through the tool pipeline.** In production code, the only callers of `ToolRegistry::dispatch` are the agent loop (`agent24-agent/src/lib.rs:2028-2030`) and sub-agents (`subagent.rs:317`). The calls in `module_tool_runtime.rs` are all under `#[cfg(test)]` modules (from `:177`). In other words, today's kernel engine is **model-driven**: to "deterministically call tools step by step and pass approval", there is no ready-made interface. ADR-DOC-01 §2 D1 also states "modules also cannot call Agent tools (📐)".
- **The `Gate` of ModuleApproval (binding) accepts only a closed set, and today that closed set contains only `schedule_callback`.** All other actions can only go through `Advise`, and `Advise` is **not binding** (`approval_callback.rs:133-197`; `types.rs` field comment: "for `Advise`, this is knowledge, not a safety control"). Today an out-of-process module **cannot obtain** a kernel approval that binds its own execution of "send email/write to Notion".
- **Consent for module tools cannot be written on the production path today.** Grants only appear in test code (K1 release notes `:27`). The first PR that adds a grant route must handle F1/F3/F6 at the same time (`:59`).

### 5.2 Answers to the 5 questions in section 4

**1) Of the four verdicts, which is wrong and which is too narrow?**

- **Wrong: two facts in verdict 4 are outdated**, see F9 and F10. There are two consequences. First, M1's "local models only" is today **automatically guaranteed** by the kernel, so OfficeAssistant does not need to patch LocalOnly propagation; conversely, any cloud processing is **impossible** today. Second, for anything that goes through the agent loop, the raw text is written into `mem_events` (`retain.rs:1-12`). If third-party sensitive materials such as meeting transcripts or email bodies enter through the main conversation, they will be retained by the memory log. See 5.3 risk 4.
- **Too narrow: verdict 1.** "The kernel already is this engine" holds only in the **model-driven** sense. "Process-style chaining" needs **deterministic** driving, and the kernel has no such entry point (see above). The direction "do not rebuild" is right, but "just use it" does not hold: using it requires adding an entry point (5.4). In addition, "there are no action/approval/receipt semantics" is too absolute. ModuleApproval already has an idempotency key, payload digest, rejection on expiry, and an execution timestamp; it lacks only two pieces: "binding force over actions executed by the module" and "receipts".
- **Too narrow: verdict 3.** It misses the Documenting precedent (F11). With that added, the attachment-point question is no longer an open question; it can be derived from ADR-DOC-01.
- **Verdict 2 is correct.** It can be stated more strongly (F7): there are no runtime Skills in Agent24 at all.

**2) Should the thin workflow layer live in A / B / C?**

**Recommend B: a first-party out-of-process domain OS (provisionally named `office`, isomorphic to `documents`).** But with one precondition: **real external actions must not be executed inside this module's process**, see 5.4.

| Trade-off | A thin composition-root layer | **B out-of-process domain OS** | C sidecar |
|---|---|---|---|
| Precedent and already-decided direction | Opposite in direction to T11 (first-party OSes moved out of the kernel) and ADR-DOC-01 D4 (rejects domain nouns in the kernel) | Consistent with Documenting, Sin90, Cos72 | The form of ADR-001; unusable today (F13) |
| Does OA-M1 require kernel changes? | Yes: new tables, migrations, REST/WS projections. K1 release notes `:89-121` state that store migrations **cannot be rolled back** | **No**. Models go through the `Models` callback (manifest declares `model_access: local_only`), progress through `Events`, and business records live in the module's own `data_dir` | — |
| Binding approval | Ready-made: kernel tools are approved individually, and the approval object is the exact input of this call | None in M1 (only `Advise`); M2 needs a kernel slice | — |
| Egress controlled by the kernel | Connectors written in the kernel are controlled (after egress gate 6b.3 lands) | Connectors written in the module process are **not controlled** (same UID, no OS sandbox) | — |
| Platforms | All platforms | Unix only; Windows has to wait for DEP-C2 (ADR-DOC-01 D2/D10) | — |

A's only real advantage is one thing: **binding approval and egress control are ready-made**. B can win that back by keeping "actions with external effects" executed in the kernel (5.4). That way B gets three things at once: zero kernel changes in M1, no conflict with T11, and egress still adjudicated by the kernel. `OFFICE-ASSISTANT-TECH-DESIGN.md:58` leans toward (a), with the reason "(b) would introduce a consent grant prerequisite". That reason does not hold for M1: M1 does not advertise tools to the agent loop and needs no consent. For M2, both A and B require a new kernel entry point, just of different shapes.

**Recommended first slice (OA-S0, ≤300 lines, no changes to kernel, protocol, or store):**

1. A one-page semantics mapping (this is the PLAN's OA-M1-01). Map the seven semantics of report §3 one by one onto **existing** primitives: `RunStatus`, kernel tool approval, ModuleApproval's `Gate`/`Advise`, `ModuleTool` consent. Add a column per item: "**is it binding today**: yes / no / only holds in mock".
2. A **pure-logic** workflow/action/receipt state-machine library with no I/O. It lives in the future `office` module, and the kernel does not depend on it. Unit tests cover the rows of report §6's negative-case matrix that **can be decided at the state layer**: row 1 (reject/expire/no channel → no action), row 2 (old approval invalid after payload change, compared by digest), row 4 (retry only incomplete steps), row 5 (after cancellation, truthfully list completed steps), row 14 (a mock receipt cannot be upgraded to "delivered").

This slice is insensitive to the final A/B choice. Even if the author switches to A, it can be moved over as is, so it will not be wasted work.

**3) The author insists on "using Skills to chain tools": the closest form that is also correct**

The correct form is **"a Skill is SOP data; the workflow definition is a code-reviewed closed set"**. Skills appear only in LLM steps, as material with provenance. A directly executable form is as follows:

```yaml
# office 模块内的 workflow 定义（代码审过、随版本发布；用户不可写）
workflow: meeting_to_actions
version: 3
steps:
  - id: extract
    kind: llm_extract          # 闭集：llm_extract | human_review | action
    sop: { skill: meeting-to-tasks, version: "1.2.0", sha256: "<hash>" }   # 只作为提示材料注入
    output_schema: minutes_v1  # 结构化输出，按 schema 校验，不合格则失败
  - id: review
    kind: human_review         # 用户编辑会生成新的 artifact 版本
  - id: deliver
    kind: action
    tool: mock.notion_write    # M1 只能是 mock.*；真实工具名必须在 manifest ∩ 用户 consent 内
    payload_from: review.artifact   # 批准绑定这个 artifact 的版本和 digest
```

*Translator's note: the code block is kept verbatim. English rendering of its comments, in order: "workflow definition inside the office module (code-reviewed, shipped with releases; not user-writable)"; "closed set: llm_extract | human_review | action"; "injected only as prompt material"; "structured output, validated against the schema, fails if invalid"; "user edits produce a new artifact version"; "in M1 this can only be mock.*; real tool names must be within manifest ∩ user consent"; "approval is bound to this artifact's version and digest".*

Accompanying rules:

- Skill text **cannot** add steps, change the `tool` or destination, or decide the risk level. The Skill row of the report's §1 table says "Text instructions alone neither grant permission".
- `sop.sha256` counts toward the **plan version**. When a Skill is upgraded the plan version changes with it, and old approvals are automatically invalidated (consistent with L-APPR's "approval is not silently reused").
- Skill text is injected per L-CTX-2: nameable, with a hash, with a size limit. It is injected as **data**, not as a system instruction, consistent with how M1 recall blocks are handled (`session_memory.rs:24-30`).
- If the author wants the Claude Code-style experience of "the model reads the SOP and calls tools itself", it is **allowed only in the preparation phase** (reading, organizing, drafting, no external effects). Once an `action` is reached, control returns to the workflow layer, and each action goes through kernel approval individually.
- Distribution and upgrading of Skills will later follow `SPEC-SKILL-DISTRIBUTION` (M5). OfficeAssistant should not implement M5 early for this; in the first version, SOPs are packaged directly with the `office` module.

**4) Are the constraints consistent with the three milestones? How should they be adjusted?**

The overall direction is consistent, but four places need adjustment:

- **OA-M1-04 and OA-M1-05 conflict with the recommended B.** The PLAN says "inside the kernel store, including migration" and "wire the extraction step into a run" (`PLAN-OFFICE-ASSISTANT.md:145-146`). Recommend changing to: records live in the module's `data_dir`, and extraction goes through the `Models` callback. This has two benefits: transcript content does not enter the agent loop and so is not written into `mem_events`; and an irreversible kernel migration is no longer needed.
- **OA-M1's "approval bound to payload" can only hold inside the module and against mocks.** Acceptance items 5 and 6 (`PLAN-OFFICE-ASSISTANT.md:159-160`) must be explicitly labeled: **this verifies workflow semantics, not a kernel constraint**. It must not be written up as "kernel approval already reused". If `Advise` is used to record approvals, it must also be labeled non-binding.
- **Insert an explicit kernel gate OA-K between M1 and M2** (not part of the OA track; belongs to the K1 track or #735). Its content is a kernel ADR making "external actions requested by modules" executed by the kernel with binding approval (5.4). It is **the same kind of need** as "modules calling Agent tools / module tool authorization" listed by Documenting in the dependency table of ADR-DOC-01 §3, and should be merged into a single ADR rather than having OA open a separate one. Until OA-K lands, M2 gets no date. The PLAN's D7 already points out this dependency; this only promotes it from a "decision pending" to "a gate between milestones".
- **M3 is too wide.** Reuse for a second workflow, bounded delegation design, LINE, and evaluation of extensions are all placed in the same milestone. Recommend that M3 keep only "reuse for a second workflow" and its "abstraction failure" criterion. Bounded delegation gets a separate design track: it is in tension with L-APPR-5/7 (`APPROVAL.md:89`, `:116`), must first go through a separate law PR (`laws/README.md:43`), and its pace should not be tied to a feature milestone.

In addition, B means the first version **supports only macOS/Linux**, which the author needs to confirm. Documenting already has the same precedent (ADR-DOC-01 D10).

**5) What other risk points are there "where this sentence will mislead"?**

1. **"Reuse kernel approval."** The kernel has three kinds of approval, each with different binding force: per-call tool approval (binding), ModuleApproval `Gate` (binding, but the closed set contains only `schedule_callback`), and `Advise` (not binding). Saying "reuse approval" without naming which one makes it very easy to slide into `Advise`, creating a false sense of security.
2. **"LocalOnly = does not leave the machine."** LocalOnly constrains only **model routing**. Network egress from module processes, `http_fetch`, and `shell_exec` is not constrained by it today (K1 release notes `:33`). Also, model URLs in LAN or domain-name form are classified as Remote (same document `:125`).
3. **"Reuse run."** In B, the `Models` callback is **not a run**: there is no run-level cancellation and it does not enter the run event stream. In A, going through the agent loop writes the raw text into the memory log. The two paths have different costs, and the phrase "reuse run" hides that difference.
4. **"`executed_at` / event = completed."** `executed_at` only means the kernel scanner executed a closed-set action (`types.rs` field comment); it has nothing to do with "the other party has received it". This is in the same family as report §5 item 8, but lands on a different field.
5. **"First-party."** ADR-DOC-01 D4 item 2 specifies: until "first-party default installation (pin + sha256)" lands, first-party module tools are also treated as `External`, and every call requires approval. OA cannot get the `WriteLocal` draft exception just because "we wrote it ourselves" (the L-APPR-5 exception also requires host verification, `APPROVAL.md:96`).
6. **The public proxy has limits: 1 MiB / 30 s, no streaming** (ADR-DOC-01 §1 table). Long transcripts and local-model extraction will both exceed them, so a job + events pattern is required (Documenting's ADR-DOC-02 §7). "Send the transcript to the assistant" cannot be done with one synchronous request.
7. **"There are already five Skills."** Those five are in the Components repository; Agent24 has no loader (F7). Calling them "existing" in the Agent24 context amounts to writing a plan up as complete.

### 5.3 Key design risks (ordered by severity)

1. **[Critical] There is no binding approval primitive for module-driven external actions.** The two implementations easiest to write today are: record an entry with `Advise`, or have the module check approval itself and then send by itself. Both turn report §3's "approval bound to the final payload" into mere copy. This must be solved by the kernel before OA-M2 (5.4).
2. **[High] Egress escapes the kernel.** If connectors (email, Notion, LINE) are placed in the `office` module process or in generic `http_fetch`, ADR-K1-02's egress gate (6b.3) cannot govern them. The same-UID process threat model does not defend against a "cooperative but buggy" module in the first place: a single bug in the module could send one extra email.
3. **[High] Authority creeps into the kernel (the cost of A).** Business tables enter the kernel store, and migrations cannot be rolled back; domain nouns such as workflow/minutes appear in the kernel, in the opposite direction to T11 and ADR-DOC-01. Once merged, it is very hard to move them out later.
4. **[High] Third-party sensitive raw text enters the memory log.** The agent loop writes the raw text of every turn into `mem_events`, and the pause switch does not prevent this (`retain.rs:1-12`). Deletion rights and volume governance are pending items of C3 P1 (`tasks.md:25`) and are not yet done. Once a transcript or email enters through the main conversation, there is no deletion promise that can be honored.
5. **[Medium] Extraction quality and mainline bandwidth.** Whether local-model extraction omits items or fabricates commitments decides whether the product is usable. This must be proven by the gold-standard gate (PLAN F1.1/OA-M1-03), not by how the page looks. In addition, OA-K competes with C3 P1 and DM-SPIKE for kernel review bandwidth. By the rules the OA track yields (`PLAN-OFFICE-ASSISTANT.md:8`), so M2's timing cannot be promised.

### 5.4 A better overall design: divide authority by "has external effects or not"

| Layer | Owned by | Content |
|---|---|---|
| Business state | The `office` out-of-process first-party domain OS (B) | Workflow/plan versions, artifact versions, human edits, SOP Skills, business projection of receipts, mock connectors |
| Actions with external effects | **The kernel** | Connectors are tools registered in the kernel (MCP or first-party connector tools, `External`, declaring `target_arg`). Real outbound actions are executed **only** in the kernel |
| The single seam between them (OA-K, requires a kernel ADR) | The kernel adds a callback: the module submits `(tool, exact input, request_id)` | The kernel runs the full pipeline on it: consent → individual human approval (the approval object is the exact input) → egress gate → execution with timeout → hash-chained audit. It returns a typed result and a receipt. `request_id` serves as the business idempotency key (following the `UNIQUE` precedent of `module_approvals`). **The kernel does not retry automatically** (consistent with ADR-DOC-01 D4 item 3) |

Its relationship to the existing constraints:

- Constraint 1 of section 2 (sole control plane) is stronger than with A. The outbound code is not in the business-layer process at all, so "opening a separate execution channel" is structurally impossible.
- Constraint 2 (thin workflow layer, no duplicated authority) is kept, just moved out of process.
- Constraint 3 (position of Skills): see question 3 in 5.2.
- Constraint 4 (first-version boundaries) is unchanged.
- OA-K adds a new kind of `Capability`. Per `ARCHITECTURE-LAYERS.md` §5, this is the kind of change with **the widest impact**; an ADR must be written first and it must land following the ME-4 slicing. But it is not specific to OfficeAssistant: Documenting's "modules calling Agent tools" (ADR-DOC-01 §3) and Cos72's channel actions both need it, so it should be merged into #735, **without changing C3 P1's mainline priority**.
- **Alternative:** build the connectors as the `office` module's own `ModuleTool`s. The K1 contract already exists; only the grant route is missing. It is cheaper, but egress stays in the module process (risk 2). If the author chooses it for speed, the ADR should record "egress not constrained by the kernel egress gate" as an explicit deviation.

### 5.5 Not verified in this section

- For `A24_GUARDIAN`, only the existence of the variable was checked; the switch logic was not read line by line.
- That the `dispatch` calls in `module_tool_runtime.rs` are all inside test modules: the basis is that the test module starts at `:177` and all call line numbers are greater than that. It was not confirmed one by one which `mod` each line belongs to.
- The job pattern of ADR-DOC-02 §7 is cited only via ADR-DOC-01 and the comments in `domain-os.yml`; the original text of ADR-DOC-02 was not read.
- The callback shape in 5.4 is a design proposal; no code was written or compiled to verify it.
- No adversarial recheck by Codex was performed this time: the codex MCP connection failed in this session.
