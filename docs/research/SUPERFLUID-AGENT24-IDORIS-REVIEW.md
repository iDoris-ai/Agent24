# Superfluid 机制抽查与 Agent24 / iDoris 架构借鉴实证研究

> 状态：**研究与架构审查返修稿（未获准投入生产，非用户批准）** ｜ 日期：2026-10-23  
> 目标仓库：`https://github.com/basecompute/superfluid`  
> 固定代码基准 SHA：[`3a3598d33918e65f605c6eb3547c677b34e42563`](https://github.com/basecompute/superfluid/tree/3a3598d33918e65f605c6eb3547c677b34e42563)（v0.1.3）  
> Agent24 基线：[`f16e65a95f9732f4e00cb288f967e40e0978eb7b`](https://github.com/iDoris-ai/Agent24/tree/f16e65a95f9732f4e00cb288f967e40e0978eb7b)（分支 `ab/memory-p1-21-superfluid-research`）  
> 工具与依赖口径：`agent-reach` 检查版本为 `v1.5.0`。本轮在固定 SHA 上静态抽查 serving、scheduler、IPC/Fleet、安全与基准文档；**未执行上游项目代码或脚本，未复跑基准，没有完成全量源码或安全审计**。

---

## 摘要与核心发现

Superfluid 是由 Base Compute 开发的本地大语言模型推理 Serving Daemon，使用 Rust 编写，旨在统一 llama.cpp、MLX 和其专有引擎 baseRT 的推理调度与对外服务接口（兼容 OpenAI、Anthropic 与 Ollama 协议），并提供多机分布式组网（Fleet）。

基于对 Superfluid 固定 commit [`3a3598d33918e65f605c6eb3547c677b34e42563`](https://github.com/basecompute/superfluid/tree/3a3598d33918e65f605c6eb3547c677b34e42563) 的选定源码静态抽查，本研究形成以下核心实证结论：

1. **机制借鉴价值**：
   - **能力契约与 Conformance Tests**：定义了标准化 Worker Capability 数据结构与行为一致性测试套件思路，值得在 Agent24 规范模型运行时与外部适配器时借鉴；
   - **QoS 分级与系统资源感知**：实现了基于 Tick 时隙推进的请求调度，配合平台内核内存压力监听进行自适应调节；
   - **失败语义与取消回收**：客户端连接中断时通过 Drop 事件与 `CancelSet` 进入取消路径并发送释放请求；实际回收延迟与显存变化仍须测量；
   - **进程隔离架构**：Daemon 与推理 Worker 采用跨进程架构，进程分离有助于缩小故障影响，但控制面稳定性仍须故障注入验证。
2. **源码抽查与上游声明差异核实**：
   - **硬件平台与跑分声明**：Apple 已于 2026-03-03 [正式发布配备 M5 Pro / M5 Max 的 MacBook Pro](https://www.apple.com/newsroom/2026/03/apple-introduces-macbook-pro-with-all-new-m5-pro-and-m5-max/)。上游在文档中分别报告了 M1 Max（纯吞吐基准）与 M5 Pro（复杂 serving 场景）两台机器的数据。**本轮未在同等硬件上复跑验证，所有性能数字均作为上游声明引用**。
   - **QoS 调度机制与场景倾斜**：上游宣称“交互请求 0.79s 对比 llama-server 67s”，该数据来自其特定 mixed-load（8 个 Agent 长任务排队后插入 short chat）场景；源码中 QoS 可通过 `--http-default-qos` 命令行参数或请求头指定，并非严格仅由客户端请求头注入。不同服务的调度配置并不相同，不能单凭此组数字将差异全部归因于某一机制；相同调度策略下的跨系统性能差距以及在 Agent24 中的实际收益仍待公平对照实验检验。
   - **WAL 测试与基准覆盖界限**：上游 `tools/suite/README.md` 明确声明其自动化套件中的 `daemon_restart` 仅覆盖无状态 HTTP 重发，未测试 session-id WAL resume；但仓库源码中确实包含崩溃恢复测试 [`crates/superfluid-daemon/tests/wal_crash.rs`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-daemon/tests/wal_crash.rs)（本轮未执行）。基准套件缺测不构成代码无法恢复的证据。
   - **进程重启、流恢复与健康检查界限**：虽设有 `MAX_WORKER_RESPAWNS = 3` 限制，但 [`scheduler.rs`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-daemon/src/scheduler.rs#L1518-L1547) 在 Tick 出错时清空当前活动 lanes 并向当前所有任务发送 `LaneEvent::Failed`，随后才触发 respawn，因此原 HTTP 流在出错时可能直接失败，并非透明续传。此外，[`openai.rs`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-daemon/src/openai.rs#L1791-L1795) 的 `/health` 接口仅返回模型数量统计与 `status: ok`，不能作为底层 Worker 就绪或无故障的证明。
   - **抢占与内存开销**：抢占（`preempt_lane`）仅标记 `Finishing` 并在调度轮次中退让，退休时 prefill 页进入内存 prefix cache，并非每次抢占都落盘；磁盘持久化参数 `--park` 默认关闭。Link W 共享内存通信包含 `copy_nonoverlapping` 和缓冲编码，并非端到端零复制，亦不能消除 CPU 开销。
   - **权限与会话状态机权威边界**：Superfluid 内部定义了 `PermissionRequest` 和 `ToolUse` 类型，但这仅代表其内置了交互事件协议，不能断定其在底层擅自越权批准；关键在于架构设计边界：在 Agent24 中，Run/session、记忆与审计以现有内核为权威，权限与审批门由内核掌控；P1–P5 是后续记忆演进计划，不是已上线的会话治理能力。若 Serving 层自建会话与审批状态机，将导致双重状态机冲突与边界模糊。

---

## 一、系统架构与 13 维机制抽查剖析

Superfluid 采用多进程分层设计：
- **Daemon 进程**：承载 HTTP/Socket API、会话日志与 WAL、分词编解码、QoS 调度器与模型注册表。
- **Engine Worker 进程**：按加载模型独立起进程，拥有 GPU 显存、KV 缓存池与采样器。
- **Link W**：Daemon 与本地 Worker 间通信，由双 Unix Domain Socket（控制帧与 `SCM_RIGHTS` FD 传递）及三个共享内存环（Inbound Tokens, Outbound Tokens, Logits）构成。
- **Link F**：多机 Fleet 间通信，基于 TCP + Noise 协议加密传输。

```
[ 客户端 (OpenAI/Anthropic 协议) ]
             │ HTTP (支持 x-superfluid-qos 或 --http-default-qos)
             ▼
┌────────────────────────────────────────────────────────┐
│ superfluid daemon (控制面/调度/编解码)                  │
│   ├── Tokenizer / Codec                                │
│   ├── Session Store & WAL (Postcard 格式)              │
│   ├── QoS Scheduler (Tick-based / Preemption)          │
│   └── Host Registry                                    │
└────────────┬─────────────────────────────┬─────────────┘
             │ Link W (SCM_RIGHTS + SHM)   │ Link F (TCP + Noise NNpsk0)
             ▼                             ▼
┌─────────────────────────┐   ┌──────────────────────────┐
│ superfluid-workerd      │   │ superfluid-noded (Fleet) │
│ (GPU/KV Cache/Runtime)  │   │ 远端推理执行节点           │
│ [llama.cpp | MLX | baseRT]│   └──────────────────────────┘
└─────────────────────────┘
```

### 1. Serving Daemon 架构
- **实现位置**：[`crates/superfluid-daemon/src/serve.rs`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-daemon/src/serve.rs) 与 [`crates/superfluid-daemon/src/scheduler.rs`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-daemon/src/scheduler.rs)
- **分析**：Daemon 与 Worker 进程分离。Daemon 处理 CPU 端文本组装与 Tokenizer 编解码，向 Worker 传递扁平化的 `token_id` 流。两者通过 Unix Socket 与共享内存进行进程间通信。

### 2. Runtime 与多机管理 (Fleet / Link F)
- **实现位置**：[`crates/superfluid-linkf/src/head.rs`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-linkf/src/head.rs) 与 [`crates/superfluid-daemon/src/fleet_manager.rs`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-daemon/src/fleet_manager.rs)
- **分析**：Link F 使用自定义二进制帧结构（单帧上限 16 MiB），基于 Epoch 和租约（Lease）进行生成任务委派。Node 节点作为执行单元向 Head 上报生成的 Token；当网络中断或租约超时触发回收。节点间故障切换分支与网络闪断恢复逻辑依赖上层调度，具体容灾边界待真实环境验证。

### 3. 能力发现 (Capability Discovery)
- **实现位置**：[`crates/superfluid-daemon/src/capabilities.rs`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-daemon/src/capabilities.rs) 与 [`crates/superfluid-adapters/superfluid-adapter-llamacpp/src/recipe.rs`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-adapters/superfluid-adapter-llamacpp/src/recipe.rs)
- **分析**：`capabilities.rs` 接收运行时通过 Link W hello 报告的 JSON，支持 `Yes / No(reason) / Unknown` 三态、limits 与能力变化比较。该文件不能单独证明动态显存遥测已接通。特别注意 `Capabilities::require` 对 `Unknown` 返回允许：这是运行时功能兼容策略，**不能照搬为 Agent24 权限或 LocalOnly 门禁**；安全能力未知时应遵循 Agent24 自身的拒绝/人工确认规则。借鉴的是能力描述与一致性测试方法，不是照抄默认放行语义。

### 4. 路由机制 (Routing)
- **实现位置**：[`crates/superfluid-daemon/src/runtime_pick.rs`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-daemon/src/runtime_pick.rs) 及 [`crates/superfluid-daemon/src/fleet_manager.rs`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-daemon/src/fleet_manager.rs)
- **分析**：包含格式适配与负载分流两层：
  1. 路径格式匹配：根据路径特征识别后端（`.base` 对应 baseRT，GGUF 对应 llamacpp，包含配置的目录对应 MLX）；
  2. 负载分派：当本地 Worker KV 池接近压力阈值时，FleetManager 可将无状态会话派发至远端空闲节点。

### 5. 生命周期与进程监控 (Lifecycle & Watchdog)
- **实现位置**：[`crates/superfluid-daemon/src/scheduler.rs#L1518-L1547`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-daemon/src/scheduler.rs#L1518-L1547)
- **分析**：Worker 作为独立子进程运行。当检测到底层 Worker 崩溃或传输错误时，限制最多连续重启 3 次（`MAX_WORKER_RESPAWNS = 3`）。需特别注意：在触发 respawn 前，调度器会清空所有正在进行的 lanes 并返回 `LaneEvent::Failed`，导致原 HTTP 请求中断报错，原流并不具备透明自动续传保证。

### 6. 调度与资源分配 (Scheduling & Resource Allocation)
- **实现位置**：[`crates/superfluid-daemon/src/scheduler.rs#L1958-L2015`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-daemon/src/scheduler.rs#L1958-L2015) 与 [`crates/superfluid-daemon/src/lib.rs#L73-86`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-daemon/src/lib.rs#L73-L86)
- **分析**：
  - **时隙推进**：基于自适应 Tick 时隙运行，时隙目标时间并非固定区间，依据运行时类型、空闲 lane 状态与 Finishing 车道动态变化（测试与代码中出现从 50ms 到数秒不等）；
  - **4 级 QoS 类别**：源码规范定义为 `interactive chat` (0)、`inline completion` (1)、`foreground agent` (2)、`background agent` (3)（第三类为前台 Agent 任务，非 batch），配备老化机制（Aged Priority）避免低优先级饥饿；
  - **抢占机制**：高优先级任务到达时，调用 `preempt_lane` 将低优先级车道置为 `Finishing`。退休车道的 prefill 数据可留存在内存 prefix cache 中供后续重用；磁盘持久化 `--park` 默认关闭（OFF by default），抢占并不自动产生磁盘落盘。

### 7. 流式取消与资源回收 (Streaming Cancellation)
- **实现位置**：[`crates/superfluid-daemon/src/scheduler.rs#L3086-L3120`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-daemon/src/scheduler.rs#L3086-L3120)
- **分析**：客户端 HTTP 连接断开后，底层连接 Drop 将 Session ID 注入 `CancelSet`。调度器在 Tick 循环中扫描并将对应车道置为 `finish::CANCELLED`，向 Worker 发送 `seed_release` 释放 GPU 显存。

### 8. 健康检查与就绪状态 (Health & Readiness)
- **实现位置**：[`crates/superfluid-daemon/src/openai.rs#L1791-L1795`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-daemon/src/openai.rs#L1791-L1795)
- **分析**：当前 `/health` 端点仅直接输出 JSON 数据 `{"status": "ok", "models_loaded": ..., "models_known": ...}`，并无针对底层 GPU Worker 卡死或异常的 503 检查逻辑；卡死检测看门狗与真实就绪探针（readiness probe）仍待代码路径与测试证实。

### 9. 可观测性 (Observability)
- **实现位置**：[`crates/superfluid-daemon/src/tui.rs`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-daemon/src/tui.rs)、[`crates/superfluid-daemon/src/otlp.rs`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-daemon/src/otlp.rs) 与 [`crates/superfluid-daemon/src/metrics.rs`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-daemon/src/metrics.rs)
- **分析**：提供终端 TUI 状态展示，包含活动车道、Prefill/Decode 速率、抢占计数及缓存占用；支持 Prometheus `/metrics` 采集与 OTLP 链路追踪导出。

### 10. 许可证、安全边界与数据副本流向
- **源码与依赖许可**：
  - 主仓库采用 [Apache-2.0](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/LICENSE) 许可；
  - 根据 [`NOTICE`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/NOTICE) 声明：`baseRT` C API headers 与 Rust 绑定属于 Apache-2.0，但其调用的专有 engine library 不在仓库内且另行授权；
  - `docs/assets/fonts`（Gerstner Programm 字体）仅授权该文档站使用，不可复用；
  - `tests/fixtures/chat_templates` 遵从对应上游模型原始协议；Rust 依赖按各 crate 自身声明授权。不能将主仓库开源等同于所有关联组件均可无条件带走。
- **通信与鉴权边界**：
  - 本地回环（127.0.0.1）默认无鉴权，同机无特权进程可能存在越权访问风险；对外提供服务时支持通过配置文件指定 API Key；
  - 多机通信使用 Noise 协议（`Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s`），提供网络传输加密，但不等同于细粒度租户隔离或设备身份认证保证。
- **数据副本流向与隐私设计分析**：
  1. **持久 Session WAL**：调用 Session API 时，完整的 Prompt、生成内容与 Tool 记录以 Postcard 格式写入 `--sessions` 目录下的 `wal.log`（明文未加密）；
  2. **无状态 Ephemeral 请求**：通过标准 HTTP `/v1/chat/completions` 发起的无状态请求在完成后由 `drop_ephemeral_artifact` 清理临时结构，并非所有 HTTP 流量都会留存持久 WAL；
  3. **KV Park**：磁盘 KV 缓存参数 `--park` 默认关闭，仅显式开启并针对会话重开时触发；
  4. **内存缓存**：进程内保留 prefix cache 与 ring buffer，受操作系统生命周期管理；
  5. **日志与遥测**：标准输出及 OTLP 追踪导出中可能携带部分请求元数据；
  6. **远端节点传输**：在 Fleet 模式下，任务数据跨机网络传输至执行节点。
  隐私设计必须区分持久 WAL、临时会话与内存缓存，不能无证据宣称全部持久化或绝对安全。

### 11. 错误处理与降级 (Errors & Fallback)
- **实现位置**：[`crates/superfluid-daemon/src/inference.rs`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-daemon/src/inference.rs)
- **分析**：将内部调度状态映射为标准 API 错误格式（如 Overloaded、ContextTooLong、StreamBroken），在显存与并发超限时执行退避拒绝。

### 12. 模型格式与平台约束 (Model Formats & Platform Constraints)
- **实现位置**：[`crates/superfluid-adapters/`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/crates/superfluid-adapters)
- **分析**：
  - `llamacpp`：支持 GGUF，动态链接系统或外部 `libllama` 库；
  - `mlx`：仅限 macOS Apple Silicon，依赖外部 Python 虚拟环境通过 C API / `dlsym` 交互；
  - `basert`：商业私有 `.base` 格式，与闭源运行时绑定。

### 13. 性能主张与基准套件审计
- **实现位置**：[`docs/performance/benchmarks.md`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/docs/performance/benchmarks.md) 与 [`tools/suite/`](https://github.com/basecompute/superfluid/blob/3a3598d33918e65f605c6eb3547c677b34e42563/tools/suite)
- **审计事实核对**：
  1. 自动化套件代码存在（包含 `track1.py` 与 `track2.py`），覆盖文档列明的场景；
  2. 上游文档分别在两台不同机器上采集：Apple M1 Max（2026-10-06，纯吞吐对比）与 Apple M5 Pro（2026-10-07，复杂 serving 场景对比）；
  3. 吞吐数据方面：MLX 在 8 并发下上游报告为 130.2 vs 96.8 tok/s，而 llama.cpp 吞吐与原生服务端基本持平（150.8 vs 145.1 tok/s）；
  4. 上游在特定 mixed-load 调度配置下报告 0.79s vs 67s；这些数字不足以单独证明机制因果或同等调度策略下的性能差距，本轮未复跑。

---

## 二、架构分工与职责对照：Agent24 vs iDoris vs Superfluid

在考察 Serving Daemon 机制时，必须严守架构边界，区分当前现状与未来规划设计：

| 领域 / 机制 | Agent24 当前现状（基线 `f16e65a9`） | Agent24 / iDoris 目标规划设计 | Superfluid 现有做法与边界评价 |
|---|---|---|---|
| **模型网关与路由治理** | [`ADR-032 L57`](https://github.com/iDoris-ai/Agent24/blob/f16e65a95f9732f4e00cb288f967e40e0978eb7b/docs/design/INTEGRATION-AGENTEAR-IDORIS.md#L57) 明确 `IDORIS_URL` **尚未接线**；`agent24-models` 已有通用 provider/router 能力及 oMLX / Ollama 配置；不能据这些接缝推定 iDoris 专属隐私/预算整链已接通。 | 由 iDoris 管理多 Provider 链路、成本与隐私降级（Remote vs Local）；Agent24 仅声明任务属性。 | Superfluid 仅在单一 Daemon 内部维护本地模型路由，不涉及企业级云端降级策略。 |
| **本地推理进程托管** | 内核直接发起 loopback HTTP 调用，不托管底层 GPU 推理子进程。 | 保持推理引擎与核心 Run/审批状态机解耦；Agent24 核心仍拥有持久状态，外部推理进程由专属服务管理。 | 采用 Daemon-Worker 双进程架构，通过 IPC 托管底层子进程。 |
| **硬件探测与分档** | `agent24-hw` 实现 5 档硬件探测（HW-TIERS）及显存保留。 | 结合系统运行态压力动态调整模型加载与并发。 | 有运行时能力与资源调度机制；是否覆盖 Agent24 的硬件分档需求尚未逐项验证。 |
| **任务 QoS 与优先级** | 未接线外部 QoS 请求头，无前后台推理优先级区分。 | Agent24 区分交互与后台任务，经 iDoris 或模型层中继至支持 QoS 的后端。 | 支持 4 级 QoS 与时隙抢占（交互首字优先）。机制有借鉴意义。 |
| **多机组网与执行** | 多机协作规划在 Agent / Hyphae 协议层治理，推理层不组网。 | 推理池多机调度下沉至网关层或由外部集群承载。 | Link F 采用 mDNS + Noise NNpsk0 组网，仅限于其自身分布式推理节点。 |
| **权限审批与安全门** | 内核 C4/D3 审批门为**唯一权威**；`agent24-policy` 控制执行权限。 | 权限决策与工具执行完全由 Agent24 内核治理。 | 内部有 `PermissionRequest` 协议；类型存在本身不证明越权。接入时必须保证请求不替代 Agent24 的批准与执行权威。 |
| **会话持久化与状态机** | 当前内核已有事件日志、Run/thread 与会话持久化；P1–P5 是记忆演进计划，不是已交付的状态。 | 会话、回滚与审计统一归属内核记忆存储。 | 内置 Postcard Session WAL；如接入，须定义推理会话与 Agent24 权威会话的映射、生命周期和清除边界。 |

---

## 三、研究建议与候选实验矩阵

> [!IMPORTANT]
> 本矩阵仅为**架构研究建议与后续候选实验**，不代表项目已批准采用，未进入生产实现，亦不作为记忆 P1 的前置依赖。

| 机制候选项 | 评估结论 | 依据与权衡分析 | 后续建议与验证门槛 |
|---|---|---|---|
| **QoS 调度机制映射** (`x-superfluid-qos`) | **候选实验** | 解决前台用户交互被后台批处理阻塞的痛点，但需通过标准请求头或配置适配。 | 仅建议作为模型客户端候选特性进行验证；必须先在本地公平对比不同配置收益。 |
| **进程隔离架构模式** | **机制借鉴** | 跨进程隔离（Daemon 控制面 + Worker 推理面）能有效隔离驱动崩溃风险。 | 借鉴其主从分离与故障隔离架构思路，用于 Agent24 外部进程纳管设计。 |
| **共享内存 IPC** (Link W) | **待测瓶颈再定** | 共享内存环仍包含 `copy_nonoverlapping` 和缓冲编码开销，在缺乏真实大 payload 性能瓶颈前过早引入会增加跨平台复杂性。 | 必须测出 Agent24 存在显著跨进程传输瓶颈后，再行评估是否引入。 |
| **Noise NNpsk0 组网思路** | **设计参考** | 局域网轻量加密通信设计清晰，但分布式协作在 Agent24 属于 Hyphae 通信层范畴。 | 留作分布式节点通信的设计参考，短期不予引入。 |
| **macOS 系统内存压力监听** | **建议借鉴** | 通过系统内核接口感知内存压力（`kern.memorystatus_vm_pressure_level`）实现优雅退避。 | 建议在后续硬件层优化中评估接入 `agent24-hw` 动态感知。 |
| **Superfluid 完整 Serving 依赖引入** | **暂不引入** | 处于 pre-1.0 阶段，另有运行时许可与运维成本；本轮未验证 Agent24 实际收益，不据上游单组数据决定引入。 | 保持技术跟踪，不作为系统硬依赖。 |
| **Superfluid 内置 Session WAL / 权限事件** | **不直接复用为核心权威** | 需要额外定义授权与数据副本生命周期，不能代替现有内核。 | 推理会话可以作为后端实现细节，但业务批准、审计和用户记忆权威仍归 Agent24。 |
| **baseRT 专有引擎及格式** | **本阶段不建议引入** | 需单独核验许可、可分发性、可替代性及收益。 | 这是研究建议，不代表用户已决定永久排除该引擎。 |

---

## 四、候选实验方案与建议量化门槛

> [!CAUTION]
> **资源约束与执行原则**：
> 1. 本轮工作为纯静态源码研究，**未执行任何基准测试（Benchmark）**；
> 2. 后续若获准进行实验，**仅可在未来获准的本地开发笔记本上使用隔离合成数据进行，严禁在 Mac Mini 上执行任何任务**（严格遵守 AGENTS.md 约定）；
> 3. **记忆 P1 优先级绝对优先**：实验仅作为候选储备，在记忆 P1 验收前不投入生产编码，不阻塞主线进度。

### 1. 候选实验设计（公平对照）
- **目标**：验证在相同硬件与同等并发压力下，QoS 抢占机制相较于无优先级排队是否能在可接受的后台惩罚下显著降低交互延迟。
- **环境建议**：
  - 硬件：未来获准的开发笔记本；
  - 模型：开放 GGUF 模型（如 Qwen2.5-Coder-7B 或 Qwen2.5-3B）；
  - 对照组设计：
    - **对照组 A**：原生 `llama-server`（记录实际调度配置，不预设必为纯 FIFO）；
    - **对照组 B**：Superfluid 搭载 `llama.cpp`（未启用 QoS 区分，基准排队）；
    - **实验组 C**：Superfluid 搭载 `llama.cpp`（后台标记 `background`，前台交互标记 `interactive`）。

### 2. 建议量化指标（待工作负载与稳定性明确定义后的建议门槛，非测量结果）
- **交互延迟改善建议**：实验组 C 的首字延迟（TTFT）在后台长任务负载下建议达到 $\le 1.5\text{ s}$；
- **后台吞吐损失建议**：后台长任务因被抢占导致的整体完成耗时增加建议控制在 $\le 15\%$ 以内；
- **稳定性指标建议**：需设计长时间混合并发压力测试验证无显存泄漏与无 Token 丢失（不能仅凭 20 次短测试推断长期无泄漏）。

---

## 五、技术路线图与主线优先级约束

根据 Agent24 当前的里程碑规划，技术演进节奏严格约束如下：
1. **主线优先级**：
   - 主线是 **Agent24 完整的交互与执行流程**；当前核心重点为记忆 P1（个人可控记忆），不是将全部主线等同于记忆组件；
   - P1 交付不依赖 Superfluid 或任何外部 Serving 基础设施的变更。
2. **渐进式吸收原则**：
   - **优先吸收机制设计与测试套件思想**（如能力自省契约、结构化测试断言、故障回收语义）；
   - 不整包引入外部二进制服务；
   - 保持 Agent24 内核权威边界，不改变当前主线开发节奏。

---

## 六、S01–S10 独立审查逐条整改与回应矩阵

下表完整保留本轮独立审查的 S01–S10 意见及整改回应；不依赖本机临时报告路径，也不把完成返修等同于获得批准：

| 审查编号 | 审查意见摘要 | 原稿缺陷分析 | 本次返修整改动作 | 报告对应章节 |
|---|---|---|---|---|
| **S01** | M5 Pro 已于 2026-03-03 正式发布，不可推断为仿真 | 原稿断言 M5 Pro 属于“内部仿真或未公开环境”存在事实错误 | 删除全部仿真推断；明确 M5 Pro 已正式发布；澄清上游分别在 M1 Max（吞吐）与 M5 Pro（serving）报告数据，本轮未复跑，仅作为声明引用 | 摘要、一.13 |
| **S02** | 完整审计声明超过证据范围，测试集非完备 | 原稿宣称“完整源码、测试集和基准套件审计”以及 suite“完备” | 调整为“固定 SHA 选定源码静态抽查”，明确未执行上游项目且未完成全量安全审计；套件表述调整为“覆盖文档列明场景” | 顶部元数据、摘要、一.13 |
| **S03** | 吞吐、QoS 与竞品结论混淆，未保留测试上下文 | 笼统断言开放后端无提升；误称严格依赖客户端头；断言竞品若有机制则无差距缺乏证据 | 明确保留 MLX 8 并发 130.2 vs 96.8 上游数据与测试配置；说明 QoS 支持 `--http-default-qos` 配置；指明缺少同策略对照，收益未验证 | 摘要、一.6、一.13 |
| **S04** | 把 WAL 基准测试缺口误写为恢复能力未打通 | 混淆了 `tools/suite` 的 HTTP 重发局限与仓库源码的恢复实现 | 澄清 suite 局限在于未测 session WAL resume；明确仓库包含 `tests/wal_crash.rs` 崩溃恢复测试源码（本轮未运行），基准缺测不构成代码无法恢复的证据 | 摘要、一.13、矩阵 |
| **S05** | 扩大了进程重启、流恢复与健康检查保证 | 误将进程 respawn 当作原 HTTP 流透明续传；误称 `/health` 会在 worker 异常时返回 503 | 引用源码指出 Tick 出错后会发送 `LaneEvent::Failed` 并清空 lanes（当前请求失败）；指出 `/health` 仅返回 OK 与模型数 JSON，非 Worker 就绪证明 | 摘要、一.5、一.8 |
| **S06** | 抢占不等同于落盘，共享内存并非端到端零复制 | 误称每次抢占 Park 存盘；误称 SHM 消除 CPU 开销与端到端零拷贝 | 明确 `preempt_lane` 仅标记 Finishing 并退让，prefill 页进内存 prefix cache，`--park` 默认关闭；指出 SHM 存在 `copy_nonoverlapping` 与缓冲开销 | 摘要、一.6、三 |
| **S07** | 采用矩阵冒充项目批准，混淆当前实现与规划 | 原稿使用“采用”“侵入极小”等表述；未核实 `IDORIS_URL` 实装；从类型名推断越权 | 改为“研究建议/候选实验”；明确 `IDORIS_URL` 在 ADR-032 中明确未接线；分工拆为现状与目标；强调内核 C4/D3 审批门权威设计 | 二、三 |
| **S08** | 实验方案提及 Mac Mini 违反项目边界 | 原稿在实验机型建议中提及 Mac Mini M4，违反 AGENTS.md 约定 | 明确本轮无 benchmark；未来实验仅限获准的本地开发笔记本，明确严禁在 Mac Mini 上执行；阈值标为待定义的建议门槛 | 四 |
| **S09** | 缺失 LICENSE / NOTICE 边界核查，数据副本边界不全 | 仅提 Apache-2.0，忽略 baseRT 另行许可与字体限制；误称每次 HTTP 必留 WAL | 详列 NOTICE 资产边界；梳理持久 WAL、无状态 Ephemeral、KV Park、内存缓存、日志遥测与远端节点等副本流向，明确各机制适用范围 | 一.10 |
| **S10** | 链接格式错误、QoS 类别名不符、存在绝对化表述 | 包含本地绝对路径链接；QoS 类名误写 batch；主观夸饰用词较多 | 替换为固定 GitHub permalink；按源码订正为 `interactive chat`, `inline completion`, `foreground agent`, `background agent`；修正自适应 Tick 说明；删除主观夸饰用语 | 顶部元数据、一.6、全篇 |

> **机械校验与本轮基准执行声明**：  
> 1. 本报告引用的所有 Superfluid 源码路径已通过固定 commit [`3a3598d33918e65f605c6eb3547c677b34e42563`](https://github.com/basecompute/superfluid/tree/3a3598d33918e65f605c6eb3547c677b34e42563) 树文件机械检查真实存在；Agent24 路径经基线 [`f16e65a95f9732f4e00cb288f967e40e0978eb7b`](https://github.com/iDoris-ai/Agent24/tree/f16e65a95f9732f4e00cb288f967e40e0978eb7b) 机械核验一致。  
> 2. **本轮工作未运行任何性能基准测试（No Benchmark Executed）**，所有性能数字均严格标明为上游文档声明。  
> 3. 本文为调研与架构审查返修草稿，**非 approved 状态**，未经用户与架构评审批准，不作为任何生产系统变更依据。
