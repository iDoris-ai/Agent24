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

## 4. A4 接线（A2 不做）

(a) 后半句「回复结束后 run 仍在跑」，按现在的代码走不到：Knowledge 那一轮会一直轮询到终态，超过 10 分钟（`MAX_WAIT_MS`）就主动 cancel 这个 run。A2 只做了规则函数和 `promoteRun`，各自有单测。

A4 有了进度卡、能把进度显示出来以后，再把超时的处理从「cancel」改为「`promotionDue({ status, replyEnded: true })` 成立就 `promoteRun`，不 cancel」。现在就这样接，会留下一个用户看不见的后台任务。

## 5. 给 Knowledge（A7）

04b 的待确认、1b-2 的草稿待确认，在 main 里调 `setPending`、`clearPending`，写进它们所在的对话：

- `kind` 用 `confirm`；
- 有对应的 run 时带上 `run_id`；
- `id` 由调用方自己定，清除时用同一个 id。

只调这些 API，不改 `main/chat-history.ts`。
