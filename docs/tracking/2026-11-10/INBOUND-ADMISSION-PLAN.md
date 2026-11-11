# 入站准入里程碑实施方案（X11 ②③④）

- 日期：2026-11-10（基线：`origin/main` @ `2fa86db3b`；第 2 版吸收 Opus 独立评审）
- 状态：方案草案，待评审。只写文档，没有改代码。
- 依据：[ADR-041](../../adr/ADR-041-inbound-events-admission-and-work-items.md) 的 041a（D1–D4、D10，已接受）；[ADR-044](../../adr/ADR-044-communication.md) D5（X11 结论）；[MILESTONES-AND-TASKS §2.4](MILESTONES-AND-TASKS.md)「尾巴 3a：X11」。
- 不实现 ADR-041b（工单，D5–D9）。
- §7 四条取舍 jason 2026-11-11 已拍板，均按推荐。

## 0. 一页结论

1. 现状比 ADR 写的更干净：X11 ① 合入（#1785）后，main 上**只剩一条**外部消息能触发 run 的路径，就是开发工具 wechat-bridge（要 `A24_DEV_TOOL=1` 才启动，桥内 uid 名单，直接 `POST /api/v1/runs`）。Hyphae 收件箱现在只是「拉取、显示」，没有任何代码把入站消息变成 run。所以本里程碑主要是**新建**入站入口，不是改造旧路径。
2. 做法：daemon 里新增「入站事件」表和一个准入服务。准入是确定性纯函数，**先算好结果，一条 INSERT 把记录和结果一起写入，再起 run**，run 与「入站事件 id」在同一事务里落库。同一条消息 id 唯一，重复到达不会起第二次 run；崩溃后不自动重跑，宁可漏跑也不重复执行。
3. 外部文本必须在 run 内被打成 `Unknown` 来源，而不是普通用户输入。现在 run 入口无条件打 `user_input/Ordinary`，所以要给 RunManager 加一个**只在进程内可调用**的入口，不能走 HTTP 的 `RunCreate`。
4. 服务放在 `agent24d`，不放进 `agent24-comm`：comm 的依赖白名单和「不含 Store、不起 run」是结构约束，保留。
5. 本机语音、ACP、MCP 服务、桌面聊天是内部输入，不进入站表；日程与自唤醒只写一行记录，不走准入。外部入口只有：Hyphae 私信、群消息与 @提及、陌生人消息、wechat-bridge；手机遥控只预留来源名。
6. 入站记录里发送者、时间、类别、准入结果长期保留；正文 180 天后清理（清理范围：连 run 与记忆中的副本一起清，jason 2026-11-11 拍板，见 §7-3）。名单外只记录、不执行、不回复；好友不等于名单成员；本轮 run 结果不自动回发。
7. 共 8 片，每片预计 <1000 行；2e 因为要接 Hyphae 的全部消息类型，规模按 L 估。
8. 4 件事等 jason 拍板（§7），都已按推荐写入方案。

## 1. ②③④ 的定义

仓库里**没有**单独定义 ②③④：`ADR-044-communication.md`、`ARCH-OPTIMIZATION-PLAN.md`、`docs/tracking/2026-11-10/*.md` 只有 X11 整体（`ADR-044-communication.md:60-72`、`ARCH-OPTIMIZATION-PLAN.md:277`、`MILESTONES-AND-TASKS.md:145`）和 ① 的完成记录（`ADR-044-communication.md:130-131`，提交 `b72b076d5`）。我按 ADR-044 D5 入口表理解：

| 编号 | 理解 | 依据 |
|---|---|---|
| ② | 建入口：入站记录 + 准入关卡 + 先记后跑 | ADR-041 D1、D3、D4；`ADR-044-communication.md:68` |
| ③ | 接 Hyphae 入站（私信、群、@、陌生人）并重做「入站起 run」 | `ADR-044-communication.md:68`、`:75`；提交 `b72b076d5` 正文 |
| ④ | wechat-bridge 改只传输；遥控只预留来源；防绕过与保留清理 | `ADR-044-communication.md:71-72`、`:78-79` |

划分只影响片的归属。**未核实**：是否与 jason 心里的 ②③④ 一致（§8）。

## 2. 现状盘点

核对方式：rg + 读文件，基线 `2fa86db3b`。「起 run」指是否自动调用 `POST /api/v1/runs`。

| 路径 | 入口 file:line | 经过准入 | 自动起 run | 落库 |
|---|---|---|---|---|
| Hyphae 收件箱拉取 | `rust/crates/agent24-comm/src/router.rs:183`、`:1755`（`pull_inbox`，`agent inbox --as`） | 否 | 否 | Hyphae 自己的历史；agent24 不记 |
| Hyphae 历史读取 | `router.rs:182` `get_history`（`history inbox`） | 否 | 否 | 同上 |
| 群聊 | `rust/crates/agent24-comm/src/groupchat.rs`（`groupchat history` 等薄封装；文件头 `:25-28` 提到 history 的游标参数） | 否 | 否 | Hyphae 侧 |
| 桌面 comm 页 | `apps/desktop/src/main/backend-proxy.ts:105-124` | 不适用 | 否 | — |
| wechat-bridge | `packages/wechat-bridge/src/bridge.ts:54-66`（uid 名单 fail-closed）；起 run `agent24.ts:50`；启动闸 `main.ts:20` | 桥内名单，非 daemon 准入 | **是（唯一一条）** | 桥内会话文件；daemon 不知道这条消息 |
| ACP | `rust/apps/agent24-cli/src/acp.rs:162`、`:206` | 内部输入，不需要 | 是 | 普通 run |
| MCP 服务 | `rust/crates/agent24-mcp/src/server.rs:2-10`、`:202`（`agent24_run`，`POST /api/v1/runs`，run 内容只经 peer 视图回给调用方） | **内部输入口径：本机 MCP 客户端，与 ACP 同处理**（我类推 jason 对 ACP 的口径，**待确认**） | 是 | 普通 run |
| 桌面聊天 | `apps/desktop/src/renderer/pages/Runs.tsx`、`pages/agent/api.ts` 经 `backend-proxy.ts` | 内部输入 | 是 | 普通 run |
| 本机语音（AgentEar） | `apps/desktop/src/renderer/pages/voice/api.ts:134-158`（只 `speak`/`stop_playback`）；`server.rs:690-693`（计时桥） | 内部输入 | 本仓库没找到「听到话就起 run」的代码 | — |
| 日程 / 自唤醒 | `agent24-scheduler`；ADR-041 §1 表 | 用户自建，无需准入 | 是 | `FireId` 投递状态机 |
| 手机遥控 | 无代码 | — | — | — |
| `POST /api/v1/runs` | `rust/apps/agent24d/src/runs.rs:166`；`RunCreate` `agent24-protocol/src/types.rs:479` | 只有 bearer 令牌鉴权 | — | `RunCreate` 无来源字段 |
| run 入口打标签 | `agent24-agent/src/lib.rs:2211-2215`（`execute` 无条件 `SourceRef::user_input`）；`agent24-store/src/source_policy.rs:266-277`（`Ordinary`/`LocalOnly`） | — | — | `run_source_tags` |
| 起 run 的内部函数 | `agent24-agent/src/lib.rs:1297`（`start_run_then`）；`:1340-1350`（回调同步执行后立刻 `tokio::spawn(execute)`） | — | — | `persist_new_run` `:679` |

ADR-041a 已有实现：搜 `inbound_events`、`Admit`、`inbound` 等，除无关的 workspace / knowledge / 模型并发准入外，**没有入站事件或入站准入的实现**；`agent24-store` 迁移目前到 `0033`。

## 3. 目标数据流

```
内部输入（桌面聊天 / ACP / MCP / 本机语音）──▶ POST /api/v1/runs（不变，不进入站表）
日程 / 自唤醒 ──▶ 照旧起 run；另外只写一行 inbound_events（source=scheduler|self，disposition='internal'，不走准入）

外部来源适配器（Hyphae 拉取、wechat-bridge POST、遥控预留）
   └▶ InboundService.ingest(event)                       （agent24d 内，唯一外部入口）
       1. 形状校验；text 截断到 4 000 字
       2. 纯函数 admit(event, policy, watermark, 在途/速率) 先算出结果（含 stale / rate_limited）
       3. 一条 INSERT ... ON CONFLICT(id) DO NOTHING，同时写 disposition、reason、policy_version、
          run_state（explicit 为 'starting'，其余 NULL）
          └ 没插入（id 已存在）→ 返回 duplicate，结束
       4. 仅 run_state='starting'：RunManager.start_run_inbound(create, InboundOrigin{event_id})
          └ run 行与 inbound_runs(event_id ↔ run_id) 在同一事务落库；execute 对该 run 的用户消息打 Unknown
       5. 起成功：UPDATE run_state='started'（这一步丢了也无妨，对账会补）
```

要点：

- **没有独立的 `received` 状态**。准入是纯函数，插入前就算好，所以「记录已落盘、结果未写」的窗口不存在。拉取型来源（Hyphae）在步骤 3 之前崩溃，行不存在，读位置没前进，重启后重新拉到，不丢。
- **「run 已落库、未写回」窗口**：run 与 `inbound_runs` 同事务，所以重启对账可按它判断：有 `inbound_runs` 行 → 补成 `started`；`run_state='starting'` 却没有 `inbound_runs` 行 → 标 `interrupted`，**不自动重跑**，只在设置页「观察」里显示「这条消息没有起成 run」，不提供重试按钮。
- **外部文本标签（评审 C1）**：`start_run_then` 的回调是同步的，run 落库后立刻 spawn `execute`，事后补标签有竞态，所以不用回调补标。改为 `RunManager` 增加进程内入口 `start_run_inbound(create, origin)`（`origin` 不进 HTTP 反序列化的 `RunCreate`，外部无法伪造），`execute` 看到 origin 时对该 run 的用户消息打 `Unknown` 标签，而不是 `lib.rs:2211-2215` 的 `user_input`。
- **身份**：`actor` 用来源原生 id（npub、微信 uid），不跨来源映射。表里加接收身份列，记「是发给哪个本机身份的」。
- **文本进模型（已拍板 §7-4，jason 2026-11-11）**：按推荐，全文进 prompt，整段标 `Unknown`，上限 4 000 字，并修订 ADR-041 D10。
- **会话（已拍板 §7-1，jason 2026-11-11）**：按推荐，每条消息一个新会话，会话 `channel` 记来源名。
- **出口**：run 结果不自动回发（已定，不是拍板项）。微信桥只回一句「已记录」。

### 3.1 入站记录表草案

迁移 `0034_inbound_events.sql`（编号以开工时 main 为准）。放在 `agent24-store`；迁移由 `agent24-store/src/lib.rs:319`（`sqlx::migrate!("./migrations").run`）与 `:379`（`embedded_schema_version`）加载，守卫在 `agent24d/src/migration_guard/space.rs:421`。**只新增表，不改旧表**（理由见 §7-2）。

```sql
CREATE TABLE inbound_events (
    id                 TEXT PRIMARY KEY NOT NULL,  -- '<source>:<native_id>'，不带编辑版本（见 §3.4）
    source             TEXT NOT NULL,              -- 'hyphae' | 'wechat' | 'remote'(预留) | 'scheduler' | 'self'
    category           TEXT NOT NULL,              -- 'direct'|'group'|'group_mention'|'stranger'|'dev_bridge'|'protocol'|'internal'
    recipient_identity TEXT,                       -- 本机接收身份（nickname/npub）
    thread_key         TEXT,                       -- 群聊用 group_id
    actor              TEXT,
    human              INTEGER NOT NULL,
    addressed          TEXT NOT NULL CHECK (addressed IN ('none','mention','direct')),
    at                 TEXT NOT NULL,
    received_at        TEXT NOT NULL,
    text               TEXT,                       -- 到期置 NULL
    meta               TEXT,                       -- 到期置 NULL
    text_scrubbed_at   TEXT,
    disposition        TEXT NOT NULL,              -- 'explicit'|'ignore'|'filtered'|'internal'
    disposition_reason TEXT,                       -- 'not_allowlisted'|'not_addressed'|'no_rule'|'stale'|'rate_limited'|'protocol'|'bot' ...
    policy_version     TEXT,
    run_state          TEXT                        -- NULL|'starting'|'started'|'done'|'interrupted'|'start_failed'
);
CREATE TABLE inbound_runs (                        -- 与 run 同事务写入
    run_id   TEXT PRIMARY KEY NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    event_id TEXT NOT NULL UNIQUE REFERENCES inbound_events(id)
);
CREATE INDEX idx_inbound_events_at     ON inbound_events(at);
CREATE INDEX idx_inbound_events_actor  ON inbound_events(source, actor, at);
CREATE INDEX idx_inbound_events_thread ON inbound_events(source, thread_key, at);
CREATE INDEX idx_inbound_events_scrub  ON inbound_events(received_at) WHERE text_scrubbed_at IS NULL;
CREATE INDEX idx_inbound_events_recover ON inbound_events(run_state) WHERE run_state = 'starting';
CREATE UNIQUE INDEX idx_inbound_events_inflight ON inbound_events(source, actor)
    WHERE disposition = 'explicit' AND run_state IN ('starting','started');  -- M6：每发送者最多一个在途
CREATE TABLE inbound_allowlist (source TEXT NOT NULL, actor TEXT NOT NULL, added_at TEXT NOT NULL, note TEXT, PRIMARY KEY (source, actor));
CREATE TABLE inbound_policy (                      -- 单行
    id INTEGER PRIMARY KEY CHECK (id = 1),
    accept_mode TEXT NOT NULL DEFAULT 'observe' CHECK (accept_mode IN ('off','observe','auto')),
    updated_at TEXT NOT NULL
);
CREATE TABLE inbound_source_watermark (            -- 每来源激活水位（评审 H5）
    source TEXT PRIMARY KEY NOT NULL, activated_at TEXT NOT NULL, max_age_secs INTEGER NOT NULL
);
```

外键方向说明：`inbound_runs.run_id → runs(id) ON DELETE CASCADE`（run 被清则关联行跟着清，事件行保留）；事件表自己不引用 `runs`，所以 run 删除不会破坏入站记录（评审 L：`ON DELETE SET NULL` 的诉求用关联表实现）。读位置若 Hyphae 自带游标则不另建表，否则 2e 加 `inbound_cursor`。

策略版本 = `sha256(排序后的名单 + accept_mode + 准入表版本号)`，判定时现算，写进该行。

### 3.2 正文清理

- **保留什么**：整行保留（发送者、时间、类别、准入结果、策略版本）；清 `text`、`meta`，写 `text_scrubbed_at`。
- **放哪**：`agent24-store` 新增 `scrub_expired_inbound(cutoff, now, batch, memory_handled)`（2a 的 `scrub_expired_inbound_text` 只留作测试），同构于 `decision_log.rs:307` 的 `scrub_expired_decision_log`，幂等。
- **谁定时**：`agent24d` 后台任务，形态照 `memory_maintenance.rs`，在 `serve_steps.rs` 启动（参照 `:409-414`）；启动先扫一次，之后每 6 小时。
- **为什么要真调用方**：`decision_log` 的清理函数目前只有测试调用（`decision_log.rs:306` 注释自述无人调用）。
- 按 `received_at` 算 180 天。
- **范围（已拍板 §7-3，jason 2026-11-11）**：按推荐，同一任务里一并清掉这些事件对应 run 的 `runs.input`、run 消息快照、会话记忆里来自该事件的内容，否则「180 天删正文」名不副实。**这会碰旧表**，2d 的规模和风险按此重估；落地方式（置空 vs 级联删）开片前读 `run_messages`、记忆写入路径再定。
- **已知限制（2d 落地时如实记录）**：
  - 会话记忆按会话整体清理，且只在该会话里的 run 全部是到期入站 run 时才清。会话混入了非入站 run（用户在 UI 里接着聊）时，该会话的记忆按设计不清（记忆里无法区分哪些来自入站），入站原文留在记忆里；run 与消息快照等副本仍照清。
  - `/chat` 不写 run，却把轮次写进调用方给的会话。清理时记忆层检查会话里是否有晚于入站 run 终态（加 1 秒宽限）的事件，有就整段保留，与上一条同一结局。
  - 「记住」类断言不由 2d 清理，依赖 X11-2c 在源头禁止入站 run 写断言。
  - 审计链（`audit_log`）里的 `approval.required` 条目带审批摘要与负载，哈希链不可改，不清。
  - 入站 run 创建的自唤醒日程：其 prompt（`schedules.action`）不清，唤醒 run 没有 `inbound_runs` 关联，会让该会话变成非独占，会话记忆也留下。
  - 不覆盖：`tool_calls.tool` 名称、记忆派生表（`mem_consolidations`、向量、trace）、非对象形态的 `runs.input`；写入缺口与遗留 kv 快照按会话 id 删除。

### 3.3 准入判定（确定性，优先级自上而下，第一条命中即结果）

| 优先级 | 条件 | 结果 |
|---|---|---|
| 1 | 机器协议消息（f4/1 信封、ack、`hyphae-behavior` query/response、canary）或好友协议消息 | `ignore(protocol)`，**一律 zero-run** |
| 2 | 事件时间早于该来源激活水位，或距今超过 `max_age_secs` | `ignore(stale)`（H5） |
| 3 | `human = false` | `ignore(bot)`（本轮不做 `inferred`） |
| 4 | 名单外（含陌生人） | `ignore(not_allowlisted)`，不回复不通知 |
| 5 | 名单内，但该 `source+actor` 已有一个在途的 explicit，或超速率上限 | `ignore(rate_limited)`（M6；在途由库强制，见 §3.4） |
| 6 | 名单内且 `addressed ∈ {direct, mention}` | `explicit`（任何接受模式都接单，ADR-041 D4） |
| 7 | 名单内但未 @ | `ignore(no_rule)`（`inferred` 本里程碑统一为这条，ADR-041 D3：无主动技能时记 `ignore(no_rule)`） |

`human` 的确定性规则：来源适配器按固定规则给——Hyphae 消息的 `kind`/内容不是协议信封、且不匹配 canary 前缀才算 `human=true`；微信 `message_type=USER` 才算。规则写进 2e/2g 的单测，不让模型判。

好友表不写进名单；本轮没有好友机制。

### 3.4 其它约束

- **激活水位（H5）**：每来源启用时记 `activated_at`，只处理其后到达的消息；另有最大时延（默认 10 分钟，待 2c 定值），超出记 `ignore(stale)`。先例：ADR-044 D5 的 F4b「解冻后只分派启动之后到达的消息」。防的是 daemon 离线几天后一次拉到积压消息全起 run。
- **速率（M6，F2）**：每 `source+actor` 同时最多 1 个在途 explicit，**由库强制而不是先查后插**：`inbound_events` 上建部分唯一索引 `CREATE UNIQUE INDEX idx_inbound_events_inflight ON inbound_events(source, actor) WHERE disposition='explicit' AND run_state IN ('starting','started')`；run 终结时由 `agent24d` 把对应行的 `run_state` 置为 `'done'`（run 状态变更处挂钩，含取消、失败；崩溃后对账同样会把无主的 `started` 收敛为 `done`/`interrupted`）。INSERT 撞上该索引时，服务捕获冲突，改写一行 `ignore(rate_limited)`（用新的行 id 后缀或同事务二次插入，2a 定细节），不起 run。放弃的另一种做法是对 `(source,actor)` 在 `ingest` 里串行，它不能防多进程且要持锁；滑动窗口速率上限（默认每分钟 3 条，待定），超出记 `ignore(rate_limited)`。
- **日程与自唤醒（H7）**：按 ADR-041 D1 写一行 `source=scheduler|self`、`disposition='internal'`，**不改它们的起 run 路径**，不走准入。

- **接受模式本轮只存不判（F5）**：`inbound_policy.accept_mode` 本轮只存储、可读写，**不参与任何判定**，所以 §3.3 没有 `mode_off` 这类原因码。它进策略版本哈希，为以后的 `observe`/`auto`（含 `inferred` 研判）留位置；`explicit` 在任何模式下都接单（ADR-041 D4），本轮与之等价。
- **编辑消息（F3）**：去重键不带 `[:<edit_version>]`。仓库内查了 wechat-bridge 的 iLink 类型（`ilink/types.ts`，无编辑字段）、`COMM-HYPHAE.md`、`ADR-044`、`agent24-comm/src`，都没有「编辑已发消息」的概念；Nostr 私信（NIP-44）本身也是不可变事件。**未核实** Hyphae 上游是否将来会有编辑。若以后出现：编辑算**新事件**，id 带版本，重新过名单和速率（与新消息同等对待，不继承原事件的准入结果），届时单独一片处理。
- **开发态名单怎么种（F1）**：`inbound_allowlist` 的写接口是 host_only（legacy 模式 403，2f），而 `POST /api/v1/inbound/events` 只在开发态挂载，两者叠加会让开发态没有办法放行任何微信 uid。办法：`A24_DEV_TOOL=1` 时，daemon 启动读取种子（环境变量 `A24_INBOUND_DEV_ALLOWLIST=wechat:<uid>,...`，或同目录配置文件），在 `ingest` 之前写进 `inbound_allowlist`（幂等 upsert，`note='dev-seed'`）；**capabilities 模式下完全不读、不生效**，所以产品构建里名单只能由 ProductHost 经 2f 写入。种子逻辑放 2g。

## 4. 分片计划

规模：S <300 行，M 300–700，L 700–1000。「派给」按 `feedback_model_split_dev`：Grok 写码，Opus 验收，Sonnet 做机械活。每片 PR 前：自审 → Codex 挑战（或独立 Opus 子代理）→ `pre-pr-check.sh`。「变异对照」指故意改坏实现后测试必须变红；「正对照」指同条件下应通过的合法输入必须通过。

| 片 | 目标 | 涉及文件 | 规模 | 依赖 | 验收 | 风险 | 派给 |
|---|---|---|---|---|---|---|---|
| X11-2a | 迁移 `0034` + store 层：上述各表；`insert_event`（`ON CONFLICT(id) DO NOTHING`，返回是否插入）；`mark_started`；对账查询；`scrub_expired_inbound_text`（仅入站表部分）；日程/自唤醒的记录行写入 | `agent24-store/migrations/0034_*.sql`、`agent24-store/src/inbound.rs`（新）、`lib.rs`；日程/自唤醒写入点（`agent24-scheduler` 或 `agent24d/src/scheduler_deliver.rs`） | M | 无 | ① 同 id 插两次，第二次返回未插入，行数 1，且**第二次不覆盖第一次的 disposition**（变异：改成 `INSERT OR REPLACE` 变红）。② 清理：早于 cutoff 的行 text/meta 为空、actor/at/category/disposition 原样；晚于 cutoff 的行不变（正对照）；再跑返回 0。③ 0033 库升级后旧表数据不变。④ 日程触发一次后 `inbound_events` 多一行 `internal`，且 run 仍照常起（旧行为不退化） | 迁移编号冲突；见 §7-2 的 CORE-5 5g 约束 | Grok |
| X11-2b | 准入纯函数 `admit(...)` + 策略版本哈希，覆盖 §3.3 全部 7 行与水位 / 速率入参 | `agent24d/src/inbound/admit.rs`（新） | S | 2a（类型） | 表驱动：每行至少一条命中用例 + 一条不命中的正对照；优先级用例（协议消息即使名单内也是 `ignore(protocol)`）；变异：反转「名单内」判断，至少 3 条变红；同输入同哈希、名单顺序无关 | 与 ADR-041 D3 不一致 | Grok |
| X11-2c | `RunManager.start_run_inbound` 与 `InboundService.ingest`：INSERT 一次写结果 → 起 run → 同事务 `inbound_runs` → 打 Unknown 标签；启动对账 | `agent24-agent/src/lib.rs`（新内部入口、`execute` 按 origin 打标签）、`agent24-store/src/source_policy.rs`（新增构造 `SourceRef::inbound_external` 或等价）、`agent24d/src/inbound/service.rs`（新）、`serve_steps.rs` | L | 2a、2b | ① explicit：恰好 1 个 run，`inbound_runs` 有行。② 该 run 的 `run_source_tags` 为 **Unknown**（变异：把 execute 改回 `user_input`，测试变红）。③ `/api/v1/peer/runs` 对该 run 隐藏内容（peer 视图见 `peer_runs.rs`）。④ 该 run 的外发被限制（沿用现有 source policy 外发门，用一个会外发的工具调用验证被拦）。⑤ **对同一行直接调用两次派发**（绕开插入去重，直接调 `dispatch(event_id)`）：run 恰好 1 个（变异：去掉 `inbound_runs.event_id UNIQUE` 或事务，测试变红）。⑥ 在「run 已落库、`run_state` 仍 `starting`」时重启：对账补成 `started`，不起第二个 run。⑦ 在「`starting`、无 `inbound_runs`」时重启：`interrupted`，不起 run。⑧ 名单外：0 个 run。⑨ 旧消息（早于水位）：0 个 run（H5）；**用真并发**（两个 future 同时 `await` 同一 actor 的两条不同消息，如 `tokio::join!`）：恰好 1 个 run、另一条为 `ignore(rate_limited)`；变异对照：删掉部分唯一索引，测试变红。run 终结后同一 actor 的下一条可以起 run（正对照） | `execute` 改动面是内核热路径，需确认不影响普通 run；`persist_new_run`（`lib.rs:679`）是否能与关联表同事务写入**未核实**（§8） | Grok，Opus 验收 |
| X11-2d | 180 天清理后台任务 + 接线（范围按 §7-3） | `agent24d/src/inbound/maintenance.rs`（新）、`serve_steps.rs`；若扩范围，`agent24-store` 的 runs/消息清理 | S（仅入站表）/ M（含 runs 与记忆） | 2a | 可注入时钟：179 天与 181 天两行，只有 181 天的正文被清（正对照 179 天不变）；若含 runs：对应 `runs.input` 与消息快照也被清，且非入站 run 不受影响；停止信号下任务退出；启动即扫一次 | 清 run 数据会影响审计与恢复，开片前读恢复路径 | Sonnet |
| X11-2e | Hyphae 入站适配：私信经 `history inbox --as`（拉取）；群消息经 `groupchat history`（有游标参数，`groupchat.rs:25-28`）；先滤自己发的；分类并写 `category/addressed/human`；调用 `ingest`；Hyphae 来源激活水位 | `agent24d/src/inbound/hyphae_source.rs`（新）、`agent24d/src/comm_routes.rs`（只读复用，不改 comm 依赖） | L | 2c | ① 用 `agent24-comm/tests/fixtures/comm_inbound_six.json` 的六类样本：plain → 按 human 规则；kind-30078 的 f4/1 信封、receipt（ack）、query、response、canary 全部 `ignore(protocol)` 且 0 个 run（变异：把协议判断删掉，至少 3 条变红）。② 好友协议消息 zero-run。③ 群消息：@ 本机身份 → `group_mention`，未 @ → `group`；私信 → `direct`；陌生人 → `stranger`。④ 自己发的消息不入表。⑤ 重启不重放（游标持久）；daemon 离线期间积压的旧消息 → `stale`，0 个 run | Hyphae 入站 JSON 字段未核实；游标语义未核实；keystore 未解锁时的行为 | Grok，Opus 验收 |
| X11-2f | 名单、接受模式、事件列表 API。写接口（`PUT /api/v1/inbound/allowlist`、`PUT /api/v1/inbound/policy`）加入 `host_only_route`（`capabilities/middleware.rs:49`），只许 ProductHost；读接口 `GET .../allowlist`、`GET .../policy`、`GET .../events`（只回元数据 + 前 50 字，不进 WS `/events`）。CORE-2 `Operation` **拆成读、写两个** | `agent24d/src/inbound/routes.rs`（新）、`capabilities/middleware.rs`、`capabilities/operations.rs`、OpenAPI 与 `packages/api-client` 生成物 | M | 2a | 在 capabilities 模式下：FirstPartyShell 令牌（无提升头）写名单 → 拒；legacy 单令牌写名单 → 拒（`host_only_route` 的 legacy 分支，`middleware.rs:75`）；ProductHost → 通过；读接口 FirstPartyShell 可读。列表不含全文；WS `/events` 抓包无入站内容。contract-tests 覆盖 | 生成物体量大 | Grok |
| X11-2g | wechat-bridge 改只传输，并实现开发态名单种子（§3.4）。daemon 侧 `POST /api/v1/inbound/events`：**`source` 由路由固定为 `wechat`，请求体里写别的来源一律 400**；只在 legacy / 开发态挂载，产品（capabilities）模式不挂。桥：id 用 `wechat:<message_id>`，只处理 `message_state=FINISH` 且 `message_type=USER` 的消息（过滤未完成与自己发的）；**删桥内名单与 y/n 审批**；回一句「已记录」，不回发结果 | `packages/wechat-bridge/src/{bridge,agent24,config,main}.ts` 及测试；`agent24d/src/inbound/routes.rs` | M | 2c、2f | ① 桥源码无 `/api/v1/runs` 的 POST。② 名单外微信用户：daemon 记 `ignore`，0 个 run。③ 同一 `message_id` 重投：1 个 run。④ `message_state=GENERATING` 与 `BOT` 消息：不入表。⑤ 请求体带 `source:"hyphae"`：400（变异：去掉固定来源，变红）。⑥ capabilities 模式下该路由 404。⑦ 仍需 `A24_DEV_TOOL=1`。⑧ **正向格**：`A24_DEV_TOOL=1` 且种子名单含该微信 uid → 该用户 FINISH 的 USER 消息恰好 1 个 run，`run_source_tags` 为 Unknown；同条件去掉种子 → 0 个 run；capabilities 模式下种子环境变量不生效（变异：让种子在 capabilities 下也生效，变红）。⑨ **行为变化**：微信里不再能回 y/n 批准，审批只在桌面做；桥回复只有「已记录」，用例钉住 | 桥现有测试大改；行为变化需在 README 和 PR 里写明 | Sonnet + Opus 验收 |
| X11-2h | 防绕过测试与文档收口 | `agent24d/tests/inbound_*.rs`；`docs/adr/ADR-044-communication.md`（回写 T2/T3 与 X11 状态）；`COMM-HYPHAE.md`；ADR-041 D10 修订（若 §7-4 采纳推荐），**并在 ADR-041 §4「后果」把「所有新表都在 CORE-5 5f–5g 接线之后才进 main」改写为：入站表（`inbound_events` 等）经 jason 2026-11-11 豁免，因为是只新增、不改旧表的独立表，其余新表（工单等，041b）仍按原约束** | S | 2a–2g | ① 静态断言：`packages/*` 与 `agent24-comm` 源码里没有对 `/api/v1/runs` 的 POST；`/runs` 的调用方白名单仅为桌面、ACP（`acp.rs`）、MCP（`agent24-mcp`）、CLI、`wechat-bridge` 已移除（变异：在 comm 里加一处 POST，变红）。② `agent24d/src` 内 `start_run*` 的外部调用点只有 `runs.rs`、`inbound/service.rs`、调度投递。③ 桌面聊天 / ACP / MCP 起 run 后 `inbound_events` 行数不变（正对照）。④ 文档里的待办状态与代码一致，ADR-041 §4 已写入豁免与 D10 修订 | 源码扫描类测试易误报，需白名单与说明 | Sonnet |

推荐顺序：2a → 2b → 2c → 2d →（2e、2f 并行）→ 2g → 2h。2a–2d 合入后 daemon 已有可测试的完整「记录 → 准入 → 起 run」闭环，只是还没有真实来源。

整体规模估计：约 3 000–4 000 行（含测试，不含 OpenAPI 生成物）。2b 可并入 2a，合并后 7 片。

**为什么 2e 放在 `agent24d` 而不是 `agent24-comm`**：comm 的结构约束是「依赖图有白名单、禁止 HTTP 客户端 crate、`CommState` 不含 Store、`ModelRouter`」（`ADR-044-communication.md:73`，测试 `agent24-comm/tests/comm_dependency_allowlist.rs`、`comm_zero_run.rs`）；入站服务需要 Store 和 RunManager，放进 comm 就得破这条约束。所以 comm 继续只做传输，适配器和准入在 `agent24d`。2h 同时回写 ADR-044：T2 提到的「并入 comm、成为 comm 的入站适配器」改为「并入 `agent24d` 的入站模块」。

## 5. 明确不做

- 工单、授权依据、订阅（ADR-041b）；run 不加 `work_item_id`。
- `inferred` 候选的模型研判与主动技能；本轮 `inferred` 一律记 `ignore(no_rule)`。
- 好友机制；好友不进名单。
- 手机遥控：只在 `source` 取值里预留 `remote`。
- 本机语音、ACP、MCP 服务、桌面聊天：按内部输入处理，不进入站表。
- run 结果自动回发（已定不发）。
- 重做 Hyphae 身份、relay、发送链路。
- 改 `agent24-comm` 的依赖白名单。
- 跨来源身份映射。
- 桌面「来源」设置页完整界面：本里程碑只给 API，界面另开片。

## 6. 与现有体系的对齐点

- 迁移：`agent24-store/migrations` 顺序编号，从 `0034` 起；加载点 `agent24-store/src/lib.rs:319`、`:379`，守卫 `agent24d/src/migration_guard/space.rs:421`。
- 清理：`decision_log` 的置空式清理；后台任务：`memory_maintenance.rs`。
- 起 run：不新造路径，在 `agent24-agent/src/lib.rs` 的 `start_run_inner`（`:1303` 起）旁加内部入口。
- 权限：写接口走 `host_only_route`；读写拆成两个 CORE-2 `Operation`。

## 7. 需要 jason 拍板的取舍

下面四条 jason 2026-11-11 均拍板选推荐项：1 每条消息一个新会话；2 豁免，新表先上；3 连副本一起删；4 全文进 prompt、标 Unknown。

**1. 外部消息起的 run，会话怎么归属？**（已拍板：按推荐）
(a) 每个「来源 + 发送者 + 群」一个长期会话，但不写长期记忆；(b) 每条消息一个新会话。(a) 有连续感，但外部文本会在会话里一直累积，还要处理会话迁移和清理；(b) 每条独立，agent 记不住上一句。**推荐 (b)**：与 ADR-041 D10 和 041b 的「按 id 取上下文」一致，也没有迁移账；要连续对话，等 041b 的工单来做。

**2. 新表要等 CORE-5 5g 之后才进 main（ADR-041 `:117`），迁移又不可回退，豁免还是等？**（已拍板：按推荐）
选项：豁免，现在做；或者等 5g 合入后再做。入站表是全新的独立表，不改任何旧表，不碰 5f/5g 要改的内核路径；但迁移一旦发布就回不去。**推荐豁免**，理由是新表独立、不改旧表，而且推迟会让 Hyphae 入站继续没有任何准入。条件是 2a 只新增表，不得顺带改旧表（这也影响 §7-3，若选推荐则 2d 要碰旧表，需单独一片再评审）。

**3. 「180 天删正文」删到哪？**（已拍板：按推荐）
选项：(a) 只清入站表的 `text/meta`；(b) 连同这条消息触发的 run 的输入、run 消息快照、会话里的记忆一起清。只清入站表，同一段正文在 `runs.input` 里还留着，「180 天后删除」就名不副实。**推荐 (b)**。代价是要碰 runs 与记忆的清理路径，2d 变成 M 并需先读恢复路径；若不愿动旧表，可退回 (a) 并在文档里如实写「入站表正文 180 天清除，run 数据另按各自规则保留」。

**4. 外部文本进模型：全文，还是只给前 50 字、其余按 id 读？**（已拍板：按推荐）
ADR-041 D10 写的是后者（默认只给 id、前 50 字，全文按 id 分页读），但那是为工单设计的；本轮没有工单也没有「读全文」的工具，照做 agent 就只能看到 50 字。**推荐全文进 prompt**，整段标 `Unknown`，上限 4 000 字，并同时修订 D10。外部文本只是输入，不改权限，这点两种做法一样。

## 8. 未核实项

已核实（评审后）：run 入口无条件打 `user_input` 标签（`agent24-agent/src/lib.rs:2211-2215`）；`start_run_then` 回调同步、随后立即 spawn `execute`（`:1340-1350`）；`host_only_route` 目前只含 comm 身份导入导出与 component report（`middleware.rs:49-54`）；MCP 服务通过 `POST /api/v1/runs` 起 run（`agent24-mcp/src/server.rs:202`）；`comm_inbound_six.json` 含 f4/1 信封、ack、query、response、canary；微信 `message_id`、`message_state`、`message_type` 字段存在（`ilink/types.ts`）。

仍未核实：

1. **②③④ 的划分**是否与 jason 一致。
2. **MCP 服务按内部输入处理**：是我按 jason 对 ACP 的口径类推，待确认。
3. **Hyphae 入站 JSON 字段**（发送者、是否 @、群 id、`event_id`）：没找到 `agent inbox` / `history inbox` 的输出契约；`COMM-HYPHAE.md`、`docs/comm/JOINT-ROUND1.md` 未读；`groupchat history` 游标的具体语义只看到文件头的错误映射描述。
4. **读位置**是否由 Hyphae 自带；没有则需 `inbound_cursor` 表。
5. **`persist_new_run`（`lib.rs:679`）能否与 `inbound_runs` 在同一事务写入**：只读了入口，没读 `insert_run` 的事务边界；2c 开工前先读。
6. **`execute` 对 origin 的打标签改动**对恢复、重放路径（`plan_resume` 等）的影响未读。
7. **peer 视图与外发门**对 `Unknown` run 的现有行为：`peer_runs.rs` 有测试但本文没逐条读，2c 验收 ③④ 依赖它们。
8. **wechat-bridge 令牌**：开发态继续用 legacy 令牌（C9 未排期）。
9. **语音**：本仓库没找到「听到话就起 run」的代码，写的是「没找到」，不是「确认没有」。
10. **§7-3 若选 (b)**：`runs.input`、`run_messages` 快照、记忆写入各处的清理方式未读。
11. **水位最大时延、速率上限的默认值**是占位，2c 定。
12. `runs` 是否有会破坏 `inbound_runs` 外键的删除路径：用了 `ON DELETE CASCADE`，实际删除路径未查。
13. 迁移编号 `0034` 以开工时 main 为准。
