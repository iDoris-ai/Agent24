# Documenting — D1-01 Baseline

> **Issue:** #700 (D1-01) · **Design baseline:** [README.md](README.md) (PR #685)  
> **Pinned Agent24 SHA:** `4fe8c2642c7c853a5d420eb5571f257fe2f1f60d` (`main`, 2026-10-07)  
> **Date:** 2026-10-07 · **Author:** David Xu  
> **Scope:** facts only. No product code was changed.

This document records what Agent24 provides **today** that the DocumentService contract (D1-02, README §10) depends on. Every claim cites a file and line at the pinned SHA, or a command that was run.

How to read it:

- Paths are relative to the repository root.
- ✅ = implemented, 🟡 = partially implemented, 📐 = designed only.
- §11 lists the gaps against the README contract and the questions the D1-02 ADR must answer.

> **Correction (2026-10-07, supplementary architecture review on PR #685, H1).**
>
> - This document's *recommendation* that document operations fit best as built-in kernel `Tool`s (§1 item 2, §5, §11 first row) is **superseded**.
> - Documenting owns identity, revisions, storage and a page. Per `docs/ARCHITECTURE-LAYERS.md` §3.2 the review recommends treating it as a **first-party domain OS**, not an L3 kernel capability. This is the working direction, to be ratified in #701. See README §2.1 and §10.3.
> - The rows here that place Documenting's tables in the kernel store (§11 "Single commit point", §12 Q4) are superseded in the same way: a domain OS keeps its own storage under `~/.agent24/os/<name>/` (ADR-029).
> - The **facts** recorded here still hold, and they matter for that choice:
>   - a domain OS cannot contribute agent tools today;
>   - MCP tools are always `External`;
>   - the tool pipeline is frozen at startup.
> - These facts are why the agent-exposure mechanism (kernel proxy tools, MCP, or a new `Capability`) must be decided in the D1-02 ADR (#701).
> - This file stays pinned to `4fe8c26`. It is not otherwise rewritten.

## 1. Summary

1. **Build and run.**
   - The Rust daemon and CLI build cleanly.
   - The daemon starts, reports status and stops cleanly in an isolated `HOME`.
   - Desktop typecheck passes.
   - One desktop test fails only on Node 26. CI uses Node 22 (§2).
2. ~~**A first-party document tool fits best as a built-in Rust `Tool`.**~~ *(Recommendation superseded, see the correction above.)* The facts stand: MCP tools are always classed `External`. Domain modules and TS capability modules cannot contribute agent tools at all (§5).
3. **Tools are reachable only through an agent run.**
   - There is no endpoint that invokes a tool directly.
   - A tool returns a plain `String`.
   - The desktop UI therefore cannot call "the same operations" without new API work (§3, §10).
4. **Risk classes are fixed at four:** `Read`, `WriteLocal`, `Exec`, `External`. Any new tool name falls into the generic `module` approval kind (§4).
5. **Agent24 is a single local user today.**
   - No principal reaches sessions, runs or tools.
   - Capability-mode principals are host-chosen, in-memory and only partly enforced (§6).
6. **The kernel has no document storage, job system or client idempotency on runs or tools.**
   - Reusable patterns exist: a version compare-and-swap, revision counters, a deterministic dedup id, and UNIQUE request keys (§7, §8).
   - None of them is wired to tools.
7. **The file working directory (`a24.workspace.v1`) is scratch only.**
   - It has no product creation route.
   - It is Unix-only and reachable per run.
   - It is not a home for saved revisions, which matches README §22.1 (§7).
8. **No context-assembly hook exists, and agent runs never use LocalOnly.** Document content sent to a model today goes to whatever provider the router picks (§9).
9. **The desktop app adds a native page with one routing file**, but several prerequisites are missing:
   - no document API;
   - no chat-to-page navigation;
   - no binary transfer through the JSON-only backend proxy;
   - no viewer or editor dependencies;
   - a CSP that blocks `blob:` URLs (§10).

## 2. Build and run

Machine: macOS (Darwin 25.6.0, Apple Silicon), cargo 1.98.1, Node 26.10.0, pnpm 9.12.0.

| Step | Command | Result |
|---|---|---|
| Rust build | `cd rust && cargo build -p agent24d -p agent24-cli` | ✅ finished in 40.6 s (dev profile) |
| Daemon start | `agent24 daemon start` (with `HOME` set to a temporary directory) | ✅ `daemon started (pid …, port 59456)` |
| Daemon status | `agent24 daemon status` | ✅ `running · … · backend rust · v0.5.1` |
| Models | `agent24 models` | ⚠️ `(no models — is a local LLM runtime running?)`. No oMLX (8088) or Ollama (11434) on this machine, so chat was not exercised. |
| Daemon stop | `agent24 daemon stop` | ✅ `stopped`, then `not running` |
| JS install | `pnpm install --frozen-lockfile` | ✅ |
| Node daemon build | `pnpm --filter @agent24/node-daemon build` | ✅ |
| Desktop typecheck | `pnpm --filter @agent24/desktop typecheck` | ✅ |
| Desktop tests | `pnpm --filter @agent24/desktop test` | ⚠️ 383/384 passed |

The one failing desktop test is `Settings.test.tsx › setCategory … warms up`. It fails with `Cannot read properties of undefined (reading 'setItem')`:

- **Cause:** Node 26's built-in Web Storage global shadows the jsdom `localStorage`.
- **Evidence:** with `NODE_OPTIONS=--no-experimental-webstorage`, the file passes 9/9.
- **Conclusion:** it is an environment issue. CI pins Node 22 (`.github/workflows/ci.yml:32`). Local development should use Node 22, or pass that flag.

The daemon's state directory after the first start contained:

- `agent24.db` and `memory.db` (SQLite, WAL);
- `workspace/` and `workspace-roots/`;
- `attach/`, `packages/` and `run/`;
- `daemon.json` and the lock files.

## 3. Tools: definition, registration, dispatch

**The `Tool` trait** ✅ is at `rust/crates/agent24-tools/src/lib.rs:265-294`. It has these members:

- `info() -> ToolInfo`;
- `parameters() -> Value`, a JSON Schema;
- `target_arg()`;
- `timeout()`, 30 s by default;
- `call(ctx, input, cancel) -> Result<String, ToolError>`.

Two consequences:

- **Results are a plain `String`.**
- **Inputs are not validated against the schema.** Each tool parses its own arguments.

Related types:

- **`ToolInfo`** (`rust/crates/agent24-protocol/src/types.rs:1062-1098`) holds `name`, `source` (`builtin | mcp | module`), `description`, `risk_class` and a derived `requires_approval`.
- **`ToolError`** (`agent24-tools/src/lib.rs:152-170`) has the variants `Invalid | Denied | Failed | Timeout | Cancelled | AbortRun`. It is flattened to text before it reaches the model.

**Built-in tools:**

| Tool | Location | Risk class |
|---|---|---|
| `fs_read` | `agent24-tools/src/local.rs:127-203` | `Read` |
| `fs_write` | `agent24-tools/src/local.rs:207-280` | `WriteLocal` |
| `shell_exec` | `agent24-tools/src/local.rs:284` | `Exec` |
| `http_fetch` | `agent24-tools/src/net.rs:27` | `Read` |

**Registration** ✅ happens in code at daemon startup:

- `rust/apps/agent24d/src/server.rs:1456` calls `ToolRegistry::builtin(…)`.
- `:1460-1468` add `explore` and `self_wake`.
- `:1469-1487` add MCP tools from `~/.agent24/mcp.json`.
- `:573-580` wrap the registry with risk overrides and `BrokerGate`.
- `ToolRegistry::with()` registers a tool and allowlists it in one step (`agent24-tools/src/lib.rs:347-353`).

There is no configuration-driven allowlist and no per-tool manifest. The tool set is frozen when the daemon starts.

**Model exposure** ✅:

- `adverts()` (`agent24-tools/src/lib.rs:470-487`) offers tools that are allowlisted and are either `Read` or behind an interactive gate.
- `plan_adverts()` (`:494-499`) offers `Read` tools only.
- The agent loop maps them to function specs at `rust/crates/agent24-agent/src/lib.rs:1681-1716`.

**The fixed pipeline** ✅ is `ToolRegistry::dispatch` at `agent24-tools/src/lib.rs:521-587`. It runs in this order:

1. trim the name and look the tool up;
2. check the allowlist;
3. compute the effective risk class and call the approval gate;
4. execute under a timeout that is raced against cancellation (`run_budgeted`, `:671-684`).

`execute_preapproved` (`:599-619`) skips the gate when a run resumes after a restart.

**There is no direct invoke endpoint** ✅. The daemon router (`server.rs:1019-1152`) exposes only:

- `GET /api/v1/tools` (`server.rs:1039`), which lists tools;
- tool overrides and standing grants;
- approvals and runs.

`/api/v1/chat` offers no tools (`rust/apps/agent24d/src/routes.rs:486-487`). The only way a UI can execute a tool is to start a run.

## 4. Risk and approval

**`RiskClass`** ✅ is at `agent24-protocol/src/types.rs:1003-1053`:

| Class | Approval | Standing grant |
|---|---|---|
| `Read` | none | – |
| `WriteLocal` | required | no |
| `Exec` | required, every time | no |
| `External` | required | yes (the only class eligible) |

Users may tighten any tool's class. They may relax a class only for tools whose `source` is not `builtin` (`agent24-tools/src/lib.rs:415-434`).

**Approval kind** ✅ is chosen by tool name in `BrokerGate::check` (`rust/crates/agent24-policy/src/lib.rs:897-902`):

- `shell_exec` → `exec`;
- `fs_write` → `fs_write`;
- `http_fetch` → `network`;
- anything else → `module`.

**Decisions** ✅ are built by `decisions_for` (`agent24-policy/src/lib.rs:54-66`):

- `approve`, `deny` and `abort` are always offered;
- non-`External` classes add `approve_for_session`;
- `External` tools with a target add `approve_for_target`.

**Broker** ✅ (`agent24-policy/src/lib.rs:243+`) checks, in order:

1. a standing grant;
2. a session grant;
3. Guardian;
4. a human, which emits `approval.required`.

It fails closed:

- A timeout counts as a deny. The default is 300 s, set by `A24_APPROVAL_TIMEOUT_SECS` (`server.rs:561-564`).
- A cancelled run resolves as `Aborted`.
- A duplicate decision returns 409 (`agent24-policy/src/lib.rs:769-821`, `rust/apps/agent24d/src/approvals.rs:116-117`).

**Guardian** ✅ is opt-in (`A24_GUARDIAN=1`, `server.rs:413-438`):

- It auto-approves only when the assessment is an explicit `Low`.
- Approval kinds in `A24_GUARDIAN_ALWAYS_REVIEW` (default `exec`) always go to a human (`agent24-policy/src/guardian.rs:150`).
- The assessment uses a local-only model.

## 5. Extension mechanisms and where document tools fit

| Mechanism | Status | Can it add agent tools? | Fit for document operations |
|---|---|---|---|
| Built-in `Tool` (`source "builtin"`) | ✅ | yes | First-party, same pipeline, risk class cannot be relaxed by users. *(Originally marked "best fit" for all of Documenting; superseded by H1. It remains one candidate for exposing a domain OS's operations to the agent, as thin proxy tools.)* |
| MCP client (`rust/crates/agent24-mcp/src/lib.rs`, tools named `mcp_{server}_{tool}`) | ✅ | yes | Poor. Always `External` (`:260-275`), with a 60 s timeout, and it labels first-party code as third party. |
| MCP server (`agent24 mcp`) | ✅ | n/a | Exposes only `agent24_run` / `list_tools` / `list_runs` to outside clients. |
| DomainModule / domain OS (ADR-029, `rust/crates/agent24-domain/src/lib.rs:1316`) | ✅ | **no** | Has its own routes and store, but there is no tool capability (`:144-159`). |
| TS CapabilityModule (`packages/node-daemon/src/capabilities/base.ts:27-30`) | ✅ | **no** | Adds HTTP routes only, in the Node reference backend. |
| Capability Provider / node-host (ADR-026 §3) | 📐 | – | Not implemented. |

ADR-026 §6.5 #6 requires that built-in, MCP and skill tools share one trait and one pipeline.

## 6. Caller context, identity and run lifecycle

**`ToolContext`** ✅ (`agent24-tools/src/lib.rs:31-44`) holds `run_id`, `session_id`, `schedule_id`, `tool_call_id` and a private workspace authority (Legacy or Bound).

- It carries **no principal or user id.**
- The agent loop builds it at `agent24-agent/src/lib.rs:429-467`.
- ADR-002 §6 plans to add `workspace_id` 📐.

**Authentication modes** (`server.rs:912-974`):

- ✅ `LegacySingleToken` is the default (`:624`): one all-powerful bearer token.
- 🟡 `Capabilities` mode (`:666`) issues `ProductHost` / `CreativeRuntime` tokens whose claims carry `principal_id` (`rust/apps/agent24d/src/capabilities/types.rs:95-107`). However:
  - the host chooses the principal at mint time;
  - the store is in memory;
  - claims never reach runs or tools;
  - resource-level authorization is 📐 (`docs/ARCHITECTURE-LAYERS.md:153-154`).
- **No multi-user model in the kernel:** sessions and runs have no principal column (`rust/crates/agent24-store/migrations/0001_initial.sql:4-24`).

**Run lifecycle** ✅:

- **Create:** `POST /api/v1/runs`, handled by `start_run` (`agent24-agent/src/lib.rs:641-721`).
- **Cancel:** idempotent (`:725-760`). The cancellation token reaches tools through `dispatch`.
- **Statuses:** a closed set with a transition matrix (`rust/crates/agent24-core/src/transitions.rs:25-38`).
- **Retries:** none for runs or tools. Resume after a crash covers only runs parked on an approval (`agent24-agent/src/resume.rs`).

**Per tool call:**

- ✅ Each call gets a `tool_calls` row with the input JSON, the status and a 500-byte output summary.
- ✅ `tool.started` / `tool.completed` events are emitted.
- The **hash-chained audit log** (`rust/crates/agent24-store/src/audit.rs:326-340`) records:
  - tool **denials** (`agent24-agent/src/lib.rs:1616-1630`);
  - approval resolutions;
  - overrides;
  - workspace retention.
- Successful mutations are **not** audited.

## 7. Storage

**The file working directory `a24.workspace.v1`** (ADR-002) is constrained by the schema (`rust/crates/agent24-store/migrations/0009_workspaces.sql`) and by code (`rust/crates/agent24-store/src/workspaces.rs:5-6`):

- `orchestrator_scratch`;
- `writeback=external`;
- `serial`;
- TTL of 24 h by default, 7 days at most.

Its state:

- 🟡 The daemon composes `WorkspaceService` on Unix (`server.rs:1521-1532`). Every non-Unix path returns `UnsupportedPlatform` (`rust/crates/agent24-workspace/src/service.rs:232-251`).
- 📐 **There is no product route to create a workspace.** `ScratchCreateSpec` is an empty placeholder (`agent24-workspace/src/spec.rs`).
- ✅ A run bound to a workspace reads and writes through `WorkspaceRunAuthority` (`agent24-workspace/src/service.rs:264-277`).
- Access is per run and per lease. A later reader can only be another run on the same workspace; a host lease is 📐.
- Legacy runs use `~/.agent24/workspace` (`server.rs:1436-1440`).

**Persistent storage** ✅ consists of:

- `~/.agent24/agent24.db`;
- `~/.agent24/memory.db`;
- `~/.agent24/os/<module>/` data directories.

The state directory is resolved by `state_dir()` (`rust/crates/agent24-protocol/src/state_file.rs:126`). **There is no blob, file or artifact area for documents or exports.**

**Migrations** ✅:

- The store applies `sqlx::migrate!` at open (`agent24-store/src/lib.rs:125-139`), with numbered, append-only files `0001`–`0013`.
- Memory has its own series `0001`–`0015`.
- A new domain adds the next numbered file.

**`ArtifactStore`** in agent24-memory (`rust/crates/agent24-memory/src/artifact.rs:100-112`) is ✅ but not wired:

- It is a versioned markdown store keyed by `(owner, path)`.
- `cas_write(a, expect_version)` returns `Conflict` on a stale version (`:109`, `:180`).
- It keeps full version history.
- Nothing in the daemon or tools uses it.

## 8. Errors, concurrency, jobs and idempotency

**HTTP error envelope** ✅: `{error:{code,message,hint?,details?}}` (`rust/crates/agent24-domain/src/http.rs:35-66`).

- `StoreError::Conflict` and invalid run transitions map to 409 `conflict` (`rust/apps/agent24d/src/runs.rs:85-100`).
- Every workspace-admission denial, including busy, collapses into that generic 409 (`runs.rs:49-56`). The more specific codes are 📐 (ADR-002 §10).

**Optimistic concurrency patterns** that exist ✅:

- `Workspace.revision` compare-and-swap. It is server-internal: the expected revision is read in the same transaction (`agent24-store/src/workspace_lifecycle.rs:64-75`).
- Schedule `revision = revision + 1 WHERE revision = ?` (`agent24-store/src/module_schedules.rs:273-274`).
- `ArtifactCas.expect_version` (§7).

Nothing checks a **client-supplied** base revision. `provenance_base_revision` is stored but never compared.

**Jobs** ❌: there is no job abstraction with status, progress, cancel and resume. Runs are the only asynchronous unit, and they have no progress field. `agent24-worker` is an unused client contract for a Python ML worker.

**Idempotency patterns to copy** ✅:

- Scheduler `FireId`: a SHA-256 of `(trigger, schedule_id, scheduled_for)` (`rust/crates/agent24-scheduler/src/fire.rs:13-45`), inserted with `ON CONFLICT (fire_id) DO NOTHING`.
- `module_approvals` UNIQUE `(module, request_id, kind)` (`agent24-store/migrations/0005_module_approvals.sql`).
- Memory event ids.

`RunCreate` has **no** idempotency key (`agent24-protocol/src/types.rs:411-424`).

## 9. Knowledge context and privacy

- **Run context today** ✅ is only the prior session history plus the prompt (`agent24-agent/src/lib.rs:507-534`).
- **Retrieval is not consumed.** `KnowledgeBase`, `FtsRetriever` and `VectorRetriever` exist in agent24-memory but have no consumers (`docs/ARCHITECTURE-LAYERS.md:136`).
- **There is no context-policy hook.** README §9.2 and T006 §7.2 require one.
- **LocalOnly** ✅ exists as `Privacy::{Any, LocalOnly}` (`rust/crates/agent24-models/src/router.rs:57-63`) and fails closed. However:
  - agent runs always use `TaskProfile::default()`, which is `Any` (`agent24-agent/src/lib.rs:1283`);
  - only Guardian and module model callbacks set LocalOnly.

## 10. Desktop app

**Structure** ✅:

- The main process is `apps/desktop/src/main/main.ts`. The window has `contextIsolation: true`, but `sandbox: false` (`:162`).
- The preload (`apps/desktop/src/main/preload.ts:28-101`) exposes a typed `window.agent24`.
- The renderer is React built with Vite.
- The Rust backend is the default (`apps/desktop/src/main/backend-manager.ts:38-40`).

**Backend access** ✅: `window.agent24.backendProxy({method, path, body})` forwards to the daemon with the bearer token (`apps/desktop/src/main/ipc/index.ts:71-114`). Its limits:

- It always sends and parses JSON (`:73`, `:102`), so **it cannot carry binary files, multipart uploads or streams.**
- It forwards **any** path that starts with `/` (`:116-131`). Tightening this is planned as F11.0 (`docs/agent/PLAN-OD-NEXT.md:37`).
- `@agent24/api-client` is not used by the desktop app.

**Navigation** ✅: there is no router library. To add a page, edit `apps/desktop/src/renderer/App.tsx`:

- the `BuiltinPage` union (`:31-43`);
- `BUILTIN_NAV` (`:45-57`);
- `BUILTIN_TITLES` (`:59-64`);
- the render switch (`:288-298`).

Two constraints come with this:

- Pages unmount when you leave them.
- `setPage` is not passed down, so **no page can navigate to another page today.**

A native React page needs none of the Creative / `WebContentsView` machinery.

**Chat** ✅:

- One blocking `POST /api/v1/chat` that renders plain text (`apps/desktop/src/renderer/pages/Chat.tsx:48-54`, `:115`).
- No tool calls, attachments, links or markdown.
- Run, tool and approval events are not forwarded to the renderer (`apps/desktop/src/main/agentear-events.ts:54`, `:78-85`).
- Approvals live on a separate page that polls every 2 s.

**Files** ❌:

- No file picker, dialog, drag and drop, or preview of any kind.
- Runtime dependencies are only `react`, `react-dom`, `ws` and `@agent24/node-daemon`. There is no PDF, markdown, rich-text or diff library.
- The CSP is `img-src 'self' data:` (`apps/desktop/src/main/main.ts:252`) and blocks `blob:` images and workers, which pdf.js needs.

**Tests** ✅:

- Vitest 4 with jsdom and Testing Library.
- Coverage thresholds of 80% lines and functions, 70% branches.
- `window.agent24` is mocked with `Object.defineProperty`.
- There is no end-to-end harness.

## 11. Gaps against the contract (inputs to D1-02)

| README requirement | Today | Gap / question for the ADR |
|---|---|---|
| Document operations as default-registered kernel capability (§10.3) | Built-in `Tool` + `with()` at startup | *(Superseded by H1: Documenting is a domain OS.)* Still relevant if the ADR chooses kernel proxy tools for agent exposure: they would register near `server.rs:1456`, and there is no switch-off mechanism today. |
| UI and agent call the **same** operations; UI edits share one business entry (§2.2, §10.3) | Tools only run inside agent runs; no invoke endpoint | **Main architectural question.** Proposal to evaluate: a shared Rust `DocumentService` crate called by (a) thin `Tool` adapters for the agent and (b) new typed REST routes for the UI, both entering the same validation/commit/audit path. The ADR must decide how UI-initiated mutations get approval and a context without a run. |
| Typed results and errors (§10.2) | `Tool::call` returns `String`; `ToolError` flattened to text | Tools: JSON-in-string with a versioned schema, or change the trait. REST: typed responses in `protocol/openapi.yaml`. |
| Business risk labels map to kernel categories (§10.2) | Four classes; name-based approval kind, default `module` | Proposed mapping: read/find/render/list → `Read`; draft/propose/review → ADR question (`Read`-like vs `WriteLocal`); `revision.commit`/`restore`/`export`/`package` → `WriteLocal`; delivery execution stays with connectors (`External`). Add a `document` approval kind in `BrokerGate::check`. |
| Identity from trusted host context (§10.2) | Single local user; no principal in `ToolContext` | Record as single-user for D1; define where principal will come from when ADR-005 lands; never accept it from model args. |
| `base_revision` + `revision_conflict` (§10.2) | No client-supplied CAS anywhere | New; model on `ArtifactCas.expect_version` / schedule revision. |
| Single commit point + idempotency (§10.1) | No revision/package tables; no idempotency on runs/tools | *(Superseded by H1: storage belongs to the Documenting domain OS's own data dir, ADR-029.)* New store migrations (next after `0013`) or a separate documents DB; idempotency via UNIQUE key or `FireId`-style hash + `ON CONFLICT DO NOTHING`. |
| Saved revisions not only in scratch (§22.1) | Scratch is Unix-only, per run, no product route; no blob store | Need a persistent document/blob area under the state dir (or the source-of-truth decided in #690). |
| Jobs for OCR/convert/export (§10.2) | No job abstraction | New job table with status/progress/cancel/resume, or reuse runs; ADR decision. |
| Audit of commits/exports | Only denials/approvals audited | Explicit `append_audit` on commit, restore, export, package, delivery.prepare. |
| Knowledge context hook, LocalOnly (§9.2, §9.3) | No hook; runs always `Privacy::Any` | Depends on T006 owner (#704, #713). Until LocalOnly per run exists, D1 must not send restricted document content to remote models by default. |
| Native page + chat handoff (§2.2) | Page = small `App.tsx` change; no cross-page navigation; chat plain text; tool events not in renderer | App-level navigation with parameters; structured file references in chat or forwarded tool events. |
| Import/export of real files (§2.2, §18) | JSON-only proxy, no dialogs, CSP blocks `blob:` | Typed IPC with native file dialogs in main; CSP review for viewers; viewer/editor dependency choice follows #692/#696. |

## 12. Open questions for review

1. Is a shared `DocumentService` crate with two thin front doors (agent `Tool` adapters, UI REST routes) acceptable, or should the UI go through runs?
2. How does a mutation the user starts directly in the UI get approval and audit without an agent run? Is the user's own click consent enough for `WriteLocal`, with audit only?
3. Should draft/propose operations be `Read` (no externally visible side effect) or `WriteLocal`?
4. *(Reframed by H1: as a domain OS, storage lives in its own data dir; the open question is its layout and the original/revision authority, #690.)* Where does document storage live: a new store migration series in `agent24.db`, or a separate `documents.db` plus a blob directory? This depends on #690.
5. What is the minimum privacy rule for D1, given that runs are always `Privacy::Any`? For example: document tools are offered only when the selected provider is local, until per-run LocalOnly lands.
