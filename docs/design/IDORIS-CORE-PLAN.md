# iDoris 成为 Agent24 必选核心（IDORIS-CORE）方案

2026-11-20 · 状态：草案，待一轮评审后冻结 · 负责人：Agent24 主会话（总管）

## 0. 决定（jason 2026-11-20）

1. iDoris 是 Agent24 的**必选核心模块**，随安装包附带，启动时自动安装、自动连接，用户无需任何操作。
2. Agent24 的**所有模型调用**（对话、子代理、摘要、改写、guardian、向量 embedding、OS 模块的 models 授权、桌面端的模型列表与预热，以及将来的云端模型）都**只经过 iDoris**，不得绕过。iDoris 当前封装 oMLX，将来接入的其他后端（LM Studio、Ollama、云端订阅等）也作为 iDoris 的上游组件。
3. **高可用**：iDoris 卡住或退出时自动重启。Agent24 和 iDoris 双向发现、握手，支持动态端口，采用业界成熟做法。
4. **预览版按全新安装设计**：没有旧版本兼容、迁移和升级路径（预览版定义见总规则）。

## 1. 现状（2026-11-20 main 的只读盘点）

已经经过 iDoris 的调用：托管 iDoris 运行时，`ModelRouter` 的所有调用都走 iDoris 槽位，包括对话、子代理、摘要、guardian、改写，以及 documents 和 Sin90 的 models 授权。已安装但没在运行时，router 直接报错，不会回退。

绕过 iDoris 的点（本方案要逐一消除）：

| # | 位置 | 现状 |
|---|---|---|
| B1 | `agent24-models/src/embedding.rs:98`、`agent24d/src/embed_backfill.rs:23` | embedding 只认 `IDORIS_URL` 环境变量，托管 iDoris 没有设置它，所以一直直连 oMLX `:8088/v1/embeddings` |
| B2 | `agent24-models/src/router.rs:433-505` | 没有 iDoris 时走 custom、omlx、ollama 的直连链 |
| B3 | `router.rs:669` | iDoris 分支不应用保存的默认模型 |
| B4 | `apps/desktop/src/main/ipc/index.ts:310-357,786-883` | `OmlxDetect`、`OmlxModels`、`OmlxWarmup`、`LlmStatus` 直连 oMLX 和 Ollama |
| B5 | `App.tsx:196-228`、`ModelServiceSettings.tsx`、`ModelSourceSettings.tsx:38` | 渲染层直接探测或启动 oMLX，模型来源页直接请求 `{url}/v1/models` |
| B6 | `OmlxStart`、`omlx-kept.ts`、`omlx-quit-policy.ts` | oMLX 由桌面端启停 |
| B7 | iDoris 安装 | 按需安装，需要用户点击并同意，从 GitHub 下载；端口固定为 9011/9012 |
| B8 | `packages/node-daemon/src/llm-gateway.ts` | 开发用路径，直连 oMLX，key 写死 |
| B9 | `agent24-cli/src/service.rs:269-276` | 向 LaunchAgent 透传 `IDORIS_URL`、`OMLX_URL` |

## 2. 目标架构

```
launchd ──KeepAlive──▶ agent24d ──supervises──▶ idoris serve ──manages──▶ oMLX（及将来的上游）
   ▲                      │  ▲                        │  ▲
   │                      │  └──── register / heartbeat（iDoris → Agent24，UDS）
   │                      └──────── /health 就绪与活性探测，数据面请求（Agent24 → iDoris，loopback 动态端口）
desktop App ──(只经 agent24d)──▶ /api/v1/models/*
```

- **唯一出口**：Agent24 进程（agent24d、桌面端 main/renderer、OS 模块）只和 iDoris 的数据面通信。Agent24 代码里不再出现 oMLX、Ollama 或任何模型服务的地址和 key。
- **职责划分**：
  - iDoris 负责：后端启停（含 oMLX）、上游配置、模型列表和类型、推荐、预热、下载、embedding、路由策略。
  - Agent24 负责：监管 iDoris、发现和握手、保存用户选择（默认模型），以及展示和确认（下载、隐私）。

## 3. 高可用设计

### 3.1 监管树（supervision tree）
- launchd（`KeepAlive`）监管 agent24d，agent24d 的 `supervisor` 监管 iDoris，iDoris 监管 oMLX 等上游。每一层只负责重启自己的直接子进程。这是 Erlang/OTP、systemd 常用的分层监管思路。
- 重启策略：
  - 指数退避加抖动：1s → 2s → 4s … 封顶 30s。
  - 熔断：5 分钟内崩溃超过 5 次，进入 `degraded`，界面显示原因。之后每 60 秒半开重试一次，**不会永久放弃**。

### 3.2 活性与就绪分离（liveness / readiness）
- 就绪（readiness）：`GET /health` 返回 `status=ok`、版本匹配、contract major 为 1，并且至少有一个上游可用。不就绪时，新请求进入等待（见 3.4）。
- 活性（liveness）：每 5 秒探测一次 `/health`，超时 2 秒。连续 3 次超时，或进程仍在但不响应，就判为**卡死**：先发 SIGTERM，按 §2.7 等 17 秒，再发 SIGKILL，然后按 3.1 重启。卡死和崩溃在统计和界面上分开记录。

### 3.3 动态端口与双向握手
1. **Agent24 → iDoris（启动注入）**：agent24d 生成 `instance_nonce` 和 admin token，从 stdin 一并交给 iDoris（沿用 §2.7 的 stdin 帧，扩展字段）。同时下发 Agent24 的**登记端点**：agent24d 在 `<data_dir>/run/` 下的 Unix domain socket 路径。
2. **iDoris 绑定动态端口**：数据面和 admin 都绑 `127.0.0.1:0`。就绪后把 `{pid, instance_nonce, data_port, admin_port, version, contract_version, started_at}` 原子写入 `<data_dir>/run/idoris.json`（权限 0600），作为**发现文件**。
3. **iDoris → Agent24（登记）**：iDoris 连上登记端点，发送 `register{instance_nonce, ports, version}`。agent24d 校验 nonce 和对端 pid（`LOCAL_PEERPID`）后回 `ack{session_id}`，**握手完成**，agent24d 再把数据面地址发布到 router 槽位。
4. **双向心跳**：
   - Agent24 按 3.2 探测 iDoris。
   - iDoris 每 10 秒在登记连接上发一次心跳。连接断开（例如 agent24d 被 launchd 重启），iDoris **继续提供服务**，并按退避节奏重连登记端点、重新握手。
5. **接管孤儿进程**：agent24d 重启后，先读发现文件。如果 pid 存活、`/health` 正常、iDoris 也重新登记上来，就**接管**这个 iDoris，不再重启第二个。否则按 SIGTERM、SIGKILL 的顺序清理，然后重新启动。同一个数据库只允许一个 iDoris 实例：iDoris 自带实例锁（退出码 3）。
6. 不用固定端口，所以不会有端口冲突；发现文件和登记两条路互为校验。

### 3.4 请求层
- iDoris 重启期间，新请求在 router 里**有界等待**就绪，上限 30 秒。超时返回 503 `idoris_unavailable`，并带 `retry_after`。
- 非流式请求在连接级失败（连接被拒、连接被重置）时，等就绪后**重试一次**。流式请求已经开始输出的，不重试，直接报错。
- 熔断打开期间立即失败，界面提示「模型服务恢复中」，不会把请求排队压垮 iDoris。

## 4. 绕过点的改法（Agent24 侧）

| 片 | 内容 |
|---|---|
| **IC-1 随包与自动接入（含 B7、B9）** | DMG 附带锁定版本的 iDoris 包（`apps/desktop/scripts/idoris-pin.json` + stage 脚本 + `extraResources`），校验 codesign team 后安装到 `<data>/components/idoris`。启动时自动执行 install 加 connect（`setup omlx` + 签发数据 key），不需要用户同意（核心组件）。去掉能力中心里的 iDoris 安装按钮，改为只读的状态面板。CLI 不再透传 `IDORIS_URL`、`OMLX_URL` |
| **IC-2 内核只走 iDoris（B1–B3）** | `ModelRouter` 删除 custom、omlx、ollama 直连链，没有 iDoris 时报错。embedding 从托管网关槽位取地址和 key。iDoris 分支应用保存的默认模型。删除 `IDORIS_URL` 环境变量入口（开发调试改为指向一个外部 iDoris 的显式开关，仍然走同一个槽位） |
| **IC-3 高可用（§3）** | supervisor 增加活性探测、卡死判定、退避加熔断加半开；动态端口、发现文件、UDS 登记、心跳、接管孤儿进程；router 的有界等待和一次重试 |
| **IC-4 桌面端只经 daemon（B4–B6、B8）** | 删除直连 oMLX 和 Ollama 的 IPC 与渲染层探测。模型列表、预热、状态都改成 daemon 的 `/api/v1/models/*`（daemon 转给 iDoris）。oMLX 启停交给 iDoris。模型来源页改为配置 iDoris 上游。node-daemon 的 llm-gateway 删除，或改为指向 iDoris |
| **IDR-1 推荐（进行中）** | `/api/v1/models/recommendation`、`/api/v1/models/local`，首启默认模型。在本方案下，回落链只剩「iDoris 不可用时报错，并显示原因」 |

## 5. iDoris 侧需要配合（交给 idoris-3b，细节在 iDoris 仓库）

| 片 | 内容 |
|---|---|
| **ID-1 动态端口与发现** | `serve` 支持数据面和 admin 绑定端口 0，就绪后原子写发现文件（路径由 launcher 通过参数或 stdin 帧指定） |
| **ID-2 登记与心跳** | stdin 帧扩展字段 `instance_nonce`、`register_socket`。就绪后向登记 UDS 发 `register`，然后每 10 秒发心跳。连接断开后继续服务并退避重连 |
| **ID-3 oMLX 生命周期** | iDoris 负责探测、启动、停止、重启 oMLX（oMLX 是 iDoris 的子进程或受管进程）。启动时 oMLX 不可达，不再返回 `NotConfigured`，改为由 iDoris 拉起 |
| **ID-4 上游与 embedding** | `/v1/embeddings` 透传到上游。自定义 OpenAI 兼容上游（LM Studio 等）作为组件配置。预热接口 |
| **ID-5 契约修订** | 以上内容写进《边界与接口规范》§2.7 修订 1.4 和 wire 1.3.0，双方确认后冻结 |

## 6. 顺序与发版

1. 本方案评审一轮后冻结，同时把 §5 发给 idoris-3b 评估、排期。
2. Agent24 侧：IDR-1 → IC-1 → IC-2 → IC-4 可以先做，不依赖 iDoris 改动。IC-3 依赖 ID-1、ID-2。
3. iDoris 发 v0.6.0（ID-1 到 ID-4）后，Agent24 更新 pin，完成 IC-3。
4. 发版前，在签名包上按**全新安装**实测（卸载并清空 `~/.agent24` 的测试副本）：
   - 抓包或日志确认，除 iDoris 外没有任何进程连接 oMLX；
   - `kill -9` iDoris 后自动恢复；
   - `kill -STOP` 模拟卡死后自动恢复；
   - 重启 agent24d 后接管孤儿 iDoris，不会出现第二个实例。

## 7. 不做（本轮）

- 云端订阅 provider 的具体接入（iDoris 已有 `subscription.yaml` 框架，归 iDoris 后续版本）。
- Linux / x86。
- 旧版本兼容、迁移、升级（预览版规则）。
