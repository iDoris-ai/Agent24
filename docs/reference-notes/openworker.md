# 研读笔记：OpenWorker — 桌面 AI 同事（吴恩达，2026-07-20 开源）

> 来源：`vendor/reference/openworker/`（andrewyng/openworker，MIT，本地只读克隆 @ `4766e59`）
> 日期：2026-07-24 | 用途：Agent24 M-F/M-G 设计输入（审批与无人值守）
> 引子：<https://blog.mushroom.cv/blog/andrew-ng-openworker-desktop-ai-coworker-local-aisuite/>
> 所有 `path:line` 相对 `vendor/reference/openworker/`。

## 0. 一句话定位 + 规模

- "AI that gets your everyday tasks done"——**交付成品而非建议**的桌面 AI 同事。本地跑、BYOK、可全离线（Ollama）。
  产品形态与 Agent24 高度重合：本地优先 + 审批门 + MCP + 定时自动化 + 渠道（Slack）+ 桌面壳 + TUI。
- 规模：Python 后端 **32.5K 行**（`coworker/`）、测试 **18.7K 行（81 个测试文件）**、GUI（React+Tauri）**26.3K 行 TS/TSX**、Rust STT 侧车 666 行。
  技术栈：Python + aisuite（吴恩达自己的统一 LLM 层）/ Tauri + React / Rust（仅语音转写）。
- **与 Agent24 的关系：产品同构、实现异构。** Agent24 是 Rust 内核 + 契约优先 + 本地小模型自建运行时；
  OpenWorker 是 Python 单体 + 产品体验优先。**因此值得借鉴的不是它的架构，而是它把「人机边界」这件事想得比我们细的那几处。**

---

## 1. ⭐ 风险分级：`RiskClass` 是声明属性，不是硬编码工具名集合（`risk.py:18`）

```python
class RiskClass(str, Enum):
    READ = "read"                # 无副作用 —— 永远放行
    WRITE_LOCAL = "write_local"  # 改工作区 —— 路径域 + 模式门
    EXEC = "exec"                # 执行命令 —— 模式门
    EXTERNAL = "external"        # 副作用离开这台机器 —— 无人值守 Inbox 的挂钩点
```

`classify(tool, metadata, overrides)`（`risk.py:39`）三级解析：**用户本地 override → 内置名表 → aisuite 元数据（`requires_approval` → EXTERNAL）→ 默认 READ**。
文件头明说这是**重构掉**了权限引擎里内联的 `WRITE_TOOLS`/`SHELL_TOOL` 名集合——「风险现在是一个声明属性，由单一 `classify` 读取」。

**对照 Agent24**：我们目前是 `ToolInfo.requires_approval: bool`（`agent24-tools/src/lib.rs`），二值。
TASKS G2 已经识别到要补「对外/不可撤回」维度——**OpenWorker 给出的正是那个维度的可落地形态**：
`EXTERNAL` 独立成类，并且只有它能享受「常驻规则」（见 §3），`EXEC` 永远问。
即：**分级的意义不在于分得细，在于不同级享有不同的豁免路径。**

## 2. ⭐ 用户本地风险 override：让「MCP 工具永远问」可解（`overrides.py:36`）

MCP 工具在 OpenWorker 里默认 `requires_approval=True`（`mcp/config.py:36`）→ 分类为 EXTERNAL。
Agent24 的 E1 做了同一个保守默认（`rust/crates/agent24-mcp/src/lib.rs:209`，注释「第三方代码，永远走人审」）。

**但保守默认不配 override 就等于不可用**——接一个 filesystem MCP server，`read_file` 每次都弹审批，用户三天内关掉这个功能。
OpenWorker 的解法是 `RiskOverrideStore`：glob 规则匹配工具名（`mcp__notion__create_page`），**最具体的规则赢**
（`_specificity`：字面字符数计分，无通配符额外 +1000）。用户在审批卡片上点一下就写入。

**不可违背的规则（原文加粗）**：这个 store 是 user-local，**永远不由 persona/包写入**。
包可以声明它想要什么工具，但只有用户决定信任到什么程度——所以 persona 加载路径从不碰这个文件。

> 这条对 Agent24 的 E3/E5（模块市场、PGL manifest）是硬约束：**模块清单不得携带自己的豁免。**
> 我们现在还没写这条，写市场之前必须先写。

## 3. ⭐⭐ 常驻定向审批（standing scoped approval）：绑到**确切目标**，而不是绑到工具

这是全仓库对 Agent24 最有价值的一处（`permissions.py:51,149-160` + `automation/models.py:26-58`）。

- 「总是允许」不是允许一个工具，而是允许一条 **`tool → 确切目标`** 规则：`send_slack_message → #ops-alerts`。
  存储形态就是一个字符串 `"tool target"`（一个空格分隔，工具名不含空格）。
- **资格判定 `standing_rule_candidate` 三重收窄**：① 只有 EXTERNAL 风险有资格（`EXEC`/`WRITE_LOCAL` 永远问——原文注释：*shell asks forever*）；
  ② 工具必须声明了 target 参数（`target_arg_for`，`connectors/tool_defs.py:1122`）；③ 调用必须真的填了这个 target。
- **规则挂在 automation（定时任务）记录上，不在全局**——所以撤销是 per-automation 的，删掉任务规则跟着走。
- 授予时 fail-closed（`grant_entries`，`models.py:35`）：只有 `access: "write"` 的条目才成为授权；
  **读权限只做「披露」——渲染在同意卡片上，从不存储**。其余一律丢弃。

**对照 Agent24**：我们的 `approve_for_session` 是 `(scope, tool)` 二元组（`agent24-policy/src/lib.rs:66`），
**一旦批准，同一 session 内该工具对任何参数都放行**。凌晨的定时任务批准了一次 `mcp__slack__post`，
之后它发到哪个频道都不再问。OpenWorker 的 `tool → exact target` 是同一个 UX 位置上更安全的语义。

## 4. ⭐⭐ Inbox：inline 与 inbox 只差一个 `visibility` 字段（`inbox.py`）

Agent24 的 G1（借鉴 MediaBot）写着「两种模式并存，由是否有人在线决定」——当时留了「怎么并存」的空白。
OpenWorker 给的答案比「两条路径」好：**只有一条路径，一个可等待、可持久化、可从任何界面应答的 parked 记录，
`visibility` 字段决定它出现在哪里**（`inbox.py:38`）：

| | `VIS_INLINE` | `VIS_INBOX` |
|---|---|---|
| 何时 | 有人在的会话 | 会话被设为 Unattended |
| 出现在 | 当前会话的输入框位置 | 跨会话的 Inbox 队列 |
| 底层 | **同一条 InboxItem，同一个 `await store.wait(id)`** | 同上 |

- 状态机就两态 `pending → resolved`，**恰好一次、幂等、first-responder-wins**——所以从 App / Slack / 恢复后的输入框
  任何一处应答都安全（`resolve()`，`inbox.py:295`）。
- 五种 kind：approval / question / notification / **directory（agent 请求授予一个文件夹）** / **plan（agent 提交计划待批）**。
  后两种是 Agent24 没有的形态：**「要权限」和「要方向」也走同一个队列**，而不是各自发明一套 UI。
- `question` 项带 `options[] + allow_text + multi`——注释直接点名对标 Claude Code 的 AskUserQuestion（结构化但永远可自由作答）。
- `unattended.py:17` 全文只有 43 行：**它只存一个 per-session 布尔**。文件头把边界写得很清楚——
  **无人值守不改变自主权上限（那是 permission mode 的事），它只改变「去哪里找人」**。开启需一次性确认（在 API/GUI 层强制）。
- 会话删除时 `resolve_session()` 把遗留 pending 全部关闭（`inbox.py:311`）——孤儿审批永远等不到有意义的答复。
- `reconcile_on_resume()`（`inbox.py:335`）：人回来时，把该会话仍 pending 的项转为 inline，**外加一份「你不在时被答复了什么」的回顾**。

## 5. ⭐ 持久化恢复（durable resume）：重启后**接着问**，而不是**全部作废**

`engine.py:250 resume()` + `:267 _unanswered_trailing_tool_calls()`：

> 重放最后一条 assistant 消息里**尚未有 tool result 的** tool_calls（即我们挂起在那儿的那一个及其之后的）；
> 提示回调会找到**已存在的 Inbox 条目**（按 `(session_id, tool_call_id)` 幂等，`inbox.py:131`）而直接返回，不重复提问；
> 已应答的调用被跳过，所以没有任何东西会被执行两次。然后继续跑模型循环把这一轮走完。

重建挂起态的信息**全部来自持久化的消息线程**——不需要额外的 checkpoint 格式。这是这个设计聪明的地方。

**对照 Agent24**：C4 的验收条目明写「daemon 被 kill 后重启，遗留 pending 审批全部标记 aborted」。
在人坐在电脑前时这是对的（安全、简单）。**F1a 之后就不对了**：launchd 两秒把 daemon 拉起来，
而凌晨那条等人批的任务已经死了——人早上醒来看到的是「任务失败」，不是「任务在等你」。
**这是 G1 的另一半，MediaBot 那条笔记没覆盖到。**

## 6. ⭐ Self-wake：让 agent 自己安排醒来（`selfwake.py`）

四个工具交给模型自己调（`selfwake.py:155`）：`sleep_for(seconds)` / `sleep_until(iso)` / `wake_on(job_id)` / `wake_on_event(event_key)`。
文件头一句话说清价值：**把「常驻 agent」变成「挂起/恢复」（事件驱动，闲置成本≈0）**——会话睡下去，
runtime 在 wake 到期时再唤起它。三类触发：定时器 / 后台作业完成 / 命名事件（连接器、webhook）。

`WakeStore` 只管 wake 记录和 due/complete 判定，**复用 automation 的 scheduler tick 来消费**（`scheduler.py:35 extra_tick`）——
没有为它新起一个循环。

**对 Agent24**：C5 的 schedule 是「用户配的定时任务」，M-F 是「24/7 常驻」。
两者之间缺的正是这个——**agent 说「这事我 20 分钟后再看一眼」而不必烧着上下文空转**。
Rust 侧成本很低：`agent24-scheduler` 已有 tick，加一张 wake 表 + 四个 Tool 即可。

## 7. 调度器：三条策略值得直接抄（`automation/scheduler.py`）

| 策略 | 实现 | Agent24 现状 |
|---|---|---|
| **run-once-catch-up** | 循环第一趟以 `trigger="catchup"` 跑一次，把宕机期间错过的到期任务补一次（只补一次，不补 N 次） | 未做（`MissedTickBehavior::Skip` 只保证不堆积，不补跑） |
| **skip-on-overlap** | `_running_ids` 集合，上一轮没跑完就跳过这一轮 | 有（pre-advance） |
| **spawn，不 await**（`:76-84`） | 一个 run 可能挂在 parked approval 上，**绝不能让它卡住调度循环 / 其他到期任务 / self-wake 恢复** | 目前同步审批，暂不成立——**做了异步审批后必然成立** |

第三条是 §4/§5 的直接推论：**异步审批与「调度器不得 await 单个 run」是配套的**，只搬前者会得到一个会僵死的调度器。
`stop()` 里还有一条契约：关停时把 spawned 的 run 一并 cancel——**挂起的 run 不得比调度器活得久**。

## 8. 工具并发：先串行授权，再并发执行（`engine.py:440-500`）

> 「一轮 assistant turn 的 tool calls：**先全部逐个授权**（串行——审批提示是交互式的），**然后执行**。
> 低风险调用（读、搜索）并发跑；其余按调用顺序一个一个来。」

Agent24 目前是完全串行（`agent24-agent/src/lib.rs:668` 的 `for (idx, call)`）。
这个「授权串行 / 读并发 / 写串行」的三分法是几乎零风险的延迟优化，且**因为授权阶段已经全部跑完，
并发执行阶段不会出现两个审批弹窗打架**——顺序不能反。

## 9. 其余可借鉴的小点

- **多根工作区**（`roots.py:18`）：`RootDir{path, writable, label}` 列表**按引用共享**给权限引擎 / 文件工具 / 上下文注入器三处，
  所以运行时增删文件夹三处同时生效，不必重建引擎。配合 `KIND_DIRECTORY` 的 inbox 项——**agent 可以主动申请一个目录**。
  Agent24 现在是固定路径白名单，M-F 之后（无人值守跑在 Mac mini 上）会需要这个。
- **审计脱敏名单**（`audit.py:13`）：`token/secret/password/api_key/access_token/bot_token/app_token/raw` + body/content/html 单列。
  我们的审计链已有 prev_hash，脱敏名单可以直接抄这份清单。
- **`parked.py`（未授权入站消息不丢弃而是暂存）**：网关默认关闭、只放行 allow-list 上的发送者，
  但被拦下的消息**存起来**，让机主在连接器页面一步解决（忽略 / 允许该发送者 / 允许并投递原消息）。
  原注释说明了动机：直接丢弃导致首次接触很别扭——对方得先发一条让自己出现在「最近发送者」里，被允许后再发一遍。
  **M-F 的微信/Nostr 入站会一字不差地撞上这个问题。**
- **`Mode` 五态**（`permissions.py:26`）：`discuss / plan / interactive / auto / custom`。
  `plan` 与 `discuss` 门禁相同（都只读），区别只在**意图**——plan 会推着 agent 走 `explore → propose_plan → 批准 → execute`。
  「同一门禁、不同引导」这个区分很干净。

## 10. ⚠️ 不该抄的部分

- **13.4K 行手写连接器**（`connectors/`，其中 `integration_tools.py` 一个文件 4892 行）——
  Slack/Gmail/GCal/GitHub/HubSpot 各自的 OAuth 账号体系、发送者归属、目标解析、目录同步。
  **注意这是在它已经支持 MCP 的前提下还手写的**：说明 MCP 给不了它要的产品体验（账号绑定、地址簿、归属标注）。
  这既**印证**了我们 E2 降级的判断（要能力就接 MCP），也**警告**了 M-F：
  渠道接入的成本大头从来不在协议，在账号与寻址。我们只要一个微信 + 一个 Nostr，不要走上这条 13K 行的路。
- **`server/manager.py` 3505 行的 God object**——和 openfang 的 kernel 是同一种病。
- **ProviderRouter 按 `provider:model` 前缀分发**（`providers/router.py:23`）：只是个前缀路由 + 懒加载缓存，
  不看成本/健康/隐私。Agent24 的 D2 `ModelRouter`（TaskProfile → tier，health+cooldown 反馈，隐私标签强制本地）**比它成熟，不要回退**。
- **JSON 文件当存储**（inbox / wakes / unattended / overrides 全是整文件重写的 JSON，只有 audit 用 SQLite）——
  我们 sqlx + SQLite 已经更对，直接建表即可。

---

## 11. 对照总表

| 维度 | OpenWorker | Agent24 现状 | 结论 |
|---|---|---|---|
| 风险模型 | 4 级 RiskClass + 分类函数 | `requires_approval: bool` | **借鉴**（G2 要的就是这个） |
| MCP 默认风险 | external（保守） | `requires_approval=true`（保守） | 一致 |
| 放宽保守默认 | 用户本地 glob override，包不得写 | 无 | **必须补**，否则 E1 实际不可用 |
| 「总是允许」粒度 | `tool → 确切目标`，仅 EXTERNAL | `(session, tool)` 全参数放行 | **收窄** |
| 有人/无人 | 同一条记录，`visibility` 二选一 | 仅同步阻塞 | **借鉴**（G1 的形态答案） |
| 重启后的待审批 | durable resume，接着问 | 全部标 aborted | **改**（F1a 之后语义错了） |
| agent 自主休眠 | sleep_for / wake_on / wake_on_event | 无 | **新增** |
| 错过的定时任务 | 启动补跑一次 | 不补 | 借鉴 |
| 调度器与挂起 run | spawn 不 await | N/A（同步审批） | 异步审批的配套前提 |
| 工具并发 | 授权串行→读并发→写串行 | 全串行 | 低风险优化 |
| 连接器 | 13.4K 行手写 | 走 MCP | **保持现状** |
| 模型路由 | 前缀分发 | TaskProfile + health/cooldown | **我们更好** |
| 存储 | JSON 整文件重写 | sqlx/SQLite | **我们更好** |
| 内核 | manager.py 3505 行 | 按 crate 拆分 | **我们更好** |

## 12. 关键文件锚点

| 主题 | 位置 |
|---|---|
| RiskClass / classify | `coworker/risk.py:18,39` |
| 权限引擎 evaluate | `coworker/permissions.py:109` |
| 常驻规则资格判定 | `coworker/permissions.py:51` |
| 用户本地风险 override | `coworker/overrides.py:36` |
| 授权 fail-closed 校验 | `coworker/automation/models.py:35` |
| Inbox 条目 / 状态机 | `coworker/inbox.py:62,295` |
| inline vs inbox 可见性 | `coworker/inbox.py:38` |
| 恢复时对账 | `coworker/inbox.py:335` |
| 无人值守开关 | `coworker/unattended.py:17` |
| durable resume | `coworker/engine.py:250,267` |
| 授权串行 + 读并发 | `coworker/engine.py:440` |
| self-wake | `coworker/selfwake.py:47,155` |
| 调度器三策略 | `coworker/automation/scheduler.py:63,76,91` |
| 多根工作区 | `coworker/roots.py:18` |
| 审计脱敏名单 | `coworker/audit.py:13` |
| 未授权入站暂存 | `coworker/connectors/parked.py` |
