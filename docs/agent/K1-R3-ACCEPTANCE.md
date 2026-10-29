# K1 第三次发布完整用例验收

- **对象**：K1 第三次发布（ab/kernel → main）里用户能看到的变化：
  - `run list|show|cancel`、`schedule create|update|delete|run-now`、`chat --session` 三组 CLI 入口（`runs`、`schedules` 是别名；`run peers|get`、`schedule list|get` 是 main 上 #1484 已发布的）；
  - `agent24 daemon start` 起不来时说出原因；
  - 授予列表按派发门的口径判状态；
  - 工具结果带来源标签：先跑 shell_exec 的 run 不能再往外发，只读文件的不受影响；
  - 只读文件的 run 能分享给外部工具；
  - 审批停住时重启 daemon，批准后照常继续；
  - 迁移 0031/0032，以及升级后不能回退。
- **怎么跑**：在发布 head 的 checkout 里跑 `scripts/k1-r3-accept.sh --old <main 的二进制目录> --new <发布 head 的二进制目录> [--work <目录>]`。两个目录里都要有 `agent24d` 和 `agent24`（`cargo build -p agent24d -p agent24-cli`）。U2 以这个 checkout 里的迁移为准。脚本逐条打出 PASS/FAIL，并在工作目录写 `report.md`；有一条 FAIL 就以非 0 退出。
- **反面对照**：把 `--new` 换成 main 的二进制再跑一遍，本次发布带来的用例应当失败，见文末。
- **按用户的操作走**：有产品入口的都走产品入口：
  - `agent24` CLI：`daemon start/status/stop`、`run list/show/cancel/peers`、`schedule create/list/update/run-now/delete`、`consent list`、`chat --session`；
- **没有产品入口的才直接调 HTTP**：
  - 发起带工具的 run（`POST /api/v1/runs`）和建会话（`POST /api/v1/sessions`）：CLI 的 `chat` 是临时 run，不进 run 列表；MCP 的 `agent24_run` 不带会话；ACP 每次提示都新建会话，接不上旧会话；
  - run 的审批（`/api/v1/approvals`）：CLI 还没有入口（#1300；`agent24 approval` 只管模块审批），桌面端有。
- **模型**：`scripts/k1-r3-model-stub.py` 在本机回环上扮演本地档（`OMLX_URL`），按提示词里的标记照脚本发工具调用；另一份扮演远端档（`OLLAMA_URL` 指向 v4 映射的回环地址，路由器把它标成 Remote）。Z2 检查 shell 的输出没到远端档。http_fetch 的目标是 `.invalid` 域名，放行时只会在 DNS 这一步失败，不会真的连出去。
- **不碰用户数据、不留进程**：同 K1-R2。每个 daemon 用工作目录下自己的 `AGENT24_DATA_DIR`；Z1 检查 `~/.agent24` 没有变化；脚本退出时杀掉本次运行开始后出现的两边的 `agent24d serve`。`--work` 要是不含空格的 ASCII 路径。
- **依赖**：curl、jq、sqlite3、python3、perl。

## 用例

旧库由 main 的 `agent24 daemon start` 建：一个普通 run；一个会话（`POST /api/v1/sessions`，ACP 的 `session/new` 也这么建），里面一个调用 fs_read 的 run；再按旧版的写法插两条授予（`node-ref/echo`、`documents/find`）。升级用发布版的 `agent24 daemon start` 打开这份库的副本。

| ID | 验什么 | 怎么验 | 对照 main |
|---|---|---|---|
| U1 | 发布版打开 main 的库 | `agent24 daemon start` 成功 | 通过 |
| U2 | 升级后 daemon 在跑 | `agent24 daemon status` 以 `running · ` 开头 | 通过 |
| U3 | 迁移 0031、0032 应用了 | 这个 checkout 里比 main 新的 store 迁移都在 `_sqlx_migrations` 里成功，且含 31、32 | 失败 |
| U4 | 旧 run 还在 | `agent24 run list` 有它，`agent24 run show` 有它的提示词 | 失败（main 没有 `run list/show`） |
| G1 | 旧授予按派发门判 stale（#1317） | `agent24 consent list --json` 里两条都是 `stale` | 失败（`granted`） |
| L1 | main 上调用过工具的会话，升级后继续限制外发（0031） | `session_tool_result_origins` 有这个会话的 `tool:legacy`；同一会话里新 run 的 http_fetch 被 `denied by policy` | 失败（没有这张表，http_fetch 放行到 DNS） |
| L2 | 会话外的 run 不受影响 | 新 run 的 http_fetch 放行到 DNS | 通过 |
| T1 | 批准过的 shell_exec 之后，本 run 的 http_fetch 被拦 | 结果是 `denied by policy` | 失败（放行到 DNS） |
| T2 | fs_read 之后，本 run 的 http_fetch 照常放行 | 放行到 DNS（`dns lookup failed`） | 通过 |
| P1 | 只读文件的 run 能分享，跑过 shell_exec 的扣下 | `agent24 run peers --json`：T2 `withheld:false`，T1 `withheld:true`，列表里没有 shell 的输出 | 失败（两个都扣下） |
| C1 | 定时任务的写操作 | `agent24 schedule create` → `update --prompt` → `schedules run-now`（别名）→ `delete`，退出码都是 0，改动看得到，删后列表里没有 | 失败（没有这些子命令） |
| C2 | 取消等审批的 run | `agent24 run cancel` 后 run 是 `cancelled` | 失败（没有 `run cancel`） |
| C3 | `chat --session` | `agent24 chat "记住我喜欢绿茶" --session …` 打出 `· remembered`，`agent24 memory list --json` 里有这条 | 失败（不认 `--session`） |
| T3 | daemon 意外退出时等审批的 run，重启后批准照常完成 | `kill -9` 后 `daemon start`，批准，输出里有 shell 的结果 | 通过（回归检查） |
| C4 | daemon 起不来时说出原因（#1332） | `A24_MODULE_TOOL_GRANTS=yes agent24 daemon start` 打出 `agent24d did not start: A24_MODULE_TOOL_GRANTS…` | 失败（只说 `exited before ready line`） |
| Z2 | shell 的输出没到远端档 | 远端桩收到的请求里没有 T1 的 shell 输出 | 通过 |
| D1 | 不能回退 | main 的 `agent24d serve` 拒绝打开升级后的库（`migration 31 was previously applied but is missing`），库的 dump 不变 | 失败（库没升级过，main 照常打开） |
| D2 | 之后发布版照样能打开 | `agent24 daemon start` 成功 | 通过 |
| U5 | `agent24 daemon stop` 每次都停得下 | 没有一次失败或要 SIGKILL | 通过 |
| Z1 | 不碰用户数据 | `~/.agent24` 没有变化 | 通过 |

**反面对照**：`--new` 换成 main 的二进制时，上表「对照 main」一栏写失败的 11 条（U3、U4、G1、L1、T1、P1、C1、C2、C3、C4、D1）应当失败，其余 9 条通过（回归和环境检查）。

**说明**：
- T3 不是这次的新行为，main 也通过。正常的 `agent24 daemon stop` 会中止等审批的 run（审批记为 `aborted`），所以 T3 用 `kill -9` 模拟 daemon 意外退出。
- Z2 只能证明 shell 的输出没到远端。这两套构建下，路由器对这些 run 都选了本地档，远端桩一次请求都没收到，所以证明不了「本该去远端时被拦下」。这一点由 `agent24-models` 路由和 `model_callback.rs` 的测试覆盖。
- K1-R2 的脚本 `scripts/k1-r2-accept.sh` 是拿 R2 时的 main（store 停在 0026）当对照写的。现在拿 main（0030，已经带上 R2）去跑，U3、H3、S1、S2、V1、V4、V5 会因为基线变了而失败，不是回归。

## 不在这里验的

- **0032 给旧会话 run 补 Unknown 下限**：只有 daemon 没有会话记忆时才看得出差别（有会话记忆时带会话的 run 本来就扣下），产品里没有关掉会话记忆的开关。由 `agent24-store` 的迁移测试覆盖。
- **模块工具的同源回送**（6b.5f-1）和**模块模型回调一律 LocalOnly**（6b.5g）：要一个声明了工具、带授予的协议 4 模块，仓库里没有这样的示例包。由 `module_tool_runtime.rs`、`model_callback.rs` 的测试覆盖，撤门时另有 #708 的真机证据。
- **撤掉 `has_remote` 过渡门**（6b.5e）：documents 模块目前不声明工具，用户看不到差别。
- **恢复时核对审批属于哪个调用**（#1379、#1406）：异常形状的对话记录造不出来，由 `resume.rs` 的测试覆盖；正常的重启后继续由 T3 验。
