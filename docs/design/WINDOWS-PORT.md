# DEP-A7 —— Windows 移植设计

> 状态：**草稿，待 1 轮评审后冻结**（`docs/Deployment/TASKS.md` DEP-A7）。只写设计，不含实现。
> 依据：`docs/Deployment/RESEARCH.md` §1.4 / §1.7 / §2 / §4；`TASKS.md` 的 A7、C2、C3；ADR-031（进程外模块）、
> ADR-032 / `A3-ATTACHED-MODULE.md`（附着模块）、`docs/specs/WIRE-OOP-MODULE.md`；仓库实查（基线 `bf3322c`）；
> sidecar-host 线只读查看（`origin/integration/open-design-main-sync-wave20`、#549、#553）。
> 范围：Rust 侧（`agent24d` / `agent24` CLI / 模块 SDK）能在 `x86_64-pc-windows-msvc` 上编译、测试，并让 Sin90 能挂载。
> 桌面端 NSIS 安装包和签名属于 C3，不在本文范围内。

## 0. 结论摘要

- **模块传输选命名管道**（§2）。tokio 和 Node 都原生支持；`GetNamedPipeClientProcessId` / `GetNamedPipeServerProcessId`
  可以直接替代 `peer_cred`；不需要句柄继承，因此 Windows 上不需要 fd 3，也不需要跳板。
- **wire 帧不变**：NDJSON 回调、`initialize`、HTTP 代理、A3 握手的字节格式一个都不改。只在 Windows 上改 **启动环境**：
  用 `A24_LISTEN_PIPE` 替换 `A24_LISTEN_FD`；`A24_CALLBACK_SOCK` 和 A3 的 `socket_path` 仍是同一个字段，只是值变成管道名。
  Unix 上的行为逐字节不变。
- **进程监督复用 ProcessKit**（Job Object 整树回收 + `spawn_isolated_piped`）。前置条件写死：**W7 开工前，sidecar 批 3 必须已合进 main**。
  其余分片不依赖它，可以先做（§7）。
- C2 拆成 W0–W10 共 11 片，每片 ≤300 行。CI 路线：手动 check → `check`+`clippy` → 串行 `test` → 黑盒测试（§8）。

## 1. 实查结果（对 RESEARCH §1.4 的核实与补漏）

`grep -rln std::os::unix` 去掉 `tests/` 目录后，**确实是 20 个文件**。按 `mod tests` 的位置再区分一次：

- **7 个文件只在测试代码里用到**：`domain.rs`、`os_config.rs`（生产代码里只有一处 `#[cfg(unix)]` 目录 fsync）、
  `os_routes.rs`、`supervisor.rs`、`memory/reconcile.rs`、`tools/local.rs`、`os-packages/discovery.rs`。
  这些在 Windows 上只需要给测试加 `cfg`。
- **13 个文件在生产代码里真正使用**，逐项处置见 §6。

RESEARCH 列出了 `proxy.rs` 的 `UnixStream`、`state_dir` 只读 HOME、`command-fds`/`close_fds`、`same_device`、
`server.rs:966` 信号、`service.rs` launchd。这些都核实无误。另外查出 **6 处遗漏**：

| 遗漏 | 位置 | 后果 |
|---|---|---|
| 直接读 `/dev/urandom` 铸 token | `launch.rs:281`、`proxy.rs:902,928`、`kernel_call.rs:204`、`agent24d/module_approval_broker.rs:128` | 能编译，**运行时**在 Windows 上铸不出 token，模块无法启动，审批失败 |
| `rustix` 的 `process`/`fs` 特性只支持 Unix | os-proto、os-packages、os-fd、agent24d（`geteuid`、`kill_process_group`、`waitid`、`OFlags`） | 编译失败（macOS 交叉 check 已证实 os-fd 有 9 处错） |
| `tokio::net::Unix*` 出现在另外 3 个文件 | `kernel_call.rs`、`scheduler_deliver.rs`（测试）、`server.rs` | 与 proxy 同类 |
| 回调根目录退回到 `/tmp/a24-run-*` | `server.rs:1886 callback_root` | 管道方案下 Windows 不需要这条路径（§3.1） |
| 子进程环境白名单只含 Unix 变量 | `tools/env_whitelist.rs`（shell_exec + MCP）、`launch.rs:50 INHERITED_ENV` | 缺 `SystemRoot`，Winsock 和加密初始化都会失败（#549 的教训，§6.2） |
| Node 参考模块用 `listen({fd: 3})` | `examples/node-module` | Node 在 Windows 上**不支持** listen fd，fd 继承路线对 Node 无效 |

`cfg(not(unix))` 分支目前共 5 处：`server.rs:966`、`state_file.rs:244`、`install.rs:332/403/416`。

**macOS 交叉预览**：用 rustup 的 stable 加 `x86_64-pc-windows-gnu`，执行 `cargo check --workspace --keep-going`。
`ring` 和 `libsqlite3-sys` 的 build script 失败（没有 C 工具链）；`command-fds`、`close_fds`、`agent24-os-fd` 编译失败；
只有 `agent24-protocol`、`agent24-core`、`agent24-domain` 通过。依赖失败的 crate 下游不会被 check，所以这份预览**不能代替** W0 的 Windows runner 摸底。

## 2. 模块传输选型

进程外模块有两条通道：**回调**（模块连接内核，NDJSON）和**入站**（内核连接模块，HTTP；Unix 上是内核 bind 好、经 fd 3 交给模块的 UDS）。
A3 附着模块只有一条常驻监听（`~/.agent24/attach/agent24d.sock`）。

| 维度 | (a) Windows AF_UNIX + 句柄继承 | **(b) 命名管道** | (c) loopback TCP + token |
|---|---|---|---|
| peer 鉴权 | `SIO_AF_UNIX_GETPEERPID` 拿到 pid 再查 SID（最低系统版本待实测）；socket 文件的 ACL 继承自目录 | **管道 DACL 只允许本用户**；`GetNamedPipeClientProcessId`/`ServerProcessId` 拿 pid 再查 SID，还能进一步判断「是否在本代 Job 里」，**比 Unix 更强** | 任何本机用户都能连接。回调方向的 `accept_one` 可能被抢先连接导致 DoS；入站 HTTP 可以绕过内核的受约束代理，直接调用模块 |
| 异步运行时 | tokio / mio **不支持** Windows AF_UNIX，需要自己接 IOCP 或用阻塞线程桥接 | `tokio::net::windows::named_pipe` 原生支持，实现了 `AsyncRead`/`AsyncWrite` | tokio 原生 |
| 句柄 / fd 传递 | std 稳定版不支持 `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`（`raw_attribute` 仍是 unstable）；std 的 `bInheritHandles=TRUE` 会把可继承 socket 漏给并发 spawn 的 shell_exec/MCP 子进程；AF_UNIX socket 句柄跨进程继承没有官方保证（官方推荐 `WSADuplicateSocket`） | **不需要**：模块按名字自己创建或连接 | 同 (a)，或改成模块自己 bind 再上报端口 |
| SDK 影响（`agent24-os-sdk`/`agent24-os-fd`） | os-fd 改为接收 SOCKET 句柄（新增 unsafe）；SDK 不变 | os-fd 在 Windows 上不参与；proto 的 `take_listener()` 换成按名字建管道（§4）；SDK 只改 1 行 | 模块必须校验每个入站请求的 token 头，**所有模块（含 Node）都要改** |
| Node 模块 | libuv 不支持 Windows AF_UNIX，**两个方向都不可用** | `net.connect(pipe)` 可直接用；入站改 `listen(pipe)`，只改 1 行 | 可用 |
| wire 是否要改 | 帧不变；`A24_LISTEN_FD` 的值变成句柄号 | 帧不变；Windows 上换成 `A24_LISTEN_PIPE`，并要求「initialize 之前先建好第一个管道实例」 | **要改**：入站请求要带鉴权头，模块侧要校验 |
| 实现量 | 大（IOCP 适配 + 自写 CreateProcess） | 中（一个 unsafe 边界小 crate + 2 个 cfg 类型别名 + 监听循环） | 小（但安全上要补的东西最多） |
| 测试难度 | 高，只能在原生 Windows 上测，而且 runner 版本有依赖 | 中，原生测试需要串行（#553） | 低 |

**推荐 (b)**。三个原因：
1. 只有 (b) 在 tokio 和 Node 两边都原生可用；
2. 只有 (b) 不需要句柄继承，因此不用碰 `HANDLE_LIST`/`CreateProcess`，可以直接接上 ProcessKit 的隔离 spawn；
3. 鉴权可以做到「DACL + pid→SID + Job 成员」三层，强度 ≥ Unix 的「0700 目录 + peer_cred」。

代价是启动环境在 Windows 上有一处差异（`A24_LISTEN_PIPE`），需要在 WIRE-OOP-MODULE §2 补一个 Windows 小节。
(c) 被否决：它要求每个模块都承担鉴权，这违背「模块信任内核代理」的现有前提。

## 3. 推荐方案细节

### 3.1 回调方向（模块 → 内核）

- **管道名**：`\\.\pipe\agent24-<u>-<pid>-cb-<n>`。`<u>` 是当前用户 SID 的短哈希，`<pid>` 是 daemon 的 pid，`<n>` 是代号，
  与 Unix 的 `run/<pid>/<n>.sock` 一一对应。管道名不受 103 字节限制，因此 `callback_root` 的 `/tmp` 回退在 Windows 上**整段不编译**。
- **创建方式**：内核用 `first_pipe_instance(true)` + `reject_remote_clients(true)` + 显式 DACL（`D:P(A;;GA;;;<本用户 SID>)`）创建。
  名字被人抢先占用时直接失败，等价于 Unix 的「目录不安全」拒绝。
- **`accept_one`**：`connect().await` → `GetNamedPipeClientProcessId` → 校验 pid 在本代 Job 里且 SID 等于本用户 → 交给握手。
  只建一个实例，连接后不再建新实例，因此「每代只服务一条」（D1）在结构上依然成立。
  校验失败返回 `EndpointError::ForeignPeer`，这一代失败，语义与 Unix 相同。
- **模块侧**：`UnixStream::connect(callback_sock)` 换成 `ClientOptions::new().open(name)`，遇到 `ERROR_PIPE_BUSY` 时有界重试。
  Node 的 `net.connect(path)` 对管道名天然适用，**不用改**。

### 3.2 入站方向（内核 → 模块 HTTP）：fd 3 的替代

- 内核在 spawn 时铸一个管道名：`\\.\pipe\agent24-<u>-<pid>-m-<n>-<128 位随机 hex>`，通过 **`A24_LISTEN_PIPE`** 传给模块。
  Windows 上不设置 `A24_LISTEN_FD`，`A24_*` 变量仍然是 4 个。
- **模块自己创建管道**，用 `first_pipe_instance(true)` + owner-only DACL，并且**必须在发送 `initialize` 之前建好第一个实例**。
  内核只在握手成功后才会代理请求，所以「initialize 之后入站一定可连」这一条与 Unix 等价。
- **内核每次连接上游**（`proxy.rs` 的 `Upstream::connect`、`kernel_call.rs`）：`ClientOptions::open`，遇到 `PIPE_BUSY` 有界重试 →
  `GetNamedPipeServerProcessId` 必须属于本代 Job，否则断开并记 `foreign_upstream`。
  这条检查替代了 Unix 上「listener 由内核 bind」所提供的防冒名保证；即使同用户的其他进程抢注了这个名字，也会被这条检查拒绝。
- **模块死亡**：管道实例随进程消失，新连接立即返回 `FILE_NOT_FOUND`，等价于 Unix 的「连接被拒、不排队」。

内核侧代码形状采用 cfg 类型别名，不引入 trait 对象，Unix 分支逐字不变。下面的签名已在 macOS 上用 `cargo check` 编译通过，
目标包括本机和 `x86_64-pc-windows-gnu`（tokio 1.53.1、axum 0.8.9）：

```rust
#[cfg(unix)]    pub type CallbackStream = tokio::net::UnixStream;
#[cfg(windows)] pub type CallbackStream = tokio::net::windows::named_pipe::NamedPipeServer;
#[cfg(unix)]    pub type UpstreamStream = tokio::net::UnixStream;
#[cfg(windows)] pub type UpstreamStream = tokio::net::windows::named_pipe::NamedPipeClient;
// handshake/mux：into_split() 的 OwnedReadHalf 改用 tokio::io::split 的 ReadHalf<CallbackStream>
pub type CallbackReader = tokio::io::BufReader<tokio::io::ReadHalf<CallbackStream>>;
```

### 3.3 进程监督（`launch.rs` / `supervise.rs`）

| Unix 现状 | Windows 方案 |
|---|---|
| 跳板进程：`close_fds` 把 ≥4 的 fd 设为 cloexec 后再 exec | **不需要跳板**。ProcessKit 的 `spawn_isolated_piped` 只让 stdin/stdout/stderr 三个句柄被继承（其余句柄一个都不漏），直接 spawn 模块程序 |
| `process_group(0)` + `killpg` | ProcessKit `ProcessGroup`（Job Object）：Job 关闭时自动 kill；用 `active_process_count == 0` 判断整棵进程树已空（照搬 sidecar `owner.rs` 的 `tree_is_empty`，不用 pid 快照） |
| SIGTERM → 宽限期 → SIGKILL | **软停 = 内核关闭回调连接**。WIRE §7.1 已规定模块读到 EOF 必须退出，SDK 也已经实现，所以不用改协议；宽限期满后 `TerminateJobObject`。不用 `CTRL_BREAK`：daemon 可能没有控制台（服务或桌面 sidecar），模块也不一定共享控制台 |
| `waitid(WNOWAIT)` + `ExitStatusExt::signal` | `Child::try_wait` + Job 计数；`Exit.signal` 在 Windows 上恒为 `None` |
| `INHERITED_ENV` 白名单 | 按平台各一份：Windows 至少需要 `SystemRoot`、`windir`、`PATH`、`PATHEXT`、`TEMP`、`TMP`、`USERPROFILE`、`LOCALAPPDATA`、`APPDATA`、`ComSpec`；**是否加 `PSModulePath` 由 W8 的对照测试决定**（§6.2） |

### 3.4 unsafe 边界

工作区设置了 `unsafe_code = "forbid"`，只有 `agent24-os-fd` 是 `deny` + 一处 `allow`。Windows 需要的 FFI 有：
DACL/SDDL → `SECURITY_ATTRIBUTES`；`GetNamedPipe{Client,Server}ProcessId`；`OpenProcessToken`/`GetTokenInformation`/`EqualSid`。
这些集中放进**一个新 crate `agent24-os-win`**：只在 `cfg(windows)` 下编译、`deny(unsafe_code)`、每处 `unsafe` 写清 SAFETY 注释。
它只导出安全 API：「建私有管道」「取对端 pid」「pid 是否本用户」。
不并入 os-fd，是为了保住 os-fd「唯一把继承 fd 变成 socket 的地方」这一定位（J-S3）。
Job 成员判断走 ProcessKit。W3 开头先评估 `interprocess` crate 能否免掉自写 FFI；能的话就不新建 crate，评估结论写进 PR body。

## 4. SDK 侧：`take_inherited_listener` 在 Windows 上的语义

- `agent24_os_fd::take_inherited_listener` **只在 `cfg(unix)` 下存在**。Windows 上没有「继承来的 fd」这个概念，
  不提供一个永远失败的同名函数去误导调用者。
- `agent24_os_proto::module::take_listener()` 签名不变，仍返回 `InheritedListener`，内部改为按平台分支：
  - Unix 分支不变；
  - Windows 分支：读取 `A24_LISTEN_PIPE`，用 `first_pipe_instance(true)` 建第一个实例，包装成 `PipeListener`；
  - 保留「每进程只能调用一次」的原子标记；
  - 错误新增 `NotAPipeName`，以及 `Squatted`（首实例创建失败，说明有人抢注），复用现有 `ListenError` 枚举。
- `PipeListener` 实现 `axum::serve::Listener`。accept 循环：先 `connect` 当前实例，再建好备用实例，然后交出当前实例。
  该实现已与 §3.2 一起在 Windows 目标上 check 通过。
- SDK 唯一的改动是 `module.rs:198`：`axum::serve(listener.into_tokio(), app)` 在 Windows 上换成 `into_pipe()`，用 cfg 分支。
  Sin90/Cos72 只依赖 SDK（J-S1），**模块源码零改动**，Windows 上重新编译即可。
- Node 参考模块只改一行：`process.env.A24_LISTEN_PIPE ? listen(pipe) : listen({fd: 3})`。
  Node 用 libuv 默认 DACL 建管道（Everyone 有读权限，没有写权限，因此无法发出请求；但可能占用实例造成 DoS）。
  所以 Node 在 Windows 上标为 best-effort，不计入 C2 验收。

## 5. A3 附着 socket 与鉴权

- 监听：`\\.\pipe\agent24-<u>-attach`，常驻多实例。DACL 只允许本用户，并开启 `reject_remote_clients`。
  启动时创建首实例失败，说明另一个 daemon 正在运行或有人抢注：附着监听**降级并记录错误**，与 A3 §5.6 中「连得上 → 另一 daemon 在跑」的处理方式相同。
- 鉴权：每条连接执行 `GetNamedPipeClientProcessId` → SID 必须等于本用户，替代 `peer_cred().uid()`。
  附着模块不是内核 spawn 的，不在 Job 里，所以**只做 SID 校验**；之后照旧走 token 握手（只存 sha256）。
- 注册响应里的 `socket_path` 在 Windows 上返回管道名。A3 §4.1 已经要求模块保存这个值而不是写死路径，因此 **wire 不变**。
- Unix 上的残留 socket 节点清理（unlink 后再 bind）在 Windows 上不需要：管道没有文件系统节点。
- AgentEar 当前只支持 macOS，W9 只用测试客户端验收。

## 6. 逐文件处置表

### 6.1 生产代码（13 个文件 + 遗漏项）

| 文件 | 现用途 | Windows 方案 | 分片 |
|---|---|---|---|
| `protocol/state_file.rs` | `state_dir()` 只读 HOME；文件 0600；`pid_alive` 用 `ps` | `state_dir()` 改读 `%LOCALAPPDATA%\Agent24`（不加 `dirs` 依赖，变量缺失时返回 None，与 HOME 缺失同语义）；0600 降级为依赖 LOCALAPPDATA 默认的用户私有 ACL；`pid_alive` 用 `tasklist /FI "PID eq n"`（沿用 `ps` 回退的写法，不需要 unsafe） | W1 |
| `domain` 的 `data_dir` 解析 | manifest 中固定写 `~/.agent24/os/<name>/` | 把 `~/.agent24/` 前缀映射到 `state_dir()`，manifest 字符串不变 | W1 |
| `cli/service.rs` | launchd plist + `launchctl` | **本期明确不支持**：`cfg(target_os = "macos")`，其他平台的 `service install` 报 `unsupported on this platform`（Linux 同样处理，与 A8 Release Notes 一致）；Windows 服务另行立项 | W1 |
| `agent24d/server.rs` | `:946` SIGTERM/SIGINT；`:1813` 回退目录私有性；`callback_root` | 信号：Windows 分支用 `ctrl_c` + `ctrl_break` + `ctrl_close` + `ctrl_shutdown`（已 check 通过），停机仍以 `POST /api/v1/shutdown` 为主；`callback_root` 和 `secure_fallback_dir` 只在 `cfg(unix)` 下编译 | W1 / W5 |
| `agent24d/lifecycle.rs` | O_NOFOLLOW\|O_NONBLOCK 读取；0700 目录 | `custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)`（std 的安全 API）再检查不是重解析点；0700 降级（继承 ACL） | W1 |
| `agent24d/attached.rs` | 0600 写入、chmod hook、目录 fsync | 0600/chmod 降级；目录 fsync 在 Windows 上跳过（std 无法以普通方式打开目录句柄做 fsync），注释写明 | W1 |
| `os-packages/lib.rs` | 包根 0700 + uid 属主检查（`rustix::geteuid`） | 属主检查 Windows 上降级为「是真实目录且不是重解析点」；uid 检查改走 `agent24-os-win` 的 SID 比较，W3 落地后补上 | W2 / W3 |
| `os-packages/install.rs` | 0700 建目录、去掉 g/o 写位、`same_device` | `same_device` 在 Windows 上返回 `Some(true)`，注释写明理由：staging 按构造就在目标父目录内，且 std `rename` 用的 `MoveFileExW` 不带 `COPY_ALLOWED`，跨卷时直接失败而不是拷贝，原子性不会静默丢失；解决 RESEARCH 中「Windows 上 os install 必拒」的问题 | W2 |
| `os-proto/launch.rs` | 跳板、`command-fds`、`close_fds`、可执行位、`/dev/urandom` | 跳板与 fd 映射只在 `cfg(unix)` 下编译；Windows 走 §3.3；可执行判断改为「`.exe` 扩展名 + 是普通文件」，`spawn.command: bin/sin90` 在 Windows 上按 `bin/sin90.exe` 解析（manifest 不变）；token 改用 `getrandom` 0.3（锁文件里已有） | W2 / W7 |
| `os-proto/supervise.rs` | 进程组信号、`waitid`、SIGCHLD | §3.3：ProcessKit Job | W7 |
| `os-proto/endpoint.rs` | 回调 UDS、0700 目录、`peer_cred` | §3.1 | W5 / W6 |
| `os-proto/module.rs` | 模块侧 connect、`InheritedListener` | §3.1 和 §4 | W5 / W6 |
| `os-proto/proxy.rs`、`kernel_call.rs` | 上游 `UnixStream::connect`、`/dev/urandom` | §3.2；`getrandom` | W2 / W6 |
| `os-fd/lib.rs` | fd 3 接管（唯一 unsafe） | 整个 crate 只在 `cfg(unix)` 下编译；依赖它的 proto 改为 `[target.'cfg(unix)'.dependencies]` | W2 |
| `agent24d/attach_listener.rs` | A3 UDS 和 `peer_cred` | §5 | W9 |
| `agent24d/module_approval_broker.rs` | `/dev/urandom` | `getrandom` | W2 |
| `tools/env_whitelist.rs` + `local.rs` | shell_exec 和 MCP 的 env 白名单 | §6.2 | W8 |
| 依赖：`command-fds`、`close_fds`、`rustix` 的 process/fs 特性、`libc` | 只能在 Unix 上用 | 全部挪到 `[target.'cfg(unix)'.dependencies]` | W2 |

### 6.2 shell_exec / MCP：吸取 #549 的 env 教训

- **shell_exec 保持 argv 直接执行、不经过 shell**。这是安全属性（`shell_exec_runs_argv_without_shell_interpretation`），
  Windows 上不改成默认走 PowerShell 或 cmd。LLM 需要 shell 时自己传入
  `["powershell","-NoProfile","-NonInteractive","-Command",…]`，Windows 上的工具描述里提示这种写法。
- `CHILD_ENV_WHITELIST` 改为按平台各一份。Windows 版的内容见 §3.3；不含任何 `*_KEY`/`*_TOKEN` 或 `A24_*`。
- **#549 的教训**：`env_clear()` 之后只留 `SystemRoot`，PowerShell 的 provider cmdlet（`Test-Path`）在 stdin 保持打开时会挂起；
  换成直接调用 .NET 就不挂。当时归因为「最小环境」，但**仍是未被隔离验证的假说**。
  因此 W8 必须带一组对照测试：①最小白名单；②白名单加 `PSModulePath`/`LOCALAPPDATA`/`TEMP`。
  两组都跑 `powershell -NoProfile -Command "Test-Path ."`，stdin 设为 null，超时 30s，记录耗时。
  测试结论决定 `PSModulePath` 进不进白名单，并写进 `env_whitelist.rs` 的注释。
- MCP 的 `npx` 在 Windows 上实际是 `npx.cmd`，std 只自动补 `.exe`。W8 增加「按 `PATHEXT` 解析」，`.cmd`/`.bat` 由 std 的批处理参数转义执行，不自己拼命令行。

### 6.3 只在测试中使用的 7 个文件，以及 `tests/` 黑盒

给 symlink、权限位、`/bin/sh` 跳板相关的测试加上 `#[cfg(unix)]`，加在整个测试函数上，不在测试内部提前 return（遵循 `domain.rs:3416` 的约定）。
`a3_*`、`me3f`、`me4_*`、`trampoline` 黑盒测试先整体标成 `cfg(unix)`，W10 再补 Windows 版的 Sin90 黑盒测试。负责分片：W2（编译）、W4（测试）。

## 7. 与 sidecar-host / ProcessKit 的关系

- **复用什么**：ProcessKit 的 `ProcessGroup`（Job Object、KILL_ON_JOB_CLOSE、`stats().active_process_count`）和
  `IsolatedPipedCommand::spawn_isolated_piped`（只继承三条 std 管道），以及 sidecar 的 Windows CI 经验：串行测试、
  PowerShell 夹具优先直接调 .NET、`assert!(status.success())` 不能套在 `cmd /C` 外面。
- **不复用什么**：sidecar 是 stdio NDJSON 的一对一父子进程，没有 UDS，也没有 fd 继承。模块传输（§2–§5）是本线独有的，与它不重叠。
- **前置条件（写死）**：**W7（进程监督）开工前，sidecar 批 3 必须已合进 main**。本文对批 3 的界定（仓库中查不到「批 3」的正式定义，请评审确认）是：
  #549（把 `processkit` 钉到 fork rev `60aa827d` 并引入 `spawn_isolated_piped`）+ #553（Windows 原生测试串行化）及其依赖的栈。
  原因有两个：
  1. 工作区只能有一个 processkit 版本；本线要是先引入 crates.io 的 3.3.4，就会与那条线的 git pin 冲突；
  2. 隔离 spawn 的 Windows 行为（#549 R4 尚未查清的挂起问题）应由那条线先收口。
- **不等待时怎样把重复降到最小**：W0–W6、W8–W9 都不依赖 ProcessKit，可以先合。W7 之前，`launch` 在 Windows 上返回
  `LaunchError::Unsupported`（daemon 能启动，模块显示 `unsupported_platform`），**不自己再写一套 Job Object**。
  W7 只在 `agent24-os-proto` 的 `[target.'cfg(windows)'.dependencies]` 中引用与 sidecar 完全相同的 processkit 来源和 rev，不改 sidecar 的任何文件。
- **风险**：#549 的 git pin 会撞上 `deny.toml` 的 `unknown-git = "deny"`。这是那条线要解决的，本线只是跟随，不去改 deny.toml 来抢先放行。

## 8. C2 分片计划

每片一个 PR，≤300 行（不含锁文件和测试夹具）。所有分片都必须在 ubuntu/macOS 上保持全量测试为绿，以此证明 Unix 零回归。

| 片 | 内容 | 依赖 | 规模 | 可证伪验收 |
|---|---|---|---|---|
| W0 | 新增 `windows-check.yml`（§9），手动触发 | — | ~60 | 在 windows-latest 上跑一次，产物 `check.log` 按 crate 分好类，回填到本文 §1（A7 的验收条件之一）；故意把 `#[cfg(unix)]` 写错一处，分类结果里能看到对应 crate |
| W1 | 平台基础：`state_dir`、`data_dir` 映射、service 平台门、信号、`pid_alive`、lifecycle/attached 权限降级 | — | ~250 | Unix 测试全绿；新增单测：`LOCALAPPDATA` 缺失时 `state_dir()==None`；在非 macOS 上执行 `agent24 service install` 退出码非 0，且输出包含 `unsupported` |
| W2 | 编译门：Unix 专有依赖挪到 target 段，os-fd/跳板/UDS 代码加 `cfg(unix)`，Windows 上模块 launch 返回 `Unsupported`；`/dev/urandom` 换成 `getrandom`；`same_device` | W1 | ~280 | windows-latest 上 `cargo check --workspace --all-targets` 和 `clippy -D warnings` 变绿 → **CI 加 Windows 的 check+clippy job**；反向验证：去掉一处 cfg，该 job 变红 |
| W3 | `agent24-os-win`：私有管道、对端 pid、pid→SID（含 `interprocess` 评估结论） | W2 | ~250 | Windows 原生单测：别的 SID 无法打开（用 `runas` 不现实，改为用「只授予 Everyone 读权限的 DACL」对照组证明 DACL 生效）；pid→SID 对本进程返回 true，对 `System`（pid 4）返回 false |
| W4 | 测试的平台 cfg | W2 | ~250（可拆两片） | **CI 加 Windows 的 `cargo test --workspace -- --test-threads=1`**（沿用 #553 的串行化），变绿 |
| W5 | 内核侧传输抽象：cfg 类型别名，handshake/mux 改用 `tokio::io::split`；**只做重构，Unix 行为不变** | W2 | ~200 | Unix 全量测试和黑盒测试全绿；`git diff` 中协议常量、帧代码零改动 |
| W6 | Windows 回调与入站管道（§3.1、§3.2、§4），包括 `PipeListener`、上游 Job/pid 校验 | W3、W5 | ~300 | Windows 原生测试：同进程的伪模块完成握手并收到代理请求；伪造上游（不在 Job 里的进程抢注管道名）→ 内核拒绝连接，并记 `foreign_upstream` |
| W7 | 进程监督：ProcessKit Job、软停=关回调、宽限期后 terminate、Windows 环境白名单 | **sidecar 批 3 已合进 main**、W6 | ~300 | Windows 原生测试：模块再起一个孙进程，stop 之后 Job 计数归 0；模块忽略 EOF 时，宽限期满被终止，`killed_after_grace` 中有记录 |
| W8 | shell_exec/MCP 的 Windows 白名单和 `PATHEXT` 解析，带 §6.2 的对照测试 | W2 | ~200 | 对照测试结果写进注释；`npx --version` 能通过 MCP 构建路径启动；含密钥的环境变量对子进程不可见（移植现有测试） |
| W9 | A3 附着管道（§5） | W3、W5 | ~200 | 同用户的测试客户端握手成功；第二个 daemon 起附着监听时降级并记录日志 |
| W10 | 验收黑盒：Sin90 Windows 包，挂载并收到调度回调 | W7、A4 产出 `x86_64-pc-windows-msvc` 包 | ~200 | **C2 验收**：windows-latest 上 `os list` 显示 `[mounted]`，`fired` 回调到达，事件出现在 WS |

**CI 路线**：W0 只手动触发 → W2 起每个 PR 都跑 Windows `check`+`clippy` → W4 起加上串行 `test` → W10 起加上黑盒测试。
这些 job 放在 `ci.yml` 里，与 A1 的 macOS 矩阵并列；用 `--exclude agent24-sidecar-host` 避免与 `sidecar-windows.yml` 重复测试。
先不设为 required，连续 10 次为绿后再由 jason 决定是否加入 ruleset。

## 9. 摸底：W0 workflow 草稿（本次只写进文档，不新增文件）

```yaml
name: Windows check (manual)
on:
  workflow_dispatch:
    inputs:
      all_targets:
        description: "also check tests/benches (--all-targets)"
        type: boolean
        default: false
permissions:
  contents: read
jobs:
  check:
    runs-on: windows-latest
    timeout-minutes: 60
    defaults:
      run:
        shell: bash
        working-directory: rust
    steps:
      - uses: actions/checkout@93cb6efe18208431cddfb8368fd83d5badbf9bfd  # v5
        with: { submodules: false, persist-credentials: false }
      - run: rustup toolchain install stable --profile minimal --component clippy
      - uses: actions/cache@0057852bfaa89a56745cba8c7296529d2fc39830  # v4
        with:
          path: |
            ~/.cargo/registry
            ~/.cargo/git
            rust/target
          key: cargo-wincheck-${{ hashFiles('rust/Cargo.lock') }}
      - name: cargo check --keep-going (never fails the job; the log is the product)
        run: |
          set +e
          extra=""; [ "${{ inputs.all_targets }}" = "true" ] && extra="--all-targets"
          cargo check --locked --workspace $extra --keep-going --message-format short 2>&1 | tee check.log
          echo "cargo exit: ${PIPESTATUS[0]}" | tee -a check.log
      - name: classify errors by crate
        run: |
          { echo "## errors by crate"; grep -oE '^(crates|apps)/[^/]+' check.log | sort | uniq -c | sort -rn
            echo "## dependency build failures"; grep -E 'could not compile|failed to run custom build' check.log | sort -u
          } | tee summary.txt >> "$GITHUB_STEP_SUMMARY"
      - uses: actions/upload-artifact@v4  # 实施时按仓库惯例钉 SHA
        with: { name: windows-check, path: "rust/check.log\nrust/summary.txt" }
```

说明：
- `--keep-going` 不会 check 依赖已经失败的下游 crate，所以每合一片都要重跑一次，把错误逐层剥开。
- `ring`、`libsqlite3-sys` 在 MSVC runner 上自带工具链；如果仍然失败，记入 summary，作为 W2 的输入。

## 10. 风险与不做的事

| 风险 | 缓解 |
|---|---|
| 范围失控（RESEARCH §4） | 本期**不承诺 Windows 功能完整**：C2 的验收只到「Rust 全量测试为绿 + Sin90 挂载 + 收到调度回调」；Node 模块、A3 真实客户端、Windows 服务都不在验收内 |
| sidecar 批 3 迟迟不合 | W7 和 W10 顺延；W0–W6、W8、W9 照常合并；Windows 上模块显示 `unsupported_platform`，daemon 和 CLI 其余功能可用 |
| `PSModulePath` 假说不成立 | W8 的对照测试本身就是证伪手段；两组都挂说明是 launch 路径的缺陷，转给 sidecar 线一起查，不在本线另起实现 |
| Windows 原生测试不稳定 | 串行运行（#553）；不靠放宽超时让测试变绿；PowerShell 夹具优先直接调 .NET |
| Defender / SmartScreen 拦截未签名的 `agent24d.exe` 和模块 exe | **Windows 签名依赖 C1**（jason 核实 Trusted Signing 资格，不行就买 OV/EV 证书），属于 C3；C2 只在 CI 上验收 |
| 管道名中的用户 SID 哈希冲突 | 只用于命名；真正的鉴权靠 DACL + SID 比较，哈希冲突只会导致首实例创建失败，不会造成越权 |

**不做**：Windows 服务注册；Windows AF_UNIX 路线；loopback TCP 路线；修改 wire 帧；改动 sidecar-host 线的任何文件、PR、
`deny.toml` 放行；ARM64 Windows；桌面端 NSIS 安装包（C3）。

## 11. 请评审重点确认

1. 是否接受「Windows 上用 `A24_LISTEN_PIPE` 替换 `A24_LISTEN_FD`，并要求模块在 initialize 之前建好首实例」这一处启动环境差异（§3.2）。
2. 「sidecar 批 3 = #549 + #553 及其栈」的界定是否正确（§7）。
3. 新建 `agent24-os-win` 作为第二个 unsafe 边界 crate，还是用 `interprocess` 免掉自写 FFI（§3.4，W3 评估）。
