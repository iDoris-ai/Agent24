# SPEC-001: 工程规范（分支 / PR / Review / 编码标准 / CI / DoD）

> 适用于 `docs/specs/TASKS.md` 中的所有任务。loop 会话实现任何任务前必须遵守本规范。

---

## 1. 仓库目标布局（M-A 重构后）

```
Agent24/
├── apps/
│   └── desktop/                # Electron + React（现 src/main + src/renderer + src/shared 迁入）
│       ├── src/main/           # Electron main：BackendManager、IPC、托盘
│       ├── src/renderer/       # React UI
│       └── src/shared/         # main↔renderer 共享类型
├── packages/
│   ├── node-daemon/            # 现 src/backend 迁入——M-A 起兼任 v1 协议 mock/参考实现
│   ├── api-client/             # openapi-typescript 自动生成的 TS client（构建产物，禁止手改）
│   └── contract-tests/         # 协议契约测试（vitest，可指向任意后端 base URL）
├── rust/
│   ├── crates/                 # agent24-core / -protocol / -agent / -models / -scheduler /
│   │                           # -policy / -tools / -store / -memory / -mcp
│   └── apps/
│       ├── agent24d/           # daemon
│       └── agent24-cli/        # CLI + `agent24 tui`
├── protocol/                   # ★ 单一真源：openapi.yaml + events.schema.json + module.schema.json
├── workers/python-ml/          # M-D+
├── docs/
└── pnpm-workspace.yaml
```

**依赖方向铁律**：
- `agent24-core` 零框架依赖（不依赖 axum/Rig/sqlx/任何厂商 SDK）
- `agent24-agent`（runtime 层）不依赖 daemon 层；需要回调上层时定义 trait 由上层实现注入（openfang `KernelHandle` 模式）
- renderer 永不直接发 HTTP；一切经 preload 暴露的 client（过渡期 `backendProxy()`，M-A 后逐步换 `api-client`）
- `packages/api-client` 只能由生成器写入，CI 校验无手工漂移

## 2. 分支与 PR 策略（stacked）

- 主干：`main`。集成方式：**stacked PR 链**。
- 分支命名：`feat/<task-id>-<slug>`（如 `feat/a1-openapi-v1`）、修 bug `fix/…`、纯文档 `docs/…`。
- **Stacked 规则**：任务 N 的分支从「任务 N-1 的分支」切出（若 N-1 尚未 merge）；PR 的 base 设为 N-1 的分支。N-1 merge 进 main 后，GitHub 自动把 N 的 PR base 重定向到 main。无依赖关系的任务可直接从 main 切。
- **一个任务 = 一个 PR**。禁止把多个任务塞进一个 PR；禁止在任务分支上夹带无关改动。
- **不碰用户的未提交改动**：工作区内不属于当前任务的 modified 文件（如 README、TRADEMARK 等历史遗留修改）一律不 stage、不提交、不回退。
- Commit 遵循 Conventional Commits（`feat:` `fix:` `docs:` `refactor:` `test:` `chore:`），commit message 末尾带 Claude-Session 链接（harness 会注入）。
- **merge 由用户执行**。loop 永不自行 merge PR；提完 PR 即基于该分支继续下一个任务（stacked）。

## 3. Review 流程（每个 PR 提交前的硬性门槛）

顺序执行。Tier 1 路径：全部通过后才 `gh pr create`。Tier 2 例外：允许在 review 阶段先建 **draft PR**（draft 不视为正式提交），review 门以 **mark ready** 为完成标志：

1. **本地验证**（按任务涉及的技术栈）：
   - TS：`pnpm typecheck && pnpm lint && pnpm test`
   - Rust：`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
   - 协议改动：contract tests 全绿 + api-client 重新生成无漂移
2. **自我 review**：`git diff <base>...HEAD` 逐文件严格审查——按「严格对抗式」标准找：并发/取消竞态、错误处理缺失、fail-open 的默认值（审批必须 fail-closed）、off-by-one、注入、资源泄漏（未 drop 的 sender/未清理的定时器）。发现即修复。
3. **Codex review**（按全局 tier 链）：
   - Tier 1：codex 插件（`/review` 或 codex:rescue 子代理），送 diff + 任务验收标准，要求严格审查
   - Codex 提出问题 → 立即修复 → 重新送审，直到无 blocker（或明确记录 deferred 项）
   - Tier 1 不可用 → Tier 2 gh Copilot：先建 **draft PR** 并挂 @copilot reviewer，意见全部处理后 mark ready（此时才算过 review 门）→ Tier 3 本地严格 review；PR 描述中标注所用 tier
4. **提交 PR**，描述使用 §4 模板；然后更新 `TASKS.md` 中该任务状态为 `in-pr` 并附 PR 号（此更新 commit 进当前任务分支）。

## 4. PR 描述模板

```markdown
## Task
<task-id>: <任务名>（docs/specs/TASKS.md）

## Changes
- <变更点列表>

## Acceptance criteria
- [x] <逐条对照 TASKS.md 中该任务的验收标准，勾选>

## Verification
- <本地跑过的命令与结果摘要（typecheck/test/clippy/contract tests）>

## Review log
- Self-review: <发现并修复的问题数>
- Codex review (Tier <n>): <轮数>轮，<最终结论>；deferred: <无 / 列表>
```

## 5. 编码标准

### Rust（rust/ 全部代码）
- Edition 2024；`#![forbid(unsafe_code)]`（workspace 级）
- 错误：`thiserror` 定义领域错误；库 crate 不用 `anyhow`（app crate 可）；禁止 `.unwrap()/.expect()` 出现在非测试代码（clippy deny）
- 日志：`tracing`（结构化字段，禁止 println）
- **取消**：任何长运行 async 任务必须接收 `CancellationToken` 并在循环点 `select!`（ADR-026 硬约束 #1）
- 并发原语选择：一对多广播用 `watch`/`broadcast`；请求-响应挂起用 `oneshot` 表（fail-closed：Drop = 拒绝）
- sqlx：migrations 进 `rust/crates/agent24-store/migrations/`；CI 用 offline 模式（`.sqlx/` 提交）
- 公共 API 类型全部定义在 `agent24-protocol`，`#[derive(Serialize, Deserialize, JsonSchema)]`，serde 统一 `rename_all = "snake_case"`、tag 见 SPEC-002

### TypeScript（apps/ + packages/）
- 沿用现有 eslint/prettier 配置（单引号、无分号、100 列、strict）
- Vitest 覆盖率阈值维持：lines/functions/statements ≥80%、branches ≥70%（Electron 运行时文件按现有豁免清单）
- 禁止手写与 `protocol/` 重复的类型——一律 import 自 `packages/api-client`

### 通用
- 注释密度跟随周边代码；只写「代码本身表达不了的约束」
- 四类定时概念命名隔离（ADR-026 硬约束 #7）：`Schedule`（挂钟 cron）/ `Trigger`（事件触发）/ `AgentTick`（自主循环，M-F+）/ `Quota`（资源配额）——禁止混用 "scheduler" 命名配额器

## 6. 测试要求

| 类型 | 位置 | 要求 |
|---|---|---|
| Contract tests | `packages/contract-tests/` | 参数化 `BASE_URL`（+可选 token）；对协议中每个 endpoint/事件类型至少一条正例一条错例；**必须能对 node-daemon 与 agent24d 双跑** |
| Rust 单测 | 各 crate `#[cfg(test)]` | 状态机穷举关键转移；审批表测 Drop=Abort；调度器测 pre-advance 防重复（用 mock clock，禁止 sleep 真实时间） |
| TS 单测 | 各包 | 维持现有覆盖率阈值 |
| 集成 | M-C 起 | `agent24d` 起真进程跑 contract tests（CI job） |

## 7. CI（`.github/workflows/ci.yml` 演进）

M-A 重构任务需把 CI 扩为三个 job：
1. `node`：pnpm install → typecheck → lint → test（现有）
2. `rust`（rust/ 存在后）：fmt --check → clippy -D warnings → test（sqlx offline）
3. `contract`：启动 node-daemon（后续加 agent24d 矩阵）→ 跑 contract-tests → 校验 api-client 生成无漂移（`git diff --exit-code`）

任何 PR CI 红 = 不得请求 review，先修。

## 8. Definition of Done（每任务）

- [ ] 验收标准全部满足（TASKS.md 逐条）
- [ ] 新增/变更行为有测试；本地全套检查绿
- [ ] 协议改动同步了 `protocol/` 真源 + 重新生成 api-client + contract tests 更新
- [ ] 自我 review + Codex review 完成，无未记录的 blocker
- [ ] PR 已提交（模板完整），TASKS.md 状态已更新为 `in-pr`
- [ ] 未夹带无关文件改动

## 9. 安全红线（任何任务不得违反）

- 审批默认 **fail-closed**（通道断开/超时/进程退出 = 拒绝）
- daemon 只绑定 `127.0.0.1`；agent24d 起用 token 认证（M-B 起）
- 不引入 GPL/AGPL 依赖（`cargo deny` 在 M-B 接入 license 检查）；参考 `vendor/reference/` 中 GPL 仓库只读思路、禁止复制代码
- 凭据不落 git；容器/子进程启动参数不裸拼 shell
- 遥测/日志不含用户内容原文（审计详情落本地库，日志只记 id/hash/时长）
