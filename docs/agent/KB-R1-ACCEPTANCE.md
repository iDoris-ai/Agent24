# Knowledge 第一次发布（ab/weknora → main）真机验收用例

- **对象**：ab/weknora 第一次合进 main 时，用户能察觉的变化（AGENTS.md 中 #1293 的要求）。这次发布没有 Knowledge 的界面入口，用户看不到 Knowledge 功能本身。能察觉的是桌面托管 daemon 的方式，以及 daemon 对外接口的几处收紧。
- **怎么跑**：在发布 head 的 checkout 里运行下面的命令：

  ```
  python3 -I scripts/kb-r1-accept.py --old <main 的二进制目录> --new <发布 head 的二进制目录> [--work <目录>]
  ```

  - 两个二进制目录里都要有 `agent24d` 和 `agent24`，用 `cargo build -p agent24d -p agent24-cli` 编出来。
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
- **不碰用户数据**：
  - 每个 daemon 都用工作目录下自己的 `AGENT24_DATA_DIR` 和 `A24_OS_PACKAGES`。
  - 调用者 shell 里的 `A24_*`、`AGENT24_*` 变量全部清掉。
  - Z1 检查 `~/.agent24` 在运行期间没有变化。
  - `--work` 不能在 `~/.agent24` 下面，并且要么不存在、要么为空。
- **不留进程**：
  - 宿主 daemon 靠关闭 stdin 来停：父进程租约结束，daemon 自己退出。15 秒内没有退出就杀掉。
  - CLI 起的 daemon 用 `agent24 daemon stop` 停。10 秒内还在的，按状态文件里的 pid 杀掉；这个状态文件就在这次运行自己新建的数据目录里。
- **依赖**：python3，只用标准库，以 `-I` 运行。

## 用例

| ID | 对应 | 做什么 | 期望 |
|---|---|---|---|
| W1a | 桌面托管 daemon | 按桌面的参数和管道拉起发布 head 的 `agent24d` | ready 行 `auth_mode` 为 `capabilities`，带 `port` 和 `product_host_token` |
| W1b | 同上 | 带、不带宿主凭据各调一次 `GET /api/v1/models` | 带凭据 200，不带 401 |
| W1c | 同上 | 在数据目录和 daemon 日志里找宿主凭据 | 找不到。凭据只走 ready 管道 |
| W2a | run 指名 binding | 宿主 `POST /api/v1/runs`，不带 `knowledge_binding_id` | 202，`input.knowledge_binding_id` 为 null |
| W2b | 同上 | 指名一个没激活的 binding | 409 `knowledge_binding_inactive` |
| W2c | 宿主接口 | `PUT …/bindings/kb-accept?seq=1` 激活，再建 run 指名它，然后 `GET /api/v1/runs/{id}` | 激活 204，建 run 202，读回的 `knowledge_binding_id` 为 `kb-accept` |
| W2d | 同上 | 再发一次 `seq=1` | 409 `stale_sequence`：先发出、后到达的变更不能覆盖更新的 |
| W2e | 同上 | `PUT …/credential?seq=2` 推送一个随机凭据，再建 run | 推送 204，建 run 202；所有应答、数据目录和 daemon 日志里都没有这个凭据 |
| W2f | 同上 | `DELETE …/bindings/kb-accept?seq=3` 停用，再指名 | 停用 204，指名 409 `knowledge_binding_inactive` |
| W2r | 反面对照 | 用 main 的 `agent24d` 做 W2c 的激活，并指名一个没激活的 binding 建 run | 激活不是 204（main 没有这个接口），建 run 202（main 不查）。说明 W2b–W2f 的检查是这次加的 |
| W3a | 非宿主调用方 | 发布 head 的 `agent24 daemon start` 起 daemon，用它的凭据指名 binding 建 run | 403 `host_authority_required` |
| W3b | 同上 | 用同一个凭据调 `PUT …/bindings/kb-accept?seq=1` | 403 `host_authority_required` |
| W3c | 同上 | 不指名 binding 建 run | 202，普通用法不受影响 |
| W3r | 反面对照 | main 的 `agent24 daemon start`，指名 binding 建 run | 202（main 不查）。说明 W3a 的拒绝是这次加的 |
| Z1 | 不碰用户数据 | 对比运行前后的 `~/.agent24` | 没有变化 |

**整体反面对照**：把 `--new` 也指向 main 的二进制再跑一遍，W2b–W2f、W3a、W3b 应当 FAIL。其余各条照常 PASS：W1 走的 capabilities 模式在 main 上已经有了，这次的变化是桌面改用它；W2a、W3c 是不指名 binding 的普通 run；W2r、W3r 本来就是 main。2026-10-27 在本机跑过，结果与此一致。

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
- **保留路径段 `host`**：模块不能再用这个段名。仓库里没有叫这个名字的模块；由 `domain.rs` 的单元测试覆盖。
- **文档、测试、内部重构。**
