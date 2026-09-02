# Changelog

All notable changes to Agent24 are documented here. This project adheres to
[Semantic Versioning](https://semver.org/).

## [0.3.0] — 2026-09-02

M-E：领域 OS 成为一等公民，M-D 记忆底座重做，Nostr 渠道收官。
自 0.2.1 起 72 个合并。

### 领域 OS（可插拔架构，ADR-029）

- `DomainModule` + `KernelCtx` 契约 crate；内核不再按名字认识 Sin90（#127）
- 内核侧挂载器 + Sin90 成为第一个 `DomainModule`：自己的 DB、自己的路由
  命名空间、自己的 event module 名（#131 #132）
- 配置驱动的领域 OS 注册表 + `agent24 os` CLI（daemon 拥有注册表）（#133 #134）
- `domain-os.yml` 清单带 `deny_unknown_fields` —— 拼错字段名报错，而不是
  静默的空能力集

### 记忆（M-D 重做：可进化 / 可替换 / 可组合）

- MD-1 Condenser 缝 + 崩溃重放，签名已冻结（#113 #116）
- MD-2 EventStore（情节权威）+ ArtifactStore（markdown-CAS）+ 双谱系对账，
  checksum 移动检测、**无静默删**（#114 #115）
- MD-3 AssertionStore 双时相（矛盾=新版本非删）+ FTS5 Retriever，owner 隔离
- MD-4 MemoryWriter 写门：WebFetch/Unknown 默认不落持久，投毒语料测试
- MD-5 Consolidator：幂等 + **增量 == 全量重跑**
- MD-6 向量检索机制 + 换模型 reindex 状态机 + FTS 兜底
- MD-7 知识层：层级合并 + 触发注入 + **审核门控 auto-memory inbox**
- MD-8 长任务符号轨迹：压缩率 >99% 且 **100% 可恢复**
- **F1 `ScopedMemory`**：两个领域 OS 不再共享记忆底座（#139，六轮对抗复审）
- **F8 所有权改为 (org, space)**，org 成为一等实体；分区目录 + v1→v2 re-key，
  全程一个事务（#140，六轮对抗复审）
- F5 两处排序 tie-breaker + 一条比字面更弱的 CHECK 约束（#138）

### 渠道

- **F4 Nostr 收官**（#85–#95）：出站 register/say/search + 入站 gated +
  npub 白名单；与 agent-speaker 双向真联调；strfry 真 NIP-33 relay 覆盖定论
- **两条会让 7×24 静默失效的缺陷**（#142）：
  - Nostr 桥的 `execFile` 无 deadline —— 子进程挂起会让入站轮询循环**永久停摆**
    （`tick()` 是串行自调度，promise 不 settle 就没有下一轮）。加 60s deadline
    + SIGKILL。**注意这只关上了「子进程挂起」那一支，不含 relay 静默，见下方
    已知缺口 FU-32。**
  - 微信会话映射改为原子写：temp → `fsync` → `rename` → `fsync` 父目录，
    外加一代 `.bak` 回退与损坏时的回退读取。**限定（FU-31）**：macOS 的
    `fsync` 不保证驱动器刷新自身写缓存（那需要 `F_FULLFSYNC`，Node 不暴露），
    所以这关上的是 page cache 那个窗口，**不是「抗断电」**

### 生态

- 模块发现服务 + 浏览过滤（#90 #91 #94）

### 文档

- ADR-030 + SPEC-ORG-SPACE + M1 规划层（#141）
- 四份 vendor 研读笔记 + `docs/laws/` + Skill 分发规格 + 三档能力边界（#143）

### 已知缺口（如实列出，不粉饰）

- **FU-32（A 级）**：入站 relay 静默 —— 桥无法区分「收件箱为空」与「relay
  连接已死」。#142 只关上了子进程挂起那一支。**F5 泡测前必须处理。**
- **FU-29**：Nostr 回复是 **at-most-once**，且子进程超时被杀时**投递状态不确定**
  —— 一条回复可能被静默丢弃，且不会重试（盲重试可能重发）。这是本次发布的 F4
  渠道的用户可见行为。修法需持久化 outbox + 发送侧幂等键。
- **FU-31**：`fsync` 在 macOS 上不含 `F_FULLFSYNC`，见上方微信桥条目的限定。
- F5 7×24 泡测**尚未跑过**（物理任务）
- `agent24 os` 只有 `list`/`enable`/`disable`，**没有 `install`** —— 第三方
  领域 OS 仍需编进二进制（ME-3 未开工）
- 没有 web UI

### 一个会被当成 bug 的正常现象

`agent24 os list` 在 v0.3.0 里仍显示 `sin90 v0.2.1`。**这不是漏改的版本号** ——
领域 OS 模块有**独立的版本线**：真值来源是 `agent24-sin90-os/domain-os.yml` 的
`version` 字段，`identity_matches_the_manifest` 断言 `MANIFEST_VERSION` 等于解析
出的 manifest，`os list` 显示的是**模块版本，不是产品版本**。此前两者恰好都是
`0.2.1` 只是巧合，本次发布把巧合打破了。

## [0.2.1] — 2026-07-26

Hardening of the H9 explorer subagent (found in an adversarial re-review of
v0.2.0) plus protocol/changelog corrections.

### Fixed

- **Explorer is now truly network-free** (H9 security): the read-only registry
  the `explore` subagent runs against no longer includes `http_fetch`. `Read`
  class means "no side effect on the machine", not "no egress" — a GET could
  still send bytes an `fs_read` just returned to an arbitrary URL, which in an
  ungated, model-spawned helper is an exfiltration channel. The explorer now
  has `fs_read` only; a network-capable researcher, if ever wanted, must be a
  separate gated tool.
- **Explorer fanout is bounded** (H9): a single model turn is capped at 16 tool
  calls (mirroring the main loop) and each `explore` call has a 120s wall-clock
  ceiling, so no input shape can make one exploration run for hours.
- **Explorer panics are contained** (H9): the sub-loop runs in its own
  supervised task; a panic becomes a `ToolError` instead of unwinding past the
  caller and leaving a dangling `running` tool-call row.
- **Empty exploration answers are distinct** (H9): an explorer that produces no
  text returns an explicit sentinel, so the caller can tell "found nothing"
  from "produced nothing".
- **Protocol doc**: `approve_for_target` (H4) is now documented in the
  `Decision` type in `openapi.yaml`, and the note that `approve_for_session` is
  not offered for `external` tools is recorded there.

## [0.2.0] — 2026-07-26

**M-H — the human boundary.** Everything a person needs to stay in control of an
agent that runs while they're away: what needs asking, how far a "yes" reaches,
and what an error actually tells them. Studied from Andrew Ng's OpenWorker and
put through an adversarial review before landing (see
`docs/reference-notes/openworker.md`). Protocol changes are additive (a pre-0.2
client keeps validating), with one deliberate behaviour change: `external`-risk
tools no longer offer the broad `approve_for_session` grant — see H4.

### Added

- **Declared risk classes** (H1): a tool's `risk_class`
  (`read`/`write_local`/`exec`/`external`) is now the one property the approval
  path reads, and `requires_approval` is derived from it — two hand-maintained
  lists can no longer drift. Additive protocol field; gating outcomes are
  byte-for-byte unchanged. Each class earns a different exemption path, which is
  what the next two features are built on.
- **User-local risk overrides** (H2): a glob rule the machine's owner writes to
  relax (or tighten) an individual tool's class — the release valve that makes
  MCP's conservative `external` default actually usable. A user may correct a
  guess (third-party `external`) but never overrule knowledge (a builtin's
  class). The store is user-local and is never written by a module, persona, or
  MCP server. `GET/PUT/DELETE /api/v1/tool-overrides`.
- **Target-scoped standing grants** (H4): "always allow" for an external tool
  now binds to an **exact target** (`send → #ops`), owned by the session or the
  schedule that fired the run, matched exactly and revoked when its schedule is
  deleted. The broad whole-tool `approve_for_session` is no longer offered for
  external tools — the safe option is the only option. `Approval.standing_target`
  labels the choice; `GET/DELETE /api/v1/standing-grants`.
- **Read-only explorer subagent** (H9): the `explore` tool delegates a bounded,
  read-only investigation to a fresh sub-agent with its own context, so the
  dozens of file reads it takes to answer "where is X handled?" never crowd the
  main transcript. Read-only and no-recursion are structural — the sub-run's
  registry simply never contains a write/exec tool or `explore` itself.

### Changed

- **Provider errors say what to do** (H12): `openai returned HTTP 429` becomes a
  named cause plus the provider's own message — bad key vs wrong model vs spent
  quota. A rate-limited or 5xx primary now falls through to a healthy backup
  provider (previously every HTTP error stopped there); auth/model errors still
  stop, since a different provider can't fix a config mistake.

## [0.1.0] — 2026-07-24

The first release of the **Rust-core Agent24**: a 24/7 personal/community
workflow agent (not a coding agent). The Electron desktop shell now ships the
Rust `agent24d` daemon as its default backend, speaking the frozen v1 protocol.

### Added — Rust core (agent24d + agent24 CLI)

- **Domain state machines + store** (C1): exhaustive Run / Approval / ToolCall
  state transitions; SQLite persistence via sqlx with `BEGIN IMMEDIATE`
  transactions and a hash-chained (sha256) tamper-evident audit log.
- **Agent loop v1** (C2): `POST /api/v1/runs` → background execution with
  first-class cancellation woven through every await point; full WS lifecycle
  events; fail-closed orphan-run sweep on startup.
- **Tool system** (C3): `Tool` trait + registry with a fixed dispatch pipeline
  (capability whitelist → approval gate → timeout). Builtins: `http_fetch`
  (SSRF-guarded, resolve-then-pin against DNS rebinding), `fs_read`/`fs_write`
  (cap-std dirfd-anchored, beneath-only traversal), `shell_exec` (argv
  execution, never a shell string).
- **Approval system** (C4): fail-closed approval broker — every non-answer path
  (timeout, run-cancel, dropped channel) resolves negative; the store row is
  the single arbiter; `approve_for_session` grants scoped to (session, tool);
  runs enter `awaiting_approval` while a decision is pending.
- **Wall-clock scheduler** (C5): cron / every / at schedules with pre-advance
  (crash cannot double-fire), skip-missed (no replay bursts), and fail-safe
  disable after 5 consecutive failures. Timezone/DST-correct cron.
- **`agent24 tui`** (C6): a ratatui operator client — runs · event stream ·
  approval queue — with WS streaming, auto-reconnect, and REST reconciliation.

### Added — Desktop

- **Runs / Schedules / Approvals pages** (C7): live REST-polling views;
  Schedules form with an instant next-fire preview; desktop notifications on
  new approvals rendering the server's `available_decisions`.

### Changed

- The desktop shell defaults to the Rust `agent24d` backend
  (`AGENT24_BACKEND=node` opts back into the legacy mock).

### Protocol

- Contract-first v1 API frozen in `protocol/` (openapi.yaml +
  events.schema.json), enforced by dual-backend contract tests and a CI
  zero-drift gate.
