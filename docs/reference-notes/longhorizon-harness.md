# 研读笔记：LongHorizon-Harness — 长程任务的循环工程（AMAP-ML，MIT）

> 来源：`vendor/LongHorizon-Harness/`（github.com/AMAP-ML/LongHorizon-Harness，**MIT**，本地只读克隆 @ `a1dd930`，v0.1.7 / 2026-08-20）
> 论文：arXiv 2608.01964 · 2026-W32 Hugging Face Daily Papers 周榜第一
> 日期：2026-08-28 | 用途：Agent24 无人值守正确性（M-F/M-H 已交付部分）的对照与补强输入
> 所有 `path:line` 相对 `vendor/LongHorizon-Harness/`（涉及本仓库代码时给出仓库内完整路径）。
>
> 配套笔记：[`proma.md`](proma.md) · [`macro.md`](macro.md) · [`berd.md`](berd.md) · [`openworker.md`](openworker.md)

---

## 0. 一句话定位 + 规模

**「它不训练模型，也不替换 agent —— 它给已有的 agent 装一个能跑几十小时的执行循环。」**

给一次目标，它反复把剩余工作切成一个**有界步骤**，在真实计算机上执行，**独立验证实际发生了什么**，
通过则 checkpoint，失败则把证据带进下一轮。核心口号：**Loop Engineering**。

| 维度 | 数字 |
|---|---|
| 核心 Python（`src/lh_harness`） | **~20,000 行 / 54 个文件** |
| 评测适配层（`eval/`） | ~205,000 行（三个 benchmark 的 harness，基本是 vendored） |
| 依赖 | **只有 5 个**：packaging · tomli · fastapi · uvicorn · websockets |
| 支持的 agent 后端 | Claude Code · Codex CLI · OpenCode · DeepSeek Harness（`dsh`） |
| 许可证 | **MIT** —— 可直接借用代码 |

**注意这个体量对比**：它做的事（三角色编排 + 独立审计 + 断点续跑 + Web 工作台 + 四种后端适配）
只用了 2 万行、5 个依赖。**这是一份可以完整读完的参考实现**，跟 Macro 的 19 万行不是一个量级的负担。

**与 Agent24 的关系**：**同一个问题域，正交的解法。**
Agent24 是**自建运行时**（自己的 loop、自己的工具、自己的审批门、自己的记忆底座）；
LongHorizon 是**寄生在别人的运行时之上**（Claude Code / Codex 保留各自的原生 loop，它只负责角色边界、
已验证状态、跨轮进展）。**所以它的价值不在「怎么写一个 agent」，在「怎么让一个已经会干活的 agent 在
几十小时里不跑偏」—— 那正是 Agent24 的 F5 泡测要面对的问题。**

---

## 1. ⭐⭐⭐ 核心机制：Executor 的话只是一项声明

整个系统的论点浓缩在 auditor prompt 的一行里（`src/lh_harness/role_prompts.py:253`）：

> **"Executor text is only a claim."**（executor 文本只是一项声明）

三个角色是**循环内部的实现边界**，不是三个各自生长的 agent：

| 循环职责 | 角色 | 拥有什么 |
|---|---|---|
| 状态与下一步 | **Manager** | 每轮从原始目标、已验证进展、失败证据、剩余工作**重建**上下文 |
| 动作 | **Executor** | **全新上下文**开始，完成一个明确定义的步骤 |
| 事实基准 | **Auditor** | **独立检查真实的文件、界面、日志、测试** —— 而不是相信 Executor 的说法 |

**只有通过独立验证的结果才成为可信任务状态。被拒绝的结果留作证据，不算进展。**

### 而且「独立」是用权限强制的，不是靠 prompt 说服

`src/lh_harness/adapters/claude_permissions.py:33` 给每个角色发不同的工具集：

| 角色 | 禁用工具 | workspace |
|---|---|---|
| `manager` / `final_response` | `Bash` · `Write` · `Edit` · `NotebookEdit` · `Agent` · `mcp__*` | **只读** |
| `gui_executor` / `cli_executor` | `Agent`（**禁递归**） | 读写 + computer MCP |
| `gui_auditor` / `cli_auditor` / `auditor_format_repair` | `Write` · `Edit` · `NotebookEdit` · `Agent` | **只读** + computer MCP |

> **Manager 连 Bash 都没有。** 它只能规划，物理上无法自己动手。
> **Auditor 有 computer MCP（能看）但没有写工具（不能改）** —— 这是「独立审计」的机械形式。
> **Executor 禁 `Agent`** —— 与 Agent24 H9 explorer subagent 的「禁递归」同一条判断。

还有一层：`path_deny_rules()`（`claude_permissions.py:78`）**把 harness 自己拥有的路径对 agent 隐藏**。
agent 读不到也改不了 harness 的运行记录 —— 「运行记录不是交付物」这条边界是**机械**的，不是提示词里的请求。

> **对我们**：Agent24 的 `risk_class`（H1）分的是**工具的危险等级**，
> LongHorizon 分的是**角色的能力边界**。两者正交，都需要：
> 一个只读的审计角色，即使它调的工具全是 `Read` 类，也不该拥有 `fs_write` —— 今天我们没有这个维度。

## 2. ⭐⭐ 三行控制头：对 LLM 输出做 fail-closed 解析

Auditor 的输出必须以**恰好三行**开头（`src/lh_harness/prompt_texts.py:193`）：

```
Status: complete|incomplete|blocked
Integrity: clean|suspect|violation
Contract audit: aligned|unknown|needs_revision|invalid
```

而 Manager 只在**三行全是好值**、且报告支撑了每一条原始要求时，才输出完成（`prompt_texts.py:80`）。

关键在解析这一侧（`src/lh_harness/auditor_agent.py:540`）：

```python
def infer_integrity_findings(text):
    integrity_status = _parse_integrity_control_header(text)
    lines = _first_nonempty_lines(text, 2)
    evidence = lines[1] if len(lines) >= 2 else "missing integrity control header"
    if integrity_status == "clean":  return "clean", []
    if integrity_status == "violation": return "violation", [...]
    return "suspect", [...]          # ← 缺头、格式错、无法解析 → 一律 suspect
```

**缺失或畸形的控制头一律降级为 `suspect`，不是 `clean`。** 这是 fail-closed。

更妙的是它不止于拒绝：**有一个专门的格式修复轮**
（`build_role_auditor_format_repair_prompt`，`role_prompts.py:304`）——
auditor 写了对的内容但格式错了，就再跑一轮**只修格式**（且用一个更小的预算，`_format_repair_budget`）。
不是丢弃有价值的审计，也不是放宽解析去将就。

> **对我们**：Agent24 的 H8 plan mode 通过 `propose_plan` **工具调用**拿结构化输入 —— 比解析文本强。
> 但我们在**别的地方**有同一个问题：Guardian 返回的 `{risk_level, rationale}`、
> LLM 摘要压缩、MD-4 的 candidate 抽取，都要从模型文本里读出结构。
> **值得核对的一条：那些解析路径遇到畸形输出时，默认落在安全的一侧还是宽松的一侧？**
> （我没有查证，不作断言 —— 但这是一条 30 分钟能查完、结论很硬的检查。）

## 3. ⭐ Auditor 会盯「删掉证据」这种作弊

`auditor_agent.py:565` 有一个专门的 `extract_deleted_artifact_actions()`：
从审计报告里正则抽出「声明删除了某个文件」的句子，记成

```python
{"action": "delete", "status": "delete_declared_unverified", "path": ..., "reason": ...}
```

配套的路径识别器（`extract_candidate_artifact_paths`）认 workspace 下的任意路径，
以及一批常见交付物后缀（png/pdf/md/csv/json/html/mp4/zip…）。

**为什么需要它**：长程任务里 executor 最省事的「完成」方式，就是删掉那个不通过的测试、
删掉那张对不上的截图。`Integrity: clean|suspect|violation` 那一行正是为这类行为准备的。

还有一条同源的防线（`role_prompts.py:255`）：

> "Harness prompts, trajectories, role outputs, and prior reports are **run records, not task deliverables
> or standalone completion evidence**."

**不许拿 harness 自己产生的东西当完成证据。** 否则系统会自我确认成功。

而**反向的过度严格**也被明确挡住了（同段下一句）：

> "Do not require every subtask to create a file when its consumed result is legitimately application state,
> a user-facing response, or independently observable external state."

> **对我们 —— 这条最接近我们已有的痛点。** F1/F8 二十余轮复审抓到的几乎全是
> 「措辞比机制强」，本质就是**自我确认**：作者说做到了、文档说做到了、但机制没有。
> LongHorizon 把这件事变成了系统内的一个**角色 + 一个控制位**。

## 4. 循环状态模型：只有 6 个 dataclass

`src/lh_harness/types.py` 全文 200 行，核心就六个（`types.py:47`）：

```python
ExecResult      # stdout / stderr / exit_code / duration_ms / termination_reason
EpisodeBudget   # max_duration_seconds（唯一的预算维度）
EpisodeResult   # status: done|timeout|error|cancelled + actions_log
AuditReport     # status + integrity_status + contract_audit_status
                # + completed / missing / blockers / artifact_actions / integrity_findings
ManagedRound    # 一轮的完整记录：plan / executor_output / auditor_report
                # + task_state + task_contract + related_report_refs
HarnessConfig   # 每个角色一份预算 + 三个上下文字符预算
```

两个细节值得单独说：

- **`task_contract` 与 `task_state` 是分开的两个字段。** contract 是「稳定的目标依据」，
  state 是「当前已验证到哪」。prompt 里明说 contract **也不能默认它是对的**
  （"but do not assume it is correct"，`role_prompts.py:245`）。
- **上下文预算是显式的三个数**：`auditor_output_chars=24_000`、
  `role_verified_context_chars=60_000`、`role_history_chars=100_000`。
  长程系统里上下文预算是一等配置，不是隐含在实现里的魔数。

## 5. ⭐ 扩展面只有两个 Protocol，加起来 38 行

```python
# src/lh_harness/adapters/base.py —— 全文 17 行
class AgentAdapter(Protocol):
    async def run_episode(self, prompt, env, budget, live_trajectory_path=None) -> EpisodeResult: ...

# src/lh_harness/environment/base.py —— 全文 21 行
class Environment(Protocol):
    async def exec(self, command, timeout=30, tee_path=None) -> ExecResult: ...
    async def screenshot(self) -> bytes: ...
    async def upload(self, local_path, remote_path) -> None: ...
    async def download(self, remote_path, local_path) -> None: ...
```

**接一个新 agent 后端 = 实现一个方法。接一个新执行环境（远程机 / 容器 / VM）= 实现四个方法。**
四种后端适配器（`claude_code.py` / `codex.py` / `opencode.py` / `deepseek_harness.py`）
都建在同一个 `cli_agent.py` 基类上。

> **对我们**：Agent24 的 `DomainModule` + `KernelCtx`（ADR-029）是同一个手法 ——
> **窄接口 + 组合根注入**。可以互相印证：ME-3「进程外 Provider」要找的那个接口形状，
> `AgentAdapter` 是一个已经被四个真实后端验证过的答案。

## 6. 无人值守的三条工程细节（这些最实用）

### 6.1 append-only 控制总线：断线不丢操作

`src/lh_harness/supervisor/control_bus.py:1`：

> 「控制命令遵循与日志相同的 append-only JSONL 规则，
> 这样浏览器断连或 API 重启都不会丢失一次操作者动作。
> **一张回执是一条命令唯一的终态权威。**」

**「receipt is the only terminal authority」这句话，和 Agent24 H3 durable resume 的
「决策已在行上，无需 await」是同一条判断。** 我们做到了审批侧，它做到了**全部控制命令**侧
（停止、追加轮次、注入指令）。

而且它用 `_open_nofollow`（`control_bus.py`）打开文件 —— **拒绝跟随符号链接**。
这正是 `SPEC-ME-FOLLOWUPS.md` **F6** 说的那条（我们今天只有检查、不是保证）。
MIT 许可证，**这段可以直接借**。

### 6.2 单一人类介入钩子，触发条件数据驱动

`src/lh_harness/dashboard/gate.py:1` 的模块 docstring 就写清了扩展契约：

> 「manager 每轮末调用**一个**钩子。
> **新增一个触发条件 = 往 `_TRIGGERS` 加一个 `_Trigger` + 在 `_classify` 加一个分支；其余不变。**」

五个触发条件：`completed`（完成也要问）· `max_rounds`（轮次预算耗尽）·
`needs_input`（manager 有问题要问）· `needs_human`（manager 报告卡住）·
**`repeated_failure`（连续失败太多轮 —— 可能在打转）**。

其中两个值得学：

- **「完成」也是一个需要人确认的门。** 完成不是自动终止，而是问「继续加轮次，还是就到这里」。
  长程系统里「模型认为完成了」本身就是一个需要复核的判断。
- **`repeated_failure` 是唯一一个不来自单轮结果、而来自趋势的触发**（`rules.evaluate(round_index, rounds)`）。
  **打转是长程任务最贵的失败模式，而它在任何单轮里都看不出来。**

> **对我们 —— 这条 Agent24 今天没有。** C5 调度器有「连续失败 5 次自动禁用」，
> 但那是 schedule 级；agent loop 内部的「反复产出无效计划 / 反复被拒」没有任何计数器。
> F5 泡测跑 7 天，最可能撞上的失败模式恰恰是这个。

### 6.3 区分「运行时死了」和「工具输出里恰好有个 Traceback」

`src/lh_harness/runtime_signals.py:16`：

```python
# A Traceback can legitimately appear in tool output (an agent running a script
# that raises), so only signals that mean the agent runtime itself died count as
# hard failures.
_HARD_SIGNAL_PREFIXES = ("AGENT_EXIT=", TURN_FAILED_SIGNAL)
_HARD_SIGNAL_VALUES = frozenset({"Connection error.", "response.failed"})
```

硬失败信号（进程退出码、连接错误、`response.failed`）与诊断信号（Traceback）分开处理。
**朴素的「日志里有 Traceback 就算失败」会把正常的调试过程判成崩溃。**

## 7. 它自己承认没做到的事（这份诚实值得记）

`claude_permissions.py:36` 的 docstring：

> 「Claude 的交互式审批系统与原生沙箱被**刻意绕过**。
> 剩下的 deny-list 表达的是 **harness 的角色分离，不是文件系统或进程沙箱**。」

**这正是 Agent24 复审二十轮在抓的那种诚实**：不声称比机制更强。
它的角色隔离是**能力边界**（不给这个角色这个工具），不是**沙箱**（进程仍然可以做任何事）。
写下来，读的人就不会误以为 auditor 被关在笼子里。

## 8. 效果数据（它自己报的）

同一个模型、同一个执行后端，**只换 harness**：

| Benchmark | 结果 |
|---|---|
| WeaveBench（GUI + CLI 完成率） | ~50% → **~80%** |
| OSWorld 2.0（完整桌面任务完成） | **3×** |
| Terminal-Bench 2.1（代码 + CLI 成功率） | 69.7% → **77.2%**，且 **token 少 24%** |

> **怎么看这组数字**：这是**论文作者自报**的，不是第三方复现。
> 但「token 反而少了 24%」这一条比完成率更值得注意 ——
> 它说明收益不是靠「多跑几轮堆算力」，而是靠**每轮从已验证状态重建、不带着垃圾上下文往前滚**。
> 这与 Agent24 MD-1 的 Condenser「view-delta 不删原始 + 记 checkpoint」是同一个方向。

---

# 第二部分：我们拿它干什么

## 9. 许可证：**MIT，可以直接抄代码**

| 仓库 | 许可证 | 能不能借代码 |
|---|---|---|
| **LongHorizon-Harness** | **MIT** | ✅ **可以**，保留版权头 + 更新 `NOTICE` |
| Berd | Apache-2.0 | ✅ 可以，同上 |
| Macro | AGPL-3.0 | ❌ **不可以**，只能学思想 |
| Proma | AGPL-3.0 | ❌ **不可以**，同上 |

`vendor/LongHorizon-Harness/` 已加进 `.gitignore`。

## 10. 三档可落地清单

### 10.1 现在就能做

| # | 学什么 | 落地 | 成本 |
|---|---|---|---|
| **A** | **loop 级的「打转」检测**（§6.2 `repeated_failure`） | agent loop 记连续「无效工具调用 / 被拒计划 / 空进展」轮数，超阈值 → 触发一次审批而不是继续烧 token。**F5 泡测跑 7 天最可能撞的就是这个** | 半天 |
| **B** | **硬失败 vs 诊断信号分离**（§6.3） | 核对 Agent24 判定 run 失败的地方，是否会把工具输出里的 Traceback 误判成运行时崩溃 | 1 小时（先查） |
| **C** | **`_open_nofollow` 借过来**（§6.1） | 直接对上 `SPEC-ME-FOLLOWUPS.md` **F6**（symlink 安全今天只有检查不是保证）。MIT，可抄 | 半天 |

### 10.2 M2 顺手做

| # | 学什么 | 对应我们的债 |
|---|---|---|
| **D** | **角色能力边界**（§1）—— 与 `risk_class` 正交的第二个维度 | H9 explorer 已经是「结构性只读」（靠不 advertise），但那是**一个特例**，不是一个可复用的角色-工具矩阵。M-E 收口的 F4 那批 API 收紧可以顺路把它抽出来 |
| **E** | **LLM 输出解析一律 fail-closed**（§2） | 先查 Guardian / MD-4 candidate 抽取 / 摘要压缩三处，畸形输出时默认落在哪一侧 |
| **F** | **上下文预算显式化**（§4） | MD-1 Condenser 已有预算概念，但 `auditor_output_chars` 这类**角色级**字符预算我们没有 |

### 10.3 需要立项

| # | 学什么 | 放哪 |
|---|---|---|
| **G** | **`AgentAdapter` 的接口形状**（§5） | **ME-3 进程外 Provider 的直接参考。** 一个 `run_episode` 方法被四个真实后端验证过 —— 比我们从零设计一个协议更有把握。与 `berd.md` §9 的 ACP 裁决**放在同一条 ADR 里一起决**：ACP 是**线协议**，`AgentAdapter` 是**进程内接口形状**，两者不冲突，但要一起想清楚 |
| **H** | **独立 Auditor 角色**（§1、§3） | 这是最大的一条，也是最该慎重的一条 —— 见下 |

## 11. ⚠️ 关于「要不要引入 Auditor 角色」：先想清楚再动

**诱惑很大**：Agent24 的 agent loop 今天**没有任何独立验证环节**。
工具执行完，结果回填进上下文，模型自己判断成功与否。这正是 LongHorizon 论证的那个缺口。

**但直接抄会踩三个坑：**

1. **成本翻倍。** 每一轮多一次完整的 LLM 调用（auditor 独立检查），
   而 Agent24 的定位是**本地小模型优先**（ADR-026 / oMLX）。
   本地 7B 模型跑 auditor 的判断质量能不能支撑 fail-closed，是**未验证的**。
2. **与 H8 plan mode 语义重叠。** 我们已经有一个「只读探索 → 提计划 → 人批准」的门。
   Auditor 是「做完之后验证」，plan mode 是「做之前批准」——**两者都对，但堆在一起会让审批疲劳翻倍**。
   谁在什么条件下触发，必须先想清楚，否则会变成 `SPEC-ME-FOLLOWUPS.md` F2 那种「两套并存」的债。
3. **它假设任务有可独立观察的外部状态。** LongHorizon 的 auditor 去看文件、看 UI、跑测试。
   Agent24 的很多 run 是对话式的（微信渠道进来的一句话），**没有可审计的产物** ——
   auditor 在那种 run 上除了复读没有别的可做。

**建议的做法**：不整体引入角色，先取**最便宜的那一半** ——
§10.1-A 的打转检测（不需要额外 LLM 调用）和 §10.2-D 的角色能力边界（纯配置）。
真要做 Auditor，应该是 **M4 Cos72 workspace 之后**：那时才有稳定的、可独立观察的交付物
（文档、任务、提案），auditor 才有事实可查。

**登记为决策待定，不排期。**

## 12. 明确不借鉴

1. **绕过后端原生审批**（`claude_permissions.py` 用 `bypassPermissions`）。
   对它是合理的 —— 它要的是无人值守连跑几十小时，交互式审批会卡死。
   **对我们是反的**：C4/D3/H1–H4 那一整套审批门是 Agent24 的核心资产，不能绕。
2. **`eval/` 那 20 万行。** 三个 benchmark 的 vendored harness，与我们无关。
   （不过 **benchmark 名单值得记**：WeaveBench / OSWorld 2.0 / Terminal-Bench 2.1 ——
   `SPEC-MD-ME.md` §0.3 的评测门今天写的是 LongMemEval + LoCoMo，那是**记忆**评测；
   如果将来要证明「Agent24 的循环比裸 agent 强」，需要的是这一批**任务完成**评测。）
3. **Python + FastAPI 那一层。** 我们是 Rust 内核，不引入第二个运行时。
4. **`bypassPermissions` + deny-list 当安全边界。** 它自己都说了那不是沙箱（§7）。

## 13. 一句话总结

> **LongHorizon-Harness 的贡献是一个判断：模型决定一轮能做什么，循环决定几十小时后能不能交付。
> 而循环的核心不是重试，是「Executor 的话只是一项声明」—— 独立验证、fail-closed 解析、
> 反作弊检查（删证据）、和把"运行记录不算完成证据"变成机械边界。**
>
> 我们与它的差距**不在 agent 能力**，在**长程正确性**：
> 今天 Agent24 的 loop 没有任何独立验证环节，也没有打转检测。
> 而 F5 泡测（Mac mini 连跑 7 天）恰好是第一次会暴露这两件事的场合。
>
> **最划算的三件**：打转检测（半天、不花 LLM 调用）· `_open_nofollow` 直接抄（治 F6）·
> LLM 输出解析核对 fail-closed（先查再说）。**Auditor 角色本身先别抄 —— 成本、语义重叠、
> 和"对话式 run 无物可审"三个问题没解决之前，引进来是负债。**

---

## 附：核对清单（本笔记的事实来源）

| 断言 | 出处 |
|---|---|
| MIT 许可证 · v0.1.7 · 5 个依赖 | `LICENSE`、`pyproject.toml:8,14` |
| 核心 2 万行 / eval 20.5 万行 | `wc -l` @ `a1dd930` |
| 三角色职责 | `README.md`「One loop. Three focused responsibilities.」 |
| "Executor text is only a claim" | `src/lh_harness/role_prompts.py:253` |
| 角色权限矩阵 · 禁递归 · 只读 workspace | `src/lh_harness/adapters/claude_permissions.py:33-70` |
| `path_deny_rules` 隐藏 harness 路径 | `src/lh_harness/adapters/claude_permissions.py:78` |
| 三行控制头 | `src/lh_harness/prompt_texts.py:193,212`；完成条件 `:80` |
| 缺头 → suspect（fail-closed） | `src/lh_harness/auditor_agent.py:540` |
| 格式修复轮 | `src/lh_harness/role_prompts.py:304`、`manager.py:1350` |
| 删除产物检测 | `src/lh_harness/auditor_agent.py:565` |
| 运行记录 ≠ 完成证据 / 反向过严守卫 | `src/lh_harness/role_prompts.py:255-256` |
| 六个 dataclass · 上下文预算 | `src/lh_harness/types.py:47-128` |
| 两个 Protocol 共 38 行 | `adapters/base.py`、`environment/base.py` |
| append-only 控制总线 · receipt 是唯一终态权威 | `src/lh_harness/supervisor/control_bus.py:1` |
| `_open_nofollow` | `src/lh_harness/supervisor/control_bus.py` |
| 单钩子 · 五触发 · repeated_failure | `src/lh_harness/dashboard/gate.py:1,48-77,81` |
| 硬失败 vs 诊断信号 | `src/lh_harness/runtime_signals.py:16` |
| 「不是沙箱」的自陈 | `src/lh_harness/adapters/claude_permissions.py:36` |
| Benchmark 数字（作者自报） | `README.md`「Same model. Same execution backend.」 |
