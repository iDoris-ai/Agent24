# Knowledge 第一次发布（ab/weknora → main）真机验收用例

- **对象**：ab/weknora 第一次合进 main 时，用户能察觉的变化（AGENTS.md 中 #1293 的要求）。这次发布没有 Knowledge 的界面入口，用户看不到 Knowledge 功能本身。能察觉的有两类：一是桌面托管 daemon 的方式变了，CLI 因此不能再借用或停掉桌面托管的 daemon，桌面端的文档功能也改在这种 daemon 上运行；二是 daemon 对外接口的几处收紧。
- **怎么跑**：在发布 head 的 checkout 里运行下面的命令：

  ```
  python3 -I scripts/kb-r1-accept.py --old <main 的二进制目录> --new <发布 head 的二进制目录> [--work <目录>]
  ```

  - 两个二进制目录里都要有 `agent24d`、`agent24` 和 `agent24-documents`，用 `cargo build -p agent24d -p agent24-cli -p agent24-documents` 编出来。
  - 脚本逐条打出 PASS 或 FAIL，并在工作目录里写 `report.md`。只要有一条 FAIL，就以非 0 退出。
  - 不需要模型：没有模型时，run 照样能建立，只是跑起来会失败，不影响这里的判定。
- **按用户的操作走**：
  - 桌面在 macOS 和 Linux 上托管 daemon 的方式，见 `apps/desktop/src/main/backend-manager.ts`：
    - 用 `agent24d serve --port 0 --auth-mode capabilities --host-bootstrap-stdio` 拉起 daemon；
    - stdin 和 stdout 都接管道，从 stdout 读一行 ready 记录，取其中的 `port` 和 `product_host_token`；
    - stdin 一直开着，作为父进程还活着的凭证。

    脚本按同样的参数和管道拉起 daemon，代替桌面。
  - 宿主私有接口 `/api/v1/host/knowledge/*` 只有桌面主进程会调用，没有 CLI 入口，所以直接调 HTTP，请求格式与 `apps/desktop/src/main/knowledge-host.ts` 一致。
  - 普通用户用 `agent24 daemon start` 起 daemon（legacy 模式），再按这个 daemon 的凭据调 HTTP。
  - W4 里，CLI 的 `daemon status`、`models`、`daemon stop` 都按用户的用法直接跑。
  - W4r 按 main 上桌面的方式拉起 daemon：`agent24d serve --port 0`，legacy 模式，stdin 不接。
  - W6 走桌面端文档功能的路径（documenting r6，`apps/desktop/src/main/documents-cli.ts`）。documents 包按模块 README 的方式组装：本 checkout 的 `domain-os.yml` 加上二进制目录里的 `agent24-documents`，不带读取引擎，因为 W6 只问模块能力、列文档，用不到。先 `agent24 os install`，再按桌面的方式拉起 daemon。然后像桌面那样调 `agent24 documents request --host-stdin --json`，daemon 的地址、凭据和请求都走 stdin。模块握手要一点时间，W6a 会反复问 `capabilities`，最多等 30 秒。
- **不碰用户数据**：
  - 每个 daemon 和每次 CLI 调用都用工作目录下自己的 `AGENT24_DATA_DIR`、`A24_OS_PACKAGES`，`HOME` 也指到工作目录下。
  - 调用者 shell 里以 `A24_`、`AGENT24` 开头的变量全部清掉。`AGENT24D_BIN` 也在其中，否则 CLI 拉起的会是别处的 daemon，反面对照就失真了。
  - Z1 检查 `~/.agent24` 在运行期间没有变化。
  - `--work` 不能在 `~/.agent24` 下面，并且要么不存在、要么为空。
  - `--work` 不能在另一次运行的工作目录里面。脚本在工作目录里放一个 `.kb-r1-accept` 标记，任何上级目录里有这个标记就拒绝运行，否则外层运行的 reap 会把里层运行的 daemon 当成自己的杀掉。并发运行时，各自的 `--work` 也不要互相嵌套。
  - `--work` 解析后的路径必须是可打印 ASCII，否则在创建任何东西之前就拒绝运行。原因是 reap 按 lsof 打出的工作目录认进程，而 lsof 会把其他字节转义（C locale 下连非 ASCII 字符也转义），这样的路径永远匹配不上，残留的 daemon 就会被放过。
- **不留进程**：
  - 宿主 daemon 靠关闭 stdin 来停：父进程租约结束，daemon 自己退出。legacy 方式拉起的发 SIGTERM。15 秒内没有退出就杀掉。
  - CLI 起的 daemon 用 `agent24 daemon stop` 停。
  - documents 模块由 daemon 拉起，daemon 正常停下时会一并停掉它。兜底：二进制在 `--work` 下面的进程（也就是 `os install` 装进来的模块）在 reap 时一律杀掉。
  - 兜底：`agent24 daemon start` 等不到 ready 就放弃时，daemon 不会随 CLI 退出，而且可能还没写出状态文件。所以运行开始时先记下已有的 `<--old 或 --new>/agent24d serve` 进程；每条用例结束后，以及脚本退出时，查之后新出现的这类进程，只杀工作目录落在 `--work` 下的。脚本让每个 daemon 和每次 CLI 调用都在各自的数据目录里启动，daemon 和 CLI 都不切换目录，所以这次运行起的 daemon 都在 `--work` 下面。别的运行或用户自己从同一个目录起的 daemon 不碰，读不到工作目录的也不碰，只打一行提示。工作目录在 Linux 上读 `/proc/<pid>/cwd`，在 macOS 上用 `lsof -d cwd`（macOS 的 `ps -E` 读不到别的进程的环境变量）。不按状态文件里的 pid 杀，免得误杀。
  - 每个 daemon 都在 `try` 里构造，构造中途出错也会被清理。
- **泄漏扫描**：W1c 和 W2e 在 daemon 停下之后才扫，停机时写出的内容也在范围内。只读普通文件（socket、FIFO 里没有可泄漏的字节）；读不了的普通文件不算「没找到」，检查直接判 FAIL。
- **依赖**：python3，只用标准库，以 `-I` 运行；macOS 上还要 `lsof`（系统自带）。

## 用例

| ID | 对应 | 做什么 | 期望 |
|---|---|---|---|
| W1a | 桌面托管 daemon | 按桌面的参数和管道拉起发布 head 的 `agent24d` | ready 行 `auth_mode` 为 `capabilities`，带 `port` 和 `product_host_token` |
| W1b | 同上 | 带、不带宿主凭据各调一次 `GET /api/v1/models` | 带凭据 200，不带 401 |
| W1c | 同上 | daemon 停下之后，在数据目录和 daemon 日志里找宿主凭据 | 找不到，凭据只走 ready 管道；不能有读不了的文件 |
| W2a | run 指名 binding | 宿主 `POST /api/v1/runs`，不带 `knowledge_binding_id` | 202，`input` 里有 `knowledge_binding_id` 这个键（OpenAPI 定为必填），值为 null |
| W2b | 同上 | 指名一个没激活的 binding | 409 `knowledge_binding_inactive` |
| W2c | 宿主接口 | `PUT …/bindings/kb-accept?seq=1` 激活，再建 run 指名它，然后 `GET /api/v1/runs/{id}` | 激活 204，建 run 202，读回的 `knowledge_binding_id` 为 `kb-accept` |
| W2d | 同上 | 再发一次 `seq=1` | 409 `stale_sequence`：先发出、后到达的变更不能覆盖更新的 |
| W2e | 同上 | `PUT …/credential?seq=2` 推送一个随机凭据，再建 run | 推送 204，建 run 202。从推送起的所有应答（包括 W2f 的两次）里都没有这个凭据；daemon 停下之后，数据目录和日志里也没有；不能有读不了的文件 |
| W2f | 同上 | `DELETE …/bindings/kb-accept?seq=3` 停用，再指名 | 停用 204，指名 409 `knowledge_binding_inactive` |
| W2r | 反面对照 | 用 main 的 `agent24d` 做 W2c 的激活，并指名一个没激活的 binding 建 run | 激活不是 204（main 没有这个接口），建 run 202（main 不查）。说明 W2b–W2f 的检查是这次加的 |
| W3a | 非宿主调用方 | 发布 head 的 `agent24 daemon start` 起 daemon，用它的凭据指名 binding 建 run | 403 `host_authority_required` |
| W3b | 同上 | 用同一个凭据调 `PUT …/bindings/kb-accept?seq=1` | 403 `host_authority_required` |
| W3c | 同上 | 不指名 binding 建 run | 202，普通用法不受影响 |
| W3r | 反面对照 | main 的 `agent24 daemon start`，指名 binding 建 run | 202（main 不查）。说明 W3a 的拒绝是这次加的 |
| W4a | CLI 与桌面托管的 daemon | 按 W1 的方式拉起 daemon，在同一个数据目录上跑 `agent24 daemon status` | 退出 0，说出这个 daemon 的 pid，并说 `shutdown report unavailable: host authority unavailable` |
| W4b | 同上 | `agent24 models` | 非 0 退出，说 `host authority unavailable`；daemon 还在 |
| W4c | 同上 | `agent24 daemon stop` | 非 0 退出，说 `host authority unavailable`；daemon 还在，宿主仍能调它 |
| W4r | 反面对照 | 按 main 上桌面的方式拉起 main 的 `agent24d`，再跑 main 的 `daemon status`、`models`、`daemon stop`。跑 `models` 时把 `AGENT24D_BIN` 指向一个不存在的文件：附着失败时 CLI 会另起一个临时 daemon，这样能堵住这条退路 | 都退出 0，`status` 说出这个 pid，`stop` 之后 daemon 退出。说明 W4 的变化是这次带来的 |
| W5a | 保留路径段 `host` | 用 W3 的 daemon，`POST /api/v1/attached` 注册名为 `host` 的 attached 模块；再注册一个叫 `kbacceptmod` 的作对照 | `host` 得 400 `invalid_manifest`，消息里有 `reserved`；`kbacceptmod` 得 201 |
| W5r | 反面对照 | 用 W3r 的 main daemon 注册 `host` | 201（main 不保留这个名字） |
| W6a | 桌面端的文档功能 | 装上 documents 包，按 W1 的方式拉起发布 head 的 daemon，用宿主凭据经 `agent24 documents request --host-stdin --json` 问 `{"op":"capabilities"}` | 30 秒内退出 0，`result` 行 `ok` 为 true，状态 200 |
| W6b | 同上 | 同样的方式问 `{"op":"list"}` | 退出 0，200，`documents` 是空数组（新数据目录） |
| W6c | 同上 | 同样的请求，凭据换成一个错的 | 退出 1，401 `unauthorized`。说明 W6a、W6b 的 200 确实经过了鉴权 |
| W6r | 对照 | 按 main 上桌面的方式（legacy，W4r 那样）拉起 main 的 daemon，用 main 的 CLI 做 W6a | 同样 200。说明桌面端的文档功能在这次发布前后都能用 |
| Z1 | 不碰用户数据 | 对比运行前后的 `~/.agent24`，包括它本身是否存在、它的 mtime | 没有变化：原来不存在的，不能被建出来，空目录也不行 |

**整体反面对照**：把 `--new` 也指向 main 的二进制再跑一遍，下面这些应当 FAIL：

- W2a：main 的 run 没有 `knowledge_binding_id` 这个键；
- W2b–W2f；
- W3a、W3b、W5a。

其余各条照常 PASS：

- W1：capabilities 模式在 main 上已经有了，这次的变化是桌面改用它；
- W3c：不指名 binding 的普通 run；
- W4a–W4c：CLI 对 capabilities 模式 daemon 的处理在 main 上已经有了，这次的变化是桌面改用这个模式，由 W4r 对照出来；
- W2r、W3r、W4r、W5r、W6r：本来就是 main。
- W6a–W6c：W6 防的是回归，不是新功能。main 的 daemon 也支持 capabilities 模式，所以它在 main 上同样 PASS。这次发布让桌面改用 capabilities 模式，W6 证明文档功能在这种模式下照常可用。

2026-10-27 在本机跑过（ab/weknora 5190144f 对 main df8a9d96）：正向 21 PASS、0 FAIL；整体反面对照与上面列的一致。加上 W6 后又跑了一次，用的是 #1390 的 head df876b29 对 main bb3fb6d8：正向 25 PASS、0 FAIL；整体反面对照仍然正好是上面那 9 项 FAIL。几次运行都没有残留进程。

## 只能在桌面端人工确认的（没有 CLI 入口）

这几项是桌面主进程独有的逻辑，CLI 没有对应的入口，由发布负责人在桌面上确认，结果写进 release PR。

| ID | 做什么 | 期望 |
|---|---|---|
| D1 | 桌面已经开着，再启动一次 | 不会出现第二个窗口，已有的窗口被调到前面 |
| D2 | 先在同一个数据目录上用另一个宿主起一个 capabilities 模式的 daemon（例如另开一份桌面），再启动桌面 | 托盘显示「✕ daemon：被另一个 Agent24 占用」，桌面不再自己拉起 daemon |
| D3 | macOS 上，从没存过 Knowledge 登录的新用户首次启动桌面 | 记录是否弹出钥匙串授权。启动时 `safeStorage.isEncryptionAvailable()` 每次都会被调用，会不会弹窗没在真机上验证过 |

## 不跑用例的（用户看不到）

按 AGENTS.md 中 #1293 的规定逐项写明：

- **Provider 编排**：`agent24-knowledge` crate，KB-C1-04 各片。还没有接进生产的 Tool 注册表，用户调不到。
- **knowledge.db 及其迁移**（`rust/apps/agent24d/knowledge_migrations/0001`）：启动时还没接线（6c-4 才接），不会在磁盘上建出文件。
- **只读契约、fixtures、schema**：`protocol/knowledge/v1`。
- **渲染进程代理拒绝 `/api/v1/host/*`**：属于安全收紧，页面本来就不调这些路径；由 `apps/desktop/src/main/ipc/index.test.ts` 覆盖。
- **文档、测试、内部重构。**
