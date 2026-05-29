# Agent24 产品计划与架构

> 完整设计文档，记录从 xiaoheishu/desktop 借鉴 → Agent24 通用框架 → 应用方 fork 这一演进路径上的所有决策。
> 创建日期：2026-04-27

---

## 一、产品定位修正：从"裁剪"到"模块化适配层"

最初设想是从 xiaoheishu/desktop fork 后裁掉特定场景代码（如小红书发布），但这思路是错的。**正确思路是模块化解耦：框架与能力解耦、框架与 AI 模型解耦**。

### 设计原则

1. **框架核心只做演进** — Electron 壳、IPC、模块加载机制、AI 适配层、记忆层、通信层
2. **能力即模块** — blog 发布、小红书发布、微信桥接、文件归档、图像处理 …… 全部抽象为 `CapabilityModule`，按需加载/卸载
3. **AI 解耦** — iDoris 为主 AI 供应商，旁有 Claude / OpenAI / 本地 LLaVA 适配器，业务层不感知具体来源
4. **后台 daemon + 任务自动分解** — desktop 启动即起后台 agent，用户交互后自动拆解任务、调度执行、跨 agent 协调

---

## 二、架构

```
┌──────────────────────────────────────────────────────────────┐
│   Electron Shell（跨平台分发 + UI 一致性）                      │
│                                                              │
│  ┌────────────────────────────────────────────────────────┐  │
│  │ Core Loop（永远在线的本地 agent 主循环）                  │  │
│  │  • 任务拆解  • 调度  • 与用户交互  • 与其他 agent 通信     │  │
│  └─────────────────┬──────────────────────────────────────┘  │
│                    │                                         │
│   ┌────────────────┼────────────────────────┐                │
│   ▼                ▼                        ▼                │
│ ┌────────────┐ ┌──────────────┐ ┌─────────────────────────┐  │
│ │ AI Layer   │ │ Memory Layer │ │ Capability Modules      │  │
│ │ (适配器)    │ │              │ │ (可插拔)                 │  │
│ │            │ │ • 短期: SQLite│ │                         │  │
│ │ • iDoris   │ │ • 长期: ATIF  │ │ ▣ blog 发布             │  │
│ │   (主)     │ │   archive    │ │ ▣ 小红书发布             │  │
│ │ • Claude   │ │ • 工作记忆: KV│ │ ▣ 微信桥接 (iDoris-SDK)  │  │
│ │ • OpenAI   │ │ • 跨设备同步  │ │ ▣ 文件归档              │  │
│ │ • Local    │ │   (Nostr)    │ │ ▣ 图像处理 (Vision LLM)  │  │
│ │   (LLaVA)  │ │              │ │ ▣ Claude Code skills     │  │
│ │            │ │              │ │ ▣ ... 用户自定义模块      │  │
│ └────────────┘ └──────────────┘ └─────────────────────────┘  │
│                    │                                         │
│         ┌──────────┴──────────┐                              │
│         ▼                     ▼                              │
│   ┌──────────────┐    ┌──────────────────┐                   │
│   │ MCP Bridge   │    │ Agent-Speaker    │                   │
│   │ • Agent24    │    │ Bridge (Nostr)   │                   │
│   │ • 任意 MCP   │    │ • 跨 agent 通信   │                   │
│   └──────────────┘    └──────────────────┘                   │
└──────────────────────────────────────────────────────────────┘
                            │
            ┌───────────────┼───────────────┐
            ▼               ▼               ▼
       Local FS         Nostr Relay    Other Agents
```

### 任务流

```
用户输入 → Conversation Layer → Task Decomposer → Executor Pool
                                                       │
                                                       ├─ Local: AI Layer 调用
                                                       ├─ Module: 加载 capability 执行
                                                       ├─ Remote: 通过 agent-speaker 派给其他 agent
                                                       └─ Schedule: cron 化定时任务
                                                       
执行完毕 → 写入 Memory Layer → ATIF 轨迹 → 触发 Evolver
```

Desktop 启动时自动起后台 daemon（Electron tray icon + 隐藏窗口），用户随时唤起对话面板。这对应 SkillClaw 论文的 `evolve-server --interval 300` 模式。

---

## 三、模块分类与 npm Scope

详见 [decision.md](decision.md) 的 ADR-003、ADR-004、ADR-006、ADR-007。

### 3.0 模块分类（三层 + 子层）

```
内核 Core
  ├─ Electron 壳 / IPC / 模块加载器 / 沙箱
  ├─ AI Layer 适配器框架
  ├─ Memory Layer (L0-L3 + SkillBank)
  ├─ Conversation Layer (任务分解 + 调度)
  └─ 设置 + 安全 + 凭据管理

基础模块层（Base）
  ├─ module-identity      ← AirAccount / WebAuthn
  ├─ module-wallet        ← SuperPaymaster (gasless)
  ├─ module-comm          ← agent-speaker / Nostr / NIP-44
  ├─ module-shared-memory ← 跨 agent 知识共享
  ├─ module-storage       ← 加密本地库 + Nostr 同步
  └─ ai-bridge            ← AI Layer 路由聚合

社区模块层（Community）
  ├─ cos72                ← 伞包，依赖下面三个
  │   ├─ myshop           ← 积分兑换
  │   ├─ mytask           ← 任务-积分
  │   └─ myvote           ← 投票
  ├─ communication        ← 复用 base 层，社区维度群组
  └─ shared-memory        ← 复用 base 层，社区共享知识

iDoris 能力包装（独立子层，渐进添加）
  ├─ idoris-input         ← 数据输入（多源融合）
  ├─ idoris-process       ← 处理（清洗、归一、聚合）
  ├─ idoris-query         ← 查询（语义搜索、跨域关联）
  └─ idoris-create        ← 创作（生成内容、洞察）

个人模块层（Personal）
  ├─ 内容发布: publish-blog / publish-xiaoheishu / publish-wechat-mp
  │           publish-xiaohongshu / publish-twitter / publish-linkedin
  │           publish-medium / writer (AI 多平台改写)
  ├─ 信息收集: scrape-web / scrape-rss / scrape-arxiv / scrape-x
  │           monitor-prices / monitor-papers
  ├─ 个人助理: files / vision / voice / calendar / automation / personal-rag
  │           module-wechat (复用 iDoris-SDK)
  └─ 学习创作: courses / inspiration / notes
```

### 3.1 npm Scope `@auraaihq` 命名规则

| 前缀 | 含义 | 例子 |
|------|------|------|
| 无前缀 | 内核 / SDK / 工具 | `@auraaihq/core`, `@auraaihq/cli` |
| `ai-` | AI 模型适配器 | `@auraaihq/ai-claude` |
| `models-` | 模型 metadata（不含权重，详见 ADR-008）| `@auraaihq/models-vision` |
| `module-` | 基础模块 | `@auraaihq/module-identity` |
| `publish-` | 发布器模块 | `@auraaihq/publish-twitter` |
| `scrape-` | 抓取器模块 | `@auraaihq/scrape-rss` |
| `idoris-` | iDoris 能力包装 | `@auraaihq/idoris-query` |
| `skills-` | Claude Code skill（M3+）| `@auraaihq/skills-evolve` |

### 3.2 仓库布局（混合 monorepo）

仓库 `AuraAIHQ/auraai-packages`：

```
auraai-packages/
├── pnpm-workspace.yaml
├── packages/                      # 紧耦合内核
│   ├── core/                      # @auraaihq/core
│   ├── sdk/
│   ├── cli/
│   ├── memory/
│   ├── skill-bank/
│   ├── evolver/
│   ├── ai-claude/ ai-idoris/ ai-openai/ ai-local/ ai-bridge/
│   ├── models-vision/ models-coding/ models-creative/
│   └── module-identity/ module-wallet/ module-comm/ module-storage/ module-shared-memory/
├── community/                     # 社区模块
│   ├── cos72/
│   ├── myshop/ mytask/ myvote/
├── publishers/                    # 发布器（低耦合，未来易拆）
│   ├── blog/ xiaohongshu/ wechat-mp/ twitter/ linkedin/ medium/ xiaoheishu/
│   └── writer/
├── scrapers/
│   ├── web/ rss/ arxiv/ x/
└── idoris/                        # iDoris 能力包装
    ├── input/ process/ query/ create/
```

### 3.3 Capability Module 接口设计延迟

接口规格 v0.1 在 M1 末才冻结（见 ADR-010）。M0-M1 早期不定接口，先做参考实现。

### 3.2 AI Layer 适配器

```
AILayer
├── iDoris    (主 AI — 个人全景洞察系统，本地运行)
├── Claude    (云端 fallback)
├── OpenAI    (云端 fallback)
└── Local     (LLaVA / Qwen2-VL — 离线视觉)
```

业务层只调用 `ai.complete(prompt, modality)`，AI Layer 根据策略路由（隐私敏感 → iDoris；推理强度高 → Claude）。

### 3.3 Memory Layer

借鉴 MemPalace 的分层 + temporal validity，加入 SkillRL 的 SkillBank：

| 层 | 内容 | 存储 |
|---|------|------|
| L0 | 用户身份 + 当前会话上下文 | KV |
| L1 | 重要事实（essential.md 风格） | SQLite |
| L2 | 主题相关记忆（按需加载） | SQLite + 全文索引 |
| L3 | ATIF 轨迹归档（DGM-style） | YAML 多文档 |
| **Skill** | **从 archive 蒸馏的 skill（SkillRL/SkillClaw 风格）** | **SKILL.md + index** |

跨设备同步：通过 Nostr relay 加密同步（NIP-44 + agent-speaker）。

### 3.4 Evolver（自进化引擎）

借鉴 SkillClaw：

```
ATIF Archive (results.log)
    │
    ▼
Pattern Miner ─── 识别 N 次重复出现的行为模式
    │
    ▼
Evolver Decision (LLM)
    ├─ Refine 已有 SKILL.md
    └─ Create 新 SKILL.md
    │
    ▼
Validation Worker (Codex MCP) ─── 校验质量
    │
    ▼ (通过)
SkillBank 主分支
    │
    ▼
所有 desktop 实例同步（通过 Nostr）
```

---

## 四、SkillClaw 借鉴评估

**核心思路**：每个用户的轨迹 → Evolver 提炼 → SKILL.md 更新 → 全员受益。

**对 Agent24 适配性**：极高。我们已有 DGM-style archive (`results.log`) + MemPalace 分层记忆 + Codex 外部评估，**缺的就是从 archive → SKILL 的自动炼化环节**。

**直接借鉴**（来自 SkillClaw `skillclaw/` 模块）：
1. **`skill_manager.py` Refine vs Create 决策** — 现 Phase 4 只写 memory，从不改 SKILL.md。加 `meta-evolve` skill：扫描 archive 重复 pattern，决定 refine 现有还是 create 新 skill
2. **`validation_worker.py` + `prm_scorer.py`** — Codex 评估器作为 validated publish gate，新 SKILL 必须通过 review 才合并
3. **OpenAI-compatible proxy 模式（`api_server.py`）** — desktop 暴露此接口，让任何外部工具透明获得"自进化"能力

**License**：MIT，无限制借鉴。

---

## 五、Top-5 自进化/长记忆 repos 调研

| 项目 | Stars | 借鉴价值 | 状态 |
|------|------:|---------|------|
| [DGM (jennyzzt/dgm)](https://github.com/jennyzzt/dgm) | 2k | Population archive + 血统追踪 | ✅ 已用 |
| [EvoAgentX](https://github.com/EvoAgentX/EvoAgentX) | 2.9k | MCTS 工作流搜索 | 暂不急 |
| [Voyager](https://github.com/MineDojo/Voyager) | 6.9k | 可执行 skill 库 + 课程学习 | 对非具身过重 |
| [SkillRL](https://github.com/aiming-lab/SkillRL) | 681 | **分层 SkillBank + 自适应检索** | **强烈推荐** |
| [MemSkill](https://github.com/ViktorAxelsen/MemSkill) | 449 | 离线轨迹蒸馏 evolving skills | 中度 |

**最值得借鉴**（独立于 SkillClaw）：
- **SkillRL 的分层 SkillBank** — 在 MemPalace L0-L3 之外加 Skill 层：从 archive 蒸馏 general / task-specific 启发式
- **SkillClaw 的 validated publish gate** — Codex 已在做评估，正好接上

---

## 六、Roadmap（里程碑）

```
M1 (4-6周) — Desktop 壳 + 模块化骨架
  □ AuraAIHQ/Agent24 仓库（已建）
  □ 借鉴 vendor/xiaoheishu/desktop 的 Electron + Vite + React 架构
  □ 抽象 CapabilityModule 接口
  □ xiaoheishu 发布逻辑包装为第一批模块（blog / 小红书 / 微信公众号）
  □ AI Layer 适配器：iDoris (placeholder) / Claude / Local LLaVA
  □ 基础对话 UI + 文件浏览器

M2 (6-8周) — Agent 永远在线 + 通信
  □ Tray icon + 后台 daemon
  □ 任务分解器（Conversation → Tasks → Executor Pool）
  □ MCP bridge 接 Agent24 skills
  □ Agent-Speaker bridge 跨 agent 通信
  □ iDoris-SDK 作为模块加载（微信能力）

M3 (8-10周) — Memory + Evolver
  □ 短期/工作/长期记忆分层（参考 MemPalace）
  □ ATIF 轨迹采集（参考原 autoagent）
  □ SkillClaw 风格 Evolver：archive → SKILL refine/create
  □ Codex 评估作为 validated publish gate
  
M4 (10-12周) — 自进化 + 共享
  □ SkillRL 分层 SkillBank 集成
  □ 跨用户 skill 共享（用户自愿，匿名 trajectory）
  □ 通过 Nostr 分发 skill 更新
  □ iDoris 主 AI 接入（替换 placeholder）

M5 (后续) — 生态
  □ Module marketplace
  □ 跨设备记忆同步
  □ 个人 agent ↔ 组织 agent ↔ 公共 agent 三级网络
```

---

## 七、与 xiaoheishu 的兼容与演进

- **当前**：xiaoheishu/desktop 是源参考，作为 submodule 引入
- **M1 完成后**：从 xiaoheishu/desktop 提取通用代码到 Agent24 主目录，xiaoheishu 特定能力（小红书发布等）抽离为独立模块
- **M2-M3**：xiaoheishu 切换为基于 Agent24 fork，自身只维护小红书相关模块和 UI 差异
- **接口契约**：双方共同维护 `CapabilityModule` 接口，保持向后兼容
- **同步策略**：Agent24 是源，xiaoheishu 周期性 rebase 上游

---

## 七.5、生态整合计划（M2 + M3）

详见 ADR-013、ADR-014（[decision.md](decision.md)）。

### M2 阶段：iDoris-SDK 合并到 monorepo

- 代码迁入 `auraai-packages/communication/wechat-bridge/`
- npm 包：`@agent-wechat/core` → `@auraaihq/wechat-bridge`（老包 deprecate）
- simple-agent 同步升级
- `AuraAIHQ/iDoris-SDK` 仓库归档

### M3 阶段：Agent24 替代为 npm 包

- 4 个 SKILL.md 拆为：
  - `@auraaihq/skills-evolve`
  - `@auraaihq/skills-evaluate`
  - `@auraaihq/skills-setup`
  - `@auraaihq/skills-org-sync`
- `install.sh` 替代为 `@auraaihq/cli install <skill>`
- agent-config.yaml 作为 skills-evolve 默认 config
- hooks 作为各 skill 包自带
- `AuraAIHQ/Agent24` 仓库标记为 **deprecated**：archive 只读，README 顶部加 deprecated banner 引导到 npm 包
- M3 末紧接着 `AuraAIHQ/Agent24` rename → `AuraAIHQ/Agent24`（详见 ADR-015）

### 整合后生态规模

活跃 repo: 7 → **3-4**
- AuraAIHQ/Agent24 (Electron 应用)
- AuraAIHQ/auraai-packages (npm monorepo)
- AuraAIHQ/iDoris (AI 模型)
- AuraAIHQ/agent-speaker (Go 通信，不进 npm)

归档 repo:
- AuraAIHQ/Agent24
- AuraAIHQ/iDoris-SDK

---

## 七.6、三方贡献 + UI 启停愿景

最终形态——**用户在 Desktop UI 看到模块清单，逐个 toggle 启停；任何符合 SDK 标准的第三方包都能出现在这里**。

### 核心 Loop

```
┌────────── 用户面 ──────────┐
│  Agent24 UI        │   ← 模块管理面板
│  ┌───────────────────────┐ │
│  │ ☑ identity   (启用)   │ │
│  │ ☑ wallet     (启用)   │ │   ← toggle on/off
│  │ ☐ publish-twitter     │ │      ↓
│  │ ☑ publish-blog        │ │   调内核 load() / unload()
│  │ ⊕ Browse marketplace  │ │      ↓
│  └───────────────────────┘ │   M4 后：可从 marketplace install
└──────────────┬─────────────┘
               │
       ┌───────▼────────┐
       │ @auraaihq/core │       ← 内核（已建，PR #6）
       │  - 注册 / 加载 │          - kernel.register()
       │  - 卸载 / 路由  │          - kernel.load(id) / unload(id)
       │  - 沙箱 / 权限  │          - 加载时 sdkVersion 校验
       └───────┬────────┘
               │ 任何实现 SDK 接口的包都能加载
   ┌───────────┴──────────────────────┐
   │     @auraaihq/sdk 公共契约 (PR #5) │   ← 第三方实现这个接口即可
   │  Module / ModuleManifest /        │
   │  Intent / Permission / Result     │
   └───────────┬──────────────────────┘
               │
   ┌───────────┴────────────┬─────────────┬──────────────┐
   ▼                        ▼             ▼              ▼
预设官方模块               社区贡献       第三方付费       本地实验
@auraaihq/module-*        @community/*    @vendor/*       npm link
@auraaihq/publish-*       @x/cool-tool                    file:./...
@auraaihq/scrape-*
@auraaihq/idoris-*
```

### 实现路线（三步走）

| 阶段 | 用户能做 | 技术建设 |
|------|---------|---------|
| **M1**（当前）| 没有 UI，开发者 npm install 后通过 main process 代码 register | ✅ SDK 契约 + 内核加载器 完成（PR #5、#6） |
| **M2** 模块管理 UI | 看到已 install 的模块列表 + toggle 启停 | Desktop 加 module manager UI（React 组件 + IPC bridge）；toggle 调 `kernel.load(id)` / `kernel.unload(id)` |
| **M3** 装/卸载流程 | UI 后台 `pnpm add @auraaihq/publish-twitter` 安装包 | Desktop 加 npm install runner（独立 child process）+ 重启 kernel reload |
| **M4** Marketplace | 浏览社区/第三方模块，一键 install | 模块发现服务（Nostr 索引或 npm scope 扫描）+ 信任校验（ADR-016：签名）|

### M2 模块管理 UI 设计要点

```ts
// Desktop main process 暴露给 renderer 的 API（preload bridge）
interface ModuleManagerAPI {
  list(): Promise<ModuleListItem[]>          // 已 install 列表
  enable(id: string): Promise<void>          // = kernel.load(id)
  disable(id: string): Promise<void>         // = kernel.unload(id)
  getDetails(id: string): Promise<ModuleDetails>  // manifest + 权限说明
  // M3+
  install(packageName: string): Promise<void>
  uninstall(id: string): Promise<void>
  // M4+
  searchMarketplace(query: string): Promise<MarketplaceResult[]>
}

interface ModuleListItem {
  id: string
  name: string
  version: string
  state: 'enabled' | 'disabled' | 'failed'
  permissions: Permission[]   // 用户启用前需确认这些权限
  lifecycle: ModuleLifecycle
}
```

### 三方贡献者的体验（最终形态）

```bash
# 1. clone + 写一个新模块
npm init @auraaihq/module my-cool-feature
# 脚手架生成 src/index.ts implementing Module interface

# 2. 本地测试
pnpm link --global              # 或 npm link
# 然后在 Desktop UI 里看到（M3 加 file:// 模块发现）

# 3. 发布到 npm
pnpm changeset add
pnpm publish

# 4. 其他用户安装
# 在他们的 Desktop UI marketplace 里搜索 → 一键 install
```

### 关键决策记录

- **预设模块 = 我们维护的标杆参考实现**：identity/wallet/comm/storage 这些"基础"模块由我们维护，作为 SDK 怎么用的范例
- **社区贡献门槛 = 实现 SDK 契约 + 通过 npm scope 命名规则 + 自描述 manifest**：不需要审批，发到 npm 就能用
- **信任分层（M3+）**：
  - 官方 `@auraaihq/*` 默认信任
  - 社区 `@community/*` 提示但不阻拦
  - 第三方 `@anyone/*` 显式授权 + 显示权限申请
- **关闭 ≠ 卸载**：toggle off 调 `kernel.unload(id)`，模块 npm 包仍在；卸载是 M3 后单独操作

### 与现有架构的契合

我们已经在 PR #5 (sdk) 和 PR #6 (core) 里把基础打牢：
- ✅ `Module<TIntent>` 接口存在
- ✅ `ModuleManifest.intents` / `lifecycle` / `sdkVersion` 字段已定
- ✅ `Permission` 枚举确定（M2 加 `module:invoke:{id}` runtime check）
- ✅ Kernel 的 `register / load / unload / list` 完整
- ✅ `assertNoDuplicateIds` / sandbox 准备 / 权限路由 都齐

**M2 只需要做 UI 这层 + main↔renderer IPC bridge。基础设施 done。**

---

## 八、相关仓库

| 仓库 | 角色 | URL |
|------|------|-----|
| AuraAIHQ/Agent24 | 自进化 Skills 系统（Claude Code skills）| https://github.com/AuraAIHQ/Agent24 |
| AuraAIHQ/Agent24 | 跨平台 Electron 框架（本仓库）| https://github.com/AuraAIHQ/Agent24 |
| AuraAIHQ/iDoris | 个人全景洞察 AI（Prism 启发）| https://github.com/AuraAIHQ/iDoris |
| AuraAIHQ/iDoris-SDK | 微信桥接 SDK（前 MushroomDAO/Agent-WeChat-SDK）| https://github.com/AuraAIHQ/iDoris-SDK |
| AuraAIHQ/agent-speaker | Nostr-based agent 通信 | https://github.com/AuraAIHQ/agent-speaker |
| AuraAIHQ/simple-agent | 微信场景的 Level 1 agent | https://github.com/AuraAIHQ/simple-agent |
| MushroomDAO/Xiaoheishu | 应用参考（M1 后从 Agent24 fork）| https://github.com/MushroomDAO/Xiaoheishu |
