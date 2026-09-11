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

## ME-3 / v0.5.0 —— 当前主线（用户 2026-09-10 裁决）

> 完整任务分解（PLAN 的 T1–T14）在 [`PLAN-OOP-OS-AND-BACKLOG.md`](PLAN-OOP-OS-AND-BACKLOG.md) §五，这里**不抄第二份**。
> 这里只放**正在做的**那几条，且 ID 带 `ME3-` 前缀，免得和下面 M1 的 `T1.x.y` 撞名。
> **哪一刀已在 main 上，跑 `bash docs/agent/me3-status.sh`，不看本表。**

### ME3-T4 3b-4 受约束代理合入  `DONE` — [#173](https://github.com/iDoris-ai/Agent24/pull/173)（`b1b4b82`，2026-09-11）
- 已 APPROVE（head `00f5e27`）；#165 合入后与之冲突（只在 SPEC 状态表）→ 合 main 解冲突、取探针（`0163803`）→ 复扫对新 head 重新 APPROVE → 合并。
- **验收**：探针 `3b-4 受约束代理` 一行变 `●`。

### ME3-MUT 变异脚手架收掉三条阻塞  `CHANGES_REQUESTED`(修复已在分支上,等推送后复审)— [#172](https://github.com/iDoris-ai/Agent24/pull/172)
- 两轮 PR 前复审发现 bash 版的问题同根(用 shell 手搓进程监督器),核心改写为 `mutate.py`;自证 45 组、元测试 26 种回退全红。
- B1 红基线不拦后续 `mut` · B2 0 个测试的基线 / 测试数变化读成「判据不承重」· B3 中途被杀留下被变异的源码；同轮收掉非阻塞项（崩溃被读成编译失败、超时只杀直接子进程、`set -u` 泄漏、路径打错建出空文件、不认识的输出落到 🔴、自证不断言）。
- **验收**：`bash docs/agent/mutate-selftest.sh` exit 0；把每条修复改回去自证都 exit 1（PR body 列回退清单）。

### ME3-DOC 状态文档对齐  `DONE` — [#174](https://github.com/iDoris-ai/Agent24/pull/174)(`09669bc`)
- `progress.md` 停在 2026-08-23，与仓库脱节十九天；本条把它和本文件对齐，并记下本轮四条待办。

### ME3-T5 3b-5 两阶段热 disable（SPEC §4）  `PR_OPEN` — 本 PR(分支 `feat/me3b-5-drain`)
- **优先级**：high（ME-3b 的最后一刀）
- **依赖**：3b-3（库层已在 main）· ME3-T4 ✅（要接进 `proxy.rs`；不叠在 #173 上开 stacked PR —— 合并自动删分支会把叠在上面的 PR 一起关掉）
- **目标**：停一个模块时，「宽限期内收不收新请求、在途 handler 还能不能回调、generation 什么时候撤」三件事由一个状态机定死，而且**撤 generation 早于杀进程**由类型保证，不靠调用顺序。
- **开发范围**：
  1. `agent24-os-proto` 里一个纯状态机：`Starting → Running → Draining → Revoked`（没有单独的 `Stopped`，理由见 SPEC「3b-5 落地时定死的几条」）；在途请求登记（带 `request_id`）；drain 在「在途清零」与「宽限到期」先到者结束。
  2. **回调准入**是它的一个纯判定：Running 全收；Draining 只收**携带并命中活跃 `request_id`** 的；Revoking 之后一律拒。回调通道本身是 ME-3c，本刀只交判定，3c 调它。
  3. **杀进程组的入口要求一个只能由「撤销 generation」产出的值**，于是「先杀后撤」写不出来（编译不过），而不是「测试里没这么写」。
  4. 接进代理：Draining 期间新的被代理请求 503（`error.code` 区分 draining / not-ready / overloaded，§2.1 已预留）；在途请求继续；drain 超时的在途请求 503、结果记为「未知」写日志，不假装成功。
- **明确不做**：回调通道（3c）；把状态机接到 daemon 的 `os disable` 路径 —— 今天 daemon 对 `out_of_process` 包在挂载时就拒绝，**没有一个运行中的进程外模块可以被热停**。
- **CLI help 的到期标记**：`agent24-cli/src/main.rs` 那两行「applies at the next daemon start」**本刀之后仍然为真**（理由同上），所以本刀**不改它们**，只把到期标记的触发条件从「3b-5 落地」改成「daemon 的 `os disable` 真的作用于运行中的模块」。PLAN T5 原文写的「同时改 help」是按「3b-5 = 接线完成」写的，与 3b-3 只交付库层的实情不符 —— 在 PR 里讲清，不静默改掉一句会变假的 help。
- **验收**（取自 SPEC §8 ME-3b 格与 PLAN T5，每条带正对照）：
  - Draining 期间新的被代理请求 503 且 `code = module_draining`；**对照**：同一时刻已在途的请求正常完成（200）。
  - Draining 期间：携带活跃 `request_id` 的回调被放行；不带 `request_id` 的被拒；携带**已结束**请求 id 的被拒。**对照**：Running 期间三者都放行。
  - drain 超时：仍在途的请求得到 503（不是 200、不是挂住），日志里有「结果未知」。
  - generation 撤销先于 `terminate_group`：由类型保证 + 一条 `compile_fail` doctest 证明「未撤销就杀」写不出来；**对照**：撤销后能杀的那条 doctest 编译通过。
  - Revoking 之后一切回调拒绝，包括持有活跃 `request_id` 的。
- **涉及文件**：`rust/crates/agent24-os-proto/src/{drain.rs,proxy.rs,lib.rs}`（探针预言的符号是 `drain.rs` 的 `pub enum DrainState`，照它命名）、`rust/apps/agent24-cli/src/main.rs`（仅注释）、`docs/specs/SPEC-ME3-OUT-OF-PROCESS.md`（记本刀的边界）
- **证据**：

### ME3-T6 3c 回调通道其余部分（SPEC §3 + §8 ME-3c 格）  `PR_OPEN` — 分支 `feat/me3c-rpc`（用户 2026-09-11 定为下一步）
- **优先级**：high（T1–T6 齐了即可发 v0.4.0 握手层）
- **依赖**：3b-1 framing、3b-2b `initialize`（均已在 main）。**不依赖** #175（3b-5）：本刀 offer set 为空，没有任何业务方法，「draining 期间回调准入」要等第一个业务方法（3d/3e）才有落点
- **目标**：握手之后那条回调连接上的一切协议行为定死，实现者不需要猜并发、取消与错误分类
- **开发范围**（`agent24-os-proto/src/rpc.rs`，探针预言符号 `pub fn dispatch`）：
  1. `dispatch`：把握手之后的**一帧**分类成「立即回错」「派发给 handler」「取消某个在途请求」「忽略（未知 notification）」—— 纯函数，不碰连接
  2. 一个连接循环：并发在途、响应可乱序、按 id 配对；超时回 `timeout` 且不重试；连接断开则在途请求就地中止、**不产生响应**
  3. 错误闭集 `error.data.kind`（SPEC §3 原文十个），常量带「整词出现在 SPEC 引文里」的测试（§8 的规则）
- **明确不做**：任何业务方法（offer set 为空，调任何方法都是 `-32601`，**不得为了凑 forbidden 测试提前注册方法**）；把连接循环接进 daemon（那是 Supervisor 的事，见 ME3-SUP）
- **SPEC 的空档，本刀要补并请评审拍板**：§3 说「并发上限见 §5，超限回 busy」，但 §5 没有给数字；超时也没有数字。取与代理侧一致的 **每连接 64 个在途 / 单次 30s**，写进 SPEC
- **验收**（取自 SPEC §8 ME-3c 格，每条带正对照）：握手后的超长行被拒并断连；并发在途按 id 配对、响应可乱序；仍在途的 id 被复用 → 该请求失败；`$/cancelRequest` 使目标请求回 cancelled；连接断开则在途请求中止且不产生响应；超时不重试；握手后 params 解析失败 `-32602` 且不派发；坏 params 只失败该行、连接继续；重复 JSON key 被拒；握手后畸形 JSON `-32700` 只失败该行；握手后重复 `initialize` → `-32600` 只失败该行；业务方法一律 `-32601`
- **证据**：

### ME3-SUP 3b-3 的 Supervisor —— 让 daemon 真的持有并监督模块进程  `BACKLOG`（排在 T6 之后、T9 之前）
- **为什么单列**：3b-3 目前只交付了库层零件（`RestartPolicy`、`terminate_group`、`launch::spawn`），daemon 里没有任何代码真正持有一个模块进程。没有它：3b-5 的热 disable 接不进 `os disable`；`KillPermit` 绑定不到具体进程（FU-46）；**T9（3f 仓外包端到端验收）跑不起来**
- **范围（待开工时细化）**：一个类型同时持有 child 与它那一代的 `Generation`，只暴露先撤后杀的 `stop(self)`，`Launched::child` 改私有（FU-46）；启动 → 握手 → ready → 挂代理 → 崩溃退避/熔断 → 停机的完整生命周期；解除 `domain.rs` 对 out_of_process 的挂载拒绝（**那一天 CLI 两行 help 与 `os_routes.rs` 的承诺到期**，绊线测试已就位）；顺带评估 FU-47
- **依赖**：T6（回调连接循环）

---

## M1 —— 记忆成为产品（2026-08-23 规划；状态未改动，未重排）

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
