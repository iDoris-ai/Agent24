# SKILL-FEATURE 提案：全局 Skill 能力（独立议题）

状态：**提案 / 待立项**（2026-10-16 由协调者起草，基于对 Agent24 现有架构的核对）。**不是已批准的设计，也不是实现承诺。**
来源：OfficeAssistant 讨论中"用 Skill 串工具"这一说法被核对为**前提不成立**（Agent24 内核今天没有任何 Skill 加载器），由此把"全局 Skill 能力"单独拆成一个议题。
配套 issue：见仓库 issue（本文是该 issue 的完整设计底稿，issue body 保留同样的信息）。

---

## 0. 一句话结论

**可以加，而且比最初设想便宜**：Agent24 今天**一个 Skill 都没有**，但仓库里已有**半张蓝图**（`SPEC-SKILL-DISTRIBUTION.md`，设计定稿未实现）与一个**可复用的评审门控机制**（`agent24-memory::knowledge` 的提案/批准收件箱）。
它应该作为**独立议题**推进（自己的 ADR + SPEC 更新），**不要塞进 OfficeAssistant 那条线**；但 OfficeAssistant 的 SOP 会是它的第一个真实用户。

---

## 1. 现状核对（2026-10-16，基线 `origin/main`）

| 需要的东西 | 现状 | 证据 |
|---|---|---|
| **Skill 加载器 / 注册表** | ❌ **完全没有** | `rust/` 全仓不区分大小写检索 `skill` 命中 **0**；内核没有任何 Skill 加载路径 |
| **Skill 的分发与升级设计** | 📐 **已定稿、未实现** | `docs/specs/SPEC-SKILL-DISTRIBUTION.md`（状态行："设计定稿，未实现"，归属 **M5**） |
| **三来源与优先级** | 📐 同上（设计已给） | 同上：`bundled`（二进制内嵌 / `<安装目录>/skills/`）、`distro`（`$AGENT24_DISTRO_DIR` 或 `<资源目录>/distro/skills/`）、`user`（`~/.agent24/skills/`）；优先级 **用户 > 分发 > 内置**，同名**整体取代、不做字段级合并**；`user` 目录**我们永不写入** |
| **版本驱动的升级** | 📐 同上 | 按 frontmatter `version` 比对（"修好的 Skill 要能到老用户手里，但不覆盖用户自己的改动"） |
| **外部 Skill 导入** | ❌ 没有 | 无任何外部形态（如 Anthropic 风格单文件 `SKILL.md`）的导入映射 |
| **"快速创建 Skill" 的流程** | ✅ **机制已存在，可直接复用** | `rust/crates/agent24-memory/src/knowledge.rs`（MD-7 指令层）：`propose()` 落 `pending` → **人工 `approve()` 才进入 `merged`/`triggered`**；owner-scoped 写路径；注释明确"**auto-memory is never auto-applied — a human gates it**" |
| **模型依赖** | 不构成障碍 | Skill 本身不消耗模型能力；"从会话蒸馏出草案"用本地模型即可（满足 LocalOnly 约束） |
| **指令层的既有形态** | ✅ 已有 | 同上 `knowledge.rs`：分层指令按优先级合并（CLAUDE.md 语义，最高优先级**最后**出现）、按 trigger 条件注入 |

**注意一个易混点**：Agent24 里叫 `KnowledgeBase` 的东西是**指令层**（分层指令 + 触发注入 + 评审收件箱），**不是**检索式文档知识库（那在 Components/WeKnora 那条线，且今天是非权限感知的原型）。

---

## 2. 要做的东西（三块）

| # | 切片 | 内容 | 依赖 |
|---|---|---|---|
| S1 | **加载器 + 注册表 + 三源 + 版本比对** | 读指令包（frontmatter：`name` / `version` / `description` / `triggers` + 正文），按三源与优先级装配；同名整体取代；按 `version` 决定是否升级；`user` 目录只读不写 | 无（照既有 SPEC 实现） |
| S2 | **导入通道（兼容外部形态）** | 把外部 Skill（例如单文件 `SKILL.md` + 资源目录）规范成我们的 frontmatter 与目录布局；**只搬文本与资源，不搬权限**；导入必须落进 `distro`/`user` 源并**走同一套评审** | S1 |
| S3 | **快速创建流程** | 从一次成功会话/一段 SOP 蒸馏出 SKILL.md **草案** → 进入已有 `propose/approve` 收件箱 → **人工批准后才生效**；附"草案质量检查"（是否有触发器、是否有输入输出约定、是否声称了它没有的权限） | S1 + 已有 `knowledge.rs` 收件箱 |

**明确不做**：Skill 不执行任何工具调用（执行仍在 run/工具流水线里）；Skill **不授权**；Skill 不引入新的审批权威。

---

## 3. 三条必须写进契约的硬约束

1. **Skill 文本不给权限，也不证明连接器可用。**
   依据：`docs/agent/research/T007-office-assistant-report.md` §1 表的 Skill 行（"Text instructions alone neither grant permission nor prove that a connector is available"）。
2. **Skill 不能决定风险等级、不能自己新增步骤或改变目的地。**
   若允许，等于让一段文本改变审批结果——审批必须只由内核按动作与 payload 裁决。
3. **导入外部 Skill 必须走与内置同等的评审。**
   否则外部文本就是一条绕过审批的指令通道。

---

## 4. 与其它线的关系

| 线 | 关系 |
|---|---|
| **OfficeAssistant** | 它的 **SOP**（"meeting_to_actions" 这类）正是 Skill 的第一个真实用户——但注意 OA 计划里已明确：**SOP 只作为材料注入 LLM 步骤，串接由代码审过的 workflow 闭集承担**。Skill 落地后，SOP 的**存放与升级**可以转为 Skill 机制，**编排责任不变** |
| **`SPEC-SKILL-DISTRIBUTION.md`** | 本议题应当**接上它**而不是另起一套：那份 SPEC 已定 Sources/Ownership/优先级/升级规则；本议题补的是**加载器实现 + 导入 + 创建流程**，如有冲突以 SPEC 的既有决定为准（或先改 SPEC 再实现） |
| **`docs/laws/`** | 需要点名受影响的法律（`laws/README.md` 要求改变可观察行为的 PR 逐条点名）。至少涉及"什么能进入上下文"（`CONTEXT.md`）与"审批/授权"（`APPROVAL.md`）两条 |
| **K1 consent/审批线** | Skill 不新增授权路径，因此**不扩 K1 的 consent 面**；但"Skill 影响进上下文的内容"要在 `CONTEXT.md` 的框架下论证 |
| **主线优先级** | 本议题**不改变**当前主线（C3 记忆 P1）。它是独立 backlog；何时插入由作者定 |

---

## 5. 待拍板 / 待设计

| # | 待定 | 为什么必须先定 |
|---|---|---|
| 1 | Skill 的**触发方式**：只做"按 trigger 注入上下文"，还是允许模型**显式选择**某个 Skill（工具式调用）？ | 后者会让 Skill 变成"模型可选的指令来源"，安全论证与前者不同 |
| 2 | Skill 能否携带**资源**（脚本/模板/数据）？若可以，资源是否允许被执行？ | 允许执行就等于引入一条新的执行通道（与"内核唯一控制面"冲突） |
| 3 | 导入的**兼容目标**：先支持哪一种外部形态（单文件 SKILL.md？某平台的 zip 结构？） | 决定 S2 的工作量与测试面 |
| 4 | "快速创建"的**起草者**：本地模型还是人工 | 涉及 LocalOnly 与内容外发边界 |
| 5 | Skill 的**作用域**：全局 / 按 workspace / 按组织（分发源） | 与 C11 组织化及 distro 源的设计耦合 |
| 6 | 是否需要一个**签名/哈希**字段（对 `distro` 源） | 与"公开构建必须自足"（L-DIST-1 拟）及供应链信任有关 |

---

## 6. 成本判断（粗估，供排期参考）

- **S1**：小到中等（读文件 + 合并优先级 + 版本比对 + 进上下文；不碰审批与执行）。
- **S2**：中（取决于要兼容几种外部形态；纯搬运 + 评审接入）。
- **S3**：小到中等（收件箱已有；主要是"蒸馏 + 质量检查"的提示词与流程）。
- **必须的前置**：一次 ADR（决定第 5 节的 1/2/5 三条）+ 一次 `SPEC-SKILL-DISTRIBUTION.md` 状态更新。
- **不涉及**：store 迁移、审批法律变更、新的执行通道。

---

## 7. 未核实

- `SPEC-SKILL-DISTRIBUTION.md` 全文未逐行读完（只读了 §0–§2 与状态行）；其中"分发缝"的具体目录约定与升级判定细节需在实现前通读。
- 没有核对 iDoris-Components 侧那五个 Skill（`meeting-to-tasks` / `mail-draft` / `line-reply` / `notion-write` / `kb-ingest`）的实际文件形态——它们可能是最合适的第一批"导入"样本。
- 没有检查桌面/CLI 是否有任何隐藏的 Skill 相关读取路径（只做了 `rust/` 的检索）。
