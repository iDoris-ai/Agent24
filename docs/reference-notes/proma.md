# 研读笔记：Proma — 本地优先的桌面 Agent 工作台（proma-ai，AGPL-3.0）

> 来源：`vendor/Proma/`（github.com/proma-ai/Proma，**AGPL-3.0**，本地只读克隆 @ `91736031`，v0.1.1 / 2026-08-28）
> 日期：2026-08-28 | 用途：Agent24 桌面壳 / 渠道桥 / Skill 分发 / 开源-商业双生模型的对照输入
> 所有 `path:line` 相对 `vendor/Proma/`（涉及本仓库代码时给出仓库内完整路径）。
>
> 配套笔记：[`longhorizon-harness.md`](longhorizon-harness.md) · [`berd.md`](berd.md) · [`macro.md`](macro.md)

---

## 0. 一句话定位 + 规模

**「本地优先的 AI 桌面应用：多模型 Chat + 通用 Agent 工作流 + 工作区 + Skills + MCP + 远程机器人 + 记忆，装在一个开源客户端里。」**

它自己的分界说得很干脆：**要答案用 Chat，要把活干完用 Agent。**

| 维度 | 数字 |
|---|---|
| TypeScript 代码 | **~176,000 行** |
| 主进程服务模块（`apps/electron/src/main/lib/`） | **198 个文件** |
| 随应用分发的默认 Skills | **17 个** |
| 技术栈 | **Bun monorepo · Electron · React + Vite + Tailwind · Radix/shadcn · Jotai** |
| Agent 运行时 | **Pi Agent Runtime**（`@earendil-works/pi-coding-agent` + `pi-agent-core` + `pi-ai`） |
| 持久化 | **`~/.proma/` 下的 JSON / JSONL，刻意不引入本地数据库** |
| 许可证 | **AGPL-3.0** |

**与 Agent24 的关系：产品形态高度重合，工程取舍几乎处处相反。**

| | Proma | Agent24 |
|---|---|---|
| 内核 | 第三方 Pi Runtime（TS） | **自建 Rust 内核**（agent24d） |
| 持久化 | JSON/JSONL + 原子写，**明令不用数据库** | **SQLite + sqlx + 迁移 + 双时相** |
| 审批 | `canUseTool` 回调 + 会话白名单 | `risk_class` 四级 + Guardian + 定向常驻授权 + durable resume |
| 渠道 | 飞书 / 钉钉 / 微信，**统一 Bridge Registry** | 微信 / Nostr，**两个独立包，无共享抽象** |
| 商业模式 | **开源版 + 商业版 + 企业版，同一代码库** | MushroomDAO 开源 + HyperCapital 商业，**尚无工程机制** |

**所以它值得学的不是架构（我们的更硬），是它把产品面铺开之后暴露出来的那些运维细节 ——
尤其是我们 F5 泡测马上要撞上的那几条。**

---

## 1. ⭐⭐⭐ 最该立刻抄的一条：睡眠/唤醒后重建长连接

`apps/electron/src/main/lib/bridge-registry.ts:70`：

```ts
/** 启动 Bridge 自愈守护：系统恢复/解锁后重建长连接，定时恢复 error 状态。 */
export function startBridgeSelfHealing(options = {}): void {
  powerMonitor.on('resume', handlePowerResume)         // 系统从睡眠恢复
  powerMonitor.on('unlock-screen', handlePowerUnlock)  // 屏幕解锁
  healthCheckTimer = setInterval(
    () => void recoverAllBridges('定时健康检查', { force: false }),
    intervalMs ?? 60_000,
  )
  healthCheckTimer.unref?.()
}
```

配套还有 `POWER_RECOVERY_DELAYS_MS = [1_500, 10_000]` —— 唤醒后**分两次**尝试恢复
（网络栈通常不会在 `resume` 事件那一刻就绪）。

### 我查了我们这边：**Agent24 全仓零处理睡眠/唤醒**

```
grep -rn "powerMonitor|suspend|unlock-screen" apps/desktop/src packages/wechat-bridge/src packages/nostr-bridge/src
→ 只有一处无关注释（backend-manager.ts:235 里 "suspends supervision" 的英文措辞）
```

两个桥的现状（已逐个读过，**不是推测**）：

- **微信桥**有重连：`Monitor.loop()` 捕获异常后 `sleep(RECONNECT_DELAY_MS)` 再继续
  （`packages/wechat-bridge/src/ilink/monitor.ts:45`），且正确区分了「长轮询空闲超时」与真错误。
  **固定延迟，无退避** —— 对长轮询大体够用。
- **Nostr 桥**有 reconnect/retry 逻辑（`packages/nostr-bridge/src/{speaker,bridge,inbound}.ts`）。

> **所以缺的不是重连，是「机器睡了一晚上，醒来时那条连接其实已经是个僵尸」这件事。**
> TCP 长连接在 macOS 睡眠后经常处于「不报错但也永远收不到东西」的状态 ——
> 错误处理路径根本不会被触发，重连逻辑不会启动。
>
> **F5 泡测的定义是「Mac mini 连续 7 天，日程照跑，无人工干预」。**
> 一台 Mac mini 七天里必然经历多次休眠/唤醒。**这是最可能让泡测在第二天早上静默失效的一条，
> 而且它不会报错、不会崩溃、launchd 也不会重启它 —— 只是消息不再进来了。**

**建议：F5 开跑前先补这个。** 成本很小（桥进程加一个定时健康检查 + 主动探活重连即可，
不必依赖 Electron 的 `powerMonitor`，桥是独立 Node 进程）。**这条我列为最高优先。**

## 2. ⭐ Bridge Registry：新增渠道只改一处

同一个文件的 docstring 把动机写得很直白（`bridge-registry.ts:5`）：

> 「解决的问题：每新增一个 Bridge（飞书、钉钉、微信…），都需要在 `index.ts` 的
> `app.whenReady()` 和 `before-quit` 两个位置分别添加启动/清理代码。
> **遗漏任一处会导致 Bridge 不启动或进程无法正常退出。**」

注册契约只有五个字段：

```ts
interface BridgeRegistration {
  name: string
  shouldAutoStart: () => boolean      // 配置齐全且启用？
  needsRecovery?: () => boolean       // 通常只在 error 状态返回 true
  start: () => Promise<void>
  stop: () => void
  recover?: () => Promise<void>       // 缺省 = stop 后 start
}
```

外加 `startAllBridges()` 的一条纪律：**每个 Bridge 独立启动，单个失败不影响其他，
且是 fire-and-forget 不阻塞主流程。**

还有一个安全细节：所有 Bridge 日志走 `redactSensitiveLogValue()`
（`bridge-log-redaction.ts`）—— **凭据不会因为一条错误日志泄漏出去。**

> **对我们**：Agent24 的 `packages/wechat-bridge` 和 `packages/nostr-bridge` 是两个独立包、
> 两个独立进程、没有共享抽象。今天只有两条渠道，**收益还不明显**；
> 但 §1 的健康检查/自愈如果要做，**做在一个共享的 registry 里比在两个包里各写一遍强** ——
> 那正是引入这层抽象最自然的时机。

## 3. ⭐ Skill 分发：靠 frontmatter 版本号驱动升级

`AGENTS.md` 里有一条硬规则：

> 「修改 `apps/electron/default-skills/<skill>/` 的**任何**内容时，
> **必须**同步递增该 Skill `SKILL.md` frontmatter 的 `version`（patch +1），
> **否则老工作区不会收到升级**。」

实现侧（`agent-workspace-manager.ts:500`）：

> 「已存在（active 或 inactive）：**比较 `SKILL.md` 的 version，bundled 更新时才覆盖**」

即：bundled skill 种进用户工作区后就是用户的了，**只有版本号变高才覆盖**。
不比内容、不比时间戳 —— 比一个作者必须显式递增的数字。

17 个默认 Skill 里有几个形状值得注意：
`skill-creator` / `tool-builder` / `agent-collaboration` / `writing-plans` / `executing-plans` /
`knowledge-maintenance` / `find-skills` / `session-cleaner` —— **元能力（做 skill 的 skill、
写计划/执行计划分成两个）占了一半。**

> **对我们**：`berd.md` §4 记的 `distro/` 分发缝解决的是「私有物怎么叠加」，
> **Proma 这条解决的是「已分发的东西怎么升级」** —— 两半合起来才是完整的分发机制。
> Cos72 要给社区分发 Skill，这两条都得有。
> 而「版本号必须手动递增，否则升级不发生」是一条**便宜且不会出错**的契约。

## 4. ⭐ 指令文件解析：禁止向上游走目录

`AGENTS.md`：

> 「用户项目的 `AGENTS.md` 由 `project-instruction-resolver.ts` **在已授权项目根内显式解析**；
> **禁止恢复 cwd、祖先目录或附加目录的环境式规则发现。**」

实现侧（`project-instruction-resolver.ts:1`）有对应的硬约束：

```ts
const MAX_SOURCE_BYTES = 64 * 1024
const MAX_TOTAL_BYTES  = 128 * 1024

interface ProjectInstructionSource {
  path: string            // canonical path，作为可审计的来源标识
  relativePath: string
  scopeRoot: string       // 这份来源作用于哪个子树
  kind: 'agents' | 'claude'
  content: string
  contentHash: string     // ← 内容哈希
}
```

四件事同时到位：**授权根内解析** · **规范化路径 + `realpathSync`（防 symlink 逃逸）** ·
**大小上限** · **内容哈希（可审计"注入了什么"）**。

**为什么这是安全边界而不是洁癖**：向上游走目录找指令文件 = 任何人只要能在你的祖先目录
放一个 `AGENTS.md`，就能往你的 agent 上下文里注入指令。这是一条真实的提示词注入路径。

还有一条相关的：**「旧项目 `CLAUDE.md` 仅是兼容输入，不能自动覆盖、合并或删除用户文件。」**

> **对我们**：Agent24 的 MD-7 知识层做「层级 markdown 合并 + 触发注入」，
> 且已经有「auto-memory 提案永不自动应用」这条门。
> **但「层级合并」的边界在哪里 —— 到哪一级停、符号链接怎么办、有没有大小上限 ——
> 我没有查证。** 这是一条值得核实的检查项，不是已知缺陷。

## 5. 本地优先的取舍：JSON/JSONL + 原子写，明令不用数据库

`~/.proma/` 的布局（`config-paths.ts`）：

```
~/.proma/
  channels.json           模型渠道
  conversations.json      会话索引
  conversations/<id>.jsonl  每个会话一个 JSONL
  attachments/<会话 id>/
  settings.json · user-profile.json · system-prompts.json
  chat-tools.json · proxy-settings.json · default-apps.json
```

配套的 `safe-file.ts`（97 行，`AGENTS.md` 规定**禁止直接 `writeFileSync`**）：

> 「解决系统强制关机/崩溃时 JSON 索引文件被截断导致数据丢失的问题。
> **写入**：write-to-temp → rename（POSIX 原子操作）+ `.bak` 备份。
> **读取**：主文件 → `.tmp` 残留 → `.bak` 回退，多层容错。」

> **这条我们不学，但值得记下对照。** Agent24 选了 SQLite + sqlx + 迁移 + `BEGIN IMMEDIATE`，
> 这是更强的选择 —— 双时相断言、事务性 re-key、幂等重放，JSON 文件做不了。
> **但 Proma 暴露了一个我们也有的问题**：`packages/wechat-bridge` 的
> `wechat-sessions.json` 用的就是**裸 `writeFileSync`**（`main.ts:36`），没有原子写。
> 那份文件断电时会被截断，且它是会话映射 —— 丢了等于所有微信用户的会话上下文断掉。
> **这是一条真实的、便宜的修补：五行代码。**（已核实：`packages/wechat-bridge/src/main.ts:33-38`。）

## 6. 值得一提的产品面（不一定要做，但要知道存在）

| 能力 | Proma 的做法 | 我们的位置 |
|---|---|---|
| **内置受管浏览器** | 10 个 `browser-*.ts` 策略模块（url 校验 / key / 观察 / 脚本 / profile / 风险声明），agent 能开页面、点按、填表、开 `localhost` 预览 | 完全没有。属 `macro.md` 归的 M6 |
| **Agent Island** | 7 个模块的运行态浮层（计划配额 / 优先级 / 可见性） | 托盘只显示 daemon 状态（F1b） |
| **Automation** | `automation-manager` + `automation-scheduler` + 通知 | C5 调度器已有，形态相当 |
| **协作子 agent** | `agent-collaboration-tools.ts`，调用与结果显示在消息流里 | H9 explorer（只读、禁递归），比它保守 |
| **工作区记忆** | `workspace-memory-change-watcher` + `agent-memory-refresh-service`，**记忆变更在 UI 里提示刷新** | 记忆底座强得多，但**没有任何 UI 呈现** |
| **流式语音输入** | 豆包 ASR，`Ctrl+\`` 全局唤起，可写进 Proma 外的任意光标位置 | D4b 的 Whisper 一直延后（无消费者） |

> 「**记忆变更在 UI 里提示刷新**」这一条值得单独想：
> Agent24 的记忆底座有事件、断言、巩固、知识层，**但用户看不见它记了什么、也无从纠正**。
> M1 F1.3 把会话写进 EventLog 之后，「记忆的可见性」会立刻变成一个真问题。
> 不必现在做，但应该记进 M4/M5 的考虑里。

## 7. ⭐ 开源 / 商业 / 企业三档：同一个代码库

`README.en.md` 里有一张八行的对照表，把两版差异写得很具体。核心分界：

| 维度 | 开源版 | 商业版 |
|---|---|---|
| 桌面体验 | **完整**，自由配置 | 相同 |
| 模型渠道 | 自己加 provider 和 API Key | Proma Cloud 托管渠道（部分低至官方 20%） |
| Agent 安全与可靠性 | **自行评估各家 provider 与第三方中转的信任与数据处理** | 托管官方链路，统一保障 + 协议兼容 + 健康监控 |
| 联网/内置能力 | 自配 search、图像生成的 key | WebSearch + GPT Image 2 内置 |
| 团队额度 | 自建流程 | 管理员分配/回收共享额度、按月自动分配 |
| **Skills 分发** | **工作区本地，跨团队分享要自己组织** | **企业版：管理员一键推送团队 Skills 给成员，版本/更新/使用范围集中管理** |

**这张表的价值不在具体条目，在它划线的方式：开源版拿到的是完整的产品，商业版卖的是
「托管、额度、分发、合规」这些天然属于组织的东西。**
没有阉割功能，也没有把核心能力藏在商业版里。

> **对我们 —— 这直接对上 `MISSION.md` 的开源+商业双生模型。**
> MushroomDAO 做数字公共物品、HyperCapital 是唯一授权商业伙伴，
> 但**「哪些能力属于开源侧、哪些属于商业侧」今天没有任何工程边界，也没有一张这样的表。**
>
> Proma 划的那条线（**产品完整开源，商业化托管与组织级分发**）与 Mycelium 的价值观是相容的 ——
> 它没有靠提取数据或阉割功能赚钱。**建议在 Cos72 商业化之前，先照着这个形状写出我们自己的那张表**，
> 再配上 `berd.md` §4 的 `distro/` 分发缝作为工程实现。**先划线，再写代码。**

## 8. 工程纪律里值得抄的两条

**① IPC 是四层契约**（`AGENTS.md`）：新增/修改 IPC 必须同步检查
`packages/shared` 的通道常量与类型 → `main/ipc.ts` 的 handler → `preload/index.ts` 的 bridge →
renderer 的调用与错误处理。**漏一层就是运行期才炸。**

> Agent24 的桌面壳 ↔ daemon 也是多层（api-client 生成 → IPC → renderer），
> 但我们有一样 Proma 没有的东西：**CI 零漂移门**（A6）。我们的协议侧比它硬。
> 值得对照的是**IPC 那一段**有没有同等强度的检查 —— 我没查。

**② 单一 Agent runtime，硬边界**（`AGENTS.md`）：

> 「Proma 仅使用 Pi Agent runtime。**不要重新引入 Claude Agent SDK 或其专属配置、session 语义和打包依赖。**」

而且旧 Claude runtime 的历史会话**降级为只读记录**：可查看，不可继续/分叉/回退。

> **这与 Agent24 的「`CanonicalSession` 与 `Condenser` 不允许两套并存」是同一条判断
> （`docs/agent/architecture.md` 核心判断 3）。** 两个仓库独立收敛到同一条：
> **运行时/存储的"两套并存"是最贵的债，宁可把老的降级为只读投影。**

---

# 第二部分：拿它干什么

## 9. ⚠️ 许可证：AGPL-3.0，和 Macro 同一条禁令

| 仓库 | 许可证 | 能不能借代码 |
|---|---|---|
| LongHorizon-Harness | MIT | ✅ 可以 |
| Berd | Apache-2.0 | ✅ 可以 |
| **Proma** | **AGPL-3.0** | ❌ **不可以** |
| Macro | AGPL-3.0 | ❌ 不可以 |

规则与 `macro.md` §10 逐字相同：**可以学架构与思想，不得复制源码，实现阶段不打开源文件当模板。**
`vendor/Proma/` 已加进 `.gitignore`。

> §1 的睡眠/唤醒自愈、§2 的 Bridge Registry、§5 的原子写 —— **这些都是十几行的通用模式，
> 我们自己写出来毫无难度**，不需要也不应该照抄它的实现。

## 10. 落地清单

### 10.1 F5 泡测开跑前（**最高优先**）

| # | 做什么 | 为什么现在 |
|---|---|---|
| **A** | **桥进程加睡眠/唤醒自愈**（§1）：定时健康检查 + 主动探活，而不是等错误触发重连 | **这是最可能让 7 天泡测在第二天静默失效的一条**，且不报错、不崩溃、launchd 不会救 |
| **B** | **`wechat-sessions.json` 改原子写**（§5）：write-to-temp → rename | 已核实是裸 `writeFileSync`（`packages/wechat-bridge/src/main.ts:36`）。断电截断 = 所有微信会话映射丢失。**五行代码** |

这两条加起来不到一天，**却直接决定 F5 能不能真的跑满 7 天。**

### 10.2 需要先核实（不是已知缺陷，别当结论说）

| # | 查什么 |
|---|---|
| **C** | MD-7 知识层的层级 markdown 合并：向上走几级？符号链接怎么处理？有没有大小上限？（对照 §4） |
| **D** | 桌面壳 IPC 那一段有没有与协议侧同等强度的漂移检查（对照 §8①） |
| **E** | 微信 `bot_token` 过期后有没有重新登录路径 —— `login()` 只在启动时调用一次（`main.ts:57`），过期后 Monitor 会永远错误重试。**我没读完 `ILinkClient` 内部，不确定** |

### 10.3 需要立项 / 决策

| # | 事 | 放哪 |
|---|---|---|
| **F** | **Bridge Registry 抽象**（§2） | 做 §10.1-A 时顺路，比在两个包里各写一遍强 |
| **G** | **Skill 版本驱动升级**（§3）+ `berd.md` 的 `distro/` 分发缝 | Cos72 Skill 分发的完整机制 = 这两半。M5 |
| **H** | **开源/商业能力边界表**（§7） | **Cos72 商业化之前，先划线再写代码。** 这是产品决策，需要你拍板 |
| **I** | **记忆的可见性**（§6） | M1 F1.3 之后会立刻变成真问题：用户看不见记了什么、也无从纠正。M4/M5 考虑 |

## 11. 明确不借鉴

1. **JSON/JSONL 当主存储。** 我们的 SQLite + 迁移 + 双时相是更强的选择，不回退（§5）。
2. **198 个文件平铺在一个 `lib/` 目录。** 没有子目录、没有分层，靠文件名前缀（`agent-` / `browser-` /
   `bridge-` / `planning-`）做分组。到这个规模应该分包了。
3. **「每次改动都必须递增版本号」**（`AGENTS.md`）。对一个日更的桌面应用说得通；
   对我们这种一个 PR 一个 task 的节奏是噪音。
4. **绑定单一第三方 Agent runtime。** 对它是对的（不自建内核就该只选一个），
   对我们是反的 —— 自建 Rust 内核正是 Agent24 的立身之本（ADR-026）。

## 12. 一句话总结

> **Proma 是一个把产品面铺得很开的本地优先 Agent 桌面应用，它的架构取舍几乎处处与我们相反 ——
> 但正因为它跑在真实用户的机器上，它踩过的那些运维坑，是我们 F5 泡测即将踩的同一批。**
>
> **最值钱的是第一条，而且它便宜得离谱**：机器睡了一晚，长连接变成僵尸 ——
> 不报错、不崩溃、launchd 不会重启它，只是消息不再进来。
> **Agent24 全仓零处理睡眠/唤醒（已验证）。F5 开跑前补上这条，比补任何别的都值。**
>
> 其次是 §7 那张开源/商业对照表 —— 它证明了「产品完整开源、商业化托管与组织级分发」
> 是一条走得通的线，而这正是 `MISSION.md` 写了价值观但还没有工程边界的地方。

---

## 附：核对清单（本笔记的事实来源）

| 断言 | 出处 |
|---|---|
| AGPL-3.0 · v0.1.1 · 17.6 万行 TS · 198 个服务模块 | `LICENSE`、`package.json`、`wc -l` @ `91736031` |
| Pi Agent Runtime（三个 `@earendil-works/*` 包） | `apps/electron/package.json:18,51`、`README.en.md` |
| 睡眠/唤醒自愈 + 两段恢复延迟 | `apps/electron/src/main/lib/bridge-registry.ts:70`、`POWER_RECOVERY_DELAYS_MS` |
| Bridge 注册契约 + 独立启动 + 日志脱敏 | `bridge-registry.ts:5,49-68`、`bridge-log-redaction.ts` |
| Agent24 零处理睡眠/唤醒 | `grep -rn powerMonitor\|unlock-screen apps/desktop/src packages/*-bridge/src` → 无匹配 |
| 微信桥的重连（固定延迟，区分空闲超时） | `packages/wechat-bridge/src/ilink/monitor.ts:45-52` |
| 微信会话映射裸 `writeFileSync` | `packages/wechat-bridge/src/main.ts:33-38` |
| Skill 版本驱动升级 | `AGENTS.md`「默认 Skills」· `agent-workspace-manager.ts:500` |
| 17 个默认 Skill | `apps/electron/default-skills/` |
| 禁止祖先目录规则发现 + 哈希 + 大小上限 | `AGENTS.md`「Agent 与项目指令」· `project-instruction-resolver.ts:1-40` |
| `~/.proma/` 布局 | `apps/electron/src/main/lib/config-paths.ts:49-211` |
| 原子写 + `.bak` 多层回退 | `apps/electron/src/main/lib/safe-file.ts:1-40` |
| 浏览器策略十件套 | `apps/electron/src/main/lib/browser-*.ts` |
| 开源 / 商业 / 企业对照表 | `README.en.md`「Download」一节 |
| IPC 四层契约 · 单一 runtime 硬边界 | `AGENTS.md`「IPC 是四层契约」「Agent 与项目指令」 |
