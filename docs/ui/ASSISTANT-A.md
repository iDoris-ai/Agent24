# 助理 A 期：对话文件 v2 与对话的 run（M4 A2）

依据：设计稿 `2026-11-10-assistant-merge-design.md` r3 §4.2，拆分 `2026-11-10-m4-assistant-a-split.md` r1 的 A2 与 §2。跟踪 #1691，本片 #1684。

前提（David 11-09）：Agent24 没有存量用户数据。v1 文件不读、不迁；装上这一版后，旧对话不再显示。

## 1. 文件格式

每条对话一个文件：`<userData>/chat-history/<id>.json`。原子替换写入，文件 0600、目录 0700，这些都跟以前一样。

- `format`：`agent24.desktop.chat-history`
- `version`：**2**。版本不是 2 的文件不读。`list` 跳过它，`get` 返回 null，文件本身原样留在磁盘上。
- v2 的每个字段都必须有：`title_source`、`group_id`、`archived_at`、`runs`、`pending`。缺一个就整个文件不读，不再按「v1 里后加的字段」去补默认值。

新增的两个字段（类型在 `apps/desktop/src/shared/chat-history.ts`）：

```ts
runs: { run_id: string; kind: 'answer' | 'task' | 'schedule'; started_at: string }[]
pending: { id: string; kind: 'approval' | 'confirm'; run_id?: string; label: string; at: string }[]
```

- `run_id`、`pending.id` 只能由 `[A-Za-z0-9_-]` 组成，长 1–128 个字符。同一条对话里不能重复。
- 一条对话最多存 1000 个 run、100 个 pending；`label` 不能为空，最长 1000 个 UTF-16 单元。时间一律用 UTC ISO（跟 `toISOString` 的输出一样）。
- 摘要（`ChatHistorySummary`，列表用）多了两个字段：`tasks`，即 kind 不是 answer 的 run；`pending`，原样带上。answer 不出现在摘要里。

## 2. 谁写 runs / pending

`runs` 和 `pending` 只有 main 能写。跟 `group_id` 一样，页面的 `save` 不管送来什么都不认：

- `save` 保留文件里已有的 `runs`、`pending`，只做一件事：把这次带来的回复里、文件还不知道的 `run_id`，以 `answer` 记进去，`started_at` 取那条回复的时间。超过上限时，新的 answer 不记。
- 下面 4 个 API 都是 `createChatHistoryStore(dir)` 的方法，供 main 的其他模块调用：

| API | 作用 | 返回 |
|---|---|---|
| `appendRun(id, runId, kind)` | 记一个 run。已经在的 run 保持原样，只有一种例外：answer 会改成传入的 kind（task 或 schedule）。task 或 schedule 不会再变回 answer | 摘要；对话不存在时为 null |
| `promoteRun(id, runId)` | 等于 `appendRun(id, runId, 'task')`：answer 升格为 task；对话还不知道这个 run 时，直接记成 task | 同上 |
| `setPending(id, item)` | 加一条待处理；已有同 id 的就替换 | 同上 |
| `clearPending(id, pendingId)` | 去掉这一条；不在就什么也不做 | 同上 |

共同规则：
- 只改已经在磁盘上的对话，不新建对话。新建对话由页面保存，或者用 main 的 `create`（A5，见 §7）。
- 不动 `messages` 和 `updated_at`，所以不会让列表重新排序。
- 参数不合法就抛错；超过上限也抛错：`Too many chat history runs` / `Too many chat history pending items`。
- runs 满了（1000 条）时（A4，PR-Daemon #1696 L1）：要记的是 task，就去掉最早的一条 answer，给它腾地方；要记的是 answer，或者已经没有 answer 可去掉，仍然抛错。task 有进度卡，不能因为问答多了就记不上。

每次真正写盘的改动，都会告诉 `onChatHistoryChange(listener)` 登记的监听器。参数是改完后的摘要。main.ts 把它用 `chat-history:changed` 事件推给窗口，页面收到后重读一遍列表；这次读跟保存、删除之间的先后顺序，按现有规则处理。

页面只能通过 IPC 调一个：`chat-history:promote-run`，也就是 `window.agent24.chatHistory.promoteRun(id, runId)`。

## 3. 升格规则

默认一切都是「答」：kind=answer 的 run 不算「进行中」，也不出进度卡。answer 变成 task 只有两种触发：

- **(a) 观察到的状态**，由 `promotionDue({ status, replyEnded })` 判：
  - run 处在 `awaiting_approval` 就升格，不管回复有没有结束；
  - 页面已经不再等这次回复（流或轮询结束），run 还是 `queued`/`running`，也升格；
  - 其他情况都不升格。
- **(b) 用户动作**「交给助理办」（A4），直接调 `promoteRun`，不经过 `promotionDue`。

A2 已经接上的：Knowledge 那一轮（`pages/chat-knowledge.ts`）。run 建好以后，`onRunStarted` 会告诉页面它的 run_id；轮询看到 `awaiting_approval` 时，页面对本轮所在的对话调 `promoteRun`（`Chat.tsx` 的 `replyWithKnowledge`）。

## 4. 进度卡与「交给助理办」（A4）

**超时不再 cancel 还在跑的 run**（`chat-knowledge.ts`）。Knowledge 那一轮等满 10 分钟（`MAX_WAIT_MS`）时：
- 等待期间最后一次成功拿到的状态是 `queued`/`running`/`awaiting_approval` → 返回 `ongoing`，不 cancel。页面按 `promotionDue({ status, replyEnded: true })` 调 `promoteRun`，回一句「这个任务我来办，进度在下面」，下一轮换新 session（`runSession` 清空），不跟还在跑的 run 抢同一个 session。
- 10 分钟里一次状态也没拿到 → 仍然 cancel，报 `run_timeout`。看不见的 run 不留在后台。

**进度卡**（`chat/TaskCard.tsx`）。卡片是数据不是消息：不进 `messages`，也不发给模型。当前对话的每个 task（不含 answer）一张，列在对话末尾：
- 状态与用时：数据来自 `run-status.ts`，跟列表共用一份。先问一次 `GET /api/v1/runs/{id}`，之后按 `/events` 重问，不轮询。用时在本地按秒走，不去问 daemon。窗口关了再开，task 在对话文件里，卡片照样出来，状态重新问。
- daemon 里没有这个 run：显示「找不到」。
- 「确认」：只在 run 处于 `awaiting_approval`、且它的待审批提供 `approve` 时出现，经 main 的确认框（`decideApproval`）**只提交 `approve`**，也就是只批准这一次。
  - daemon 的决定是开放集合（openapi `Decision`）。`approve_for_session` 和 `approve_for_target` 会授予更大的范围，`abort` 会结束整个 run，所以这三个都不由「确认」代选（Knowledge 对 #1700 的复核）。
  - 「拒绝」（A6）：只在待审批提供 `deny` 时出现。点了以后在卡片里展开一行理由，**理由不能为空**，空的不发（SPEC-002），发的是 `{ type: 'deny', reason }`，同样经 main 的确认框。拒绝以后模型会收到理由，run 继续。
  - **已知局限（M4 不做）**：卡片只处理 `approve` 和 `deny` 两种决定。待审批两样都不提供时，卡片不显示按钮，只提示「这一项只能在终端里处理：agent24 tui」。
- 「停止」（进行中时出现）：`POST /api/v1/runs/{id}/cancel`。
- 完成的 run 显示它的回答，**跟聊天里的回答一样过一遍 `grounding`**，用的是 `GET /runs/{id}` 返回的 `knowledge`。run 有知识库记录时显示检索说明；回答里点名、但 run 没读到的来源，标出来，不算来源。

**交给助理办**。每条已发出的用户消息下面有这个按钮（已归档、正在回复时不显示）：
- 新建一个 session，把这句话作为一个独立的 run 发出去（`POST /api/v1/runs`）。
- 它跟其他 `/runs` 请求一样经过 main 的 `withKnowledgeBinding`：知识库开着就带上绑定。
- main 记下这个 task 以后卡片才出现。记不上就 cancel 这个 run，提示「没能交给助理」。

## 5. 列表的四个筛选（A3）

列表顶部有四个筛选：全部、进行中、待我处理、已完成。规则在 `renderer/pages/chat/filters.ts` 的 `filtersOf`：

- **进行中**：有 task 处在 `queued` 或 `running`。
- **待我处理**：`pending` 非空，或者有 task 在 `awaiting_approval`。
- **已完成**：`pending` 为空，并且最近一个 task 已经结束（`completed`、`failed`、`cancelled`）。「最近」按 `started_at` 算；时间相同时，取 runs 里排在后面的那一个。
- 只看 task 和 schedule 的 run，answer 不算（摘要里本来也没有 answer）。
- 一条对话可以同时属于好几个筛选。
- 状态还不知道的 task，只出现在「全部」里，其他筛选都不算它。daemon 查不到的 run 记为 `missing`，也一样只在「全部」里。

run 的状态怎么来（`renderer/pages/chat/run-status.ts`）：

- 列表上出现的每个 task，问一次 `GET /api/v1/runs/{id}`。
- daemon 的 `/events` 上来了 `run.started`、`run.completed`、`run.failed`、`run.cancelled`、`approval.required`、`approval.resolved`，并且是列表问过的 run，就再问一次。
- 事件由 main 的 `agentear-events.ts` 转发，IPC 是 `run:event`，只带 `type` 和 `run_id`。
- 不轮询。

几条约定（PM 11-09 同意）：

- 已归档的对话不算进四个筛选的计数和结果，只在「全部」里出现。
- 选了「全部」以外的筛选时，列表改成一份扁平列表；搜索词照样生效。回到「全部」，恢复分组视图。
- 筛选栏只在某条对话有 task 或 pending 时才出现。都没有时，「全部」以外的筛选都是空的。筛选栏消失时，选中的筛选回到「全部」。

## 6. 给 Knowledge（A7）

04b 的待确认、1b-2 的草稿待确认，在 main 里调 `setPending`、`clearPending`，写进它们所在的对话：

- `kind` 用 `confirm`；
- 有对应的 run 时带上 `run_id`；
- `id` 由调用方自己定，清除时用同一个 id。

只调这些 API，不改 `main/chat-history.ts`。

## 7. 日程的归属对话（A5）

每条日程有一条自己的对话，日程的 run 都记在那里（设计稿 r3 §3.4）。

- **哪些 run 是日程的**：daemon 的 `run.started` 事件带 `schedule_id`，不是日程发起的 run 为 null。日程按时触发和 `run_now` 都会发这个事件；`schedule.fired` 只有按时触发才发，所以不用它。main 在 `/events` 上听（`agentear-events.ts` 的 `parseScheduledRunFrame`），有没有窗口都记。
- **第一次**：新建一条对话，标题「日程：<日程名>」（三语，main-i18n `schedule.conversationTitle`），`title_source = 'schedule'`。名字取自 `GET /api/v1/schedules/{id}`，拿不到就用日程 id。
- **之后**：同一条日程的 run 都用 `appendRun(id, runId, 'schedule')` 记进这条对话，进度卡与 A4 相同。
- **绑定关系**存在 `<userData>/schedule-conversations.json`（`agent24.schedule-conversations` v1，最多 1000 条，满了先丢最早的）：
  - 对话已被删除，或者文件读不了：下一次 run 时新建一条。
  - 对话已归档：照常往里记，它仍是这条日程的对话。
- **记录顺序**：一次只记一条，按到达顺序。所以一个新日程的两个 run 会进同一条新对话。
- **断线补录**（Codex A5 C1、r2）：daemon 不重放事件。客户端断线，或者因为落后被 daemon 主动断开（`agent24d` events.rs），都要靠 REST 对账。
  - main 每次连上 `/events`（第一次和每次重连）都取一次 `GET /api/v1/runs`，把其中带 `schedule_id`、但**还没处理过**的 run 按创建时间从旧到新补记。
  - 「处理过」的 run 记在同一个文件的 `handled` 里：实时收到并记进对话的、补录记进去的，都算。用的是 run id，不比较时间，所以不受时钟影响。
  - 实时收到的 run 也先查 `handled`（Codex A5 r3 C3）。所以一个 run 不论走哪条路，记过一次就不会再记，用户删掉的对话也不会被重新建出来。
  - 第一次补录时还没有 `handled`：这时列表里已有的日程 run 都算处理过，不补历史。但有一种例外（Codex A5 r3 C2）：在这之前实时收到、却没记进去的 run，先存在 `retry` 里。第一次补录不把它们算作处理过，而是把它们补记进去。还有一种（PR-Daemon #1704 N1）：首次补录在等 run 列表时实时收到的 run，排在补录后面，但列表里已经有它。这样的 run 从收到那一刻起就记在内存里，首次补录同样不把它算作处理过；轮到它时照常记上。
  - 日程的对话满 1000 个 run 时（#1719 F-A），新开一条同名对话，之后的 run 记进新的那条。满的那条原样保留，它里面的 run 仍算这个日程的：A6 的 `claim` 也查它们（文件里的 `former`，每个日程留最近 10 条），所以这些 run 的审批不会在「助理发起」里再出一张卡。
  - 某一条因为别的原因记不进去（例如写盘失败），这一条跳过、记日志，其余照补；它留在「没处理过」里，下次连上再试。
  - 取不到 run 列表：这次什么都不改，下次连上再补。
  - 文件坏了、读不了：绑定和 `handled` 都从头来过（日程下一次 run 时新建对话，第一次补录不补历史）。这段时间里漏掉的 run 不再补。
  - 每次补录时，`handled` 只保留 daemon 列表里还有的 run（Codex A5 r3 C1）。它不会比 daemon 存的多，也不按条数截断，所以 daemon 还留着的旧 run 不会被挤出去、再被重记。
  - 已知局限（M4 不做）：`GET /api/v1/runs` 没有分页，也不能按日程过滤，一次返回全部 run 和它们的输入输出。main 自己发的这个请求，上限放宽到 64MB（窗口的请求仍是 8MB）。超过 64MB 时，补录每次都会失败，只记日志。要根治，得让 daemon 支持过滤和分页：内核待办 #1701（`GET /api/v1/runs` 加 `schedule_id`、`session_id` 过滤和游标分页），归内核线。
- **标题优先级**：用户改名 > main 自己起的名字（日程名 `schedule`，或者 A6「助理发起」对话的 `assistant`，两者同级）> 模型标题 > 第一句话。在这类对话里提问，标题不会被改掉；用户改名可以改。
- main 新增 `create({ title, title_source })`：新建一条没有消息的对话，并通知 `onChatHistoryChange`，打开着的窗口会把它列出来。
- `conversationOf(scheduleId)`：A6 的「主动」页（原日程页）上，每条日程有一个入口，打开它的对话（IPC `schedule:conversation`）。

## 8. 页面合并与对话外 run 的审批（A6）

- **删掉的页面**：运行页（`runs`）和审批页（`approvals`）。审批在 run 的卡片上做（§4）。
- **设置 → 活动 = 诊断，不是任务入口**。那里的 run 列表保留，用来查问题；任务都在对话里，以卡片的形式出现。总览上原来打开运行页的「账本」站点，现在打开设置 → 活动；「闸门」站点打开助理。
- **「日程」改名「主动」**（Proactive / 主动 / เชิงรุก），每条日程带一个打开它的对话的入口（§7）。
- **离页守卫**（`capabilities/providers.tsx` 的 `useLeaveGuard(dirty, message?)`，来自 Knowledge #1681 的 Low）：
  - 页面挂着时登记守卫，卸载时注销；看的是挂载，不是可见。
  - 经 `navigate`/`open` 切到另一页之前，只要有一处守卫说有未保存的修改，就弹**一次**原生确认框。文案用第一个带 message 的守卫的；都没带就用三语通用文案。用户取消就留在原页。
  - 要去的就是当前页时不问。但同一页如果要重新挂载（例如再次打开设置，key 变了），会丢掉页面上的状态，所以算换页，照样要问（Codex #1688 C1）。模块被移除时自动跳回能力页，不问。
  - 不管关窗口和退出 app。Knowledge 页的接入归 A7。
- **待审批计数**从「总览」「能力」移到侧栏的「助理」上。计的是 daemon 所有待审批，不只是当前对话的。
- **删除、清空对话时先停掉 task，停完再删**：确认框写明有几个进行中的 task 会被停掉。
  - 确认以后，对这些对话的每个 task 直接发 `POST /api/v1/runs/{id}/cancel`，一次最多 4 个。daemon 的 cancel 幂等，任何状态都能发（openapi `cancelRun`），所以不先查状态，也就不会漏掉两次查询之间换了状态的 run（Codex A6 C4）。daemon 里已经没有的 run（404）算停了。
  - 有一个停不下来（daemon 拒绝或连不上）：**什么都不删**，提示「有任务没能停下，所以什么都没删」（Codex A6 C1）。
  - 清空时，新对话要等 task 都停了才开，所以停 task 期间在新对话里问的话，不会被一起清掉（Codex A6 C2）。
  - 已知局限（M4 不做，Codex A6 C3）：在停 task 和真正删除之间，main 又往这条对话里追加了 run（例如「助理发起」对话来了新审批），这个 run 不会被停掉，卡片会随对话一起删掉。这段间隔只有几次本机请求那么长。
- **对话外 run 的审批**（PM 11-09 方案 A，`main/assistant-approvals.ts`）。审批页删掉以后，每个待审批都要在某张卡片上能处理：
  - main 在 `/events` 上听到 `approval.required`，用 `GET /api/v1/runs/{id}` 看这个 run 是谁的：
    - 带 `schedule_id`、并且 A5 已经把它记进日程的对话：卡片在那里（§7），不用管。A5 先把排在前面的 run 记完再回答（`claim`）。A5 的首次补录算作处理过的、或者没记进去的日程 run，按下面的「其余」处理（Codex A6 C5）。这时 A5 把它记成处理过，以后的补录、以及之后才到的实时记录，都不再把它记进日程对话，所以不会两处各一张卡（Codex A6 r2 C1、r3 C1）。A6 如果没能记进「助理发起」对话，就把它还给 A5（`settle`），由 A5 下次补录照常补记（Codex A6 r3 C2）。`claim` 之前就处理过的，仍算处理过。其余的：已经有 `handled` 时从里面移出，因为首次补录可能恰好在 `claim` 期间把它算了进去（#1719 C1）；还没有 `handled` 时放进 `retry`。`claim` 先写盘，写成功了才在内存里预留；写失败时什么都不预留，这个 run 仍归 A5 照常记（#1719 C2）。
    - 它的 session（`GET /api/v1/sessions/{id}`）的 channel 是 `desktop`：是窗口发起的，由页面升格（§3、§4），不用管。
    - 其余的（命令行的 `agent24`、别的 channel、没有 session、session 已经不在）：作为 task 记进一条「助理发起」对话（三语，main-i18n `assistant.conversationTitle`，`title_source = 'assistant'`）。卡片上有确认和拒绝。这条对话第一次用到时用 `create` 新建，之后都记在这里。
  - daemon 说不清（连不上、答复不对）：这个 run 不算处理过，下次补录再分。
  - **断线补录**：每次连上 `/events`，跟 §7 一样补一次，取的是 `GET /api/v1/approvals?status=pending`，把其中没处理过的 run 按上面的规则分。跟 §7 不同的是，**第一次补录也会补**：之前就在等的审批，窗口里没有别的地方可以处理。
  - 「处理过」的 run 记在 `<userData>/assistant-approvals.json`（`agent24.assistant-approvals` v1）的 `handled` 里，最多 5000 条，满了先丢最早的。一个 run 只分一次，同一个 run 的第二个审批不会再记一遍。用户删掉的「助理发起」对话，只有来了没处理过的 run 才会重新建出来。
  - 一次只分一个 run，按听到的顺序。
  - 「助理发起」对话记满 1000 个 run 时（这条对话里全是 task，没有 answer 可以腾出位置），新建一条「助理发起」对话，之后的 run 都记在新的这条（Codex A6 C6）。
  - 已知局限（M4 不做）：窗口发起的 run，如果页面已经不在等它（例如窗口在等审批时关掉又开），就没有卡片，只能在 `agent24 tui` 里处理；侧栏「助理」的计数仍然算它。
