# Agent24 任务台账 — Task

> 前置：[`roadmap.md`](roadmap.md)（M→F）·[`architecture.md`](architecture.md) ·[`spec.md`](spec.md)
> 每个 Task 自包含，可独立开发与验收。**验收标准可机器验证**。
> 状态：BACKLOG · READY · IN_PROGRESS · BLOCKED · PR_OPEN · CHANGES_REQUESTED · APPROVED · DONE
>
> **全局验收前置**（每个 task 都适用，不再逐条重复）：
> ```
> cd rust && cargo fmt --all --check \
>   && cargo +1.98.0 clippy --workspace --all-targets -- -D warnings \
>   && cargo +1.98.0 test --workspace
> ```
> **新回归测试一律做变异验证**：把修复改回去，确认测试变红；在 PR body 里写明变异方式与结果。

---

## F1.1 — 判定接缝（原 F8b）

> 依赖 PR #140（F8）已合并。**若 #140 尚未合并，本 Feature 全部 task 保持 BACKLOG。**

### T1.1.1 `Authorizer` 契约与默认实现  `READY`
- **优先级**：high
- **目标**：内核持有一个判定点，签名一次到位。
- **开发范围**：在 `rust/apps/agent24d/src/authz.rs` 新建 `Actor` / `Op` / `AccessRequest` / `Decision` / `Authorizer`（签名逐字照 `architecture.md`「契约 / 接口」一节）；实现 `ModulePrivateOnly`：`allow ⟺ space == SpaceId::module_private(req.module)`。
- **明确不做**：不接线（T1.1.2 做）；不引入任何存储；不放进 `agent24-domain`（那是模块契约，出现能命名空间的参数就是模块能跨的边界）。
- **依赖**：无
- **交付物**：`agent24d/src/authz.rs` + 单测
- **验收命令**：`cd rust && cargo +1.98.0 test -p agent24d --bin agent24d authz`
- **验收要求**：至少三条断言 —— 自有空间 allow；他模块空间 deny；`Decision.reason` 非空（审计要用）。
- **涉及文件**：`rust/apps/agent24d/src/authz.rs`、`rust/apps/agent24d/src/main.rs`(mod 声明)
- **风险/回滚**：纯新增，无回滚风险
- **证据**：

### T1.1.2 把判定点接进句柄发放路径  `BACKLOG`
- **优先级**：high
- **目标**：句柄发放**经过**判定点，且**行为零变化**。
- **开发范围**：`MemoryLease::lend` 在 `catalogue.record(...)` 之前调用 `Authorizer::decide`；deny 则不发句柄并 `tracing::warn!` 带上 `reason`。
- **明确不做**：不改判定逻辑；不给模块任何选择空间的能力。
- **依赖**：T1.1.1
- **交付物**：接线 + 「行为零变化」证明
- **验收命令**：`cd rust && cargo +1.98.0 test --workspace`
- **验收要求**：F1/F8 既有的**跨模块隔离探针全部继续通过**；新增一条测试断言「换成一个恒 deny 的 Authorizer 时，模块拿不到 memory 能力」——**这条是变异验证的落点**。
- **涉及文件**：`rust/apps/agent24d/src/domain.rs`、`rust/apps/agent24d/src/authz.rs`
- **风险/回滚**：判定写错会让所有模块失去记忆 → 验收必须包含既有隔离探针全绿
- **证据**：

---

## F1.2 — personal space（原 F8c）

### T1.2.1 `SpaceId::personal` 与不相交性  `BACKLOG`
- **优先级**：high
- **目标**：agent loop 的记忆在模型里**有一个名字**，且不可能与模块空间相撞。
- **开发范围**：`SpaceId::personal(user) -> "usr:<user>"`；一条把 `usr:` 与 `os:` 不相交性钉死的测试（**扫小的交叉积，不是两个手挑的例子** —— 照 F8 的 `the_partition_key_is_versioned_and_unambiguous` 的做法）。
- **明确不做**：不迁移任何数据（T1.2.2 做）；不 bump `KEY_VERSION`。
- **依赖**：T1.1.2
- **交付物**：构造器 + 不相交性测试
- **验收命令**：`cd rust && cargo +1.98.0 test -p agent24d --bin agent24d space`
- **涉及文件**：`rust/apps/agent24d/src/os_memory.rs`
- **证据**：

### T1.2.2 把 agent loop 的记忆迁进 personal space  `BACKLOG`
- **优先级**：high
- **目标**：消掉 ADR-030 硬门槛 3 —— agent loop 的记忆不再用裸 user id 做 key。
- **开发范围**：迁移 `0014_personal_space.sql`（**只登记目录行，不重写 owner_key**，理由见 `spec.md`）；启动时复用 `rekey_os_partition` 把裸 user id 分区搬到 `partition_key(org, SpaceId::personal(user))`。
- **明确不做**：**不在 SQL 里算 key**（SQLite `length()` 数字符不数字节，非 ASCII user id 会得到与内核不一致的 key —— 0013 已经踩过这条，注释里写清）。
- **依赖**：T1.2.1
- **交付物**：0014 + re-key 接线 + 真实升级路径测试
- **验收命令**：`cd rust && cargo +1.98.0 test -p agent24-memory --lib migration_0014 && cargo +1.98.0 test --workspace`
- **验收要求**：用 `pool_migrated_up_to(&path, 14)` 建一个 0013 态的库 + 一条裸 user id 的事件 → 跑 0014 + sweep → **断言那条事件在新 key 下读得到、老 key 下读不到**；再跑一次 sweep **移动 0 个**（幂等）。
- **风险/回滚**：**这是本轮唯一动到真实用户数据的 task。** re-key 必须一个事务；其余八张 owner-scoped 表有行就整体拒绝（`rekey_os_partition` 已有此行为，**不得放宽**）。
- **证据**：

---

## F1.3 — 记忆接进 agent loop（原 F2）

### T1.3.1 会话轮次写进 EventLog  `BACKLOG`
- **优先级**：high
- **目标**：让「情节权威」名副其实 —— 今天 EventLog 里根本没有对话。
- **开发范围**：agent loop 每一轮追加 `MemEvent`（`kind=chat.user` / `chat.assistant`），scope 用 T1.2.1 的 personal space key。
- **明确不做**：还不动 `CanonicalSession` 的压缩（T1.3.2 做）；不改 `Condenser` 本身。
- **依赖**：T1.2.2
- **交付物**：接线 + 端到端测试
- **验收命令**：`cd rust && cargo +1.98.0 test --workspace`
- **验收要求**：一次模拟对话后，`EventLog` 里能按 seq 顺序读回**完整轮次**；`replay` 出的对话与直接读的**逐条相等**。
- **涉及文件**：`rust/crates/agent24-agent/`、`rust/apps/agent24d/`
- **证据**：

### T1.3.2 `Condenser` 取代 `CanonicalSession` 的压缩  `BACKLOG`
- **优先级**：high
- **目标**：一份压缩实现，不是两套并存。
- **开发范围**：压缩改由 `Condenser`（token 预算触发、策略可换）承担；`CanonicalSession` **降级为投影**，`save(kv)` 那条把会话存成 KV blob 的路径退役。
- **明确不做**：**不允许两套并存** —— `SPEC-ME-FOLLOWUPS.md` F2 与 `architecture.md` 核心判断 3 已定死。不新增压缩策略。
- **依赖**：T1.3.1
- **交付物**：切换 + no-loss 保证的等价证明
- **验收命令**：`cd rust && cargo +1.98.0 test --workspace`
- **验收要求**：`CanonicalSession` 原有的 **no-loss 保证必须仍然成立**（摘要失败不丢消息、下次重试）—— 用一条「摘要器必然失败」的测试钉住；一条长对话经压缩后 `covers(n)` 与实际覆盖轮次一致。
- **风险/回滚**：改的是真实会话的压缩路径。**若切换后 no-loss 无法在 `Condenser` 下等价成立 → 标 `BLOCKED`，在 progress.md 写清，不要自行放宽保证。**
- **证据**：

### T1.3.3 崩溃重放对真实会话生效  `BACKLOG`
- **优先级**：mid
- **目标**：兑现 MD-1b —— 重放的是真实对话，不是空的。
- **开发范围**：一条端到端测试：写入对话 → 模拟崩溃（丢弃内存态）→ 从 EventLog 重放 → 上下文与崩溃前**逐条相等**。
- **明确不做**：不新增重放机制（`replay` 已有），只证明它对真实会话生效。
- **依赖**：T1.3.2
- **交付物**：端到端测试
- **验收命令**：`cd rust && cargo +1.98.0 test --workspace replay`
- **证据**：

---

## 依赖链（一眼看清顺序）

```
T1.1.1 → T1.1.2 → T1.2.1 → T1.2.2 → T1.3.1 → T1.3.2 → T1.3.3
```

**严格串行**，没有可并行的分支 —— 每一步都建立在上一步的存储形状上。
