# 研读笔记：Macro — 统一工作区（macro-inc/macro，AGPL-3.0）

> 来源：`vendor/macro/`（github.com/macro-inc/macro，**AGPL-3.0**，本地只读克隆 @ `c5e609e13`，2026-08-24）
> 日期：2026-08-25 | 用途：Cos72 workspace 能力的设计输入 + Agent24 M1 判定接缝的直接输入
> 所有 `path:line` 相对 `vendor/macro/`（涉及本仓库代码时给出仓库内完整路径）。
>
> **本文分两半**：§1–§9 是「它是什么、怎么搭的」；§10–§13 是「我们拿它干什么」。
> 想直接看结论的，跳到 **§10（许可证硬边界）** 和 **§12（路线图设计）**。

---

## 0. 一句话定位 + 规模

**「把 Slack + Linear + Notion + HubSpot + Superhuman 重新设计成一个系统」** ——
邮件 / 消息 / 文档 / 任务 / 通话 / 文件 / CRM / Agent 共用一个后端、一套权限、一张双向图。
自称由 ~15 人团队 dogfood 两年（`README.md:31`）。

| 维度 | 数字 |
|---|---|
| Rust crates | **191** |
| 后端服务 | **47** |
| Pulumi 基础设施 stack | **41** |
| Rust 代码行 | **~717,000** |
| 前端（SolidJS）行 | ~40,000 |
| 版本 | `v2026.4.28.0` |

技术栈：**Rust（axum + sqlx + async-graphql）+ SolidJS + Tauri 2 桌面壳**（`apps/web/tauri/src-tauri/`）+
Postgres / OpenSearch / Redis / DynamoDB / S3 / Kafka / Lambda，IaC 用 **Pulumi**，开发环境用 **Nix flake**。

**与 Agent24 的关系：产品同构、体量异构、许可证冲突。**
它是一个融了钱的 SaaS 公司的全部代码；我们是一个本地优先的单进程内核。
**它值得学的不是规模，是它在 191 个 crate 上没有失控的那几条纪律。**

---

## 1. ⭐ 纪律一：每个 crate 都是六边形，无一例外

191 个 crate，凡是有业务的，目录结构**逐字一致**：

```
crates/<name>/src/
  domain/     模型 + ports(trait) + service 实现   ← 不依赖任何外部技术
  inbound/    axum router / AI toolset 适配器      ← feature = "inbound"
  outbound/   pg_*_repo / s3 / opensearch 适配器    ← feature = "outbound"
```

（`crates/entity_access/src/lib.rs:1`、`crates/soup/src/lib.rs:4`、`crates/skills/src/lib.rs:11`、
`crates/connection/src/lib.rs:1` —— 四个毫不相干的 crate，同一段注释模板。）

而且 **inbound / outbound 是 cargo feature**。一个服务只编它要的那一半：
`soup` 在 API 服务里带 `inbound`，在 AI 工具进程里带 `ai_tools`，在 worker 里只带 `outbound`。
**「哪一层能被谁依赖」由编译期强制，不靠 code review。**

> **对我们**：Agent24 已经是这个形状（`agent24-domain` 契约 / `agent24d` 组合根 / `agent24-memory` 存储），
> 但 **feature 门这一招我们没用**。`agent24-memory` 今天把 `KvStore` 和 `ScopedMemory` 编在同一个
> crate 的同一份构建里 —— 「模块永远拿不到 `KvStore`」是**文档保证 + 私有字段**，不是**构建保证**。
> 见 §11.1。

## 2. ⭐ 纪律二：`Entity` 是全系统唯一地址

一个 `Entity = (EntityType, id)`，所有 11 类东西共用（`crates/models_soup/src/item.rs:27`）：

```rust
pub enum SoupItem<T = ()> {
    Document, Chat, Project, EmailThread, Channel, ChannelThread,
    Call, CalendarEvent, CrmCompany, ForeignEntity, Reminder,
}
```

任何一项都能 `.entity()` 出同一种地址（`item.rs:54`）。于是：

- **@link 是一条边**，不是 11 种特例；
- **权限只有一套**（§3）；
- **搜索只有一个索引**；
- **Agent 的工具面只有一组**（§5），不用给每种类型写一个 `list_emails` / `list_docs`。

## 3. ⭐⭐⭐ 纪律三：`EntityAccessReceipt<T>` —— 权限是**类型级凭证**，不是布尔

**这是整个仓库里最该抄的一个想法**（`crates/entity_access/src/domain/models.rs:435`）：

```rust
/// 表示某用户对某 id 拥有某权限。
/// 类型参数 T 编码了「创建这张收据时被验证过的最低权限」。
pub struct EntityAccessReceipt<T: RequiredPermission> {
    pub(crate) auth: EntityAccessAuth,
    pub(crate) entity: Entity,
    pub(crate) entity_permission: EntityPermission,
    pub(crate) _marker: PhantomData<T>,
}

impl<T: RequiredPermission> EntityAccessReceipt<T> {
    pub fn try_new(...) -> Result<Self, AccessError> {
        if !entity_permission.satisfies::<T>() {
            return Err(AccessError::Unauthorized);   // 唯一的构造入口
        }
        ...
    }
}
```

三件事叠起来才有力量：

1. **字段全是 `pub(crate)`** —— crate 外造不出来；
2. **唯一构造函数会验证** —— 造得出来就等于验过了；
3. **领域方法签名要求 `EntityAccessReceipt<CanWrite>`** —— **「忘了检查权限」是编译错误，不是漏掉的 if。**

还有一个细节值得单独记：`try_into_requirement::<U>()`（`models.rs:452`）
允许把一张**更强**的收据降级传给只读方法，**不重新查库** —— 既省一次查询，又不会
因为「懒得再查一次」而出现绕过检查的旁路。

> **对我们 —— 这条直接改 T1.1.1**：
> `docs/agent/architecture.md:69` 现在的签名是
> ```rust
> pub struct Decision { pub allow: bool, pub reason: &'static str }
> ```
> 这是**一个可以被忽略的返回值**。调用点写成 `let _ = authz.decide(&req);` 就悄悄绕过了，
> 而 `architecture.md:10` 自己说「真正贵的是**调用点**」—— 那正是最该由类型盯住的地方。
>
> **建议在 T1.1.1 落地前把 `Decision` 改成 `Lease` 凭证**（详见 §11.2）。这条改动现在做几乎零成本，
> 等 F1.3 把调用点铺开之后再做就是「另一个量级的工作」—— 用 `architecture.md` 自己的话说。

## 4. `soup`：统一查询面

> "Soup is an amalgamated service which allows callers to query for data by filters and receive **many entities of different types**" —— `crates/soup/src/lib.rs:2`

Soup 是「统一收件箱 / 统一搜索」的**服务端本体**：一个分页游标、一套过滤 AST
（`filter_ast` + `item_filters`）、一套分组（`models_grouping`）、一个排序方向
（`crates/soup/src/domain/models.rs:52`），返回混合类型的一页。

排序方法里有一个叫 **`Frecency`**（frequency + recency，`crates/frecency/`）——
「我最近常碰的东西」是一等排序键，不是搜索的附属品。

配套：`item_filter_index` / `predicate_index`（把过滤器反向索引起来，用于实时推流：
一条新消息进来，反查哪些已保存视图会被它影响）、`soup_realtime`、`saved_views`。

GraphQL 侧由 `complete_graph` 把 6 个领域 schema 组合成一张完整 schema 并导出 SDL
（`crates/complete_graph/src/lib.rs:1`），跨域字段以 `SoupEdges` 的形式**组合**上去，
而不是让每个领域互相 import。

## 5. Agent 层：ACP 在这里也出现了

| crate | 干什么 |
|---|---|
| `agent` | agentic loop 本体，基于 `rig_core`，`PredefinedModel` 抽象模型 |
| `agent_session` | 会话（能自动命名，见最新提交 `1899a6f93`） |
| `agent_harness` | **容器化沙箱**：「Container transports for sandboxed coding-agent sessions」（`crates/agent_harness/src/lib.rs:1`），provider 是 Daytona |
| `agent_runtime_protocol` | **「外层协议，携带 ACP 消息 + 运行时控制消息，不解释被包裹的 ACP 载荷」**（`src/lib.rs:4`） |
| `agent_trigger` / `agent_fold` | 定时/事件触发的自动化；结果折叠 |
| `ai_toolset` | 工具定义框架：trait + `schemars` 自动生成 JSON Schema + **每个工具用 `FromRef` 抽取自己那部分 context** |
| `ai_tools` | 具体工具集：`all_tools()` / `mcp_tools()` / `no_tools()`（`crates/ai_tools/src/lib.rs:112`）+ `subagent.rs` |
| `mcp_client` / `mcp_select` / `pipedream_mcp` | MCP 客户端与选择 |
| `skills` | **「Skill 就是 sub_type = `skill` 的 markdown 文档」**（`crates/skills/src/lib.rs:4`） |

两点值得记：

- **`agent_runtime_protocol` 是「不解释 ACP 载荷」的外层信封。** 和 Berd 用 ACP 连 Goose
  是同一个协议、不同的用法：Berd 是 ACP 的**客户端**，Macro 是给 ACP 加了一层**运行时控制信封**。
  两个独立团队都选了 ACP，这是一个值得注意的收敛信号（详见 `berd.md` §9）。
- **Skill 不是新概念，是文档的一个子类型。** 所以 skill 天然继承了文档的权限、搜索、@link、版本历史。
  这比我们把 skill 做成独立注册表要省一整套设施。

## 6. ⭐ `memory` 在 Macro 里是**派生投影**，不是底座 —— 和我们正好相反

```rust
pub type Memory = String;                      // crates/memory/src/domain/ports.rs:23
static GENERATION_MODEL: PredefinedModel = PredefinedModel::Smart;      // service.rs:11
static JUDGE_MODEL:      PredefinedModel = PredefinedModel::Sonnet4_6;  // service.rs:12
```

Macro 的「团队记忆」是这样来的：一个 agent 拿着全套工具**去把工作区研究一遍**
（"Look at my documents, projects, emails, channels, and search for content I've created"），
生成一段 1000–3000 字的散文，塞进后续 prompt 的前面；再由**另一个模型当裁判**，
数据不足 / 全是 hedge 词（"likely" "suggests"）就**打回重来**（`service.rs:60+`）。

**这是一个诚实的架构选择，也是一个明确的取舍：**

| | Macro | Agent24 |
|---|---|---|
| 记忆是什么 | **一段字符串**，工作区的有损摘要 | EventLog + 断言账本 + 向量 + consolidation |
| 权威在哪 | **工作区本身**（邮件/文档/消息才是事实） | **记忆底座本身**（EventLog 是情节权威） |
| 怎么更新 | 定期整段重生成 + LLM 裁判 | 增量事件追加 + 投影 |
| 能不能重放 | 不能 | 能（MD-1b） |
| 能不能审计「为什么这么说」 | 不能 | 能（evidence / supersedes） |
| 隔离粒度 | 用户级（`save_memory(memory, user)`） | (org, space) 分区键 |

**Macro 之所以能这么草率地对待「记忆」，是因为它有一个统一的工作区当权威。**
它不需要记忆很准 —— 记错了，agent 再 search 一次就是了。

**我们没有那个工作区。** 这正是本笔记 §11 的主线：
**Agent24 的记忆底座是对的，但它今天没有东西可记 ——「零消费者问题」（`SPEC-ME-FOLLOWUPS.md` F2）。
Cos72 workspace 要补的正是这一块：给记忆底座一个真实的、持续产生事件的工作区。**

## 7. 搜索与协作

- **搜索**：OpenSearch（`opensearch_client` / `opensearch_query_builder` / `search_processing_service`）
  + `models_search_cursor` 分页 + `name_search` + `frecency` 排序。
  README 提到一个设计得很对的点：**给 agent 暴露的是「统一搜索」工具，能直接搜出邮件附件 PDF 的正文，
  而不是让 agent 先拉邮件线程、再拉附件、再解析**（`README.md:62`）。
  工具面的形状决定了 agent 要绕多少弯。
- **协作**：**Loro CRDT**（`packages/loro-mirror`、`packages/collaboration`）+ Lexical 编辑器
  （`packages/lexical-core`、`services/lexical-service`）+ Cloudflare Durable Objects 做房间。
  README 声称 agent 作为**对等的 CRDT 协作者**加入文档编辑，冲突由 CRDT 原生处理（`README.md:118`）。

## 8. 「定制部署」的真实成本 —— 这条别抱幻想

用户问的「provide customized deployment」，在 Macro 这边的真实形状是：

| 层 | 内容 |
|---|---|
| 本地开发栈 | `docker/docker-compose.yml`：**23 个服务容器** + Postgres/Redis/OpenSearch/Jaeger/Mailpit |
| 生产 IaC | `infra/stacks/` **41 个 Pulumi stack**，全部 AWS `us-east-1`（`infra/README.md:8`） |
| 密钥 | Doppler（`infra/stacks/doppler-projects`） |
| 认证 | 自建 FusionAuth 实例（`infra/stacks/fusion-auth`） |
| 可观测 | Datadog（`us-central-1`） |
| 开发环境 | **必须 Nix**（`docs/RUNNING_LOCALLY.md:15`），sqlx 离线元数据要 `nix develop --command just prepare_db` |

**结论：Macro 的「自部署」= 复刻一家公司的整个 AWS 账号。**
它开源的是**源码**，不是**可交付的部署单元**。README 里的入口是 "Sign up" 和 "Book demo"。

> **对我们**：这恰恰是 **Cos72 的差异化位置**。
> 社区 / 小团队要的不是 41 个 Pulumi stack，是**一个二进制 + 一个 SQLite + 一条 systemd unit**。
> Agent24 今天的运行形态（`docs/agent/architecture.md:97`：单 `agent24d` + 单 SQLite）
> **正好是 Macro 做不到的那一档**。别去追它的规模，去占它放弃的那一格。

## 9. 不该学的

1. **191 个 crate 的粒度。** `generic_email_domains`、`ensure_exists`、`non_empty`、`maybe_send`
   这种一个函数一个 crate 的做法，是为了 CI 增量编译和 20 人并行开发买的单。我们 2 个人抄这个只有痛苦。
2. **camelCase 的数据库列。** 他们自己的 AGENTS.md 要专门警告
   「记得 `SELECT "userId" as "user_id"`」（`AGENTS.md:52`）—— 这是历史债，不是设计。
3. **`\cd` 别名。** `AGENTS.md:66` 要求 agent 用 `\cd` 而不是 `cd` 来导航仓库。这是工具坏了打的补丁。
4. **AWS 全家桶。** DynamoDB 只用来跟踪 WebSocket 连接、Kafka 只是事件总线 —— 我们单机不需要。
5. **把 CRM 抄进来。** Cos72 是社区 OS，不是销售 OS。`myshop/mytask/myvote`（ADR-004）才是它的本体。

---

# 第二部分：我们拿它干什么

## 10. ⚠️ 许可证硬边界（先读这条，再读设计）

| 项目 | 许可证 |
|---|---|
| **Macro** | **AGPL-3.0**（`vendor/macro/LICENSE.txt:1`） |
| Agent24 | Apache-2.0（`LICENSE`、`package.json:6`） |
| Berd | Apache-2.0 |

**AGPL-3.0 与 Apache-2.0 单向不兼容**：AGPL 代码可以吸收 Apache 代码，反过来不行。
而且 AGPL 的**网络条款**意味着：只要我们把一个含 Macro 派生代码的服务通过网络提供给别人用，
就必须以 AGPL 开放**整个作品**的源码 —— 对一个准备做「商业实体授权 + Pro 服务」的生态（见 `MISSION.md`
的开源+商业双生模型），这是会传染到商业侧的。

**因此本笔记的使用规则，写死在这里：**

- ✅ **可以**：读它、学它的**架构思想**（想法与架构不受版权保护）、独立用自己的方式实现；
- ✅ **可以**：引用它的**接口形状**做设计讨论（如 §3 的收据模式）；
- ❌ **不可以**：复制粘贴任何 Macro 的源码到 Agent24；
- ❌ **不可以**：把 `vendor/macro/` 提交进仓库（**已在 `.gitignore` 加 `vendor/macro/`**）；
- ❌ **不可以**：让 agent 在实现 Cos72 workspace 时**打开 macro 源文件当模板**。
  实现阶段应该只看本笔记，不看原始代码 —— 这是「清洁室」纪律，也是唯一能事后说得清的做法。

> 顺带：`vendor/berd/` 是 Apache-2.0，**没有这条限制**，可以直接借用代码（保留版权头和 NOTICE）。
> 两个 vendor 目录的规则不一样，别记混。

## 11. 三条可以**立刻**用上的（不等 Cos72）

### 11.1 给 `agent24-memory` 加 feature 门，把「模块拿不到 `KvStore`」变成构建保证

今天 `docs/agent/architecture.md:88` 的第一条不可动摇边界是：
> 「模块永远拿不到 `KvStore`、pool、`EventLog` 或任何由它们派生的原始 store。」

这条今天靠**私有字段 + 复审纪律**保证。Macro 的做法是让它靠 **cargo feature** 保证：
`root` / `scoped` 分成两个 feature，模块侧的依赖只开 `scoped`，`KvStore` 在那个构建里**根本不存在**。
「拿不到」从「没有公开的路径」升级成「没有那个符号」。

**成本**：一次 `Cargo.toml` 重排 + 若干 `#[cfg(feature)]`。**不改任何逻辑。**
**收益**：F1 那六轮复审反复确认的东西，从此由 `cargo check` 确认。

### 11.2 ⭐ 把 T1.1.1 的 `Decision` 换成凭证类型（**建议在开工前定**）

T1.1.1 现在是 `READY`、还没开工，正是改签名的窗口。对照 §3：

```rust
// 今天的设计（docs/agent/architecture.md:69）——返回值可以被忽略
pub struct Decision { pub allow: bool, pub reason: &'static str }
pub trait Authorizer { fn decide(&self, req: &AccessRequest<'_>) -> Decision; }

// 建议：凭证不可伪造，且是发句柄的唯一入场券
pub struct AccessGrant {
    space:  SpaceId,          // 私有字段
    module: String,
    op:     Op,
    reason: &'static str,     // 审计要用，保留
}

impl Authorizer {
    /// 唯一构造 AccessGrant 的地方。deny 时返回 Err，没有第二条路。
    fn authorize(&self, req: &AccessRequest<'_>) -> Result<AccessGrant, Denied>;
}

// 于是句柄发放的签名变成：
impl MemoryLease {
    fn lend(&self, grant: AccessGrant) -> OsScopedMemory { … }
    //             ^^^^^^^^^^^ 没有 grant 就编译不过
}
```

**这与 `architecture.md` 已有的两条判断严丝合缝，不是新方向：**

- 判断 2「执行点是**句柄发放**，不是每次查询」—— 凭证正是「发放」这个动作的形式化；
- 判断 1「判定接缝的价值在**签名**」—— 那就让签名把「必须问过」这件事**扛住**。

**风险与代价（诚实说）**：
- `AccessGrant` 里带 `SpaceId` 意味着凭证与空间绑定，`lend` 不能再从别处取 space —— 这是**收紧**，是好事，但要确认 F8 的 `lend` 调用点全都能拿到 grant；
- `Denied` 变成错误类型后，调用点从 `if !d.allow` 变成 `?`，**日志点要重新安排**（今天的设计是 deny 时 `tracing::warn!` 带 reason，`tasks.md:37`）——建议 `Denied` 自己带 reason，在**内核统一一处**记录，比每个调用点各记一次更可靠；
- **T1.1.2 的「行为零变化」验收不受影响**：默认实现仍是 `allow ⟺ space == module_private(module)`，只是拒绝路径从「返回 false」变成「返回 Err」。
- ⚠️ **这条我没有实测过。** 上面是设计意见，不是验证结论；真正落地前应按仓库纪律先做一轮对抗式挑战（尤其是「F8 的所有 `lend` 调用点是否都能拿到 grant」这条）。

### 11.3 `frecency` 是一等排序键

Agent24 的记忆检索今天是「向量相似度 + 时间」。Macro 把 **frequency × recency 做成独立的排序方法**，
理由很实在：**人要找的东西，绝大多数是自己最近反复碰过的**，而这一维语义检索抓不到。
这条对 F1.3 之后的记忆召回质量是便宜的大改进，登记进 M2 的跟进项即可。

## 12. ⭐ Cos72 Workspace：设计 + 排进路线图

### 12.1 先厘清一件事：我们要的「不是 Macro」

用户要的能力是：**「messages, docs, tasks, calls, email, unified search, AI context」**。
但 Cos72 是**社区 OS**（ADR-004：`myshop` 积分兑换 / `mytask` 任务积分 / `myvote` 投票），
不是创业公司 OS。原样照搬 Macro 的七件套会做出一个**没有社区的 Slack**。

**正确的读法是：Macro 证明了一件事 —— 那七个东西的价值不在各自的功能，在于它们共用
一个 `Entity` 地址、一套权限、一张双向图。**
Cos72 要的是**同一个机制**，装的是**不同的内容物**：

| Macro 的块 | Cos72 的对应 | 为什么 |
|---|---|---|
| Messages（channels/DM） | **Messages** ✅ 直接对应 | 社区的中心就是对话；已有 F4 Nostr + F3 微信两条真实渠道 |
| Docs（CRDT markdown） | **Docs** ✅ 直接对应 | 社区提案、章程、纪要 |
| Tasks | **`mytask`** ✅ 已在 ADR-004 | 但要接上积分，不是纯 issue tracker |
| CRM（Company/Contact） | **Members / 贡献档案** | 社区不管客户，管**成员与贡献** |
| Calls（录音转写） | **Calls** ⏸ 延后 | 转写成本高、隐私面大，价值在 M6 之后 |
| Email | **Email** ⏸ 延后 | 社区场景弱于消息；且 Gmail OAuth 是一整套合规工作 |
| — | **`myshop` / `myvote`** ➕ 新增 | Cos72 独有，Macro 没有对应物 |
| Unified search | **统一搜索** ✅ 核心 | 这是「一个系统」的证据 |
| AI context | **接进 M-D 记忆底座** ✅ 核心 | 我们比 Macro 强的地方在这里（§6） |

### 12.2 架构落点：Cos72 是一个 `DomainModule`，workspace 是它的内容

好消息：**这件事不需要动内核。** ADR-029 的 `DomainModule` 缝、ME-2 的配置驱动注册表、
F1 的 `ScopedMemory`、F8 的 `(org, space)` 所有权 —— **地基已经浇好了，Cos72 workspace 就是第一个
真正压上去的重物。**

```
agent24d（内核，领域无关）
 ├─ MemoryLease + Authorizer          ← M1 F1.1/F1.2 正在做
 ├─ EventLog（情节权威）               ← M1 F1.3 接线
 └─ mount_all → DomainModule
      ├─ agent24-sin90-os              （个人 OS，已有）
      └─ agent24-cos72-os              （社区 OS）
           ├─ workspace/                ← 【新】统一实体 + 双向图 + 统一查询
           │    ├─ CosEntity            （Message/Doc/Task/Member/Proposal/Order…）
           │    ├─ links                （双向边表）
           │    └─ query                （过滤 AST + 游标 + frecency 排序）
           ├─ mytask / myshop / myvote  （ADR-004 三件套，作为 CosEntity 的子类型）
           └─ channels adapters         （复用已有 F3 微信 / F4 Nostr）
```

**关键设计约束（三条，来自 Macro 的教训 + 我们已有的边界）：**

1. **`CosEntity` 的地址和 `SpaceId` 是两件事，不能混。**
   `SpaceId` 是**内核**的分区键（F8/ADR-030：进 key 的只有不可变 ID）；
   `CosEntity` 是**模块内部**的对象地址。模块面向的 API 里**依然不能出现 space 参数**
   （`architecture.md:89`）—— workspace 全部活在内核发给它的那一个句柄底下。
2. **双向图存在模块自己的库里**（`~/.agent24/os/cos72/cos72.db`），**不进 `agent24-memory` 的 11 张表**。
   理由：那 11 张表是**内核的记忆底座**，加一张「社区消息的边表」进去，等于把领域知识焊死进内核 ——
   ADR-029 的边界线就白画了。
3. **workspace 产生的每一个有意义的动作，都往 `EventSink` 打一条事件。**
   这才是 §6 说的那条主线：**记忆底座的价值 = 有东西可记。**
   Cos72 workspace 是 M-D 记忆底座的**第一个真实消费者**，也是「零消费者问题」的解药。

### 12.3 排进路线图

现有 `docs/agent/roadmap.md` 是 M1（记忆成为产品）/ M2（M-E 收口余项）/ M3（组织化，不排期）。
Workspace 是一条**新的产品线**，不该塞进 M1，也不该跟 M2 的技术债混在一起。建议**新开 M4/M5**，
并且**明确写死它排在 M1 之后** —— 理由不是优先级偏好，是**依赖**：

> workspace 的全部价值依赖「事件真的落进 EventLog、且按空间隔离」。
> **M1 F1.3 没做完，Cos72 workspace 写出来的事件就没有权威的地方可去。**
> 先做 workspace 等于在一个已排定要搬家的分区上盖房子 —— 和 `roadmap.md:17` 拒绝调整
> F1.1→F1.2→F1.3 顺序的理由，是同一条。

```
M1 记忆成为产品        ← 当前，不变
M2 M-E 收口余项        ← 不变
M4 Cos72 Workspace 底座   ← 【新】依赖 M1 全部完成
M5 Cos72 三件套上线       ← 【新】依赖 M4
M6 Workspace 扩展面       ← 【新】Calls / Email / 桌面壳，等真实需求
M3 组织化              ← 不变，仍不排期（等第二个真实用户）
```

**M4 — Cos72 Workspace 底座**（目标：一条社区消息能被搜到、被 @link、被 agent 用作上下文）

- **F4.1 `CosEntity` 与双向图** — 统一地址 + 边表 + 迁移。验收：一条消息 @link 一个任务后，
  从任一端都能查到对端；删除任一端时边的行为有定义（不是悬空）。
- **F4.2 统一查询面** — 过滤 AST + 游标分页 + 混合类型一页 + frecency 排序。
  验收：一次查询返回 message/doc/task 混合结果，游标翻页无重无漏（**这条要有真实的乱序插入测试**）。
- **F4.3 事件落底座** — workspace 的每个动作产生 `MemEvent`，经模块句柄写入。
  验收：重放 EventLog 能还原 workspace 的状态摘要；跨模块隔离探针**全部继续通过**。
- **F4.4 Agent 工具面** — 给 agent 暴露 `workspace.search` / `workspace.get` / `workspace.link`。
  验收：agent 一次调用能拿到跨类型结果（学 §7 那条：**别让 agent 绕弯**）。

**M5 — Cos72 三件套上线**（目标：Cos72 是一个社区真的能用的东西）

- **F5.1 `mytask`** — 任务 ↔ 积分，绑在消息上下文上（Macro §README:96 的核心洞见：
  任务追踪器过时是因为它和对话不在一个系统里）。
- **F5.2 `myshop`** — 积分兑换。
- **F5.3 `myvote`** — 提案 / 投票，提案是 `CosEntity`，与 docs 共用编辑与权限。
- **F5.4 渠道接入** — 已有的 F3 微信 / F4 Nostr 成为 workspace 的入站/出站边，不新建渠道。

**M6 — Workspace 扩展面（等真实需求，不排期）**

- Calls（录音转写）、Email（Gmail OAuth）、CRDT 协作编辑、桌面壳。
- **明确不排期的理由**：这三样各自是一个季度的工作量，且都**不是社区场景的第一需求**。
  没有真实社区在用之前做它们，会把形状猜错 —— 和 M3 不排期是同一条理由。

**「定制部署」放在哪？** 不单独立里程碑，作为 **M5 的交付条件**：
Cos72 上线的定义就是「一条 `agent24 os install cos72 && os activate cos72` + 一个二进制 + 一个 SQLite
能让一个真实社区跑起来」（ADR-029 已经定义了这条流水）。**这正是 §8 说的、Macro 放弃的那一格。**

## 13. 一句话总结

> **Macro 用 191 个 crate 证明了：统一工作区的价值不在功能数量，在于一个 `Entity` 地址、
> 一套权限凭证、一张双向图。**
> 我们抄不了它的代码（AGPL），也不该抄它的规模（41 个 Pulumi stack）。
> **该抄的是三样：类型级权限凭证（§11.2，现在就能用）、统一实体与查询面（M4）、
> 以及它那条"每个 crate 都是六边形、层与层之间由 cargo feature 强制"的纪律（§11.1）。**
>
> 而我们比它强的地方要守住：**它的记忆是一段会过期的字符串，我们的是可重放、可审计、按空间隔离的底座。**
> Cos72 workspace 的意义，是终于给那个底座一件真实的工作做。

---

## 附：核对清单（本笔记的事实来源）

| 断言 | 出处 |
|---|---|
| AGPL-3.0 | `LICENSE.txt:1` |
| 191 crates / 47 services / 41 stacks | `ls` 计数 @ `c5e609e13` |
| 六边形三段式 | `crates/{entity_access,soup,skills,connection}/src/lib.rs` |
| `SoupItem` 11 类 | `crates/models_soup/src/item.rs:27` |
| `EntityAccessReceipt<T>` | `crates/entity_access/src/domain/models.rs:435` |
| Soup 定位 | `crates/soup/src/lib.rs:2` |
| `pub type Memory = String` | `crates/memory/src/domain/ports.rs:23` |
| 记忆生成 + 裁判双模型 | `crates/memory/src/domain/service.rs:11,12` |
| ACP 外层信封 | `crates/agent_runtime_protocol/src/lib.rs:4` |
| Skill = markdown 文档子类型 | `crates/skills/src/lib.rs:4` |
| 沙箱化 coding agent | `crates/agent_harness/src/lib.rs:1` |
| Pulumi / AWS us-east-1 | `infra/README.md:8` |
| 必须 Nix | `docs/RUNNING_LOCALLY.md:15` |
| Loro CRDT | `packages/loro-mirror/package.json` |
| SolidJS + Tauri 2 | `apps/web/package.json`、`apps/web/tauri/src-tauri/` |
