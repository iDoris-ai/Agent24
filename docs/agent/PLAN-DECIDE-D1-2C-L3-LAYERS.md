# D1-2c — L3 逐层记录（model ref / answers / 耗时）与日志 `layers[]` 契约提案

状态：**纯文档提案，待外部 PR-Daemon 审查**；以下所有选项均为推荐方案，不是已批准政策，本文不实现、也不授权任何生产代码或测试。
基线：`ab/decide@4acdc3959c580b87485f137b32d65bfe477489e6`（含 L1 实现 [#908](https://github.com/iDoris-ai/Agent24/pull/908)、L2 契约 [#909](https://github.com/iDoris-ai/Agent24/pull/909)）。
依据：[总计划](PLAN-DECIDE.md) §2.1 与「D1 前置」L3；[D1 计划](PLAN-DECIDE-D1.md) D1-2 行；格式与严格度基准为 [L1 契约](PLAN-DECIDE-D1-2A-L1-UNAVAILABLE.md) 与 [L2 契约](PLAN-DECIDE-D1-2B-L2-VALIDATION.md)。
L2 实现 [#1033](https://github.com/iDoris-ai/Agent24/pull/1033) 撰写时为 OPEN（head `66472b89`，未合入）；本文按 #909 契约形状（`decide -> Result<Decision, InvalidRequest>`、`Outcome::InvalidResponse`）书写，L3 实现必须在 L2 实现合入之后开工，若 #1033 最终形状偏离 #909，先回到本文修订。

## 1. 现状（实际代码，非推测）

| 位置 | 事实 |
|---|---|
| `src/types.rs:121` | `ModelRef { id: String, revision: String }`，derive `Serialize + Deserialize`；本片不改其形状 |
| `src/types.rs:248` | `Decision { answers, backend, model: Option<ModelRef>, latency_ms, outcome }`，只 `Serialize`，只承载最后一个决定性层 |
| `src/service.rs:51/66/81` | `Decided`、`Unavailable`、耗尽 `Abstain` 三处 `model: None` 硬编码（#1033 再加一处 `InvalidResponse`，同样 `None`） |
| `src/service.rs:36,100` | 一个 `Instant` 计总耗时；`elapsed_ms` = `as_millis()` 截断，溢出饱和为 `u64::MAX`；没有逐层耗时 |
| `src/backend.rs:39` | `trait DecisionBackend { fn kind(&self) -> BackendKind; async fn evaluate(..) -> BackendOutcome; }`，无任何上报模型身份的途径 |
| `src/log.rs:47` | `LoggedLayer { backend, model: Option<ModelRef>, label: Option<String>, p: Option<f32>, latency_ms: u64 }`，`LogEntry.layers: Vec<LoggedLayer>`，`DECISION_LOG_SCHEMA_VERSION = 1` |
| 存储 / 导出 | `agent24-store` 的 `layers` 列是不透明 JSON 文本（migration `0014`）；`agent24-cli` export 只 `parse` 后原样输出；均不依赖 `LoggedLayer` 类型 |
| 实现者 / 调用者 | 仓内 `impl DecisionBackend` 只有 `RuleBackend` 与 `service.rs` 测试 fake；`DecisionService::new/decide` 只在本 crate 测试中被调用；无生产 `DecisionLog` writer |

## 2. 契约决定

### 2.1 后端上报 `ModelRef`（向后兼容）

给 trait 增加一个**带默认实现的同步方法**，不改 `evaluate` 签名、不改 `BackendOutcome`：

```rust
pub trait DecisionBackend: Send + Sync {
    fn kind(&self) -> BackendKind;
    /// 本配置项实际使用的模型身份；无模型（规则）或未知时为 None。
    fn model(&self) -> Option<ModelRef> { None }
    async fn evaluate(&self, request: &DecisionRequest) -> BackendOutcome;
}
```

- 源码兼容：`RuleBackend`、全部现有测试 fake 及任何仓外实现不改一行即可编译；未覆盖者等价于今天的 `None`。
- 调用时机与次数：每个**实际尝试**的阶段，`kind()` 一次 → `evaluate()` 一次 → `model()` 一次（在 `evaluate` 返回之后，以便懒加载后端报告真正跑过的模型）。终止阶段之后的配置项三者调用均为 0；非法请求（L2 `Err`）时所有后端三者调用均为 0。
- 校验：`Some(m)` 要求 `m.id.trim()` 与 `m.revision.trim()` 均非空，否则该阶段以稳定 code `invalid_model_ref` 终止为 `Outcome::InvalidResponse`（见 §2.4）。只做非空检查：不裁剪、不改写、不查模型目录、不校验 sha256/HF 仓库（那是 D1-3 目录职责）；匹配区分大小写与 Unicode 字节。
- 不限制种类组合：`Rule` 报 `Some`、`Encoder` 报 `None`、`LlmSimulation` 报 `Some` 均合法；不从 `kind`、显示名或目录推断模型。
- 否决方案：把 `model` 放进 `BackendOutcome::Decided/NoConclusion`（破坏所有实现与匹配）；在 service 构造时传入 `Vec<(backend, ModelRef)>`（与后端实际加载的模型可能不一致）。

### 2.2 `Decision.model` 的取值

`Decision.model` 永远描述与 `Decision.backend` **同一个阶段**，不再硬编码：

| 结局 | `backend`（沿用 L1/L2，不变） | `model`（新） |
|---|---|---|
| `Decided` | 决定层 kind | 决定层 `model()` |
| 有旧有效非空 floor 的 `Unavailable` / `InvalidResponse` / `Abstain` | floor 来源层 kind | floor 来源层 `model()` |
| 无 floor 的 `Unavailable` / 因答案非法的 `InvalidResponse` | 失败层 kind | 失败层 `model()`（已通过校验） |
| 因 `invalid_model_ref` 的 `InvalidResponse` 且无 floor | 失败层 kind | `None`（非法引用不外泄） |
| 无 floor 的耗尽 `Abstain`（含零后端） | 占位 `rule` | `None`（占位不是真实阶段） |

### 2.3 逐层轨迹：新增 `decide_traced`，`Decision` 形状不变

```rust
impl DecisionService {
    pub async fn decide_traced(&self, request: &DecisionRequest) -> Result<TracedDecision, InvalidRequest>;
    /// 等价于 decide_traced(..).map(|t| t.decision)；调用次数与结果完全相同。
    pub async fn decide(&self, request: &DecisionRequest) -> Result<Decision, InvalidRequest>;
}

#[derive(Debug, Clone, Serialize)]          // 只 Serialize，不加 Deserialize
pub struct TracedDecision {
    pub decision: Decision,
    pub layers: Vec<StageRecord>,
    question_count: usize,                   // 私有，不序列化；供 §2.6 映射检查
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StageRecord {                    // 字段即序列化顺序
    pub stage_index: usize,                  // L1 的配置 vector 零基下标
    pub backend: BackendKind,                // 该阶段 kind()
    pub model: Option<ModelRef>,             // 该阶段 model()；invalid_model_ref 时为 None
    pub outcome: StageOutcome,
    pub answers: Vec<Answer>,                // 已 normalized_for(kind) 的答案
    pub latency_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StageOutcome {
    Decided,
    NoConclusion,
    Unavailable { reason: String },
    InvalidResponse { code: String },
}
```

`TracedDecision`、`StageRecord`、`StageOutcome` 从 crate 根 re-export。`question_count` 私有，故仓外无法用结构体字面量伪造轨迹。

- `layers` 严格按尝试顺序，每个实际尝试的阶段**恰好一条**，不合并、不去重、不跳过空 floor：`layers[i].stage_index == i`。
- 长度：`Decided` 于阶段 k → `k+1`；`Unavailable`/`InvalidResponse` → `failed_stage_index + 1`；耗尽 `Abstain` → 后端总数（零后端为 `[]`）；`Err(InvalidRequest)` → 无轨迹。
- `answers`：`Decided` 为该层通过 L2 校验并归一化后的全部答案（原顺序）；`NoConclusion` 为其 floor（空 floor 记 `[]`，非空 floor 归一化后原样记录——即使之后被更新的 floor 取代，本条仍保留该层自己的 floor）；`Unavailable` 与 `InvalidResponse` 恒为 `[]`——畸形答案集整体丢弃，不记录、不抢救其中合法项（与 L2 一致）。
- `LlmSimulation` 的答案在轨迹中同样 `calibrated=false`（与 `Decision.answers` 相同的归一化结果）。
- 否决方案：给 `Decision` 直接加 `layers` 字段——改变所有 `Decision` JSON 字节与结构体字面量，违背 §2.7 承诺。

### 2.4 与 L1 `failed_*`、L2 `InvalidResponse` 的共存

- 终止阶段的轨迹条目与结局一一对应：`layers.last().stage_index == failed_stage_index`、`layers.last().backend == failed_backend_kind`；`StageOutcome::Unavailable.reason` 与 `Outcome::Unavailable.reason` 逐字相同；`StageOutcome::InvalidResponse.code` 与 `Outcome::InvalidResponse.code` 逐字相同。轨迹不替代、不改写 L1/L2 字段。
- 单阶段处理顺序（固定）：① `kind()` ② 计时并 `evaluate()` ③ `model()` ④ 若 `model()` 非法 → `invalid_model_ref` 终止（**先于**对该层答案或 `Unavailable` 的解释，适用于全部三种 `BackendOutcome`）⑤ 按 L1/L2 现行规则处理该层结果（L2 答案校验顺序与 code 不变）。
- 理由：身份无法如实记录的阶段，其结论也不能如实入日志；把它判为“已运行但畸形”比判为不可用更准确。
- floor 规则不变：`invalid_model_ref` 层的 floor 同样整体丢弃；保留最近有效非空 floor；floor 永不升级为 `Decided`。L3 不新增结局种类，只为 L2 的 `InvalidResponse` 增加一个 code。

### 2.5 耗时测量口径

- 逐层：在该阶段调用 `evaluate()` 前即刻取 `Instant::now()`，在其 future 完成后即刻取差值；不含 `kind()`、`model()`、校验与归一化。换算沿用 `elapsed_ms`（`as_millis()` 向下截断，饱和 `u64::MAX`）。单调时钟，不用墙钟。
- 总计：`Decision.latency_ms` 口径不变（仍为 service 内总耗时，L2 下自请求校验通过后起算）。因阶段串行且每项向下截断，恒有 `Σ layers[i].latency_ms ≤ Decision.latency_ms`；这是可测的不变量，但测试不得断言任何具体毫秒数或阈值。
- 不引入可注入时钟；快照测试在序列化前把 `decision.latency_ms` 与每个 `layers[i].latency_ms` 置 0。

### 2.6 填充日志 `layers[]`（`LoggedLayer` v1 不变）

```rust
impl TracedDecision {
    pub fn logged_layers(&self) -> Result<Vec<LoggedLayer>, LayerLogError>;
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LayerLogError { pub code: String }   // 仅 Serialize，不加 Display/Error/Deserialize
```

- `LoggedLayer` 字段、顺序、derive 与 `DECISION_LOG_SCHEMA_VERSION = 1` 均不变；存储迁移、CLI export 不动。
- 映射：每个 `StageRecord` 恰好对应一个 `LoggedLayer`，同序同长：`backend`←`backend`，`model`←`model`，`latency_ms`←`latency_ms`；`label`/`p` 取自该层 `answers`：
  - `answers` 为空（空 floor、`Unavailable`、`InvalidResponse`）→ `label=None`、`p=None`；
  - 恰一个答案 → `p = answer.p()` 原样（`None` 保持 `None`，不伪造 0/1）；`label` 按值：`Label(s)` → `s` 原字符串；`Bool(b)` → `"true"`/`"false"`；`Score(x)` → `x` 的 Rust `f32` `Display`（最短可往返表示，如 `-1.0`→`"-1"`、`0.5`→`"0.5"`；L2 已保证有限）。
- v1 一层只有一个 `label`/`p` 槽位，因此 **`question_count != 1` 时整体返回 `Err(LayerLogError{code:"multi_question_unsupported"})`**，不截断、不只取第一题、不拼接。D1-4/D1-5 的 `retain.intent`、`recall.gate` 按单题请求接入；多题日志需要 schema v2，另立任务。
- 已知 v1 局限（不在本片修复）：`LoggedLayer` 不记录 `calibrated` 标志与阶段结局种类。`llm_simulation` 仍可凭 `backend` 识别；结局种类可由 `LogEntry.final` 与层序推得，不完整。消费者不得把日志中的 `p` 当作已校准概率，除非另有证据。
- 本片只产出 `Vec<LoggedLayer>`；`LogEntry` 的 `final`（依赖 L4 阈值）、`decision_id`/`ts`/`hw_tier` 的生成与 `DecisionLog::record` 的调用属于 D1-2d/D1-4/D1-5。

### 2.7 JSON 字节兼容承诺

- 在所有后端使用默认 `model()`（`None`）、`latency_ms` 固定为 0 时：`Decision` 的 `Decided`/`Abstain`/`Unavailable`/`InvalidResponse` 四种 JSON 与 L2 实现合入后的基线**逐字节相同**。
- 后端上报 `ModelRef` 时，`"model":null` 变为 `{"id":..,"revision":..}`：字段已存在，只是值变化，这是本片的有意变更。
- 新增 `invalid_model_ref` 只是 `InvalidResponse.code` 的新取值，形状不变。
- `TracedDecision`/`StageRecord`/`LayerLogError` 是新形状，以 §3 fixtures 为准；`LoggedLayer`/`LogEntry`/`ModelRef`/`Answer`/请求/profile 的 JSON 及现有日志 fixtures 无 diff；不新增 `Deserialize`、不做日志迁移。实际耗时不承诺相同。
- 序列化 fixture 取出的是 `TracedDecision`/`Decision`/`LayerLogError` 本身，**不直接序列化 `Result`**（同 L2）。

### 2.8 稳定错误与边界清单

| 情形 | 结果 |
|---|---|
| `model()` 返回 `Some`，`id` 或 `revision` 为 `""`、`" "`、`"\t"` | 该阶段 `InvalidResponse{code:"invalid_model_ref"}`；轨迹该条 `model=null`、`answers=[]` |
| `model()` 合法但含前后空格（如 `" m"`） | 合法，原样保留，不裁剪 |
| 后端返回 `Unavailable` 且 `model()` 非法 | `invalid_model_ref` 优先（§2.4 ④） |
| 答案非法且 `model()` 非法 | `invalid_model_ref` 优先 |
| 合法 `model()` + 非法答案 | L2 原 code（如 `answer_coverage`），轨迹 `model` 为该合法引用 |
| `TracedDecision::logged_layers` 且 `question_count >= 2` | `Err(LayerLogError{code:"multi_question_unsupported"})` |
| 零后端单题请求 | `layers=[]`，`logged_layers()=Ok([])`，`Decision` 同旧 `Abstain`/`rule`/`model=null` |
| 非法请求 | `Err(InvalidRequest)` 不变，无轨迹，后端 `kind/model/evaluate` 调用均为 0 |

稳定 code 仅新增两个：`invalid_model_ref`（`Outcome::InvalidResponse` / `StageOutcome::InvalidResponse`）、`multi_question_unsupported`（`LayerLogError`）。L2 的 14 个 code 与其顺序不变。

## 3. 固定 fixtures 与验收矩阵

请求 Q1（单题）与答案 N；ModelRef `ME={"id":"m-enc","revision":"rev-1"}`、`MD={"id":"m-deep","revision":"rev-2"}`。数值仅为 fixture，不代表真实模型。

```json
{"point":"retain.intent","input":"","context":{},"questions":[{"kind":"noul","id":"n","prompt":""}],"side_effect":"reversible"}
```

N = `Answer::new("n", Bool(true), Some(0.75), <层 kind>, true)`；多题行改用 L2 契约的 Q（n/c/s 三题）。全部使用可报告任意 kind/model 并分别计数 `kind`/`model`/`evaluate` 调用的 fake backend；无模型文件、网络、硬件假设或计时阈值。

**F1**：stage0 Rule（model None）`NoConclusion{floor:[]}`；stage1 Encoder（ME）`Decided([N])`。

```json
{"decision":{"answers":[{"question_id":"n","value":{"type":"bool","value":true},"p":0.75,"calibrated":true}],"backend":"encoder","model":{"id":"m-enc","revision":"rev-1"},"latency_ms":0,"outcome":{"kind":"decided"}},"layers":[{"stage_index":0,"backend":"rule","model":null,"outcome":{"kind":"no_conclusion"},"answers":[],"latency_ms":0},{"stage_index":1,"backend":"encoder","model":{"id":"m-enc","revision":"rev-1"},"outcome":{"kind":"decided"},"answers":[{"question_id":"n","value":{"type":"bool","value":true},"p":0.75,"calibrated":true}],"latency_ms":0}]}
```

F1 的 `logged_layers()`：

```json
[{"backend":"rule","model":null,"label":null,"p":null,"latency_ms":0},{"backend":"encoder","model":{"id":"m-enc","revision":"rev-1"},"label":"true","p":0.75,"latency_ms":0}]
```

**F2**：stage0 Encoder（ME）`NoConclusion{floor:[N]}`；stage1 Deep（MD）`Unavailable{reason:"deep unavailable"}`；stage2 任意，调用 0。

```json
{"decision":{"answers":[{"question_id":"n","value":{"type":"bool","value":true},"p":0.75,"calibrated":true}],"backend":"encoder","model":{"id":"m-enc","revision":"rev-1"},"latency_ms":0,"outcome":{"kind":"unavailable","reason":"deep unavailable","failed_stage_index":1,"failed_backend_kind":"deep"}},"layers":[{"stage_index":0,"backend":"encoder","model":{"id":"m-enc","revision":"rev-1"},"outcome":{"kind":"no_conclusion"},"answers":[{"question_id":"n","value":{"type":"bool","value":true},"p":0.75,"calibrated":true}],"latency_ms":0},{"stage_index":1,"backend":"deep","model":{"id":"m-deep","revision":"rev-2"},"outcome":{"kind":"unavailable","reason":"deep unavailable"},"answers":[],"latency_ms":0}]}
```

**F3**：stage0 Encoder，model `{"id":"m-enc","revision":" "}`，`Decided([N])`；无旧 floor。

```json
{"decision":{"answers":[],"backend":"encoder","model":null,"latency_ms":0,"outcome":{"kind":"invalid_response","code":"invalid_model_ref","failed_stage_index":0,"failed_backend_kind":"encoder"}},"layers":[{"stage_index":0,"backend":"encoder","model":null,"outcome":{"kind":"invalid_response","code":"invalid_model_ref"},"answers":[],"latency_ms":0}]}
```

**F4**：`LayerLogError` 快照：`{"code":"multi_question_unsupported"}`。

| 输入 / 层序列 | 必须观察到的结果 |
|---|---|
| 全部后端默认 `model()`；Decided / Abstain / Unavailable / InvalidResponse 各一例（固定 latency） | `Decision` JSON 与 L2 合入后基线逐字节相同；`model` 均为 null |
| `RuleBackend` 命中 | 不改实现即编译；`decision.model=null`；`layers=[{rule,null,decided,[答案]}]` |
| F1 | 与 F1 快照及其 `logged_layers` 逐字节相同；stage1 的 `model()` 恰一次 |
| Decided 于 stage0，后接 2 个计数层 | `layers.len()=1`；后续层 kind/model/evaluate 调用 0 |
| F2 | 与 F2 快照相同；`Decision.model` 是 floor 来源 ME 而非失败层 MD；stage2 调用 0 |
| 无 floor，stage0 Deep（MD）Unavailable | `backend=deep`、`model=MD`、`layers.len()=1` |
| stage0 floor=[N]（ME），stage1 floor=[]，stage2 Encoder（ME）floor=[N]，stage3 Unavailable | 4 条，stage1 `answers=[]` 仍记录；`Decision.model` 指向 stage2 |
| 两个 Encoder 配置项报不同 ModelRef，第二个 Decided | `stage_index` 0/1 区分；`Decision.model` 为第二个的引用 |
| F3；revision 改为 `""`、`"\t"`；id 改为 `""`/`" "` | 每例 `invalid_model_ref`；有旧 floor 时保留旧 floor 与其 model |
| model 非法且后端 `Unavailable`；model 非法且答案非法 | 两例都是 `invalid_model_ref`，不是 Unavailable / L2 code |
| model 合法（含 `" m"` 这类前后空格）+ `Decided([])` | `answer_coverage`；轨迹 `model` 原样保留 `" m"` |
| L2 Q 的 stage2 Deep 畸形（L2 矩阵原行） | L2 原结局不变；`layers.len()=3`，最后一条 `invalid_response` 且 `answers=[]` |
| `LlmSimulation` 层谎报 Encoder、calibrated=true | `Decision.answers` 与轨迹该层 `answers` 均为 calibrated=false |
| 非法请求（L2 任一请求负例） | `Err`，所有后端 kind/model/evaluate 调用 0；`decide` 与 `decide_traced` 结果一致 |
| 零后端合法 Q1 | `layers=[]`，`logged_layers()=Ok([])`，Decision 同旧 Abstain |
| 单题 label 映射：Label `"yes"`、Bool false、Score -1.0 / 0.5（各配 Q1 变体） | label 分别 `"yes"`、`"false"`、`"-1"`、`"0.5"`；p=None 时 `p=null` |
| L2 三题 Q 任意轨迹 | `logged_layers()` 返回 F4 错误，不产出部分结果 |
| 同一 fake 序列分别调用 `decide` 与 `decide_traced` | `decide` 的结果等于 `decide_traced(..).decision`；各后端调用计数相同 |
| 慢 fake（仅用于不变量，不断言数值） | `Σ layers[i].latency_ms ≤ decision.latency_ms` |
| 既有日志/profile/store/CLI fixtures | 无 diff；`DECISION_LOG_SCHEMA_VERSION` 仍为 1 |

实现须 test-first：先在 L2 合入后的基线上提交失败断言（`model` 恒 null、无逐层记录、无法产出多层 `layers[]`），再做最小实现。

## 4. 范围、预算与后续切片

| 后续 task（不是本次授权） | 范围与预算 / 门 |
|---|---|
| D1-2c-T | 本契约精确 head APPROVE 且 L2 实现合入后：trait `model()` 默认方法、`StageRecord`/`StageOutcome`/`TracedDecision`、`decide_traced` 与 `Decision.model` 取值、`invalid_model_ref`；≤160 生产行；fake-backend 矩阵与 F1–F3 快照 test-first |
| D1-2c-G | T 合入后：`logged_layers()` 与 `LayerLogError`、label 渲染；≤80 生产行；F1 映射、F4 与 label 矩阵 test-first |

两片均只改 `rust/crates/agent24-decide`，base 仅 `ab/decide`，生产代码各自 ≤300 行（测试、文档、锁文件、生成物不计），超预算继续拆；各实现 PR 另行外审、CI/本地门禁与 pre-pr-check；Cargo 门禁串行运行，不并发跑 workspace 级门禁。

## 5. 不做的事

- 不改 `LoggedLayer`/`LogEntry` 形状、`DECISION_LOG_SCHEMA_VERSION`、存储迁移或 CLI export；不新增多题日志（schema v2）、`calibrated` 入日志或日志迁移。
- 不新增 `Deserialize`；不改 `ModelRef`、`Answer`、`DecisionRequest`、`BackendOutcome`、`evaluate` 签名或 L1/L2 既有字段与 code。
- 不实现 L4 阈值动作与 `final` 生成，不写 `DecisionLog` 实现、不调用 `record`，不接 `retain.intent`/`recall.gate` 调用点，不碰 agent/memory/recall、WriteGate、owner/active 等 A 类判断。
- 不做 ModelRef 与模型目录（hf_repo/revision/sha256）的交叉校验、下载、ONNX 运行时或硬件档逻辑（D1-1/D1-3）；不从显示名推断模型。
- 不引入可注入时钟或任何计时阈值断言；不承诺实际耗时相同。
- 不宣称 D1-2 或 D1 完成；本文只是 L3 契约提案。

## 6. 审查与完成门

- 本 PR 仅为契约提案，全文选项均待外审；Agent24 的 PR 必须由外部后台 PR-Daemon 审查，内部模型评审不能替代。
- 本文中的任何选项都不得被视为已批准；实现须等外部精确 head APPROVE。

实现须等外部精确 head APPROVE（即外部 PR-Daemon 对本契约文档精确 head 的 APPROVE）。
