# K1 第二次发布（#1280）真机验收用例

- **对象**：K1 第二次发布（#1280，按 #1293 从新的 ab/kernel head 重切）对外行为的变化，即发布 PR 正文「对外行为的变化」一节的 5 条。
- **怎么跑**：在发布 head 的 checkout 里跑 `scripts/k1-r2-accept.sh --old <main 的二进制目录> --new <发布 head 的二进制目录> [--work <目录>]`，U3 以这个 checkout 里的迁移为准。两个目录里都要有 `agent24d` 和 `agent24`（`cargo build -p agent24d -p agent24-cli`）。脚本逐条打出 PASS/FAIL，并在工作目录写 `report.md`；有一条 FAIL 就以非 0 退出。
- **按用户的操作走**：发布这边，有产品入口的都走产品入口，也就是用户用 CLI、外部工具用 MCP 和 ACP 时走的路：
  - `agent24` CLI：`daemon start/status/stop`、`schedules list/run-now`、`runs list/show`、`os install/uninstall/list/disable`；
  - MCP（`agent24 mcp`）：`agent24_run`、`agent24_list_runs`；
  - ACP（`agent24 acp`）：`session/new`、`session/prompt`。
- **没有产品入口的才直接调 HTTP**：
  - 授予（`/api/v1/module-consents`）：桌面端和 CLI 都没有授予的入口；
  - peer 视图（`/api/v1/peer/runs`）：这是 MCP 转给外部工具的原始视图，直接读它才能逐个字段检查扣下了什么。
- **main 这边照旧**：main 的 `agent24` 没有 `schedules`、`runs`，造旧数据用 HTTP；D1 直接起 main 的 `agent24d` 读它的日志，因为 main 的 `daemon start` 不说启动失败的原因。
- **只有两处直接读写库**：
  - 造旧数据：main 没有写授予、写 self-wake 的产品入口，旧授予和旧 self-wake 只能写进库里；
  - 读迁移结果：`_sqlx_migrations` 和 `schedule_source_origin` 没有对外接口。
- **不碰用户数据**：每个 daemon 都用工作目录下自己的 `AGENT24_DATA_DIR` 和 `A24_OS_PACKAGES`。Z1 检查 `~/.agent24` 在运行期间没有变化。
- **前提**：先停掉自己的 daemon（`agent24 daemon stop`），不然 Z1 会把它正常的写入当成被碰过；脚本发现 daemon 在跑会直接拒绝运行，这一步在创建任何东西之前。`--work` 不能在 `~/.agent24` 下面：两边都先解析到最终路径（包括悬空的链接和还不存在的子路径），再从 `~/.agent24` 最近一个已存在的上级目录（它本身存在时就是它）比起：`--work` 的已存在上级目录中，凡与它是同一个 inode 的（firmlink、bind mount 会给同一个目录另起名字），就把余下的路径和 `~/.agent24` 缺的那几段不分大小写比较（大小写不敏感的卷上 `~/.Agent24` 就是 `~/.agent24`）。这些都在创建任何东西之前做。`--work` 要么不存在，要么为空，免得上一次的残留混进结果。
- **环境变量**：调用者 shell 里的 `A24_*`、`AGENT24_*` 全部清掉，daemon 和 CLI 只拿到各条用例自己设的值。
- **不需要模型**：没有模型时 run 以 `provider_unavailable` 结束，来源标签在调模型之前就写好了，所以 peer 视图的判定照样成立。
- **依赖**：curl、jq、sqlite3、python3（MCP、ACP 的对话）、perl（给 CLI 限时）、node（P1 要起示例模块 `examples/node-module`）。脚本开头设 `LC_ALL=C`：系统里没装调用者 shell 设的 locale 时，perl 每次启动都会打警告；`tr`、`grep` 也按字节处理。
- **读应答**：每次请求前先删掉上一次的应答文件，请求没拿到应答时不会误读上一条；判定「被扣下」时先要求 200。

## 准备：一份 main 写的旧库

用 main 的 `agent24d` 起一次，经 API 建一个用户计划（`sch_…`）和一个 run，停掉后往库里补三样东西：

- 一个升级前的 self-wake：复制那个用户计划，id 改成 `wake_01ACCEPTOLD`；
- 一个已经删掉的 self-wake 留下的 run：schedule_id 为 `wake_01ACCEPTGONE`；
- 两条按 0015–0018 写的授予（node-ref/echo、documents/find），没有制品身份。

然后把这份库复制一份，交给发布 head 打开。

## 用例

| ID | 对应 | 做什么 | 期望 |
|---|---|---|---|
| U1 | 升级 | 用发布 head 的 `agent24 daemon start` 打开 main 的库 | 说 started，正常就绪 |
| U2 | 升级 | `agent24 daemon status` | 退出 0，说 running |
| U3 | 升级 | 读 `_sqlx_migrations` | store 库补上这个 checkout 里比 main 新的全部迁移（至少 19–26，连续），memory 库到这里最新的那一个（至少 18） |
| U4 | 升级 | `agent24 schedules list`、`agent24 runs show <旧 run>` | 原来的计划、self-wake 和 run 都在 |
| G1 | 第 1 条 | `GET /api/v1/module-consents` | 旧授予是 `stale`（#1290 已由 #1317 修好：列表按派发门的口径判状态） |
| G2 | 第 2 条 | 不设开关时 `POST …/modules/node-ref/tools/echo` | 403 `grants_closed` |
| G3 | 第 2 条 | `A24_MODULE_TOOL_GRANTS=1`、`A24_MODULE_GRANT_MAX_DAYS=90` 再 POST | 不再是 `grants_closed` |
| G4 | 第 2 条 | 开关写成 `yes`，或期限写成 91，再 `agent24 daemon start` | 起不来，并说 `agent24d did not start: <变量名>…` |
| G5 | 第 5 条 | `agent24 os install` 一个有旧授予的名字 | 退出 0，并记下这个模块的撤销 |
| P1 | 协议 | 装上没有声明协议（即协议 1）的 node-ref，起 daemon | `agent24 os list` 里它是 `[mounted]` |
| G6 | 第 5 条 | 给 node-ref 补一条更新的授予，`agent24 os disable node-ref` | 退出 0，授予变成 `revoked` |
| S1 | 第 4 条 | 读 `schedule_source_origin` | 两个 `wake_` 都是 `unknown`，用户计划没有记录 |
| S2 | 第 4 条 | `agent24 schedules run-now` 旧 self-wake 和用户计划 | self-wake 的新 run 被扣下，用户计划的 run 可见 |
| V1 | 第 3 条 | `GET /api/v1/peer/runs/{旧 run}` | `withheld: true`，`notice` 是固定提示，`prompt`、`output`、`error` 都为空（V2、S2、V3 中被扣下的 run 也按这个查） |
| V2 | 第 3、4 条 | 已删 self-wake 留下的 run | `withheld: true` |
| V3 | 第 3 条 | MCP `agent24_run` 新建一个 run（不带会话），ACP `session/prompt` 新建一个会话 run | 前者可见，MCP 的应答里也没有固定提示；后者被扣下 |
| V4 | 第 3 条 | 整个 peer 列表 | 至少 6 条；不带会话的新 run 可见；至少 4 条被扣下，每一条都按 V1 的方式查；不含被扣下的 run 的任何标记文字 |
| V5 | 第 3 条 | MCP `agent24_list_runs`（原样转出 peer 列表） | 和 V4 一样逐条查 |
| V6 | 第 3 条 | V3 里那次 ACP `session/prompt` 的应答 | 只回固定提示，不带提示词，也不带出错细节 |
| H1 | 第 5 条 | 没有 HOME 时 `agent24 os install` | 退出 0，说明授予没有撤销 |
| H2 | 第 5 条 | 没有 HOME 时 `agent24 os uninstall` | 退出 0，同样说明 |
| H3 | 第 5 条（对照） | main 在同样条件下 install | 非 0 退出 |
| D1 | 升级 | main 打开升级后的库 | 拒绝启动（`migration 19 was previously applied but is missing`），前后 `.dump` 的哈希相同 |
| D2 | 升级 | 发布 head 的 `agent24 daemon start` 再打开它 | 正常就绪 |
| Z1 | 隔离 | 运行前后的 `~/.agent24` | 没有变化 |

## 不在这里验的

- **派发时旧授予判 Stale**，以及按引用停、结束通知：要一个声明协议 4、带工具的模块，仓库里没有这样的示例包。这几项由 `consent_gate.rs`、`module_tool_runtime.rs` 的测试覆盖，CI 已跑过。
- **启用时推进纪元**：只有拿着授予的协议 4 模块才看得出效果，理由同上。
- **真实模型下的对话**：K1 的这次发布没有改模型路由。
