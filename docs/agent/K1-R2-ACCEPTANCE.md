# K1 第二次发布（#1280）真机验收用例

- **对象**：release PR #1280（`release/kernel-k1-r2-20261026`，head 27acdd72）对外行为的变化，即 PR 正文「对外行为的变化」一节的 5 条。
- **怎么跑**：`scripts/k1-r2-accept.sh --old <main 的二进制目录> --new <发布 head 的二进制目录> [--work <目录>]`。两个目录里都要有 `agent24d` 和 `agent24`（`cargo build -p agent24d -p agent24-cli`）。脚本逐条打出 PASS/FAIL，并在工作目录写 `report.md`；有一条 FAIL 就以非 0 退出。登记过的已知局限没通过时记 KNOWN，不算失败。
- **只走产品入口**：daemon 的 HTTP API、`agent24` CLI，以及 MCP（`agent24 mcp`）和 ACP（`agent24 acp`）两座桥的 stdio。只有两处直接读写库：
  - 造旧数据：main 没有写授予、写 self-wake 的产品入口，旧授予和旧 self-wake 只能写进库里；
  - 读迁移结果：`_sqlx_migrations` 和 `schedule_source_origin` 没有对外接口。
- **不碰用户数据**：每个 daemon 都用工作目录下自己的 `AGENT24_DATA_DIR` 和 `A24_OS_PACKAGES`。Z1 检查 `~/.agent24` 在运行期间没有变化。
- **前提**：先停掉自己的 daemon（`agent24 daemon stop`），不然 Z1 会把它正常的写入当成被碰过；脚本发现 daemon 在跑会直接拒绝运行。`--work` 不能在 `~/.agent24` 下面，经符号链接指过去也不行。
- **环境变量**：调用者 shell 里的 `A24_*`、`AGENT24_*` 全部清掉，daemon 和 CLI 只拿到各条用例自己设的值。
- **不需要模型**：没有模型时 run 以 `provider_unavailable` 结束，来源标签在调模型之前就写好了，所以 peer 视图的判定照样成立。
- **依赖**：curl、jq、sqlite3、python3、perl（给 MCP 和 ACP 的交互限时）、node（P1 要起示例模块 `examples/node-module`）。

## 准备：一份 main 写的旧库

用 main 的 `agent24d` 起一次，经 API 建一个用户计划（`sch_…`）和一个 run，停掉后往库里补三样东西：

- 一个升级前的 self-wake：复制那个用户计划，id 改成 `wake_01ACCEPTOLD`；
- 一个已经删掉的 self-wake 留下的 run：schedule_id 为 `wake_01ACCEPTGONE`；
- 两条按 0015–0018 写的授予（node-ref/echo、documents/find），没有制品身份。

然后把这份库复制一份，交给发布 head 打开。

## 用例

| ID | 对应 | 做什么 | 期望 |
|---|---|---|---|
| U1 | 升级 | 发布 head 的 daemon 打开 main 的库 | 正常就绪 |
| U2 | 升级 | `GET /api/v1/health` | 200 |
| U3 | 升级 | 读 `_sqlx_migrations` | store 库补上 19–26，memory 库到 18 |
| U4 | 升级 | 读计划和旧 run | 原来的计划、self-wake 和 run 都在 |
| G1 | 第 1 条 | `GET /api/v1/module-consents` | 旧授予是 `stale`。**已知局限 #1290**：列表不比对制品身份，仍显示 `granted`，但派发门照样拒绝。只有两条都显示 `granted` 时记 KNOWN，其他结果（比如授予丢了）照样记 FAIL；修好后应为 PASS |
| G2 | 第 2 条 | 不设开关时 `POST …/modules/node-ref/tools/echo` | 403 `grants_closed` |
| G3 | 第 2 条 | `A24_MODULE_TOOL_GRANTS=1`、`A24_MODULE_GRANT_MAX_DAYS=90` 再 POST | 不再是 `grants_closed` |
| G4 | 第 2 条 | 开关写成 `yes`，或期限写成 91 | daemon 拒绝启动，并点名是哪个变量 |
| G5 | 第 5 条 | `agent24 os install` 一个有旧授予的名字 | 退出 0，并记下这个模块的撤销 |
| P1 | 协议 | 装上没有声明协议（即协议 1）的 node-ref，起 daemon | `GET /api/v1/os` 里它是 `mounted` |
| G6 | 第 5 条 | 给 node-ref 补一条更新的授予，`PATCH /api/v1/os/node-ref {"enabled":false}` | 200，授予变成 `revoked` |
| S1 | 第 4 条 | 读 `schedule_source_origin` | 两个 `wake_` 都是 `unknown`，用户计划没有记录 |
| S2 | 第 4 条 | 立即触发旧 self-wake 和用户计划 | self-wake 的新 run 被扣下，用户计划的 run 可见 |
| V1 | 第 3 条 | `GET /api/v1/peer/runs/{旧 run}` | `withheld: true`，`notice` 是固定提示，`prompt`、`output`、`error` 都为空（V2、S2、V3 中被扣下的 run 也按这个查） |
| V2 | 第 3、4 条 | 已删 self-wake 留下的 run | `withheld: true` |
| V3 | 第 3 条 | 新建一个不带会话的 run 和一个会话 run | 前者可见，后者被扣下 |
| V4 | 第 3 条 | 整个 peer 列表 | 不含被扣下的 run 的任何标记文字 |
| V5 | 第 3 条 | MCP `agent24_list_runs` | 能看到可见的那个 run，看不到其余 run 的内容 |
| V6 | 第 3 条 | ACP `session/prompt`（ACP 总在会话里） | 只回固定提示，不带提示词，也不带出错细节 |
| H1 | 第 5 条 | 没有 HOME 时 `agent24 os install` | 退出 0，说明授予没有撤销 |
| H2 | 第 5 条 | 没有 HOME 时 `agent24 os uninstall` | 退出 0，同样说明 |
| H3 | 第 5 条（对照） | main 在同样条件下 install | 非 0 退出 |
| D1 | 升级 | main 打开升级后的库 | 拒绝启动（`migration 19 was previously applied but is missing`），前后 `.dump` 的哈希相同 |
| D2 | 升级 | 发布 head 再打开它 | 正常就绪 |
| Z1 | 隔离 | 运行前后的 `~/.agent24` | 没有变化 |

## 不在这里验的

- **派发时旧授予判 Stale**，以及按引用停、结束通知：要一个声明协议 4、带工具的模块，仓库里没有这样的示例包。这几项由 `consent_gate.rs`、`module_tool_runtime.rs` 的测试覆盖，CI 已跑过。
- **启用时推进纪元**：只有拿着授予的协议 4 模块才看得出效果，理由同上。
- **真实模型下的对话**：K1 的这次发布没有改模型路由。
