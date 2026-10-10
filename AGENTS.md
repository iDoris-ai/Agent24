# AGENTS.md — Agent24 给 Codex / 编码代理的工作约定

> 2026-10-03 起 Agent24 的全部任务（派发、编译、测试、验收）都在**笔记本**上执行；B 机（Mac mini ab-codex）因磁盘不足不再构建 Agent24，下文已去掉 B 端专属约定。

## 限时授权记录（2026-10-10）
- 用户于 2026-10-10 新增常设指令：所有 PR 必须交由后台 PR-Daemon 外部进程审查；外部审查对精确 head APPROVE、required CI 全绿且 mergeable 时，即授权立即合并。协调者自行判断成本与风险，低成本/低风险的文档 follow-up PR 与接口澄清无需逐次询问；高影响、不可逆或优先级变更仍依全局 Agent 指令上升。
- 本分支是 PR #854 合并后的一个低风险、纯文档 follow-up，范围仅限明确 D1-2a L1 失败层身份与 JSON/API 形状；不含生产代码/测试实现、发布或其他方向工作。仍须由 PR-Daemon 外部进程审查，并遵守项目门禁。

## 分支规则
- 集成分支：`ab/<里程碑或方向>`（例如 `ab/m1-memory`、`ab/decide`），规则见下节「里程碑方向分支工作流」。**每个 task PR 的 base 必须是对应的 `ab/*` 集成分支**，绝不能是 `main`。
- task 分支：`ab/<里程碑>-NN-<短名>`，每个 task 一个 git worktree，放在 `~/Dev/auraai/` 下（笔记本 Agent24 主 checkout 是 `~/Dev/auraai/Agent24`）。
- task PR ≤ 300 行（不含锁文件/生成文件），超了就拆。
- **不许碰**：`main`、任何 `feat/*`、`ci/*`、`test/*`、`docs/*`、`build/*` 分支及其 PR（那些由笔记本 + PR-Daemon 管）。不许 force-push 别人的分支。
- 集成分支 → `main` 的 release PR 永远由人开、人审，worker 不开。

## 里程碑方向分支工作流（2026-10-07 jason 拍板，适用于每个方向的里程碑系列）
- **一个方向一条集成分支** `ab/<方向>`，例如：
  - `ab/decide`：决策服务 D1–D3；
  - `ab/memory-p1`：记忆 P1；
  - `ab/comm`：Hyphae COMM6b/7。

  各方向独立开发、互不阻塞。
- **task PR 合进集成分支**。每攒约 10 个，开一次 **release PR `ab/<方向>` → `main`**，作为该方向的阶段发布。
- **评审与合并门槛**（PR-Daemon 照常评审全部 PR，包括 base 为集成分支的 task PR）：
  - **普通 task PR**：CI 全绿 + `pre-pr-check.sh` 逐类回应 + 本地门禁通过后，即可合入集成分支；PR-Daemon 的意见在后续 task 中修复。
  - **安全 / 权限 / 授权、数据写入语义、存储迁移、对外协议 / wire 格式类 task PR**：必须等 PR-Daemon APPROVE 后再合。
  - **release PR**：必须 PR-Daemon APPROVE，并通过 jason 真机验收，才合入 main。
- **三条硬约束**：
  1. **集成分支每周至少吸收一次 main**。用 merge，不用 rebase，避免 M10 那样漂移 600+ 提交。
  2. **集成分支受 ruleset 保护**：禁止 force-push，task PR 合并前 CI 必须全绿（ruleset `ab-integration`）。ruleset 按**精确分支名**列出集成分支，避免误伤 task 分支；新开一个方向时，先把它的集成分支名加进 ruleset。
  3. **命名**：集成分支 `ab/<方向>`；task 分支 `ab/<方向>-NN-<短名>`。task 分支只合进本方向的集成分支，不跨方向。
- 未经 jason 允许，**禁止在 Mac mini 上执行任何任务**（构建、测试、基准、下载模型）。

## 任务来源
- 当前里程碑的任务定义与验收标准只认集成分支上的计划文档（任务 prompt 会指明路径）。prompt 与计划冲突时以计划为准，并在 PR body 里指出冲突。

## 构建缓存
- 所有 worktree 共用主 checkout 的 target：cargo 命令前先 `export CARGO_TARGET_DIR=$HOME/Dev/auraai/Agent24/rust/target`（并发构建时 cargo 会自动排队等锁，正常）。**不要**在 worktree 里生成独立的 `rust/target`（每个 5GB+）。

## 门禁（提交前必须全部通过，PR body 贴结果摘要）
Rust（在 `rust/` 下）：
```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
**Codex 沙箱已知失败（白名单）**：沙箱禁止 `ps`/跨进程信号，下面 4 个既有测试在 Codex 沙箱里必然失败，与你的改动无关：
`agent24-protocol state_file::tests::current_pid_is_alive`、`agent24-os-proto launch::tests::the_child_gets_its_own_process_group`、
`agent24-cli` 的 `uninstall_hot_stops_a_running_module_and_leaves_no_tombstone` 与 `daemon_stop_waits_for_the_lock_before_reporting_success`。
**只有这 4 个失败时视为门禁通过**：照常提交、推送、开 PR，在 PR body 写明「仅白名单沙箱失败」并列出 workspace 测试计数；最终以 PR 上 GitHub CI（完整 `cargo test --workspace`）为准。出现白名单外的任何失败都不算通过。
跑测试前先 `ulimit -n 4096`。

**PR 体积**：≤300 行是默认上限；单片确实不可再拆（拆开后中间版本不可用）时允许超出，在 PR body 说明为什么不拆。

TypeScript（只在改了 `apps/` 或 `packages/` 时跑，在仓库根）：
```
pnpm install --frozen-lockfile
pnpm typecheck && pnpm lint && pnpm test
```
改了 `protocol/` 时额外跑 `pnpm lint:openapi` 和 `pnpm gen:api` 并确认无 diff。

## 提交与 PR
- commit author 必须是 `jhfnetboy <jhfnetboy@gmail.com>`（CLA 检查），不要把 Codex/AI 写成 author。
- commit message、PR 标题与正文用中文；标题格式 `<type>(<scope>): <任务ID> —— <摘要>`。
- 测试先行：验收判据先写成测试并确认它在修复前失败（反面对照），PR body 说明怎么证的。
- 只修真 bug，不做范围外重构；原型阶段不要过度设计。
- 包管理一律 pnpm，不要用 npm/yarn。
