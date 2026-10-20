# MAINT-03 —— 数据目录可覆盖（`AGENT24_DATA_DIR`）

> 任务书。本文件是唯一验收判据来源；与其它文档冲突时以本文件为准。

## 1. 为什么做

2026-10-16 真机安装验收发现：`/Applications/Agent24.app`（v0.5.1，build 20261014.1，main 基线）
装好后**核心对话不可用**，真实用户数据目录下 `agent24d` 启动即失败：

```
ERROR agent24d: serve failed: migration 15 was previously applied but is missing in the resolved migrations
```

原因：用户既有 `~/.agent24/agent24.db` 被 `ab/kernel` 线（K1-6a）的构建迁移到了 schema 17，
而预览包（main 基线）只认到 14。**预览包与开发环境共用同一个 `~/.agent24`，schema 一旦先行就永久互斥。**

需要一个**显式的数据目录覆盖开关**，让预览包（以及任何多实例/受控实验）能用自己的数据目录，
互不破坏。默认行为必须完全不变，否则等于静默迁移用户数据。

## 2. 目标

`agent24_protocol::state_file::state_dir()` 是**唯一的数据根**：
`daemon.json`、`logs/`、`agent24.db`、`attach/`、tool workspace 全部由它派生
（见 `rust/apps/agent24d/src/server.rs` 里 `state_dir()` 的调用点）。
因此只要这一个函数可覆盖，数据隔离就是完整的。

## 3. 具体要求

### 3.1 Rust（`rust/crates/agent24-protocol/src/state_file.rs`）

- `state_dir()` 解析顺序：
  1. 环境变量 `AGENT24_DATA_DIR`（非空、可解析为绝对路径即可）→ 返回该路径（**不再拼 `.agent24`**）；
  2. 否则维持现状：`$HOME/.agent24`；
  3. `HOME` 也没有 → 仍返回 `None`（调用方原有报错路径不变）。
- **默认行为必须逐字节不变**：未设置 `AGENT24_DATA_DIR` 时结果与改动前完全一致。
- `state_path()`（`daemon.json`）保持 `state_dir().join("daemon.json")`，不要另开分支。

### 3.2 Desktop（`apps/desktop/src/main/backend-manager.ts`）

- `daemonStatePath()` 必须与 Rust 侧同一口径：优先 `AGENT24_DATA_DIR`，否则 `$HOME/.agent24`。
- 该函数旁的注释请一并更新（它现在写的是"Mirrors `agent24_protocol::state_file::state_path()`"）。

### 3.3 测试（必须真跑，不许只写断言不跑）

- Rust：至少覆盖
  - 未设置 `AGENT24_DATA_DIR` → `<HOME>/.agent24`；
  - 设置后 → 就是该值（注意：环境变量在测试进程里是全局的，请用与仓库现有风格一致的隔离手法，
    不要污染其它并行测试）。
- TS：`backend-manager.test.ts` 增加 `daemonStatePath()` 对 `AGENT24_DATA_DIR` 的用例，
  并保留原有 `$HOME` 行为用例。

### 3.4 文档

- 在 `docs/Deployment/README.md` 补一小节：如何用 `AGENT24_DATA_DIR` 跑一个隔离数据目录的实例
  （包含"预览包与开发环境共存"的场景与影响：隔离实例看不到原数据）。

## 4. 明确不做（越界即失败）

- **不改默认数据目录**，不迁移、不复制、不删除任何既有数据，不动 `agent24.db` 内容。
- 不加 CLI 新子命令，不加配置文件格式，不做 GUI 设置项。
- 不碰 `ab/kernel*` 的任何分支，不碰 `main`，不 merge，不 force-push。
- 不改 `state_dir()` 的返回类型与签名（调用方零改动）。
- 不做与本任务无关的重构（不顺手改日志、不顺手规整格式）。

## 5. 完成判据

1. `AGENT24_DATA_DIR=/tmp/x` 时，daemon 的 `agent24.db` 与 `daemon.json` 都落在 `/tmp/x/` 下；
2. 不设置时行为与改动前完全一致（有测试证明）；
3. `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、
   `cargo test -p agent24-protocol` 与 `cargo test -p agent24d` 通过；
   TS：`pnpm typecheck && pnpm lint && pnpm test` 通过；
4. 改动只落在本分支，工作区干净，提交信息中文，author `jhfnetboy <jhfnetboy@gmail.com>`；
5. 生产代码改动 ≤ 100 行（测试/文档不计）。

## 6. 证据回写

按 baton 车道要求：决策追加到分支上的 `baton-decisions.tsv` 并推送；
回复里写清「做了什么 / 证据（命令与结果）/ 没做完的下一步」。
