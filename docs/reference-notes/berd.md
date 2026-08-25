# 研读笔记：Berd — 桌面 Agent 工作台（block/berd，Apache-2.0）

> 来源：`vendor/berd/`（github.com/block/berd，**Apache-2.0**，本地只读克隆 @ `20c005ae`，2026-08-24，`v0.6.2`）
> 日期：2026-08-25 | 用途：Agent24 桌面壳 + 工程纪律的设计输入
> 所有 `path:line` 相对 `vendor/berd/`（涉及本仓库代码时给出仓库内完整路径）。
>
> 配套笔记：[`macro.md`](macro.md)（统一工作区）。两者都用 **ACP**，见 §9。

---

## 0. 一句话定位 + 规模

**「一个桌面壳，把 agent 运行时（Goose）包装成日常能用的工作台」** ——
它自己**不跑 agent loop**，通过 ACP over WebSocket 连一个 `goose serve` sidecar。

| 维度 | 数字 |
|---|---|
| 前端（React 19 + TS） | ~448,000 行（含 sdk 与生成代码） |
| Rust（Tauri 侧） | ~72,000 行 |
| `src/features/` 特性目录 | **32** |
| 自有 SQLite 迁移 | **3 个文件，全部只跟 layout 有关** |
| 版本 | v0.6.2 |

技术栈：**Tauri 2 + React 19 + Vite + Biome + Playwright**，后端 = 上游 **Goose**（Block 自己的 agent 框架）。

**最重要的一个数字是那个「3」。**
Berd 有 32 个特性目录、45 万行前端，**但它自己的持久化只有三张 layout 表**
（`src-tauri/migrations/`：create_layout / remove_layout_item_kind_check / add_layout_item_widget_state）。
会话、agent、skill、项目、记忆 —— **全部住在 Goose 那边**（`~/.config/goose/`）。

> **这就是它的核心架构判断：壳只拥有「壳的状态」，agent 状态归运行时。**
> 这条判断让它可以随时换后端版本（§2），也让「用户的数据是自己的、在自己的文件系统上」成立
> —— `distro/agents/berdy.md` 里那段对用户讲的话不是营销，是架构事实：
> 「everything Berd remembers about them lives in plain text files on their own computer」。

---

## 1. 架构总览

```
Berd (Tauri 2 桌面应用，Apache-2.0)
 ├─ src/                React 19 前端 —— 32 个 feature，UI + 编排 + 唯一的信任边界(§5)
 ├─ src-tauri/
 │    ├─ plugins/berdctl      本机 broker（纯传输）
 │    ├─ plugins/app-test-driver
 │    ├─ crates/berdctl       给 agent 用的 CLI
 │    ├─ crates/berd-voice    语音
 │    └─ migrations/          只有 layout
 ├─ sdk/                @aaif/goose-sdk —— ACP 的 TS 客户端 + 从后端 schema 生成的类型/zod
 └─ distro/             分发缝：bundled agents / skills / goose config / bin
        ↓ ACP over WebSocket
   goose serve （sidecar，版本由 goose-backend.lock.json 钉死）
        ↓
   Claude / OpenAI / Gemini / Ollama / OpenAI-compatible
```

## 2. ⭐ 后端用 lockfile 钉死，像依赖一样管理

```bash
scripts/update-goose-backend-lock.sh main   # 改 goose-backend.lock.json（走 PR）
just goose-sync                             # 拉取并构建那个被钉住的 commit
```

`just dev` **复用被钉住的构建产物，且在 lockfile 的 commit 与缓存不一致时直接失败**（`README.md:24`）。
想用自己的 goose？必须显式 `GOOSE_BIN=/path/to/goose`，README 明说这是
「an explicit local override and bypasses the managed pinned checkout」。

打包时 Tauri 把它作为 **external sidecar** 装进去：
`src-tauri/binaries/goosed-<rust-host-triple>`，对应 `tauri.conf.json` 的 `"externalBin"`。

> **对我们**：Agent24 也有一个「壳 + 守护进程」的分裂（桌面托盘 F1b ↔ `agent24d`），
> 但我们的两侧是同一个仓库同一次构建，**版本天然一致**，这条对我们价值有限。
> **真正有价值的是 oMLX / 模型运行时那一侧**：我们今天对「用哪个版本的 oMLX、哪个权重」
> 靠约定和文档。**lockfile + 「不一致就 fail，不静默回退」是可以直接搬的做法。**

## 3. ⭐⭐ `LAWS/` —— 把产品不变量写成 RFC 2119 法律，与 feature spec 分开

`LAWS/README.md:1`：
> 「架构法律定义 Berd **必须**具备的产品与体验行为。它们是**产品做什么**的唯一权威，
> 与当前实现如何组织**无关**。」

规则本身写得极克制（`LAWS/README.md:16`）：

- 法律**必须**描述**产品中可观察**的行为，**不得**描述实现细节；
- 法律**必须**表达**刻意确立的、持久的**不变量或边界；
- 「可观察 + 已定」**不足以**成为法律 —— 必须产品**明确选择**把它封为持久约束；
- 每条法律**只表达一个要求**；
- **代码与测试必须符合法律**；
- 改变可观察行为的 PR **必须指出受影响的法律**，并让代码/测试/法律三者达成一致。

`LAWS/AGENTS.md` 全文只有一句：
> An agent MUST have a configured provider and model before it can be invoked.

`LAWS/CHAT.md` 则是 20 多条关于「composer 队列 ↔ session 派发」的不变量，例如：

- 一条消息**不得**在其会话就绪前被派发；
- 派发失败**必须**让该消息留在队列首位；
- 队列**不得**以任何方式派发出**多于一个用户轮次**；
- 对队列中某条消息的编辑**不得**改变它的位置。

> **对我们 —— 这条最值得学，而且我们已经差一点就走到了。**
> Agent24 有 `docs/decision.md`（ADR，记**为什么**）、`docs/specs/`（记**怎么做**）、
> `docs/agent/architecture.md` 的「不可动摇的边界」（记**不许什么**）。
> 最后这一节**已经是法律的雏形**，但它：
> ① 混在架构文档里；② 是散文不是可逐条引用的编号条款；③ **没有「PR 必须指出受影响的法律」这条闭环。**
>
> F1/F8 加起来二十多轮复审、每轮都在抓「措辞比机制强」——
> **那正是「法律与实现不一致」的另一种说法。** 见 §10.1。

## 4. ⭐ `distro/` —— 分发缝：企业定制不进公开源码树

```
distro/
  distro.json   分发级默认值（marketplace URL 模板、后端 endpoint…）
  config.yaml   传给 goose serve 的可选配置
  bin/          prepend 到 PATH 的可执行文件
  skills/       种到用户全局 skills 目录的 bundled skills
  agents/       种到用户全局 agents 目录的 bundled agents
```

解析顺序：`GOOSE_DISTRO_DIR` 环境变量 → Tauri `resource_dir()/distro`（`distro/README.md:13`）。

README 把这件事说得很直白（`README.md:8`）：
> 「本仓库构建一个**通用公开发行版**。组织可以通过仓库的**分发缝**提供托管的 provider 设置、
> 私有资源和发布基础设施来创建**企业发行版**，**而不必把私有材料加进公开源码树**。」

`distro.json` 的字段校验也很讲究：`skillUrlTemplate` **必须** HTTPS、**必须**含且只含一个
`{skillId}` 占位符、**不得**出现在 URL 的 authority 部分、**不得**带凭证或 fragment
（`distro/README.md:44`）—— 一个配置字段，六条校验。

> **对我们 —— 这是「开源 + 商业双生」的工程答案。**
> `MISSION.md` 写着 MushroomDAO 做数字公共物品、HyperCapital 做商业发行。
> **今天我们没有任何机制保证这两者不互相污染。**
> Berd 的答案是：**公开构建必须自足**（"The public build is self-contained and does not require
> private package registries or enterprise credentials"），私有物**只能**通过预定义的缝叠加。
> 这条应该在 Cos72 商业化之前就定下来，而不是之后再拆（见 `macro.md` §10 那条许可证教训的同构版）。

## 5. ⭐⭐ `berdctl` —— 三层 CLI，信任边界画在**最里面**

`docs/berdctl-architecture.md:1`：agent 用来控制桌面应用的 CLI。

```
1. CLI      src-tauri/crates/berdctl/     clap 解析、读发现文件、发 JSON
                                          「CLI validation is convenience only」
2. Broker   src-tauri/plugins/berdctl/    localhost 服务器，拒绝浏览器 origin，
                                          限并发/超时，转发 —— 无任何命令语义
3. Registry src/features/berdctl/commands/ zod strict 解析、guard、执行
                                          ★ 这里是信任边界
```

**为什么信任边界在第 3 层**，文档说得斩钉截铁（`docs/berdctl-architecture.md:18`）：
> 「**任何同用户进程都可以绕过 CLI 直接 POST 给 broker。**」

层规则同样硬：
- **Broker 只做传输** —— host/origin 检查、握手、并发上限、超时、请求关联。
  **不得**出现命令名词、动词、动作名或任何命令相关策略；
- **Registry 拥有策略** —— zod `.strict()`、边界值、安全元数据、运行中会话的守卫、状态变更；
- **CLI 拥有 agent 体验** —— 稳定的 flag 名、本地解析错误、退出码、**手写的 help**
  （"Agents should be able to rely on `--help`"）。

每个命令模块自带：zod strict schema（每个字段都要 `.describe()`）、guardrail 边界、
`summary`/`description`/`helpFooter`、**安全元数据 `effect` / `visibility` / `destructive`**、
`precheck` 与 `execute`。而且 `just check` 里有一条 **`berdctl-contract-check`**
（`justfile`）—— **契约漂移会让 CI 红。**

> **对我们 —— 这条直接命中 ME-2b。**
> 我们刚做完 `agent24 os` CLI over a daemon-owned registry（#134），形状**几乎一样**。
> 但对照之下有三处我们没有：
> 1. **「CLI 校验只是便利」这条判断有没有写下来？** 如果 `agent24d` 的 HTTP 面
>    信任了 CLI 已经校验过的东西，那就是同一个洞 —— 任何同用户进程都能直接打 daemon。
> 2. **命令的安全元数据**（`destructive` / `effect`）—— 我们的审批门今天靠工具名判断风险
>    （见 `openworker.md` §1 记过的 `RiskClass`），而 Berd 把它做成**每个命令自带的声明**。
> 3. **契约漂移的 CI 检查** —— `os list` 的输出形状变了，今天没有任何东西会红。
>    而 `SPEC-ME-FOLLOWUPS.md` F4a 抓到的正是这个病的另一个病灶：
>    「`lint:openapi` 只 lint 已存在的东西，CI 抓不到『少写了一个端点』」。**同一个坑，两个位置。**

## 6. Agent = 一个 markdown 文件

`distro/agents/*.md` —— frontmatter + 提示词正文，就是全部：

```markdown
---
name: choosey
display_name: Choosey
description: Makes choices clearer without making them for you.
avatar: app-avatar:gloopies-6
metadata:
  berdBundled: true
---

You are Choosey. …
```

7 个 bundled agent：`berdy`（新手向导）、`choosey`（帮你在选项间收敛）、
`pushback`（挑刺）、`wildcard`（发散）、`copycat`、`tinker`、`agt-builder`（做 agent 的 agent）。

值得注意的是**角色之间是互相知道边界的**。Choosey 的提示词里写着：
> 「你不发明新选项（那是 Wildcard 的活），也不单独加强某一个选项（那是 Pushback 的活）。
> 如果有人给你一个选项问『这个好不好』—— **那不是你的赛道，直说，然后把人指给 Pushback。**」

**这是一个多 agent 系统里很少见的设计：不靠 orchestrator 分派，靠每个 agent 自己知道该把球传给谁。**

`berdy.md` 还有一段特别值得读的（关于「记忆该放哪」）：
> Settings 放应用偏好 · 全局 hints（`~/.config/goose/AGENTS.md`）放**规则** ·
> memory extension 放**事实** · 而 **skill / agent / project / automation 本身也是一种记忆**。
> 「『你已经三次让我说话简短点』是一条记忆。『你每周一都做这个』是一条自动化。
> 『那个通知很烦』是一个设置。『替我发东西前永远先问我』是一条全局规则。」

> **对我们**：Agent24 的 `~/.claude/skills` + MemPalace + Sin90 之间**没有这样一张对照表**。
> 「什么该进记忆、什么该进 skill、什么该进配置」今天是凭感觉。这张表可以直接借用。

## 7. Skill 的三个来源分得很清

| 位置 | 用途 |
|---|---|
| `skills/` | **公开可移植** Agent Skills，可独立于 Berd 应用安装 |
| `distro/skills/` | 随应用捆绑、种进用户全局目录（`agent-builder` / `berd-help` / `skill-builder`） |
| `.agents/skills/` | **贡献者工作流**用的，不是给终端用户的（`code-review` / `create-pr` / `berdctl-new-command` / `experimental-features` / `assistive-ux`） |

README 专门澄清这三者互不相同（`README.md:60`）。
`.agents/checks/design-system-tokens.md` 则是给 agent 读的**检查清单**。

> **对我们**：Agent24 也有三类 skill 混住（用户的 `~/.claude/skills`、仓库 `.claude/`、
> 未来 Cos72 要分发给社区的）。**这条分类现在划清，比以后拆便宜。**

## 8. 工程纪律：`just check` 里有几条我们没有的

```
just check = design-system-check
           + berdctl-contract-check     ← §5
           + frontend-fmt-check + lint + i18n-check + typecheck
```

`design-system-check` 自己又是四条（`justfile`）：
- `design-system-manifest-check` —— 清单一致；
- `design-system-tokens` —— **应用里的颜色用法必须符合 token 契约**（不许写死色值）；
- `design-system-audit`；
- `design-system-coverage --strict` —— **组件页面必须符合页面契约**。

外加 `i18n-check`（`scripts/check-i18n-strings.mjs`）—— **硬编码英文字符串会让 CI 红。**

而 `DESIGN.md` 本身是一份**从真实 token 反向生成的**设计系统文档
（"documented from the actual Berd design-system tokens"，`DESIGN.md:3`）——
**文档不是手写的承诺，是从事实导出的。**

> **对我们**：Agent24 的桌面壳今天没有任何设计系统检查。
> 更值得注意的是那条元规则：**「文档从事实生成」比「文档描述事实」强一个量级** ——
> 这与我们 F1/F8 复审反复抓的「措辞比机制强」是同一件事的正面版本。

## 9. 两个仓库都用 ACP —— 这是一个信号

- **Berd**：ACP 客户端，通过 WebSocket 连 `goose serve`（`README.md:3`、`sdk/README.md:3`）；
- **Macro**：`agent_runtime_protocol` 是「**携带 ACP 消息 + 运行时控制消息、不解释被包裹的 ACP 载荷**
  的外层协议」（`vendor/macro/crates/agent_runtime_protocol/src/lib.rs:4`）。

两个互不相干的团队（Block / Macro Inc.）在 2026 年都把 ACP 当作
**「壳 ↔ agent 运行时」的边界协议**。

> **对我们**：Agent24 今天的 `agent24d` ↔ 桌面壳走自定义 HTTP + WebSocket。
> ME-3（进程外 Provider）已经在规划里，**那正是要选一个协议的地方**。
> **建议：ME-3 开工前，把「自定义协议 vs ACP」作为一条显式的 ADR 来裁决**，
> 而不是默认延续自己的。理由不是「大家都用」，是**互操作**：
> 采用 ACP 意味着 Agent24 的领域 OS 可以被 Berd 之类的壳挂载，反过来 Goose 也能成为
> Agent24 的一个 provider —— 对 Mycelium「共生」那条范式，这是实打实的接口红利。
> （**这条是建议，不是结论**：ACP 的能力覆盖度、与我们审批门/`ScopedMemory` 的契合度都没验证过。）

---

# 第二部分：怎么学、学什么

## 10. 按「现在就能做 / M2 顺手做 / 需要立项」分三档

### 10.1 现在就能做（不依赖任何里程碑）

| # | 学什么 | 怎么落地 | 成本 |
|---|---|---|---|
| **A** | **`LAWS/`** | 把 `docs/agent/architecture.md` 的「不可动摇的边界」六条 + `SPEC-ME-FOLLOWUPS.md` F1 那些「不得声称的话」，抽成 `docs/laws/*.md`，逐条编号、一条一个要求、用 MUST/MUST NOT。**再在 `.github/PULL_REQUEST_TEMPLATE` 加一行：「本 PR 影响哪几条法律？」** | 半天 |
| **B** | **记忆归属对照表**（§6 berdy） | 写进 `docs/` 一页：什么进 memory、什么进 skill、什么进配置、什么进 automation | 1 小时 |
| **C** | **skill 三来源分类**（§7） | 现在划清 `.claude/` vs 用户全局 vs 未来 Cos72 分发 | 1 小时 |

**A 是三条里最有价值的。** 理由很具体：F1 六轮、F8 六轮复审，抓到的**几乎全是**
「文字承诺 > 机制实际」。`LAWS/` 的贡献不是又一份文档，是那条闭环 ——
**「改变可观察行为的 PR 必须指出受影响的法律，并让代码/测试/法律三者达成一致」**。
我们今天有前两者，缺第三者，于是每次都靠复审的人肉记忆去发现不一致。

### 10.2 M2（M-E 收口）顺手做

| # | 学什么 | 对应我们已知的债 |
|---|---|---|
| **D** | **契约漂移 CI**（§5） | 直接治 `SPEC-ME-FOLLOWUPS.md` **F4a**（「CI 抓不到少写了一个端点」）。Berd 的做法是 `generate-berdctl-contract --check`：**从命令模块生成契约，再和签入的对比**。我们对 `protocol/openapi.yaml` 可以做同一件事 |
| **E** | **命令安全元数据**（`effect`/`destructive`） | `agent24 os` 的命令目前没有声明式风险等级；审批门也还在按名字判断 |
| **F** | **「CLI 校验只是便利」写进契约** | 需要**核实** `agent24d` 的 HTTP 面是否独立校验了 `agent24 os` 传来的东西。**如果没有，这是一个真实的洞，不是文档问题** |

**F 需要先查再说 —— 我这次没有查。** 它可能已经是对的（daemon 侧有 zod/serde 校验），
也可能不是。**在核实之前不要把它写进任何里程碑，也不要说成「我们有这个问题」。**

### 10.3 需要立项

| # | 学什么 | 放哪 |
|---|---|---|
| **G** | **分发缝 `distro/`**（§4） | Cos72 商业化之前。对应 `macro.md` §12 的 **M5**（Cos72 上线 = 一条 install 流水）—— 分发缝就是那条流水的形状 |
| **H** | **ACP 作为壳↔运行时协议**（§9） | **ME-3 的前置 ADR**。先裁决，再开工 |
| **I** | **oMLX 版本 lockfile**（§2） | 模型运行时的版本管理，登记进 M2 跟进项 |
| **J** | **设计系统 CI**（§8） | 桌面壳真正做起来的时候（`macro.md` §12 的 M6） |

## 11. Berd 本身值不值得 fork？——不建议

用户问过「fork Berd 当作 iDoris / Agent24 的桌面工作台」。三条理由说不：

1. **它的价值绑死在 Goose 上。** 45 万行前端，服务的是「ACP 客户端 + Goose 配置管理」。
   Agent24 的内核是我们自己的 Rust `agent24d` + `DomainModule` + `ScopedMemory` + 审批门。
   fork 之后要么把 Goose 换掉（等于重写它最核心的那部分），要么把 Agent24 内核降级成 Goose 的一个 extension
   （**等于放弃 M-D/M-E 全部成果**）。
2. **它不接受外部 PR**（`README.md:76`：outside PR 会被自动关闭）。
   fork 之后是**永久分叉**，上游每一个改进都要手工搬。
3. **我们的桌面壳需求今天还很小**（托盘 + 状态 + 启停，F1b 已 done）。
   为一个还没有形状的需求扛 45 万行，是买了一个负债。

**但它是 Apache-2.0，所以可以做一件更划算的事**（对比 `macro.md` §10 的 AGPL 禁令）：
> **可以直接借用具体文件**，保留版权头、更新 `NOTICE`。
> 首选目标：`scripts/generate-berdctl-contract.mjs`（契约检查，对应 §10.2 的 D）
> 与 `scripts/check-i18n-strings.mjs`。这两个是自足的小脚本，价值高、耦合低。

## 12. 一句话总结

> **Macro 教我们「一个系统该长什么样」，Berd 教我们「一个团队该怎么保证它不走样」。**
>
> Berd 最该抄的三样都不是代码：
> **① `LAWS/` —— 不变量与 PR 的闭环**（现在就做，半天）；
> **② berdctl 三层 —— 信任边界画在最里面 + 契约漂移让 CI 红**（M2 顺手）；
> **③ `distro/` 分发缝 —— 公开构建必须自足，私有物只能叠加**（Cos72 商业化的前置）。
>
> 而它的整体形态**不该抄**：它是一个 Goose 的壳，我们有自己的内核。
> 我们不需要一个更好的壳，我们需要壳外面那些让 191 个 crate 不失控、让 20 轮复审有地方落的纪律。

---

## 附：核对清单（本笔记的事实来源）

| 断言 | 出处 |
|---|---|
| Apache-2.0 / 不接受外部 PR | `LICENSE`、`README.md:76` |
| Tauri 2 + React 19 + ACP WebSocket | `README.md:3` |
| 自有迁移只有 layout 三张 | `src-tauri/migrations/` |
| 后端 lockfile + 不一致就 fail | `README.md:24`、`goose-backend.lock.json` |
| sidecar 打包 | `README.md:40` |
| LAWS 规则 | `LAWS/README.md:16` |
| LAWS 实例 | `LAWS/AGENTS.md`、`LAWS/CHAT.md` |
| 分发缝 / 公开构建自足 | `README.md:8,46`、`distro/README.md` |
| berdctl 三层 + 信任边界 | `docs/berdctl-architecture.md:1,18` |
| 命令自带安全元数据 | `docs/berdctl-architecture.md:38` |
| agent = 单个 markdown | `distro/agents/*.md` |
| agent 之间互知边界 | `distro/agents/choosey.md` |
| 记忆归属对照 | `distro/agents/berdy.md` |
| skill 三来源 | `README.md:60`、`skills/`、`distro/skills/`、`.agents/skills/` |
| CI 检查项 | `justfile`（`check` 目标） |
| DESIGN.md 从 token 生成 | `DESIGN.md:3` |
| Macro 也用 ACP | `vendor/macro/crates/agent_runtime_protocol/src/lib.rs:4` |
