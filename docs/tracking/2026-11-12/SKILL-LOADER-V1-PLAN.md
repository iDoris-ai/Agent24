# ADR-040 技能加载器 v1 实施方案

- 日期：2026-11-12
- 状态：方案草案，待 jason 拍板 §5 与 §8。只写文档，没有改代码。
- 依据：[ADR-040](../../adr/ADR-040-skill-contract-v1.md)（已接受，零代码）；上位 [ADR-039](../../adr/ADR-039-concept-model.md) §2.3、§3 规则 3、§6；[ADR-038](../../adr/ADR-038-component-contract-v1.md) D8、D9；[SPEC-SKILL-DISTRIBUTION](../../specs/SPEC-SKILL-DISTRIBUTION.md)；[REGISTRY.md](../../capabilities/REGISTRY.md)；[ADR-K1-03](../../adr/ADR-K1-03-enable-authorization-trust-revocation.md)；[ADR-041](../../adr/ADR-041-inbound-events-admission-and-work-items.md)；issue #1037。
- 本工作树没有 `docs/tracking/2026-11-12/C9-REQUIRES-AUTHZ-PLAN.md`。C9 的含义以 ADR-038 D7 为准：组件 `requires` 接入授权，尚未接线。
- 与 SPEC、#1037 冲突时，以已接受的 ADR-040 为准。冲突写在 §3、§8，不改 SPEC 原文。

## 1. 一页结论

技能今天仍然不能加载。ADR-040 把技能定义成一份带版本的 `SKILL.md`：声明依赖哪些已登记能力，本身不持有权限；安装、激活、执行分开；激活前要过评测和人的审批；改动可回滚。仓库里只有这份契约、一份未实现的分发规格，以及 Documents 清单上强制为空的 `skills` 占位。

v1 做的是这条手动路径：校验清单、把不可变版本放进数据库、用 `exact` / `contains` 检查拼出来的提示文本、人在桌面确认后把「当前」指针指到该版本、用户点名要跑哪个技能时把正文注入这一次 run 的 system 消息。注入不增加工具，也不代替审批。

v1 不做主动技能、不做模型打分、不做从会话自动起草、不执行技能目录里的脚本、不把技能字段写进组件锁。技能加载器不依赖 C9 才能上线。C9 管的是组件向本体要权限；技能的 `requires` 只是能力 id，解析完仍然要走现有的工具审批和 K1-03。

能力中心现在是导航目录，不是「能力 / 技能」两栏。v1 在同一页加两栏：能力栏只读现有组件清单，技能栏只读新的技能 API。旧的固定到侧边栏的交互留在原地。

## 2. v1 范围

下面「要做」是 ADR-040 已写明、且没有被同一份 ADR 推到 v2 或写明不做的部分。「不做」是 ADR 原文已经排除的。

### 要做

- **D1 清单。** `SKILL.md` 加 frontmatter。`name`、`version`、`description`、`requires`、`evals` 必填，缺任一项拒绝加载，报 `skill_manifest_invalid`。`requires` 只能是 `REGISTRY.md` 已登记的能力 id，或 `mcp-<server>`。不新增 `capabilities` 字段。外部导入缺字段时，向导补 `requires: []`、`mode: manual`、`version: 0.1.0`、空评测，记 `base.source = imported`，导入后是未激活候选，不能直接运行。原文：`docs/adr/ADR-040-skill-contract-v1.md:22-41`。
- **D2 运行前解析依赖。** 本体按 `requires` 找提供者；同一能力多个提供者且用户没选默认时拒绝并提示。一次 run 固定 `(skill_id, version, {能力 id → 组件 id@版本})`。技能里的任何字段都不能放行工具。原文：`:43-48`。
- **D3 三阶段。** 安装后状态 `installed`，不可运行。激活要先评测通过（或用户逐条接受失败），再经提升级审批，然后「当前」指针指向该版本。执行只能跑「当前」版本。零依赖技能同样走三阶段。原文：`:50-55`。
- **D4 不可变版本与回滚。** 版本记录含来源、内容哈希、基线、时间、`origin`。用户改过的不覆盖；来源升级时展示两份差异，用户选「保留我的」或「用新默认」，不自动合并。回滚是把指针指回历史版本，本身要审批，只影响之后的执行。原文：`:57-62`。
- **D5 的 v1 子集。** 评测文件是 jsonl，每条 `{ input, expect, judge }`，`judge` 只实现 `exact` 和 `contains`。激活前全部通过，或用户看着失败项逐条接受并记审计。评测用例的改动与正文分开审批。原文：`:64-68`。`local_model` 与自动生成草案明确不在 v1（`:66-69`）。
- **D7 技能栏要显示的字段。** 名称、版本、来源、依赖的能力（缺的标红）、模式与接受模式、评测通过率、「有新版本」。用户看不到 `base_hash`、`origin`。原文：`:77-79`。v1 没有主动技能，接受模式列固定显示「手动」。

### 不做（ADR 已写明）

- **D6 主动技能。** ADR 状态行：「D6 主动技能 v1 不做」（`:4`）。`mode: proactive`、`off | observe | auto`、入站 `inferred` 触发，都不实现。
- **D5 `local_model`。** 「v1 只实现 `exact / contains`；`local_model` … v2」（`:66`）。
- **D5 自动生成草案。** 「v1 不做自动生成，只做人写 + 导入」（`:69`）。因此不接 ADR-043 D4 审查器提案，也不做 #1037 的 S3「从会话蒸馏」。
- **C7 之前只接受空 `requires` 的限制已经解除。** ADR 写明登记表已建（`:4`、`:38`）。v1 按登记表校验，不再把非空 `requires` 整批拒绝。

## 3. 现状盘点与差距

核对日期 2026-11-12，本工作树 `docs/skill-loader-v1-plan` @ `bb03081e8`。

| 需要的东西 | 现状 | 证据 |
|---|---|---|
| 技能加载器 | 没有。`rust/` 里 `skill` 只出现在 Documents 占位和测试读 ADR 的断言 | `documents_catalog.rs:51`、`:235-236`；`component_routes/tests.rs:1155-1163`、`:3278-3279` |
| 清单占位 | Documents 的 `skills` 必须是空数组，非空则清单非法 | `component.json:27`；`documents_catalog.rs:235-236`；测试 `:3278-3279` |
| 组件锁 | `deny_unknown_fields`，没有 `skills` 字段 | `agent24-components/src/lock.rs:12-41` |
| 能力 id 登记表 | 已有六条：`model-gateway`、四个 `nostr-*`、`documents`。MCP 不进表 | `docs/capabilities/REGISTRY.md:8-14`、`:34-45` |
| 多提供者默认 | 未实现。全仓没有 `components.json` 的读写，只有 ADR 句子 | `ADR-038-component-contract-v1.md:324-327`；`docs/tracking/2026-11-04/ARCH-OPTIMIZATION-PLAN.md:303` |
| 分发规格 | 设计定稿，未实现。三来源、只比版本号、用户改过不覆盖 | `SPEC-SKILL-DISTRIBUTION.md:3`、`:21-34`、`:79-90`、`:107-114` |
| 数据目录 | `<data_dir>` 默认 macOS/Linux 为 `$HOME/.agent24`，可用 `AGENT24_DATA_DIR` 覆盖 | `agent24-protocol/src/data_dir.rs:1-12` |
| 指令收件箱 | `propose` 落 pending，`approve` 才进入生效集合。这是指令层，不是技能 | `agent24-memory/src/knowledge.rs:1-15`、`:92-111` |
| run 怎么组上下文 | 快照是：记忆召回、可选暂停通知、会话历史、用户这句话。没有技能正文 | `agent24-agent/src/lib.rs:2439-2506` |
| run 怎么给工具 | 每轮 `tool_specs_for`：计划模式给只读子集，否则给已注册且在白名单里的工具 | `lib.rs:2923-2957`、`:3406-3411`；`agent24-tools/src/lib.rs:1756-1857` |
| 工具从哪来 | 内建四个，加 `register_module_tool`。没有「按技能增减工具」的入口 | `agent24-tools/src/lib.rs:1812-1857` |
| 消息来源 | 闭集：`HostRecall`、`UserPrompt`、`SessionHistory`、`Assistant`、`ToolResult`、`HostSystem`。没有技能来源 | `agent24-store/src/message_origin.rs:22-32` |
| 能力中心界面 | 一页导航目录（助手、文档、模型、语音等），按组固定到侧边栏。不读组件 `provides`，没有技能栏 | `apps/desktop/src/renderer/pages/Capabilities.tsx:42-89`；`capabilities/registry.ts:8-23` |
| 组件状态 API | `GET /api/v1/components` 已有，Everyday，操作为 `components.read` | `route_policy.rs:67`；`operations.rs:52-54` |
| 提升级确认 | 桌面 presence 通道已有：`PresenceRead` / `PresenceDecide` | `operations.rs:44-46`；`presence_desktop.rs:6-7` |
| 存储迁移 | 最新是 `0035_inbound_cursor.sql`。没有技能表 | `rust/crates/agent24-store/migrations/` |
| C9 | `requires` 只被解析和展示，没有授权路径读取它 | `ADR-038-component-contract-v1.md:288-295` |
| issue #1037 | 仍 OPEN。正文是 2026-10-16 的盘点：当时 `rust/` 对 `skill` 零命中，并建议做加载器、导入、从会话蒸馏。蒸馏与 ADR-040 D5 冲突 | `gh issue view 1037` |

差距就这一句：契约和登记表在，运行时没有技能对象。run 循环只认本体提示、记忆和已注册工具；能力中心只认导航项。

和旧文档的三处冲突（实现时以 ADR-040 为准）：

1. SPEC §3.1 把内置/分发技能**写入** `~/.agent24/skills/`，来源版本更高就覆盖（`SPEC-SKILL-DISTRIBUTION.md:79-86`）。ADR-040 D4 要求用户改过的不覆盖，并让用户在两份差异里选择（ADR `:60-61`）。
2. SPEC §5 把 Skill 说成「能力」（`:129`）。ADR-039 把技能和能力分成两个概念（`ADR-039-concept-model.md:40-48`）。
3. #1037 的 S3 要做自动起草。ADR-040 D5 写明 v1 不做（`:69`）。

## 4. 目标设计

### 4.1 清单格式与存放

目录约定（待拍板，推荐见 §5 Q1）：

```
<data_dir>/skills/<name>/SKILL.md
<data_dir>/skills/<name>/evals/*.jsonl
```

`<name>` 等于 frontmatter 的 `name`。frontmatter 字段以 ADR-040 D1 的示例为准（`:24-35`），不再加字段。`mode` 缺省当 `manual`。v1 见到 `mode: proactive` 直接拒绝安装，错误码 `skill_proactive_unsupported`，避免装上一个不会跑的主动技能。

三来源仍按 SPEC §1 的优先级 **用户 > 分发 > 内置**，同名整体取代、不做字段级合并（`:33-34`）。位置：

| origin | 只读输入位置 | v1 谁来写 |
|---|---|---|
| `user` | `<data_dir>/skills/<name>/` | 只由用户或「导入」API 写入这一棵目录 |
| `distro` | `$AGENT24_DISTRO_DIR/skills/`，否则 `<资源目录>/distro/skills/` | 本体不写 |
| `bundled` | `<安装目录>/skills/` | 本体不写 |
| `imported` | 导入 API 落进 `user` 那棵目录，frontmatter 记 `base.source: imported` | 导入 API |
| `proposed` | 不落盘 | v1 不产生 |

权威状态在数据库，不在目录。目录是输入。启动时扫描三个来源，校验通过的才成为一条 `installed` 版本。校验失败的来源留在磁盘上，列表里标 `skill_manifest_invalid`，不进入「当前」。

默认安装时 `<data_dir>` 就是 `~/.agent24`（`data_dir.rs:8`），所以和 SPEC 写的 `~/.agent24/skills/` 在默认路径上是同一个目录。设了 `AGENT24_DATA_DIR` 就只认数据目录，不另写一份到家目录。

### 4.2 依赖解析，以及它和 C9 的关系

技能 `requires` 的元素是**能力 id**（ADR-038 D9，`ADR-038:311-314`；登记表 `REGISTRY.md:3`）。组件锁里的 `requires` 是另一回事：本体权限 `kernel` 与 `operations`（`lock.rs:110-117`，ADR-038 D7 `:292`）。两套字段同名，解析代码必须分开类型，不能复用同一个结构体。

一次执行前：

1. 读该版本的 `requires`。
2. 对每个能力 id，在 `GET /api/v1/components` 已经在用的清单里找 `provides[].id` 相同、且状态可用的组件。
3. 零个：拒绝这次执行，错误给用户看，带登记表里的中文名和「首个提供者」。不静默跳过（ADR-039 规则 2，`ADR-039:57`）。
4. 多于一个且该能力 id 没有默认提供者：拒绝并提示去选。不自动挑（ADR-038 D9 `:324-327`）。
5. 恰好一个，或已有默认：绑定 `组件 id` 加清单上的 `version`。
6. 把绑定写进这次 run，执行中不改（ADR-040 D2 `:46`）。

**技能不持有权限。** 绑定只说明「这次用哪个组件来提供这个能力」。它不是 capability token，不是 K1-03 的逐工具同意，也不是审批通过。步骤里的每次工具调用仍走 `ToolRegistry` 的白名单和审批门（`agent24-tools/src/lib.rs:1756-1803`），以及已经落地的模块同意记录。技能版本、评测通过、激活审批都不能让某次工具调用跳过这些门（ADR-040 `:47`；ADR-K1-03 `:19-32`：安装、导入、签名都不是同意）。

**不依赖 C9 才能做。** C9 要接的是组件锁的 `requires` ∩ `Grants` ∩ 用户确认（ADR-038 `:288-295`）。技能加载器不读那组字段，也不替组件申请 `operations`。v1 缺能力只表示「没装」或「没选默认提供者」。等 C9 落地后，可以另加一态「组件已装但本体权限未授」，那一态不属于本方案的任何一片。在 C9 之前，组件工具该被拒的，仍由现有审批和同意记录拒绝。

v1 要补的是 ADR-038 写了但代码没有的「默认提供者」选择。存放见 §4.8。选择本身不是授权。

`mcp-<server>` 不在登记表里（`REGISTRY.md:34`）。解析时认 `GET /api/v1/components` 里形态为 MCP、`provides` 含该 id 的那一条。id 不合法或重复、因而没有 `provides` 的 server（`REGISTRY.md:42-45`）对技能来说就是缺能力。

### 4.3 安装、激活、执行

**安装。** 清单和评测文件分开校验。清单通过即插入一条不可变版本，状态 `installed`。空评测允许安装（导入向导会写出空评测，ADR `:40`），但不允许激活。`installed` 的版本不能被 run 选中。

**激活。** 顺序固定，不能并成一个请求：

1. 评测跑器对**这一对**（正文版本，评测集版本）给出通过或逐条失败。
2. 有失败时，请求里必须带用户逐条勾选的失败 id。没勾全就拒绝。
3. 通过或勾全之后，走提升级（现有桌面 presence，`presence_desktop.rs:6-7`）。用户拒绝则指针不动。
4. 指针改为该版本，写审计。审计只记 `skill_id`、版本、内容哈希、结果码，不记正文（ADR-K1-04 `:15`、`:45`）。

**执行。** 只有指针指向的版本能跑。run 请求显式带 `skill_id`；不带则行为与今天完全一样。服务端忽略客户端传来的版本号，只用指针上的版本。解析失败则不起 run。

回滚走同一条激活路径，目标改成一条历史版本：再评测（评测集用那条历史自己的）、再提升级、再移动指针。不撤销已经发生的工具副作用，不恢复已撤销的同意，不恢复已删除的记忆（ADR-040 `:62`）。

### 4.4 版本与「默认更新」

表意（列名实现时可以缩短，语义不能少）：

- `skill_revision`：`(skill_id, version, kind, source, content_hash, base_hash, created_at, origin, body_or_evals)`。`kind` 只有 `body` 和 `evals`。同一 `skill_id + version + kind` 只能有一行。正文改动和评测改动是两行，分开激活（ADR-040 `:68`）。
- `skill_current`：每个 `skill_id` 一行，指向已激活的 body 修订和 evals 修订。没有这行就是未激活。
- `run_skill_binding`：`(run_id, skill_id, version, content_hash, providers_json)`。providers 是能力 id 到 `组件 id@版本` 的映射。旧 run 没有这行，读取时当「这次没使用技能」。

`origin` 闭集：`bundled`、`distro`、`user`、`imported`、`proposed`。v1 不写入 `proposed`。

用户改过的判定用哈希，不用 mtime（SPEC `:88-90` 与 ADR 备选表 `:88` 一致）。内置或分发出现更高版本时：插入一条新的 `installed` 修订，**不**改 `skill_current`。界面同时给出「旧默认 → 新默认」和「你的版本 → 新默认」。用户选「用新默认」就是一次激活；选「保留我的」只把提示标成已读。不自动合并（ADR `:61`）。

bundled 技能改内容必须升 `version`。CI 门沿用 SPEC §3.2（`:92-97`），单独一条工作流，不塞进解析器那片。仓库里还没有 `skills/` 时，门先对着即将新增的 `skills/<name>/SKILL.md`。

### 4.5 评测门

jsonl 每行一个对象：`input`、`expect`、`judge`。`judge` 不是 `exact` 或 `contains` 则整份评测非法。

v1 **不调用模型**。跑器把「技能正文 + 这条 input」拼成将要注入的 system 文本，再比较：

- `exact`：拼好的文本与 `expect` 全等。
- `contains`：`expect` 是拼好的文本的子串。

这只证明「激活后注入的就是这份文字」，不证明模型会照着做。模型打分是 v2 的 `local_model`（ADR `:66`）。评测不注册工具、不发网络、不写工作区。带 `requires` 的技能在 v1 也只评这段文字；缺能力不在评测里假装通过，激活之后的执行仍会按 §4.2 拒绝。

通过率 = 通过条数 / 条数，给技能栏显示。零条时通过率显示为空，激活按钮不可用，除非走导入之后用户补上至少一条（ADR D1 `evals` 必填是加载条件；导入向导允许先落空评测，`:40`，激活仍要 D5 的至少一条，`:55`、`:66-67`）。

用户逐条接受失败：每一条失败有稳定 id（文件相对路径 + 行号）。接受记录进审计，不改评测文件，不把失败改写成通过。

### 4.6 如何进入 run_loop

**推荐：显式选中的「当前」技能，整段注入 system，不做成工具。** 取舍待拍板（§8）。

注入点是 `execute` 组装首包快照的地方（`agent24-agent/src/lib.rs:2439-2506`），放在暂停通知之后、会话历史之前，这样 `normalize` 会把它和别的 system 段合成一条（同文件 `:447-466`）。不改 `run_loop` 里每轮重算工具的逻辑（`:2940-2957`）。技能正文不进入 `ToolRegistry`。

来源不能复用 `HostSystem`。那个变体今天专指记忆暂停通知（`message_origin.rs:31-32`）。技能正文要新的来源种类，至少带 `skill_id`、`version`、`content_hash`，并且走新的迁移。否则审计和出站裁剪无法把「内核通知」和「用户激活的技能文字」分开。出站时技能文字可以送给模型（它就是提示），但来源标签必须在。

不提供 `use_skill` 一类工具。模型不能自己换技能、不能读未激活版本、不能因为技能正文里写了工具名就多看到一个工具。

没带 `skill_id` 的 run：快照与今天相同。这是回归哨兵。

### 4.7 能力中心最小界面

同一页两个栏，不新开产品名词。

- **能力。** 只读。把 `GET /api/v1/components` 的 `provides` 展平：能力 id、三语名（没有就用 id）、提供者组件 id 与状态、是否为该能力的默认提供者。缺提供者的登记表 id 也列出，并标「未安装」。这一栏不提供安装按钮（安装仍走现有组件流程）。
- **技能。** 只读列表加三个动作：激活、回滚、在「有新版本」时选择保留或改用新默认。列就是 D7 那一组（ADR `:79`）。缺的依赖标红。不展示 `base_hash`、`origin`、哈希原文。
- **不动**现有侧边栏固定逻辑（`Capabilities.tsx:62-79`，`registry.ts:8-23`）。那是导航，不是 ADR-039 的能力栏。两栏做完之后，导航目录仍可用。

激活、回滚、接受失败项、改用新默认，都走桌面已有的 presence 确认，不在渲染进程里直接改指针。

### 4.8 API 与 CLI

新操作，挂到现有路由表（`route_policy.rs` 的 `POLICIES`）和 `Operation`（`operations.rs:4-61`）：

| 方法与路径 | 操作 | 级别 | 作用 |
|---|---|---|---|
| `GET /api/v1/skills` | `skills.read` | Everyday | 列表。不含正文 |
| `GET /api/v1/skills/{id}` | `skills.read` | Everyday | 一个技能的当前指针、已安装版本、依赖是否齐、评测摘要 |
| `POST /api/v1/skills` | `skills.install` | Everyday | 安装或导入为 `installed`。不移动指针 |
| `POST /api/v1/skills/{id}/eval` | `skills.read` | Everyday | 只跑评测，不激活 |
| `POST /api/v1/skills/{id}/activate` | `skills.activate` | Elevated | 激活或回滚。要 presence |
| `PUT /api/v1/capabilities/{id}/provider` | `skills.provider` | Everyday | 记下该能力 id 的默认提供者。不发权限 |

`RunCreate` 增加可选 `skill_id`。缺省省略，旧客户端不受影响。服务端不接受 `skill_version`。

CLI 与桌面共用上述 HTTP，不直开数据库。`agent24 skill list|show|install` 对着 read/install。`activate` 在核实 CLI 能否走完 presence 之前**不做**（§9）。默认提供者的选择放在桌面能力栏，v1 CLI 不提供。

错误码闭集：`skill_manifest_invalid`、`skill_proactive_unsupported`、`skill_capability_unknown`、`skill_capability_missing`、`skill_provider_unselected`、`skill_not_active`、`skill_eval_failed`、`skill_eval_empty`。

### 4.9 存储

新表都在 `agent24-store` 的**新迁移**里，编号接在 `0035_inbound_cursor.sql` 之后。不改旧迁移。

建议拆成两条迁移，避免一片又改表又改消息来源：

1. `0036_skill_revisions.sql`：`skill_revision`、`skill_current`、`skill_provider_choice`、`run_skill_binding`。`skill_provider_choice` 就是 D9 那个「每个能力 id 一个默认提供者」。不新建 `components.json`，免得再出现一个与 SQLite 双写的文件。路径名仍可在文档里叫「默认提供者」，用户看不到文件名。
2. 下一条迁移只加技能消息来源（实现那片再编号）。来源种类是安全相关的既有模式（`0027`–`0029` 已这么做过），和技能表分开审。

`skill_revision` 保存正文和评测原文，因为回滚和「默认更新」的差异要能离线算出来。审计表不保存原文。

这些迁移是数据写入语义，按仓库约定，对应 PR 要等 PR-Daemon APPROVE 再合。

组件锁和 Documents 清单在 v1 **不**加非空 `skills`。现有测试继续要求空数组（`component_routes/tests.rs:3278-3279`）。组件附带技能将来走同一套安装 API，`origin` 用 `bundled`，另记 `component_id`；升级组件不改 `skill_current`（ADR-040 `:41`）。那一条等到真有一个组件带技能目录时再开片，不在本 v1。

## 5. 对 ADR-040 开放问题的推荐

四条都是 ADR §6 已有的建议。这里采纳原文建议，并补一句实现含义。**四条都待 jason 拍板。**

- **Q1 目录放 `<data_dir>/skills/`。推荐：是。** 与 ADR-037 的数据目录是同一个根（`data_dir.rs:1-12`）。默认安装因此也满足 SPEC 写的 `~/.agent24/skills/`。不要在 `AGENT24_DATA_DIR` 之外再写一份。
- **Q2 社区 `SKILL.md` 读到哪。推荐：v1 只读 frontmatter 和正文，不执行附带脚本，不加载脚本、二进制和未声明的附件。** 多出来的文件当未知内容拒绝导入，不静默忽略后执行。
- **Q3 能否声明「只在 LocalOnly 下运行」。推荐：v1 不加字段。** 数据能不能出本机，仍由技能用到的工具和 K1-02 决定。技能正文不是出口政策。
- **Q4 一条 `exact` 够不够。推荐：v1 够。** 激活至少一条 `exact` 或 `contains`。通过率以后再提高门槛。空评测不能激活。

## 6. 分片计划

集成分支用 `ab/skill`（本方案不创建分支）。task 分支 `ab/skill-NN-<短名>`，PR 基线是 `ab/skill`，不是 `main`。下表「规模」是该片产品代码的估计行数，不含测试。超过 300 就再拆。没有一片依赖 C9。

| 片 | 目标 | 涉及文件 | 规模 | 依赖 | 验收（含变异） | 风险 | 建议派给 |
|---|---|---|---|---|---|---|---|
| SL-1 | 纯解析：D1 字段、semver、能力 id 字符集、`skill_manifest_invalid`、拒绝 `proactive` | 新 `rust/crates/agent24-skill`（只解析，不读盘、不入库） | ~200 | 无。不依赖 C9 | 缺任一必填字段、未登记 id、`capabilities` 字段、`proactive` 都失败。删掉「缺字段即失败」后，缺 `evals` 的夹具变绿，即变异失败 | 低 | Rust 代理 |
| SL-2 | 迁移 0036：修订、当前指针、默认提供者、run 绑定。只插入不覆盖；指针单独更新 | `agent24-store/migrations/0036_skill_revisions.sql` 与对应 store API | ~250 | SL-1 的类型。不依赖 C9。**数据写入，等 PR-Daemon APPROVE** | 同一 `(skill_id, version, kind)` 插第二次失败。没有指针时「当前」查询为空。把「冲突则失败」改成覆盖后，第二条测试变红 | 迁移写坏旧库 | Rust 代理 |
| SL-3 | 扫描三来源，校验后写入 `installed`。更高版本不改指针。用户目录只读，扫描器不写目录 | `agent24-skill` 的扫描；daemon 启动时调用 | ~250 | SL-2。不依赖 C9 | 四种播种：没有、更高、相同或更低、哈希不同。第四种不覆盖。把「哈希不同则不动」改成按 SPEC §3.1 覆盖写入，该测试变红 | 扫到家目录以外的路径 | Rust 代理 |
| SL-4 | `POST/GET /api/v1/skills`。安装后不可被 run 选中 | `agent24d` 路由、`operations.rs`、`route_policy.rs`；CLI 只读客户端可另片 | ~250 | SL-2、SL-3。不依赖 C9 | 安装响应状态是 `installed`，`GET` 的当前指针仍空。若有人把安装写成顺手激活，测试变红 | 新 Operation 漏登记会让能力模式拒路由或误放行 | Rust 代理 |
| SL-5 | `exact` / `contains` 跑器，不调模型、不碰工具 | `agent24-skill` 评测模块；`POST .../eval` | ~200 | SL-1。不依赖 C9 | `contains` 命中子串则过，改一个字则不过。把比较对象从「拼好的提示」改成「永远等于 expect」，负例变绿，即变异失败 | 低 | Rust 代理 |
| SL-6 | 激活与回滚：评测门、逐条接受、presence、审计、移动指针 | `agent24d` 激活路由；审计 action 白名单 | ~280 | SL-2、SL-5、现有 presence。不依赖 C9。**授权与审计，等 PR-Daemon APPROVE** | 未评测、评测失败且未逐条接受、presence 拒绝，指针都不动。删掉 presence 检查后，未确认也激活成功，测试变红。回滚不删除 `run_skill_binding` 旧行 | 把技能激活做成了授权 | Rust 代理 |
| SL-7 | 执行前解析能力 id；零个拒绝；多个且无默认拒绝；写出 `run_skill_binding`。默认提供者用 SL-2 的表 | daemon 执行前检查；`PUT .../provider` | ~250 | SL-2、现有组件清单。**不读组件 `requires`，不依赖 C9** | 两个提供者且无默认时不起 run。自动挑第一个的变异会使该测试变绿（失败）。`requires: [documents]` 不会写出任何 `Operation` 或同意记录 | 和 C9 的 `requires` 混类型 | Rust 代理 |
| SL-8 | `RunCreate.skill_id`；快照注入正文；新消息来源。不改工具列表。无 `skill_id` 的快照与现在一致 | `agent24-agent/src/lib.rs` 快照段；`message_origin.rs`；一条新迁移；协议 `RunCreate` | ~250 | SL-6、SL-7。不依赖 C9。**来源标签与数据写入，等 PR-Daemon APPROVE** | 带技能的首包多一条带来源的 system，工具广告与不带技能时相同。把技能正文塞进 `tool_specs_for` 的变异会让「工具列表不变」变红。无 `skill_id` 的快照字节级对比（来源序列）与基线一致 | 注入变成第二套系统提示且无法追溯 | Rust 代理。改 `agent24-agent` 后要跑 `cargo test -j 4 -p agent24d --bin agent24d` |
| SL-9 | 导入：缺字段按 D1 补齐，`imported`，空评测，状态 `installed`，随后激活被 `skill_eval_empty` 拒绝 | 导入 API（可并入 SL-4，若合并后非测试超过 300 则保持独立） | ~200 | SL-4、SL-5。不依赖 C9 | 一份只有正文的社区 `SKILL.md` 导入成功但不能激活。补一条 `exact` 之后才能走 SL-6。导入路径若直接把指针写上，测试变红 | 外部正文绕过激活 | Rust 代理 |
| SL-10 | CLI：`skill list`、`show`、`install`。REST 客户端，不开库 | `agent24-cli/src/main.rs` 与小模块 | ~200 | SL-4。不依赖 C9 | 对临时 daemon：list 能看到 install 的结果；没有 activate 子命令。本片不改 daemon 环境变量；若误读新 env，再补 `agent24-cli` 的 env 扫描测试 | 低 | Rust 代理 |
| SL-11 | 桌面技能栏：D7 列、缺依赖标红、激活/回滚走 presence | `apps/desktop/src/renderer/pages/Capabilities.tsx` 加栏，不改固定逻辑 | ~250 | SL-4、SL-6 的 API。不依赖 C9 | 组件测试或页面测试：缺依赖的行带红标；未激活没有「执行」。本片合入前用桌面手工点激活被拒、点接受后指针变化。只改文案不算完成 | 渲染进程直写指针 | 桌面代理 |
| SL-12 | 桌面能力栏最小列表：展平 `provides`，未安装的登记 id 可见 | 同上页的另一栏；可与 SL-11 分 PR | ~200 | 现有 `GET /api/v1/components`。不依赖 C9，也不展示「未授权」 | 登记表有而组件清单没有的 id 出现且标未安装。把「未安装」画成已启用的变异，断言变红 | 与导航目录混淆 | 桌面代理 |
| SL-13 | 「有新版本」两份差异与两个选择。选择「用新默认」走 SL-6，不自动合并 | store 查询加桌面一行 | ~200 | SL-3、SL-6。不依赖 C9 | 用户改过哈希、来源版本更高：指针不变，且响应里有两段差异。自动把指针拨到新默认的变异，测试变红 | 差异里带出正文到审计 | Rust 代理做 API，桌面代理做两颗按钮 |
| SL-14 | bundled 技能的版本递增 CI | 新的检查脚本，对着 `skills/<name>/` | ~150 | 无产品代码依赖。不依赖 C9 | 夹具：改了正文但 version 未升，检查失败。删掉这条比较后检查变绿，即变异失败 | 低 | Rust 代理 |

建议顺序：SL-1 → SL-2 → SL-5 → SL-4 → SL-6，然后 SL-3、SL-7、SL-9 可并行，SL-8 在 SL-6 和 SL-7 之后，SL-10 在 SL-4 之后，SL-11/12/13 在对应 API 之后。SL-14 可随时做。

SL-2、SL-6、SL-8 属于数据写入或授权记录，合入集成分支前要等 PR-Daemon APPROVE。其余片 CI 全绿加本地门禁后可合，意见在后续片修。

## 7. 明确不做

- 不做 D6：不读入站事件，不实现 `observe` / `auto`，不让 ADR-041 D3 的 `inferred` 启动技能。`mode: proactive` 安装即拒绝。
- 不做 `local_model`，评测不调用 iDoris 或云端模型。
- 不做从会话蒸馏、不做审查器提案、不调用 `KnowledgeBase::propose`。指令收件箱保持原语义（`knowledge.rs:12-15`）。
- 不执行技能目录里的脚本、二进制或工具调用。技能不是工具，也不往 `ToolRegistry` 注册。
- 不把 `skills` 写进 `ComponentLock`，不放宽 Documents 的空 `skills`（`documents_catalog.rs:235-236`）。
- 不实现 C9，不把技能 `requires` 映射成 `Operation` 或模块同意。
- 不改无 `skill_id` 的 run、不改计划模式的工具子集、不改记忆召回顺序。
- 不自动合并用户改过的技能，不按 mtime 升级，不在升级时删除用户目录。
- 不做技能市场、签名验证、跨社区 Nostr 分发（SPEC §7 仍未决）。
- 不改 `docs/tracking/2026-11-10/MILESTONES-AND-TASKS.md` 和 `followups.md`。
- 不在本方案里写代码。

## 8. 需要 jason 拍板的取舍

推荐已经写进 §4，这里只留决定。加上 §5 的 Q1–Q4，一共这些。

1. **进 run 的方式：注入 system，而不是给模型一个选技能的工具。** 用户或界面指定 `skill_id`，本体只注入「当前」版本的正文。模型不能自己换技能。这和「零依赖技能就是提示模板」（ADR-039 `:48`）一致，也避免一段文本变成新的工具入口。
2. **v1 评测不跑模型。** `exact` / `contains` 比较的是即将注入的提示文本。真要看模型听话与否，等 v2 的 `local_model`。
3. **版本表是权威，扫描器不按 SPEC §3.1 把更高版本覆盖写进用户目录。** 用户目录只作为 `user` 来源的输入。这样才做得到「保留我的 / 用新默认」。
4. **技能 v1 不等 C9。** 依赖解析只回答装了谁、选了谁。权限仍走现有工具门。C9 之后再考虑「已装但未授权」的红标。
5. **默认提供者放数据库表，不新建 `components.json`。** 选择不是授权。组件锁继续不收 `skills`。

## 9. 未核实

- 本工作树没有 C9 方案文件。若那份方案把「能力 id」和组件 `requires` 收成同一个结构，SL-7 要跟着改类型，避免混用。
- CLI 令牌能不能走完桌面 presence，从而安全地调用 Elevated 的激活接口：只看到桌面通道（`presence_desktop.rs:6-7`），没有把 CLI 的确认路径读完。所以 SL-10 不含 `activate`。
- `<安装目录>/skills/` 与 `<资源目录>/distro/skills/` 在打包产物里的实际路径没有对照安装器核对。SL-3 开工前要钉死这两个根，否则 bundled 扫描会扫错目录。
- `RunCreate` 现在的字段表没有在本方案里逐项抄录。SL-8 加 `skill_id` 时要确认旧客户端省略该字段仍能反序列化，并确认 OpenAPI 与 `pnpm gen:api` 的生成物。
- 技能正文进 system 之后，出站裁剪和 LocalOnly 是否会把整段 `HostSystem` 以外的 system 当内核文本放行，没有沿 `request_messages_for` 的每一条分支读完。SL-8 必须补一条：技能来源不得被当成比用户提示更可信的内部文案。
- iDoris-Components 里那五个技能文件（#1037 第 7 节提到的样本）不在本仓库，没有打开看。导入夹具不要假设它们的 frontmatter。
- 能力中心是否还有第二套「组件卡片」页面会和 SL-12 重复，只查了 `Capabilities.tsx` 和 `capabilities/registry.ts`。开工前在 `apps/desktop/src/renderer` 再搜一次组件列表的展示。
