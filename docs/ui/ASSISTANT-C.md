# 助理 C 期：主动消息的数据层（M6 C1）

依据：拆分提纲 `2026-11-11-m6-admission-proactive-split-outline.md` 的 C1 行和 §5 T-1、T-2（PM 11-11 派单）。本片只在桌面本地做，不依赖内核。

前提（David 11-09/11-10）：Agent24 没有存量用户数据。v2 文件不读、不迁；装上这一版后，C1 之前的对话不再显示。

裁决记录：
- 发起方怎么表示：`~/.claude/decisions/2026-11-11-m6c1-initiator-field/`（R0 一致：新增独立字段）
- 待看的单位：`~/.claude/decisions/2026-11-11-m6c1-unseen-unit/`（R0 不一致，小档按证据自裁：按事项计）
- 口径来源：T-1（角标按对话数，横幅按未看条数，分开算）、T-2（要人处理的事放桌面：各对话的 `pending[]` 加待看；入站处置记录归内核）

## 1. 文件格式

跟 A 期一样，每条对话一个文件 `<userData>/chat-history/<id>.json`。原子替换写入，文件 0600、目录 0700。

- `version`：**3**。版本不是 3 的文件不读（v1、v2 都算），`list` 跳过它，`get` 返回 null，文件本身原样留在磁盘上。
- 新增两个必填字段（类型在 `apps/desktop/src/shared/chat-history.ts`），缺一个就整个文件不读：

```ts
initiator: 'user' | 'schedule' | 'assistant'
unseen: string[]   // 未看的 run_id，最早的在前
```

- `unseen` 里每一项都必须是这条对话 `runs` 里的 `run_id`，不能重复，所以最多 1000 项。用户自己发起的对话（`initiator = 'user'`）里 `unseen` 必须为空。不满足这些条件的文件按损坏处理，不读。
- 摘要（`ChatHistorySummary`）原样带上 `initiator` 和 `unseen`。

## 2. 发起方 `initiator`

| 值 | 谁发起 | 在哪里定 |
|---|---|---|
| `user` | 用户在主对话里自己开的 | 页面第一次 `save`、文件里还没有这条对话时，由 main 记为 `user` |
| `schedule` | 日程的归属对话（A5） | `schedule-conversations.ts` 建对话时传给 `create()` |
| `assistant` | 「助理发起」对话（A6，没有对话认领的 run 的审批） | `assistant-approvals.ts` 建对话时传给 `create()` |

- 只有 main 能写。`save` 保留文件里已有的值；页面送来什么都不认。
- 定下以后不再变：用户改名（`title_source` 变成 `user`）、对话里出现 task、页面保存，都不会改它。这也是没有复用 `title_source`、也没有从 `runs` 的 kind 去推断的原因（见裁决记录）。
- `create()` 只接受 `schedule` 或 `assistant`：main 自己建的对话不会是用户发起的，用户的对话从页面的 `save` 开始。
- C7 的入站来源（微信、Nostr）到那一片再加值，本片不预留。

## 3. 待看 `unseen`：按事项计

**单位是事项，也就是 run**，以 `run_id` 去重。横幅「助理有话说 (n)」里的 n，是未归档的非用户发起对话的 `unseen` 项数之和（`unseenCount()`，shared；归档与否由调用方筛，C2 定为只算未归档的，见 §7）。

为什么按事项，不按对话、也不按消息：
- 这两类对话里，用户看到的东西就是 run，一个 run 一张卡片（TaskCard）。这两类对话的 `messages` 一开始是空的，main 也只往里追加 run；按消息数，n 永远是 0。
- 对话满 1000 个 run 后会换一条新对话。按对话数，同一个日程换对话前后会从 1 变成 2，数字的含义不稳定。
- 一条对话里可以积累很多 run，「有几条对话」说不出「有几件事」。按事项记，以后想按对话折叠显示也能从数据算出来；反过来按对话记，就算不回事项数了。
- 代价：高频日程（比如每 5 分钟一次）一夜不看，n 可能上百。数据照实记，**怎么显示（比如封顶成 99+）是 C2 的事**。

什么时候记进 `unseen`（都在 `appendRun` 的同一次写盘里完成）：
- 只有 `appendRun` **真正新加了一个 run**，并且这条对话的 `initiator` 不是 `user`、run 的 kind 不是 `answer`，才把它的 `run_id` 记到 `unseen` 末尾。
- 已经在的 run 再来一次（补录、重连后的 catch-up），`appendRun` 不改记录，`unseen` 也不变，所以补录是幂等的。
- answer 升格为 task（用户在对话里问出来的 run），即使发生在日程对话里，也不记：那是用户自己问的。
- 页面的 `promoteRun` 一律不记，不管文件里有没有这个 run。它只由页面经 IPC 调用，碰到的只会是用户在当前对话里问出来的 run；这个 run 可能在回答保存之前就转入等审批或后台任务，那时文件里还没有它（Codex C1 R1）。main 的模块（日程、助理发起）只调 `appendRun`。
- 对话满了、最早的 answer 让位时，它不会在 `unseen` 里（answer 从来不记）；`unseen` 始终是 `runs` 的子集。
- 对话已满、全是 task 或日程 run 时，`appendRun` 照旧报错，`unseen` 不变；日程和「助理发起」模块会换一条新对话，新对话的 `initiator` 跟原来一样。

## 4. 打开即清零

- 新 API：`markSeen(id)`（`createChatHistoryStore` 的方法），IPC 通道 `chat-history:mark-seen`，preload 上是 `chatHistory.markSeen(id)`。
  - 清空 `unseen`，`runs`、`messages`、`updated_at` 都不动，原子写盘，并通知页面（跟 `appendRun` 一样走 change listener）。
  - 本来就没有待看的，不写盘，也不通知。
  - 对话不在磁盘上时返回 null，不新建；id 不合法时报错。
- 页面在 `openSaved` 打开一条存过的对话后调用一次（`history.ts` 的 `markConversationSeen`），列表用 main 返回的摘要更新。调用失败，或者 bridge 上没有 `markSeen`，这条对话就保持未看，下次打开再试；打开过程中对话被删了，不会再出现在列表里。
- `runs` 留着，所以清零之后补录同一个 run，它也不会再变成未看。

## 5. 不计入的东西

- **observe 记录**（C7 的「观察」分组，入站事件在 observe 档位下的决定记录）不进 `unseen`，也不进「待我处理」：它们不走 `appendRun`，也不写 `pending`。
- **「待我处理」角标**仍是 A3 筛选 `mine` 的对话数（`pending` 非空，或有 task 在等审批），与 `unseen` 分开算（T-1）。`unseen` 不影响四个筛选。
- 用户自己发起的对话里的 task 不计入 n。

## 6. 已知局限

- 没有归属对话的审批，第一次到达时如果没能路由（daemon 暂时答不上来是谁的 run），要等下一次连上 `/events` 补录时才进「助理发起」对话，那时才计入 `unseen`。
- 去重只在一条对话内有效。跨对话不重复记，靠的是两个模块各自的 handled 记录；「助理发起」的 handled 只保留最近 5000 个（`assistant-approvals.ts` `MAX_HANDLED`），更早的 run 再来时，靠 `appendRun` 在原对话里只记一次。
- 对话正开着时新来的 run：C2 起，主对话页正显示这条对话、并且窗口有焦点时当场清零（§7）。「有焦点」只是「在看」的近似：人离开了窗口还在前台，也会清零；`markSeen` 一次清掉整条对话，不看卡片有没有滚进视口。
- main 建的对话被用户删了，页面又用同一个 id 保存，会被记成 `user` 发起。这跟 `runs`、`pending` 现有的行为一致。

## 7. 两个入口（M6 C2）

裁决记录：`~/.claude/decisions/2026-11-11-m6c2-banner-semantics/`。Q4 是 R0 一致；Q1–Q3 不一致，按小档依证据自裁。

代码：`renderer/pages/chat/proactive.ts`、`ProactiveBanner.tsx`，以及 `renderer/components/Banner.tsx`。

### 7.1 主对话顶部的横幅「助理有话说 (n)」
- 位置：放在主对话页（`page === 'assistant'`）里，在 ChatPage 上方。n 为 0 时不显示。
- n：未归档、非用户发起的对话里 `unseen` 的总数（`bannerCount`）。已归档的不算：归档是用户自己收起来的，和列表四个筛选的计数一致（Q1）。归档期间 `unseen` 照记，恢复以后就重新计入。
- 写法：99 以内照实写，超过 99 写成「99+」。可访问名称（aria-label）里带精确数字（Q3）。
- 点一下：打开「最早出现未看事项」的那条对话。比较的是每条对话里还在 `unseen` 中的那些 run，取它们在 `tasks` 里的 `started_at`；时间相同就选 id 小的（`bannerTarget`，Q2）。打开以后照 §4 清零，n 跟着变小；横幅还在的话，再点就去下一条。
- 横幅是一个按钮，键盘能聚焦，回车或空格可以触发。回复还在进行时（页面不能切换对话），横幅照样显示，但按钮是禁用的。

### 7.2 侧栏「助理」上的「待我处理」角标
- 数字是列表筛选「待我处理」（A3 的 `mine`）下的对话数，只算未归档的，和列表自己的计数一致（T-1，`mineCount` / `useMineCount`）。`unseen` 不计入。
- 不再每 5 秒轮询 `GET /api/v1/approvals?status=pending`。没有对话认领的审批，会由 A6 放进「助理发起」对话，那条对话里的 task 处于等审批状态，就落进了 `mine`。
- 离线时：run 的状态沿用上一次问到的（`run-status.ts`）。对话列表是本机文件，不受影响。还没问到状态的 task，处理方式和列表一样。

### 7.3 正在看的对话（Q4）
- 三个条件同时成立就清零（调 `markConversationSeen`）：主对话页正在显示、ChatPage 打开的是这条对话、窗口有焦点（`document.hasFocus()`，用 `focus`、`blur` 事件更新）。
- 三个时机都会重新检查：main 推来这条对话有新的未看；从别的页面切回主对话页；窗口重新获得焦点。
- 所以点横幅时，如果目标就是已经打开的那条对话（`openSaved` 会提前返回，不再调 §4 的清零），也由这里来清零。
- 已知近似：见 §6。

