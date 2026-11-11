# 入站准入里程碑实施方案（X11 ②③④）

- 日期：2026-11-10（基线：`origin/main` @ `2fa86db3b`）
- 状态：方案草案，待评审。只写文档，没有改代码。
- 依据：[ADR-041](../../adr/ADR-041-inbound-events-admission-and-work-items.md) 的 041a（D1–D4、D10，已接受）；[ADR-044](../../adr/ADR-044-communication.md) D5（X11 结论）；[MILESTONES-AND-TASKS §2.4](MILESTONES-AND-TASKS.md)「尾巴 3a：X11」。
- 不实现 ADR-041b（工单，D5–D9）。

## 0. 一页结论

1. 现状比 ADR 写的更干净：X11 ① 合入（#1785）后，main 上**只剩一条**外部消息能触发 run 的路径，就是开发工具 wechat-bridge（要 `A24_DEV_TOOL=1` 才启动，桥内 uid 名单，直接 `POST /api/v1/runs`）。Hyphae 收件箱现在只是「拉取、显示」，没有任何代码把入站消息变成 run。所以本里程碑主要是**新建**入站入口，不是改造旧路径。
2. 做法：daemon 里新增「入站事件」表和一个准入服务。顺序固定：**先落一条入站记录，再判准入，通过才起 run**，并把 `run_id` 写回这条记录。同一条消息的 id 唯一，重复到达只会返回 `duplicate`，不会起第二次 run。
3. 这个服务放在 `agent24d`，**不放进 `agent24-comm`**。comm 有依赖白名单，`CommState` 不含 Store，也不能起 run（`agent24-comm/tests/comm_zero_run.rs`）；这条结构约束保留，comm 只负责「拿到消息、标好发送者/是否 @」。
4. 本机语音和 ACP 是内部输入，和桌面聊天一样，不进这张表、不走准入。外部入口只有：Hyphae 私信、群消息与 @提及、陌生人消息、wechat-bridge；手机遥控只预留来源名字，不做。
5. 入站记录里「发送者、时间、类别、准入结果」长期保留；消息正文和 `meta` 在 180 天后由 daemon 的后台清理任务置空（只清正文，不删行），做法照搬已有的 `decision_log` 清理。
6. 默认策略沿用 ADR-041 D3/D4：名单外一律只记录不执行、不回复；名单内且私信或 @ 才起 run；好友不等于名单成员。
7. 共 8 片，预计每片 <1000 行，Grok 写码、Opus 验收。最大的风险是 Hyphae 入站消息的字段（发送者、是否 @）现在没有在仓库里找到契约，第 D 片开工前要先对一遍真实输出。
8. 有 2 个需要 jason 拍板的取舍（见 §7），都有推荐。

## 1. ②③④ 的定义

仓库里**没有**把 ②③④ 单独定义出来的文字：`ADR-044`、`ARCH-OPTIMIZATION-PLAN.md`、`docs/tracking/2026-11-10/*.md` 里只有「X11」整体（`ADR-044-communication.md:60-72`、`ARCH-OPTIMIZATION-PLAN.md:277`、`MILESTONES-AND-TASKS.md:145`），以及 ① 的完成记录（`ADR-044-communication.md:130-131`，提交 `b72b076d5`）。编号 ② ③ ④ 只出现在 jason 的任务口径里，我按 ADR-044 D5 的入口表把它们理解为：

| 编号 | 我的理解 | 依据 |
|---|---|---|
| ② | 建入口：入站记录 + 准入关卡 + 先记后跑（daemon 内） | ADR-041 D1、D3、D4；`ADR-044-communication.md:68` |
| ③ | 接 Nostr/Hyphae 入站（私信、群消息、@、陌生人）并重做「入站自动起 run」 | `ADR-044-communication.md:68`、`:75`；提交 `b72b076d5` 说明「以后在入站准入里程碑经 ADR-041a 准入重做」 |
| ④ | wechat-bridge 改成只传输；手机遥控只预留来源；防绕过测试与保留清理 | `ADR-044-communication.md:71-72`、`:78-79` |

这个切分只影响片的归属，不影响内容。**未核实**：jason 心里的 ②③④ 是否如此划分（见 §8）。

## 2. 现状盘点

核对方式：rg + 读文件，基线 `2fa86db3b`。「起 run」指是否会自动调用 `POST /api/v1/runs`。

| 路径 | 入口 file:line | 经过准入 | 自动起 run | 落库 |
|---|---|---|---|---|
| Hyphae 收件箱拉取 | `rust/crates/agent24-comm/src/router.rs:183`（路由）、`:1755`（`pull_inbox`，调 `agent inbox --as`） | 否 | **否**（只转发 Hyphae 的输出） | 落在 Hyphae 自己的历史里，agent24 不记；`GET /comm/history` 读 `history inbox`（`router.rs:182`、`:1709`） |
| Hyphae 历史读取 | `router.rs:182` `get_history` | 否 | 否 | 同上 |
| 群聊 | `rust/crates/agent24-comm/src/groupchat.rs`（`groupchat history/send` 薄封装，文件头表） | 否 | 否（全文无 run 调用） | Hyphae 侧 |
| 桌面 comm 页 | `apps/desktop/src/main/backend-proxy.ts:105-124`（白名单代理 comm 路由） | 不适用 | 否 | — |
| comm 零 run 约束 | `agent24-comm/tests/comm_zero_run.rs`、`comm_dependency_allowlist.rs`（ADR-044 `:73` 引用；文件存在，未逐行读） | — | 守住「comm 不起 run」 | — |
| wechat-bridge | `packages/wechat-bridge/src/bridge.ts:54-66`（`handle`，uid 名单 fail-closed）；起 run：`packages/wechat-bridge/src/agent24.ts:50`（`POST /api/v1/runs`）；启动闸 `main.ts:20` | **桥内名单，非 daemon 准入** | **是**（唯一一条） | 桥内会话文件 `session-store.ts`；daemon 不知道这条消息；无事件 id |
| ACP | `rust/apps/agent24-cli/src/acp.rs:162`（建 session）、`:206`（`POST /api/v1/runs`） | 内部输入，不需要 | 是（预期） | 走普通 run/session |
| 桌面聊天 | `apps/desktop/src/renderer/pages/Runs.tsx`、`pages/agent/api.ts` 经 `backend-proxy.ts` 调 `/api/v1/runs` | 内部输入，不需要 | 是（预期） | 普通 run |
| 本机语音（AgentEar） | `apps/desktop/src/renderer/pages/voice/api.ts:134-158`（只调 `speak` / `stop_playback` 命令）；daemon 侧只有 attach 与计时桥（`server.rs:690-693`） | 内部输入，不需要 | 本仓库内**没找到**语音转写自动起 run 的代码 | — |
| 日程 / 自唤醒 | `agent24-scheduler`（ADR-041 §1 表） | 用户自建，无需准入 | 是 | `FireId` 投递状态机 |
| 手机遥控 | 无代码（ADR-044 `:72`：设计文档不存在） | — | — | — |
| `POST /api/v1/runs` 本身 | `rust/apps/agent24d/src/runs.rs:166` `create_run`；`RunCreate` 见 `rust/crates/agent24-protocol/src/types.rs:479` | **只有 bearer 令牌鉴权**，不区分调用方来源 | — | `RunCreate` 没有「来源/入站事件」字段 |

ADR-041a 已有实现：在 `rust/`、`apps/`、`packages/` 里搜 `inbound_events`、`Admit`、`admission`、`inbound` 的结果，除去无关的 workspace / knowledge / model 并发准入（`model_callback.rs:351` 等）之外，**没有任何入站事件或入站准入的实现**。`agent24-store` 迁移目前到 `0033_runs_before_tool_result_tagging.sql`，没有 `inbound` 表。

结论：入口「Nostr 入站执行」在 #1785 删桥时已经没有了；本里程碑是在空地上建。

## 3. 目标数据流

```
                        ┌─ 内部输入（桌面聊天 / ACP / 本机语音 / 日程）──▶ POST /api/v1/runs（不变，不进入站表）
外部来源适配器          │
 Hyphae 拉取（③）       │
 wechat-bridge（④，POST）──▶ InboundService.ingest(event)         （agent24d 内，唯一入口）
 遥控（以后，只预留）    │      1. 校验形状，截断 text 到 4 000 字
                        │      2. INSERT OR IGNORE inbound_events（disposition='received'）
                        │         └ 已存在 → 返回 duplicate，结束（不判、不起 run）
                        │      3. 确定性准入（纯函数，无模型）→ explicit / inferred / ignore / filtered
                        │         UPDATE 同一行：disposition、reason、policy_version
                        │      4. 仅 explicit：条件更新 run_state='starting'（WHERE run_state IS NULL，抢占成功才继续）
                        │         → start_run_then(RunCreate{prompt, session_id}) → UPDATE run_id、run_state='started'
                        └─ inferred：本里程碑只记录，不起模型、不起 run（没有主动技能，ADR-041 D3 记 ignore(no_rule)）
```

要点：

- **先记后跑**：第 2 步的行在第 4 步之前已经落盘。进程在第 4 步中途退出，记录还在；启动时对账见下。
- **防重复执行**：id 主键 + 第 4 步的条件更新。重复事件到第 2 步就停；并发的两个相同事件只有一个能抢到 `starting`。
- **崩溃恢复口径（我的取舍，写在这里供评审）**：启动时遇到 `run_state='starting'` 且 `run_id IS NULL` 的行，**不自动重跑**，改标 `run_state='interrupted'`，在设置页「观察」里可见。理由：jason 的目的是「防重复执行」，宁可漏跑一次也不要重复执行；用户可以手动让它重试。
- **身份**：`actor` 用来源原生 id（npub、微信 uid），不跨来源映射（ADR-041 D2）。
- **正文标签**：入站 `text` 和 `meta` 按 ADR-041 D10 当 `Unknown` 来源，只是输入，不改变权限；起 run 时 prompt 怎么带来源标签见 §8 未核实项。
- **出口**：本里程碑起的 run 的结果只落在桌面（session 里可见），**不自动回发给对方**（原因见 §7 问题 2）。

### 3.1 入站记录表草案

迁移 `0034_inbound_events.sql`（下一个编号以开工时 main 为准）。放在 `agent24-store`，与 `decision_log`（`0014_decision_log.sql`）同库同体系，迁移由 `sqlx::migrate!("../../crates/agent24-store/migrations")` 统一加载（`rust/apps/agent24d/src/migration_guard/space.rs:421`）。

```sql
CREATE TABLE inbound_events (
    id                TEXT PRIMARY KEY NOT NULL,   -- '<source>:<native_id>[:<edit_version>]'（ADR-041 D1）
    source            TEXT NOT NULL,               -- 'hyphae' | 'wechat' | 'remote'（预留）
    category          TEXT NOT NULL,               -- 'direct' | 'group' | 'group_mention' | 'stranger' | 'dev_bridge'
    thread_key        TEXT,                        -- 群聊用 group_id
    actor             TEXT,                        -- npub / 微信 uid，来源原生 id
    human             INTEGER NOT NULL,            -- 0/1
    addressed         TEXT NOT NULL CHECK (addressed IN ('none','mention','direct')),
    at                TEXT NOT NULL,               -- 事件发生时间，ISO 8601 UTC
    received_at       TEXT NOT NULL,               -- daemon 收到时间
    text              TEXT,                        -- 180 天后置 NULL
    meta              TEXT,                        -- 来源自有 JSON，180 天后置 NULL
    text_scrubbed_at  TEXT,                        -- 非空 = 已清正文
    disposition       TEXT NOT NULL,               -- 'received' | 'explicit' | 'inferred' | 'ignore' | 'filtered'
    disposition_reason TEXT,                       -- 'not_allowlisted' | 'not_addressed' | 'no_rule' | 'mode_off' ...
    policy_version    TEXT,                        -- 判定时的策略哈希（ADR-041 D4）
    run_state         TEXT,                        -- NULL | 'starting' | 'started' | 'interrupted' | 'start_failed'
    run_id            TEXT REFERENCES runs(id)
);
CREATE INDEX idx_inbound_events_at ON inbound_events(at);
CREATE INDEX idx_inbound_events_actor ON inbound_events(source, actor, at);
CREATE INDEX idx_inbound_events_thread ON inbound_events(source, thread_key, at);
CREATE INDEX idx_inbound_events_scrub ON inbound_events(received_at) WHERE text_scrubbed_at IS NULL;
CREATE INDEX idx_inbound_events_recover ON inbound_events(run_state) WHERE run_state = 'starting';
```

名单与接受模式（同一迁移或 `0035`）：

```sql
CREATE TABLE inbound_allowlist (
    source TEXT NOT NULL, actor TEXT NOT NULL,
    added_at TEXT NOT NULL, note TEXT,
    PRIMARY KEY (source, actor)
);
CREATE TABLE inbound_policy (          -- 单行
    id INTEGER PRIMARY KEY CHECK (id = 1),
    accept_mode TEXT NOT NULL DEFAULT 'observe' CHECK (accept_mode IN ('off','observe','auto')),
    updated_at TEXT NOT NULL
);
```

策略版本 = `sha256(排序后的名单 + accept_mode + 准入表版本号)`，每次判定时现算，写进该行。

### 3.2 180 天正文清理

- **保留什么**：整行保留（发送者、时间、类别、准入结果、策略版本、run_id 都长期保留）；只清 `text` 与 `meta`，写 `text_scrubbed_at`。
- **放哪**：`agent24-store` 新增 `scrub_expired_inbound_text(cutoff, now)`，SQL 形如 `UPDATE inbound_events SET text=NULL, meta=NULL, text_scrubbed_at=? WHERE received_at < ? AND text_scrubbed_at IS NULL`，与 `decision_log.rs:307` 的 `scrub_expired_decision_log` 同构，幂等。
- **谁定时**：`agent24d` 起一个后台任务，形态照抄 `memory_maintenance.rs`（循环 + 随全局 `CancellationToken` 停），在 `serve_steps.rs` 里启动（参照 `:409-414`）。启动时先扫一次（补上关机期间过期的），之后每 6 小时一次。
- **为什么不是只靠读时过滤**：`decision_log` 的清理函数现在**没有调用方**（`decision_log.rs:300` 注释自述「D0 中无人调用」；已用 rg 核对，只有测试在调）。本里程碑必须有真实调用方，并有测试钉住「到期后正文真的为空」。
- **按 `received_at` 算 180 天**（不是 `at`），避免来源时钟错乱导致正文立刻被清或永不被清。
- 时钟回拨：回拨只会让清理变慢，不会提前清；可接受，不处理。

### 3.3 准入判定（确定性，沿用 ADR-041 D3）

| 条件 | 结果 |
|---|---|
| `accept_mode = off` | 仅 `explicit` 仍接单（ADR-041 D4：`explicit` 在任何模式下都接单）；其余 `ignore(mode_off)` |
| `human = false`（bot / CI） | 最多 `inferred` |
| 名单内 且 `addressed ∈ {direct, mention}` | `explicit` |
| 名单内 且 `addressed = none`（群里没 @） | `inferred` → 本里程碑 `ignore(no_rule)`，仅记录 |
| 名单外（含陌生人） | `ignore(not_allowlisted)`，不回复、不通知对方 |
| 范围外（来源标 `filtered`） | `filtered`，仍记录 |

好友表不写进名单；本轮没有好友机制（jason 口径 6）。

## 4. 分片计划

编号沿用 `X11-2*`。规模：S <300 行，M 300–700，L 700–1000。「派给」按 `feedback_model_split_dev`：Grok 写码，Opus 规划/验收，Sonnet 做机械活。每片 PR 前照旧：自审 → Codex 挑战（或独立 Opus 子代理）→ `pre-pr-check.sh`。

| 片 | 目标 | 涉及文件 | 规模 | 依赖 | 验收（可测；含正/反对照） | 风险 | 派给 |
|---|---|---|---|---|---|---|---|
| X11-2a | 迁移 + store 层：`inbound_events` / `inbound_allowlist` / `inbound_policy`；`insert_event`（重复返回 duplicate）、`set_disposition`、`claim_run_start`（条件更新抢占）、`mark_started`、`scrub_expired_inbound_text` | `rust/crates/agent24-store/migrations/0034_*.sql`、`rust/crates/agent24-store/src/inbound.rs`（新）、`lib.rs` | M | 无 | ① 同 id 插两次：第二次 `duplicate`，行数 1（**反例**：去掉主键冲突处理则测试变红）。② 两个并发 `claim_run_start`：恰好一个成功。③ 清理：`received_at` 早于 cutoff 的行 `text/meta` 为空且 `actor/at/category/disposition` 原样；晚于 cutoff 的行不变（正对照）；再跑一次返回 0（幂等）。④ 迁移测试：对一个 0033 库升级后旧表数据不变 | 迁移编号与并行 PR 冲突；`runs(id)` 外键与已清理 run 的关系 | Grok |
| X11-2b | 准入纯函数 `admit(event, policy) -> Decision` + 策略版本哈希 | `rust/apps/agent24d/src/inbound/admit.rs`（新） | S | 2a（类型） | 表驱动单测覆盖 §3.3 每一行；**变异对照**：把「名单内」判断反转，至少 3 条用例变红；不依赖模型/网络；同输入同策略得同哈希，名单顺序不同哈希相同 | 与 ADR-041 D3 逐条不一致 | Grok |
| X11-2c | `InboundService.ingest`：记录 → 准入 → 抢占 → `start_run_then` → 写回；启动对账（`starting` 无 run → `interrupted`） | `rust/apps/agent24d/src/inbound/service.rs`（新）、`serve_steps.rs`（接线）、`lifecycle.rs` 视需要 | L | 2a、2b | ① explicit：恰好起 1 个 run，`run_id` 写回。② 同一事件 id 并发 10 次：run 恰好 1 个（**变异对照**：去掉 `claim_run_start` 则变红）。③ 名单外：0 个 run，行里 `ignore(not_allowlisted)`。④ 在「已记录、未起 run」时模拟崩溃重启：不起 run，行变 `interrupted`。⑤ `start_run` 失败：`start_failed`，不重试 | run 起来之后的「来源标签」接线（§8-1）；session 归属见 §7-1 | Grok，Opus 验收 |
| X11-2d | 清理后台任务 + 接线 | `rust/apps/agent24d/src/inbound/maintenance.rs`（新）、`serve_steps.rs` | S | 2a | 用可注入时钟：造 179 天与 181 天两行，跑一次任务，只有 181 天的正文被清；daemon 停止信号到来时任务退出；启动即扫一次 | 与 `memory_maintenance` 启动顺序冲突 | Sonnet |
| X11-2e | Hyphae 入站适配：daemon 定期拉 `agent inbox`/`history inbox --as`，持读位置，标 `actor`/`human`/`addressed`/`category`，先滤自己发的，调用 `ingest` | `rust/apps/agent24d/src/inbound/hyphae_source.rs`（新）；只**调用** comm 的 HTTP/内部接口，不改 `agent24-comm` 的依赖 | L | 2c | ① 用 `agent24-comm/tests/fixtures` 同款假 hyphae 脚本：私信→`direct`、群 @→`group_mention`、群未 @→`group`、陌生人→`stranger`。② 自己发的消息不入表。③ 重启不重放（读位置持久）。④ **`comm_zero_run` 与 `comm_dependency_allowlist` 两个测试照旧通过**（反向证明 comm 没被污染） | **Hyphae 入站 JSON 字段未核实**（§8-2）；读位置落哪（§8-3）；keystore 未解锁时的行为 | Grok，Opus 验收 |
| X11-2f | 名单与接受模式 API + 事件只读列表：`GET/PUT /api/v1/inbound/allowlist`、`GET/PUT /api/v1/inbound/policy`、`GET /api/v1/inbound/events`（只回元数据 + 前 50 字，不进 WS `/events`） | `rust/apps/agent24d/src/inbound/routes.rs`（新）、`capabilities/operations.rs`（登记 `Operation`，写为提升级）、OpenAPI + `packages/api-client` 生成物 | M | 2a | ① 无令牌 / 低权限令牌写名单被拒（反对照）；读允许。② 改名单后下一条同发送者事件的策略版本变化。③ 列表接口不含全文，且 WS `/events` 抓包无入站内容。④ contract-tests 覆盖新路由 | CORE-2 `Operation` 登记漏项；OpenAPI 生成物体量大会撑大 diff | Grok |
| X11-2g | wechat-bridge 改成只传输：消息 `POST /api/v1/inbound/events`（`source=wechat`，id 用微信消息 id 或 `from_user_id+seq`），删桥内名单与起 run 代码；y/n 审批回复保留 | `packages/wechat-bridge/src/{bridge,agent24,config,main}.ts` 及测试；daemon 侧加 `POST /api/v1/inbound/events`（Elevated 以外的专用令牌？见 §8-4） | M | 2c、2f | ① 桥不再直接调用 `/api/v1/runs`（静态断言：`agent24.ts` 无 `/runs` POST）。② 名单外微信用户：daemon 记 `ignore`，不起 run。③ 重复投递同一条微信消息只起 1 个 run。④ 仍需 `A24_DEV_TOOL=1`（`main.test.ts` 现有用例不退化） | 桥现有 90 项测试（#1785 记录）需大改；审批 y/n 流程依赖 run 同步返回 | Sonnet（机械改写）+ Opus 验收 |
| X11-2h | 防绕过与文档收口：测试钉住「外部来源只能经 `InboundService` 起 run」「语音/ACP 不进入站表」；回写 ADR-044 T2/T3、MILESTONES §2.4、`COMM-HYPHAE.md` | `rust/apps/agent24d/tests/inbound_*.rs`、文档 | S | 2a–2g | ① 源码扫描：`agent24d/src` 内除 `runs.rs`/`inbound/service.rs`/调度投递外无新的 `start_run` 调用点（**变异对照**：临时加一个调用点则变红）。② ACP 与桌面聊天起 run 后 `inbound_events` 行数不变（正对照）。③ 文档里的「待办」行与代码状态一致 | 源码扫描类测试易误报，需带白名单与说明 | Sonnet |

推荐顺序：2a → 2b → 2c → 2d →（2e、2f 可并行）→ 2g → 2h。2a–2d 合入后，daemon 已有可测试的完整「记录→准入→起 run」闭环，只是还没有真实来源。

整体规模估计：约 2 500–3 500 行（含测试），不含 OpenAPI 生成物。按「大改动默认拆 stacked PR，但以少为好」，2b 可并入 2a，2d 可并入 2c，合并后 6 片。

## 5. 明确不做

- 工单、授权依据、订阅（ADR-041b，D5–D9）；run 不加 `work_item_id`。
- `inferred` 候选的模型研判与主动技能（ADR-041 D3 后半、ADR-040 D6）；本轮 `inferred` 一律只记录。
- 好友机制（COMM-FRIENDS）；好友不进名单。
- 手机遥控：只在 `source` 取值里预留 `remote`，不写任何接入代码、不写设计。
- 本机语音（AgentEar）与 ACP：按内部输入处理，不进入站表，不改它们的路径。
- 自动回发：run 完成后不自动把结果发回对方。
- 重做 Hyphae 的身份、relay、发送链路（ADR-044 其它条款）。
- 改 `agent24-comm` 的依赖白名单或让 `CommState` 持有 Store。
- 跨来源身份映射。
- 桌面「来源」设置页的完整界面（ADR-041 D10 的三档「谁能让我做事」）：本里程碑只给 API；界面另开片，等名单 API 稳定。

## 6. 与现有体系的对齐点

- 迁移：`agent24-store/migrations` 顺序编号，本片从 `0034` 起；迁移守卫 `agent24d/src/migration_guard/space.rs:421` 统一加载，不另起库。
- 清理模式：`decision_log` 的置空式清理（`0014_decision_log.sql` 头注释）。
- 后台任务：`memory_maintenance.rs` 的形态（循环 + `CancellationToken`）。
- 起 run：`runs.rs:211` 的 `state.runs.start_run_then`，不新造起 run 的路径。
- 权限：新路由登记 CORE-2 `Operation`，写操作为提升级（ADR-041 D10 同款要求）。

## 7. 需要 jason 拍板的取舍

**1. 外部消息起的 run，会话怎么归属？**
选项：(a) 每个「来源 + 发送者 + 群」一个长期会话，同一个人的后续消息接着聊；(b) 每条消息一个新会话，互不相干。(a) 体验像在聊天，但上下文会累积外部文本；(b) 干净，但 agent 记不住上一句。推荐 (a)，会话的 `channel` 记来源名（如 `hyphae`），并且这类会话里的外部文本一律按「不可信输入」处理，权限不因聊久了而放宽。

**2. run 做完后，结果要不要自动发回给对方？**
选项：(a) 本轮不发，结果只在桌面里看，要回复由你点确认；(b) 自动回发。目前 Hyphae 的发送「没有在场证明」，这个风险你已经接受过但留到 CORE-2d/7c 再收口（ADR-044 X13）；如果现在加自动回发，名单内的人就能让 agent 往外发消息，范围比「让它干活」大一档。推荐 (a)，等发送的在场证明做完再开。

## 8. 未核实项

1. **起 run 时给外部文本打 `Unknown` 来源标签的接口**：ADR-041 D10 要求，`RunCreate`（`types.rs:479`）没有来源字段；`run_source_tags`（迁移 0016）的写入入口是「run 入口的用户输入」。是否要给 `RunCreate` 加字段、还是 `InboundService` 直接调 `source_policy` 写标签，**没有读 `source_policy.rs` 与 `start_run_then` 的实现**，2c 开工前先读。
2. **Hyphae 入站消息 JSON 的字段**（发送者 npub、是否 @、群 id、`event_id`、是否人类）：没有在本仓库找到 `agent inbox` / `history inbox` 的输出契约；只在 `router.rs` 看到路由转发。`COMM-HYPHAE.md`、`docs/comm/JOINT-ROUND1.md` 未读，2e 开工前要对真实 Hyphae 输出（及其仓库的 group-chat 接口文档，`groupchat.rs` 文件头引用）。ADR-041 的 `addressed` 与 `human` 是否能由 Hyphae 直接给出未知。
3. **读位置放哪**：ADR-041 D2 要求「来源持读位置、重启不重放」。`agent inbox` 是否自带游标未核实；若没有，要在 `inbound_events` 旁加一张 `inbound_cursor` 表（会让 2a/2e 各增一点）。
4. **wechat-bridge 用什么令牌 POST 入站事件**：现在桥用 legacy 全权令牌（ADR-044 `:79`）。是否给 `POST /api/v1/inbound/events` 单独做一个只能写入站的令牌，取决于 CORE-2 现状（C9 未排期），**未核实**；最小做法是开发态继续用现有令牌，并在方案里注明仍是开发工具。
5. **语音转写会不会自动起 run**：在 `apps/desktop/src/renderer/pages/voice/` 和 `agent24d` 里只找到 `speak` / `stop_playback` 与计时桥，没找到「听到话就起 run」的路径；可能在 AgentEar 仓库那边（经桌面聊天）。按 jason 口径当内部输入处理，不影响本方案，但「现状」一行写的是「本仓库没找到」，不是「确认没有」。
6. **ACP 与桌面聊天起 run 的完整调用链**只核对到 HTTP 调用点（`acp.rs:206`、`backend-proxy.ts`），没有读 `runs.rs` 以外的鉴权细节；「它们不会绕过令牌」默认成立。
7. `comm_zero_run.rs` 与 `comm_dependency_allowlist.rs` 只确认文件存在、被 ADR-044 引用，没有逐行读它们具体断言了什么。
8. 第 1 节的 ②③④ 划分是我的理解（仓库里没有定义）。
9. `inbound_events.run_id REFERENCES runs(id)`：runs 是否有清理/删除路径会破坏外键，未核实。
10. 迁移编号 `0034` 以开工时 main 为准；本文基线 `2fa86db3b` 的最大编号是 `0033`。
