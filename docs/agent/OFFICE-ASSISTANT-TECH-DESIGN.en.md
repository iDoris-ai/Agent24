# OfficeAssistant Technical Design (Draft)

> **Translation status**: machine-drafted English translation of [`OFFICE-ASSISTANT-TECH-DESIGN.md`](OFFICE-ASSISTANT-TECH-DESIGN.md); native review pending. Where this translation and the Chinese version conflict, **the Chinese version prevails**. Terminology follows [`research/T007-office-assistant-report.md`](research/T007-office-assistant-report.md); see [`OFFICE-ASSISTANT-TRANSLATION-NOTES.md`](OFFICE-ASSISTANT-TRANSLATION-NOTES.md) (Chinese) for the glossary.

> Status: **design draft, pending review**; written 2026-10-16, based on that day's main (`92f9f0cf`). This is not an ADR and approves no implementation; the parts that need to be finalized are listed at the end as "pending ADR".
> For the product plan see [`PLAN-OFFICE-ASSISTANT.en.md`](PLAN-OFFICE-ASSISTANT.en.md) (hereafter "the plan"); for the source of truth see [`research/T007-office-assistant-report.md`](research/T007-office-assistant-report.md) (hereafter "the report").
> All code locations cited were actually opened or searched in the worktree during this pass; line numbers correspond to the current tree and will drift as the code changes.

---

## 1. Technology Stack

### 1.1 Carried over (no new frameworks)

| Layer | Existing foundation | Basis |
|---|---|---|
| Kernel | Rust daemon `agent24d`: Tokio/Axum, REST/WS; store uses SQLite (sqlx) + hash-chained audit; policy/tools/MCP/model router/memory/scheduler are existing kernel services | Report §4 "Existing reusable foundations"; `../ARCHITECTURE-LAYERS.md` §1 diagram and §1.1 dependency table |
| Desktop | TypeScript / Electron / React / Vite; i18n already exists (Thai entries found by search, `apps/desktop/src/renderer/i18n/index.ts`) | Same paragraph of report §4; search in this pass |
| Protocol | `protocol/openapi.yaml` (hand-written, authoritative), `protocol/events.schema.json` (exported from Rust) | `../ARCHITECTURE-LAYERS.md` §2 L0 |
| Authorization/audit | K1: `module_consents`, `module_consent_revocations`, `run_source_tags`; `StoreConsentGate`; typed audit of module tool `PreDispatch`/`Terminal` | `K1-RELEASE-NOTES-20261016.md` lines 15–31 |
| Models | `ModelRouter` (`TaskProfile` routing; `LocalOnly` selects only `Local`/`Lora`) | Same, lines 33, 149–158 |

### 1.2 Explicitly not done

- **Do not introduce a second general-purpose Agent framework**, nor use one to replace the kernel (report §4 "Recommended trade-offs", last row).
- **Do not add a database merely because the historical prototype mentioned Postgres/LangGraph**; the strings returned by Components' `assistant_status` do not establish that those dependencies are deployed (report §4 paragraph 2 and the paragraph after the table). LangGraph + an independent state store is a candidate only after it has been shown that Agent24 cannot meet the need (same table, row 3).
- **Do not lock in a specific model version**; actual meeting-extraction and multi-step tool evaluations decide whether a higher tier is needed (report §4 "For models…").

---

## 2. Layers and Responsibilities

```
┌───────────── UI 投影层（Desktop）─────────────┐
│ 开始任务 · 任务详情 · 连接与权限（+ 技术页）    │  只渲染内核快照与服务端给的选项
└──────────────────────┬────────────────────────┘
                       │ REST / WS（读快照 + 事件；断连后重读快照）
┌──────────────────────▼────────────────────────┐
│ 业务编排层（薄）：workflow / step 状态机        │  计划版本、步骤依赖、投影状态
│   不持有审批/授权结论，只引用内核记录           │
└──────────────────────┬────────────────────────┘
                       │ 走内核 run / 工具调用 / 审批门
┌──────────────────────▼────────────────────────┐
│ 控制面（内核 agent24d）：run · 审批 · consent ·  │  唯一权威：状态、批准、授权、审计
│ 来源标签/LocalOnly · 审计 · workspace          │
└──────────────────────┬────────────────────────┘
                       │ 注册为工具（内置或模块），受同一门禁
┌──────────────────────▼────────────────────────┐
│ 连接器层：本地材料 · mock · 邮件 · Notion · LINE │  有界动作；不自带授权
└───────────────────────────────────────────────┘
```

*Translator's note: the diagram above is kept verbatim from the Chinese version. English rendering of its labels, top to bottom:*

```
┌───────────── UI projection layer (Desktop) ─────────────┐
│ Start a task · Task details · Connections and           │  Renders only kernel snapshots and
│ permissions (+ technical page)                          │  server-supplied choices
└──────────────────────────┬──────────────────────────────┘
                           │ REST / WS (read snapshot + events; re-read snapshot after disconnection)
┌──────────────────────────▼──────────────────────────────┐
│ Business orchestration layer (thin): workflow / step    │  Plan version, step dependencies,
│ state machine. Holds no approval/authorization          │  projection status
│ conclusions; only references kernel records             │
└──────────────────────────┬──────────────────────────────┘
                           │ Goes through kernel run / tool calls / approval gates
┌──────────────────────────▼──────────────────────────────┐
│ Control plane (kernel agent24d): run · approval ·       │  Sole authority: state, approval,
│ consent · source tags/LocalOnly · audit · workspace     │  authorization, audit
└──────────────────────────┬──────────────────────────────┘
                           │ Registered as tools (built-in or module), subject to the same gates
┌──────────────────────────▼──────────────────────────────┐
│ Connector layer: local materials · mock · email ·       │  Bounded actions; carry no
│ Notion · LINE                                           │  authorization of their own
└─────────────────────────────────────────────────────────┘
```

| Layer | Owns | **Does not own** |
|---|---|---|
| Control plane (kernel) | Run lifecycle, approval decisions, consent records and revocation, source tags and effective mode, audit chain, workspace permissions | Business semantics (what minutes are, action-item fields) |
| Business orchestration layer | Workflow definitions, plan versions, step dependencies, user-visible projection status (e.g. "partially complete", "result needs verification") | Any approval/authorization conclusion (only references kernel approval/consent IDs); any outbound execution channel |
| Connector layer | Protocol details with external services, how idempotency keys are applied, reconciliation queries | Authorization (credentials establish technical identity only and do not replace business authorization; report §5 "Four capabilities", first paragraph); self-reported risk does not lower the host risk level (ADR-K1-03 decision 1, risk row) |
| UI projection layer | Presentation, editing input, initiating requests | Authoritative state; must not maintain its own set of approval results (report §4, last sentence of the paragraph after the table); after disconnection must not infer success from local animation (report §3 Status row) |

**Where it lives** (pending ADR): the business orchestration layer can be (a) a thin crate compiled into the kernel, or (b) a supervised out-of-process module connected to the kernel through os-proto callbacks (report §4 table, row 2). This document leans toward starting with (a) — OA-M1 does not need independent releases, and (b) would introduce a prerequisite on the manifest/consent grant path (see §4.6). Either way, no side channel that bypasses model/tool controls may be opened.

**Naming**: backend IDs are kept distinct and explicitly related: `workflow_id`, `run_id`, `workspace_id`, `artifact_id` (report §1 "Distinct meanings of Workspace" paragraph 2); the UI uses "task", and file-location settings use "file workspace".

---

## 3. Minimum Data Contract

Report §4 "Proposed minimum data contract" gives four kinds of records; below are the field refinements and reuse relationships. All of this is **an OfficeAssistant contract still to be finalized**, not an API already supplied by Agent24 (last sentence of the same section of the report).

### 3.1 Records and relationships

```
workflow 1 ──< step >── 0..1 action 1 ──< attempt
   │                         │              │
   │                         ├── approval_ref（→ 内核 Approval.id / consent 记录）
   │                         └── receipt 0..1
   └──< artifact（版本链；action 的输入/输出引用 artifact 版本）
run：每个 step 关联一个或多个内核 run（新 attempt = 新 run，不复活终态 run）
```

*Translator's note: diagram kept verbatim. `approval_ref（→ 内核 Approval.id / consent 记录）` = approval_ref (→ kernel Approval.id / consent record); `artifact（版本链；action 的输入/输出引用 artifact 版本）` = artifact (version chain; action inputs/outputs reference artifact versions); last line = "run: each step is associated with one or more kernel runs (new attempt = new run; a terminal run is never revived)".*

| Record | Fields (suggested) | New / reused |
|---|---|---|
| **workflow** | `workflow_id`, `goal` (one sentence), `source_refs[]` (canonical source ID + revision/hash), `plan_version`, `steps[]` (dependencies), `run_ids[]`, `projection_status`, `created_by` (principal injected by the host), `created_at` | **New**. `source_refs` reuses the source-marking semantics of ADR-K1-02 §2.1 and `run_source_tags` |
| **action** | `action_id`, `step_id`, `tool`/`connector`, `account_ref` (no credentials), `exact_destination`, `effective_input_digest` (hash of the final payload), `payload_version`, `approval_ref`, `idempotency_key`, `attempt_count`, `risk_class` (computed by the host), `simulated: bool` | **New**; `approval_ref` points to the kernel `Approval` (`agent24-protocol/src/types.rs:514-540`) or a module approval/consent record |
| **attempt** | `attempt_id`, `action_id`, `run_id`, `tool_call_id`, `started_at`, `outcome_class` (closed set), `reconciled: bool` | **New**; `run_id + tool_call_id` is consistent with the ADR-K1-04 §2.1 audit linkage |
| **artifact** | `artifact_id`, `type`, `location` (controlled-storage reference), `version`, `content_hash`, `parent_version`, `edited_by_human: bool`, `evidence[]` (source_quote locations), `read_scope`/`write_scope` | **New**; storage location reuses the kernel's controlled storage (specific choice pending ADR, see §7) |
| **receipt** | `receipt_id`, `action_id`, `business_outcome` (closed set, see §4.3), `external_id`/`link`, `provider_accepted_at`, `verified_at`, `verification_method`, `compensable: bool` | **New** |

### 3.2 Separating sensitive body content from metadata

- Body content (transcripts, minutes, email bodies, page content) is kept only in controlled artifact storage; workflow/action/receipt records, events, and audit contain only necessary metadata (report §4, last paragraph of the data contract).
- Audit follows ADR-K1-04 §2.1: it must not record body content, credentials, full prompts, full tool arguments, or free-text reasons; outcomes use only closed-set result codes.
- "Binding a payload by hash" does not mean writing the body to logs (same paragraph of report §4).
- `account_ref` is an opaque reference; credentials are only in host-side secure storage (specific mechanism pending ADR; **not verified** whether existing credential storage can be reused — COMM's PasswordStore is Hyphae-specific, see `tasks.md` COMM-1b; its general applicability has not been evaluated).

### 3.3 Storage and migration

- Use SQLite (same database as the kernel or a separate database, pending ADR); new tables/projections need an ADR to decide the schema, migrations, restart recovery, and boundary with the main store (report §4, paragraph after the table).
- Note: kernel migrations run forward automatically at `Store::open` and **cannot be rolled back by version** (`K1-RELEASE-NOTES-20261016.md` lines 89–121). OfficeAssistant migration PRs fall into the "storage migration" category and require PR-Daemon APPROVE (`AGENTS.md`).

---

## 4. Key Mechanism Design

### 4.1 Authorization and approval binding

- Every action with external effects goes through the kernel approval gate; approval is bound to **action_id + payload_version + effective_input_digest + exact_destination**. All four are rechecked before execution; if any differs → the old approval is invalidated and the user is asked again (report §3 Approval row; §5 item 5).
- The existing `Approval` has `payload` (free-form Map), `available_decisions`, `standing_target`, and `expires_at`, but **no field binding a payload version/digest** (`types.rs:514-540`, read in this pass). An extension is needed: the approach is to compute the digest on the kernel side, store it in the approval record, and compare before dispatch; the business layer or UI **must not** self-report the digest. This is an external-protocol and approval-semantics change → PR-Daemon APPROVE.
- fail-closed: rejection, expiration, disconnection, daemon restart, and an unavailable approval channel are all equivalent to deny/abort (`types.rs` `Decision` comment: "Fail-closed: the implementation default (broken channel, cancelled run, daemon restart) is equivalent to abort/deny").
- When the effective risk of an outbound action is `External`, only `approve` / `approve_for_target` (when eligible) / `deny` / `abort` appear, never `approve_for_session` (L-APPR-4; `agent24-policy/src/lib.rs:56-68` `decisions_for`). For OA-M1/M2 it is **recommended not to offer** `approve_for_target`: the report requires individual confirmation in the first MVP (report §5 item 6).
- Plan approval is not unlimited authorization: after plan mode unlocks a set of tools, external actions still pass their own gates according to risk, destination, and actual payload (report §3 "Three points" item 1; for plan mode see the comment at `types.rs` lines 390–394).
- Do not conflate "gate" and "advise": an approval event does not mean execution has happened and cannot be used to show "sent" (report §3 item 2).

### 4.2 Idempotency and reconciliation

- **Business idempotency keys** are generated by the business layer for each action (e.g. a value derived from `workflow_id + action_id + payload_version`) and passed with the request to services that support idempotency; for services that do not, use "query before write + verify after write" instead.
- `tool_call_id` **must not** be used as a business idempotency key (explicitly prohibited by ADR-K1-04 §3 and §4 item 2). Searching the repository for `idempoten` only hits idempotency comments on the kernel's own operations; **there is no connector-level business idempotency mechanism** (search in this pass).
- Unknown outcome (timeout, disconnection, process crash) → status "result needs verification" → reconciliation query first (by idempotency key or external ID) → then decide "completed / needs retry / needs a human". Blind replay of the whole chain is prohibited (report §3 Retry row; §6 negative case row 3).
- Retries target only incomplete steps; receipts of successful steps are preserved (report §6 negative case row 4).

### 4.3 Layered receipts and "events are not proof of delivery"

`business_outcome` closed set (suggested): `prepared` (local artifact ready) → `approved` → `dispatched` (tool called) → `provider_accepted` (provider accepted, e.g. HTTP 2xx) → `verified` (independent query confirms remote state); side branches: `unknown_needs_verification`, `failed`, `cancelled_before_dispatch`, `simulated_accepted` (mock only; never enters `verified`).

- Approval, tool return, provider acceptance, and actual delivery are recorded separately (report §3 Receipt row). Email acceptance does not mean the recipient has received it; LINE's HTTP 200 does not mean final delivery (report §2 table; §5 LINE paragraph).
- `verified` must come from an independent query of external state, not from local logs or audit (L-CTX-4).
- WS events are only notifications: `seq` is **monotonic per connection**, not a global cursor across reconnects (`protocol/events.schema.json` describes a "per-connection monotonic seq"; report §5 item 8).

### 4.4 The difference between cancellation and compensation

- **Cancellation**: stops future work; stops in-flight calls at cancellable boundaries; lists external effects that already occurred. It does not claim to undo sent email or written pages (report §3 Cancel row).
- **Compensation**: a new action targeting an effect that already occurred (e.g. deleting a created page, sending a correction email); it is itself a new action that **requires separate authorization**, not part of cancellation (report §5 item 7).
- Run terminal states (`Completed/Failed/Cancelled`) have no outgoing transitions; "partially complete/outcome unknown/stopping" are workflow-layer projections; a retry creates a related new attempt/new run and does not revive a terminal run (report §3 item 3; for `RunStatus` see `types.rs:381-388`).

### 4.5 Disconnection and recovery

- After reconnecting, the client **first reads the authoritative snapshot** (workflow + action + receipt + kernel run/approval state), then subscribes to events; it must not resubmit based on an old confirmation card (report §6 negative case row 6).
- Daemon restart: kernel recovery is only permitted from the `awaiting_approval` state with unanswered tool calls (ADR-K1-02 §1 table, "sessions and restart recovery" row); the business layer sets every attempt that is `dispatched` but has no terminal state to `unknown_needs_verification` and runs reconciliation.
- On recovery, revalidate source policy, authorization validity period, and revocation status; if revalidation is impossible → no side effects are executed (ADR-K1-02 §2.2 item 3).

### 4.6 Govern LocalOnly and outbound paths separately

| Egress | Current state (after K1 merged) | OfficeAssistant requirement |
|---|---|---|
| Models (extraction/drafting) | The main path has been narrowed to LocalOnly; no local model means unavailable (K1 release notes lines 125–158) | OA-M1 uses only local models; if unavailable, fail explicitly and do not switch to the cloud (report §4 paragraph 3) |
| Connector outbound (email/Notion/LINE/HTTP/MCP) | ADR-K1-02 §2.4 item 2 lists these as egress gates; but the unified egress decision (6b.3) is not done, and `http_fetch`/`shell_exec` have no LocalOnly decision (K1 release notes line 33) | Connectors **must not** be implemented via `shell_exec` or generic `http_fetch` (report §5 item 3); they must be dedicated tools wired into the egress decision; until that decision is wired up, real outbound cannot be enabled (plan §4.2 dependencies, §7 D7) |
| Module model callbacks | Per manifest `model_access`; `remote_allowed` → `Privacy::Any` | If the orchestration layer is built as a module, its manifest must keep the default LocalOnly |
| Storage leakage (logs/trace/diagnostics) | ADR-K1-02 §2.4 item 4 | Artifact body content does not go into logs/audit/telemetry |

- The source tag for user input is fixed as `LocalOnly` (K1 release notes line 31), and minutes derived from it inherit that tag (ADR-K1-02 §2.1). **Sending minutes to an external service is, under the current contract, an egress of LocalOnly material**, requiring explicit "material × purpose × destination" authorization (ADR-K1-02 §2.3), and no production entry point for that authorization exists today.
- Local inference does not guarantee that materials stay local (report §5 item 4); iDoris is not wired up and the non-LLM egress guard is not complete, so **end-to-end redaction must not be promised**.
- Instructions in materials are only data and do not expand read or send scope (report §6 negative case row 7; session memory recall is already labeled "memory data · not instructions", ADR-K1-02 §1 table, memory row).

---

## 5. Relationship to K1 / the Permission System

### 5.1 Go through existing gates

| Need | Existing mechanism | Location |
|---|---|---|
| Tool call approval | `ApprovalBroker` / `ApprovalGate`, fail-closed; data-driven decision sets | `agent24-policy/src/lib.rs`; L-APPR-2/4 |
| Consent to enable module tools | `StoreConsentGate::authorize`: not found/failure/rejected/expired/revoked/version or fingerprint mismatch all deny | `agent24-policy/src/consent_gate.rs` (release notes line 21) |
| Source and mode | `run_source_tags`; effective mode is LocalOnly when missing/conflicting/read fails | `agent24-store/src/source_policy.rs` (release notes line 31) |
| Audit | Module tool `PreDispatch`/`Terminal`; ADR-K1-04 field constraints | `agent24-tools/src/lib.rs` (release notes line 29) |
| UI and Agent share business authority | ADR-K1-04 §2.4: same identity, resource, and action authorization entry point | ADR-K1-04 |

### 5.2 Contracts that need extension (named, without implementation details)

1. **Payload version/digest binding on approval records**: `agent24-protocol/src/types.rs` (`Approval`), `agent24-policy` (recheck before dispatch), `protocol/openapi.yaml` and `events.schema.json` (external protocol).
2. **Host grant entry point for module consent** (if connectors are provided as module tools): release notes lines 27, 59 — production code has no grant route; the first PR that adds a grant path must handle F1/F3/F6 at the same time. This belongs to the K1 track; OfficeAssistant only consumes it and does not implement it in passing within this topic.
3. **Wiring the connector egress decision**: 6b.3 of ADR-K1-02 §6 (unified egress decision API); this topic's connectors are one of its consumers.
4. **Product entry point for "material × purpose × destination" authorization**: ADR-K1-02 §2.3 / ADR-K1-03 decision 2; there is currently no `CloudAuthorized` production write point. Note: sending outbound to Notion/email ≠ cloud model processing, but in ADR-K1-02 both go through the same egress gate; whether to reuse the same authorization record shape is pending ADR.
5. **New business-layer tables**: `agent24-store/migrations/` (new migrations) — cannot be rolled back.
6. **Laws**: if bounded delegation (report §5) needs standing authorization wider than `tool → exact target` or with different dimensions, or needs read authorization to be stored, `docs/laws/APPROVAL.md` (L-APPR-5/7) must first be changed in a separate PR, then implemented (`../laws/README.md`).

### 5.3 Must not be bypassed

- "The user clicked a UI button" must not be used to bypass resource/source/organization policies (ADR-K1-04 §2.4).
- `os.json enabled=true`, Creative capability, and module signatures cannot replace module authorization or LocalOnly (ADR-K1-03 counterexample 8).
- Skill text, MCP descriptions, and external documents cannot expand their own permissions (report §5 item 1).
- The default legacy bearer has full authority, route-level resource authorization is not complete, and Desktop general-purpose proxy isolation is not done (`COMPONENT-ROADMAP.md` C5 current state) — so the first version only validates trusted local materials and mocks, and is not opened to untrusted plugins or organizational sharing (report §5 item 2).

---

## 6. Observability and Testing

### 6.1 Backend and frontend assertions for the seven semantics

| Semantic | Backend assertion (automated) | Frontend assertion (component/E2E) |
|---|---|---|
| Plan | Plan version fixed; new scope/destination/material changes → plan version increments and related approvals are invalidated | Shows materials to be read, artifacts to be produced, which steps write/send |
| Action | Unique `action_id` per step; attempts linked to runs; natural language is not an execution credential | Confirmation card shows account, destination, diff, affected scope |
| Approval | Approval bound to payload digest; dispatch rejected after payload change; expiration/rejection/disconnection → no dispatch | Renders only the server-supplied `available_decisions`; no `approve_for_session` |
| Cancel | No new dispatch after cancellation; effects that already occurred are listed in receipts | Shows "stopping → stopped" and the list of completed actions |
| Retry | Unknown outcome → reconcile first; successful steps are not redone; the same idempotency key produces no second external effect | Shows steps that can be safely retried |
| Status | Projection status computed from the kernel snapshot; reconnect reads the snapshot | No optimistic success shown after disconnection |
| Receipt | `provider_accepted` separate from `verified`; mock is never `verified` | "Simulated" and "result needs verification" labels are visible |

### 6.2 Automating the negative-case matrix

- The 14 rows of the report's §6 negative-case matrix are mapped row by row to test case IDs (`OA-NEG-01..14`); each row is first shown to fail on the "old behavior/not implemented", then implemented, then shown to pass (following the ADR-K1-03/04 counterexample method).
- Automatable levels: Rust unit/integration tests (approval binding, idempotency, recovery); fault injection in the mock connector (timeout with remote success, partial failure, token expiration); desktop component tests (labels, button sets).
- Real-connector negative cases (OA-M2): mostly covered with record-replay; on the real account run only "one write + one reconciliation + one revocation", executed by jason or with jason present.
- Extraction quality: a script comparing item by item against the gold standard, outputting counts and lists for four categories — omissions, unsupported commitments, "needs confirmation" labeling errors, provenance errors; safety/correctness counterexamples are reported separately and not averaged into a score (report §6, paragraph after the negative cases).

### 6.3 Distinguishing demonstration from real

- The `simulated` field of an action record is set by the host according to the connector type and cannot be rewritten by the business layer or the model; it is always shown in the UI and receipts.
- The demonstration package explicitly labels samples as synthetic and contains no real customer material, real credentials, or unauthorized accounts (report §6 R0).
- A local demonstration must not be described as having completed a Notion or email delivery loop (report "How the original page plan…" paragraph 4, last sentence).

---

## 7. Technical Risks and Alternatives

| Risk / choice | What happens if not done this way | Fallback / alternative path |
|---|---|---|
| Keep the business orchestration layer thin, authority stays in the kernel | Frontend or business layer each maintain approval conclusions, producing two authorities; inconsistency after disconnection/restart; audit cannot be matched | If the thin layer is insufficient, evaluate an out-of-process module (via os-proto); LangGraph only as a candidate after a gap is demonstrated (report §4 table) |
| Approval bound to payload digest | The user approves A and B is actually sent (changed recipients/attachments) without anyone noticing | Before the kernel extension, OA-M1 does "compare before dispatch + reject on mismatch" in the business layer, but marks it as transitional and does not publicly claim a mechanism guarantee |
| Business idempotency key + query before acting | Retries after timeout produce duplicate emails/duplicate pages | When the service does not support idempotency: query before write + verify after write; if still undeterminable → stop at "result needs verification" and hand to a human |
| Connectors do not go through `shell_exec`/generic `http_fetch` | Bypasses model and permission boundaries; a fixed cwd is not an OS sandbox (report §5 item 3) | Only dedicated tools; real outbound is not enabled before the egress decision is wired up |
| Local models only | Sensitive meeting content leaks out; violates K1 LocalOnly | When extraction quality is insufficient: narrow the task (extract action items only), more "needs confirmation", raise the local model tier; no fallback to the cloud |
| Separate body content from metadata | Body content enters the non-deletable hash-chained audit (ADR-K1-04 §1 table, "arbitrary audit details" row) | Audit uses only closed-set fields; body content only in controlled artifact storage |
| Read authorization not persisted (L-APPR-7) | If persisted, the authorization surface spreads from "explainable actions" to "exploration" | Disclose read scope for each task; if the product truly needs persistent read authorization, change the law separately first and add the mechanism |
| Individual confirmation (no bounded delegation in the first version) | If delegation arrives too early, a single agreement becomes an open proxy | M3 first produces design + counterexamples + law proposals, implementation only after acceptance |
| Migrations cannot be rolled back | After release, cannot downgrade to the old binary | Migration PRs small and independent; release notes spell out backup steps (cf. K1 release notes line 261) |
| Kernel prerequisites not ready (grant entry point, egress decision) | OA-M2 cannot complete a real write in a compliant way | OA-M2 stops at "read-only + remote-draft review + mock write"; real write waits for the prerequisites (plan §7 D7) |
| Workspace name confusion | Permissions, lifecycles, and state machines mistaken for shared | IDs and UI copy kept strictly separate (report §1) |

---

## 8. Decisions Pending ADR

1. Location of the business orchestration layer (kernel crate vs out-of-process module).
2. Schema, migrations, boundary with the main store, and restart recovery semantics of the four kinds of records.
3. Payload digest binding on approval records (protocol change).
4. Connector integration method and credential custody (OA-M2, depending on the connector selected by the author).
5. Whether outbound authorization records reuse the "material × purpose × destination" shape of ADR-K1-02.
6. Authorization model for bounded delegation (OA-M3; must go through the law process first).

## 9. Not Verified in This Document

- Whether the existing credential storage mechanism can be generalized to email/Notion credentials.
- Thai coverage of the desktop i18n (only Thai-related entries were found by search; not checked page by page).
- The report's facts about the Components repository (not in this repository).
- API behavior of external platforms (Notion, LINE, email services) follows the official documentation cited by the report; this document did not recheck it.
