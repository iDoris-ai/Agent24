# SPEC-MD-ME — M-D 记忆 + M-E 模块生态 实现蓝图

> 权威依据:[ADR-028](../decision.md)(记忆:权威+投影)· [ADR-029](../decision.md)(内核↔领域 OS 边界)· 研究报告 v2(`agent24-memory-research/RESEARCH-REPORT-v2.md`,18 仓库读源码 + 10 篇 blog + MemPalace)· Codex 对抗式复审收口。
>
> **定位澄清(关键)**:M-D 建的是**通用 agent 的记忆底座**,**不是把某个个人知识库(如 MemPalace)搬进内核**。方法是**借各家最佳实现之长**,能直接复用的 Rust 模块(如 codex 的 trait 形状)就复用。
>
> **状态**:🟢 MD-1 spike 已交付并冻结签名(见 §2.1);MD-2a/2b 权威层已合并。MD-1c 后向量实现/SQLite DDL 仍按各自 MD-x 落。本文钉死:设计原则、数据结构形状、trait 契约、to-do+测试+验收、借鉴映射、技术标准。
>
> **进度**:✅ MD-1a 条件器 · ✅ MD-1b 崩溃重放 · ✅ MD-1c LongMemEval 装载 · ✅ MD-2a EventStore · ✅ MD-2b ArtifactStore · ✅ MD-2c 双谱系对账 ·（下一步:MD-3 AssertionStore 双时相+Retriever)。

---

## 0. 设计原则与技术标准(钉死,不冻结实现细节)

### 0.1 记忆设计原则

1. **本地优先、无强制外部服务**:SQLite(sqlx)为基线;向量为**可选**本地索引;嵌入默认本地 oMLX(可插拔 `Embedder`)。**不**强制 Neo4j / 云向量服务。
2. **权威 + 投影**(替代"层门面"):三个**持久权威**——`EventLog`(不可变事件)、`ArtifactStore`(用户/agent 可编辑 markdown + 知识,CAS 版本)、`AssertionLedger`(不可变语义断言,链证据,双时相)——加**可重建投影**(prompt 视图 / 摘要 / FTS / 向量 / KG 索引),每个投影带 `generation`/`checkpoint`、可从命名权威 `rebuild`。
3. **权威按数据产品分,不全局**:EventLog 权威于情景;AssertionLedger 权威于语义;markdown(ArtifactStore)只权威于用户创作的 core/知识;FTS/向量表是可弃投影。`memory rebuild` 明确能/不能恢复什么。
4. **真双时相**:断言带 **valid-time(`valid_from/valid_to`)+ recorded-time(`recorded_from/recorded_to`)两个区间**;更新=新断言版本 + 旧的置 recorded/valid 结束,**绝不删证据**。支持 `as_of(valid_at, recorded_at)` 查询。
5. **治理写门**:LLM 输出=**提案**;闭 schema 校验 + 确定性策略决定是否持久;**未确认候选不进默认召回**;强制非空 `owner/tenant`;origin/trust 标签;PII/secret 分类;注入隔离;全审计。
6. **Condenser = 视图增量,绝不删原始**:压缩产出 `Condensation{forgotten_event_ids, summary, offset}` 追加事件,重放时隐藏被压缩事件;投影返回**带 source event IDs + 理由/分数 + 安全标签 + 预算**的 typed fragments。
7. **三根正交轴分别打标**(别混):① 语义密度(L0 raw→L3 persona)② 生命周期(短/中/长)③ 任务视野(H1 上下文内 / H2 跨上下文 / H3 跨任务)。每个 store 三轴各打一标。
8. **Compress / Select / Discard 三操作**,Discard(遗忘/退休)一等公民,不是只有 Select。
9. **巩固(consolidation)循环**是重点投入:后台"睡眠"合成——读未巩固事件、写跨记忆 insight、更新 persona 层;带 `importance` + `consolidated` 标记。("记忆天花板不是向量库,是巩固机制"。)

### 0.2 Rust / 工程技术标准(全仓已有,记忆/模块 crate 遵守)

- edition 2024;`forbid(unsafe_code)`;workspace lints:`clippy -D warnings` + `unwrap_used`/`expect_used` = deny。
- **宽容 serde**(抗 schema 漂移,借 goose):LLM 产出的结构体用 `#[serde(default)]` + 容错反序列化,一个坏字段不炸全局。
- trait **对象安全**(需 `Box<dyn>` 的用之)+ `async_trait`(或 RPITIT,MD-1 定);错误用 `thiserror`;取消用 `CancellationToken`。
- 持久化 sqlx SQLite;`BEGIN IMMEDIATE` 写事务(沿用 sin90-store 惯例);迁移经 `sqlx::migrate!`(**改已应用迁移会破 checksum,新增只 append**)。
- 跨语言/对外类型经 `agent24-protocol`(serde + schemars)→ openapi/events schema 单一来源,CI 零漂移门。
- **路径安全**(借 codex):任何文件/目录记忆拒绝父路径穿越、隐藏组件、符号链接跟随。
- **崩溃一致性**:事件 id 作幂等键;投影 checkpoint;人/agent 编辑用 CAS 版本;确定性 replay/rebuild 测试。

### 0.3 评测标准(CI 回归门)

- 接 **LongMemEval + LoCoMo** 语料(MemPalace 已带 runner,可直接借其数据装载)。记忆改动**不得回归** raw-recall 基线。
- MD-1 起建**可回放语料**:显式召回 / 纠正 / 矛盾 / 迟到事实 / 多用户隔离 / 投毒源 / 删除 / 重启重建 / 长程压缩。Writer 与 Retriever **各自独立**过门。

### 0.4 借鉴映射(哪家学哪块,全部读过源码)

| 能力 | 学谁 | 具体 |
|---|---|---|
| Rust trait 形状(可直接复用) | **codex** | `MemoriesBackend`(add/list/read/search)、`ThreadStore`(带 `Unsupported` 默认的能力探测)、路径/符号链接护栏、no-op trace 句柄零开销 |
| 压缩 trait 拆分 | **goose** | `CompactionModel` / `TokenEstimator` trait、宽容 serde `StructuredSummary`、**hide-not-delete** 可见性元数据、渐进式工具响应削减 |
| Condenser 范式 | **OpenHands** | `Condenser` + `PipelineCondenser` + `Condensation` **view-delta** 事件(隐藏非删除),`View.from_events` 重放 |
| 文件真源对账 | **basic-memory** | 双谱系版本账本(db/file checksum)、CAS 跳过陈旧计划、**checksum 移动检测保 id** |
| 真双时相 | **graphiti** | 两区间 + `reference_time`、就地失效不删、证据双向 UUID 链 |
| 治理写门 | **Dense-Mem** | "输出=提案"、闭 schema 校验、append-only 证据、候选隔离召回 |
| 用户画像/巩固 | **memobase / MemoryScope / Google** | buffer→flush→槽合并;定时 observation→insight;后台 consolidation + importance |
| 分层指令记忆 | **gemini-cli** | 层级 `*.md` 合并 + **审核门控 auto-memory inbox**(从不自动应用) |
| 排序压缩上下文 | **aider** | PageRank 个性化先验 + 预算二分 render + 优雅降级 |
| 长任务符号轨迹 | **TencentDB** | Mermaid 符号图 + `node_id` 下钻,全量日志落 `refs/*.md` |
| 幂等/provenance 管线 | **cognee** | 确定性命名 id、run-id、rollback、先 flush 后完成 |

---

## 1. M-D 数据结构(权威形状;字段名/类型待 MD-1 微调)

```rust
// ---- 作用域(强制 owner;能力受限 handle 的过滤依据)----
struct Scope { owner: String, tenant: Option<String>,   // owner 强制非空
               agent: Option<String>, session: Option<String>, run: Option<String> }
enum Visibility { Private, Project, Public }
struct Origin { source: String, trust: Trust }           // Trust: UserSaid|ToolOutput|WebFetch|Model|System
enum Sensitivity { Normal, Pii, Secret }

// ---- L2 情景:不可变事件日志(权威)----
struct MemEvent { id: EventId, scope: Scope, kind: String, body: Value,
                  origin: Origin, at: String, causal: Vec<EventId> }   // id 即幂等键

// ---- 上下文投影(Condenser 产物;绝不删原始)----
struct ContextFragment { text: String, source_events: Vec<EventId>,
                         reason: String, score: f32, security: Sensitivity }
struct ContextProjection { fragments: Vec<ContextFragment>,
                           tokens_exact: usize, tokens_estimated: usize,
                           checkpoint: Option<CheckpointId>, transform_lineage: Vec<String> }
struct Condensation { forgotten_event_ids: Vec<EventId>, summary: String,
                      offset: usize, checkpoint: CheckpointId }        // 追加为事件,隐藏非删

// ---- L1 工作/Core:CAS + markdown 权威(ArtifactStore)----
struct Artifact { path: String, body: String, version: u64,
                  db_checksum: String, file_checksum: String,          // 双谱系(basic-memory)
                  scope: Scope, updated_by: String, reason: String }

// ---- L3 语义:不可变双时相断言(AssertionLedger)----
struct Assertion { id: AssertionId, scope: Scope, subject: String, predicate: String, object: Value,
                   valid_from: String, valid_to: Option<String>,       // 领域有效时间
                   recorded_from: String, recorded_to: Option<String>,// 记录/事务时间(真双时相)
                   evidence: Vec<EventId>, confidence: f32, modality: Modality, // Said|Observed|Derived
                   speaker: Option<String>, writer_version: String,
                   supersedes: Option<AssertionId>, qualified: bool }  // qualified=false 不进默认召回

// ---- 写门(候选管线,不 mutate)----
enum WriteDecision { Commit(Assertion), Reject{reason:String}, NeedsApproval(CandidateId) }

// ---- 嵌入(可复现索引)----
struct Embedding { model_id: String, revision: String, dims: u32, normalized: bool, vector: Vec<f32> }
struct ProjectionCheckpoint { name: String, generation: u64, up_to_event: EventId }
```

SQLite schema(草案,MD-1 定 DDL):`mem_events(seq PK, id UNIQUE, scope_owner, …, payload, at)` · `mem_artifacts(path PK, version, db_checksum, file_checksum, …)` · `mem_assertions(id PK, …, valid_from, valid_to, recorded_from, recorded_to, evidence_json, confidence, qualified, supersedes)` · `mem_candidates(…, status)` · `mem_projection_ckpt(name PK, generation, up_to_event)` · `mem_embeddings(assertion_id, model_id, revision, dims, vec BLOB)` · `mem_consolidations(id, insight, importance, source_events_json)`。

---

## 2. M-D trait 缝(先在一个 `agent24-memory` crate 内定;别过早拆 crate)

```rust
trait EventStore   { async fn append(&self, e: MemEvent)->R<EventId>; async fn scan(&self, q: EventQuery)->R<Vec<MemEvent>>; async fn checkpoint(&self, name:&str)->R<CheckpointId>; }
trait ArtifactStore{ async fn read(&self, path:&str, scope:&Scope)->R<Option<Artifact>>; async fn cas_write(&self, a: Artifact, expect_version:u64)->R<Artifact>; async fn history(&self, path:&str)->R<Vec<Artifact>>; }
trait AssertionStore{ async fn assert(&self, a: Assertion)->R<AssertionId>; async fn retract(&self, id:AssertionId, at:&str)->R<()>; async fn beliefs_as_of(&self, q: BeliefQuery)->R<Vec<Assertion>>; } // 默认 qualified=true only
trait Condenser    { async fn condense(&self, view:&[MemEvent], budget:usize)->R<ContextProjection>; }   // 组合:PipelineCondenser
trait MemoryWriter { async fn propose(&self, turn:&Turn, scope:&Scope)->R<Vec<WriteDecision>>; }         // candidate→validate→approve→commit
trait Retriever    { async fn search(&self, q:&str, scope:&Scope, budget:usize)->R<Vec<ContextFragment>>; } // FTS + 可选向量 + 排序
trait Embedder     { async fn embed(&self, text:&str)->R<Embedding>; }                                    // 默认 OmlxEmbedder;NoopEmbedder=纯 FTS
trait Consolidator { async fn run_once(&self, scope:&Scope)->R<usize>; }                                  // 后台巩固
trait ProjectionJob{ async fn run_from(&self, ckpt: CheckpointId)->R<ProjectionOutcome>; }               // 幂等重建
```

`MemoryStore` facade 组合各 trait;每 trait 后可换实现、可 `None`(未启用层)。**能力受限 handle**:`KernelCtx::memory(scope, grants)` 返回一个只在给定 scope/权限内可读写的 `ScopedMemory`,**不是** ambient `MemoryStore`(修 ADR-029 的洞)。

---

## 2.1 已冻结签名(MD-1 spike 出口 · 实现见 `rust/crates/agent24-memory`)

> MD-1 spike(MD-1a 条件器 + MD-1b 崩溃重放 + MD-1c LongMemEval 装载)已合并并经外部对抗式复审多轮变异实测。以下签名**已冻结**——后续 MD-x 在其上加层,不重塑这些形状;改动需新 ADR/复审。冻结的是**形状与不变式**,SQLite DDL 与向量实现仍按原计划在各自 MD-x 落。

**`condenser`**(MD-1a):`Condenser::condense(&self, history: &[Msg], budget_tokens: usize) -> Result<ContextProjection, String>`。`ContextProjection { fragments: Vec<ContextFragment>, folded: Vec<usize>, tokens_estimated }`;`ContextFragment { msg, source: Vec<usize>, reason }`。**不变式**:每个源下标恰好一次落在 `fragments[*].source ∪ folded`(`covers(n)` 记账,非语义);预算是 **best-effort**,两处允许超支(tool-safe 不可拆分对、summary 固定开销);最新消息永不被折走。`TokenEstimator` 缝可换真 tokenizer。

**`event`**(MD-2a):`EventStore::{append(&MemEvent)->i64, scan(&EventQuery)->Vec<StoredEvent>, checkpoint_at(name,seq), checkpoint(name)->i64, checkpoint_seq(name)}`。`append` 幂等于 `id`;跨租户/异 payload 撞 id → `Conflict`。`Scope.owner` 强制非空;`Trust` 含 `Unknown`(最严,未识别值不降级)。`scan` 恒绑 owner + 恒加 LIMIT。

**`artifact`**(MD-2b):`ArtifactStore::{read(path, owner:&str)->Option<Artifact>, cas_write(Artifact, expect_version)->Artifact, history(path, owner:&str)->Vec<Artifact>}`。身份是 `(owner, path)`,narrowing 非隔离;CAS 陈旧写 → `Conflict`(`BEGIN IMMEDIATE` 串行化,输家拿干净 `Conflict` 非裸锁错);每版本留存,双谱系 `db_checksum`/`file_checksum` 待 MD-2c 对账发散。

**`replay`**(MD-1b):`replay_history(&EventLog, &EventQuery) -> Result<Replayed>`(分页到底,不砍最新)+ `replay_history_lenient(...) -> (Replayed, Vec<SkippedEvent>)`(坏行跳过并上报,带 id+seq)。`Replayed { messages, provenance: Vec<Provenance{event_id, trust}>, last_seq }`——`messages` 与 `provenance` 位置对齐、一趟产出,**trust 溯源随重放保留**(MD-4 写门可用)。owner-only 重放合并所有 session,要隔离传 `.session(s)`。

**`eval`**(MD-1c):`parse_cases`/`load_cases_from_file`/`ingest_case`/`run_case` + `EvalOutcome`。LongMemEval 装载跑通;`answer_in_view` **按答案 turn 的持久 event id 在重放 `provenance` 中定位下标、再判 ∈ `fragments[*].source`**——**不用子串**(否则子串重合会**高报**),也**不用 case 内 flat 下标**(`run_case` 重放整个 owner 历史,同 owner 多 case 会错位,故按 event id 定位,无「一 owner 一 case」前提)。近期窗口对深答案 `answer_in_view=false` 是**基线**(MD-3 retriever 要超越的数,宁低报不高报),`lossless` 恒真。**边界(诚实标注)**:投毒排除属 MD-4 写门,深召回属 MD-3;二者在 MD-1 只钉边界不实现。

---

## 3. M-D to-do / 测试 / 验收

> 分期跟消费者走;每条独立可发。"验收"= 该条合并的硬门槛。

| ID | 交付 | 依赖 | 测试 | 验收 |
|---|---|---|---|---|
| **MD-1** ✅ | **评测/恢复 spike**:两个 `Condenser`(确定性 recent-window + 保留尾部 summary,发 `Condensation` view-delta,**不删原始**);建可回放语料 + benchmark 装载 | D1 | 崩溃/重启/幂等重放;语料测 token 预算/关键事实保留/因果/投毒排除/跨 scope 泄漏;LongMemEval 装载跑通 | ✅ 已交付(MD-1a #113 + MD-1b #116 + MD-1c):session 测试全绿 + 上述全过 + **签名已冻结(§2.1)** |
| **MD-2** ✅ | **EventStore + ArtifactStore**(权威层):事件表 + markdown-CAS + 双谱系对账(checksum 移动检测) | MD-1 | 事件 append/scan/checkpoint 幂等;CAS 拒陈旧写;外部改文件→对账不静默删;rebuild 从事件重建投影 | ✅ 2a EventStore(#114)· ✅ 2b ArtifactStore(#115)· ✅ 2c 对账(`reconcile` 四类状态 + checksum 移动检测 + **无静默删** + 确定性 + path-safe `observe_dir`);rebuild-from-events 见 `replay`(MD-1b) |
| **MD-3** | **AssertionStore 双时相 + Retriever(FTS)**:断言表两区间 + 证据链 + `qualified` 门;FTS 检索 + scope 隔离 | MD-2 | 写-查-失效-`as_of(valid,recorded)` 回看;矛盾=新版本非删;候选不进默认召回;scope 泄漏 0 | 双时相四象限查询正确 + 跨 scope 零泄漏 |
| **MD-4** | **MemoryWriter 写门(治理)**:candidate→闭 schema 校验→确定性策略→approve/commit;强制 owner;origin/trust;审计 | MD-3 | 恶意 ToolOutput/WebFetch 默认不落持久;UserSaid+显式 remember 才自动 commit;dry-run/review;bulk rollback | 投毒语料:未确认候选不进召回;审计可回放 |
| **MD-5** | **Consolidator 巩固循环**:后台读未巩固事件→写 insight→更新 persona;importance/consolidated 标记 | MD-3 | 巩固幂等;importance 排序;增量==全量重跑 | LongMemEval/LoCoMo 相对纯检索有提升(对照) |
| **MD-6** | **Retriever 本地向量(可选)**:`OmlxEmbedder` + SQLite 向量 + 双索引迁移 + FTS 兜底;`Embedding{model_id,revision,dims}` | MD-3, D4b | 换模型触发 reindex 状态机;可续重嵌;混版本行为 | 语义召回优于纯 FTS 对照 + reindex 不丢 |
| **MD-7** | **知识/指令层(L4)**:层级 markdown(CLAUDE.md 式)合并 + 触发注入 + **审核门控 auto-memory inbox**(gemini-cli) | MD-2 | 层级合并优先级;触发命中;auto-memory 从不自动应用 | 层级覆盖正确 + inbox 需人批 |
| **MD-8** | **长任务符号轨迹(H1/H2)**:全量工具日志落 `refs/*.md`,留符号图 + `node_id` 下钻(TencentDB) | MD-2 | 符号图可下钻回原文;压缩可恢复(非截断) | 轨迹压缩率 + 100% 可恢复 |
| **MD-X** | **crate 拆分**:`agent24-memory` → `memory-{core,episodic,semantic,knowledge}` + facade——**仅当依赖/发布边界被证明**(Codex 收口:先模块后 crate) | MD-2..7 | 编译/依赖图无环 | 有真实边界才拆,否则不拆 |

---

## 4. M-E 数据结构 + 契约(领域 OS,ADR-029)

```rust
// domain-os.yml 反序列化(清单)
struct DomainOsManifest {
    name: String, version: String,
    route_namespace: String,          // "/api/v1/<name>"
    event_module: String,             // EventBody::Module 的 module 名
    data_dir: String,                 // ~/.agent24/os/<name>/
    requires_models: Vec<String>,     // 需要的本地模型
    requires_apis: Vec<String>,       // 需要的外部 API/密钥
    requires_deps: Vec<String>,
    kernel_capabilities: Vec<Cap>,    // model|scheduler|policy|memory(scoped)
    ui_entry: Option<String>,
    impl_kind: ImplKind,              // InProcessCrate | OutOfProcessProvider
}
// 领域 OS 实现(进程内)
trait DomainModule {
    fn name(&self) -> &str;
    fn manifest(&self) -> &DomainOsManifest;
    async fn open_store(&self, dir: &Path) -> R<()>;        // 自己的 DB + 迁移
    fn routes(&self, ctx: KernelCtx) -> axum::Router;       // 自己的命名空间
    fn event_module(&self) -> &str;
}
// 内核能力(单向;能力受限,含 scoped memory handle)
trait KernelCtx {
    fn models(&self) -> ModelHandle;
    fn scheduler(&self) -> SchedulerHandle;
    fn policy(&self) -> PolicyHandle;
    fn memory(&self, scope: Scope, grants: Grants) -> ScopedMemory;   // 非 ambient
    fn events(&self) -> EventSink;                                     // 只能发自己 module 的
}
// 注册表配置(~/.agent24/config)
struct DomainOsConfig { active_domain_os: String, installed: Vec<String> }
```

安装生命周期(CLI `agent24 os …`):`install`(校验+按需下模型+查依赖+建目录+跑迁移)· `activate`(翻 `active_domain_os` + 重启)· `deactivate` · `uninstall`(清 `~/.agent24/os/<name>/`,可回退)。

---

## 5. M-E to-do / 测试 / 验收

| ID | 交付 | 依赖 | 测试 | 验收 |
|---|---|---|---|---|
| **ME-1** | **`DomainModule` + `KernelCtx` trait**;把 **Sin90 改造成第一个 `DomainModule`**,去掉 `AppState.sin90` 具体字段 + 硬编码 `/sin90/*` 路由 | — | Sin90 经 trait 挂载后 SPIKE-00 全测试仍绿;内核零依赖 sin90 不变 | agent24d 不再按名字认识 Sin90;`cargo` 图仍单向 |
| **ME-2** | **配置驱动模块注册表 + `agent24 os` CLI**(install/activate/uninstall);独立 `~/.agent24/os/<name>/`;缺资源明确报错 | ME-1 | 装/激活/卸载往返;缺模型→明确报缺;禁用后 `/…/*` 503 | 换装是纯配置+脚本,不改内核不重编 |
| **ME-3** | **进程外 Provider 路径**(第三方 OS,经 MCP/协议;不重编内核) | ME-2 | 一个 mock Provider OS 挂载 + 路由代理 + 事件转发 | 装第三方 OS 零改内核 |
| **ME-4** | **第二个领域 OS 样例(Cos72 骨架)** 证明可替换;`os install cos72 && os activate cos72` | ME-2 | 清 Sin90→装 Cos72→各自 DB 隔离、路由切换 | 三玩法(默认/定制/替换)都是干净一次性动作 |
| **ME-5** | **E5 PGL manifest** 解析钩子 + AgentStore 元数据展示 | ME-2 + 消费者 | pgl.yml 解析 + 展示 | **等 AgentStore 真消费**(先有消费者) |
| **ME-6** | **模块签名 + AirAccount 信任根**(ADR-016 阶段3):sigstore keyless 签名 + 验签 + "只信任 AirAccount X" | ME-2 | 签名验证 + 信任策略 | **P4 门后**(需用户确认) |

---

## 6. 顺序 / 里程碑门

1. **M-D 先于 M-E**(用户定)。M-D 内 **MD-1 spike 先行**,过了才冻结 trait、才决定 MD-X 拆 crate。
2. M-E 的 **ME-1(Sin90→DomainModule)** 是"可替换领域 OS"的地基,收口 #103/#104 推迟的模块挂载 seam。
3. **P4 门**(ME-6 签名 + 信任根、跨用户分发)进前停下用户确认。
4. 每层每模块**跟消费者走**,不按编号硬推。

## 7. 技术规范汇总(标准清单,逐条钉死)

- **存储**:SQLite + sqlx;`BEGIN IMMEDIATE`;迁移只 append(checksum 约束);领域 OS 各自 DB。
- **权威/投影**:任何投影带 `generation/checkpoint`、可 `rebuild`;权威按数据产品分。
- **时间**:断言双区间 + `as_of`;失效非删除。
- **治理**:强制 owner;写=提案+校验+审计;候选隔离;delete≠invalidate;PII/secret 分类。
- **上下文**:Condenser view-delta 不删原始;投影带 source ids + 安全标签 + 预算。
- **嵌入**:`{model_id,revision,dims,normalized}`;换模型走 reindex 状态机;FTS 兜底;oMLX 是 adapter 非默认锁定。
- **序列化**:LLM 产出宽容 serde;对外类型经 protocol 单一来源 + 零漂移门。
- **安全**:路径穿越/符号链接护栏;能力受限 handle 非 ambient。
- **一致性**:事件 id 幂等键;CAS 版本;确定性 replay/rebuild 测试。
- **评测门**:LongMemEval + LoCoMo 进 CI,不得回归;Writer/Retriever 各自过门。
- **Rust**:edition 2024、forbid unsafe、clippy -D warnings、unwrap/expect deny。

---

*本文与 [SPEC-MEMORY.md](SPEC-MEMORY.md) 的关系:SPEC-MEMORY 是 ADR-028 的早期分层草案,本文是经研究 v2 + Codex 收口后的**完整实现蓝图并覆盖 M-E**;若冲突以本文为准。MD-1 spike 后回填冻结的签名与 DDL。*
