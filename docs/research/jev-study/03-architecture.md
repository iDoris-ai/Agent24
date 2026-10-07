# 03 架构：`agent24-decide` 决策服务

> 出处：`rust/crates/agent24-decide/src/*.rs`（下文简写为文件名）、`docs/decision.md` ADR-033 / ADR-034、`docs/agent/PLAN-DECIDE.md`、`docs/research/DECISION-MODELS.md` §7.3。
> 状态（截至 2026-10-07）：D0 只交付契约、日志、探测与评测，**没有接入任何现有调用点**（ADR-033 第 3 条、PLAN-DECIDE §4）。下文描述的是已合并到 main 的代码形状，不是线上行为。

## 1. 总图

```
调用点（D1 起：retain 记住意图 / recall 召回门控；D2：guardian 工具风险 / 入口路由 …）
   │  DecisionRequest { point, input, context, questions:[Choice|Noul|Score], side_effect_class }
   ▼
┌──────────────────────── agent24-decide（L3，零内部依赖）────────────────────────┐
│ DecisionService::decide —— 按顺序级联各层 DecisionBackend                         │
│                                                                                  │
│   ① RuleBackend（确定性规则，高精度地板）                                         │
│        命中 ─────────────────────────────► Decided（短路，后面的层不再跑）         │
│        未命中 → NoConclusion{floor: []} ──┐                                      │
│   ② Encoder 层（进程内小模型，D1+ 才接）   │                                      │
│        Decided ──────────────────────────► Decided                               │
│        NoConclusion{floor} ──（留下非最终 floor，继续下探）                        │
│        Unavailable ──────────────────────► Unavailable（停止级联，带上最近 floor） │
│   ③ Deep 层（oMLX/Ollaya 上的 Kev 类，可选，D1+）同上                              │
│   全部无结论 ─────────────────────────────► Abstain                               │
│                                                                                  │
│   每层答案返回后：Answer::normalized_for(backend.kind()) 重新盖章 calibrated        │
└──────────────────────────────────────────────────────────────────────────────────┘
   │  Decision { answers[], backend, model, latency_ms, outcome: Decided|Abstain|Unavailable }
   ▼
ThresholdBands（每个决策点单独配置，无全局默认）
   p ≥ execute_at          → Execute（执行）
   escalate_below ≤ p < execute_at → AbstainOrAsk（不做 / 反问）
   p < escalate_below      → Escalate（交人审批）
   │
   ▼
A 类确定性代码（WriteGate / Authorizer / 审批门 …）——决策只能让动作更严，不能放宽
   │
   ▼
DecisionLog（trait 在 decide；SQLite 表在 agent24-store；实现归 agent24d，见 ADR-034）
```

图中 ①②③ 的层次来自 `DECISION-MODELS.md` §7.3；代码里目前**只有 `RuleBackend` 一个实现**（`backend.rs` 模块文档），Encoder / Deep / LlmSimulation 后端是 D1+ 工作。阈值与级联的关系见 §4 的「已知缺口 L4」。

## 2. 三种题型（借 Jev 的接口形态）

`types.rs`：`Question` 只有三种形状，命名照抄 TypeSafe Jev `/v1/systemone` 的词汇（Jev 本身闭源，不使用，只借接口形态；出处 `types.rs` 模块文档、`DECISION-MODELS.md` §7.2）。

| 题型 | 含义 | 对应答案 `AnswerValue` | 本项目的例子 |
|---|---|---|---|
| `Choice` | 封闭标签集多选一 | `Label(String)` | 记住意图 6 类；工具风险 4 级 |
| `Noul` | 是 / 否（Jev 自己的拼写，不是 `bool` 的笔误） | `Bool(bool)` | 召回门控：这句需要记忆吗 |
| `Score` | 有界数值打分 | `Score(f32)` | （尚无调用点） |

每个 `Answer` 带可选概率 `p` 和 `calibrated` 标记。`DecisionPoint` 是开放的字符串 newtype（不是封闭枚举），已用的 ID：`retain.intent`、`recall.gate`、`guardian.risk`（`points.rs`）。

`SideEffectClass`（`None` / `Reversible` / `Irreversible`）标记被这个决策把守的动作可逆性（`types.rs`）。

## 3. 级联语义与「不静默降级」

`service.rs` / `backend.rs` / ADR-033 第 5 条：

- 每层返回三态 `BackendOutcome`：`Decided(answers)`、`NoConclusion { floor }`、`Unavailable { reason }`。
- **`NoConclusion` 与 `Unavailable` 刻意区分**：规则没命中是正常、健康的结果，继续下探；后端根本跑不了（模型没下载、运行时崩溃、断网）不是——必须停下来。
- `Unavailable` 时**立即停止级联**，返回 `Outcome::Unavailable`，并带上「最近一个真正留下非空 floor 的层」的答案（没有就是空）；绝不跳过失败层去问更深的后端，也绝不替失败层编造结果。
- 三个单测锁住这三种情形：`unavailable_layer_stops_the_cascade_without_downgrading`、`unavailable_layer_carries_the_last_floor_forward`、`an_empty_floor_carries_forward_as_empty_not_as_a_fabricated_answer`。
- 「不静默降级」借鉴自 jev-skill 的 no-silent-fallback 规则：「A silent fallback is a hidden bug」（出处：`DECISION-MODELS-EVIDENCE-2026-10-06.md` §2，规则存在已核实、确切文件路径未核实）。

这条路径在评审前其实是死代码：`floor_answers` 永远为空，测试也没断言 `answers`。#688 评审 M3 指出后，作者选择「把路径接通」而不是删掉描述（详见 `08-code-review-findings.md` 第 3 条）。

## 4. 三段阈值

`threshold.rs`：

- `ThresholdBands { execute_at, escalate_below }`，区间 `[0, escalate_below)` → `Escalate`，`[escalate_below, execute_at)` → `AbstainOrAsk`，`[execute_at, 1]` → `Execute`。
- **没有 `Default`，没有零参构造**：PLAN-DECIDE §0 明令禁止「全局 0.5 默认」。用 `compile_fail` doctest 在编译期强制（`ThresholdBands::default()` 必须编译不过）。
- **反序列化也走校验**：`#[serde(try_from = "RawThresholdBands")]`，配置文件里写反、越界的阈值会被拒（#688 评审 M2，见 `08-code-review-findings.md` 第 2 条）。
- 为什么按决策点分别设：误判代价不对称。`threshold.rs` 模块文档的例子是 `guardian.risk` 需要较低的 `escalate_below`（把「低风险」判错代价高）。EVIDENCE §8 引用了内容审核行业的同类做法，以及 Youden's J 默认等代价假设在真实业务中不成立。

**已知缺口（PLAN-DECIDE「D1 前置」L1–L4，来自 #688 评审的非阻塞建议）**：

- L1：`Unavailable` 不报告是哪一层失败；
- L2：service 不校验答案是否覆盖所有问题、`p` 是否在 [0,1]、`Score` 是否越界；
- L3：`Decision.model` 恒为 `None`，`Decision` 只保留最后一层，日志 `layers[]` 填不全；
- L4：`DecisionService` **目前完全不使用 `ThresholdBands`**——级联只看某层是否 `Decided`，不看概率落在哪个区间，所以「弃权 / 反问」这条语义今天走不到。阈值放在 service 还是调用方应用，D1 前要定。

## 5. `llm_simulation` 标记

`types.rs` / `service.rs` / ADR-033 第 6 条：

- `BackendKind` 四值：`Rule`、`Encoder`、`Deep`、`LlmSimulation`。`LlmSimulation` 指「让通用 LLM 模拟校准分类器的输出形状」，它的数字不是校准概率。
- 不变量：`LlmSimulation` 的答案永远 `calibrated = false`。
- **真正的强制点在 `DecisionService::decide`**：每个后端 `evaluate()` 返回后，立刻用该后端自己的 `kind()`（调用前取得，后端无法在 `evaluate()` 里改变）重新盖章（`Answer::normalized_for`）。`Answer::new` 里的同类覆盖只是纵深防御——因为 `new` 的 `backend` 参数由后端自己传，后端可以撒谎（#688 评审 M1 用独立探针 crate 实跑出反例，见 `08-code-review-findings.md` 第 1 条）。
- `Answer` 字段私有、无 `Deserialize`：不从进程外接受，只能经 `Answer::new` 构造。

## 6. A 类不变量在代码里怎么体现

- `lib.rs` 模块文档把 PLAN-DECIDE §0 的硬约束原样写成「扩展本 crate 时不得放宽」的清单。
- `Outcome` 只有 `Decided` / `Abstain` / `Unavailable` 三态，类型系统里没有「放宽权限」的出口（ADR-033 第 4 条）。
- crate 只依赖 `serde` / `serde_json` / `async-trait` / `thiserror`（以及 D0-3 引入的 `sysinfo`，MIT，`hw.rs` 文档注明核过许可证），不依赖任何内部 crate（ADR-033 第 2 条）。

## 7. ADR 要点

### ADR-033：`agent24-decide` 位于 L3，D0-1 只定契约、不接入任何调用点（2026-10-07，采纳）

1. 分层位置 L3（内核能力服务），与 `agent24-models` / `agent24-tools` / `agent24-policy` 同级：无状态能力，不持有 run 生命周期，不做权威持久化，也不是 L0 的边界契约。
2. 依赖最小化，零内部依赖——为 D1 保留挂载点（`agent24-agent` 或 `agent24d`）的自由。
3. D0-1 不接入任何现有调用点，`retain.rs` 行为零变化。
4. A 类判断永不进入。
5. 不静默降级（含 #688 评审修正说明）。
6. `llm_simulation` 由 service 强制（含 #688 评审修正说明）。
7. 没有全局默认阈值，反序列化走同一条校验（含 #688 评审修正说明）。

代价：`DecisionRequest` / `Decision` 暂时不是 wire 类型，D1 接 REST 时再决定是在 `agent24d` 补映射还是加 `agent24-protocol` 依赖。

值得写进文章的细节：三条评审修正都以「原文 + 2026-10-07 评审修正」小字的形式留在 ADR 里，没有悄悄改写历史（#688 第 2 轮评审特意指出这一点）。

### ADR-034：决策日志契约由 `agent24d` 实现，不是 `agent24-store`（2026-10-07，采纳）

1. `agent24-store` 是 L2、`agent24-decide` 是 L3；依赖只能向下，所以 store 不能 `use agent24_decide`。实现 `DecisionLog` 的具体类型归组合根 `agent24d`（L5）；D0-2 只定 trait，不写实现。
2. 两边各有一份 DTO（`LogEntry` vs `NewDecisionLogEntry`），是**故意的重复**：合并需要一方依赖另一方，而今天哪个方向都不成立。
3. `agent24 decide export / delete` CLI 直接打开 `~/.agent24/agent24.db`，不经过 daemon——查看、导出、删除自己的数据是隐私功能，不该依赖一个可能没在跑的进程（SQLite WAL + `busy_timeout`）。
4. D0-2 交付三块彼此吻合但未接通的契约；`DecisionService` 结果 → `LogEntry` 的翻译留给 D1。

## 8. 硬件分档与模型目录（D0-3）

另见 `06-training-and-deployment.md`。

```
HardwareProbe ──► HardwareProfile ──► TierPolicy::decide ──► TierDecision{hardware_tier, effective_tier, reasons[]}
 (sysinfo：内存/架构/     （纯函数，无 I/O）     ↑ DownloadConsent（默认未同意 → T0）
  加速器/磁盘/OS/电池)                          ↑ 用户覆盖（只能降档）
```

对外接口：`GET /api/v1/decide/profile`（host-only，落在 `ModelsAdmin` 鉴权分支，见 #689 评审）。
