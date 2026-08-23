# Agent24 规格（M1）— 落地细节

> 「建成什么样」。精确到能照着实现。架构与边界见 [`architecture.md`](architecture.md)。
> 记录日期：2026-08-23

## 产品定义

一个本地 AI 代理框架，**记忆是它的第一场景**。本轮把记忆从「已交付但零消费者的底座」变成「agent loop 真的在用、且归属清晰、且访问有判定点」的东西。

## 数据模型

### 既有（F8 已建，本轮不改结构）

| 表 | 关键字段 | 说明 |
|:---|:---|:---|
| `mem_orgs` | `org_id` PK（生成、不可变、不解析）· `display_name` · `created_at` | 组织 |
| `mem_org_members` | `(org_id, user_id)` PK · `joined_at` | 一人一个 home org |
| `mem_os_partitions` | `owner_key` PK · `key_version` · `org_id` FK · `space_id` · `logical_user`（**创建者**，非所有者）· `module_name` · `first_seen_at` · `last_seen_at` | **目录**：逻辑身份 → 物理 key → 该 key 的编码 |
| `mem_events` | `scope_owner` · `id` UNIQUE(全局) · `seq` · … | 情节权威 |
| `mem_checkpoints` | `(scope_owner, name)` PK · `up_to_seq` | 投影书签 |

### 本轮新增

**F1.1**：**无新表。** 判定接缝纯内存、纯内核侧。

**F1.2**：新增迁移 `0014_personal_space.sql` —— **只用于记录**，不改表结构：
- 为每个已有 user 在目录里登记一条 `kind=personal` 的分区行（`space_id = 'usr:<user>'`，`module_name` 用哨兵值 `'-'` 表示「非模块分区」）。
- **不在 SQL 里重写 `owner_key`** —— 理由与 0013 完全相同：SQLite 的 `length()` 数**字符**不数**字节**，非 ASCII 的 user id 会得到一把与内核派生不一致的 key。re-key 走 Rust（`KvStore::rekey_os_partition`）。

> ⚠️ `module_name` 有 `trim(...) <> ''` 的 CHECK，所以哨兵不能是空串。用 `'-'`，并在迁移注释里写明它表示什么。

**F1.3**：**无新表**。会话轮次作为 `MemEvent` 写进既有 `mem_events`。

## 关键取值

| 常量 | 值 | 约束 |
|:---|:---|:---|
| `KEY_VERSION` | `v2` | 本轮**不 bump** —— personal space 用的是同一套 (org, space) 编码，不是新编码 |
| personal `SpaceId` | `usr:<user>` | 与 `os:<module>` 同名空间不相交（前缀不同） |
| module `SpaceId` | `os:<module>` | 已建 |

**`usr:` 前缀必须与 `os:` 不相交**，否则一个叫 `usr:alice` 的模块就能撞上 alice 的 personal space。模块名已被校验为 ASCII，但**必须加一条测试**把这条不相交性钉死 —— 「不可达」是论证，测试才是属性。

## 状态机

### agent loop 的会话（F1.3 后）

```
用户发话 → 追加 MemEvent(kind=chat.user)  ──┐
                                            ├─→ EventLog（唯一权威）
模型回复 → 追加 MemEvent(kind=chat.assistant)┘
                       │
                       └─→ CanonicalSession（投影：从事件重建当前上下文）
                                  │
                                  └─→ Condenser（按 token 预算压缩，策略可换）
```

**`CanonicalSession::save(kv)` 这条把会话存成 KV blob 的路径退役。** 它是「情节权威里没有对话」的直接原因。

### 分区 re-key（F1.2 复用 F8 的路径）

```
目录登记逻辑身份 → 事务{ 移 mem_events → 移 mem_checkpoints → 改目录行 } → 提交
                     ↑ 其余八张 owner-scoped 表有行 → 整个 re-key 拒绝
```

## 错误处理 / 幂等

- **判定失败 = 不发句柄**，不是发一个会在每次调用上失败的句柄（F1 已确立的形状）。
- **re-key 幂等**：sweep 只处理 `key_version` 落后的行；跑第二次移动 0 个。
- **目录记录是发句柄的前置条件**，不是事后记账 —— 记不上就不借，否则产生没人能归属的孤儿行。
- **`ensure_org_for_user` 在歧义上 fail-closed**（一人两 org → 报错而非挑一个）。

## 测试策略

| 层 | 覆盖 | 本轮新增 |
|:---|:---|:---|
| 单测 | `Authorizer` 默认实现的 allow/deny 两侧 | F1.1 |
| 跨分区探针 | 两个模块互相读不到（F1/F8 已有，**必须继续通过**） | 回归 |
| 不相交性 | `usr:` 与 `os:` 空间永不相撞 | F1.2 |
| 真实迁移路径 | 用 `pool_migrated_up_to` 建 0013 态库 → 跑 0014 → 断言 | F1.2 |
| 端到端 | 一次真实对话在 EventLog 里留下可重放事件；重放结果 == 直接读的结果 | F1.3 |
| 变异验证 | **每条新回归测试都要**：把修复改回去，看它变红 | 全部 |

**「机器可验证」的底线**：`cargo +1.98.0 test --workspace` 全绿 + `cargo fmt --check` + `cargo +1.98.0 clippy --workspace --all-targets -- -D warnings` 零输出。

## 本轮明确不做

- 不引入 grants / groups / workspace 持久化（ADR-030：等第二个真实用户）。
- 不 bump `KEY_VERSION`。
- 不碰 `mem_kv` 的 namespace 模型（F5a 已单独登记）。
- 不做配额（F7）。
- 不改 `/api/v1/<name>` 路径（F3 属 M2）。
