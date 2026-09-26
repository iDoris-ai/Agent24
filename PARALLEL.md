# Agent24 × DSH 并行开发工作流

> 建立日期：2026-09-20 ｜ 适用仓库：`/Users/jason/Dev/auraai/Agent24`
> 这份文档是给**人和两个 agent（Claude Code / DSH）**看的总说明。

---

## 1. 背景 / 为什么做这件事

Claude Code 一直在 `Agent24` 上持续开发（主 worktree + 15 个 feature worktree）。
目标：让 **DeepSeek Harness (DSH)** 并行领一部分任务，做到

- 两条线**分支不重叠**、**工作目录不重叠** → 不会互相覆盖
- 但**能共享对话上下文** → DSH 知道 Claude 做过什么，可以接着往下做

---

## 2. 环境事实（实测确认，不是推测）

| 项目 | 实际值 |
|---|---|
| App | DSH Desktop（Electron） |
| DSH_HOME | `/Users/jason/Library/Application Support/dsh-desktop/harness` |
| 运行 profile | `web` |
| Session 格式 | **v3**（`SESSION_FORMAT_VERSION = 3`） |
| Agent24 工作区 | `/Users/jason/Dev/auraai/Agent24`（DSH 按会话 `cwd` 自动分组） |
| Claude 会话目录 | `~/.claude/projects/-Users-jason-Dev-auraai-Agent24`（9 个会话） |

> ⚠️ **两个坑，别踩**：
> 1. 终端里的 `dsh` 默认 `DSH_HOME=~/.dsh`，和 App 用的 harness home **不是同一个**。用 CLI 装插件不会影响 App。
> 2. `--profile desktop` 是被 CLI 硬禁止的保留名；`~/.dsh/profiles/desktop` 是一个废弃空壳。App 真正跑的是 `web`。

---

## 3. 用到的插件：claude2dsh

- 包：`@claude2dsh/plugin@0.3.0`（作者 kirkchinese，仓库 github.com/kirkchinese/claude2dsh）
- 安装位置：`<DSH_HOME>/profiles/web/`（`dependencies` + `dsh.profile.bundles`）
- 选它的理由：README 明确要求 DSH `>=0.1.5-rc.1 <0.2.0`，并写明「DSH 0.1.5 把 Session log 改成 v3」，与本机 0.1.5-rc.2 精确匹配

**数据流**

| 方向 | 行为 |
|---|---|
| Claude → DSH | 读 `~/.claude/projects`（只读），写 DSH 原生会话；**增量 append**，两边都改过时**暂停**而不是覆盖 |
| DSH → Claude | 默认只写安全副本 `<DSH_HOME>/claude2dsh/exports`；要写原始 `~/.claude` 必须显式开 `allowOriginalClaudeDir: true`（**保持关闭**） |

### 已移除的插件（不要再装）
`@z47_rose_1/dsh-session-sync@0.2.0` —— 输出的是 Session 格式 **v0**，与本机 v3 不兼容，**一个会话都写不进去**。已卸载。

> 注意：npm 上**无前缀**的 `dsh-session-sync` 是**另一个**插件（跨设备 git 同步），不是导入器，别装错。

---

## 4. 并行不重叠的约定

### 4.1 worktree 划分

| 侧 | 工作目录 | 分支 |
|---|---|---|
| Claude Code | `/Users/jason/Dev/auraai/Agent24`（main）及既有 `Agent24-<topic>` | `feat/a24-*` |
| **DSH** | `/Users/jason/Dev/auraai/Agent24-dsh` | `feat/a24-dsh` |

### 4.2 任务归属与文件边界

| 任务 | 归属 | 分支 | 涉及文件 | 状态 |
|---|---|---|---|---|
| （示例）capability registry | Claude | feat/a24-capability-auth | `crates/registry/**` | 进行中 |
| （示例）isolation tests | DSH | feat/a24-dsh-isolation | `tests/isolation/**` | 待领 |

**硬规则**

1. 一条任务只能有**一个**归属
2. 必须写明「涉及文件/模块」；**两条在跑的任务文件集不能相交**
3. 公共文件（`package.json`、lockfile、`Cargo.toml`、CI 配置）同一时间只允许一方改
4. 分支命名：Claude `feat/a24-*`，DSH `feat/a24-dsh-*`

### 4.3 「共享上下文」的真实语义（重要）

- ✅ DSH 能读到 Claude 的对话，并**原生 resume** 继续往下做
- ⚠️ Claude **读不到** DSH 的实时对话：导出默认只写安全副本，而且 Claude Code 不会把外部追加进「正在运行的会话」的 JSONL 热加载进来
- 👉 所以真正的同步点是 **git 代码 + 上面的任务表**，对话历史只是参考，不要指望「一个实时共享的大脑」

---

## 5. 操作手册

### 5.1 首次导入（一次性）
`设置 → Claude2DSH` → 源目录填
```
/Users/jason/.claude/projects/-Users-jason-Dev-auraai-Agent24
```
→ **Preview import**（只读预览）→ 确认 → **Run import**
→ 刷新左侧会话列表，9 个会话会出现在 `Agent24` 工作区下

### 5.2 打开自动 watch
`设置 → Claude2DSH → Auto mirror`

- 勾选 **enabled**
- `claudeProjectsRoot` = `/Users/jason/.claude/projects/-Users-jason-Dev-auraai-Agent24`
- debounce 保持默认 500ms

开启后 Claude Code 一边写盘，DSH 这边近实时增量追加（`ignoreInitial: true`，**只跟进新的变化，不回填历史** —— 历史靠 5.1 手动导一次）。

### 5.3 日常流程

1. DSH 开工前先导入一次，读 Claude 的最新进展
2. 按任务表领一条**不与 Claude 相交**的任务
3. 在 `Agent24-dsh` 里干活，提交到 `feat/a24-dsh`
4. 收尾合并回 main

---

## 6. 已知限制 / 坑

| 坑 | 说明 |
|---|---|
| subagent 不自动导入 | watcher `depth: 2`；subagent transcript 在 `<项目>/<会话>/subagents/agent-*.jsonl`（深度 4），只能手动导入 |
| watcher 根目录是**固定**的一个 | 以后若在某个 worktree 里直接跑 Claude Code，会生成新的 project 目录，watcher 看不到；届时改设置里的指向，或指向 `~/.claude/projects` 全量（代价：所有仓库都会同步） |
| 在跑的会话无法热加载 | Claude Code 不会读取外部对当前会话 JSONL 的追加 |
| **移除插件必须重启 App** | DSH 的热重载能**新增** patch 条目，但**移除/停用不会真正 dispose** 已加载的插件。`@z47_rose_1` 那个还额外泄漏了 chokidar watcher（模块级全局、dispose 时不 close），只有重启才能清掉 |
| 会话按 `cwd` 分组 | 在 worktree 里产生的会话会归到那个 worktree 路径的工作区，不是主 `Agent24` 工作区 |

---

## 7. 当前状态 —— **初始化已完成**（2026-09-20 19:12）

- [x] 卸载不兼容插件 `@z47_rose_1/dsh-session-sync`
- [x] 安装 `@claude2dsh/plugin@0.3.0` 并注册为 profile bundle
- [x] 重启 DSH Desktop，插件加载成功（15 个 `claude2dsh_*` 工具已注册）
- [x] **首次导入完成**：9/9 会话，共 **365 轮对话 / 17,659 个事件**
- [x] **Auto mirror 已开启**并落盘到 `settings.yaml`（`paused: false, conflicts: [], pending: []`）
- [x] 建立 DSH 专用 worktree `Agent24-dsh`（分支 `feat/a24-dsh`）
- [x] 写入本文档
- [ ] 删除 4.2 任务表里的示例行，填入真实任务

### 7.1 导入明细（首批）

| 源会话 | 轮数 | 事件 | 工具调用 |
|---|---|---|---|
| `01018bd3…` | 30 | 2271 | 418 |
| `17234327…` | 15 | 2830 | 549 |
| `5f3d565a…` | 4 | 440 | 83 |
| `92218e04…` | 31 | 1782 | 325 |
| `93b8fc9f…` | 37 | 2088 | 373 |
| `a90126ee…` | 104 | 3199 | 525 |
| `ba90fc8c…` | 102 | 2385 | 367 |
| `e9f936cf…` | 19 | 955 | 171 |
| `fe14b3a4…` | 23 | 1709 | 317 |

落盘位置：

- 会话本体：`<DSH_HOME>/sessions/--Users-jason-Dev-auraai-Agent24--/claude-<uuid>/`（`session.v3.jsonl.zstd`）
- 去重登记：`<DSH_HOME>/claude2dsh/registry.json`
- 来源映射：`<DSH_HOME>/claude2dsh/session-sources.json`
- 镜像状态：`<DSH_HOME>/claude2dsh/auto-sync-state.json`

### 7.2 重跑 / 改设置

重复导入同一目录是**幂等**的：已导入报 `already-imported`，源文件长出新轮次则报 `appended`（两边都改过时会**暂停**而不是覆盖）。

设置页对应的 HTTP 接口（仅本机可信，无需 token）：

```sh
U='http://127.0.0.1:43129/plugins/claude2dsh/settings'
curl -s "$U" -H 'Host: 127.0.0.1:43129'                    # GET 当前设置
curl -s -X POST "$U" -H 'Host: 127.0.0.1:43129' \\
     -H 'content-type: application/json' \\
     -d '{"autoSync":{"enabled":false}}'                     # PATCH 设置
```
