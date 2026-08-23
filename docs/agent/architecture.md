# Agent24 架构（M1 相关部分）— 技术判断与边界

> 「怎么搭」。定义契约与**不可动摇的边界**。数据细节见 [`spec.md`](spec.md)。
> 记录日期：2026-08-23。全局架构决策见 [`../decision.md`](../decision.md)（ADR-028/029/030）。

## 核心判断

### 1. 判定接缝的价值在**签名**，不在实现

`Authorizer` 的默认实现只有几行（只允许模块自己的私有空间）。真正贵的是**调用点**：今天读路径就那么几条，F1.3 一接线会多出一批，事后给每一条补参数是另一个量级的工作。

所以签名必须**现在**就容得下将来要问的东西 —— actor / module / space / op / scope / reason 六项，缺一项将来就要重铺一次。这是 Codex 复审对第一版 SPEC 的核心意见，已写进 ADR-030。

### 2. 执行点是**句柄发放**，不是每次查询

F8 的形状本来如此：模块拿到的句柄**绑死一个分区**，没有参数能让它换一个。判定因此发生在「内核决定把哪些空间的句柄交给这个 (user, module) 组合」的时候。

这样 `confidential` 之类的约束才是**构造性**的（拿不到句柄），而不是每次查询被拒。

### 3. `CanonicalSession` 与 `Condenser` **不允许两套并存**

`SPEC-ME-FOLLOWUPS.md` F2 已经定死：「`CanonicalSession` 要么退役，要么明确降级为投影（而不是两套并存）」。

本轮选择**降级为投影**：EventLog 成为会话的**唯一权威**，`CanonicalSession` 从「存储 + 压缩」退成「从事件投影出来的当前上下文」。理由是 MD-1b 的崩溃重放对真实会话必须生效，而只要 `CanonicalSession::save(kv)` 还把会话存成 KV blob，情节权威里就根本没有对话。

### 4. 迁移一律**通过目录**，绝不前缀匹配

F1 的 `mem_os_partitions` 目录在 F8 里第一次被真的用上（v1→v2 re-key）。F1.2 的 personal space 迁移**复用同一条路径**：先在目录里记下逻辑身份，再 re-key，全程一个事务。

**不得**用 `LIKE` 去匹配含 NUL 的 owner key —— 这是 F1 复审逐条否掉过的做法。

## 系统骨架（本轮涉及的部分）

```
agent24d (组合根)
 ├─ MemoryLease::open(user, kv)          ← 解析 org、跑遗留分区 sweep
 │    └─ Authorizer                       ← 【F1.1 新增】判定点
 ├─ mount_all → MemoryLease::lend(...)   ← 句柄发放，判定在此发生
 │    └─ OsScopedMemory                   ← 模块拿到的能力受限句柄
 └─ agent loop
      ├─ CanonicalSession                 ← 【F1.3】降级为投影
      └─ EventLog                         ← 【F1.3】成为会话唯一权威

agent24-memory (存储)
 ├─ KvStore                               ← ROOT 句柄，绝不交给模块
 ├─ EventLog / AssertionLedger / …
 ├─ mem_orgs / mem_org_members            ← F8
 ├─ mem_os_partitions                     ← 目录：逻辑身份 → 物理 key → 编码
 └─ Condenser                             ← 【F1.3】接管压缩
```

## 契约 / 接口

### `Authorizer`（F1.1，内核侧，**不进模块契约**）

```rust
pub enum Actor<'a> { User(&'a str) }   // 今天只有 User；Service 是已知缺口

pub enum Op { Read, Write, Admin }

pub struct AccessRequest<'a> {
    pub actor:  Actor<'a>,
    pub module: &'a str,                 // 第二个主体
    pub space:  &'a SpaceId,             // 不可变 ID
    pub op:     Op,
    pub scope:  Option<&'a ActiveScope<'a>>,  // 请求级作用域，今天恒为 None
}

pub struct Decision { pub allow: bool, pub reason: &'static str }

pub trait Authorizer: Send + Sync {
    fn decide(&self, req: &AccessRequest<'_>) -> Decision;
}
```

**放在 `agent24d`，不放 `agent24-domain`。** 模块契约里出现任何能命名空间的参数，就是一个模块能跨越的边界 —— F1 的第 1 条结构性规则。

### 默认实现（F1.1 交付的唯一实现）

```
allow ⟺ space == SpaceId::module_private(req.module)
```

**跟今天完全等价。** 它不是 access control，也不许被描述成 access control。

## 不可动摇的边界

- **模块永远拿不到 `KvStore`、pool、`EventLog` 或任何由它们派生的原始 store。** 拿到 `KvStore` 就等于拿到全部（events / artifacts / assertions / retriever / consolidation / knowledge / trace / vector 的访问器都挂在它上面）。
- **模块面向的 API 任何地方都没有 owner / space 参数** —— 内核注入，正如 `EventSink` 盖模块名而不是接受模块名。
- **没有全局维护操作交给模块**（`rebuild()` 是跨全部 owner 的操作）。
- **进 key 的只有不可变 ID**（ADR-030 决策 2/3）。任何变得勤的东西 —— 部门、授权、作用域 —— 永不进 key。
- **迁移必须一个事务**：事件行、checkpoint、目录行同生共死；半个搬移比不搬更糟。
- **`rekey_os_partition` 只搬 events + checkpoints**，其余八张 owner-scoped 表**有行就拒绝**，不静默孤儿化。

## 运行形态

单个 `agent24d` 守护进程、单个 SQLite 文件、单用户（`LOCAL_USER` 常量）。
**org/space 是否可能跨物理数据库或地区，是 ADR-030 的硬门槛 2，本轮不触碰，也不得在本轮引入任何假定它可以的代码。**
