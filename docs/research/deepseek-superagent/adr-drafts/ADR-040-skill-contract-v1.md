# ADR-040：技能契约 v1（草案）

- 状态：**提议**（2026-11-01，Fable 初稿，待 Codex 挑战与 jason 拍板）
- 上位：[ADR-039 概念模型](../../../adr/ADR-039-concept-model.md) §2.3、§3 规则 3、§6
- 相关：[ADR-038 组件契约 v1](../../../adr/ADR-038-component-contract-v1.md) D9（能力 id、`skills` 占位）、`docs/specs/SPEC-SKILL-DISTRIBUTION.md`（三来源与版本播种，设计定稿未实现）、issue #1037、ADR-K1-03（逐工具授权）
- 依据：[研究初稿](../SUPERAGENT-EVOLUTION-2026-11-01-fable.md) §3.1 第 6 条；原型 `docs/004-prompts.md` Versions、`docs/002-authorization.md` Accept mode（[SOURCES](../SOURCES.md) S2、S4）

## 1. 背景

ADR-039 已定义技能：从用户目标出发、按步骤组合多项能力；内容有说明、步骤、依赖的能力、评测用例、版本；本身不持有权限；是自进化的主要对象，改动必须有版本、通过评测、经过审批、可回滚。

现状：没有加载器（#1037 OPEN；`rust/` 检索 `skill` 命中 0）；`SPEC-SKILL-DISTRIBUTION.md` 定了三来源（内置 / 分发 / 用户，优先级用户 > 分发 > 内置，同名整体取代）与版本播种规则，未实现；`agent24-memory::knowledge` 有 propose → 人工 approve 的收件箱可复用（#1037 §1）。

原型给了两条现成机制：每次保存成不可变版本，记录基线修订、前驱、来源；升级后默认文本变了，标「Default updated」并显示差异，用户选「保留我的」或「用新默认」。主动行为有 `off / observe / auto` 三档，默认 `observe`：评估照做、记录「会接受」，但不执行。

## 2. 决策

### D1 清单格式：`SKILL.md` + frontmatter，沿用社区格式再加 Agent24 字段

```yaml
---
name: weekly-report           # 技能 id，[a-z0-9][a-z0-9-]{1,31}，全局唯一
version: 1.2.0                # semver，必填（SPEC-SKILL-DISTRIBUTION §3.1）
description: { zh: …, en: …, th: … }
requires: [documents, nostr-dm]   # ADR-038 D9 的能力 id；零依赖技能为 []
mode: manual | proactive      # proactive 技能受 D5 接受模式约束；默认 manual
evals: evals/*.jsonl          # 相对技能目录；可为空，但 proactive 必须有
base: { source: bundled, version: 1.1.0 }   # 用户改过时记录基于哪个来源版本（D3）
---
（说明与步骤正文，三语分节）
```

- `name`、`version`、`description`、`requires` 必填。缺任一项拒绝加载，报 `skill_manifest_invalid`。
- `requires` 只能引用 `docs/capabilities/REGISTRY.md` 已登记的能力 id 或 `mcp-<server>`。未登记拒绝。
- 不新增 `capabilities` 字段（ADR-039 §4.1 消歧规则）。
- 外部导入（Anthropic 风格单文件 `SKILL.md`）：缺 Agent24 字段的，导入向导补 `requires: []`、`mode: manual`、`version: 0.1.0`，记 `base.source = imported`。

### D2 技能不持有权限，依赖解析在运行前

- 运行技能前，本体按 `requires` 逐项解析提供者（ADR-038 D9 多提供者规则：未选默认则拒绝并提示）。缺能力 → 提示去装，不静默失败（ADR-039 规则 2）。
- 技能步骤里的每次工具调用照常走审批门与 K1-03 授权。技能清单里的任何字段都不能放行工具。
- 技能可以在步骤里引用本体内部资源（会话、工作区、入站事件、工单），只能读，不能绕过工具写。

### D3 版本：每次改动是不可变版本，带基线

- 本体为每个技能保存版本序列：`(skill_id, version, source, content_hash, base_hash, created_at, origin)`；`origin ∈ {bundled, distro, user, imported, proposed}`。
- 三来源与播种规则按 SPEC-SKILL-DISTRIBUTION §1、§3.1：只比版本号；用户改过的不覆盖，提示有新版本。
- **「默认更新」规则**（原型 004 Versions）：用户改过的技能，当内置 / 分发来源升级时，本体保存新默认，界面显示「旧默认 → 新默认」差异与「你的版本 → 新默认」差异，用户选「保留我的」或「用新默认」。不自动合并，不做字段级合并。
- 回滚 = 把当前指针指回任一历史版本，本身也是一次版本记录。

### D4 评测与审批门

- 评测用例是 `jsonl`，每条 `{ input, expect, judge }`；`judge ∈ {exact, contains, local_model}`。`local_model` 用 LocalOnly 模型打分（Guardian 同形态），不出境。
- 新版本进入「当前」指针前必须：评测全部通过（或用户在界面上看着失败项明确接受）→ 审批（提升级操作，CORE-2 在场证明）。两步都记审计（K1-04 字段）。
- 自动生成的草案（从会话蒸馏、从失败记录改进）只能进 `proposed` 收件箱，复用 `agent24-memory::knowledge` 的 propose / approve 路径，人不批不生效。

### D5 主动技能的接受模式

- `mode: proactive` 的技能由入站事件触发（ADR-041）。每个主动技能有自己的接受模式 `off | observe | auto`，默认 `observe`。
- `observe`：触发条件满足时记录「会运行」，产出放草稿区，零外部动作。`auto`：真正运行，仍逐工具审批。
- 从 `observe` 切到 `auto` 是提升级操作；切换时界面展示 `observe` 期间的决定记录（原型 002：「review the recorded Decisions」）。

### D6 对外

能力中心「技能」栏：按技能列出名称、版本、来源、依赖的能力（缺的标红）、模式与接受模式、评测通过率、「有新版本」提示。用户看不到 `base_hash` 之类内部字段。

## 3. 备选方案及否决理由

| 备选 | 否决理由 |
|---|---|
| 技能直接声明所需工具权限，安装时一次授权 | 违反 ADR-039 §2.3「技能本身不持有权限」；原型 004 也把「工具与宿主强制的事实」放在提示词之上 |
| 按内容哈希或 mtime 判断升级 | SPEC-SKILL-DISTRIBUTION §3.1 已否决：用户任何改动都会变冲突；mtime 受 checkout / 时钟影响 |
| 评测不过也可由模型自判放行 | 违反 ADR-039 规则 3；自进化的闸必须是人 |
| 主动技能默认 `auto` | 原型默认 `observe` 并要求先看记录；Agent24 的主动行为触达用户的联系人，错一次代价高 |
| 自定义清单格式不兼容 `SKILL.md` | ADR-039 §4.1 已拍板与社区格式同词、便于导入 |

## 4. 后果

- 好处：ADR-039 规则 3 有了可执行的定义；#1037 可立项；自进化有起点且不越权。
- 代价：加载器、版本表、评测跑器、能力中心两栏都是新代码；三语说明增加技能作者负担。
- 风险：`local_model` 评测的稳定性未知；首版可只用 `exact / contains`。

## 5. 与其他 ADR 的关系

- 实现 ADR-039 §2.3、§3 规则 3、§6 第一条。
- 依赖 ADR-038 D9 能力 id 登记表（C7 建表）。
- 主动技能依赖 ADR-041（入站事件与准入）；本 ADR 不定义触发规则。
- 审批与在场证明沿用 CORE-2；审计字段沿用 ADR-K1-04。

## 6. 开放问题

- Q1 技能目录放 `~/.agent24/skills/` 还是 `<data_dir>/skills/`（ADR-037 数据目录契约）。建议后者。
- Q2 零依赖技能（prompt 模板）要不要评测用例。建议可选。
- Q3 技能能否声明「只在 LocalOnly 下运行」。建议 v1 不加字段，由依赖的能力与 K1-02 决定。
- Q4 从失败记录自动生成改进草案（真正的「越用越好」）放 v1 还是 v2。建议 v2，v1 只做人写 + 导入。
