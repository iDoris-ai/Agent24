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
**状态**：⛔ Superseded by ADR-027（2026-08-12）——移动端从"单选 Tauri"改为 shell-agnostic 双壳示例；Tauri 作为其中一条路径保留，下方 M0-M4 的 Tauri-friendly 约束仍有效

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
**状态**：⛔ Superseded by [ADR-026](ADR-026-rust-core-polyglot.md)（2026-07-23：后端内核改为 Rust，Python 降级为 ML Worker）

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

## ADR-027：移动端 shell-agnostic 双壳（Tauri + Expo/React Native）

**日期**：2026-08-12
**状态**：✅ 采纳（supersedes ADR-018）

### 背景

ADR-026 把内核收敛为 shell-agnostic 的 Rust daemon + HTTP/WS 协议边界。移动端因此不再是"用哪个框架把 app 重写一遍"的单选题——任何前端都能作瘦壳、经协议远程消费 daemon，而 daemon 与模型可不在端上（如跑在用户的 Mac）。

### 重新审视 ADR-018 的三条理由

ADR-018 当年选 Tauri，核心理由是 Rust-on-device 一致性、包体积、避免 node 原生依赖。**在瘦壳模型下这三条大半失效**：端上不再跑 Rust 内核或本地模型，只是一个协议客户端；包体积与 node 依赖对一个"只发 HTTP/WS 的薄客户端"不再是决定性约束。

### 决策

移动端（iOS / Android）采用 **shell-agnostic 策略**：官方各提供一个瘦壳示例——**Tauri 2.0**（承接 ADR-018）与 **Expo / React Native**——二者都只经 HTTP/WS 协议接入 daemon，供社区 follow。**不再钦定单一框架**；桌面仍以 Electron 为参考壳（不变）。

### 影响

- ADR-018 标 Superseded（其 M0-M4 的"Tauri-friendly、避免 Electron-only API / node 原生依赖"约束仍有效，且对两种瘦壳都友好）。
- ROADMAP 的"Tauri 2.0 mobile 端"改为"移动瘦壳示例（Tauri + Expo/RN）"。

### 遗留（代码侧 follow-up）

"daemon 跑在端外、移动端远程消费"要成为真支持能力，`agent24d` 需要一条 bind-address 配置路径（现 `server.rs` 硬编码 `127.0.0.1`）+ 远程认证/配对（现为本机 stdout token）。浏览器内的 Web 壳还需处理 daemon 有意的 Origin/CSRF 拒绝（原生壳无此问题）。

---

## ADR-028：记忆架构 — 可进化 / 可替换 / 可组合的分层模型（M-D 重做）

**日期**：2026-08-21
**状态**：🟡 暂定 / 需 spike（2026-08-21 经 Codex 对抗式复审后从"采纳 85%"下调——见文末「Codex 复审收口」；保留本地优先+无强制外部服务的约束，但**不冻结**当前层边界、两列"双时相"、全局文件权威、crate 拆分）。实现分期见 `docs/specs/SPEC-MEMORY.md`

### 背景

现状 `agent24-memory`（M-D/D1）只有两层：L0 `KvStore`（命名空间 JSON KV）+ `CanonicalSession`（单一 `Summarizer`/`CompactionPolicy` 压缩）。README 画的 "L0→L3 + SkillBank + 自进化" 尚是愿景。为把 Agent24 做成**通用 agent 底座**，需要一套记忆方案，其**首要属性是可进化 / 可替换 / 可组合**：即便未来出现新记忆范式，也能**加一层或换某一层的实现，而不动其余层**。

调研了业界最先进方案（对源码逐条核实，报告见研究目录）：mem0（两阶段 LLM 写 + 可插拔后端）、letta/MemGPT（core/recall/archival 自编辑 block）、cognee（ECL 任务管线）、graphiti（双时相知识图）、basic-memory（markdown 真源 + SQLite 索引）、codex（Rust rollout-trace + AGENTS.md）、cline（Memory Bank 约定）、aider（排序压缩 repo-map）、OpenHands（事件流 + 可插拔 Condenser）、Claude Code（一文件一事实 + 索引）。**收敛结论**：记忆 = 4 类角色（工作/情景/语义/程序）；分水岭是"写策略"（显式编辑 / LLM 抽取对照 / 自动压缩，最好三者组合）；本地优先系统靠 文件+SQL，"聪明召回"才要向量/图。

### 决策

**四层记忆模型，每层一个 trait 缝、独立可换、独立可发**（"可组合"=facade 组合各层；"可替换"=每层 trait 后换实现；"可进化"=可新增层或策略而不动其余）：

- **L0 KvStore**（已有）：命名空间 JSON substrate。
- **L1 Working/Core**：小、常驻上下文、结构化、agent+人可编辑的 block（persona/偏好/当前焦点）。trait `CoreMemory`（append/replace/apply_patch）。借鉴 letta / cline / Claude Code。
- **L2 Episodic**：append-only 事件/轮次流 + **可插拔 `Condenser`** 建上下文（策略 recent/summarize/mask/forget）。**泛化现有 `Summarizer`**。复用内核 runs/events 脊柱。借鉴 OpenHands / codex。
- **L3 Semantic**：可检索事实/实体，带**双时相**有效性（`valid_at/invalid_at`，更新=失效非删除）。trait `MemoryWriter`（抽取→对照 ADD/UPDATE/DELETE）+ `Retriever`（SQLite FTS + 可选本地向量 + 排序预算）。借鉴 mem0 / graphiti / aider。
- **L4 Procedural/知识**：触发式指令/技能（SkillBank）。**markdown 权威** + file-watched 索引。trait `KnowledgeSource`。借鉴 codex AGENTS.md / Claude Code CLAUDE.md / OpenHands microagent。

**跨层原则**：① **文件即真源、SQLite 即可重建索引**（basic-memory）——记忆人可审 + 可重建；② 作用域键（user/agent/session/run）一等公民（mem0）；③ 嵌入走**可插拔 `Embedder`，默认本地 oMLX，零云依赖**；④ 双时相只用 SQLite 两列，**不引入 Neo4j/向量服务硬依赖**。

**crate 拆分**：`agent24-memory` 拆为 `memory-core / memory-episodic / memory-semantic / memory-knowledge`，一个 `MemoryStore` facade 组合；每层 trait 后可换、可测。

### 显式否决

- ❌ 强制图数据库（Neo4j）或外部向量服务——违反本地优先；双时相/关系用 SQLite 表达。
- ❌ 一次性把 L1–L4 全建——按消费者分期（M-D.1 先做 Condenser）。
- ❌ 把三级模型路由塞进记忆层——路由归模型侧（agent24-models），与记忆解耦。

### 置信度（诚实标注）

方向置信 ~85%：4 角色分类与"可插拔层"由多仓收敛证据强支撑，且贴合我们 Rust/本地优先/事件溯源约束；**精确的层边界与分期 ~80%，会在实现中随 trait 缝微调**——这正是选 trait 缝的原因。要再抬高需一个 spike：本地嵌入/向量那半（oMLX embedding + SQLite 向量）、以及 `Condenser` trait 对真实 agent loop 的验证。

### Codex 复审收口（2026-08-21，中立裁定）

Codex 对抗式复审（读了全部 9 仓库 checked-out 源码，`CODEX-REVIEW.md`，118 处引用）。我作为中立裁判逐条判定，**大部分成立、已采纳**，据此把状态下调为「暂定/需 spike」，修订方向如下：

- **[采纳·关键] 重构为"权威 + 投影"而非"层门面"**：三个**持久权威**——`EventLog`（不可变事件，因果 ID/来源/保留级）、`ArtifactStore`（用户/agent 可编辑 markdown + 知识，CAS 版本/来源/ACL/git 审计）、`AssertionLedger`（不可变语义断言，链证据，**双时相**）——加**可重建投影**（prompt 视图/摘要/FTS/嵌入/图索引，各带 generation/checkpoint）。"工作/情景/语义/程序"降为**产品词汇/视图**，不是硬 crate 边界。
- **[采纳·关键] "双时相"我原来写错了**：`valid_at/invalid_at` 只是 valid-time 单轴。真双时相要 **valid-time + recorded-time 两个区间**（"周三我以为的" vs "周三实际为真的"是两根轴）。Graphiti 的 `created_at`+`expired_at`+`valid_at`+`invalid_at`+`reference_time` 为证。改为双区间断言版本 + as-of 语义。
- **[采纳·关键] 权威按数据产品分，不全局**：原"文件即真源、SQLite 即索引"与 L1(KV)/L3(Fact 表) 自相矛盾。改为：EventLog 权威于情景；AssertionLedger 权威于语义；markdown 只权威于用户创作的 core/知识；FTS/向量表是可弃投影。明确 `memory rebuild` 能/不能恢复什么。
- **[采纳·关键] 补安全/授权/同意模型**：`Scope` 现在只是过滤元数据、可空=无主记忆——不行。强制非空 owner/tenant、不可变 origin/trust 标签、读/写/删/admin 分权、project/personal/public 可见性、显式 vs 自动写的同意、PII/secret 分类、注入隔离、审计。**并修 ADR-029 的洞**：领域模块经 `KernelCtx` 拿的是**能力受限的 filtered handle，不是 ambient `MemoryStore`**（否则 Sin90 DB 物理隔离被架空）。
- **[采纳·高] Condenser 拆开**：现在把 durable retention / 预算选择 / 变换 / 渲染 / **删除策略** 混在一起,且 `forget` 不能和 context-view 策略互换、condenser **绝不删原始事件**。拆 `EventStore/ContextSelector/ContextTransformer/ContextRenderer/RetentionPolicy`；投影返回**带 source event IDs + 理由/分数 + 安全标签 + 预算**的 typed fragments（对齐 codex compaction checkpoint、OpenHands condensation = view delta 非删除）。
- **[采纳·高] Fact 补来源/信念质量**：加断言/证据模型、置信、抽取器/模型版本、观察时刻、说话人、模态("A 说" vs "为真")、矛盾集、派生血缘。**断言与证据分开存**；巩固产出"首选信念"而不抹掉竞争断言。
- **[采纳·高] 自动写策略要 candidate→validate→approve→commit**：**追加断言而非 mutate**；持久化 writer 版本+决策 trace；默认只对**可信用户话语 + 显式 remember 指令**自动写，其余等评测达标（防"恶意网页变成永久偏好/指令"）。
- **[采纳·高] 先别拆 crate**：先在**一个 crate** 里定 capability trait（EventStore/ArtifactStore/AssertionStore/ContextProjector/ProjectionJob），出现真实依赖/发布边界再拆——"一层一 trait"不等于可进化。（仍满足"可拆解"：先模块、后 crate。）
- **[采纳·高] 崩溃一致性/并发契约**：事件 ID 做幂等键、投影 checkpoint、事务化 outbox、人/agent 编辑用 CAS 版本、确定性 replay/rebuild 测试。
- **[采纳·中高] L4 是安全边界**：可执行 procedure（权限/签名/发布者/版本）与只读知识分开；触发文本是注入面。接 ADR-016 的签名 + AirAccount 信任根。
- **[采纳·中高] 本地嵌入=可复现索引**：`Embedding{model_id,revision,dims,normalized,vector}` + 投影 generation + 双索引迁移 + 可续重嵌 + FTS 兜底；oMLX 是一个 adapter,不是架构默认（等 spike 过）。
- **[采纳·中] 补生命周期/用户权利 + 评测契约**：保留/过期/配额/安全擦除/"忘掉我"/备份导出；"invalid≠deleted"。M-D.3b 前先建可回放语料（显式召回/纠正/矛盾/迟到事实/多用户隔离/投毒源/删除/重启重建）。
- **[修正] 研究报告的事实错误**（`RESEARCH-REPORT.md` 已知需订正，Codex C 节 12 条）：OpenHands 这个 checkout 是 **Agent Canvas UI**、不含后端 condenser（我的策略清单来自先验知识、非此 checkout 可证）；letta-code 用 **git-backed MemFS + apply-patch commit**，非经典 MemGPT core/recall/archival；mem0 现为 **V3 单抽取批处理**、非两阶段 enum；codex 有真实 **`codex-rs/ext/memories` Rust 记忆子系统**（我漏了，Rust→Rust 直接可借）；"四层"应为"L0 substrate + 四角色/五层"。

**未全盘照收（裁判保留）**：完整 ACL/PII 分类/投毒隔离的重型治理**分期**做（接 ADR-016 P4），M-D 先落"强制 owner + 能力受限 handle + delete≠invalidate"这三条硬的；"权威+投影"重构**采纳为修订方向**，但产品仍用"工作/情景/语义/程序"词汇（用户心智），二者不冲突。

**下一步**：M-D.1 改为**评测/恢复 spike**（两个投影器 over 现有事件流 + 崩溃/重放测试 + 语料），而非只重命名 trait；spike 过再冻结公开 trait 与是否拆 crate。SPEC-MEMORY 按本收口重写。

---

## ADR-029：Agent24 内核 ↔ 领域 OS（Sin90 / Cos72…）边界 + DomainModule 挂载缝

**日期**：2026-08-21
**状态**：✅ 采纳（边界原则）；`DomainModule` 挂载缝为实现工作项（收口 #103/#104 推迟的"真正的模块挂载 seam" + Sin90KernelCtx）

### 背景

Sin90 是 Agent24 **默认搭载**的 Personal-OS，但它应可**关闭 / 清除 / 替换**成别的领域 OS（如 Cos72 社区成员 OS、BusinessOS）。需要明确 **Agent24 硬基础** 与 **领域 OS** 之间的边界线画在哪。

### 现状：边界已清的四维（今天就成立，代码为证）

1. **crate 依赖单向**：内核 crate（core/agent/store/scheduler/models）**零依赖 sin90**（Cargo 图强制核实）；`agent24-sin90-store` 只反向用内核 util（ulid/now_iso8601）。
2. **数据隔离**：Sin90 有**独立 `sin90.db`**，物理隔离于内核 `agent24.db`。
3. **事件**：Sin90 经**通用 `EventBody::Module{module,kind,payload}`** 信封发事件，内核不认识其语义、只转发。
4. **API**：只经 `/api/v1/sin90/*` REST/WS 触达；外壳（Pet0…）走协议消费，不进程内耦合。

### 现状：唯一未清的一维 = 挂载/替换缝

**边界线在 `agent24d`（组合根）**：它是全仓唯一"按名字认识 Sin90"的地方——`AppState.sin90: Option<Sin90Store>`（具体字段）+ `build_router` 硬编码 `/api/v1/sin90/*` → `crate::sin90::*`（编译进二进制）。没有 `DomainModule` trait / 注册表，所以"清掉 Sin90 换 Cos72"今天需要改 `agent24d` 源码重编。

### 决策

边界线**明确画在 `agent24d`**：内核（所有 `agent24-*` 内核 crate + agent24d 的通用部分）**领域无关**；领域 OS（Sin90/Cos72）活在自己的实现 + 自己的 DB + 自己的路由命名空间 + 自己的 event `module` 名。换 OS 必须是**配置 + 安装脚本驱动的一次性动作，不改内核源码、不重编 agent24d**。

#### 1. 领域 OS 是一个「可安装包」，不只是 crate + 配置

一个领域 OS = **清单 + 实现 + 安装生命周期 + 独立目录**，四件套：

- **清单 `domain-os.yml`**：`name` / `version` / 路由命名空间(`/api/v1/<name>/*`) / event `module` 名 / **资源要求**(需要哪些本地模型、哪些 API/密钥、哪些依赖) / **请求的内核能力**(model 路由 / scheduler / policy——经 `KernelCtx`) / UI 入口(给外壳)。
- **实现**（二选一，同一清单统一描述）：
  - **进程内 Rust crate**（第一方，如 Sin90/Cos72）：实现 **`DomainModule` trait**——`name() / open_store()+migrations / routes() / event_module()`，经 **`KernelCtx` trait** 单向用内核能力。
  - **进程外 Capability Provider**（第三方/多语言，ADR-026 §3）：经协议/MCP 暴露同一套契约，`agent24d` 只代理，**装新 OS 不需重编内核**。
- **安装脚本 / 生命周期钩子**：`install`(校验+按需下载模型、检查 API/密钥/依赖、建独立目录、跑迁移) · `activate` · `deactivate` · `uninstall`(保留或清除独立目录)。
- **独立数据目录 `~/.agent24/os/<name>/`**：装该 OS 的 DB + 资产，于是 `uninstall` = 干净地删这个目录。

#### 2. 配置驱动的激活 + 干净的一次性换装

- 配置里 `active_domain_os: sin90`（默认）。`agent24d` 启动：从注册表解析激活的 OS → 跑其 `install` 校验(模型/依赖到位否) → 经 `DomainModule` 缝挂载路由/store/事件。缺资源就**明确报缺什么**，不半死不活。
- 换装是一条清晰的一次性流水（CLI）：`agent24 os install cos72`（校验+建目录+下模型）→ `agent24 os activate cos72`（翻配置 + 重启）→ 可选 `agent24 os uninstall sin90`（清 `~/.agent24/os/sin90/`）。Sin90 的数据目录不清就**可回退**。

#### 3. 诚实的进程模型取舍（Rust 编译期现实）

- **第一方进程内 OS**（我们自己写的 Sin90/Cos72）：都编进 `agent24d`，配置在启动时**选激活哪一个**——目标 OS 已编入就能**免重编换装**。
- **全新第三方 OS**（未编入）：走**进程外 Provider**路线（ADR-026 的 Node Host/MCP/容器/远程），**完全不动 agent24d**。
- 两条路由同一清单 + 同一 `DomainModule`/Provider 契约描述，对上层一致。

于是三种玩法都成立且都是**干净、一次性、可回退**的动作：**用默认 Sin90** / **基于 Sin90 定制** / **`os install cos72 && os activate cos72` 清掉 Sin90 换 Cos72**。取代今天 `AppState.sin90` 具体字段 + 硬编码 `/sin90/*` 路由。

### 与记忆（ADR-028）的关系

记忆层保持**领域 OS 无关**：内核提供通用记忆（L0–L4），领域 OS **用但不拥有**它；领域态（Sin90 的 direction/proposal…）留在领域 OS 自己的 DB。换 OS 不动内核记忆。

---

## ADR-030：记忆的所有权维度 = (组织, 空间)；组织为一等实体

**日期**：2026-08-23
**状态**：✅ 采纳（不变量）。F8 已实现所有权维度（PR #140）；其余为后续工作项。
**详细设计**：[`docs/specs/SPEC-ORG-SPACE.md`](specs/SPEC-ORG-SPACE.md)

### 背景

ADR-029 画清了内核 ↔ 领域 OS 的边界，但没定义**记忆归谁**。F1（#139）把领域 OS 的记忆分区键定为 `derive(用户, 模块)` —— **那是个人产品的形状**：它让一条记忆的所有者，是当时恰好登录的那个人。

一旦有两个人，真正的所有者是他们共同关联的**容器**（Team Shared / Finance Private / Customer A），人是它的**访问者**。F1 把容器和访问者压成了同一维。

**这一维写进了存储 key，事后改不动** —— 这是本 ADR 存在的唯一理由。

### 决策：只记不变量，不记表结构

以下八条是**改起来贵**的决定。grant 表的列、workspace 的持久化形态、密级级数、group 是否嵌套 —— 全部**不在本 ADR 内**，留给 F9 被真实需求定形。在没有第二个用户时把那些固化成决定，就是把猜测写成契约。

#### 1. 所有权维度 = **(org, space)**，二者的不可变 ID 进 key

一条记忆属于**某个组织里的某个空间**，不属于某个人。个人部署 = **org of 1**，不是另一套架构、不带特例代码。

#### 2. **变得勤的东西，永不进 key**

这是本 ADR 的脊椎，也是 F1 教训的一般化。按变更频率分层：

| 概念 | 在 key 里 | 多久变 |
|---|---|---|
| Org / Space（**仅不可变 ID**） | ✅ | 几乎不变 |
| Member（我默认在哪个 org 做事） | ❌ | 很少 |
| **Group（部门 / 团队）** | ❌ | **经常** |
| Grant | ❌ | **经常** |
| Workspace（此刻作用域） | ❌ | **每天** |

**推论：部门是一个 Group，不是 key 的一层。** 组织重组是企业里最频繁的结构变更；把 `finance` 编进 owner key，等于每次重组重写全部记忆行。部门**可以**拥有一个隔离空间 —— 进 key 的是那个空间的不可变 ID，不是部门标识。

#### 3. **Space 的 ID 不可变、生成、永不复用**

`display_name` 随便改，不进 key。**第一个 shared space 建立之前必须定死** —— F8 的 `partition_key` 把整个 space 字符串编进 owner key，用名字做 ID 就等于「改名 = 弃数据」。

#### 4. **组织是一等实体，其 id 生成而非派生**

`mem_orgs` + `mem_org_members`，按**成员关系**解析。曾评估过 `org_id = "u:" || user`（零迁移），**否决**：一个身份是用户函数的组织，不是「有一个成员的组织」，而是「披着组织名字的用户」—— 它在获得第二个成员的当天就必须换 id，而那个 id 已经编进了每一把 owner key。

#### 5. **成员资格 ≠ 访问权**

- **Member** 只回答「我默认在哪个 org 做事」。**一人一个 home org。**
- **Grant** 回答「我能碰什么」，**可以指向非成员**。

跨组织协作因此是一条 grant，**不是第二个成员身份** —— 后者会让该用户的 org 解析不出来，进而对他的每一个模块扣留记忆（F8 因此显式拒绝这个状态）。

**没有「org 全员」这种授权主体** —— 它会让本条按构造为假（加入即得权限）。要全员就建一个 group 显式加人。

#### 6. **两个主体的交集**：用户 ∩ 模块

一次交互式访问有**谁在问**（user）和**什么在执行**（module）两个主体，有效权限取**交集**：

> 模块不能超出用户的授权；用户也不能借模块去够模块没被授权的东西。

没有这条，装一个第三方模块本身就是一条越权路径。**「交互式」是准确措辞** —— 定时任务 / 服务账号没有「谁在问」，本模型表达不了（见 SPEC §8）。

#### 7. **作用域 ≠ 权限**

权限说「我可以碰什么」，作用域说「此刻什么在里面」，**两者都要允许**。顾问对两个客户都有合法权限；做 A 时 B 不出现，是作用域的事，不是每天改授权。

#### 8. **不做 deny 规则；执行点在句柄发放**

只做并集。有 deny 就有优先级，有优先级就有「为什么我看不到」查不清的那天。

判定发生在**内核决定把哪些空间的句柄交给这个 (user, module)** 的时候，不是每次查询 —— F8 的形状本来如此（句柄绑死一个分区，没有参数能换）。

### 不能等第二个用户的三件事

其余都能等真实需求。这三件一旦有数据，就重演 F1 的错误：

1. **shared / personal space 的不可变 ID**（决策 3）—— 第一个 shared space 之前。
2. **org/space 是否可能跨物理数据库或地区** —— 今天明确是单 SQLite 文件；数据驻留后补要搬迁全部 owner-keyed 数据。写入多地区数据之前。
3. **agent loop 自己那份记忆的归属**（今天用裸 user id，游离在模型外）—— 它与空间模型产生任何交互之前。

### 明确不做（完整清单见 SPEC §8）

不是沙箱 · 不加密隔离 · 不做行/字段级权限 · 不做配额（F7 仍欠）· **表达不了**：外部系统凭据、服务账号与定时任务主体、委托与破窗、子公司/控股结构、数据驻留与法务封存、空间 stewardship、嵌套 group。

**凭据是相对「组织 Agent」这个目标最大的缺口。** ADR-016 定过「API key 经 keychain 存、内核注入」，那解决的是**存**，没解决**谁能用哪个凭据在哪个空间做什么** —— 后者形状与 Grant 一致，应作为一种受管资源接进同一套授权，本期不做。

### 与既有 ADR 的关系

- **ADR-028（记忆分层）**：不变。本 ADR 定的是那些层里的行**归谁**。
- **ADR-029（内核 ↔ 领域 OS 边界）**：不变。每个领域 OS 拿到的仍是一个能力受限句柄 —— 只是那个句柄现在绑定的是**一个空间**，而不是「用户+模块」。
- **ADR-016（模块权限）**：manifest 声明权限的模型继续有效，本 ADR 是它在**记忆**这一维上的细化：模块声明的 `memory:read/write` 将来要落到「对哪些空间」。

### 后续顺序

**F8b**（判定接缝，行为零变化）→ **F8c**（agent loop 记忆进 personal space，硬门槛 3）→ **F9**（grants / groups，**等第二个真实用户**）→ **F10**（持久化 workspace）。与 M-E 台账（F2/F3/F4/F6/F7）的交错见 `SPEC-ME-FOLLOWUPS.md`。

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
| ADR-030 前推荐 `org_id = "u:" || user`（从用户派生），用户追问才改口 | 用"别建没人读的表"去拦一张**有读者**的表（key 派生要它）。对**身份**这类一旦写进 key 就改不动的东西，该问"它是不是一等的"，不是"它现在有没有用" |
