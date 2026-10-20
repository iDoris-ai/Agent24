# OfficeAssistant 设计讨论：把你的四个判断对照 Agent24 代码

状态：**讨论稿（已由 Opus 5.5 评估，见第五节）**。日期 2026-10-16。作者：协调者。
对照基线：`origin/main`（`92f9f0cf`，含 K1 发布）。凡"代码事实"都给了文件/行号或命令，可逐条复核。

> ⚠️ **本稿第一、四节有两条事实已被第五节更正，引用时以前者为准**：
> - **F9**：记忆不止 D1——M1（#720，2026-10-07）已合入，agent loop 现在消费 `SessionLog`/`EventStore`（每轮原文写进 `mem_events`）/`AssertionStore`；仍未被消费的是 retriever/consolidator/knowledge。
> - **F10**：LocalOnly **已经**覆盖主对话/`/api/v1/chat`/explorer/会话压缩四条路径（K1 之后），本稿说"未传播"是过时的；真正没做的是反方向（无生产入口写 `CloudAuthorized`）与 `http_fetch`/`shell_exec` 的统一出站判定。
> - 另有两处判窄：**F5**（内核已有最接近 action/approval/receipt 的原语 `ModuleApproval`）、**F11**（挂点已有先例：Documenting 的 ADR-DOC-01 已裁定办公能力用进程外领域 OS）。

---

## 一、你的四个判断，逐条对照

| # | 你的说法 | 代码事实 | 判定 |
|---|---|---|---|
| 1 | 它偏向**工具调用型引擎**，调用很多工具完成工作场景的**流程化串联** | **内核已经是这个引擎。** `rust/crates/agent24-tools/src/lib.rs:266` 有 `pub trait Tool`、`:305` 有 `ToolRegistry`；工具走固定流水线 **normalize → 能力白名单 → 审批门 → 超时执行**；`agent24-policy` 是审批 broker + 工具门（Guardian 默认关闭，`A24_GUARDIAN=1` 才开）；`agent24-agent` 是 run 管理器 + agent loop；**MCP 外部工具被包装成普通 `Tool`，走同一条审批流水线** | ✅ **对，但归属说反了**：OfficeAssistant 不该再建一个引擎，而是**用它** |
| 2 | 可以设计一些 **Skill 去串起工具调用** | Agent24 的 Skill **不是编排**：`docs/specs/SPEC-SKILL-DISTRIBUTION.md`（状态"**设计定稿、未实现**"，归属 M5）定义的是**指令包的分发与升级**——三来源 bundled/distro/user、优先级 用户 > 分发 > 内置、同名整体取代不做字段级合并。另 `rust/crates/agent24-memory/src/knowledge.rs` 是**指令层**（CLAUDE.md 式分层指令合并 + 触发注入 + 评审门控的自动记忆收件箱），也**不是**检索式知识库 | ⚠️ **需要修正**：Agent24 里"用 Skill 串工具"没有对应物 |
| 3 | **嵌入在 Agent24 上** | Agent24 有四类外挂（`docs/ARCHITECTURE-LAYERS.md` L6）：① 进程内 DomainModule（🟡 生产 catalogue 为空）、② 进程外领域 OS（Sin90/Cos72 ✅）、③ 附着模块（AgentEar ✅）、④ sidecar（Open Design）；另有 MCP（工具级）。内核状态机只有 **Run / Approval / ToolCall**（`rust/crates/agent24-core/src/transitions.rs`） | ✅ **对，但要先选"挂在哪一面"** |
| 4 | 使用 Agent24 提供的**上下文记忆、knowledge base 等公共服务** | 记忆：agent loop **只消费 D1**（`KvStore` + `CanonicalSession`）；`retriever`/`consolidator`/`knowledge` 等高层**尚未被 agent loop 消费**（`ARCHITECTURE-LAYERS.md` L3 表，属 M1/P1）。路由：`TaskProfile::default()` 仍由主对话/子 agent/REST 传，**LocalOnly 未传播**；iDoris provider 未接线（无 `IDORIS_URL`）。事件 schema 里 `workflow` / `artifact` / `receipt` **出现 0 次**、`step` 仅 2 次 → 报告 §4 的字段是**待新增契约**，不是已有接口 | ✅ **方向对，成熟度必须打折** |

### 逐条的"更准确的说法"

1. **工具引擎已经存在，别重建。** 你要的"流程化串联"缺的不是工具引擎，而是**业务层的工作流状态**：
   内核的 Run/ToolCall 状态机只描述"这一次运行/这一次工具调用"，**没有 workflow / step / action / artifact / receipt 这层业务语义**（命令核实：
   `git show origin/main:protocol/events.schema.json` 里 `workflow`/`artifact`/`receipt` 命中数为 0）。
2. **"Skill 串工具"应当拆成两件事**：
   - **确定性串接 → 工作流层**（代码/状态机）。办公流程要求"固定计划版本、逐步 `action_id`、幂等键、单独回执"（报告 §3），
     这些是**可测的契约**，写成指令文本无法被机械验证。
   - **给模型的知识与规则 → Skill/指令包**（报告 §1 表也是这个定位：Skill 是"输入输出规则、步骤、质量约束、适用条件"，
     且"**文本指令既不给权限，也不证明连接器可用**"）。
3. **挂点三选一（这是本讨论最该拍板的技术决定）**：

   | 挂点 | 复用到的权威 | 代价 | 与"唯一控制面"的关系 |
   |---|---|---|---|
   | A. 组合根里的**薄业务层**（`agent24d` 内新模块 + 新 REST/WS 投影） | 直接复用 run/approval/audit/store | 要动内核组合根与协议；内核职责变宽 | 最直白：不新开通道 |
   | B. **进程外领域 OS 模块**（os-proto 回调，像 Sin90/Cos72） | 经 `KERNEL_OOP_GRANTS`（Events/Memory/Approval/Scheduler/Models） | 需要模块打包与监管；回调面比内核 REST 窄 | 天然隔离，但"业务编排"放进领域 OS 是否合适要论证 |
   | C. **sidecar**（像 Open Design） | 需 capability 模式与路由级资源授权——**今天尚未实现** | 最重，且当前 capability 模式下除 `GET /api/v1/health`、`GET /api/v1/models` 外基本不可用 | 与 ADR-001/005 的目标形态一致，但**不是首版能走的** |
4. **公共服务的真实成熟度**（决定首版能承诺什么）：
   - **可依赖**：D1 记忆（KvStore/CanonicalSession，agent loop 已在用）、审批/审计/取消/事件、工具流水线、模型路由（本地档）。
   - **不可依赖/未接线**：高层记忆（retriever/consolidator）、检索式 KB（在 iDoris-Components 侧，visibility 固定 public、**非权限感知**）、
     LocalOnly 传播到主对话、iDoris 出口脱敏、capability 模式的资源级授权。
   - 因此报告 §5 的第 2、4 条（"可信本地材料 + mock 连接器"、**不得承诺端到端脱敏**）不是保守，而是与代码现状一致。

---

## 二、由此推出的设计约束（我的建议，待 Opus 复核）

1. **内核唯一控制面不变**：OfficeAssistant 的每一次外部动作都过既有 approval/consent 门；
   不得因为"这是工作流"而另开执行通道（报告 §5 第 1 条）。
2. **OfficeAssistant = 薄工作流层**（即报告 §4 首推的 "Agent24 kernel + a thin workflow layer"）：
   新增的是**业务状态**（workflow / step / action / artifact / receipt），与内核的 Run/Approval/ToolCall
   **用不同 ID、显式引用**（报告已在 §1 提出 `workflow_id` / `run_id` / `workspace_id` / `artifact_id` 的分离要求）；
   **绝不复制权威状态**，前端不维护第二套审批结果。
3. **Skill 的正确定位**：人类可读的 SOP / 指令包（供模型理解与质量约束），
   **不承载授权、不承载确定性编排**；编排由工作流层驱动，权限由内核裁决。
4. **首版边界照报告 §5 收紧**：真实外部动作逐次确认；不做无界持续授权；
   mock 与真实必须在界面上可区分；"事件不等于送达"要落到验收（报告 §6 的负例矩阵）。

---

## 三、需要新增或明确的东西（候选清单，供后续切片）

| # | 待定项 | 为什么必须先定 |
|---|---|---|
| 1 | 四类业务记录落在哪（新增表 / 投影 / 事件）以及与三张内核状态机的关系 | 决定是否要 ADR；决定幂等与恢复怎么做 |
| 2 | "计划版本 + 批准绑定最终 payload"的实现落点（内核 approval 是否够用，是否要 action 级批准） | 报告 §3 的"批准不得被静默复用"要靠机制而非文案 |
| 3 | 幂等与对账：外部成功 + 本地超时 → 先查询；未知结果不得盲重试 | 报告 §6 负例矩阵里两条直接对应 |
| 4 | LocalOnly 传播与**出站路径分别治理**（LLM / 邮件 / Notion / HTTP / MCP 是不同的出口） | K1 已把主对话收窄为 LocalOnly，但 `TaskProfile::default()` 未传播 |
| 5 | 首版可用的授权面：今天是 legacy 全权 bearer + capability 模式不完整 | 决定"可信本地材料 + mock"的边界，不能对外承诺隔离 |

---

## 四、留给 Opus 5.5 的评估问题

1. 上面四条判定里，**哪一条我判错了、哪一条我判窄了**？请指到代码或报告原文。
2. "薄工作流层"应该落在 **A（组合根薄层）/ B（进程外领域 OS）/ C（sidecar）** 中的哪一个？给出取舍理由与首个切片的推荐。
3. 如果作者坚持"用 Skill 串工具"，**最接近又正确的形态**是什么？请给出可执行的替代写法（例如 Skill 作为 SOP 文本 + 工作流层执行）。
4. 这份约束与三个里程碑（M1 本地闭环 / M2 一条真实连接器 / M3 复用与有界委托）的切法是否自洽？需要怎么调？
5. 还有哪些"会被这句话误导"的风险点，是我没点出来的？

---

## 五、Opus 5.5 的评估（2026-10-16）

> 本节作者：Claude Opus 5.5（`claude-opus-5-5`）。依据是本讨论稿前四节和题述事实源，另加核对中发现的
> `docs/documenting/adr/ADR-DOC-01-placement-and-integration.md`、`docs/agent/K1-RELEASE-NOTES-20261016.md`、
> `rust/apps/agent24-documents/`、`rust/apps/agent24d/src/approval_callback.rs`。**本节不是审批**，也不替作者拍板。
> 核对基线：本 worktree 的 HEAD `92f9f0cf`，与讨论稿基线相同。下文行号都按这个提交，未核实的会注明。

### 5.1 事实核对（逐条）

| # | 讨论稿主张 | 核对结果 | 证据 |
|---|---|---|---|
| F1 | `agent24-tools/src/lib.rs:266` 有 `pub trait Tool`，`:305` 有 `ToolRegistry` | **更正：行号错。** `pub trait Tool` 在 `:389`，`pub struct ToolRegistry` 在 `:1063`。`:266` 是 `ToolContext` 的 `Debug` 实现，`:306` 是 `pub trait ApprovalGate` | `grep -n "pub trait Tool\|pub struct ToolRegistry" rust/crates/agent24-tools/src/lib.rs` |
| F2 | 固定流水线是 normalize → 能力白名单 → 审批门 → 超时执行 | **确认。** 文件头 `:4`；`dispatch` 内的步骤注释在 `:1336`（1）、`:1343`（2）、`:1351`（3）、`:1400`（4）附近。K1 之后，模块工具在进入运行时之前还要多过一道 consent 门（`ModuleTool`，`:546` 起） | 同文件 |
| F3 | MCP 外部工具包装成普通 `Tool`，走同一流水线 | **确认，补一句：** MCP 工具一律定成 `RiskClass::External` | `rust/crates/agent24-mcp/src/lib.rs:269-273` |
| F4 | Guardian 默认关闭，`A24_GUARDIAN=1` 才开 | **确认**（只确认了环境变量名，开关逻辑没有逐行读） | `rust/apps/agent24d/src/server.rs` 含 `A24_GUARDIAN` |
| F5 | 内核状态机只有 Run / Approval / ToolCall | **判窄。** `transitions.rs` 另有 `ScheduleHealth`（`:102-111`）。更要紧的是，内核里已经有一套**和 action/approval/receipt 最接近的现成原语**，讨论稿没有提：**ModuleApproval**（`agent24-protocol/src/types.rs:590-640`、`agent24-store/migrations/0005_module_approvals.sql`）。它带 `request_id` 幂等键 `UNIQUE(module, request_id, kind)`、内核自己算的 `payload_digest`、`expires_at` → `timed_out`（fail-closed），以及只表示"内核执行了"的 `executed_at` | 见左 |
| F6 | 事件 schema 中 `workflow`/`artifact`/`receipt` 为 0、`step` 为 2 | **确认。** 两处 `step` 都是描述文字（`protocol/events.schema.json:506`、`:663`），和业务步骤无关 | `Grep "(?i)workflow\|artifact\|receipt\|step" protocol/events.schema.json` |
| F7 | Skill 没有对应物；`SPEC-SKILL-DISTRIBUTION` 是"设计定稿、未实现" | **确认，而且比讨论稿说的更彻底：** `rust/` 下对 `skill`（不分大小写）检索命中为 **0**，内核没有任何 Skill 加载器。报告提到的五个 Skill 在 iDoris-Components 仓库（报告 §2 [S03]–[S07]），不在 Agent24 | `Grep -i skill rust/` → 0；`SPEC-SKILL-DISTRIBUTION.md:3` |
| F8 | `knowledge.rs` 是指令层，不是检索式 KB | **确认**（`knowledge.rs:1-27`）。补充：agent loop 没有消费它，`agent24-agent/src` 里检索不到 `KnowledgeBase` | 同左 |
| F9 | 记忆：agent loop **只消费 D1**；retriever/consolidator/knowledge 未消费 | **更正（已过时）。** M1 已在 2026-10-07 合入 main（#720，`tasks.md:24`）。agent loop 现在消费的是 `SessionLog`、`EventStore`（每轮原文写进 `mem_events`）、`AssertionStore`（用户显式"记住"的事实，召回时作为**数据块**注入）（`session_memory.rs:5-11`、`:17-30`；`retain.rs:1-12`；`lib.rs:724`）。retriever/consolidator/knowledge **仍未**被消费，这一半成立。讨论稿引用的 `ARCHITECTURE-LAYERS.md` 是 2026-10-03 写的，早于 #720 | 见左 |
| F10 | `TaskProfile::default()` 仍由主对话/子 agent/REST 传，**LocalOnly 未传播** | **更正（已被 K1 推翻）。** 主对话 run（`agent24-agent/src/lib.rs:1726`）、`/api/v1/chat`（`agent24d/src/routes.rs:569`）、explorer（`subagent.rs:231`）、会话压缩（`lib.rs:240`）都经过 `task_profile_for_run` / `merge_source_policy`（`lib.rs:49-56`、`:482-495`）。用户输入的来源标签固定为 `LocalOnly`，查不到时也按 `LocalOnly` 处理，所以**四条路径今天都是 LocalOnly、远端 fail-closed**（K1 发布说明 `:149-158`）。真正还没做的是**反方向**：没有任何生产入口能写 `CloudAuthorized`（发布说明 `:31`），`http_fetch`/`shell_exec` 也没有统一的出站判定（`:33`）。第三节第 4 行"K1 已把主对话收窄为 LocalOnly，但 `TaskProfile::default()` 未传播"前后矛盾，以前半句为准 | 见左 |
| F11 | 四类外挂：② 进程外领域 OS 为 Sin90/Cos72 | **判窄：漏了最直接的先例。** 本仓库 `rust/apps/agent24-documents/`（Documenting，第一方进程外领域 OS，`rust/Cargo.toml:29` 是 workspace 成员）。它的 ADR-DOC-01（Accepted，2026-10-07）裁定的正是本议题第四节第 2 问："第一方办公能力放在哪一面"。结论是 ②，并**否决**了"内核内置代理工具"，理由是内核里不放领域名词，与 T11 一致（ADR-DOC-01 D1/D2/D4；`decision.md:1126`） | 见左 |
| F12 | `KERNEL_OOP_GRANTS` = Events/Memory/Approval/Scheduler/Models | **确认** | `rust/apps/agent24d/src/domain.rs:116-122` |
| F13 | C（sidecar）在 capability 模式下除 health/models 外基本不可用 | **确认** | `ARCHITECTURE-LAYERS.md:158` |

**另外三条讨论稿没写、但会影响判断的事实：**

- **没有"非模型调用方"走工具流水线的入口。** 生产代码里调用 `ToolRegistry::dispatch` 的只有 agent loop（`agent24-agent/src/lib.rs:2028-2030`）和子代理（`subagent.rs:317`）。`module_tool_runtime.rs` 里的调用都在 `#[cfg(test)]` 模块之下（`:177` 起）。也就是说，今天的内核引擎是**模型驱动**的：要想"确定性地按步骤调工具并过审批"，没有现成接口可用。ADR-DOC-01 §2 D1 也写明"模块也不能调用 Agent 工具（📐）"。
- **ModuleApproval 的 `Gate`（有约束力）只接受一个闭集，今天闭集里只有 `schedule_callback`。** 其余动作只能走 `Advise`，而 `Advise` **不具约束力**（`approval_callback.rs:133-197`；`types.rs` 字段注释："for `Advise`, this is knowledge, not a safety control"）。进程外模块今天**拿不到**一个能约束它自己执行"发邮件/写 Notion"的内核批准。
- **模块工具的 consent 今天在生产路径上写不进去。** grant 只出现在测试代码里（K1 发布说明 `:27`）。第一个加 grant 路由的 PR 必须同时处理 F1/F3/F6（`:59`）。

### 5.2 回答第四节的 5 个问题

**1) 四条判定里哪条判错、哪条判窄？**

- **判错：第 4 条的两处事实已过时**，见 F9、F10。影响有两个。第一，M1 的"只用本地模型"今天已由内核**自动保证**，不需要 OfficeAssistant 去补 LocalOnly 传播；反过来，任何云处理今天都**做不到**。第二，凡是走 agent loop 的内容，原文都会被写进 `mem_events`（`retain.rs:1-12`）。会议转录、邮件正文这类第三方敏感材料如果从主对话进入，就会被记忆日志留存。这一点见 5.3 风险 4。
- **判窄：第 1 条。** "内核已经是这个引擎"只在**模型驱动**的意义上成立。"流程化串联"需要的是**确定性**驱动，而内核没有这个入口（见上）。"别重建"这个方向是对的，但"直接用它"不成立：用它需要新增一个入口（5.4）。另外，"没有 action/approval/receipt 语义"说得太绝对。ModuleApproval 已经有幂等键、payload 摘要、过期即拒绝、执行时间戳，只差"模块执行动作的约束力"和"回执"两块。
- **判窄：第 3 条。** 漏了 Documenting 先例（F11）。补上之后，挂点问题已经不是一道开放题，可以照 ADR-DOC-01 推出来。
- **第 2 条判定正确。** 可以说得更强（F7）：Agent24 里根本没有运行时 Skill。

**2) 薄工作流层落在 A / B / C 哪个？**

**推荐 B：第一方进程外领域 OS（暂名 `office`，与 `documents` 同构）。** 但有一个前提：**真实外部动作不能在这个模块进程里执行**，见 5.4。

| 取舍 | A 组合根薄层 | **B 进程外领域 OS** | C sidecar |
|---|---|---|---|
| 先例与已拍板方向 | 与 T11（第一方 OS 迁出内核）、ADR-DOC-01 D4（否决内核领域名词）方向相反 | 与 Documenting、Sin90、Cos72 一致 | ADR-001 的形态，今天不可用（F13） |
| OA-M1 需要改内核吗 | 要：新表、迁移、REST/WS 投影。K1 发布说明 `:89-121` 写明 store 迁移**不可回退** | **不需要**。模型走 `Models` 回调（manifest 写 `model_access: local_only`），进度走 `Events`，业务记录放模块自己的 `data_dir` | — |
| 约束性审批 | 现成：内核工具逐次审批，审批对象就是这次调用的确切输入 | M1 没有（只有 `Advise`）；M2 需要内核切片 | — |
| 外发受内核控制 | 连接器写在内核里就受控（出站门 6b.3 落地之后） | 连接器写在模块进程里就**不受控**（同 UID，没有 OS 沙箱） | — |
| 平台 | 全平台 | 仅 Unix，Windows 要等 DEP-C2（ADR-DOC-01 D2/D10） | — |

A 的真正优势只有一条：**约束性审批与外发控制是现成的**。B 可以把这一条拿回来，做法是把"有外部效果的动作"留在内核执行（5.4）。这样 B 同时得到三样东西：M1 零内核改动、不违背 T11、外发仍由内核裁决。`OFFICE-ASSISTANT-TECH-DESIGN.md:58` 倾向 (a)，给的理由是"(b) 会引入 consent 授予前置"。这个理由对 M1 不成立：M1 不向 agent loop 通告工具，用不到 consent。对 M2，A 与 B 都需要内核新增入口，只是形状不同。

**推荐的首个切片（OA-S0，≤300 行，不改内核、protocol、store）：**

1. 一页语义映射（就是 PLAN 的 OA-M1-01）。把报告 §3 的七项语义逐项映射到**现有**原语：`RunStatus`、内核工具审批、ModuleApproval 的 `Gate`/`Advise`、`ModuleTool` consent。每项加一列"**今天有没有约束力**：是 / 否 / 只在 mock 中成立"。
2. 一个**纯逻辑**的 workflow/action/receipt 状态机库，不做 I/O。放在将来的 `office` 模块里，内核不依赖它。用单测覆盖报告 §6 负例矩阵中**能在状态层判定**的几行：第 1 行（拒绝/过期/无通道 → 无动作）、第 2 行（payload 变化后旧批准失效，用 digest 比较）、第 4 行（只重试未完成的步骤）、第 5 行（取消后如实列出已完成步骤）、第 14 行（mock 回执不能升级为"已送达"）。

这一片对 A/B 的最终选择不敏感。即使作者改选 A，它也能原样搬过去，所以不会白做。

**3) 作者坚持"用 Skill 串工具"：最接近又正确的形态**

正确形态是 **"Skill 是 SOP 数据，workflow 定义是代码审过的闭集"**。Skill 只出现在 LLM 步骤里，作为带出处的材料。可直接执行的写法如下：

```yaml
# office 模块内的 workflow 定义（代码审过、随版本发布；用户不可写）
workflow: meeting_to_actions
version: 3
steps:
  - id: extract
    kind: llm_extract          # 闭集：llm_extract | human_review | action
    sop: { skill: meeting-to-tasks, version: "1.2.0", sha256: "<hash>" }   # 只作为提示材料注入
    output_schema: minutes_v1  # 结构化输出，按 schema 校验，不合格则失败
  - id: review
    kind: human_review         # 用户编辑会生成新的 artifact 版本
  - id: deliver
    kind: action
    tool: mock.notion_write    # M1 只能是 mock.*；真实工具名必须在 manifest ∩ 用户 consent 内
    payload_from: review.artifact   # 批准绑定这个 artifact 的版本和 digest
```

配套规则：

- Skill 的文本**不能**新增步骤、改动 `tool` 或目的地，也不能决定风险等级。报告 §1 表的 Skill 行写的是"Text instructions alone neither grant permission"。
- `sop.sha256` 计入**计划版本**。Skill 升级后计划版本随之改变，旧批准自动失效（与 L-APPR 的"批准不被静默复用"一致）。
- Skill 文本按 L-CTX-2 注入，要求可点名、带哈希、有大小上限。注入时作为**数据**，不作为系统指令，与 M1 召回块的做法一致（`session_memory.rs:24-30`）。
- 如果作者想要 Claude Code 那种"模型读 SOP 自己调工具"的体验，**只允许用在准备阶段**（读、整理、起草，没有外部效果）。一旦进入 `action`，就回到 workflow 层，逐次走内核审批。
- Skill 的分发与升级以后照 `SPEC-SKILL-DISTRIBUTION`（M5）执行。OfficeAssistant 不应为此提前实现 M5，首版的 SOP 直接随 `office` 模块打包。

**4) 约束与三个里程碑是否自洽？需要怎么调**

整体方向自洽，但有四处要调：

- **OA-M1-04、OA-M1-05 与推荐的 B 冲突。** PLAN 写的是"内核 store 内，含迁移"和"抽取步骤接入 run"（`PLAN-OFFICE-ASSISTANT.md:145-146`）。建议改成：记录放在模块 `data_dir`，抽取走 `Models` 回调。这样有两个好处：转录内容不进 agent loop，也就不会被写进 `mem_events`；同时也不再需要一次不可回退的内核迁移。
- **OA-M1 的"批准绑定 payload"只能在模块内、对 mock 成立。** 验收第 5、6 条（`PLAN-OFFICE-ASSISTANT.md:159-160`）要明确标注：**这验证的是 workflow 语义，不是内核约束**。不得写成"已复用内核审批"。如果用 `Advise` 落审批记录，也必须标为非约束性。
- **在 M1 与 M2 之间插入显式的内核门 OA-K**（不属于 OA 线，归 K1 线或 #735）。内容是一份内核 ADR，让"模块请求的外部动作"由内核执行并产生约束性批准（5.4）。它与 Documenting 在 ADR-DOC-01 §3 依赖表中挂的"模块调用 Agent 工具 / 模块工具授权"是**同一类需求**，应该合并成一份 ADR，而不是 OA 单独开一条。在 OA-K 落地之前，M2 不排日期。PLAN 的 D7 已经指出这个依赖，这里只是把它从"待拍板项"升格为"里程碑之间的门"。
- **M3 太宽。** 第二条 workflow 的复用、有界委托设计、LINE、扩展项评估，被放进了同一个里程碑。建议 M3 只保留"第二条 workflow 复用"及其"抽象失败"判据。有界委托另开一个设计轨道：它与 L-APPR-5/7 有张力（`APPROVAL.md:89`、`:116`），必须先走单独的法律 PR（`laws/README.md:43`），节奏不该绑在功能里程碑上。

另外，B 意味着首版**只支持 macOS/Linux**，需要作者确认。Documenting 已有同样的先例（ADR-DOC-01 D10）。

**5) 还有哪些"会被这句话误导"的风险点**

1. **"复用内核审批"。** 内核有三种审批，约束力各不相同：工具逐次审批（有约束力）、ModuleApproval `Gate`（有约束力，但闭集里只有 `schedule_callback`）、`Advise`（无约束力）。说"复用审批"而不点明是哪一种，最容易滑向 `Advise`，造成虚假的安全感。
2. **"LocalOnly = 不出本机"。** LocalOnly 只约束**模型路由**。模块进程、`http_fetch`、`shell_exec` 的网络出口今天都不受它约束（K1 发布说明 `:33`）。另外，局域网或域名形式的模型 URL 会被判为 Remote（同文 `:125`）。
3. **"复用 run"。** 在 B 中，`Models` 回调**不是 run**：没有 run 级取消，不进入 run 事件流。在 A 中走 agent loop 则会把原文写进记忆日志。两条路的代价不同，"复用 run"这句话把这个差别藏起来了。
4. **"`executed_at` / 事件 = 已完成"。** `executed_at` 只表示内核扫描器执行了闭集动作（`types.rs` 字段注释），与"对方已收到"无关。这一条与报告 §5 第 8 条同族，但落在另一个字段上。
5. **"第一方"。** ADR-DOC-01 D4 第 2 条规定：在"第一方默认安装（pin + sha256）"落地之前，第一方模块的工具也按 `External` 处理，每次调用都要审批。OA 不能因为"是我们自己写的"就拿到 `WriteLocal` 草稿例外（L-APPR-5 的例外还要求宿主核验，`APPROVAL.md:96`）。
6. **公开代理有限制：1 MiB / 30 s，不支持流式**（ADR-DOC-01 §1 表）。长转录和本地模型抽取都会超限，必须用 job + 事件的模式（Documenting 的 ADR-DOC-02 §7）。"把转录发给助手"并不是一次同步请求就能完成的。
7. **"Skill 已有五个"。** 那五个在 Components 仓库，Agent24 中没有加载器（F7）。在 Agent24 的语境里把它们说成"已有"，等于把计划说成已完成。

### 5.3 关键设计风险（按严重度排序）

1. **【Critical】模块驱动的外部动作没有约束性批准原语。** 今天最容易写出来的实现有两种：用 `Advise` 记一笔，或者模块自己检查批准后自己发送。两种都会让报告 §3 的"批准绑定最终 payload"变成文案。这是 OA-M2 之前必须由内核解决的问题（5.4）。
2. **【High】外发脱离内核。** 连接器（邮件、Notion、LINE）如果放在 `office` 模块进程或通用 `http_fetch` 里，ADR-K1-02 的出站门（6b.3）管不到它们。同 UID 进程的威胁模型本来就不防"合作但有 bug"的模块：模块里一个 bug，就可能多发一封邮件。
3. **【High】权威向内核蔓延（A 的代价）。** 业务表进入内核 store，迁移不可回退；内核出现 workflow/minutes 这类领域名词，与 T11、ADR-DOC-01 方向相反。一旦合入，后面很难再迁出。
4. **【High】第三方敏感原文进入记忆日志。** agent loop 每轮把原文写进 `mem_events`，暂停开关也不阻止这一点（`retain.rs:1-12`）。删除权与体积治理是 C3 P1 的待办（`tasks.md:25`），尚未完成。转录或邮件一旦经主对话进入，就没有可兑现的删除承诺。
5. **【Medium】抽取质量与主线带宽。** 本地模型抽取会不会漏项、会不会编造承诺，决定产品是否可用。它要由金标准门禁（PLAN F1.1/OA-M1-03）证明，不能靠页面观感。另外，OA-K 与 C3 P1、DM-SPIKE 争内核评审带宽。按规则 OA 线让位（`PLAN-OFFICE-ASSISTANT.md:8`），所以 M2 的时间不可承诺。

### 5.4 更好的整体设计：按"有无外部效果"切权威

| 层 | 归谁 | 内容 |
|---|---|---|
| 业务状态 | `office` 进程外第一方领域 OS（B） | workflow/plan 版本、artifact 版本、人工编辑、SOP Skill、receipt 的业务投影、mock 连接器 |
| 有外部效果的动作 | **内核** | 连接器是在内核注册的工具（MCP 或第一方连接器工具，`External`，声明 `target_arg`）。真实外发**只**在内核执行 |
| 二者之间的唯一接缝（OA-K，需要内核 ADR） | 内核新增一个回调：模块提交 `(tool, 确切 input, request_id)` | 内核对它跑完整流水线：consent → 逐次人审（审批对象是确切 input）→ 出站门 → 超时执行 → 哈希链审计。返回类型化结果和 receipt。`request_id` 作为业务幂等键（照 `module_approvals` 的 `UNIQUE` 先例）。**内核不自动重试**（与 ADR-DOC-01 D4 第 3 条一致） |

它与现有约束的关系：

- 第二节约束 1（唯一控制面）比 A 更强。外发代码根本不在业务层进程里，"另开执行通道"在结构上就做不到。
- 约束 2（薄工作流层、不复制权威）保留，只是挪到了进程外。
- 约束 3（Skill 定位）见 5.2 第 3 问。
- 约束 4（首版边界）不变。
- OA-K 新增的是一种 `Capability`。按 `ARCHITECTURE-LAYERS.md` §5，这是**影响面最大**的一类改动，必须先写 ADR，按 ME-4 的切法落地。但它不是 OfficeAssistant 专属：Documenting 的"模块调用 Agent 工具"（ADR-DOC-01 §3）和 Cos72 的渠道动作都需要它，所以应该合并进 #735，**不改变 C3 P1 的主线优先级**。
- **备选方案：** 连接器做成 `office` 模块自己的 `ModuleTool`。K1 契约已经有了，只缺 grant 路由。它更便宜，但外发留在模块进程里（风险 2）。如果作者为了速度选它，应该在 ADR 里把"外发不受内核出站门约束"写成显式偏离。

### 5.5 本节未核实

- `A24_GUARDIAN` 的开关逻辑只核对了变量存在，没有逐行读。
- `module_tool_runtime.rs` 中 `dispatch` 的调用都在测试模块内：依据是测试模块从 `:177` 开始，而所有调用行号都大于它。没有逐个确认每一行所在的 `mod`。
- ADR-DOC-02 §7 的 job 模式只按 ADR-DOC-01 和 `domain-os.yml` 的注释引用，没有读 ADR-DOC-02 原文。
- 5.4 的回调形状是设计提议，没有写代码或编译验证。
- 这次没有调用 Codex 做对抗复核：本会话里 codex MCP 连接失败。
