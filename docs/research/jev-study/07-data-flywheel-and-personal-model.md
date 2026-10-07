# 07 数据飞轮与个人决策模型

> 出处：`docs/agent/PLAN-DECIDE.md` §2；`rust/crates/agent24-decide/src/log.rs`；`rust/crates/agent24-store/migrations/0014_decision_log.sql` 与 `rust/crates/agent24-store/src/decision_log.rs`；`docs/decision.md` ADR-034；PR #717 及其评审。
> 状态：D0-2 交付了 schema、存储、导出 / 删除 CLI 和 trait，**没有任何调用点真正写入决策**（ADR-034 第 1、4 条）。D1 起才开始记录。

## 1. 为什么从第一天就记

jason 2026-10-07 追加要求之一：「从第一天起积累数据，为将来训练用户自己的决策模型（个人版 Jev）打基础」（PLAN-DECIDE 文首）。所以日志字段从一开始就按「可直接导出训练」设计（PLAN-DECIDE §2.1）。

## 2. 决策日志 schema

两张表（`0014_decision_log.sql`）：

**`decision_log`**：一次 `DecisionRequest` 一行。

| 字段 | 含义 | 超期清理时 |
|---|---|---|
| `decision_id` | 调用方给的不透明 ID（主键；重复插入报错，不静默覆盖） | 保留 |
| `ts` | ISO 8601 UTC | 保留 |
| `schema_version` | 日志自身的 schema 版本（当前 1，`DECISION_LOG_SCHEMA_VERSION`） | 保留 |
| `point` | 决策点，如 `retain.intent` | 保留 |
| `input` | 用户原文，仅本地 | **置空** |
| `context` | 结构化上下文特征（不存整段对话） | **置空** |
| `question` | 题面与候选标签（模板，不是原文） | 保留 |
| `layers` | 每层的 backend、模型 id+revision、标签、概率、耗时（JSON 数组） | 保留 |
| `final_action` | `execute` / `abstain` / `ask` / `escalate`（CHECK 约束） | 保留 |
| `hw_tier` | 当时的硬件档位 | 保留 |
| `scrubbed_at` | 何时被清理过原文（NULL = 从未） | — |

**`decision_outcome`**：事后标签，一次决策可以追加多条（事件溯源，只追加不覆盖），外键 `ON DELETE CASCADE`。

| 字段 | 取值 |
|---|---|
| `signal` | `clarify_answer` / `user_retract` / `user_says_wrong` / `approval_denied` / `approval_granted` / `recalled_uncorrected` |
| `label` | JSON |
| `quality` | `high` / `medium` / `low` |

Rust 侧对应类型：`LogEntry`、`LoggedLayer`、`FinalAction`（序列化键名用 PLAN 原文的 `final`）、`OutcomeEntry`、`OutcomeSignal`、`OutcomeQuality`（`log.rs`）。

## 3. 标签从哪来（不打扰用户）

PLAN-DECIDE §2.2：

| 信号 | 标签含义 | 质量 |
|---|---|---|
| 用户回答了澄清问题（「你是想让我记住吗？」→ 是/否） | 强标签 | 高 |
| 用户撤回刚写入的记忆 / 说「不对」「我没让你记」 | 判错 | 高 |
| 审批被拒 / 被批 | 风险判断的对错 | 中 |
| 写入后被正常召回且未被纠正 | 弱正例 | 低（只做辅助） |

设计要点：
- 澄清问题本身就是三段阈值中「反问」那一段的产物——**弃权带既是安全机制，也是标注机制**（**推断**：PLAN 没有这样表述，但 §2.2 第一行的信号只能来自「反问」动作）。
- 「写记忆有副作用，偏精确」+ 系统回执（M1-T14 的 `memory_receipt`）+ 撤回入口，让用户的纠正自动变成标签（`DECISION-MODELS.md` §7.3）。
- 低置信样本进复核队列、批量回灌，是主动学习的成熟做法（EVIDENCE §8，引 Roboflow、Label Studio 文档）。

## 4. 隐私硬约束

PLAN-DECIDE §2.3 与实现：

1. **只存本地**（agent24 store，`~/.agent24/agent24.db`），永不上传、不进遥测（迁移文件头注释原文：「Local-only, no telemetry: nothing in this file is ever sent off-device」）。
2. **可查看、可导出、可删除**：`agent24 decide export --jsonl [--point X] [--since T]`；删除支持按条、按决策点、全部三种粒度（`DeleteSelector::{ById, ByPoint, All}`），删除 `decision_log` 行会级联删掉其 outcome。CLI 直接开库、不经过 daemon——隐私功能不应依赖一个可能没在跑的进程（ADR-034 第 3 条）。
3. **保留期 180 天**：jason 2026-10-07 在 PR #717 确认（「决策日志保留期默认 180 天（超期只删原文与 context、保留统计）」）。代码常量 `DEFAULT_DECISION_LOG_RETENTION_DAYS = 180`；`log.rs` 里「待 jason 定」的注释按该评论约定在 D1 接线时改为已定。选 180 天的理由（`log.rs`）：足以覆盖 D1 每个决策点约 200 条带标签样本的累积目标，同时仍是有界窗口。
4. **超期只删原文**：清理（`scrub_expired_decision_log`）只把 `input`、`context` 置空并打 `scrubbed_at`，统计字段永久保留，使这一行仍计入累积目标；幂等，不会重复打戳（测试 `retention_scrub_is_idempotent_and_does_not_re_stamp_already_scrubbed_rows`）。D0 没有调用方触发这个清理。
5. **联邦学习**（只传 LoRA、同 base）是更后面的事，**必须单独征得同意**，本计划不实现（PLAN-DECIDE §2.3、§4「不上传任何用户数据」）。

## 5. 个人模型（D4）与晋升门

PLAN-DECIDE §2.4：

- **开工门槛**：某决策点累计 ≥ N 条带高 / 中质量标签，且正负两类都有。N 由 D1 数据量定，**初估 200**。数据量不够就不开工。
- **方法**：在 T3（或用户自愿的 T2）机器上做 SetFit 头 / encoder LoRA（MLX），**不动 base**。
- **晋升门**（必须同时满足）：
  1. 通用中文评测集不退化；
  2. 危险操作漏判仍为 0；
  3. 在用户自己的留出集上优于全局模型。
- 不达标就不启用，继续用全局模型；**可一键回退**。

（**推断**：「用户留出集」从决策日志的高 / 中质量标签里切分；这一点 PLAN 没有写明切分方式。）

## 6. 评审里发现的、D1 接线前要处理的数据问题

出处：PR #717 第 1 轮评审（非阻塞 forward-note）。这些在「零调用点」时是休眠的，一旦开始真实记录就会变成问题：

- `decision_outcome` **没有幂等键**：一次模糊失败的重试可能重复插入同一条 outcome，在 §2.4 的累积统计里被重复计权。
- `--since` 与清理的时间戳按**裸文本比较**，没有 RFC3339/UTC 归一化；混用 `Z` 和 `+08:00` 会悄悄给出错误结果。
- 导出用 `IN (?, ?, …)`，占位符数等于匹配行数，理论上会碰到 SQLite 参数上限。
- `FinalAction::as_db_str()` 与 CHECK 约束一致性的测试只比较了字符串常量，没有真正插库触发 CHECK。
- CLI 第一次拿到对 `agent24.db` 的迁移运行权：新版 CLI 先迁移后，旧版 daemon 会因 `VersionMissing` 起不来。评审判定这不是新的风险类别（任意两个 daemon 版本之间已存在），下调为非阻塞。
- 另有两条未验证的 Info：`secure_delete` 是否开启；在空库上跑 `export` 会产生「只读命令却建文件 + 跑迁移」的副作用。

另外 #688 评审 L3：`Decision` 只保留最后一层、`model` 恒为 `None`，所以 `layers[]` 目前填不全——D1 要先扩展 `DecisionService`（PLAN-DECIDE「D1 前置」）。

## 7. 与 iDoris 的关系

接口归 Agent24，后端长期迁往 iDoris 网关；iDoris 规划里的 `HardwareAwareModelRecommender` 先在这里落地一个只服务决策模型的版本（PLAN-DECIDE §0、§1.1）。
