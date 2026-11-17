# ADR-046：iDoris 由 agent24d 托管

索引：[架构总入口](../architecture/README.md)・[ADR 索引](README.md)

- 状态：**已接受**（2026-11-17 按现行代码订正行号和生命周期门。初稿 2026-11-12，S0-8，[#1823](https://github.com/iDoris-ai/Agent24/pull/1823)。记录 IL-13 与 S2-2 之后的现行代码）。
- 编号：在 [ADR 索引](README.md) 领取 ADR-046。
- 上位：[架构优化方案](../tracking/2026-11-04/ARCH-OPTIMIZATION-PLAN.md) §4.1 S0-8 的 O3、S2-2；[ADR-032](../decision.md)（iDoris 作兼容接口，不做成进程外模块）；[ADR-036](../decision.md)（硬件账归 Agent24，选型归 iDoris）；[ADR-037](../decision.md)（agent24d 是本体）。
- 关系：部分取代 [ADR-035](../decision.md)。被取代的条款见 ADR-035 文首。本篇 D3 是 Admin token 的现行交接，D6 是安装、连接、启动、停止。知识库凭据见 [ADR-KB-02](../knowledge/ADR-KB-02-HOST-AUTHORITY.md)，那一篇不取代 ADR-035。

## 1. 背景

ADR-032 把 iDoris 定成模型网关，不是进程外领域模块。ADR-035 当时的前提是「Agent24 不拥有、不拉起 iDoris」，所以 Admin session token 要由外部 launcher 分两路交给 iDoris 和 Electron main。IL-13 之后这条前提不再成立：daemon 拉起并监督 iDoris，桌面不再持有 Admin token。[#1472](https://github.com/iDoris-ai/Agent24/pull/1472)（`b3c9b10c`，2026-10-28）删掉桌面的 `TOKEN_ARG` 旧交接。S2-2（[#1790](https://github.com/iDoris-ai/Agent24/pull/1790)，`a5096360`）把托管实现从 agent24d 挪进 `agent24-idoris`，并用 `IdorisSlot` 打断 `agent24-models` 与 `agent24-idoris` 的依赖环。R1 C5（provision 绕过依赖注入读上游密钥）已由 [#1799](https://github.com/iDoris-ai/Agent24/pull/1799) 修掉，原跟踪号 FU-115。

## 2. 决策

### D1 agent24d 是托管进程的组合根，生命周期在 `agent24-idoris`

非临时 daemon 启动时构造 `ManagedIdoris`，再 `IdorisHost::system`（`rust/apps/agent24d/src/serve_steps.rs:323-350`）。构造失败时 daemon 继续起来，并把 iDoris 标成不可用（`:351-357`）。HTTP 路由留在 agent24d 的 `idoris_routes.rs`，不随 crate 下沉。这符合方案 Q3：状态机、存储、监督下沉；axum handler 与启动编排留在组合根。

`IdorisHost::system` 把状态放在数据目录，发行文件放在 ADR-038 的托管根。agent24d 用 `install::managed_root` 传入 `<data>/components/idoris`（`serve_steps.rs:76-83`；`rust/crates/agent24-idoris/src/idoris_host.rs:607-627`）。

### D2 `IdorisSlot` 是模型路由与托管之间的唯一缝

`agent24-models` 已经依赖 `agent24-idoris`，托管层不能再反过来点名 `ModelRouter`（`managed_idoris.rs:9-11`）。缝是 `IdorisSlot`：挡住旧网关、用准备好的网关替换、按引用打开网关、做一次真实对话校验（`managed_idoris.rs:77-96`）。`ModelRouter` 的实现在 `rust/crates/agent24-models/src/idoris_slot.rs:45-88`。`ManagedIdoris` 只保存 `Arc<dyn IdorisSlot>`（`managed_idoris.rs:132-151`）。

数据面密钥不进托管配置。落盘的 `ManagedConfig` 只有 `url` 和 `key_id`（`managed_idoris.rs:104-109`）。连接时按 `key_id` 向注入的 `SecretSource` 要 `Zeroizing<String>`，再交给 `open_gateway`（`:217-240`）。`IdorisGateway::from_credentials` 收的就是这串内存中的 secret（`idoris_slot.rs:62-70`）。钥匙串锁住或没有条目就失败，不回退到无密钥（`managed_idoris.rs:235-238`）。

### D3 Admin session token：daemon 生成，只交给它拉起的 iDoris

一次启动从 `/dev/urandom` 读 32 字节，编成 64 位小写 hex（`supervisor.rs:933-942`）。加换行后经子进程 stdin 一次写完并关掉写端（`:464`、`:488-511`、`:910-930`）。参数只有 `serve --admin-token-stdin`（`:874`）。spawn 前 `env_clear`，再放入固定的 `PATH`、少数父进程变量（`HOME`、`TMPDIR`、`LANG`、`LC_ALL`、`LC_CTYPE`、`TZ`）和 `IDORIS_*`（`:871-906`）。Admin token 不在其中。

token 留在该子进程这一代的 `Zeroizing` 内存里，字段名 `_admin`（`:450-451`、`:485`）。用途是就绪检查时给本机 Admin `GET /admin/api/v1/status` 带 Bearer（`:636-642`）。子进程结构丢掉时，这份副本一起丢掉。下次启动在 `:464` 重新生成。

桌面不接收、不保存这枚 token：

- `apps/desktop/src/main/idoris-admin.ts` 不存在。main、preload、ipc 源码里没有 `--idoris-admin-token-fd` 和旧的 `idorisStatus` IPC（`apps/desktop/src/main/idoris-admin.test.ts:5-9`）。
- 渲染进程只经 `backendProxy` 调 daemon 的 `/api/v1/idoris/*`。允许的就是状态、候选、操作、安装、连接、启动、停止（`backend-proxy.ts:64-72`）。`GET /admin/api/v1/status` 不在允许表里（`idoris-proxy.test.ts:15`）。代理附上的是 daemon 的宿主令牌，不是 iDoris Admin token（同测试 `:26-28`）。

由此取代 ADR-035 里「第二条匿名管道交给 Electron main」「main 读 65 字节帧并在 main 内存持有 token」「renderer 经零参数 `idorisStatus()` 间接受询」「launcher 写完两条管道后丢掉自己的副本」。stdin 交给 iDoris、token 不进 argv、普通文件和日志，仍然有效。启动者改成 daemon 里的 supervisor。

ADR-035 决策 2（Windows 用显式 allowlist 的 inherited HANDLE）本篇不取代。当前 `supervisor.rs` 只有上面的 stdin 管道，没有 HANDLE 白名单。FU：Windows 句柄交接。

### D4 三样秘密分开，上游密钥只走注入的 `Provisioner`

| 秘密 | 存放 | 交给谁 |
|---|---|---|
| Admin session token | 仅本代进程内存（D3） | 只经 stdin 给被监督的 iDoris；daemon 自己用来探 Admin |
| 数据 key（`idk_`） | 钥匙串；配置只存 `key_id`（D2） | `SecretSource` → `IdorisSlot::open_gateway`。不写入服务 plist |
| 上游 oMLX key | 该端点已保存则读钥匙串；否则问 `Provisioner::upstream_key` | 仅 setup 与 `serve` 的环境变量 `IDORIS_OMLX_API_KEY`（`supervisor.rs:904-906`）。key 子命令和安装器不拿它 |

`IdorisHost::open` 默认装上 `SystemProvisioner`（`idoris_host.rs:676`）。在 host 还没分享给路由之前，`with_provisioner` 可以换成另一份实现，并 `rebind` 到 runtime（`idoris_host/provision.rs:271-284`；`idoris_host.rs:490-492`）。真正要密钥的两条路都问这份 provisioner，不再自己读设置文件：

- 启动输入：有 `omlx_key_id` 就读钥匙串，否则 `provisioner.upstream_key()`（`idoris_host.rs:502-516`）。
- 连接：端点与已存引用一致时读钥匙串，否则同样问 provisioner（`provision.rs:687-702`）。
- 候选探测：无确认端点时不发送凭据；有注入实现时用它返回的 key（`provision.rs:294-303`）。

`SystemProvisioner::upstream_key` 本身在阻塞任务里调用 `read_settings_key`（`provision.rs:176-185`）。这是默认实现，不是旁路。测试 `inputs_reads_the_injected_upstream_key` 用只返回固定密钥的实现，断言探测请求带的是注入值 `Bearer sk-injected-fixed`（`idoris_host/tests/provision.rs:1392-1404`）。这就是 FU-115：上游密钥注入不再被 provision 路径绕开。

安装和连接的请求体拒绝明文密钥字段。`ConnectRequest` 没有 key 字段，多出来的 `omlx_key` 因 `deny_unknown_fields` 变成 `invalid_request`，响应里也不回显（`provision.rs:32-48`；`idoris_routes.rs:236-266`）。同意缺失是 `consent_required`（`provision.rs:332-357`）。

### D5 不改的边界

- 模型选型、量化、加载仍归 iDoris（ADR-036）。Agent24 分预算、监督进程、持有 Admin token。
- iDoris 仍不是进程外领域模块（ADR-032）。这里的子进程是 daemon 监督的运行时，不走 ADR-029 的模块挂载缝。
- 硬件探测与整机内存账仍在 `agent24-hw`。预算经 `IDORIS_ALLOCATED_BUDGET_BYTES` 传给子进程（`supervisor.rs:892-899`），不把剩余预算伪装成物理内存。

### D6 生命周期 API 在 daemon 上，Admin 凭据不进 Electron

路由与动作（`server.rs:231-252`；`idoris_routes.rs:66-105`）：

| 方法 | 路径 | 动作 | 保证级 |
|---|---|---|---|
| GET | `/api/v1/idoris/status` | 读状态 | Everyday，`idoris.read` |
| GET | `/api/v1/idoris/operations/{id}` | 读一次操作 | Everyday，`idoris.read` |
| POST | `/api/v1/idoris/install` | 安装。要 `consent: true` | Elevated，`idoris.lifecycle` |
| POST | `/api/v1/idoris/connect` | 连接。要同意分享 oMLX key，并核对日用/深度模型 | Elevated |
| GET | `/api/v1/idoris/omlx/candidates` | 列候选 | Elevated |
| POST | `/api/v1/idoris/start` | 启动 | Elevated |
| POST | `/api/v1/idoris/stop` | 停止 | Elevated |
| POST | `/api/v1/idoris/operations/{id}/cancel` | 取消 | Elevated |

保证级在 `route_policy.rs:69-76`。文件头写的是 host-only（`idoris_routes.rs:1`），桌面代理注释同样写 host-only（`backend-proxy.ts:64`）。代码里的门比这句窄一些，也比「只许 ProductHost」宽一些：

- `host_only_route` 不包含这些路径。那张表是通信私钥、组件报告和入站名单（`middleware.rs:43-60`）。
- Creative 调用上面每一条都是 403（`idoris_routes.rs:152-180`）。Creative 允许表只有 `GET /api/v1/models`（`route_policy.rs:235-237`）。
- 组件运行时允许表只有 `POST /api/v1/inbound/events`（`route_policy.rs:242-244`）。
- 一方外壳的令牌带有目录里的 `IdorisLifecycle`（`first_party.rs:513-523`）。写操作和候选列表要先完成在场确认（`middleware.rs:442-651`；可确认的路径在 `first_party.rs:648-657`）。因此登记过的 CLI 外壳，在用户确认之后，也能调安装、连接、启动、停止。
- ProductHost 不走这段一方外壳的在场分支，按自己的操作集调用。
- legacy 共享令牌也不在 `host_only_route` 和 `legacy_unelevatable` 里（`middleware.rs:72-78`、`:98-100`），legacy 模式可以打这些路由，且没有在场确认。FU：legacy 下的 iDoris 生命周期不走在场确认。

Admin token 仍然只在 D3 那条路上。这些 HTTP 路由的响应里没有它。

## 3. 证据

| 决策 | 证据 |
|---|---|
| D1 组合根与托管 crate | `serve_steps.rs:76-83`、`:323-357`；`idoris_host.rs:607-627`；#1790 |
| D2 缝与数据 key | `managed_idoris.rs:77-96`、`:104-109`、`:132-151`、`:217-240`；`agent24-models/src/idoris_slot.rs:45-70` |
| D3 Admin token 与桌面不再持有 | `supervisor.rs:450-451`、`:464`、`:488-511`、`:636-642`、`:852-906`、`:910-942`；`backend-proxy.ts:64-72`；`idoris-admin.test.ts:5-9`；`idoris-proxy.test.ts:15`、`:26-28`；#1472 |
| D4 上游密钥注入（FU-115） | `provision.rs:32-48`、`:176-185`、`:271-284`、`:687-702`；`idoris_host.rs:490-516`、`:676`；`tests/provision.rs:1392-1404`；#1799 |
| D6 生命周期 API 的门 | `idoris_routes.rs:1`、`:66-105`、`:152-180`；`server.rs:231-252`；`route_policy.rs:69-76`、`:235-244`；`middleware.rs:52-60`、`:442-651`；`first_party.rs:513-523`、`:648-657` |
