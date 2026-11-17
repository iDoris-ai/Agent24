# SIN90-UI-0 —— Sin90 桌面完整页面方案

2026-11-17。只定页面，不改代码。
jason 拍板：Sin90 随 Agent24 默认发包；桌面要有方向、节奏、任务三层。

本页只画**已经装上并且挂上**之后的样子。没装、装了没重启、停用，沿用 OSI-1 的状态卡，这里不另做一套下载或安装。

## 0. 依据

以**已经装在这台机器上的 Sin90 0.5.2** 为准，不以 Agent24 的 OpenAPI 为准。

| 事实 | 出处 |
|---|---|
| 已装包 `version: 0.5.2`，命名空间 `/api/v1/sin90`，`kernel_capabilities` 含 events、memory、approval、scheduler、models | `~/.agent24/packages/sin90/domain-os.yml` |
| 路由与 0.5.2 标签一致。标签 `v0.5.2` = `059c01cf`，工作区 `sin90-design/.wt/xarch` 的 `src/http/mod.rs`、`src/core/types.rs`、`src/core/proposal.rs` 相对该标签无 diff | 2026-11-17 在 `sin90-design` 仓库核对 |
| 路由注册 | `sin90-design` `src/http/mod.rs:148–191` |
| 旧检出 `/Users/jason/Dev/iDoris-ai/Sin90`（`8eb2d50`，清单写 0.5.0）没有 routines / reviews / reject / ai。那份不能当 0.5.2 | 同日 `git log` |
| 本机库 `~/.agent24/os/sin90/sin90.db` 迁移 1–15（含 routines、review、ai、proposal rejections）。行数：方向 1 条且 `id=sin90-triage`、`status=active`，其余业务表 0 行 | 只读 sqlite，2026-11-17 |
| daemon 在跑：`agent24d` pid 82163，`daemon.json` 端口 62959，`auth_mode: capabilities`，版本 0.6.0。`GET /api/v1/health` → 200。无令牌 `GET /api/v1/sin90/directions` → 401 `unauthorized` | 同日 curl |
| 未签名进程连 `enroll.sock` 得到 `identity_rejected`，拿不到 ProductHost 令牌。各 GET 的 200 响应体**未用真机打到**。字段形状下面都标了源码行；和真机不一致的地方写「未核实」 | 同日 |
| Agent24 OpenAPI 的 sin90 段只覆盖旧的七条路由，行号约 2695–2964，schema 约 8555–8805。任务里写的 2037–2252 对不上当前文件 | `protocol/openapi.yaml` |
| 桌面 Sin90 入口今天是占位页 | `apps/desktop/src/renderer/capabilities/CapabilityPage.tsx:6–18`，注册表 `registry.ts:17`，挂载 `App.tsx:332–333` |
| OSI-1 分支 `feat/sin90-dynamic-install` 工作区未提交，只有下载登记（`os-package-registry.ts` 等）。状态卡组件还没落地 | 同日看该 worktree |

下文文件路径，Sin90 侧都指 ` /Users/jason/Dev/auraai/sin90-design/.wt/xarch/`（与标签 v0.5.2 的上述三文件一致）。Agent24 侧指本仓库。

## 1. 现有能力

三层是方向、节奏、任务。领域（Area）在方向上面，是分组容器，不是第四层。周是节奏的时间容器。复盘和 AI 提案是横切，第一版页面带上提案的看和接受，复盘正文不进主流程。

直写要请求头 `x-sin90-actor-key`。读不要。人用人类钥匙，自动化钥匙只能提交提案、捕获、触发 AI。接受和拒绝只要人类钥匙。内核代理会剥掉 `Authorization`，不会剥 `x-sin90-actor-key`（`agent24-os-proto/src/proxy.rs:170–191`）。钥匙从 `SIN90_HUMAN_KEY` / `SIN90_AUTOMATION_KEY` 来；没设就每次进程生成并打到 stderr（`src/http/actor.rs:29–66`）。Agent24 拉起模块时只继承 `PATH HOME LANG TZ TMPDIR TMP TEMP USER`（`rust/crates/agent24-os-proto/src/launch.rs:52–55`），**不传 `SIN90_*`**。所以桌面今天没有稳定渠道拿到人类钥匙。这是 Sin90 侧缺口，不在 Agent24 里刮日志补。

错误体统一 `{error:{code,message}}`。非法状态迁移 409 `conflict`。引用了不存在的 id，外键失败映射成 404 `not_found`（`src/http/mod.rs` 开头 `map_err`）。

### 1.1 方向（Direction）

实体字段：`id, area_id?, title, status, target_window, created_at, updated_at`（`src/core/types.rs:201–212`）。
新建即 `draft`。`target_window` 是月或季，例如 `2026-08`、`2026-Q3`；种子包用字面量 `ongoing`。

状态（`src/core/transitions.rs:83–94`）：

- `draft → active | abandoned`
- `active → achieved | abandoned | paused`
- `paused → active | abandoned`
- 终态：`achieved`、`abandoned`

| 操作 | 方法 | 端点 | 请求 / 响应 | 门 |
|---|---|---|---|---|
| 列表 | GET | `/api/v1/sin90/directions` | 200 `{directions:[Direction]}` | 读 |
| 新建 | POST | `/directions` | `{title, target_window, area_id?}` → 201 Direction。事件 `direction.created` | 人类钥匙 |

**Sin90 侧缺口：** 没有改标题、改窗口、改 `area_id`、改状态的端点。状态机在代码里，HTTP 推不了。页面只展示状态，不提供「启用 / 暂停 / 达成 / 放弃」按钮。

### 1.2 节奏（Rhythm）

节奏在 0.5.2 里是两套东西，都要在这一层看见：

1. **配额 Rhythm**：几条方向之间的注意力百分比。字段 `id, status, allocations[{direction_id,pct}], created_at, updated_at`（`types.rs:246–252`）。状态 `active → adjusted | retired`，`adjusted → adjusted | retired`（`transitions.rs:215–220`）。百分比之和必须 ≤ 100。
2. **重复 Routine**：到点做的模板。字段 `id, area_id?, direction_id?, title, kind, cron, tz, target_count?, target_minutes?, status, created_at, updated_at`（`types.rs:339–352`）。`kind`：`deep_work | exercise | review | read | other`。状态 `active ↔ paused`，两者都能 → `retired`（`transitions.rs:270–275`）。`cron` 是 5 段；数字星期不接受，用 `*` 或星期名。`tz` 缺省 `UTC`。
3. **周 Week**：`id, status, iso_week, created_at, updated_at`。`iso_week` 如 `2026-W33`。状态只向前：`planning → active → reviewing → closed`（`transitions.rs:148` 一带）。只有 `planning` 和 `active` 算开着，能往里塞任务。
4. **时间块 ScheduleBlock**：`id, direction_id?, task_id?, status, planned_minutes, created_at, updated_at`。**没有「哪一天」**。状态 `planned → started → completed | skipped`，`planned` 也可以直接 `skipped`。
5. **注意力**：完成块之后，用事件回放算实际分钟。`GET /attention?start&end` 要固定宽度 `YYYY-MM-DDThh:mm:ssZ`，`start < end`。响应 `{attention:[{direction_id, direction_title, actual_min}]}`。`GET /weeks/{id}/attention` 响应 `{week_id, planned_min, actual_min, deviation_min}`。

| 操作 | 方法 | 端点 | 请求 / 响应 | 门 |
|---|---|---|---|---|
| 建配额 | POST | `/rhythms` | `{allocations:[{direction_id,pct}]}` → 201。事件 `rhythm.created` | 人类 |
| 列配额 / 读一条 | GET | `/rhythms`、`/rhythms/{id}` | `{rhythms:[]}` / 单条 | 读 |
| 建重复 | POST | `/routines` | `NewRoutine`：`title, kind, cron` 必填，其余可选 → 201。事件 `routine.created` | 人类 |
| 列 / 读 | GET | `/routines?status&area_id&direction_id`、`/routines/{id}` | `{routines:[]}` / 单条 | 读 |
| 改重复的标题、cron、时区、目标次数、目标分钟 | PATCH | `/routines/{id}` | 缺字段表示不动；`target_*` 传 `null` 表示清空。事件 `routine.updated` | 人类 |
| 暂停 / 恢复 / 退役 | POST | `/routines/{id}/transition` | `{to}`。事件 `routine.paused` / `routine.resumed` / `routine.retired` | 人类 |
| 建周 | POST | `/weeks` | `{iso_week}` → 201，状态 `planning`。事件 `week.created` | 人类 |
| 列周 | GET | `/weeks` | `{weeks:[]}` | 读 |
| 推进周 | PATCH | `/weeks/{id}` | `{to}`。事件 `week.transitioned` | 人类 |
| 周的计划 vs 实际 | GET | `/weeks/{id}/attention` | 见上 | 读 |
| 建块 | POST | `/schedule-blocks` | `{planned_minutes, direction_id?, task_id?}` → 201 `planned`。事件 `block.created` | 人类 |
| 列块 | GET | `/schedule-blocks` | `{blocks:[]}` | 读 |
| 开始 / 完成 / 跳过 | PATCH | `/schedule-blocks/{id}` | `{to}`。完成时事件里带分钟和方向标题，供注意力回放。事件 `block.transitioned` | 人类 |
| 一段时间的实际分钟 | GET | `/attention?start&end` | `{attention:[]}` | 读 |

**Sin90 侧缺口：**

- 配额不能改、不能退役。状态机有，HTTP 没有。改配额只走提案 `adjust_rhythm`。
- 时间块不能改分钟、不能改挂的方向或任务，也不能删除。
- 周不能改 `iso_week`，不能倒退。
- 块没有日期。页面用「分钟队列」表达日块，不画周历格子。等 Sin90 有日期字段再画日历。

内核调度打进来的 `POST /_a24/scheduler/fired` 是模块内部回调，桌面不调用。

### 1.3 任务（Task）

字段：`id, direction_id?, week_id?, parent_task_id?, title, status, kind, energy, est_minutes?, carried_from?, created_at, updated_at`（`types.rs:215–233`）。
新建缺省 `kind=other`、`energy=mid`、状态 `backlog`。`direction_id` 空就是收件箱。有父任务的任务当作项目，只允许一层。

状态（`transitions.rs:121` 起）：

- `backlog → planned | dropped`
- `planned → in_progress | dropped | carried_over`
- `in_progress → done | dropped | carried_over`
- 终态：`done`、`dropped`、`carried_over`

| 操作 | 方法 | 端点 | 请求 / 响应 | 门 |
|---|---|---|---|---|
| 列表 | GET | `/tasks?direction_id&area_id&status` | 200 `{tasks:[Task]}` | 读 |
| 新建 | POST | `/tasks` | `{title, direction_id?, parent_task_id?, kind?, energy?, est_minutes?}` → 201。事件 `task.created` | 人类 |
| 推进 | PATCH | `/tasks/{id}` | `{to}` → 200 Task。非法 409。事件 `task.transitioned` | 人类 |
| 随手记下 | POST | `/capture` | `{text}` → 201，一条无方向的 backlog 任务。事件同样是 `task.created` | 人类或自动化 |
| 今天 | GET | `/today` | `{must_do, deep_block, inbox, carry_over_candidates}`。`must_do` 最多 3 条有方向且未完成的任务；`deep_block` 是最早的一条 `planned` 块（不是「今天的日历块」）；`inbox` 是无方向且未完成的任务 | 读 |

**Sin90 侧缺口：**

- 不能改标题、类型、精力、预计分钟、父任务。
- 直写不能把任务放进某一周，也不能排序。这两件事只在提案里：`create_tasks`、`reorder_tasks`。
- `carried_over` 不能靠 PATCH 单独做完整结转。结转是提案 `carry_over_task`：关掉旧任务，在目标周新建一条，用 `carried_from` 连上。人在页面上点「结转到下周」要走提案再接受，不能只 PATCH。

### 1.4 横切：领域、提案、复盘、AI

领域不是三层之一。页面用它给方向分组。

| 操作 | 端点 | 说明 | 门 |
|---|---|---|---|
| 列领域 / 新建 / 归档或恢复 | GET `/areas`；POST `/areas` `{title}`；PATCH `/areas/{id}` `{to: active\|archived}` | 新建即 `active`。事件 `area.created` / `area.transitioned` | 写要人类 |
| 装五个生活领域 | POST `/packs/install` | 再装一次会再复制一套（源码写明不幂等） | 人类 |
| 列提案 / 读一条 | GET `/proposals`、`/proposals/{id}` | `{proposals:[]}` 或单条。含 `ops`、`source`、`rationale`、`status`、接受后的 `result` | 读 |
| 提交 | POST `/proposals` | 体是 `Sin90Proposal`。202 `{id, status:pending}`。同 id 同内容幂等，内容不同 409。事件 `proposal.submitted` | 人类或自动化 |
| 接受 | POST `/proposals/{id}/accept` | 200 回执 `{proposal_id, event_ids}`。非 pending 409，状态对不上 422。事件 `proposal.applied` | 人类 |
| 拒绝 | POST `/proposals/{id}/reject` | 体可空，或 `{reason}`。200 `{id, status:rejected}`。事件 `proposal.rejected` | 人类 |
| 事件账 | GET `/events?entity&entity_id&since_seq&limit` | `{events:[{seq,id,entity,entity_id,kind,from_state,to_state,payload,at}]}` | 读 |
| AI 分类 | POST `/ai/classify` | `{task_ids?}` → 202 `{run_id, capability}`。忙则 409 `ai_busy` | 人类或自动化 |
| AI 排期 | POST `/ai/propose` | `{week_id}` → 202。周没开着则 409 | 同上 |
| AI 写周复盘草稿 | POST `/ai/summarize` | `{review_id}` → 202。只接受 `weekly` 且仍是草稿 | 同上 |
| 看 AI 这一轮 | GET `/ai/runs/{run_id}` | 读 | 读 |
| AI 是否允许走执行脑 | GET/PUT `/settings/ai` | `{executive_enabled}`。写要人类。事件 `setting.changed` | 读 / 人类 |
| 复盘 | POST/GET `/reviews`，GET/PATCH `/reviews/{id}`，POST `/reviews/{id}/finalize`，GET `/review/weekly/draft?week=` | 草稿可改 `body`，定稿单向。本页主流程不做 | 写要人类 |

提案 `ops[].op` 实际有（`src/core/proposal.rs:50–146`）：`create_area`、`create_task`、`create_direction`、`transition_task`、`create_tasks`、`reorder_tasks`、`adjust_rhythm`、`carry_over_task`、`assign_task_direction`、`draft_review_body`。
OpenAPI 的 `Sin90Op` 只有中间那六个旧变体（`openapi.yaml:8623–8667`），也没有 `area_id`。桌面按 Sin90 实装解析，不按这份 OpenAPI 生成客户端。改 OpenAPI 不在这几片里。

`source`：`local_brain | executive | rule`。提交时 `status` 必须是 `pending`。

**Sin90 侧缺口（页面不补）：** 方向状态与改名；任务改名与直接入周；配额的改和退役；时间块的日期与修改；复盘不进这轮主流程。

## 2. 页面怎么摆

一个 Sin90 页，三层叠在一起，不用三个顶层标签。方向是选中的对象，节奏和任务都挂在它下面。标签页会让人以为三层彼此独立。

顶上一条「今天」，不单占一层：必做、下一块、收件箱。这是 `GET /today` 已经算好的。

领域只做分组标题。没有领域的方向放在「未归类」。

```
+------------------------------------------------------------------+
| Sin90                                                            |
| 今天                                                             |
|  必做 (最多3)          下一块 (分钟, 不是某日)    收件箱          |
|  [ ] 写方案            90 分钟 · 开始            [记下……] [收下] |
+------------------------------------------------------------------+
| 方向                          | 工作 · Q4 交付          draft    |
| 未归类                        | 窗口 2026-Q4   （状态只读）       |
|  · Q4 交付            <选中>  |----------------------------------|
| 工作                          | 节奏                             |
|  · 交付 v0.5                  |  本周 2026-W47   planning        |
|  [新建方向]                   |  [开始这周]                      |
|                               |  计划 180 分钟 · 实际 0 · 差 -180 |
|                               |  块（队列，无日期）               |
|                               |   90 分钟 · 写方案 · planned [开始]|
|                               |  [加一块]                        |
|                               |  重复                            |
|                               |   每天深度工作  active  [暂停]    |
|                               |  配额  工作 70% · 学习 30%  （只读）|
|                               |----------------------------------|
|                               | 任务                             |
|                               |  [ ] 写方案   backlog  [排进计划] |
|                               |  [新建任务]   [结转] 会先变成提案  |
+------------------------------------------------------------------+
| 待你确认的提案 (2)                                         [打开]|
+------------------------------------------------------------------+
```

空状态：

- 没有方向：一句「先定一个方向」，一个标题框和窗口框。不自动装五个领域。
- 选中方向但没有周、没有块：节奏区写「这周还没有安排」，主按钮「开一周」。
- 没有任务：任务区写「这个方向下还没有任务」。
- 收件箱空：今天条里不占大块，留输入框。
- 没有待确认提案：底栏不显示。
- 模块未挂上：不渲染上图，交给 OSI-1 状态卡。OSI-1 还没合进来时，继续现在的 `CapabilityPage` 占位（`App.tsx:332`）。

主操作（有端点的才做按钮）：

| 层 | 主操作 | 端点 |
|---|---|---|
| 今天 | 记下一条；把必做标成进行中或完成；开始下一块 | capture、PATCH task、PATCH block |
| 方向 | 新建。状态只读 | POST directions |
| 节奏 | 开周、向前推进一步、加块、开始/完成/跳过、暂停或恢复重复 | weeks、blocks、routines |
| 任务 | 新建、按合法边推进、结转（走提案） | tasks、proposals |
| 提案 | 看人话摘要、接受、拒绝 | proposals |

编辑标题：只有重复项能改。方向和任务的标题改不了，输入框不要做成能保存。

文案走页面旁的 `Sin90.i18n.ts`，`defineMessages` 让英文、中文、泰文键一致（`apps/desktop/src/renderer/i18n/index.ts:1–10`）。状态词用三语短标签，不把英文枚举直接甩给用户。

## 3. 实时更新

Sin90 把事件交给内核 `_a24/events/emit`。内核在 `GET /api/v1/events` 上推：

```json
{"v":1,"seq":12,"ts":"...","type":"module","payload":{"module":"sin90","kind":"task.transitioned","payload":{...}}}
```

`type` 是 `module`，真正的名字在 `payload.kind`（`rust/crates/agent24-protocol/src/events.rs:110–117`、`152–159`）。`seq` 是这条 WebSocket 自己的序号。断了、或者客户端落后被踢掉，v1 不重放，要靠 REST 对上（`rust/apps/agent24d/src/events.rs:22–24`；桌面日程已经按这个做，`schedule-conversations.ts:17–19`）。

桌面已经有一条到 `/api/v1/events` 的连接（`agentear-events.ts:216–232`）。`parseModuleFrame` 能解开任意模块。`main.ts:117–119` 今天只把文档模块的事件转给渲染进程，Sin90 的帧到了就被丢掉。

做法：还用这一条连接。`module === "sin90"` 时另发一个 IPC，带上 `kind` 和内层 `payload`。渲染进程不自己开 WebSocket。

页面上：

- 收到 `*.created`、`*.transitioned`、`*.updated`、`routine.paused|resumed|retired`、`proposal.submitted|applied|rejected`、`routine.fired`：重新拉当前方向的任务、块、周、提案。拉的是 REST，事件只当「该刷新了」。
- 不按事件体直接改列表。事件体经常只有 id，字段不全（例如 `task.transitioned` 只有 `task_id` 和 `to`）。
- 连接 `open` 时（含重连）整页对一次：`today`、方向列表、当前方向的任务和块、`proposals` 里 `pending` 的。用一个递增序号丢掉过期响应，日程页已经这么做（`Schedules.tsx:71–78`）。
- 人在别的页时，主进程不必记 Sin90 日志。回到本页再对一次账。

## 4. 权限与确认

内核把整个已挂载模块命名空间标成 **Everyday**，不是 Elevated（`route_policy.rs:36–45`、`271–275`）。Sin90 的读写都不要走「提升确认」对话框，也不要在代理表上标 `[E]`。

渲染进程今天经 `backendProxy` 出站，允许表里没有 `/api/v1/sin90/*`（`backend-proxy.ts:58–131`）。代理发出去的头只有 daemon 认证和可选的 `Idempotency-Key`（同文件 `266–270`），没有 `x-sin90-actor-key`。

所以：

- 读：加进允许表即可。Everyday，ProductHost 直接能打。
- 写：同样是 Everyday，可以走允许表，但主进程在转发时附上人类钥匙。钥匙不进渲染进程，不进页面状态。
- 钥匙从哪来：Sin90 侧缺口（见第 1 节）。本方案不刮 stderr，不在 Agent24 里新做一把钥匙。钥匙还没有稳定落点时，写按钮停用，旁边一句「Sin90 还没有把人类钥匙交给桌面」。读不受影响。
- 接受提案是提交状态，和别的写一样附人类钥匙。不要为每次接受再弹一个系统对话框。提案卡片本身就是确认：人看见摘要，再点接受或拒绝。
- AI 的三个 `POST /ai/*` 只产生提案，不改方向和任务。按钮文案写「生成建议」，不写「已安排」。建议进底栏，等人接受。

提案卡片给人看的句子，由桌面把 `op` 译出来：

| op | 人话 |
|---|---|
| `create_direction` | 新建方向「标题」，窗口 … |
| `create_task` / `create_tasks` | 新建任务「标题」 |
| `transition_task` | 把「任务」改成完成 / 放弃 / … |
| `assign_task_direction` | 把收件箱里的一条归到某方向 |
| `adjust_rhythm` | 把配额改成 … |
| `carry_over_task` | 把未完成的任务结到某一周 |
| `reorder_tasks` | 调整这一周的任务顺序 |
| `draft_review_body` | 改写一篇复盘草稿 |
| `create_area` | 新建领域 |

对不上的 `op` 显示原始 JSON，不要装懂。`source` 显示成「本地」「执行脑」「规则」三种。`rationale` 有则放在卡片下面。

## 5. 分片

五片。每片都能单独合进当时的集成分支，默认不超过约 300 行（不含锁文件）。超了就把测试拆出去，不把两层 UI 绑死在一片里。文案三语算在行数里，所以每片少做控件。

未安装的安装卡属于 OSI-1。这五片都不做下载。

验收都要：`pnpm` 下该页的 vitest；改了 `apps/desktop` 时按仓库门禁跑 typecheck 和 lint（见 `Agents.md`）。真机冒烟用生产包加 `--remote-debugging-port=9333`，用仓库里的 `ws` 做 CDP（做法见 `docs/demo/2026-09-30-v050-desktop-audit/README.md` 第 16 行）。冒烟用空数据或临时数据目录，不往 jason 正在用的 `~/.agent24/os/sin90` 写。

### 片 1 —— 只读骨架

已挂载时用本页换掉占位。顶上今天，左边方向，右边该方向的任务，全是读。空状态按第 2 节。三语。允许表只加 GET：`/today`、`/directions`、`/tasks`、`/areas`。

验收：vitest 覆盖空列表、三条必做、选中方向后任务过滤、未挂载时不渲染本页。CDP：打开 Sin90，看见空状态或已有方向的标题，不出现占位句「尚未设置」。

### 片 2 —— 任务主操作

记下、新建任务、按合法边显示按钮（排进计划、开始、完成、放弃）。非法边不显示。钥匙缺失时按钮停用并有那一句说明。允许表加上对应 POST/PATCH。主进程能附钥匙时才真正发出；单测用假代理，不断言真钥匙。

验收：vitest 对每个状态只渲染合法按钮；停用态不发请求。CDP：在临时目录记下一条，收件箱出现它。钥匙尚未打通时，CDP 只核对停用态，并在 PR 里写明没打到 201。

### 片 3 —— 节奏

当前方向下的周、块队列、重复项、本周计划/实际分钟、配额只读。开周、推进一步、加块、块的三个动作、重复项的暂停和恢复。

验收：vitest 用假数据显示块是队列而不是七列日历；完成一块后数字区会再请求 attention。CDP：临时目录里开一周，看见 `planning` 和「开始这周」。

### 片 4 —— 提案

底栏和抽屉。列出 `pending`。接受、拒绝。三个「生成建议」按钮（分类收件箱、为当前周排期、周复盘仅当已有草稿 id；没有复盘对象就不显示第三个）。生成之后只刷新提案列表，不把任务标成已完成。

验收：vitest 把 `carry_over_task` 译成中文句子；接受后该条离开待确认。CDP：抽屉能打开；没有 pending 时底栏不在。

### 片 5 —— 事件

主进程把 `module=sin90` 转到渲染进程。页面在事件和重连时按第 3 节对账。不新开第二条 WebSocket。

验收：vitest 喂一条 `task.transitioned` 帧，断言重新请求了任务列表，且列表内容来自第二次 GET 而不是事件体。CDP：临时目录里把一条任务标完成，页面不用手刷就换成完成。若片 2 的钥匙还没通，这条 CDP 改成「重连后今天区再请求一次」，并写明。

## 6. 请 jason 拍板

**1. 人类钥匙还没交到桌面，写操作先停用，还是先做一套传递？**

Sin90 每次启动若没有 `SIN90_HUMAN_KEY`，就现生成一把并写进日志。Agent24 启动模块时不传这个环境变量。桌面若去翻日志，重启就失效，也不该把钥匙放进渲染进程。

选项：甲，页面先只读，写按钮停用，等 Sin90 把钥匙稳定放进只有本机用户能读的文件（或由内核在代理时注入）。乙，Agent24 这轮就改模块启动，把钥匙注入进去。丙，桌面自己生成并设法写进 Sin90 进程环境。

推荐甲。注入和改启动属于 Sin90 或内核的缺口，不应混进页面片。页面把停用态做出来，Sin90 补上之后片 2 不用改布局。

**2. 节奏区用分钟队列，还是做成周一到周日的格子？**

时间块只有 `planned_minutes` 和状态，没有日期。周才是时间容器。格子会让人以为块已经落在某一天，数据对不上。

选项：甲，周的状态加一块队列，实际分钟用 attention 接口。乙，先不做块，只做周和重复项。丙，等 Sin90 给块加日期再做这一层。

推荐甲。三层都在，也不伪造日历。日期是 Sin90 侧以后的事。

**3. 复盘正文进不进这一轮？**

0.5.2 已经能建复盘、改草稿、定稿、让 AI 写周复盘。它不是方向 / 节奏 / 任务三层。放进来会变成第四块工作区，五片里放不下。

选项：甲，主流程不做复盘编辑；AI 写复盘的按钮只在「已经有一篇周复盘草稿」时出现，结果仍进提案抽屉。乙，单独一块复盘页，和三层并列。丙，连提案里的复盘摘要也不做。

推荐甲。人要确认的 AI 建议留在抽屉里。写复盘正文以后再说。

## 7. FU

- OpenAPI 的 sin90 段仍是七条旧路由，且 `Sin90Direction` 没有 `area_id`，`Sin90Op` 缺四个新变体。页面不依赖它。要不要把契约改成 0.5.2，另开任务。
- 方向不能改状态、不能改名；任务不能改名、不能直接入周；配额不能改。都在 Sin90。
- 时间块没有日期，日历视图以后再说。
- `POST /packs/install` 再调一次会复制一套领域。页面这轮不放这个按钮。
- 种子方向 `sin90-triage` 已在库里（迁移 14）。页面要不要把它从方向列表里藏起来，等用真实数据看一眼再定。这轮若它出现，就当普通方向显示。
- OSI-1 的状态卡还没落地。片 1 用「未挂载则保持今天的占位」接上，不并行做安装 UI。
- 真机 GET 的 200 体没打到（capabilities 模式，未签名进程不能 enroll）。片 1 的 CDP 是第一次对上响应形状的机会。
- 本地旧仓库 `/Users/jason/Dev/iDoris-ai/Sin90` 停在 0.5.0 一带，和已装的 0.5.2 不是同一棵树。以后对 Sin90 行为，以已装包和 `sin90-design` 的 `v0.5.2` 为准。
