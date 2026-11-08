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
- 只改已经在磁盘上的对话，不新建对话。
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
- 「确认」（`awaiting_approval` 时出现）：按 run_id 在待审批里找到这一条，取第一个不是 deny 的决定，经 main 的确认框（`decideApproval`）提交。拒绝要填理由，仍在审批页做。
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
