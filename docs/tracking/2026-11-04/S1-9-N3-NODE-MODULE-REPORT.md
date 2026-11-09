# S1-9-N3: Node 参考模块改走 attach 调研与改造报告

## 1. 调研结论：A3 attach 线协议 vs A1 kernel 拉起 + fd 3 监听模型差异

| 维度 | A1 内核拉起（`out_of_process_provider`） | A3 附着模块（`attached_process`） |
|---|---|---|
| **进程拉起** | 内核直接 spawn，环境变量注入（`A24_LISTEN_FD=3`、`A24_CALLBACK_SOCK`、`A24_HANDSHAKE_TOKEN`、`A24_DATA_DIR`） | 外部或测试 harness 自行启动进程；内核不拉起、不杀进程 |
| **凭据分发** | 启动包扫描加载，每代现铸一次性 token 经环境变量传递 | REST `POST /api/v1/attached` 注册 manifest，返回长期 token 与常驻 socket 路径 |
| **套接字拓扑** | 双套接字：继承 fd 3 作为入站 HTTP 监听；主动连接 `A24_CALLBACK_SOCK` 作为出站回调 | 单套接字：全双工 Unix domain socket（`~/.agent24/attach/agent24d.sock`） |
| **内核→模块通信** | 内核向模块 fd 3 发起 HTTP 请求（反向代理 `/api/v1/<ns>/*`） | 同一连接上下发 `_a24/command/invoke` JSON-RPC 请求，无 HTTP 代理 |
| **能力授予** | 全量授予（events, memory, scheduler, approval, models） | A3 P2 目前仅授予 `events` 与 `models`，无 `memory` 分区 |
| **断连与生命周期** | 连接即生命线，断连触发 SIGTERM/SIGKILL 停机；有两阶段 drain | 连接断开仅撤销该代（generation），外部进程继续运行，支持无 drain 重连 |

## 2. 改造方案与实现

由于 A3 现有的线协议（NDJSON + JSON-RPC 2.0）已能完整承载独立的进程外握手与回调通信，**无需修改任何内核协议代码**：
1. **Manifest (`domain-os.yml`)**：
   - 更新为 `impl_kind: attached_process`，移除 `spawn` 段；
   - 声明 `kernel_capabilities: [events]`。
2. **Node 参考实现 (`index.js`)**：
   - 保持零 npm 依赖与纯 Node.js 标准库（`net`, `crypto`, `fs`, `path`）；
   - 移除 `http.createServer().listen({ fd: 3 })` 及其入站路由与 memory 调用；
   - 支持从 CLI 参数或环境变量（`A24_ATTACH_SOCK` / `A24_ATTACH_TOKEN`）读取 socket 路径与 token；
   - 连接常驻附着 socket，发送 `initialize` 握手请求；
   - 握手成功后调用 `_a24/events/emit` 广播 `task.transitioned` 事件并原子写入 `startup_probe.json`；
   - 监听连接 EOF 并在关闭时优雅退出。
3. **黑盒验收测试 (`me4_node_module_blackbox.rs`)**：
   - 移除向 `packages/node-ref` 的文件安装逻辑；
   - 走 `POST /api/v1/attached` 注册 manifest 获取凭据；
   - 启动 Node 进程，验证实时 WS 收到 `task.transitioned` 事件，验证探针文件与 `attached` 状态；
   - 增加守卫测试 `wrong_token_handshake_is_rejected`。
4. **文档更新**：
   - 更新 `WIRE-OOP-MODULE.md`、`README.md`、`WINDOWS-PORT.md`，明确安装包只允许原生可执行程序；签名按 S1-9 §5 平台策略（Linux/Windows v1 无代码签名），Node 参考模块改走 attach。

## 3. 设计取舍：旧内核拉起与 memory 验证

旧测试的「非 Rust 实现按 WIRE-OOP 文档接入内核拉起模式（fd3 HTTP 代理 + memory）」证明不再保留：
- 安装包在生产规则下只允许原生可执行程序（由完整性清单与原生可执行判定强制守卫，见 feat/s1-9-n3a-native-exec），因此非 Rust 解释型语言模块无法再作为安装包被内核直接拉起；
- 内核拉起模式（fd3 HTTP 代理 + memory 回调）已由 Rust SDK 与 `me3f_blackbox` 测试完整覆盖；
- A3 附着模式下，出于安全最小权限模型，对附着模块的 memory 授权固定为 NONE（不支持迁入 memory），因此 Node 参考实现仅保留并演示 `events` 回调与反向 `host_commands`（`echo`）交互。
