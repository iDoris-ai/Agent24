# SPEC-ME-FOLLOWUPS — M-E 收口：修复 / 完善 / 隔离

> 立于 2026-08-22。来源三处合并：`docs/architecture/kernel-boundary.html` 那两张图识别出的缺口、`improvement/README.md` 的 TODO-A、以及 #131–#134 四轮复审里判定「不挡本 PR、但要单独治」的条目。
>
> **用户指令（2026-08-22）**：先把这些做到位，再继续 ME-3 / ME-4（Cos72 骨架已写完并 park 在 `feat/me4-cos72-skeleton`，未提 PR）。

---

## 0. 为什么是这几项

M-E 到 ME-2 为止交付的是**可换**：内核不再按名字认识 Sin90，装哪些 OS 由配置决定。

但架构图末尾那句话没有变：

> **换 OS 今天能换，但「换了不互相污染」还没有机制保证。**

下面按「**隔离 → 兑现 → 收口**」排序。F1 是唯一一条动到核心不变量的，其余都是把已知的缺口补齐。

---

## F1 · 领域 OS 之间的记忆隔离（最高优先）

**这是用户直接问过的那个问题**：「我切换不同的 Sin90 或 Cos72，它们之间的 RAG 数据库或底层 memory 是不是相通的？总不能相互污染。」

**今天的答案是：相通的。** 三条事实叠在一起：

| 事实 | 位置 | 后果 |
|---|---|---|
| `KernelCtx::memory(scope)` **不存在** | `agent24-domain` 契约里明确标注为未实现 | 领域 OS 拿不到受限记忆句柄，只能各自开库 |
| `Scope` 有 `owner/agent/session/run` 四维，**只有 owner 维在存储层强制** | `agent24-memory` 11 张表 | — |
| **`agent` 维全仓零使用**（0 处 `scope_agent` 列） | 同上 | **同一 owner 下挂两个 OS，记忆就是相通的** |

> 注意区分：**每个 OS 自己的业务库是物理隔离的**（`~/.agent24/os/<name>/<name>.db`，ME-1b-b 起由内核派生目录，模块不自己拼路径）。没有隔离的是**共享的 M-D 记忆底座**——那才是「RAG / 底层 memory」。

### 三个候选方案（2026-08-22 评估）

| | 做法 | 代价 | 评价 |
|---|---|---|---|
| **A** | 强制既有的 `agent` 维：11 张表加 `scope_agent`、进主键、读写路径全带 | SQLite **改不了主键**，11 张表逐个重建；~12 个模块 API 改动 | 语义勉强说得通 |
| **B** | 新增独立的 `scope_module` 维 | 同 A | 语义更诚实（agent ≠ module），但代价一样 |
| **C** | **内核为模块句柄派生 owner 键** | **零迁移** | ← 倾向这个 |

**C 的做法**：`KernelCtx::memory()` 返回的 `ScopedMemory`，其 owner 键是 `derive(user, module_name)`（NUL 分隔，所以用户 id 伪造不出另一个模块的键）。隔离因此**骑在已经被证明过的 owner 强制上**，而不是新引入一条同样需要六轮探针的路径。模块面向的 API **任何地方都没有 owner 参数** —— 它注入，正如 `EventSink` 盖模块名而不是接受模块名。

**倾向 C 的三条理由**
1. `ScopedMemory` 是抽象，**键怎么定是它背后的实现细节** —— 所以现在选 C **不会锁死将来换 B**，模块面向的 API 不用变;
2. 复用一条 #115/#119/#122–#125 六轮复审已经逐表打过跨 owner 探针的强制路径;
3. 零迁移 = 对既有用户数据零风险。

> 这个决定正在做对抗式挑战（重点:C 在哪里会漏、把 owner 重载成「(用户, 模块)」将来会不会后悔、以及「C 不锁死 B」这句话是不是真的）。**结论落定后回填这里。**

**要交付**
1. `KernelCtx::memory() -> Option<&dyn ScopedMemory>`：**能力受限句柄，不是 ambient `MemoryStore`**；能力=句柄存在，与 `events()` 同构；**不接受调用方传进来的 `Grants`**（ME-1a 已把 `Grants` 定性为信息而非权威）。
2. 按上表定下的方案实现隔离，并**逐表打跨模块探针**（照那六轮的做法）。
3. **决定共享模型**并写明**污染面分析**。
4. 若确实需要跨 OS 共享：设计**显式**机制 —— 共享什么、谁授权、怎么审计、怎么撤销。

**验收**
- 两个领域 OS 挂在同一 owner 下，A 写入的记忆 **B 读不到**，且有跨 OS 探针测试逐表覆盖；
- `KernelCtx::memory` 拿不到未授予的 scope（能力=句柄存在，与 `events()` 同构）；
- 迁移不丢数据（老库无 agent 维，需给出默认值策略）。

---

## F2 · M-D 记忆底座的零消费者问题

架构图核对代码时发现：**M-D 建好的 12 个模块，agent loop 一个都没用** —— 仍走 M-D 之前的 `session::CanonicalSession`。

底座建成了但没接上，等于**两套记忆并存**：M-D 的 EventLog/AssertionLedger/Condenser 在那里，而实际跑的对话用的是另一套。

**要交付**：给出接入方案（一次性切换 vs 双写过渡），并至少让 **Condenser** 与 **EventLog** 进入真实 agent loop。

**验收**：一次真实对话在 M-D 的 EventLog 里留下可重放的事件；`CanonicalSession` 要么退役，要么明确降级为投影。

---

## F3 · 领域 OS 命名空间要不要改成 `/api/v1/os/<name>`（需改 ADR）

**现状**：`/api/v1/<name>`，由 ADR-029 / SPEC-MD-ME §2 钉死。

**问题**：模块名可能撞上内核路由段，而 **axum 对精确路由重叠会 panic** —— 即一个第三方 OS 取名 `health` 就能让 daemon 起不来。ME-1b-a 用「保留段名单 + 双向集合相等测试」挡住了，但那是**检查**；`/api/v1/os/<name>` 会让这一整类**不可表达**。

**代价**：改 ADR-029 + SPEC；`/api/v1/sin90/*` 变成 `/api/v1/os/sin90/*`（今天无外部消费者，越早越便宜）。

**这是架构决定，等用户裁决。** 不裁决就维持现状 + 保留名单。

---

## F4 · #134 复审判定「单独治」的五条

| ID | 条目 | 说明 |
|---|---|---|
| F4a | **`/api/v1/os` 没进 `protocol/openapi.yaml`** | 那份契约连 `/sin90/*` 都写了；`packages/api-client` 由它生成，所以 TS 客户端拿不到 `DomainOs*` 类型。**且 `lint:openapi` 只 lint 已存在的东西，CI 抓不到「少写了一个端点」** —— 这条本身也该治。 |
| F4b | **`patch_os` 在 tokio worker 上做阻塞的 `lock_exclusive()` / `sync_all()`** | 对端拿着锁不放会无限期占住一个 runtime worker。→ `spawn_blocking`。 |
| F4c | **`agent24 os list` 在无常驻 daemon 时回答的是临时 daemon 的状态** | `state`/`detail`/`restart_required` 描述的是一个用完即弃的实例，输出里没有一个字说明。→ 标注，或 `os list` 不走 ephemeral 回退。 |
| F4d | **`needs_models` 已是常量，但对外措辞仍断言代码不再检查的事** | 且 `Installed` 将来加 `requires_models` 时没有任何东西提醒回来改回谓词。→ 改措辞 + 加绑定测试。 |
| F4e | **PATCH 放行 `Refused` 条目** | `os enable <会被准入拒绝的模块>` 返回成功并落盘，但不存在任何能让它生效的路径。今天不可达（生产 catalogue 只有 sin90 且可准入），**ME-3 会让它变成现实问题**。 |

---

## F5 · 记忆层剩余的隔离一致性

| ID | 条目 |
|---|---|
| F5a | **判定 `kv` 的 namespace-vs-owner 模型**：`PRIMARY KEY (namespace, key)` 是另一套隔离模型。namespace 是否由 owner 派生？两个 OS 挂同一 owner 会不会撞？（与 F1 同批做最省。） |
| F5b | **收紧 `mem_events` 的 `scope_owner` CHECK 到 `trim(...)`**：其余八张表都是 `trim`，只有它是 `<> ''`，**纯空格 owner 在它这里过得去**。0002 是已发布迁移不可改，需新迁移。 |
| F5c | **`agent24-store` 五处列表查询只按秒级时间戳排序、无 tie-breaker**（`repo.rs` 的 sessions/runs/tool calls/approvals/standing grants）。`agent24-sin90-store` 已用 rowid 兜底并有同秒顺序测试。当前无测试依赖那些顺序，属债不属活 flake。 |

---

## F6 · 模块目录的 symlink 安全

ME-1b-a 的挂载器只做了「目标已是 symlink 就不用它、把该模块降级」的**检查**——它挡住了真实会发生的那种（`~/.agent24/os/cos72` 软链到 sin90 目录 = 两个 OS 共用一个库），但**不是保证**：`root` 的任一祖先仍可以是软链，且 `symlink_metadata` 到 `create_dir_all` 之间存在 TOCTOU。

真正的隔离需要 `openat` 式目录句柄（`cap-std` / `openat2`）。契约文字已同步收窄，不再声称做到了。

---

## 7. 建议顺序

1. **F1**（隔离，也是用户直接问的那条）
2. **F4**（五条小修，一个 PR 能收完，其中 F4a 顺手治「CI 抓不到少端点」）
3. **F2**（记忆底座接入 agent loop —— 体量最大，建议单独排期）
4. **F5**、**F6**
5. **F3** 等用户裁决

ME-3（进程外 Provider）与 ME-4（Cos72，已 park）在这些之后。

---

## 8. 关联

- 架构快照：[`docs/architecture/kernel-boundary.html`](../architecture/kernel-boundary.html)（带日期，图中「当前架构」的两处缺陷已修）
- ADR-029：`docs/decision.md`
- 分期表：`docs/specs/SPEC-MD-ME.md` §5
- 长期台账：`improvement/README.md`
