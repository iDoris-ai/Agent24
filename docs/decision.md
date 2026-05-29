# 决策记录（Decision Log）

> 本文档记录 Agent24 框架设计中所有关键决策的论证过程、备选方案、决策依据。
> 格式参考 ADR (Architecture Decision Records)，按时间倒序追加，已采纳的决策不删除（仅在被推翻时标注 Superseded）。
> 维护者：jhfnetboy + Claude Code | 起始日期：2026-04-27

---

## ADR-001：从 xiaoheishu/desktop 借鉴而非从零开发

**日期**：2026-04-27
**状态**：✅ 采纳

### 背景

需要一个跨平台（mac/win）的 Electron 桌面应用承载 Agent24 能力。备选方案：
- A. 从零设计 Electron 应用
- B. 从 xiaoheishu/desktop 借鉴
- C. fork 某个开源 desktop agent 框架（如 Open Interpreter desktop）

### 论证

**已勘察 xiaoheishu/desktop（`/Users/jason/Dev/mycelium/blog/submodules/xiaoheishu/desktop`）**：
- Electron 30 + Vite + React 18 + TypeScript（成熟主流栈）
- `node-llama-cpp` 已集成本地 LLM，含模型自动下载、HF endpoint 检测、硬件推荐
- `better-sqlite3` 本地存储
- 干净的 IPC 模块化架构（`electron/ipc/{posts,publish}.ts`）
- 安全的 `localfile://` protocol handler
- Playwright 浏览器自动化（虽然是 xiaohongshu 专用，但架构通用）

放弃 A 的理由：上述技术决策都已经验证过，重做没意义。
放弃 C 的理由：第三方 desktop agent 框架（如 Letta Desktop、AnythingLLM）的核心都是封闭的 chat UI，扩展困难；xiaoheishu 的代码量小、可读性高、无第三方依赖陷阱。

### 决策

**B**。把 xiaoheishu 作为 git submodule 引入 `vendor/xiaoheishu`，提取通用部分到 Agent24 主目录，xiaoheishu 自有功能（小红书发布等）后续抽离为独立 npm 包。

---

## ADR-002：从"裁剪"改为"模块化适配层"

**日期**：2026-04-27
**状态**：✅ 采纳（已修正初版方案）

### 背景

最初我（Claude）的设计方案是"裁掉 xiaoheishu 的特定场景代码（小红书发布）"。用户立刻反对：

> "我有一点疑问，就是第一为什么要裁掉原来的一些呃能力。…… 我希望这个 desktop 是一个融合的 desktop 不应该去裁掉原来的能力。换句话说，或者它是一个模块加载的方式。…… 这样的话，我们的框架就跟能力是解耦的，跟 AI 模型也是解耦的，框架只做的核心的迭代。"

### 论证

"裁剪"假设了"我们要做一个新应用"。但用户的真实诉求是"做一个壳，让能力按需加载"——这本质上是平台思维 vs 应用思维。

**裁剪方案的问题**：
- 一旦裁掉，xiaoheishu 后续的更新就再也合不回来
- 框架和场景紧耦合，每加一个新场景（公众号、Twitter）都要改框架本体
- 没法支持"用户自己开发模块"

**模块化方案的优势**：
- 框架只做内核演进（Electron 壳、IPC、AI Layer、Memory Layer）
- 能力变成可插拔的 npm 包，按需 install/uninstall
- xiaoheishu 完成自身调试后，自家功能也成为模块（`@auraaihq/publish-xiaohongshu`），其他人能直接复用

### 决策

**模块化适配层**：内核 + 三层模块（Base/Community/Personal），所有能力都是可插拔模块。

---

## ADR-003：模块按"服务对象"分三层（Base/Community/Personal）

**日期**：2026-04-27
**状态**：✅ 采纳

### 背景

需要给模块分类，备选维度：
- A. 按功能分：发布 / 抓取 / 处理 / 通信 / 身份 ……
- B. 按服务对象分：基础设施 / 社区 / 个人
- C. 按运行时状态分：内核内嵌 / 后台守护 / 按需触发

### 论证

**功能分类（A）的缺点**：
- 维度太多且会膨胀，分到第二第三层就糊
- 一个功能（如"发布"）可能既服务个人也服务社区，归类困难

**服务对象分类（B）的优势**：
- 对应 Mycelium Protocol 的"个人 / 社区 / 城市"三层服务对象（详见 `Brood/protocol/MISSION.md`）
- 清晰映射到我们的产品定位："为个人服务，以社区为协作单位"
- 用户决策路径直观："我要装这个模块吗？" → "它服务于谁？"

**运行时状态分类（C）**：作为正交维度，每个模块自己声明（如 `runtime: "daemon" | "lazy" | "embedded"`），不作为分类轴。

### 决策

主分类用 B，每个模块用前缀编码：
- 基础模块：`module-*`（identity, wallet, comm, storage, shared-memory）
- 社区模块：直接命名（cos72, myshop, mytask, myvote）
- 个人模块：按子类前缀（publish-*, scrape-*, idoris-*）

---

## ADR-004：cos72 包含 myshop / mytask / myvote 三个核心子模块

**日期**：2026-04-27
**状态**：✅ 采纳（用户明确指定）

### 背景

我最初提议社区层包含"cos72 + governance + team-comm"三个模块。用户修正：

> "cos72 它包含三个基础模块……第一个是 myshop 是做积分兑换的，第二个是 mytask 是完成任务获得积分的。第三是 myvote 是投票的。"

### 论证

**用户的分法符合"完整闭环"**：
- mytask（创造价值，得积分）
- myshop（消费积分，换价值）
- myvote（治理决策，影响规则）

这三个模块构成最小可运转的"社区经济+治理"闭环，缺一不可。比起抽象的"governance"更具体落地。

### 决策

社区模块层包含 4 个：cos72（伞包，依赖 myshop/mytask/myvote）+ communication（复用 base 层的 agent-speaker）+ shared-memory。后续按需扩展。

---

## ADR-005：iDoris-SDK 不收纳 publishers，每个 publisher 独立 npm 包

**日期**：2026-04-27
**状态**：✅ 采纳

### 背景

用户设想："小黑书完成调试后打包为 npm 包到 iDoris-SDK（是这个思路吧）。"

我反对了这个收纳思路。

### 论证

**iDoris-SDK 已有特定职责**：是微信桥接 SDK（前 `MushroomDAO/Agent-WeChat-SDK`），定位是"把任意 Agent 接入个人微信号"。

**混进去的问题**：
1. 职责混淆——"内容发布到各平台" ≠ "微信桥接"
2. 文档/版本/issue 全乱
3. 消费方困惑——只想用微信能力的开发者被迫面对一堆 publisher 包

**正确做法**：每个发布平台独立 npm 包，统一在 `@auraaihq/*` scope 下，命名 `@auraaihq/publish-{platform}`。

### 决策

- iDoris-SDK 保持原职责（微信桥接）
- xiaoheishu 中的 xiaohongshu publisher 抽离为 `@auraaihq/publish-xiaohongshu`
- 所有发布器统一前缀 `publish-`，所有抓取器 `scrape-`

---

## ADR-006：npm scope 用 `@auraaihq`

**日期**：2026-04-27
**状态**：✅ 采纳

### 备选

- `@auraai`：和组织名贴近，发音简短
- `@a24`：超短，跟 Agent24 一致
- `@auraaihq`：和 GitHub 组织 `AuraAIHQ` 一致，npm 上已注册

### 论证

- `@auraai` 在 npm 上没注册，临时改去注册可能与 GitHub 组织名脱节
- `@a24` 太晦涩，外部新用户看不懂
- `@auraaihq` 已在 https://www.npmjs.com/settings/auraaihq/packages 注册

### 决策

**`@auraaihq`**。所有包名 `@auraaihq/{name}`。

---

## ADR-007：混合 Monorepo 策略（pnpm workspace + 按"未来可拆"边界组织目录）

**日期**：2026-04-27
**状态**：✅ 采纳

### 备选

- A. 纯 monorepo：所有包在一个仓库，CI 统一
- B. 纯 multi-repo：每个包一个仓库
- C. 混合：单 repo 但目录结构按可拆分边界组织

### 论证

**纯 monorepo 问题**：
- 想要拆出去时（某个 publisher 由社区独立维护），改造成本高
- issue tracker 容易拥堵

**纯 multi-repo 问题**：
- 早期跨包 PR 协调成本极高
- 几十个包的 release 全手动协调，痛
- 内核迭代快时，每次都要发多个 repo 的版本

**混合方案 C 的关键性质**：
| 性质 | 实现 |
|------|------|
| 包名稳定 | `@auraaihq/publish-blog` 不论在 mono 还是拆出去都是这个名字 |
| 每子目录是完整包 | 自带 package.json + 版本号 + tests |
| workspace 协议 | 内部依赖 `"workspace:*"`，发布时自动转版本号 |
| CI 仅构建变更子树 | 用 turbo/nx/changesets |
| 拆分工具成熟 | `git filter-repo --path X --to-subdirectory-filter -` 一行命令出独立 repo |

### 决策

**混合 monorepo**。仓库 `AuraAIHQ/auraai-packages`，目录按"高耦合内核 / 低耦合扩展"分：
- `packages/`：内核 + AI 适配 + memory + base modules（紧耦合）
- `publishers/`、`scrapers/`、`idoris/`：扩展模块（低耦合，未来易拆）

**何时拆**：当某子目录满足「独立 maintainer 团队 / release cycle 严重不一致 / License 必须不同 / 拥堵到拖累 mono」中的任一条件。**半年内不拆**，先验证 mono 够不够用。

---

## ADR-008：模型包不存权重，只放 metadata

**日期**：2026-04-27
**状态**：✅ 采纳

### 背景

`@auraaihq/models-vision` 这种包要不要把模型权重一起发到 npm？

### 数据

| 模型 | FP16 | Q4 量化 |
|------|------|---------|
| LLaVA-1.5-7B | ~14 GB | ~4 GB |
| Qwen2-VL-7B | ~16 GB | ~4.5 GB |
| MiniCPM-V-2.6 | ~8 GB | ~2.5 GB |
| Whisper-large-v3 | ~3 GB | ~1.5 GB |

### 论证

**npm 限制**：单文件 >100MB 困难，要走 LFS-like 方案，复杂度高
**包大小**：放权重 → 几 GB；不放 → 几 KB
**更新成本**：放权重 → 改一个字段要重传几 GB；不放 → 几行 metadata 改完即发
**离线场景**：不放权重时第一次需要联网下载，下载后永久离线（不是真问题）
**xiaoheishu 现有做法**：`electron/ai.ts` 已经实现了"按需下载到 userData"的完整流程，可直接复用

### 决策

**不存权重**。`@auraaihq/models-*` 包只有：
- 模型 ID
- HuggingFace URL（含镜像 fallback）
- 文件大小、SHA256
- 硬件需求（最小内存、推荐 GPU）
- 推荐量化等级

权重通过 `node-llama-cpp` + HF API 运行时下载到用户 `~/Library/Application Support/{App}/models/`。

---

## ADR-009：SkillBank 与 Evolver 是互补的两个独立包，不合并

**日期**：2026-04-27
**状态**：✅ 采纳

### 背景

用户问："skill-bank 和 evolver 这俩是啥关系，是不同风格的还是互补的？"

### 论证

它们解决"自进化循环"中的不同两半：

| 维度 | SkillBank（SkillRL）| Evolver（SkillClaw）|
|------|---------------------|---------------------|
| 类比 | 图书馆 | 编辑部 |
| 路径 | hot path（每任务）| cold path（周期性）|
| 输入 | 当前任务 context | 历史 ATIF 轨迹 |
| 输出 | 检索出的 top-K skills | 新/refined SKILL.md |
| 优化 | 检索准确率 + 延迟 | skill 质量 + 覆盖率 |

**为什么必须分开**：
1. 关注点分离——检索算法和生产算法独立演化
2. 频次差几个数量级——一个每秒，一个每天
3. 故障隔离——Evolver 挂掉不影响 agent 用现有 skills
4. 可独立替换——换检索引擎不影响 evolver

合并的话内部还是这两个子系统，对外 API 还是分两组，徒增包间依赖。

### 决策

**两个独立包**：`@auraaihq/skill-bank` + `@auraaihq/evolver`，evolver 输出写入 SkillBank 的 storage，agent 检索时读 SkillBank。

---

## ADR-010：先做参考实现 + 渐进提取，不一开始就定接口

**日期**：2026-04-27
**状态**：✅ 采纳

### 背景

用户说："我希望整个结构先讨论，确认清楚，然后再去更新我们的里程碑啊相关的。"

### 论证

接口规格的成熟度依赖于至少 2-3 个真实模块的实现经验。过早冻结接口 = 后期大量 breaking change。

**正确路径**：
1. 内核裸跑（M0-M1 早期）
2. 从 xiaoheishu 提取 1-2 个模块作为参考实现（M1）
3. 总结共性，定 v0.1 接口规格（M1 末）
4. 再加 3-5 个模块（M2），可能需要小调整
5. v1.0 接口规格冻结（M3+）

### 决策

M0 阶段**不写接口**，先：
- 决策记录（本文档）
- 结构图 + 模块清单（PLAN.md）
- 仓库与 npm scope 初始化
- 跟 xiaoheishu 提取的边界划分

接口设计延迟到 M1 中后期。

---

## ADR-011：跨切关注点初步规划

**日期**：2026-04-27
**状态**：✅ 采纳（M1+ 细化）

### 决策清单

| 维度 | 起步策略（M1）| 长期目标（M3+）|
|------|-------------|--------------|
| 模块发现 | 仅内置 npm 包 | + Git URL + IPFS hash |
| 模块信任 | 仅核数字签名校验 | + AirAccount 签发 + 沙箱 |
| 模块权限 | 加载时静态声明 | 首次启用时动态授权 UI |
| 模块配置 | YAML/JSON | UI 自动从 schema 生成表单 |
| 模块状态 | Memory Layer 隔离命名空间 | + 加密 + 跨设备同步 |
| 模块版本 | semver | + 自动更新 + rollback |
| 模块通信 | 事件总线（不直接调用）| + actor model |

---

## ADR-012：iDoris-SDK 不合并进 @auraaihq/sdk

**日期**：2026-04-27
**状态**：✅ 采纳

### 背景

用户问："iDoris-SDK 要不要把它纳入到我的 Agent24 这个 SDK 里边呢？换句话说，我们只要 1 个 SDK 就行。"

### 论证

iDoris-SDK 与 `@auraaihq/sdk` 是两个不同维度的"SDK"，受众和职责都不同：

| 维度 | iDoris-SDK | @auraaihq/sdk |
|------|------------|---------------|
| 受众 | 任何想把 Agent 接入微信的人 | 开发 Agent24 模块的人 |
| 职责 | WeChat 协议适配（iLink ↔ Agent 接口）| 框架开发者 API（types/hooks）|
| 依赖 | wechat-agent-bridge（第三方）| 应零运行时依赖 |
| 场景 | 可独立运行（Mac mini 24/7）| 必须在 desktop 框架内 |
| Release | 跟随腾讯 iLink 协议（外驱）| 跟随我们框架（内驱）|
| 现有消费 | simple-agent 已用 `@agent-wechat/core` | 暂无 |

### 合并的代价

1. 破坏 simple-agent 等现有消费方——它们被迫依赖整个 desktop 框架
2. 绑定第三方依赖——`@auraaihq/sdk` 被迫依赖 wechat-agent-bridge，污染所有 module 开发者的依赖树
3. 限制 iDoris-SDK 复用范围——本来能服务整个 AuraAI 生态（甚至外部），合并后仅服务 desktop

### 类比

`axios`（HTTP 协议库）vs `@tanstack/react-query`（React 集成层）——不合并。
- axios 任何 JS 环境都能用
- react-query 仅 React 应用用

### 决策

**不合并**。iDoris-SDK 保持独立。在 Agent24 中的集成路径：

```
iDoris-SDK (@agent-wechat/core)        ← 协议层 SDK，独立存在
    ↓ 被使用 by
@auraaihq/module-wechat                ← 适配模块（"消费者"）
    ↓ 实现
@auraaihq/sdk 的 Module 接口            ← 框架 SDK
```

这样 iDoris-SDK 服务多受众（simple-agent + 我们 + 任何 third party），`@auraaihq/sdk` 保持纯净。

---

## ADR-013：iDoris-SDK 合并进 auraai-packages monorepo（M2 执行）

**日期**：2026-04-27
**状态**：✅ 采纳（修正 ADR-012 的回答范围）

### 背景

ADR-012 答的是"iDoris-SDK 合并进 `@auraaihq/sdk` 单个包"——这个不行（不同受众、不同依赖）。

但用户后续问的是另一个问题："iDoris-SDK 合并进 `@auraaihq` scope（即 auraai-packages monorepo）"。这两件事不一样。

### 论证

合并到 monorepo 与合并到单个 SDK 包的区别：

| 维度 | 合并进单个 SDK 包（ADR-012 拒绝）| 合并进 monorepo（本 ADR 接受）|
|------|------------------------------|---------------------------|
| 包名 | 强制改名 `@auraaihq/sdk` | 改为 `@auraaihq/wechat-bridge`（独立包）|
| 受众 | 被迫只服务 desktop 模块开发者 | 仍可服务任何 Agent 作者 |
| 依赖 | 污染 SDK 依赖树 | 隔离在自己包里 |
| 与 ADR-007 兼容 | ❌ | ✅（混合 monorepo 设计就支持低耦合包并存）|

合并到 monorepo 实际是 ADR-007 的应用场景——`communication/` 子目录正好对应"低耦合、未来易拆"的扩展模块。

### 决策

**M2 阶段执行**：
- iDoris-SDK 代码迁入 `auraai-packages/communication/wechat-bridge/`
- npm 包改名 `@agent-wechat/core` → `@auraaihq/wechat-bridge`
- 老包 deprecate（保留发布历史，README 指向新包）
- simple-agent 同步升级依赖
- AuraAIHQ/iDoris-SDK 仓库归档（README 指向新位置）

**为什么 M2**：M2 时机做 module-wechat 集成，连同协议层一起搬干净，避免来回改。

---

## ADR-014：Agent24 在 M3 后被 @auraaihq/skills-* + skill-bank + evolver 替代

**日期**：2026-04-27
**状态**：✅ 采纳（M3 执行）

### 背景

用户指出：Agent24 实质是 4 个 SKILL.md + agent-config.yaml + install.sh + 2 个 hook，没有独立运行时。一旦 `@auraaihq/skill-bank` + `@auraaihq/evolver` + `@auraaihq/skills-*` 包落地，Agent24 作为独立 repo 就成了冗余。

### 拆解

| Agent24 当前内容 | M3 后位置 |
|----------------|----------|
| `skills/evolve/SKILL.md` | `@auraaihq/skills-evolve` |
| `skills/evaluate/SKILL.md` | `@auraaihq/skills-evaluate` |
| `skills/setup/SKILL.md` | `@auraaihq/skills-setup` |
| `skills/org-sync/SKILL.md` | `@auraaihq/skills-org-sync` |
| `agent-config.yaml` | `@auraaihq/skills-evolve` 默认 config |
| `install.sh` | `@auraaihq/cli install <skill>` |
| `hooks/*.sh` | 各 skill 包自带 |

### Agent24 与 skill-bank/evolver 的关系（修正）

不是"替代"是"承接"：
- `@auraaihq/skills-*` = 静态内容（初始 skill markdown）
- `@auraaihq/skill-bank` = 运行时存储+检索容器
- `@auraaihq/evolver` = 进化引擎，扫 archive 写新 skill 到 skill-bank

Agent24 是 skill-bank 的**初始种子内容**，evolver 是后续填充器。

### 时间表

| 阶段 | Agent24 状态 |
|------|------------|
| 当前 ~ M2 | **保留**——唯一可用的实现 |
| M3（skill-bank + evolver 落地）| **迁移**——4 个 skill 拆为 npm 包 |
| M3 末 | **Deprecated**——`AuraAIHQ/Agent24` 仓库 archive 为只读，README 显眼标注 deprecated 状态，引导到新 npm 包<br>之后名字空出来给 Agent24 改名（见 ADR-015）|

### 为什么不现在做

- skill-bank 和 evolver 当前是 placeholder（M3 才实现）
- Agent24 是今天唯一能跑的东西，提前迁移会留下一个 M2-M3 的空窗期
- M3 落地时一起改，避免双轨

### 决策

M3 执行迁移。在那之前 Agent24 保持现状。

---

## ADR-015：M3 后 Agent24 改名为 Agent24

**日期**：2026-04-27
**状态**：✅ 采纳（M3 末执行，依赖 ADR-014 完成）

### 背景

ADR-014 决定 M3 时 Agent24（旧）的内容迁出到 npm 包，老仓库归档。这空出了 "Agent24" 这个名字。

用户提议："Agent24 改回 Agent24，这样'Desktop' 后缀去掉。它本来就是一个壳，未来发移动端也合理（mobile + desktop）。"

### 论证

**支持**：
- "Agent24" 这个名字暗示了"仅桌面端"，限制了未来发展方向
- 框架本质是 Electron 跨平台壳，加 Capacitor 或 Tauri 就能上 mobile（iOS/Android）
- 与定位"个人 24 小时在线 Agent"匹配——agent 在哪都能用，不限于 desktop
- 老 Agent24 归档后名字空出来，刚好用上
- 减少品牌认知割裂（一个产品两个名字）

**潜在问题**：
- GitHub repo rename 会有一段重定向期，破坏外部 PR / star 关注（但 GitHub 自动 301 重定向，影响可控）
- 现有文档/链接需要更新

### 时间表

| 阶段 | 状态 |
|------|------|
| 当前 ~ M3 中 | `Agent24` 仓库还在用（Skills），`Agent24` 同时存在 |
| M3 末 ADR-014 完成时 | 旧 `AuraAIHQ/Agent24` 归档（README 指向 npm 包）|
| M3 末紧接着执行 | `AuraAIHQ/Agent24` rename → `AuraAIHQ/Agent24` |
| M4+ | 应用产品名去掉 "Desktop"，为 mobile 端开口 |

### 决策

执行 ADR-015。在 ADR-014 完成后立即做 repo rename。

### 长期路径（M5+ 推测）

Agent24 应用形态可能演化为：
- Desktop：Electron（mac/win/linux）—— 现在的形态
- Mobile：Tauri 2.0 mobile / Capacitor + 同一份 React 代码—— 未来路径
- Web：纯 PWA（最简，但本地能力受限）

不强制要求 M5 实现 mobile，但**架构设计（M0-M3）就要避免 desktop-only 的耦合**——例如不要假设永远有 `node-llama-cpp` 等只在 Node 环境的依赖。

---

## ADR-016：模块安全与权限模型

**日期**：2026-04-27
**状态**：✅ 采纳（M1 设计模块加载器时落地）

### 背景

之前的 ADR-011 笼统说"模块发现 / 信任 / 权限"，没具体方案。但模块加载器是 M1 的核心交付，没有清晰的安全模型就实现不了。

### 决策（分阶段）

**M1 起步版**：
- **沙箱**：每个模块跑在独立 Node `worker_thread`（不跨进程，性能/隔离折中）
- **权限**：模块在 manifest 中声明所需权限（`fs:read`、`fs:write`、`net`、`ai`、`memory:read`、`memory:write`、`module:invoke:<id>`），加载器据此构造受限的 ModuleContext
- **签名**：跳过——M1 只支持内置模块和明示信任的 npm 包
- **凭据**：所有 API key / token 经 keytar 存系统 keychain，模块申请时由内核解密注入

**M3 增强版**：
- **沙箱**：升级到独立子进程（child_process.fork），可单独崩溃恢复
- **签名**：新模块发布时强制 sigstore 签名，加载时验证
- **AirAccount 信任根**：用户可设置"只信任此 AirAccount 签发的模块"

**M5 企业版**：
- 完全 VM 沙箱（webcontainer 风格）+ 流量审计 + 权限运行时审批 UI

### 论证

不一开始上完整方案的原因：模块作者是稀缺资源，过度安全限制会劝退开发者；M1 阶段先让生态长起来。沙箱和签名按"加密圈+模块成熟度"渐进强化。

---

## ADR-017：数据隐私与轨迹共享（Privacy & Trajectory Sharing）

**日期**：2026-04-27
**状态**：✅ 采纳

### 背景

iDoris 定位"隐私优先"，evolver 又依赖跨用户轨迹做进化。这两个目标必须显式协调。

### 决策

**默认全本地**：
- 所有 ATIF 轨迹、memory、archive 默认**仅本地存储**（加密 SQLite）
- 跨设备同步默认关闭，开启后用 NIP-44 端到端加密
- evolver 默认仅在本机轨迹上运行（个人 SkillBank）

**Skill 共享是 Opt-in**：
- 用户必须显式开启"contribute to community SkillBank"
- 开启时也只发送**已 evolver 蒸馏过的 skill**（SKILL.md），不发原始轨迹
- 蒸馏过程在本地完成，敏感信息（API key、个人数据）按规则脱敏
- 默认匿名（pubkey 不绑定真实身份），可选公开署名

**iDoris 数据流**：
- iDoris 调用产生的中间数据**不离开设备**
- 用户可设置"敏感任务路由"：某些任务类型强制使用 iDoris（不调云端）

### 论证

privacy-first 必须是"安全默认值"，不能默认 share-on（用户不知情下被采集）。SkillClaw 论文中的"集体进化"是 opt-in 加 federated 蒸馏，照搬这个模式。

---

## ADR-018：移动端技术路径选 Tauri 2.0（M5+）

**日期**：2026-04-27
**状态**：✅ 采纳（影响 M0-M4 的依赖选择）

### 备选

ADR-015 提到将来要做 mobile，需要在三种路径间选：
- **A. Capacitor + Electron 共代码**：复用现有 Electron 工程，加 Capacitor 包装
- **B. Tauri 2.0**：原生跨平台（含 mobile），Rust 后端 + Web 前端
- **C. React Native 重写**：mobile 优先，desktop 用 RN-Windows/macOS

### 论证

| 维度 | Capacitor | Tauri 2.0 | RN |
|------|-----------|-----------|-----|
| 现有 Electron 代码复用 | 高 | 中（前端 React 可全留）| 低（重写）|
| 包大小 | 60-100MB | 8-15MB | 15-30MB |
| Mobile 性能 | 中 | 高（Rust 后端）| 高（原生 bridge）|
| Node 生态依赖 | ✅ 全支持 | ❌ 不支持 node-llama-cpp 等 | ❌ |
| AI/llama.cpp 在 mobile | 受限 | 需 Rust 重写桥接 | 需原生重写 |
| 学习曲线 | 低 | 中（Rust）| 高 |
| 长期维护 | Capacitor 团队 | Tauri 团队（活跃）| Meta（活跃）|

### 决策

**Tauri 2.0**。理由：
- Tauri 2.0 已支持 mobile (iOS + Android)
- 包大小决定性优势——desktop agent 不能臃肿
- Rust 后端与 iDoris 未来可能的 Rust 绑定路径一致
- 前端 React 代码可全部复用

### 影响 M0-M4 的设计约束

为了 M5 能顺利切换：
- ❌ **避免** Electron-only API（如 `BrowserWindow.webContents` 直接调用）
- ❌ **避免** Node 原生依赖（除非有 Rust 替代品）
- ✅ **使用**：HTTP/IPC 抽象层、独立进程通信、可移植的存储 API
- ✅ AI Layer 设计上预留"Rust binding via Tauri command"接口

M0-M4 仍用 Electron 实现（开发速度快），但模块接口设计要 Tauri-friendly。

---

## 整合后的生态简化

ADR-013 + ADR-014 落地后，活跃仓库从 7 个降到 3-4 个：

```
活跃:
  AuraAIHQ/Agent24      ← Electron 应用
  AuraAIHQ/auraai-packages      ← 单一 monorepo 装：
                                  - 内核 / SDK / CLI
                                  - skills-* (从 Agent24 迁入)
                                  - skill-bank / evolver
                                  - communication/wechat-bridge (从 iDoris-SDK 迁入)
                                  - publishers/* / scrapers/* / idoris/*
  AuraAIHQ/iDoris               ← AI 模型代码（独立技术栈）
  AuraAIHQ/agent-speaker        ← Nostr 通信（独立 Go 项目，不进 npm 体系）

Deprecated（archive 只读，README 引导到新位置）:
  AuraAIHQ/Agent24              ← M3 末 deprecated（之后名字让给 Agent24 rename）
  AuraAIHQ/iDoris-SDK           ← M2 末 deprecated（content 已迁入 monorepo）
```

---

## 附：开放问题（Open Issues，待 M2-M3 决策）

下面这些是已识别但暂未决策的设计点，列出来防止遗漏。每条会在合适的里程碑上升为 ADR。

| # | 问题 | 何时决策 |
|---|------|--------|
| OI-1 | 模块意图冲突（两个 publisher 都想接管 "send tweet"）→ 用户优先 / 显式声明 / dispatcher 投票？ | M1（dispatcher 设计时）|
| OI-2 | 模块版本冲突（A 依赖 `core@^1.0`，B 依赖 `core@^2.0`）解决策略 | M1 |
| OI-3 | API 调用配额（Claude / OpenAI 月度上限）UI 展示 + 警告 + 自动降级到 iDoris/Local | M2 |
| OI-4 | 首次启动 onboarding 流程（默认装哪些模块？引导用户做什么？）| M2 末 |
| OI-5 | 模块更新/回滚机制（auto-update / 通知后手动 / staged rollout）| M2 |
| OI-6 | 模块市场经济模型（如有）：纯免费？付费模块？打赏？| M4+ |
| OI-7 | 多账号支持（同一台机器多个用户身份）| M3+ |
| OI-8 | i18n（UI 中英双语）| M3 末 |
| OI-9 | 自动化测试体系（unit / module integration / e2e）| M1 |
| OI-10 | telemetry / 错误上报（opt-in，匿名化，告警关键 bug）| M2 |
| OI-11 | 模型能力路由表（vision 任务用 LLaVA，长文本用 Claude，本地隐私用 iDoris）| M2 |
| OI-12 | 备份与恢复（用户 memory + config 整体备份/迁移到新设备）| M3 |

---

## ADR-019：LLM Gateway 模式（能力模块不直接调用 LLM API）

**日期**：2026-05-09
**状态**：✅ 采纳

### 背景

M2 开始引入能力模块，每个模块都需要调用 LLM。备选方案：
- A. 每个模块直接调用 LLM API（Ollama/OpenAI/Claude）
- B. 统一 LLM Gateway，模块只调 `llm.chat()` 接口

### 论证

**直接调用（A）的问题**：
- Token 用量分散，无法统计哪个模块消耗了多少资源
- 每个模块各自实现错误处理、重试、超时，代码重复
- 切换底层模型需要改所有模块
- 无法在不改模块的前提下增加配额限制、响应缓存、审计日志
- 模块作者需要知道 Ollama API 细节，耦合底层

**Gateway 模式（B）的优势**：
- 统一统计：按模块维度追踪 token 消耗，M3 可持久化到 SQLite
- 可替换性：底层从 Ollama 切换到 OpenAI 兼容接口只改 Gateway，模块零感知
- 权限控制：Gateway 可为不同模块设置不同配额（防止单个模块耗尽资源）
- 缓存：相同 prompt 命中缓存，减少 Ollama 调用延迟（M3 实现）
- 审计日志：所有 LLM 调用统一记录，满足 ADR-017 隐私追踪需求
- 能力模块接口稳定：`llm.chat(req)` 签名不变，底层实现可独立演进

### 决策

**B**。所有能力模块只知道 `CapabilityContext.llm.chat()` 接口，不直接依赖 Ollama / OpenAI / Claude SDK。LLM Gateway 作为 `src/backend/llm-gateway.ts` 实现，M2 底层对接 Ollama，M3 扩展为可配置（通过 `LLM_BACKEND` 环境变量选择 Ollama / OpenAI 兼容 / Claude API）。

---

## ADR-020：后端 Daemon 技术选型（TypeScript + Node http，localhost:8765）

**日期**：2026-05-09
**状态**：✅ 采纳

### 背景

能力模块需要一个进程内 HTTP 服务，提供 REST 接口给 Electron renderer（通过 IPC proxy）和未来的 CLI 消费者。备选框架：
- A. Python + FastAPI（跨语言 daemon）
- B. Node.js + Express（老牌框架）
- C. Node.js + Fastify（高性能 TypeScript 友好框架）
- D. Node.js 内置 `http` 模块（零依赖，M2 过渡方案）

### 论证

**Python FastAPI（A）的问题**：
- 语言上下文切换：团队主栈是 TypeScript，Python 会引入两套工具链（pip/poetry vs pnpm）
- 打包体积：macOS 打包需带 Python runtime，增加 50-80MB
- IPC 类型安全：跨语言时 TypeScript 类型无法端到端贯通

**Express（B）vs Fastify（C）的对比**：
- Fastify 性能比 Express 快约 35%（官方 benchmark）
- Fastify 原生支持 JSON Schema 路由验证，与 TypeScript 配合更好
- Fastify 插件系统与 CapabilityModule 注册模式天然契合

**内置 http（D）的取舍**：
- 零额外 npm 依赖，不增加 package.json 复杂度，M2 PR review 最简洁
- 功能完全够 M2 需求（路由 + JSON 解析）
- 注释标注"生产版本替换为 Fastify"，M3 升级时接口不变

**端口选择**：
- 8765：不与 3000（React dev）、5173（Vite）、8080（常见代理）、11434（Ollama）冲突
- localhost 绑定：仅本机访问，不暴露公网

**进程管理**：Electron main 通过 `child_process.fork()` 启动，共享 Node 运行时（比 spawn 省 50-100ms 启动时间），MessageChannel 可用于进程间快速通信（M3 扩展）。

### 决策

**D（M2）→ C（M3）**。M2 用 Node.js 内置 `http` 模块实现零依赖服务器，端口 8765，TypeScript 类型完整。M3 在不改变路由接口的前提下替换为 Fastify，获得验证、插件、更好的错误处理。Electron main 通过 BackendManager（`src/main/backend-manager.ts`）管理进程生命周期。

---

## ADR-021：一站式安装 — Zero-CLI Onboarding

**日期**：2026-05-09
**状态**：✅ 采纳（M2 实现 Onboarding Wizard，M3 完善 Ollama 捆绑）

### 背景

用户要求："面向小白的框架。安装之后打开做一个简单的配置，比如说下载对应的模型，不要让它去运行任何命令。"

核心约束：**用户全程不触碰终端**。

### 备选方案

- **A. 仅安装 Electron App，文档说明手动装 Ollama**：最简，但有命令行操作，违背约束
- **B. App 内引导用户去 Ollama 官网手动下载**：次优，减少命令行但有跳转摩擦
- **C. App 启动时自动检测 + 下载安装 Ollama + 拉取模型**：完全无 CLI，用户体验最佳
- **D. 把 Ollama 二进制捆绑进安装包**：最自包含，但 macOS .dmg 体积 +150-200MB

### 论证

**Ollama 下载策略**（C vs D）：
| 维度 | C（运行时下载）| D（捆绑安装包）|
|------|---------------|----------------|
| 安装包大小 | < 200MB | ~350MB+ |
| 离线首次使用 | ❌ 需联网 | ✅ |
| Ollama 版本更新 | 自动（下载最新）| 需重新打包 App |
| 实现复杂度 | 中 | 低 |
| 竞品做法（Jan.ai）| 运行时检测 | — |

M2 采用 C：检测 Ollama → 未装则下载安装包（GitHub releases API）→ 静默安装 → 拉取推荐模型。

**硬件检测 → 模型推荐逻辑**：
| RAM | 推荐模型 | 量化 |
|-----|----------|------|
| < 8GB | 提示"配置偏低，将使用云端 API 模式" | — |
| 8-16GB | llama3:8b / qwen2.5:7b | Q4_K_M |
| 16-32GB | llama3:13b / qwen2.5:14b | Q4_K_M |
| > 32GB | llama3:70b / qwen2.5:32b | Q4_K_M |

GPU 检测（Metal/CUDA 可用时）可升一档模型。

**Onboarding Wizard 流程**（首次启动）：
```
Step 1: 欢迎页 — 说明 Agent24 是什么
Step 2: 环境检测 — 检测 RAM/GPU/已有 Ollama（自动，2-3秒）
Step 3: 推荐方案 — 展示推荐模型及理由，可手动选其他
Step 4: 安装进度 — Ollama 下载/安装 + 模型拉取（进度条）
Step 5: 就绪 — 首次对话界面
```

如果用户已有 Ollama（检测到 localhost:11434 响应），跳过 Step 4 的 Ollama 安装部分，只拉取模型。

### 决策

**实现路径**：
- `src/main/ollama-manager.ts`：检测 → 下载安装 → 启动/停止 Ollama 进程
- `src/renderer/onboarding/`：5 步 Wizard UI（React）
- `src/main/hardware-detect.ts`：RAM + GPU 检测（Node `os.totalmem()` + systeminformation 包）
- Wizard 完成状态持久化到 `userData/onboarding-complete.json`，已完成则直接进主界面

**打包策略**：Ollama 二进制**不**捆绑进 .dmg，运行时下载（M3 重新评估是否捆绑）。
**更新**：Ollama 由 App 管理，不依赖用户系统已有的 Ollama，避免版本冲突。

---

## ADR-022：LLM 运行时默认 MLX，UI 可切换

**日期**：2026-05-09
**状态**：✅ 采纳（修正 ADR-019/021 中"默认 Ollama"的假设）

### 背景

用户明确要求："不一定 Ollama，默认我想用 oMLX，但是用户可以切换为 Ollama 或者其他类似工具，在界面配置即可。"

ADR-019 的 LLM Gateway 设计假设底层是 Ollama，需要修正。

### 备选 LLM 运行时（Apple Silicon Mac 场景）

| 运行时 | 特点 | 适合场景 |
|--------|------|----------|
| **MLX**（默认）| Apple 官方 ML 框架，Metal GPU 原生，Apple Silicon 最优 | 日常对话、本地隐私 |
| **Ollama** | 最流行，生态最广，API 兼容 OpenAI | 跨平台、丰富模型库 |
| **Rapid-MLX** | 号称比 Ollama 快 4.2×，专注极致性能 | 高频调用、延迟敏感 |
| **LM Studio** | 图形化，开箱即用，有 REST API | 不熟命令行的用户 |
| **远程 API** | Claude / OpenAI / DeepSeek | 本地算力不足时 |

### 论证

**MLX 作为默认**：
- Apple Silicon Mac 用户（M1/M2/M3/M4）占本框架目标用户大多数
- MLX 由 Apple 维护，Metal GPU 加速原生，统一内存利用率最高
- MLX 是 Python 库，天然与 Python 后端（ADR-023）集成
- Rapid-MLX 作为 MLX 的性能加强版值得关注（benchmark 验证后可切换）

**可切换的必要性**：
- 不同用户硬件不同（非 Apple Silicon 无法用 MLX）
- 不同任务偏好不同模型生态
- 避免供应商锁定

**LLM Gateway 抽象层（ADR-019）的价值在此体现**：所有能力模块只调 `llm.chat()`，底层运行时通过配置切换，模块代码零改动。

### 决策

- **默认**：MLX（`mlx-lm` 库）
- **可切换**：Ollama / Rapid-MLX / LM Studio API / 远程 OpenAI-compatible API
- **切换入口**：设置页 → LLM 运行时配置（下拉选择 + 地址/端口/API Key 输入）
- **Gateway 适配层**：每个运行时实现同一 `LLMAdapter` 接口（`chat(messages) → AsyncGenerator`）

---

## ADR-023：后端语言从 Node.js 切换到 Python FastAPI（M3 执行）

**日期**：2026-05-09
**状态**：✅ 采纳（部分修正 ADR-020，M3 执行切换）

### 背景

ADR-020 选择了 Node.js + 内置 http（M2 过渡）。但 ADR-022 确定 MLX 为默认运行时后，Python 成为后端的自然选择。

用户提供的参考文档明确推荐：**Python 3.11+ + FastAPI + uvicorn**。

### 论证

**为什么 Python**：
| 能力需求 | Node.js | Python |
|----------|---------|--------|
| MLX 集成 | ❌ 需跨进程调用 | ✅ 原生 `import mlx` |
| ComfyUI / SD 集成 | 仅 HTTP 调用 | ✅ 原生调用 + HTTP |
| LangChain / LlamaIndex | 有 JS 版但不完整 | ✅ 最成熟生态 |
| Playwright 自动化 | ✅ 同等 | ✅ 同等 |
| asyncio 工作流引擎 | 需要额外设计 | ✅ 原生 asyncio.Queue |
| FastAPI（类型、文档、异步）| — | ✅ 生产成熟 |

**M2 Node.js 实现的价值**：
- 证明了 Electron main → 后端 daemon → LLM Gateway 的架构可行性
- IPC 接口、健康检查、进程管理逻辑可直接复用
- M2 作为骨架，M3 替换后端语言，前端和 IPC 接口不变

### 决策

| 阶段 | 后端实现 |
|------|---------|
| M2（当前）| Node.js 内置 http，零依赖，验证架构 |
| M3 | Python 3.11 + FastAPI + uvicorn，端口保持 8765 |
| M4+ | 同一 Python 进程内集成 MLX / ComfyUI / Playwright |

**接口约定**：M3 Python 后端实现与 M2 完全相同的 REST 接口（`/health`、`/api/llm/chat`、`/api/llm/usage`、能力模块路由），Electron 侧 BackendManager 无需改动。

---

## ADR-024：工作流引擎 — asyncio.Queue + Step 模式

**日期**：2026-05-09
**状态**：✅ 采纳

### 背景

后台需要支持多步骤异步工作流（如"生成文案 → 生成图片 → 合成视频 → 发布"），需要选择任务调度方案。

### 备选

- **A. Prefect / Temporal**：企业级，功能完整，但重——需要独立服务
- **B. Celery + Redis**：成熟，但需要 Redis，增加部署依赖
- **C. Python asyncio.Queue + Step**：轻量，无外部依赖，适合单机场景

### 论证

本框架定位是"个人/小团队私有化生产力中台"，单机运行，无需分布式调度：
- Prefect/Temporal 引入了协调服务，违背"零命令行"原则
- asyncio.Queue 是标准库，零依赖，与 FastAPI 天然集成
- 每个 Step 实现为 `async def step(ctx) -> StepResult`，可调用任意能力（MLX / ComfyUI / Playwright / 外部 API）
- 进度通过 WebSocket 实时推送给前端

### 核心 API 端点（同文档规范）

```
POST /api/v1/chat              — 多轮对话（调 LLM Gateway）
POST /api/v1/workflow/run      — 启动工作流（返回 task_id）
GET  /api/v1/task/{id}         — 任务状态查询（进度、日志、结果）
WS   /ws/task/{id}             — 实时进度推送（WebSocket）
POST /api/v1/files/upload      — 媒体文件上传
GET  /api/v1/files/{id}        — 结果文件下载
```

所有路径加版本号 `/api/v1/...`，预留升级空间。

### 内置工作流模板（初期硬编码）

- `short-video`：文案生成 → 图生视频 → 字幕合成 → 导出
- `social-publish`：内容生成 → 审核 → 多平台发布
- `research-digest`：网页抓取 → 提炼摘要 → 邮件/推送

### 决策

**asyncio.Queue + Step + WebSocket 推送**。SQLite 持久化任务记录（`aiosqlite` 库），无需外部数据库。

---

## ADR-025：内存管理 — 串行 LLM 推理 + 模型热切换限制

**日期**：2026-05-09
**状态**：✅ 采纳

### 背景

64GB 统一内存虽大，但同时运行 LLM + ComfyUI + Playwright 可能吃紧。需要显式的并发控制策略。

### 数据参考（64GB Mac）

| 场景 | 内存占用 |
|------|---------|
| 70B Q4 LLM | ~40GB |
| 34B Q4 LLM | ~20GB |
| 13B Q4 LLM | ~8GB |
| ComfyUI + SD XL | ~8-12GB |
| macOS + Electron + 后台 | ~6-8GB |

同时跑 34B LLM + ComfyUI + 系统 = ~36GB，勉强可行但有风险。

### 决策

1. **LLM 推理串行**：任务队列中同一时刻最多 1 个 LLM 推理，后续请求排队等待
2. **能力模块并发**：非 LLM 步骤（Playwright 抓取、文件处理）可并发
3. **模型卸载策略**：Ollama 模式下换模型时自动卸载旧模型；MLX 模式下显式 `del model; gc.collect()` 释放
4. **内存警告**：后台监控系统内存，低于阈值（默认 6GB）时暂停新任务入队并推送告警到前端

---

## 附：决策中我（Claude）犯的错误（用于改进）

| 错误 | 教训 |
|------|------|
| 初版主张"裁剪"xiaoheishu | 应该先理解用户"模块化平台"诉求再设计 |
| 提议把 publishers 收纳进 iDoris-SDK 候选方案前没批驳 | 应该立刻指出职责重叠 |
| 错过用户已注册 `@auraaihq` 的事实，先建议 `@auraai` | 应该先查 npm 状态 |
| 一度想把 skill-bank 和 evolver 合并 | 关注点分离原则不应妥协 |
| ADR-012 答错了问题（把 monorepo 合并和单包合并混为一谈）| 用户复问时才纠正 → ADR-013 |
| 把 Agent24 描述成"认知架构层"等夸大词 | 用户指出后承认它就是 4 个 markdown 文件 → ADR-014 |
