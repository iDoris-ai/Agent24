# AGENTS.md — Agent24 给 Codex（B 端 ab-codex worker）的工作约定

## 分支规则
- 集成分支：`ab/<里程碑>`（例如 `ab/m1-memory`）。**每个 task PR 的 base 必须是对应的 `ab/*` 集成分支**，绝不能是 `main`。
- task 分支：`ab/<里程碑>-NN-<短名>`，每个 task 一个 git worktree，放在 `~/Dev/iDoris/` 下（B 上 Agent24 主 checkout 是 `~/Dev/iDoris/Agent24`）。
- task PR ≤ 300 行（不含锁文件/生成文件），超了就拆。
- **不许碰**：`main`、任何 `feat/*`、`ci/*`、`test/*`、`docs/*`、`build/*` 分支及其 PR（那些由笔记本 + PR-Daemon 管）。不许 force-push 别人的分支。
- 集成分支 → `main` 的 release PR 永远由人开、人审，worker 不开。

## 任务来源
- 当前里程碑的任务定义与验收标准只认集成分支上的计划文档（任务 prompt 会指明路径）。prompt 与计划冲突时以计划为准，并在 PR body 里指出冲突。
- 计划里标了「本地模型」的任务不在 B 上做（需要 oMLX/本地推理的留给笔记本）。

## 构建缓存（B 盘空间有限，必须遵守）
- **所有 cargo 命令前先** `export CARGO_TARGET_DIR=$HOME/Dev/iDoris/.target-agent24`（所有 Agent24 task 共用一个 target 目录；并发构建时 cargo 会自动排队等锁，正常）。**不要**在 worktree 里生成独立的 `rust/target`（每个 5GB+，曾把 B 盘写满导致所有任务失败）。
- 若 worktree 里已有 `rust/target`，先删掉再构建。

## 门禁（提交前必须全部通过，PR body 贴结果摘要）
Rust（在 `rust/` 下）：
```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
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
