# 附录 D：性能（Grok 4.7，只读，基线 ab/core 4f4f49ab）

> 原始评审稿，仅去掉首段工具叙述；主报告中的修正见 CODE-REVIEW-2026-10-27.md §2。

## 发现

### P-01 High — 聊天 HTTP 在回答生成之后还要等压缩模型

位置：`rust/apps/agent24d/src/routes.rs:615-669`，`rust/crates/agent24-agent/src/lib.rs:136,188-244,774-780`，`rust/crates/agent24-agent/src/session_memory.rs:405-422`，`rust/crates/agent24-memory/src/session.rs:40-47`

问题：`POST /api/v1/chat` 在 `complete_served` 之后、`Json(ChatResponse)` 之前 `await chat_remember_turn`。尾部超过 40 条时，`compact` 再打一发 `LocalOnly` 摘要（`progress: None`，走 120 秒总超时）。这次调用发生在持有该会话 `Mutex` 期间，同会话的下一次 `context()` 要等它结束。

证据：默认策略 `max_recent: 40`，`keep_recent: 20`。`RouterSummarizer::summarize` 构造最多 3.2 万字符的转写再 `router.complete`。`remember` 在 `drop(guard)` 之前执行 `timeout_at(deadline, self.compact(sid))`，预算 `MEMORY_WRITE_BUDGET = 30s`。

估计影响：长会话大约每 20 条用户可见回复多一次本地模型往返，接口多等数秒到 30 秒。同会话的下一次发送排在这把锁后面。

建议：先返回 HTTP，压缩放到后台任务。摘要请求单独设首包/停顿超时，不要占会话锁。

工作量：M。置信度：CONFIRMED。

### P-02 High — 每轮为了 turn 序号扫完整段历史

位置：`rust/crates/agent24-agent/src/session_memory.rs:319-323,426-456`，`rust/crates/agent24-memory/src/session_log.rs:75-86`

问题：`remember` 在追加本轮之前调用 `count_user_messages`。它按 500 行一页扫描该会话的全部 `mem_events`，把每条 `message` 的 JSON 反序列化一遍，只为数用户消息个数。事件日志只追加；压缩只写 `session.summary`，不删旧消息。

证据：扫描条件是 `scope_owner + scope_session`，索引 `mem_events_session_seq` 能用，但 payload 仍要读出来。`append_turn` 用这个计数做幂等 id。

估计影响：会话寿命内线性变差。一万条历史事件时，每轮多几次范围扫描和整页 JSON 解析，大约几十到几百毫秒，并占住一条 SQLite 连接和会话锁。

建议：在库里维护 `(owner, session) → next_turn`，追加时原子加一。重试用已分配的序号，不要重数。

工作量：M。置信度：CONFIRMED。

### P-03 High — 桌面每轮重发全部历史，压缩减不掉下次 prompt

位置：`apps/desktop/src/renderer/pages/Chat.tsx:52-74,147-165`，`rust/apps/agent24d/src/routes.rs:499-542`，`rust/crates/agent24-agent/src/lib.rs:714-761`

问题：渲染进程把 `messages` 整表放进 `POST /api/v1/chat`。daemon 的 `chat_memory_prelude` 只前置召回块和暂停提示，不读取已压缩的 `SessionLog` 视图。下一轮 prompt 仍是客户端累积的全文。压缩白做在响应路径上，token 不降。

证据：`Chat.tsx` 发送 `[...messages, userMsg]`，并带上 `session_id`。`post_chat` 把 prelude 接到这份 `base_messages` 上。列表用 `messages.map` 全量渲染，每次 `messages` 变化都 `scrollIntoView({ behavior: 'smooth' })`。状态数组没有上限。

估计影响：n 轮、每轮约 1 KB 时，第 n 轮请求体和模型输入都是 O(n)。总 token 约 O(n²)。一百轮量级到 10⁵ token 的重复前缀，首响和费用按这个斜率涨。DOM 同步变长，但是次要的。

建议：桌面只送本轮用户文本。上下文由 daemon 的已压缩会话视图提供。列表虚拟化可以后做。

工作量：M。置信度：CONFIRMED。

### P-04 High — 升级后的 FTS 重建堵在 ready 之前，并且失败会每次启动重来

位置：`rust/crates/agent24-memory/migrations/0017_assertions_fts_cjk.sql:1-16`，`rust/crates/agent24-memory/src/lib.rs:316-317,365-372`，`rust/crates/agent24-memory/src/retriever.rs:55-72`，`rust/apps/agent24d/src/server.rs:1603-1606`

问题：`0017` 把 `needs_rebuild=1` 写进库。`KvStore::open` 在迁移之后、返回之前调用 `rebuild`。`open_memory_base` 在 daemon 对外 ready 之前执行。重建是一条 `BEGIN IMMEDIATE`：`DELETE` 全索引，`fetch_all` 全部断言，再逐行 `INSERT`。标记在同一事务里删除，失败会留下标记，下次启动再全量重建。

证据：`SELECT id, scope_owner, subject, predicate, object FROM mem_assertions ORDER BY id` 没有 `recorded_to IS NULL` 过滤，撤回行和现行行一起重建。写锁覆盖整个 `memory.db`。

估计影响：断言行数线性。一万条中文断言时，启动多一次全表读取加一万次 FTS 插入，WAL 在提交前胀到整次重建的大小。ready 被这段堵住。

建议：打开库后先发 ready，重建放到后台分批事务。失败保留旧索引并告警，不要在关键路径上反复全量重建。

工作量：M。置信度：CONFIRMED。

### P-05 Medium — 中文 bigram 写放大，召回用全局 OR

位置：`rust/crates/agent24-memory/src/retriever.rs:75-130,278-308`，`rust/crates/agent24-memory/src/assertion.rs:309-317,372-384`，`rust/crates/agent24-agent/src/session_memory.rs:158-161`，`rust/crates/agent24-memory/migrations/0005_assertions_fts.sql:5-9`

问题：每条断言在同一条写事务里再写一行 FTS。`cjk` 列是重叠二字组，n 个汉字大约 n−1 个 token。`unicode61` 原本把一整段汉字当成一个 token。撤回和取代只改 `mem_assertions.recorded_to`，FTS 行留下。`search_any` 把查询切成 bigram 后用 `OR` 匹配，SQL 没有 `f.scope_owner` 约束（该列是 `UNINDEXED`）。`LIMIT` 在 `bm25` 排序之后。带 `session_id` 的聊天每轮都走 `search_any`。

证据：`index_tx` 把 subject、predicate、object 和 `cjk_bigrams(...)` 一起 `INSERT`。`recall` 调用 `search_any(prompt, owner, 5)`。`0005` 写明检索时再过滤现行信念，`0017` 删掉触发器后 Rust 插入路径同样不删旧行。

估计影响：一条 100 字事实的倒排项从约 1 个变成约 99 个，FTS 内容列再存一份约 3 倍的 `cjk` 文本。20 字的中文问题大约 19 个 OR 项。语料变大后，召回从毫秒级走向整库打分，并且索引只增不减。

建议：FTS 用 `content=''` 或外部内容表，避免再存一份正文。召回先用 owner 分区，再对少量表做 bigram。撤回时删对应 FTS 行。查询改为少数内容词，而不是整句 OR。

工作量：L。置信度：CONFIRMED（SQL 与写入路径）；延迟随语料增长的具体毫秒数未在本提交量过，量级判断为 PLAUSIBLE。

### P-06 Medium — 每个模型轮次全量加载当前信念

位置：`rust/crates/agent24-agent/src/lib.rs:1650-1674,1706-1715`，`rust/crates/agent24-agent/src/session_memory.rs:221-230`，`rust/crates/agent24-memory/src/assertion.rs:387-421`，`rust/crates/agent24-memory/migrations/0004_assertions.sql:39-41`

问题：`run_loop` 最多 10 轮，每轮 `request_messages_for` 都调用 `active_ids`。后者是 `beliefs_as_of(owner)`，把该 owner 全部现行断言拉进内存。现有索引只有 `(scope_owner, subject)`。这条查询不带 subject，还要在边上过滤 `recorded_to`、`qualified` 和两个时间区间。

证据：`SELECT ... FROM mem_assertions WHERE scope_owner = ? AND valid_from <= ? ... ORDER BY subject, recorded_from, id`，然后 `fetch_all`。

估计影响：agent run 的每一轮模型调用前多一次按 owner 的历史扫描。信念和被取代行积累到几千时，每轮多一次数十毫秒级的读取和反序列化，并复制整份 `messages`。

建议：部分索引 `(scope_owner) WHERE recorded_to IS NULL AND qualified = 1`。`active_ids` 只取 id，并缓存到本 run，遗忘时失效。

工作量：S。置信度：CONFIRMED。

### P-07 Medium — 每轮 run 把会话尾部复制进 `run_messages` 再删掉

位置：`rust/crates/agent24-agent/src/lib.rs:1488-1536,1614-1622`，`rust/crates/agent24-store/src/repo.rs:1090-1116`，`rust/crates/agent24-store/migrations/0004_run_messages.sql:16-26`，`rust/crates/agent24-agent/src/session_memory.rs:17,279-283`

问题：新 run 把召回块、最多 `max_recent * 4 = 160` 条先验上下文和本轮 prompt 放进一次 `BEGIN IMMEDIATE`，逐行 `INSERT`。run 结束后再 `DELETE` 这段先验上下文。主键已经是 `(run_id, seq)`，`MAX(seq)` 不是缺索引。成本是每轮对整个 `agent24.db` 持有写锁，写入然后删除一份会话尾部。

证据：注释写明这些行是 `SessionLog` 的副本。`append_run_messages_tx` 在同一事务里对每条消息执行带相关子查询的 `INSERT`。

估计影响：160 条、每条数 KB 时，每轮多几百 KB 的 WAL 写入和一次全局写锁，大约几毫秒到几十毫秒。并发的日程、审批、审计都要等这把锁，加上 5 秒 `busy_timeout`。

建议：resume 直接读 `SessionLog`。`run_messages` 只留本 run 新产生的行。

工作量：M。置信度：CONFIRMED。

### P-08 Medium — 没有生产环境 WAL checkpoint，磁盘剩余也不在写入门上

位置：`rust/crates/agent24-store/src/lib.rs:163-171,399-405`，`rust/crates/agent24-memory/src/lib.rs:300-314`，`rust/crates/agent24-memory/migrations/0014_owner_usage_quota.sql:77-83,189-191`

问题：两个库都是 WAL、最多 5 条连接、`busy_timeout` 5 秒。全仓库只有测试钩子执行 `PRAGMA wal_checkpoint(TRUNCATE)`。生产路径依赖 SQLite 默认的 passive autocheckpoint（约 1000 页）。P-02 的长读、P-04 的重建、P-07 的写事务都会钉住读标记，checkpoint 截不断 WAL。写入门是 `mem_events` 上的行数/payload 字节触发器：默认每 owner 20 万行、256 MiB，注释写明这是猜的，而且只算 `payload`，不算 FTS、索引和 WAL。Rust 树里没有 `statvfs` / `statfs`。硬件探测是启动时一次快照，不拦记忆写入。

证据：`Store::open` 与 `KvStore::open` 只设置 `journal_mode=WAL`。`test_hooks::checkpoint` 在执行 checkpoint 后把池关掉。`0014` 的 `byte_count` 定义写明排除 scope、索引和 WAL。

估计影响：24/7 daemon（ADR-037 的目标形态）下，WAL 可以长到数倍于主库。配额内的中文写入仍会因为 FTS 和 WAL 把卷写满。写满之后是 `SQLITE_FULL`，不是提前拒绝。

建议：空闲时用单独连接做 `PASSIVE`，再在无读者时 `TRUNCATE`。写入前用缓存的可用空间做门，缓存按秒刷新，不要每条语句 `statvfs`。预算把 FTS 和 WAL 算进去。

工作量：M。置信度：CONFIRMED（没有 checkpoint、没有 statvfs、配额口径）；WAL 会涨到多大取决于读者是否常驻，这一点是 PLAUSIBLE。

### P-09 Medium — ready 之前的串行启动，并且有同步文件系统调用

位置：`rust/apps/agent24d/src/server.rs:1549,1589-1606,1762-1769,1961-1964`，`rust/apps/agent24d/src/mcp.rs:31-43,121-140`，`rust/apps/agent24d/src/domain.rs:587-591,1108-1230`，`rust/apps/agent24d/src/server.rs:307-309`

问题：非临时 daemon 在对外服务前依次做：单例锁、扫孤儿临时文件、打开并迁移 `agent24.db`、`create_dir_all(workspace)`、打开并迁移 `memory.db`（含 P-04）、串行挂载 MCP、按目录逐个 `await mount_package`。任一已启用模块声明了模型时，再加一次最多 3 秒的 `ModelCatalog::probe`。MCP 总预算 10 秒、单服务器 6 秒，注释写明这是为了卡在 CLI 15 秒 ready 期限里。`prepare_dir` 使用了 `spawn_blocking`。`create_dir_all`、孤儿扫描和 `mcp::load_config` 的 `std::fs::read_to_string` 直接跑在 async `serve` 上。

证据：`mount` 是 `for spec in specs` 里 `timeout(budget, connect).await`。`mount_all` 对每个 package `await mount_package`。

估计影响：有 MCP 或要冷启动的进程外模块时，ready 稳定多出数秒到 10 秒以上。迁移加大库 FTS 重建时更长。同步目录扫描会占住一个 Tokio worker，直到返回。

建议：迁移和水印完成即发 ready。MCP 与模块进程并行挂载，失败的降级。启动期的目录扫描放进 `spawn_blocking`。

工作量：M。置信度：CONFIRMED。

### P-10 Medium — 聊天和 run 不使用已有的流式进度超时

位置：`rust/crates/agent24-models/src/lib.rs:128-134,282-283,582-584`，`rust/apps/agent24d/src/routes.rs:532-542,609-614`，`rust/crates/agent24-agent/src/lib.rs:1714-1722,1764-1772`，`apps/desktop/src/renderer/pages/Chat.tsx:59-68`

问题：模型层已有 `Progress`（首包、停顿、总期限）和 8 MiB 响应上限。`/chat` 和 `run_loop` 都把 `progress` 设为 `None`，因此使用 120 秒的 `chat_timeout`，整段生成结束后才返回。`ModelDelta` 在完整文本到手后发一次。桌面用一次阻塞 IPC 计整个往返，没有首字。

证据：`OpenAiCompatProvider::new` 把 `chat_timeout` 设为 120 秒。`Chat.tsx` 注释写明 `/api/v1/chat` 是单次阻塞调用。

估计影响：用户在整个生成期间看不到输出。本地小模型首字本可以低于 1 秒（ADR-026 的目标），界面仍要等全文，最长 120 秒。挂起的上游会占满这一整个窗口。

建议：`/chat` 打开 `Progress`，用 WS 或分块 HTTP 把 delta 推到桌面。总超时留给无进度的连接。

工作量：M。置信度：CONFIRMED。

### P-11 Medium — 压缩失败时 `load_view` 把未折叠尾部一次读入

位置：`rust/crates/agent24-memory/src/session_log.rs:252-287`，`rust/crates/agent24-agent/src/session_memory.rs:273-291,459-463`

问题：`context` 和 `compact` 都 `fetch_all` `seq > covered_through_seq` 的全部分 `message`。压缩成功时尾部停在大约 40 条。摘要模型失败或超时时，原文保留，`covered_through_seq` 不前进。下一轮仍把变长的尾部整段反序列化进 `Vec`。`context` 返回给模型的份数被硬顶到 160 条，内存分配发生在这道顶之前。

证据：`load_view` 没有 SQL `LIMIT`。超限时只打错误日志并 `skip`。

估计影响：摘要持续失败时，每轮内存和 IO 随会话历史线性增长。硬顶只减少送进模型的条数。

建议：SQL 侧按 seq 倒序限制在硬顶条数。连续压缩失败要退避，避免每轮都再打一次注定失败的模型。

工作量：S。置信度：CONFIRMED。

### P-12 Medium — 记忆库 5 连接，进程内路径不进准入信号量

位置：`rust/crates/agent24-memory/src/lib.rs:112-120,312-322`，`rust/apps/agent24d/src/domain.rs:378-386`

问题：文件库 `max_connections = 5`。OOP 信号量是 4，用来给进程内留 1 条。进程内的召回、`count_user_messages`、`load_view`、`BEGIN IMMEDIATE` 不获取这把信号量，可以占满 5 条。OOP 先拿 permit 再等池连接，等待期间 permit 不释放。`agent24.db` 同样是 5 条连接，没有对应的准入信号量。

证据：`KvStore::open` 里 `Semaphore::new((KVSTORE_MAX_CONNECTIONS - 1) as usize)`。注释写明这是唯一的 OOP 配额。

估计影响：一次长扫描加上一条写事务，就能让 OOP 记忆调用堵到 5 秒 `busy_timeout`。进程内若再等同一个 OOP 模块，等待链会拉长。我没有在这条路径上读到已经闭合的死锁。

建议：进程内也走同一把准入，长读使用 `BEGIN DEFERRED` 并尽快释放。OOP 在借到连接之后再占 permit，或者 permit 等待要有超时。

工作量：M。置信度：CONFIRMED（连接与信号量的不对称）；互相等待成死锁是 PLAUSIBLE。

### P-13 Low — 日程每 10 秒全表读取

位置：`rust/apps/agent24d/src/server.rs:2330-2344`，`rust/crates/agent24-scheduler/src/lib.rs:651-670`，`rust/crates/agent24-store/src/module_schedules.rs:1043-1046`，`rust/crates/agent24-store/migrations/0001_initial.sql:57-67`

问题：tick 默认 10 秒。每次 `SELECT * FROM schedules ORDER BY name`，在 Rust 里解析 JSON 再比较 `next_run_at`。`schedules` 上没有 `(enabled, next_run_at)` 索引。投递表有部分索引，主表这侧没有。

证据：`list_schedules_for_tick` 的文档写明「每一行、不过滤」。

估计影响：几十行时可以忽略。模块日程堆到几千行后，每 10 秒一次全表反序列化，大约数毫秒到数十毫秒，并占一条 `agent24.db` 连接。

建议：SQL 只取 `enabled = 1 AND next_run_at <= ?`，并为它建索引。

工作量：S。置信度：CONFIRMED。

### P-14 Low — 桌面轮询叠在常驻壳上

位置：`apps/desktop/src/renderer/App.tsx:149-172`，`apps/desktop/src/renderer/pages/Approvals.tsx:68`，`Runs.tsx:43`，`Schedules.tsx:61-69`，`apps/desktop/src/renderer/pages/voice/VoicePanel.tsx:46-47,361-381`，`apps/desktop/src/main/main.ts:323`

问题：壳一直开着时，每 5 秒打 `/health`，成功后再打模块列表和 `/api/v1/models`。审批页 2 秒、Runs 3 秒、日程 5 秒另加每秒 `setNowTick` 重渲染。语音页附着 3 秒、用量 5 秒。托盘 4 秒。这些请求互相独立，没有共用一次快照。

证据：上述 `setInterval` 都在组件挂载时启动。`checkBackend` 在 health 成功后无条件再发两路 IPC。

估计影响：空闲桌面大约每 5 秒 3 次本机 HTTP/IPC。单次很轻。和 P-03 的整表重绘叠在一起时，主线程会在轮询和长列表上一起抖。

建议：健康、模型和模块列表合成一次推送或一次轮询。时钟显示用 CSS 或只更新那一个节点。

工作量：S。置信度：CONFIRMED。

### P-15 Low — CI 把最大的 crate 重复编译，锁文件里有 29 组重复主版本

位置：`.github/workflows/ci.yml:51-84,100-130`，`rust/Cargo.lock`

问题：`rust` job 超时 20 分钟，依次 `cargo fmt`、`cargo clippy --workspace --all-targets`、`cargo test --workspace`。`rust-macos` 再做一遍 clippy 和 test，超时 30 分钟。按行数最大的编译单元是 `apps/agent24d`（约 5.7 万行，其中 `server.rs` 6517、`domain.rs` 5907）、`agent24-store`（约 3.6 万）、`agent24-os-proto`（约 2.8 万）、`agent24-documents`（约 2.3 万）。`agent24d/tests` 有 17 个集成测试文件，每个是单独的测试二进制。锁文件 418 个包里有 29 个多版本：`windows-sys` 四个版本，`getrandom` / `rand` 三个，`rustix` 0.38 与 1.1，`tokio-tungstenite` / `tungstenite` 0.24 与 0.29，`syn` 2 与 3，`thiserror` 1 与 2。`sqlx`、`tokio`、`axum`、`hyper`、`reqwest`、`serde` 各只有一个版本。

证据：上述行数是本工作区源文件统计。重复版本来自解析 `Cargo.lock` 的 `name`/`version`，没有跑 `cargo tree`（避免动 target 目录）。附着握手的旧 flaky 在 `attach_listener.rs:266` 记为已按锁合并修复，不单列成现缺陷。

估计影响：Linux 上 clippy 与 test 各编一次巨型 crate，macOS 再来一轮。干净缓存时这两份 job 是墙钟主体。20 分钟上限离「再加一个大测试二进制就红」不远，但是我没有计时数据。

建议：clippy 与 test 分 job 并共享同一 `CARGO_TARGET_DIR` 缓存键。macOS 只跑会碰系统调用的测试。把 `server.rs` 里纯逻辑拆出 `agent24d` 二进制，减少每个集成测试的链接面。`cargo deny` 收紧重复的 `rustix` 和 `tungstenite`。

工作量：L。置信度：CONFIRMED（结构和重复版本）；是否经常顶满 20 分钟是 PLAUSIBLE。

### P-16 Low — 已启用的进程外模块没有进程数上限

位置：`rust/apps/agent24d/src/domain.rs:1227-1248`，`rust/crates/agent24-os-proto/src/supervise.rs`（监督循环没有信号量或最大进程数）

问题：`mount_all` 对每个启用的 package 启动一个进程，启动路径上没有进程数或文件描述符预算。停机倒是有统一的 drain 期限。

证据：包分支直接 `await mount_package`。`supervise.rs` 管的是单个子进程的重启和杀进程组。

估计影响：启用模块从几个变成几十个时，启动时间和常驻 RSS、句柄线性增加。个人默认安装通常远小于这个数。

建议：给同时就绪的模块进程一个小上限，其余保持已安装但不启动，直到真的被调用。

工作量：M。置信度：CONFIRMED（没有上限）；默认安装会打到这个上限是 PLAUSIBLE，所以严重度放在 Low。

## 建议优化顺序

1. P-01 与 P-02：聊天先返回。turn 序号改成持久计数器。压缩移出响应和会话锁。这是用户能感到的最大一块。
2. P-03 与 P-10：桌面只送本轮文本，daemon 用已压缩会话视图，并打开已有的流式进度。这样 P-01 的摘要才会真正减少下次 token。
3. P-04 与 P-05：FTS 重建离开 ready 路径。召回不要对整句中文做全局 OR。撤回要删索引行。
4. P-08：在 ADR-037 的常驻进程落地前加上 WAL checkpoint，并把 FTS/WAL 算进磁盘预算。不要在每条写入上调用 `statvfs`。
5. P-07、P-06、P-09：去掉 `run_messages` 里的会话副本，给现行信念加部分索引，把 MCP 和模块挂载从 ready 的串行段里挪走。
