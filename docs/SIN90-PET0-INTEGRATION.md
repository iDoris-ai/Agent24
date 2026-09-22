# Sin90 ↔ Pet0 集成约定（权威版）

> 文档类型：跨仓库集成契约（Integration Contract）
> 权威源：本文件（`iDoris-ai/Agent24:docs/SIN90-PET0-INTEGRATION.md`）
> 镜像：`tools/Pet0:docs/AGENT24_INTEGRATION.md`（内容同源，仅视角前置不同）
> 状态：v0.3 · T11（Sin90 迁出内核）已交付后的重写版，据此各自开发接口模块
> **对齐**：`iDoris-ai/Sin90` 的 `docs/DESIGN-LIFEOS.md`（业务语义权威）· `protocol/events.schema.json`（事件信封权威）
> 最后更新：2026-09-22
>
> **本文件不复述状态机与事件字段**，只引用权威源。上一轮复审发现的漂移全部集中在
> 「机制描述段」——凡是复述过来的，都比来源先过期；而两边各改一次不会有任何东西报错。
> 所以状态机看 `iDoris-ai/Sin90` 的 `docs/DESIGN-LIFEOS.md`，事件信封看
> `protocol/events.schema.json`，本文件只说**谁跟谁怎么接**。
>
> ⚠️ **v0.2→v0.3 的架构性变化（读之前先知道这一点）**：v0.2 写的是"Sin90 是
> Agent24 内核内置的领域模块"（纯域 crate `agent24-sin90` + 自带 store
> `agent24-sin90-store`，编译进 `agent24d` 二进制）。**这个形状已经不存在。**
> T11（[#342](https://github.com/iDoris-ai/Agent24/pull/342)，2026-09-22 合并）把这三个
> crate 从内核彻底删除。Sin90 现在是**独立仓库 `iDoris-ai/Sin90` 的进程外模块**
> （`out_of_process_provider`）——自己的二进制、自己的 `sin90.db`、自己的 HTTP 业务层，
> 通过 Agent24 的模块协议（manifest 声明 + 握手 + 回调 socket + 受约束代理）挂载进
> `agent24d`。**对 Pet0 而言 API 路径面基本不变**（仍是本地 `agent24d` 代理出的
> `/api/v1/sin90/*`），但"Sin90 代码住在哪个仓库""谁来维护业务逻辑""鉴权 header
> 用哪个"这几件事全变了，见下文。

---

## 0. 一句话

**Pet0 是建在 Agent24 之上的第一个垂直产品(桌面伴侣 + Personal OS)；Sin90（独立仓库
`iDoris-ai/Sin90`，进程外模块）提供基础领域模型与业务逻辑，Agent24 内核只提供挂载/代理/
事件转发/审批这些通用底层机制；Pet0 只做壳、感官与产品语义，通过 `agent24d` 的本地
HTTP/WS 消费 Sin90，绝不重写地基，也不直接连 Sin90 进程。**

---

## 1. 三方而非两方(架构已经从"两个仓库"变成"三个仓库")

v0.2 时是 Agent24（内核 + 内置 Sin90）↔ Pet0 两方。现在是三方，各自一个仓库：

| 仓库 | 角色 |
|---|---|
| `iDoris-ai/Agent24` | 内核：进程外模块宿主机制（Supervisor、握手协议、受约束代理、事件转发、审批回调）——ME-3 专项已于 2026-09-20 整体收口，T1–T9 全部 `DONE` |
| `iDoris-ai/Sin90` | Sin90 业务本身：Direction/Area/Task/Week/ScheduleBlock/Proposal 的实体、状态机、HTTP 路由、自己的 `sin90.db` |
| `tools/Pet0` | 桌面壳：只经 `agent24d` 本地 HTTP/WS 消费 Sin90，不直连 Sin90 进程、不直连 `sin90.db` |

Pet0 写进文档的 6 条硬约束，Agent24 内核用代码实现每一条，**但"落库其实是谁的代码"这件事，已经从「内核里的一个 crate」变成「Sin90 自己仓库里的一个独立二进制」**：

| Pet0 原则 | 现状 |
|---|---|
| Database is source of truth, AI is not | `sin90.db` 完全属于 `iDoris-ai/Sin90` 进程,与 Agent24 内核的 `agent24-store` 物理隔离(独立 SQLite 文件,默认 `~/.agent24/os/sin90/sin90.db`) |
| AI 输出只是 Proposal,落库必过确定性校验 | 校验代码在 Sin90 自己的 `src/store`,事务全在 `sin90.db` 一个库里完成,不跨库 |
| Core 不依赖具体模型,只依赖 IntelligenceProvider | 不变,属 Agent24 内核范畴(`agent24-models`) |
| 每次状态变更都产生事件 | Sin90 通过握手拿到的回调通道把事件转发给内核(`_a24/events/emit`),内核只转发不解释payload |
| Router 每次路由决策都记账 | 属 Agent24 内核范畴,不变 |
| 换掉 Codex 不改 Core 一行 | 属 Agent24 内核范畴,不变 |

---

## 2. 边界：壳与核之间是本地进程边界，核与 Sin90 之间还有一道进程边界

**关键事实：`agent24d`(Rust daemon)在 `127.0.0.1` 上跑 HTTP + WebSocket(bearer token
握手、动态端口)。壳通过本地 HTTP/WS 与它对话——这仍是 Pet0 要的「表现层只发 Event、
只订阅 State」。但现在 `agent24d` 自己也不直接实现 Sin90 的业务——它把 `/api/v1/sin90/*`
下的请求原样代理转发给一个独立运行的 Sin90 子进程。**

```
┌─────────────────────────────┐
│  Pet0 Shell (Tauri 或复用 Electron) │  表现层 + Reflex(FSM) + 感官
│  透明窗 / 穿透 / 动画 / 气泡 / 语音     │
└───────────────┬─────────────┘
        本地 HTTP + WS (agent24d)      ← 唯一集成面,壳只认这一条
┌───────────────▼─────────────┐
│  agent24d  (Rust daemon,iDoris-ai/Agent24)  │
│  ┌─────────────────────────┐  │
│  │ store · core · policy ·    │  │  事务状态机 / 事件日志(内核自己的)
│  │ scheduler · memory · models │  │  cron / KV+session / 可配置 provider 注册表
│  │ · mcp · 进程外模块宿主       │  │  Supervisor / 握手 / 受约束代理 / 事件转发
│  └────────────┬────────────┘  │
└───────────────┼───────────────┘
     受约束代理 + 回调 socket(进程边界,ME-3 协议)
┌───────────────▼─────────────┐
│  sin90 (独立二进制,iDoris-ai/Sin90) │  Area/Direction/Task/Week/ScheduleBlock
│  自己的 HTTP 路由(handler 不经内核) │  状态机 / Proposal 门 / 事件产出
│  sin90.db (SQLite,唯一事实来源)     │  与 agent24-store 物理隔离
└─────────────────────────────┘
```

壳技术选型只看**分发体积 vs 复用现有壳代码**，与本节改动无关，结论不变（见 v0.2 §2）。

---

## 3. Sin90 —— 独立仓库的进程外领域模型

Sin90 现在是 **`iDoris-ai/Sin90` 仓库里的一个独立 Rust 二进制**，以 Agent24 的
`out_of_process_provider` 形态被安装/挂载（manifest 见 Sin90 仓库根目录的
`domain-os.yml`：`name: sin90`、`route_namespace: /api/v1/sin90`、
`kernel_capabilities: [events]`——**只要 events,不用内核记忆**，自己管数据）。
依赖方向不变：Sin90→内核单向，内核绝不反向依赖或知道 Sin90 的业务语义。

### 3.1 实体与状态机

命名与约定沿用原设计（每个实体一对 `<entity>_transition_allowed`/`check_<entity>_transition`，
落库前强制校验），但**代码位置变了**：

| 实体 | 说明 |
|---|---|
| **Area** 生活领域 | M0 新增，五大生活系统的种子分组（`POST /packs/install`） |
| **Direction** 长期方向 | 月/季度级方向，可挂在 Area 下 |
| **Task** | 支持 `capture`（原始收件箱条目）与常规任务两种入口 |
| **Week** 周容器 | M2 新增，`planning → active → reviewing → closed` |
| **ScheduleBlock** 时间块 | 计划的执行块，实际由事件对账 |
| **AttentionBudget** 注意力预算 | 物化视图，按 Direction 的 planned vs actual，纯事件回放算出 |

> **迁移矩阵与完整设计权威源已经搬家**：以前是 Agent24 的
> `agent24-sin90/src/transitions.rs`（**已删除，见 T11 #342**），现在是
> `iDoris-ai/Sin90` 仓库的 `src/core/`，配套设计文档
> `docs/DESIGN-LIFEOS.md`。本文件不复述,复述必然过期。
> **Rhythm 与 Review 两个实体、`/rhythms`/`/reviews` 路由**——v0.2 曾作为「目标接口」
> 列出,截至本次核实（2026-09-22，`iDoris-ai/Sin90` commit `9debb89`）**仍未实现**,
> 不在 `src/http/mod.rs` 的路由表里,按 M3+ 的设计顺序排期,不要假设它们存在。

### 3.2 事件与对账(Sin90 的灵魂)

- Sin90 自己的 `sin90_events` append-only 表(在 `sin90.db` 里，不在内核的
  `agent24-store`)。任何 Sin90 状态变更**必须**产生事件；无事件的状态变更视为 bug。
- 事件产出后，Sin90 经握手拿到的回调连接调用 `_a24/events/emit` 转发给内核，
  内核再经通用的 `module` 事件信封（见 §5 下方）推给订阅 WS 的客户端。
  **内核只转发，不解释、不校验 payload 语义。**
- `GET /api/v1/sin90/attention` 与 `GET /api/v1/sin90/weeks/{id}/attention` 必须能
  纯从事件回放算出用量，不依赖任何对话上下文——这是 SPIKE-00 定的判定，M0/M1/M2
  已用真实端到端测试（见 §8）验证过。

### 3.3 Proposal 门(AI 不写库) + Actor-Key 直写门禁

AI(本地脑或 Codex)产出的一切都是 `Sin90Proposal`，经确定性校验后转事务落库并产事件——
这条不变。**新增的一层**（T11 迁出后才补上，`iDoris-ai/Sin90` 自己实现，不是内核）：

> **Actor-Key 门禁（design §7.1，`src/http/actor.rs`）**：Sin90 自己区分"人类直写"与
> "自动化只能走 Proposal"两种调用方——除 `GET`/`POST /proposals`/`POST
> /proposals/{id}/accept`/`POST /capture` 外，其余直写路由(`POST /areas`、
> `PATCH /tasks/{id}` 等)一律要求携带**人类 key**。
>
> **⚠️ Pet0 集成时最容易踩的坑**：鉴权 header 是 **`x-sin90-actor-key`**，
> **不是** `Authorization`。原因：Agent24 的受约束代理会剥掉每个转发请求的
> `Authorization`/`Cookie`/`X-A24-*` header（内核安全策略，与模块无关），一个读
> `Authorization` 的鉴权实现在真实代理后面**从第一天起就完全失效**、只在绕过代理
> 直调 `router()` 的测试里才work。这是 2026-09-20 一次真实端到端挂载验证抓到的
> 生产级 bug（`ab66b37`，已修复），Pet0 客户端从一开始就要用对的 header 名，
> 不要照抄内核其它 API 的 `Authorization: Bearer <token>` 惯例。
> `SIN90_HUMAN_KEY` / `SIN90_AUTOMATION_KEY` 两个环境变量对应两把 key（未设置会在
> 启动日志打印一次性生成的随机值，生产部署务必显式设置并妥善保管）。

```
AI 输出 → Sin90Proposal → 确定性校验(schema + 状态机) → 事务写入 + 产事件
                              │失败
                              └→ 拒绝 / 降级到 Rule / 问用户

人类直写 → x-sin90-actor-key: <human key> → 校验 + 状态机 → 事务写入 + 产事件
```

### 3.4 Three-Brain 路由归属

不变，见 v0.2（Reflex 在 Pet0 壳、Local/Executive 在 Agent24 的 `agent24-models`）。

---

## 4. 职责划分(据此各自开发)

### 4.1 Agent24 内核做(我们，`iDoris-ai/Agent24`)

1. **进程外模块宿主机制**（`agent24-os-proto` + `agent24d` 的 Supervisor）：握手、
   回调 socket、受约束代理（透明转发原始路径）、事件转发、审批往返、热 disable。
   **ME-3 专项已于 2026-09-20 整体收口，T1–T9 全部 `DONE`**——这部分对 Sin90 和未来
   任何领域 OS 都是现成的，不用再开发。
2. **可配置模型网关**：`agent24-models` provider 注册表——Executive(Codex/OpenAI 兼容)+
   Local(GGUF/MLX 经 oMLX)+ 三级路由 policy。
3. **调度**：`agent24-scheduler`(现成)——Sin90 若要接 Rhythm/Nudge 触发，调用方式
   见 T13（`agent24-os-sdk`，未开工）落地后的封装。
4. **MCP 适配**：`agent24-mcp`(现成)，供后续集成走同一 dispatch + 审批门。
5. **API 面**：`agent24d` 挂载 Sin90 的路由（透明代理，不解释路径）、WS 走通用
   `module` 信封；`@agent24/api-client` 出 typed client。
6. **不做**：Sin90 的实体/状态机/HTTP handler/`sin90.db`——**这些已经不是 Agent24
   内核的职责范围**，全部在 `iDoris-ai/Sin90` 自己的仓库里。

### 4.2 Sin90 做(独立仓库，`iDoris-ai/Sin90`)

1. **业务实体与状态机**：Area/Direction/Task/Week/ScheduleBlock/Proposal，见其
   `docs/DESIGN-LIFEOS.md`。
2. **自己的存储**：`sin90.db`，自己管迁移，不写 `agent24-store`。
3. **自己的 HTTP 层**：`src/http/mod.rs` 的路由表（§5 权威源）。
4. **Actor-Key 门禁**：自己实现、自己维护密钥（§3.3），不依赖内核的 auth 概念。
5. **握手客户端**：`src/adapter_agent24`，读 `A24_*` 环境变量完成 `initialize` 握手、
   拿回调连接转发事件。**当前没有官方 Rust SDK 可复用**（T13 `agent24-os-sdk` 尚未
   交付），这部分是 Sin90 自己手写的约 200 行——T13 落地后可以考虑迁移，但不阻塞。

### 4.3 Pet0 做(Pet0)

1. **桌面壳**：Tauri(或复用 Electron)——透明窗、点击穿透、多显示器、托盘。
2. **桌宠表现 + Reflex**：精灵图/动画/FSM/气泡、`.petpack` 格式(Reflex 脑在此)。
3. **感官/语音链路**：VAD/KWS/STT/TTS(sherpa-onnx)，在边缘壳侧。
4. **Nudge 呈现**：触发是 Sin90 的 Rule（一旦落地），**措辞与渲染在壳**。
5. **Onboarding / first-run**：引导用户经 Sin90 API（`POST /packs/install` 或
   `POST /directions`）建立第一个 Area/Direction。
6. **只经 `agent24d` 消费 Sin90**：本地 HTTP/WS，**绝不直连 `sin90.db`、绝不直连
   Sin90 进程端口、绝不重写状态机/事件/proposal**。
7. **鉴权**：直写请求带上正确的 `x-sin90-actor-key`（§3.3），Proposal 路由与
   `capture`/只读路由不需要或接受自动化 key。
8. **产品语义输入**：Sin90 实体字段与状态机由 Pet0 作为领域专家与 Sin90 团队共定，
   **实现权归 `iDoris-ai/Sin90`**（不再是 Agent24）。

### 4.4 共同边界(不可协商)

- 壳↔核 = `agent24d` 本地 HTTP + WS；核↔Sin90 = 受约束代理 + 回调 socket——**两条都
  是进程边界**，任何一方都不得越过中间人直连另一端。
- 壳**发命令(经校验、可被拒)，只订阅事件**——从不直接写既成事实。
- **离线优先是硬指标**：除 Executive 脑外，全部断网可用（Sin90 进程本身天然离线，
  不依赖网络）。
- AI 不写库，一切经 Proposal；直写只对人类 key 开放。
- 每次状态变更产事件；每次路由决策记账。
- schema 变更必须有迁移，用户 DB 不靠重建（Sin90 自己的迁移体系，见其
  `src/store/migrations`）。

---

## 5. Sin90 API 面

沿用现有 `/api/v1/*` + WS 约定。全部 `/api/v1/sin90/` 前缀。

> ⚠️ **权威源已搬家**：不再是 Agent24 已删除的 `agent24-sin90-os` crate，而是
> `iDoris-ai/Sin90` 仓库的 `src/http/mod.rs` 路由表（`router()` 函数）与其
> `domain-os.yml`。本节按 2026-09-22 核实的 `iDoris-ai/Sin90` commit `9debb89`
> 抄一份**当前真实存在**的路由，供快速查阅；**冲突时以 Sin90 仓库代码为准**。

### 5.1 当前已实现

```
POST             /api/v1/sin90/areas                   # 人类 key
GET              /api/v1/sin90/areas
PATCH            /api/v1/sin90/areas/{id}               # 人类 key
POST|GET         /api/v1/sin90/directions               # POST 需人类 key
POST             /api/v1/sin90/tasks                    # 人类 key
GET              /api/v1/sin90/tasks
PATCH            /api/v1/sin90/tasks/{id}                # 人类 key
POST             /api/v1/sin90/capture                  # 人类或自动化 key（低风险直写）
GET              /api/v1/sin90/today                    # 无需鉴权（只读）
POST|GET         /api/v1/sin90/schedule-blocks          # POST 需人类 key
PATCH            /api/v1/sin90/schedule-blocks/{id}      # 人类 key
POST|GET         /api/v1/sin90/weeks                    # POST 需人类 key
PATCH            /api/v1/sin90/weeks/{id}                # 人类 key
GET              /api/v1/sin90/weeks/{id}/attention      # 无需鉴权（只读）
POST|GET         /api/v1/sin90/proposals                # 人类或自动化 key
GET              /api/v1/sin90/proposals/{id}
POST             /api/v1/sin90/proposals/{id}/accept     # 人类或自动化 key
GET              /api/v1/sin90/attention?start&end       # 无需鉴权（只读）
GET              /api/v1/sin90/events                    # 无需鉴权（只读）
POST             /api/v1/sin90/packs/install             # 人类 key，种子五大生活系统

# 事件流(复用现有 WS,内核提供)
GET              /api/v1/events
```

### 5.2 目标接口(尚未实现，按 M3+ 排期)

```
GET|PATCH        /api/v1/sin90/directions/{id}
GET|POST         /api/v1/sin90/rhythms
                 # 注意:Rhythm 变更不是状态迁移,是重新分配占比,预计经 Proposal 门
GET|POST         /api/v1/sin90/reviews
GET|PATCH        /api/v1/sin90/reviews/{id}
POST             /api/v1/sin90/proposals/{id}/reject
```

**事件信封是通用的,不是 `sin90.*`。** 领域模块触达 WS 流只有一条缝：`type` 恒为
`"module"`，模块名与模块自己的事件名在 payload 里。客户端按 `payload.module` +
`payload.kind` 分发：

```jsonc
{
  "type": "module",
  "payload": {
    "module": "sin90",                  // 哪个领域 OS
    "kind": "task.transitioned",        // 模块自己的命名空间,内核不解释
    "payload": { /* 模块自定义 */ }
  }
}
```

> 权威定义在 `protocol/events.schema.json` 的 `ModuleEventPayload`（必填
> `module` / `kind` / `payload`）与 `rust/crates/agent24-protocol` 的
> `EventBody::Module`。**本文件不复述字段**。

Reflex(壳内 FSM)不走这些接口；它只在需要落库时（如「用户完成了任务」）发一条 Sin90
**命令**（可被拒，且要带正确的 `x-sin90-actor-key`）。壳从不写既成事实。

---

## 6. Pet0 需要砍掉的自建

原 Pet0 计划里以下**改为复用，不再写第二遍**：

- M1 W1 内核：SQLite 建库/迁移框架/事件回放/事务化 Proposal → 用独立仓库
  `iDoris-ai/Sin90`（`out_of_process_provider`，装进 Agent24 的 packages 目录；
  **不是** Agent24 内核的一部分，也不是 `agent24-store`）。
- M2：`IntelligenceProvider` 抽象 / Intelligence Router 基础设施 / MCP 接入 →
  用 Agent24 内核的 `agent24-models` + `agent24-mcp`。
- 调度/Nudge 触发底座 → 用 Agent24 内核的 `agent24-scheduler`（接线方式待 T13）。

Pet0 保留自有：桌宠 FSM/动画/petpack、语音链路、Nudge UX、以及 Sin90 领域语义的
**共定输入**（现在共定对象是 `iDoris-ai/Sin90` 团队，不是 Agent24 内核团队）。

---

## 7. 一个哲学差异(划清)

不变，见 v0.2 §7：Agent24 内核的「魂」含 Nostr/联邦/多渠道，Pet0 刻意单用户、
local-first、可离线。这条与 Sin90 迁出内核无关，Sin90 本身也是单用户、本地进程、
可离线的。

---

## 8. 第一个联合里程碑：SPIKE-00（已验证，且已升级为真实端到端）

**目标**：证明 Pet0 能坐在 Sin90 上——这条已经不只是理论验证，而是**真实挂载验证过**：

- **Sin90 交付**：M0（Area/Direction/Task 基础 CRUD + 状态机 + 事件查询，13 条判据）→
  M1（`POST /capture` + `GET /today`）→ M2（Week 状态机 + carry-over + attention）
  均已在 `iDoris-ai/Sin90` main 完成，`cargo test` 65 passed。
- **真实挂载判定**：`iDoris-ai/Sin90` 的 `tests/agent24_mount_blackbox.rs`（对着一个
  真实编译的 `agent24d` 二进制跑，`--ignored`，需 `AGENT24_CHECKOUT` 指向已完成 T11
  的 Agent24 checkout）——挂载、代理转发、路由行为不变、事件转发四条判据全部通过。
  2026-09-22 用 T11 分支（现已合并进 main）验证过一次，结果 1 passed。
- **仍欠的一步**：Codex 对抗式评审（尤其 `ab66b37` 的 actor-key 门禁修复）——本轮
  待办已排上日程，见 `docs/agent/tasks.md` 的 2026-09-22 待办清单 P0。
- **Pet0 交付**：一个一次性壳，经 API 建一个 Direction、渲染对账结果——**尚未由
  Pet0 侧执行**，仍是待办。

后续里程碑对齐 Pet0 的 M1/M2/M3，但内核任务替换为「对接 Sin90 API」。

---

## 9. 归属与流程

- **Sin90 业务规格权威**：`iDoris-ai/Sin90` 自己的 `docs/DESIGN-LIFEOS.md`（**不再是**
  Agent24 的 `docs/specs/SIN90-domain.md`——那份文档写于 T11 迁出之前，且早已标注
  「§0 边界论述已被 ADR-029/ADR-030 取代」，仅作历史考古，不要照它排期）。
- **Agent24 内核机制权威**：`docs/specs/SPEC-ME3-OUT-OF-PROCESS.md` + `docs/agent/PLAN-OOP-OS-AND-BACKLOG.md`。
- 本约定两仓库镜像；任何条款变更先改本权威版，再同步 Pet0 镜像，重大变更走 ADR。
- 领域字段/状态机的产品语义：Pet0 提议 → 与 `iDoris-ai/Sin90` 团队确认 → Sin90 实现
  （不再是 Agent24 实现）。

---

## 10. 待确认(Open Questions)

1. Pet0 壳最终选 Tauri 还是复用 Electron？（不阻塞本约定，但影响分发与复用估算）
2. ~~Local 脑走 oMLX 能否吃 Qwen3-0.6B?~~ **已定**：能，见 v0.2 记录，结论不变。
3. ~~Sin90 同库不同表 vs 独立 DB?~~ **已定，且已经比 v0.2 设想的更彻底**：不是
   「内核里的可加载模块 + 独立 DB」，而是**完全独立的仓库与进程**。壳↔Sin90 仍然
   走 HTTP/WS（经 `agent24d` 代理，不是直连）；Sin90↔内核走握手后的回调 socket
   （不是进程内 ctx 句柄——v0.2 设想的「进程内 adapter」形态从未实现，`SPEC-ME3-OUT-OF-PROCESS.md`
   定的是进程外协议）。
4. `.petpack` 的 `behaviors.json` 沙箱与 Agent24 的模块/审批模型如何对齐？（未变，仍待定）
5. **新增**：T13（`agent24-os-sdk`）落地后，Sin90 手写的握手客户端（`src/adapter_agent24`）
   是否要切换成 SDK？不阻塞当前集成，留给 T13 完成后评估。
