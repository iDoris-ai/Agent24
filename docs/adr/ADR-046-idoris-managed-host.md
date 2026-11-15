# ADR-046：iDoris 由 agent24d 托管

索引：[架构总入口](../architecture/README.md)・[ADR 索引](README.md)

- 状态：**已接受**（2026-11-12，S0-8。记录 S2-2 #1790 之后的现行代码）。
- 编号：在 [ADR 索引](README.md) 领取 ADR-046。
- 上位：[架构优化方案](../tracking/2026-11-04/ARCH-OPTIMIZATION-PLAN.md) §4.1 S0-8 的 O3、S2-2；[ADR-032](../decision.md)（iDoris 作兼容接口，不做成进程外模块）；[ADR-036](../decision.md)（硬件账归 Agent24，选型归 iDoris）；[ADR-037](../decision.md)（agent24d 是本体）。
- 关系：部分取代 [ADR-035](../decision.md)。被取代的条款见 ADR-035 文首；本篇 D3 是现行交接。

## 1. 背景

ADR-032 把 iDoris 定成模型网关，不是进程外领域模块。ADR-035 当时的前提是「Agent24 不拥有、不拉起 iDoris」，所以 Admin session token 要由外部 launcher 分两路交给 iDoris 和 Electron main。IL-13 之后这条前提不再成立：daemon 拉起并监督 iDoris，桌面不再持有 Admin token。S2-2（#1790）把托管实现从 agent24d 挪进 `agent24-idoris`，并用 `IdorisSlot` 打断 `agent24-models` 与 `agent24-idoris` 的依赖环。R1 C5（provision 绕过依赖注入读上游密钥）已修，原跟踪号 FU-115。

## 2. 决策

### D1 agent24d 是托管进程的组合根，生命周期在 `agent24-idoris`

非临时 daemon 启动时构造 `ManagedIdoris`，再 `IdorisHost::system`（`rust/apps/agent24d/src/serve_steps.rs:323-350`）。HTTP 路由留在 agent24d 的 `idoris_routes.rs`，不随 crate 下沉。这符合方案 Q3：状态机、存储、监督下沉；axum handler 与启动编排留在组合根。

`IdorisHost::system` 把状态放在数据目录，发行文件放在 ADR-038 的托管根 `<data>/components/idoris`（`rust/crates/agent24-idoris/src/idoris_host.rs:607-627`）。

### D2 `IdorisSlot` 是模型路由与托管之间的唯一缝

`agent24-models` 已经依赖 `agent24-idoris`，托管层不能再反过来点名 `ModelRouter`（`managed_idoris.rs:9-11`）。缝是 `IdorisSlot`：挡住旧网关、用准备好的网关替换、按引用打开网关、做一次真实对话校验（`managed_idoris.rs:77-96`）。`ModelRouter` 的实现在 `rust/crates/agent24-models/src/idoris_slot.rs:45-88`。`ManagedIdoris` 只保存 `Arc<dyn IdorisSlot>`（`managed_idoris.rs:132-151`）。

数据面密钥不进托管配置。落盘的 `ManagedConfig` 只有 `url` 和 `key_id`（`managed_idoris.rs:104-109`）。连接时按 `key_id` 向注入的 `SecretSource` 要 `Zeroizing<String>`，再交给 `open_gateway`（`:217-240`）。`IdorisGateway::from_credentials` 收的就是这串内存中的 secret（`idoris_slot.rs:62-70`）。钥匙串锁住或没有条目就失败，不回退到无密钥（`:235-238`）。

### D3 Admin session token：daemon 生成，只交给它拉起的 iDoris

一次启动生成 32 字节随机数，编成 64 位小写 hex，加换行后经子进程 stdin 一次写完并关掉写端（`supervisor.rs:852-878`、`:910-930`、`:933-936`、`:488-511`）。参数只有 `serve --admin-token-stdin`（`:874`）。spawn 前 `env_clear`，不把父进程环境原样传下去（`:871-881`）。

token 留在该子进程这一代的 `Zeroizing` 内存里，用途是就绪检查时给本机 Admin `GET /admin/api/v1/status` 带 Bearer，以及日志脱敏（`supervisor.rs:450-451`、`:636-642`）。字段名 `_admin` 不外借。子进程结构丢掉时，这份副本一起丢掉；下次启动重新生成（`:464`、`:478-486`）。

桌面不接收、不保存这枚 token：

- `apps/desktop/src/main/idoris-admin.ts` 不存在；main、preload、ipc 源码里没有 `--idoris-admin-token-fd` 和旧的 `idorisStatus` IPC（`apps/desktop/src/main/idoris-admin.test.ts:5-9`）。
- 渲染进程只经 `backendProxy` 调 daemon 的 `/api/v1/idoris/*`。允许的就是状态、候选、操作、安装、连接、启动、停止（`backend-proxy.ts:59-67`）。`GET /admin/api/v1/status` 不在允许表里（`idoris-proxy.test.ts:12-16`）。代理附上的是 daemon 的宿主令牌，不是 iDoris Admin token（同测试 `:26-28`）。

由此取代 ADR-035 里「第二条匿名管道交给 Electron main」「main 读 65 字节帧并在 main 内存持有 token」「renderer 经零参数 `idorisStatus()` 间接受询」「launcher 写完两条管道后丢掉自己的副本」。stdin 交给 iDoris、token 不进 argv、普通文件和日志，仍然有效，只是启动者改成 daemon 里的 supervisor。

### D4 三样秘密分开，上游密钥只走注入的 `Provisioner`

| 秘密 | 存放 | 交给谁 |
|---|---|---|
| Admin session token | 仅本代进程内存（D3） | 只经 stdin 给被监督的 iDoris；daemon 自己用来探 Admin |
| 数据 key（`idk_`） | 钥匙串；配置只存 `key_id`（D2） | `SecretSource` → `IdorisSlot::open_gateway`。不写入服务 plist |
| 上游 oMLX key | 该端点已保存则读钥匙串；否则问 `Provisioner::upstream_key` | 仅 setup 与 `serve` 的环境变量 `IDORIS_OMLX_API_KEY`（`supervisor.rs:904-906`）。key 子命令和安装器不拿它 |

`IdorisHost::open` 默认装上 `SystemProvisioner`（`idoris_host.rs:676`）。在 host 还没分享给路由之前，`with_provisioner` 可以换成另一份实现，并 `rebind` 到 runtime（`idoris_host/provision.rs:271-284`；`idoris_host.rs:490-492`）。真正要密钥的两条路都问这份 provisioner，不再自己读设置文件：

- 启动输入：有 `omlx_key_id` 就读钥匙串，否则 `provisioner.upstream_key()`（`idoris_host.rs:502-516`）。
- 连接：端点与已存引用一致时读钥匙串，否则同样问 provisioner（`provision.rs:687-702`）。
- 候选探测：无确认端点时不发送凭据；有注入实现时用它返回的 key（`provision.rs:299-300`）。

`SystemProvisioner::upstream_key` 本身在阻塞任务里调用 `read_settings_key`（`provision.rs:176-185`）。这是默认实现，不是旁路。测试 `inputs_reads_the_injected_upstream_key` 用只返回固定密钥的 `FixedUpstream`，断言探测请求带的是注入值 `Bearer sk-injected-fixed`，而不是设置文件里的另一把（`idoris_host/tests/provision.rs:1361-1404`）。这就是 FU-115：上游密钥注入不再被 provision 路径绕开。

### D5 不改的边界

- 模型选型、量化、加载仍归 iDoris（ADR-036）。Agent24 分预算、监督进程、持有 Admin token。
- iDoris 仍不是进程外领域模块（ADR-032）。这里的子进程是 daemon 监督的运行时，不走 ADR-029 的模块挂载缝。
- 硬件探测与整机内存账仍在 `agent24-hw`。预算经 `IDORIS_ALLOCATED_BUDGET_BYTES` 传给子进程（`supervisor.rs:891-899`），不把剩余预算伪装成物理内存。

## 3. 证据

| 决策 | 证据 |
|---|---|
| D1 组合根与托管 crate | `serve_steps.rs:323-350`；`idoris_host.rs:607-627` |
| D2 缝与数据 key | `managed_idoris.rs:77-96`、`:104-109`、`:132-151`、`:217-240`；`agent24-models/src/idoris_slot.rs:45-70` |
| D3 Admin token 与桌面不再持有 | `supervisor.rs:450-451`、`:464`、`:488-511`、`:852-906`、`:910-936`、`:636-642`；`backend-proxy.ts:59-67`；`idoris-admin.test.ts:5-9`；`idoris-proxy.test.ts:12-16` |
| D4 上游密钥注入（FU-115） | `provision.rs:102`、`:176-185`、`:271-284`、`:687-702`；`idoris_host.rs:490-516`、`:676`；`tests/provision.rs:1392-1404` |
