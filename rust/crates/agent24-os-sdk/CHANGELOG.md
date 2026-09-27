# Changelog — agent24-os-sdk

All notable changes to the `agent24-os-sdk` crate (and its companion
`agent24-os-fd`) are documented here. This crate has its own version line,
separate from the Agent24 product version (`agent24d`/`agent24-cli`) — see
`docs/design/ME4-S3-os-sdk.md` §7/§8 Q9 for why. Tags are named
`agent24-os-sdk-v<version>` and pushed on the `main` merge commit that
contains the corresponding change (J-S14).

## [0.1.0] — 2026-09-28

**最低内核版本 = agent24 0.4.0.** 这是 SDK 与 `agent24-os-fd` 的首个原型版本，
需要 Agent24 daemon（`agent24d`）≥ v0.4.0（本版本首次交付进程外领域 OS 的
调度回调 `_a24/scheduler/*` 与推理回调 `_a24/model/complete`，SDK 的
`SchedulerClient`/`ModelClient` 依赖这两个内核回调的 wire 形状）。

**原型阶段声明**：这是从两个真实调用方（ME4 调度/推理回调实现、Sin90 计划中的
迁移）提炼出的第一版公共接口，**接口可能变**——尤其 `ClientError` 的分类、
`FiredBody`/`RequestContext` 的字段集合，随 Sin90（ME4-5.2.1）迁移过程中的
真实反馈调整。

### 新增

- `agent24-os-proto` 新增模块侧 transport/握手：`ModuleEnv`、`Hello`、
  `InitializeReply`、`connect_from_env`；`manifest::{ManifestFacts,
  facts_from_yaml}`；`manifest_digest`；`Connection` mux（读写任务、按 id
  分发、64 在途、`declare_dead`、drop 取消、响应/写超时、`slot_wait`）；
  `module::testing`（`test-util` feature）（#515）
- 新 crate `agent24-os-fd`：`take_inherited_listener`（唯一 `unsafe`，两平台
  分支）+ proto 侧 `take_listener`/`InheritedListener`，负责继承监听 socket
  的 fd 传递（#515）
- `agent24-os-sdk` 骨架：`clippy.toml`、`Module`/`ModuleBuilder`/`SdkError`/
  `serve`/`with_env`、`ClientError`/`UnavailableCause`（不含 `retry_class`，
  见下方「不提供」）、`RequestContext`/`RequestId`/`ApprovalToken`（#516）
- 五个客户端：`EventsClient`、`MemoryClient`（含 `remember_once`，写前
  `recall` 预查 dedup 标记，`RECALL_PRECHECK_MAX_PAGES = 10`）、
  `ApprovalClient`（advise，孤儿约束）、`SchedulerClient`、`ModelClient`，
  各自与 agentd 对等的 wire 测试（#516）
- `FiredBody` 挪进 `agent24_os_proto::kernel_call`（内核序列化、SDK 反序列
  化共享同一个类型）+ fired 提取器 + `with_fired` 注册点（#516）
- `examples/minimal` + 挂载冒烟测试 + 探针 `4c SDK`（#516）

### 不提供（原型阶段的有意省略，见 `docs/agent/followups.md`）

- `ClientError::retry_class()`（FU-87）：v0.1.0 只给 `is_permanent()`/
  `is_retryable()`；`Cancelled` 在不同调用方的语义不一致，等 Cos72 有了
  outbox 代码后再决定是否提取统一的重试分类。
- `agent24-os-proto` 未拆 `kernel`/`module` cargo feature（FU-86）：引入本
  crate 会连带编译 `command-fds`、`close_fds`、`rustix`、`hyper`/
  `hyper-util`、`agent24-domain`（→ `serde_yaml`、`schemars`）。
- SDK 侧 `handshake` 无超时、响应缺字符串 `id` 时要等到连接级
  `CALL_TIMEOUT`（35s）才失败、`PASSTHROUGH_VARS` 未做成显式枚举、
  `agent24-os-fd` 缺跨 `exec` 集成测试（FU-95，随 ME4-5.2.1 一起处理）。

### 已知问题

- 握手 token 非合法 UTF-8 时误报 `EnvError::Missing`（FU-94，非阻塞）。
- `MemoryClient` 的 `delete_and_list_round_trip` 测试未实际断言 list 之后
  确实不再返回已删记录；fired 提取器对全空白 `X-A24-Fire-Id` 未做非空校验
  （FU-98，均非阻塞）。

### 评审状态

`agent24-os-proto` 模块侧改动（#515）与 `agent24-os-sdk` 骨架（#516）均只
经 PR-Daemon 本地评审（各两轮 CHANGES → APPROVE），**未经 Codex 对抗评审**
（`docs/agent/followups.md` `ME4-CODEX-DEBT-9`，Codex 额度 2026-09-29
19:28 恢复后补审）。
