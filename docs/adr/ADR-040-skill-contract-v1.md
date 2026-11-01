# ADR-040：技能契约 v1

索引：[架构总入口](../architecture/README.md)・[ADR 索引](README.md)

- 状态：**提议**（2026-11-01，三步评审终稿，待 jason 拍板）
- 上位：[ADR-039 概念模型](ADR-039-concept-model.md) §2.3、§3 规则 3、§6
- 相关：[ADR-038 组件契约 v1](ADR-038-component-contract-v1.md) D8、D9（能力 id、`skills` 占位）；`docs/specs/SPEC-SKILL-DISTRIBUTION.md`（三来源与版本播种，设计定稿未实现）；issue #1037；ADR-K1-03（逐工具授权）；[ADR-041](ADR-041-inbound-events-admission-and-work-items.md)（主动技能的触发来源）
- 依据：[演进终稿](../research/deepseek-superagent/SUPERAGENT-EVOLUTION-2026-11-01.md) §2 裁决 9、§4 第 ⑦ 条；原型 `docs/004-prompts.md` Versions、`docs/002-authorization.md` Accept mode（[SOURCES](../research/deepseek-superagent/SOURCES.md) S2、S4）

## 1. 背景

ADR-039 已定义技能：从用户目标出发、按步骤组合多项能力；内容有说明、步骤、依赖的能力、评测用例、版本；本身不持有权限；是自进化的主要对象，改动必须有版本、通过评测、经过审批、可回滚。

现状：没有加载器（`rust/` 下 `skill` 零命中；#1037 OPEN）；`SPEC-SKILL-DISTRIBUTION.md` 定了三来源（内置 / 分发 / 用户，优先级用户 > 分发 > 内置，同名整体取代）与版本播种规则，未实现；`agent24-memory::knowledge` 有 propose → 人工 approve 的收件箱可复用；能力 id 登记表 `docs/capabilities/REGISTRY.md` **尚不存在**，由 ADR-038 C7 建。

上午的可进化性评估建议 5 写「评测门槛以后再加」，与随后接受的 ADR-039 规则 3 矛盾。本 ADR 服从 ADR-039：评测是激活前置。

原型给了两条机制：每次保存成不可变版本，记录基线修订、前驱、来源；升级后默认文本变了，标「Default updated」并显示差异，用户选「保留我的」或「用新默认」。主动行为有 `off / observe / auto` 三档，默认 `observe`。原型没有技能的评测与审批闭环，这两项是 Agent24 自己的要求。

## 2. 决策

### D1 清单：`SKILL.md` + frontmatter，沿用社区格式再加 Agent24 字段

```yaml
---
name: weekly-report           # 技能 id，[a-z0-9][a-z0-9-]{1,31}，全局唯一
version: 1.2.0                # semver，必填
description: { zh: …, en: …, th: … }
requires: [documents, nostr-dm]   # ADR-038 D9 的能力 id；零依赖技能为 []
mode: manual | proactive      # proactive 受 D6 接受模式约束；默认 manual
evals: evals/*.jsonl          # 相对技能目录；必须至少一条用例（D5）
base: { source: bundled, version: 1.1.0 }   # 用户改过时记录基于哪个来源版本（D4）
---
（说明与步骤正文，三语分节）
```

- `name`、`version`、`description`、`requires`、`evals` 必填。缺任一项拒绝加载，报 `skill_manifest_invalid`。
- `requires` 只能引用 `REGISTRY.md` 已登记的能力 id 或 `mcp-<server>`。未登记拒绝。**本条依赖 ADR-038 C7**；C7 之前加载器只接受 `requires: []`。
- 不新增 `capabilities` 字段（ADR-039 §4.1 消歧规则）。
- 外部导入（社区单文件 `SKILL.md`）：缺 Agent24 字段的，导入向导补 `requires: []`、`mode: manual`、`version: 0.1.0`、空评测，记 `base.source = imported`；导入后是**未激活候选**，不能直接运行（D3）。
- 组件附带的技能（ADR-038 D9 `skills`）同样逐个登记：独立的内容摘要、版本、评测与激活审批。升级组件不顺带替换其附带技能的「当前」版本。

### D2 技能不持有权限，依赖解析在运行前

- 运行前，本体按 `requires` 逐项解析提供者（ADR-038 D9 多提供者规则：未选默认则拒绝并提示）。缺能力 → 提示去装，不静默失败（ADR-039 规则 2）。
- **一次运行固定技能版本与提供者绑定**：run 记录 `(skill_id, version, {能力 id → 组件 id@版本})`，执行中不换；旧 run 回放与审计按记录的版本解释。
- 技能步骤里的每次工具调用照常走审批门与 K1-03 授权。技能清单里的任何字段都不能放行工具；技能版本、评测通过、激活审批都不是授权凭证。
- 技能可以在步骤里引用本体内部资源（会话、工作区、入站事件、工单）作为只读上下文，不能绕过工具写。

### D3 安装、激活、执行三个阶段分开

- **安装**：清单校验通过即落盘为一个版本记录，状态 `installed`，不可运行。
- **激活**：评测通过（D5）→ 审批（提升级操作，CORE-2 在场证明）→ 「当前」指针指向该版本。两步都记审计（K1-04 字段）。
- **执行**：只能执行「当前」版本；`proactive` 技能还受 D6 接受模式约束。
- 零依赖技能（prompt 模板）同样走三阶段；它的评测可以只是 `exact / contains`。

### D4 版本：每次改动是不可变版本，带基线

- 版本序列：`(skill_id, version, source, content_hash, base_hash, created_at, origin)`，`origin ∈ {bundled, distro, user, imported, proposed}`。
- 三来源与播种规则按 SPEC-SKILL-DISTRIBUTION §1、§3.1：只比版本号；用户改过的不覆盖，提示有新版本。
- **「默认更新」**（原型 004 Versions）：用户改过的技能，当内置 / 分发来源升级时，本体保存新默认，界面显示「旧默认 → 新默认」与「你的版本 → 新默认」两份差异，用户选「保留我的」或「用新默认」。不自动合并。
- **回滚** = 把「当前」指针指回任一历史版本，本身也是一次版本记录，同样要审批。回滚只改变**之后**的执行；不撤销已发生的外部动作，不恢复已撤销的权限，不复活已删除的记忆（记忆 P1 §6 防复活）。

### D5 评测与审批门

- 评测用例是 `jsonl`，每条 `{ input, expect, judge }`；`judge ∈ {exact, contains, local_model}`。v1 只实现 `exact / contains`；`local_model` 用 LocalOnly 模型打分（Guardian 同形态），v2。
- 激活前评测必须全部通过，或用户在界面上看着失败项逐条明确接受（记审计）。
- 候选版本**不能修改评分器、评测用例集或隐藏失败**：评测用例集的改动是独立的版本记录，与技能正文改动分开审批。
- 自动生成的草案（从会话蒸馏、从失败记录改进、ADR-043 审查器的提案）只能进 `proposed` 收件箱，复用 `agent24-memory::knowledge` 的 propose / approve 路径，人不批不生效。v1 不做自动生成，只做人写 + 导入。

### D6 主动技能的接受模式

- `mode: proactive` 的技能由入站事件的 `inferred` 候选触发（ADR-041 D3）。每个主动技能有自己的接受模式 `off | observe | auto`，默认 `observe`；与本体全局模式（ADR-041 D4）取更严者。
- `observe`：触发条件满足时记录「会运行」，产出放草稿区，零外部动作。`auto`：真正运行，仍逐工具审批。
- 从 `observe` 切到 `auto` 是提升级操作；切换时界面展示 `observe` 期间的决定记录。

### D7 对外

能力中心「技能」栏：按技能列出名称、版本、来源、依赖的能力（缺的标红）、模式与接受模式、评测通过率、「有新版本」提示。用户看不到 `base_hash`、`origin` 之类内部字段。

## 3. 备选方案及否决理由

| 备选 | 否决理由 |
|---|---|
| 只加载 Markdown，不记版本 | 不能追踪版本与依赖；违反 ADR-039 规则 3 |
| 技能直接声明所需工具权限，安装时一次授权 | 违反 ADR-039 §2.3「技能本身不持有权限」；原型 004 也把「工具与宿主强制的事实」放在提示词之上 |
| 每次运行自动取最新版 | 不可复现，且改变已批准的行为 |
| 按内容哈希或 mtime 判断升级 | SPEC-SKILL-DISTRIBUTION §3.1 已否决 |
| 评测门槛以后再加 | 与已接受的 ADR-039 规则 3 矛盾 |
| 评测不过也可由模型自判放行 | 自进化的闸必须是人 |
| 主动技能默认 `auto` | 原型默认 `observe`；Agent24 的主动行为触达用户的联系人，错一次代价高 |
| 自定义清单格式不兼容 `SKILL.md` | ADR-039 §4.1 已拍板与社区格式同词 |

## 4. 后果

- 好处：ADR-039 规则 3 有可执行定义；#1037 可立项；自进化有起点且不越权；旧 run 可复现。
- 代价：加载器、版本表、评测跑器、能力中心两栏都是新代码；三语说明增加作者负担；`REGISTRY.md` 先于技能依赖解析。
- 风险：`local_model` 评测稳定性未知，所以放 v2；三阶段对只想「贴一段 prompt」的用户是额外步骤，零依赖技能的评测要做到一键生成。

## 5. 与其他 ADR 的关系

- 实现 ADR-039 §2.3、§3 规则 3、§6 第一条。
- 依赖 ADR-038 D9 能力 id 登记表（C7 建表）；接管 D9 `skills` 占位的内容。
- 主动技能的触发来源与接受模式见 ADR-041 D3、D4；审查器提案见 ADR-043 D4。
- 审批与在场证明沿用 CORE-2；审计字段沿用 ADR-K1-04。
- 训练归 iDoris；本 ADR 不定义任何数据上送，默认无。

## 6. 开放问题

- Q1 技能目录放 `<data_dir>/skills/`（ADR-037 数据目录契约）。建议是。
- Q2 社区 `SKILL.md` 格式支持到哪个子集（frontmatter 字段、附件、脚本）。建议 v1 只读正文与 frontmatter，不执行附带脚本。
- Q3 技能能否声明「只在 LocalOnly 下运行」。建议 v1 不加字段，由依赖的能力与 K1-02 决定。
- Q4 评测最低标准：一条 `exact` 用例够不够。建议 v1 够，S6 后按通过率数据再提。

## 7. 评审记录

- 来源：Fable 初稿 `adr-drafts/ADR-040-skill-contract-v1.md`。
- Codex 改了什么：加「安装 / 激活 / 执行分开」（D3）；加「运行固定版本与提供者绑定」（D2）；加「组件附带技能独立记录与审批」（D1）；加「候选不能改评分器」（D5）；加「回滚不撤销已发生外部动作、不恢复已撤销权限 / 已删数据」（D4）；指出评测门槛必须服从 ADR-039。
- 裁判改了什么：`evals` 改为必填并把 `local_model` 推到 v2（D1、D5）；写明 `requires` 的登记表依赖 C7、C7 之前只接受 `requires: []`（D1）；导入技能改为未激活候选（D1）；自动生成草案明确 v1 不做（D5）；补「技能版本不是授权凭证」（D2）；路径引用改为 `docs/adr/` 相对路径。
