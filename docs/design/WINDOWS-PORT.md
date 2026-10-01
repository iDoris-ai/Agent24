# DEP-A7 —— Windows 移植设计

> 状态：**v2，已处置第 1 轮评审（CHANGES，3 High），待 §1 回填 W0 结果后冻结**（`docs/Deployment/TASKS.md` DEP-A7）。只写设计，不含实现。
> 依据：`docs/Deployment/RESEARCH.md` §1.4 / §1.7 / §2 / §4；`TASKS.md` 的 A7、C2、C3；ADR-031（进程外模块）、
> ADR-032 / `A3-ATTACHED-MODULE.md`（附着模块）、`docs/specs/WIRE-OOP-MODULE.md`；仓库实查（基线 `bf3322c`）；
> sidecar-host 线只读查看（`origin/integration/open-design-main-sync-wave20`、#549、#553）。
> 范围：Rust 侧（`agent24d` / `agent24` CLI / 模块 SDK）能在 `x86_64-pc-windows-msvc` 上编译、测试，并让 Sin90 能挂载。
> 桌面端 NSIS 安装包和签名属于 C3，不在本文范围内。评审处置见 §12。

## 0. 结论摘要

- **模块传输选命名管道**（§2）。tokio 和 Node 都原生支持；鉴权在两个方向上都是「管道 DACL 只给本用户 + 对端 pid→SID 必须等于本用户」，
  与 Unix 的「0700 目录 + `peer_cred`」**强度对等**。不需要句柄继承，因此 Windows 上没有 fd 3，也没有跳板。
- **wire 帧不变**：NDJSON 回调、`initialize`、HTTP 代理、A3 握手的字节一个都不改。只在 Windows 上改**启动环境**：
  用 `A24_LISTEN_PIPE` 替换 `A24_LISTEN_FD`；`A24_CALLBACK_SOCK` 和 A3 的 `socket_path` 字段不变，值变成管道名。
  WIRE 增加 Windows 小节（§3.5），其中有几条 MUST。Unix 行为不变。
- **进程监督复用 ProcessKit**（Job 整树回收 + `spawn_isolated_piped`），不重复造轮子。W7 的门槛是一个可判定条件，
  两个选项由 jason 选（§7）。其余分片不依赖它。
- A7 自带 W0（`windows-check.yml`，已单独提交）。C2 拆成 W1–W9 共 9 片，每片 ≤300 行；A3 附着、Node 模块、Job 成员校验三项**推迟，不计入 C2 验收**（§8）。

## 1. 实查结果（对 RESEARCH §1.4 的核实与补漏）

`grep -rln std::os::unix` 去掉 `tests/` 目录后，**确实是 20 个文件**。按 `mod tests` 的位置再区分一次：
- **7 个文件只在测试代码里用到**：`domain.rs`、`os_config.rs`（生产代码里只有一处 `#[cfg(unix)]` 目录 fsync）、`os_routes.rs`、
  `supervisor.rs`、`memory/reconcile.rs`、`tools/local.rs`、`os-packages/discovery.rs`；
- **13 个文件在生产代码里真正使用**，逐项处置见 §6。

RESEARCH 列出的 `proxy.rs` `UnixStream`、`state_dir` 只读 HOME、`command-fds`/`close_fds`、`same_device`、`server.rs:966` 信号、
`service.rs` launchd，都核实无误。另外查出 **6 处遗漏**：

| 遗漏 | 位置 | 后果 |
|---|---|---|
| 直接读 `/dev/urandom` 铸 token | `launch.rs:281`、`proxy.rs:902,928`、`kernel_call.rs:204`、`agent24d/module_approval_broker.rs:128` | 能编译，但**运行时**在 Windows 上铸不出 token，模块起不来，审批失败 |
| `rustix` 的 `process`/`fs` 特性只支持 Unix | os-proto、os-packages、os-fd、agent24d（`geteuid`、`kill_process_group`、`waitid`、`OFlags`） | 编译失败 |
| `tokio::net::Unix*` 还出现在另外 3 个文件 | `kernel_call.rs`、`scheduler_deliver.rs`（测试）、`server.rs` | 与 proxy 同类 |
| 回调根目录退回到 `/tmp/a24-run-*` | `server.rs:1886 callback_root` | 管道方案下 Windows 不需要这条路径 |
| 子进程 env 白名单只含 Unix 变量 | `tools/env_whitelist.rs`（shell_exec + MCP）、`launch.rs:50 INHERITED_ENV` | 缺 `SystemRoot`（#549 的教训，§6.2） |
| Node 参考模块用 `listen({fd: 3})` | `examples/node-module` | Node 在 Windows 上**不支持** listen fd |

`cfg(not(unix))` 分支目前共 5 处：`server.rs:966`、`state_file.rs:244`、`install.rs:332/403/416`。

**macOS 交叉预览**（rustup stable + `x86_64-pc-windows-gnu`，`--keep-going`）：
- `ring` 和 `libsqlite3-sys` 的 build script 失败（没有 C 工具链）；
- `command-fds`、`close_fds`、`agent24-os-fd` 编译失败；
- 只有 `agent24-protocol`、`agent24-core`、`agent24-domain` 通过。

这份预览**不能代替** W0。

**W0 结果（windows-latest，按 crate 分类）**：〔占位——`windows-check.yml` 的小 PR 合并并跑过之后回填，然后冻结本文〕

## 2. 模块传输选型

进程外模块有两条通道：
- **回调**：模块连接内核，走 NDJSON；
- **入站**：内核连接模块，走 HTTP。Unix 上是内核 bind 好 UDS，经 fd 3 交给模块。

A3 附着模块只有一条常驻监听。

| 维度 | (a) Windows AF_UNIX + 句柄继承 | **(b) 命名管道** | (c) loopback TCP + token |
|---|---|---|---|
| peer 鉴权 | `SIO_AF_UNIX_GETPEERPID` 拿 pid 再查 SID（最低系统版本待实测）；socket 文件 ACL 继承自目录 | **DACL 只给本用户**；`GetNamedPipeClientProcessId` / `ServerProcessId` 拿 pid 再查 SID，两个方向都能做，与 Unix 对等 | 本机任何用户都能连。回调 `accept_one` 可能被抢连造成 DoS；入站 HTTP 可以绕过受约束代理 |
| 异步运行时 | tokio/mio **不支持** Windows AF_UNIX，要自己接 IOCP 或桥接阻塞线程 | `tokio::net::windows::named_pipe` 原生支持 | tokio 原生 |
| 句柄 / fd 传递 | std 稳定版没有 `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`（`raw_attribute` 是 unstable）；`bInheritHandles=TRUE` 会把句柄漏给并发 spawn 的子进程；AF_UNIX 句柄跨进程继承没有官方保证 | **不需要**：模块按名字自己创建或连接 | 同 (a)，或改由模块上报端口 |
| SDK 影响（`agent24-os-sdk`/`agent24-os-fd`） | os-fd 要接收 SOCKET 句柄（新增 unsafe） | os-fd 在 Windows 上不参与；proto 的 `take_listener()` 改为按名字建管道（§4）；SDK 改 1 行 | 每个模块都要校验每个入站请求的 token 头 |
| Node 模块 | libuv 不支持 Windows AF_UNIX，两个方向都不可用 | `net.connect(pipe)` 直接可用；入站改为 `listen(pipe)` | 可用 |
| wire 是否要改 | 帧不变；`A24_LISTEN_FD` 的值变成句柄号 | 帧不变；只改启动环境（§3.5） | **要改**：入站请求加鉴权头 |
| 实现量 | 大 | 中（一个 unsafe 小 crate + cfg 别名 + 监听循环） | 小，但安全上要补的最多 |
| 测试难度 | 高 | 中（原生测试要串行，见 #553） | 低 |

**推荐 (b)**，理由有三：
1. 只有它在 tokio 和 Node 两边都原生可用；
2. 只有它不需要句柄继承，可以直接接 ProcessKit 的隔离 spawn；
3. 两个方向都能做到「DACL + 对端 SID」，与 Unix 现有保证对等。

(c) 被否决，因为它要求每个模块自己承担鉴权，违背「模块信任内核代理」的前提。

## 3. 推荐方案细节

### 3.1 回调方向（模块 → 内核）

- **管道名**：`\\.\pipe\agent24-<u>-<pid>-cb-<n>-<128 位随机 hex>`，经 `A24_CALLBACK_SOCK` 传递。`<u>` 是用户 SID 的短哈希。
  随机后缀让其他用户无法提前抢注（H2）。管道名没有 103 字节限制，所以 `callback_root` 的 `/tmp` 回退在 Windows 上整段不编译。
- **创建**：用 `first_pipe_instance(true)` + `reject_remote_clients(true)` + 显式 DACL（`D:P(A;;GA;;;<本用户 SID>)`）。名字已被占用时直接失败。
- **`accept_one`**：`connect().await` → `GetNamedPipeClientProcessId` → SID 必须等于本用户 → 交给握手。
  只建一个实例，所以「每代只服务一条」（D1）在结构上仍然成立。SID 不符时返回 `EndpointError::ForeignPeer`。
- **模块侧**：用 `ClientOptions::new().open(name)`，遇到 `ERROR_PIPE_BUSY` 有界重试；Node 的 `net.connect(path)` 不用改。

### 3.2 入站方向（内核 → 模块 HTTP）：fd 3 的替代

- 内核在 spawn 时铸管道名 `\\.\pipe\agent24-<u>-<pid>-m-<n>-<128 位随机 hex>`，经 **`A24_LISTEN_PIPE`** 传递。
  Windows 上不设置 `A24_LISTEN_FD`，`A24_*` 仍然是 4 个。
- **模块自己创建管道**：`first_pipe_instance(true)` + 只给本用户的 DACL，并且**必须在发送 `initialize` 之前建好首实例**。
- **内核每次连接上游**（`proxy.rs` `Upstream::connect`、`kernel_call.rs`）：`ClientOptions::open`，遇 `PIPE_BUSY` 有界重试 →
  `GetNamedPipeServerProcessId` → SID 必须等于本用户，否则断开并记录 `foreign_upstream`。
  名字是随机的，再加上 SID 校验，可以替代 Unix 上「listener 由内核 bind」所提供的防冒名保证。
  对**同用户**进程不做额外防护，这与 Unix 相同：同用户本来就能连 0700 目录里的 socket。
- **模块死亡**：管道实例随进程消失，新连接立即返回 `FILE_NOT_FOUND`，等价于 Unix 的「连接被拒、不排队」。

**读写两半按平台分别取别名**（H3）。Unix 保留 `into_split` / `Owned*Half`：drop 写半时会发 FIN，现有代码依赖这个语义。
Windows 用 `tokio::io::split`。下面的代码已用 `cargo check` 验证，覆盖本机和 `x86_64-pc-windows-gnu` 两个目标（tokio 1.53.1、axum 0.8.9）：

```rust
#[cfg(unix)]    pub type CallbackStream    = tokio::net::UnixStream;
#[cfg(windows)] pub type CallbackStream    = tokio::net::windows::named_pipe::NamedPipeServer;
#[cfg(unix)]    pub type CallbackReadHalf  = tokio::net::unix::OwnedReadHalf;
#[cfg(unix)]    pub type CallbackWriteHalf = tokio::net::unix::OwnedWriteHalf;
#[cfg(windows)] pub type CallbackReadHalf  = tokio::io::ReadHalf<CallbackStream>;
#[cfg(windows)] pub type CallbackWriteHalf = tokio::io::WriteHalf<CallbackStream>;
#[cfg(unix)]    pub fn split_callback(s: CallbackStream) -> (CallbackReadHalf, CallbackWriteHalf) { s.into_split() }
#[cfg(windows)] pub fn split_callback(s: CallbackStream) -> (CallbackReadHalf, CallbackWriteHalf) { tokio::io::split(s) }
// UpstreamStream 同理：unix → UnixStream，windows → NamedPipeClient
```

**命名管道不支持半关闭**：对写半调用 `poll_shutdown` 不会让对端读到 EOF。`tokio::io::split` 的两半共享同一个句柄，
只有**读写两半都 drop** 之后管道才会关闭，对端才能读到 EOF。所以凡是靠「关闭连接」传递信号的地方（软停 §3.3、D1 断代），
Windows 分支都必须把两半一起 drop，不能只关写半。

### 3.3 进程监督（`launch.rs` / `supervise.rs`）

| Unix 现状 | Windows 方案 |
|---|---|
| 跳板进程：`close_fds` 把 ≥4 的 fd 设为 cloexec 后再 exec | **不需要跳板**：`spawn_isolated_piped` 只让三条 std 管道被继承。spawn 之后**立即关闭子进程的 stdin**（模块的 stdin 在 Unix 上就是 null；#549 R4 的挂起问题与 stdin 保持打开有关） |
| `process_group(0)` + `killpg` | `IsolatedPipedChild`（它已经接管了 ProcessGroup）：`kill_all()` 回收整棵进程树，`tree_is_empty()` 判断是否已空，与 #549 `owner.rs` 的用法相同 |
| SIGTERM → 宽限期 → SIGKILL | 顺序：**drain**（`A24_MODULE_DRAIN_MS`）→ **关闭回调**（读写两半一起 drop；WIRE §7.1 已规定模块读到 EOF 必须退出）→ **宽限期** → **`kill_all()`**（TerminateJobObject）。不用 `CTRL_BREAK`，因为 daemon 可能没有控制台 |
| `waitid(WNOWAIT)` + `signal()` | `try_wait` + `tree_is_empty`；`Exit.signal` 在 Windows 上恒为 `None` |
| `INHERITED_ENV` | 按平台各一份。Windows：`SystemRoot`、`windir`、`PATH`、`PATHEXT`、`TEMP`、`TMP`、`USERPROFILE`、`LOCALAPPDATA`、`APPDATA`、`ComSpec`，**键名不区分大小写匹配**（§6.1）；要不要加 `PSModulePath` 由 W8 的对照测试决定 |

### 3.4 unsafe 边界

工作区设置了 `unsafe_code = "forbid"`，目前只有 `agent24-os-fd` 是 `deny` 加一处 `allow`。Windows 需要的 FFI 有：
- SDDL → `SECURITY_ATTRIBUTES`；
- `GetNamedPipe{Client,Server}ProcessId`；
- `OpenProcessToken` / `GetTokenInformation` / `EqualSid`。

这些全部放进**新 crate `agent24-os-win`**：只在 `cfg(windows)` 下编译，`deny(unsafe_code)`，每处 unsafe 都写 SAFETY 注释。
它只导出安全 API：建私有管道、取对端 pid、判断 pid 是否属于本用户。
不并入 os-fd，是为了保住 os-fd「唯一把继承 fd 变成 socket 的地方」这个定位（J-S3）。
W4 开头先评估 `interprocess` crate；如果能免掉自写 FFI，就不新建这个 crate。

### 3.5 WIRE-OOP-MODULE 的 Windows 小节（W6 一并修改 spec）

- §2：Windows 上用 `A24_LISTEN_PIPE` 取代 `A24_LISTEN_FD`；模块 **MUST** 在发送 `initialize` 之前建好首实例，
  并且 **SHOULD** 使用只给本用户的 DACL。`A24_CALLBACK_SOCK` 的值是管道名。
- §7.3：Windows 上没有 SIGTERM。软停 = 回调连接 EOF，宽限期满后整棵进程树被终止。模块读到 EOF **MUST** 尽快退出（§7.1 已有）。
- A3 §4.1：附着客户端 **MUST** 满足两条：
  - 打开管道时带 `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`（tokio `ClientOptions::security_qos_flags`），防止服务端冒用客户端身份；
  - **发送 token 之前**，用 `GetNamedPipeServerProcessId` 取得服务端 pid，并校验其 SID 与自己相同。不符就断开，不发 token。

## 4. SDK 侧：`take_inherited_listener` 在 Windows 上的语义

- `agent24_os_fd::take_inherited_listener` **只在 `cfg(unix)` 下存在**。不提供一个在 Windows 上永远失败的同名函数。
- `agent24_os_proto::module::take_listener()` 签名不变，仍返回 `InheritedListener`，内部按平台分支：
  - Windows 上读 `A24_LISTEN_PIPE`，以 `first_pipe_instance(true)` 建首实例，包装成 `PipeListener`；
  - 保留「每进程只能调用一次」的原子标记；
  - `ListenError` 新增两个错误：`NotAPipeName`，以及 `Squatted`（首实例创建失败）。
- `PipeListener` 实现 `axum::serve::Listener`。accept 循环的顺序是：`connect` 当前实例 → 建好备用实例 → 交出当前实例。该实现已在 Windows 目标上 check 通过。
- SDK 的 `Module::connect` 已经是「先拿到 listener，再握手」的顺序，所以 §3.5 的 MUST 自然满足。
  SDK 唯一要改的是 `module.rs:198`：用 cfg 选 `into_tokio()` 还是 `into_pipe()`。Sin90/Cos72 的源码**零改动**。

## 5. A3 附着 socket 与鉴权（**推迟，不计入 C2 验收**）

- 监听管道名为 `\\.\pipe\agent24-<u>-attach`。这个名字必须可预测，客户端才找得到，所以防抢注靠下面几条，不靠随机：
  - DACL 只给本用户，加 `reject_remote_clients`，以 `first_pipe_instance(true)` 创建；
  - **首实例创建失败时大声报错**：`error` 级日志写明「attach 管道已被占用，可能被抢注」；`/health` 和 `os attach` 返回 `attach_unavailable`，CLI 退出码非 0；不静默降级（H2）；
  - 客户端侧的 MUST 见 §3.5。
- 每条连接：`GetNamedPipeClientProcessId` 取 pid → SID 必须等于本用户 → token 握手（服务端只存 sha256）。
  附着模块不是内核 spawn 的，本来就只能校验 SID。
- 注册响应里的 `socket_path` 在 Windows 上返回管道名，wire 不变。管道没有文件系统节点，不需要清理残留。

## 6. 逐文件处置表

### 6.1 生产代码（13 个文件 + 遗漏项）

| 文件 | 现用途 | Windows 方案 | 分片 |
|---|---|---|---|
| `protocol/state_file.rs` | `state_dir()` 只读 HOME；文件 0600；`pid_alive` 用 `ps` | `state_dir()` 改为 `%LOCALAPPDATA%\Agent24`（变量缺失时返回 None）；0600 降级为依赖默认的用户私有 ACL；`pid_alive` 用 `tasklist /FI "PID eq n" /FO CSV /NH`，按 CSV 第 2 列精确匹配 pid | W1 |
| `domain` 的 `data_dir` 解析 | manifest 固定写 `~/.agent24/os/<name>/` | 把 `~/.agent24/` 前缀映射到 `state_dir()`，manifest 不变 | W1 |
| `cli/service.rs` | launchd | **本期明确不支持**：加 `cfg(target_os = "macos")` 门，其他平台报 `unsupported on this platform` | W1 |
| `agent24d/server.rs` | 信号、回退目录、`callback_root` | 信号用 `ctrl_c`/`ctrl_break`/`ctrl_close`/`ctrl_shutdown`（已 check 通过），停机仍以 `POST /api/v1/shutdown` 为主；`callback_root` 只在 `cfg(unix)` 下编译 | W1 / W3 |
| `agent24d/lifecycle.rs` | 用 O_NOFOLLOW\|O_NONBLOCK 读文件；目录 0700 | `custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)`，打开后检查不是重解析点；0700 降级 | W1 |
| `agent24d/attached.rs` | 0600 写入、chmod hook、目录 fsync | 0600 降级；目录 fsync 跳过（std 无法以普通方式打开目录句柄），注释写明 | W1 |
| `os-packages/lib.rs` | 包根 0700 + uid 属主检查 | 降级为「是真实目录且不是重解析点」；SID 属主检查在 W4 之后补上 | W3 |
| `os-packages/install.rs` | 0700、去掉 g/o 写位、`same_device`；升级、卸载 | `same_device` 返回 `Some(true)`：staging 按构造就在目标父目录内，`MoveFileExW` 不带 `COPY_ALLOWED`，跨卷时直接失败；**升级、卸载前先停掉该模块**（Windows 上正在运行的 exe 删不掉也改不了名） | W3 / W7 |
| `os-proto/launch.rs` ① | `inherited()` 按大小写精确匹配 env 键 | Windows 上的键是 `Path` 这类写法，改为不区分大小写匹配（签名已 check 通过） | W7 |
| `os-proto/launch.rs` ② | 用 `contains(MAIN_SEPARATOR)` 判断是不是路径；`which()` 只查可执行位 | Windows 上 `MAIN_SEPARATOR` 是 `\`，会把 `bin/sin90` 当成命令名去查 PATH。改为同时认 `/` 和 `\`；`which()` 按 `PATHEXT` 补扩展名；**`spawn.command` 在 Windows 上只允许 `.exe`**，manifest 写 `bin/sin90` 时按 `bin/sin90.exe` 解析；`canonicalize` 改用 `dunce`，避免 `\\?\` 前缀 | W7 |
| `os-proto/launch.rs` ③ | `check_tree` 要求每个条目的 uid 等于本用户，且没有 g/o 写位 | uid → 条目属主 SID 必须等于本用户（经 `agent24-os-win`）；mode → 不再检查，改为拒绝重解析点。W4 落地之前只做重解析点检查，并在日志里说明这是降级 | W7 |
| `os-proto/launch.rs` 其余 | 跳板、`command-fds`、`close_fds`、`/dev/urandom` | 跳板与 fd 映射只在 `cfg(unix)` 下编译；token 改用 `getrandom` 0.3（锁文件里已有） | W3 |
| `os-proto/supervise.rs` | 进程组、`waitid`、SIGCHLD | 见 §3.3 | W7 |
| `os-proto/endpoint.rs`、`module.rs` | 回调 UDS、`peer_cred`、`InheritedListener` | 见 §3.1、§3.2、§4 | W2 / W6 |
| `os-proto/proxy.rs`、`kernel_call.rs` | 上游 `UnixStream`；`/dev/urandom` | 见 §3.2；`getrandom` | W2 / W3 / W6 |
| `os-fd/lib.rs` | fd 3 接管 | 整个 crate 只在 `cfg(unix)` 下编译，proto 改为 target 依赖 | W3 |
| `agent24d/attach_listener.rs` | A3 | 见 §5 | 推迟 |
| `agent24d/module_approval_broker.rs` | `/dev/urandom` | `getrandom` | W3 |
| `tools/env_whitelist.rs` + `local.rs` | shell_exec / MCP 的 env | 见 §6.2 | W8 |
| `command-fds`、`close_fds`、`rustix`（process/fs）、`libc` | 只能在 Unix 上用 | 挪到 `[target.'cfg(unix)'.dependencies]` | W3 |
| 桌面端 `daemon.json` 路径 | 桌面端读 `~/.agent24/daemon.json`（A5） | **列入 C3 契约**：Windows 上与 `state_dir()` 一致，即 `%LOCALAPPDATA%\Agent24\daemon.json` | C3 |

### 6.2 shell_exec / MCP：吸取 #549 的 env 教训

- **shell_exec 保持 argv 直接执行**，这是一项安全属性。需要 shell 时由 LLM 自己传
  `["powershell","-NoProfile","-NonInteractive","-Command","<单个脚本字符串>"]`：
  - 脚本只作为一个参数传，不拆成多个 argv；
  - 工具描述里写明这一用法。
- `CHILD_ENV_WHITELIST` 按平台各一份，键名不区分大小写。
- #549 的挂起被归因为「`env_clear()` 之后只剩最小环境」，但这仍是**没有被单独验证过的假说**。W8 用一组对照测试来证伪：
  - 对照组 ①：最小白名单；
  - 对照组 ②：在 ① 的基础上加 `PSModulePath`、`LOCALAPPDATA`、`TEMP`；
  - 两组都执行 `Test-Path .`，stdin 为 null，超时 30s，并记录耗时；
  - 测试结论写进注释，决定 `PSModulePath` 是否进入白名单。
- MCP 用的 `npx` 在 Windows 上是 `npx.cmd`。W8 让 `which` 按 `PATHEXT` 解析，`.cmd` 交给 std 的批处理参数转义执行。

### 6.3 只在测试中用到的 7 个文件 + `tests/` 黑盒

- 整个测试函数加 `#[cfg(unix)]`，沿用 `domain.rs:3416` 的约定。
- `a3_*`、`me3f`、`me4_*`、`trampoline` 这几组黑盒测试先整体标为 `cfg(unix)`。
- W5 新增 `.gitattributes`（`* text=auto eol=lf`），避免 Windows checkout 把换行改成 CRLF，打坏 fixture 和快照。

## 7. 与 sidecar-host / ProcessKit 的关系

- **复用**：
  - `IsolatedPipedChild`：`spawn_isolated_piped`、`kill_all`、`tree_is_empty`、`try_wait`、`take_pipes`；
  - sidecar 的 Windows CI 经验：测试串行化、PowerShell 夹具优先直接调 .NET、不对 `cmd /C` 的退出码做断言。
- **不复用**：sidecar 是 stdio NDJSON 一对一，没有 UDS，也没有 fd 继承；§2–§5 的模块传输与它没有重叠。
- **目标**：不重复造轮子。W7 之前，Windows 上的 `launch` 返回 `LaunchError::Unsupported`（模块显示 `unsupported_platform`，daemon 照常运行），**不自己写一套 Job Object**。
  W7 只在 `agent24-os-proto` 的 `[target.'cfg(windows)'.dependencies]` 里引用 processkit，不改 sidecar 的任何文件。
- **W7 门槛（可判定，二选一，由 jason 选）**：
  - **(a)** main 的 `deny.toml` 已经通过 `[sources] allow-git` 放行 `https://github.com/jhfnetboy/ProcessKit-rs.git`，
    且 main 的 `Cargo.lock` 里能看到 rev `60aa827db378daa5b1ec638f3b3fdaaf9c201560`（或之后由 sidecar 线写明的新 rev）。
    满足后 W7 直接引用这个来源和 rev。判定方法：`git show origin/main:rust/deny.toml` 加上 `grep 60aa827d rust/Cargo.lock`。
  - **(b)** 以下 PR 全部合入 main：sidecar 整条 stacked 链，从 #275 到 #553 共 90 个。
    链首依次是 #275、#278、#279、#282–#285、#290–#292、#302；链尾依次是 #545、#547、#548、**#549**（isolated pipes + fork pin）、#551、**#553**（Windows 测试串行化）。
    完整链可以从 #553 起沿 `baseRefName` 逐级回溯得到。判定方法：`gh pr view 553 --json state` 返回 `MERGED`，且上游 PR 都没有被跳过。
  - 对比：(a) 只依赖一个可核验的配置事实，不受那条线合并节奏影响，但要 jason 授权放行一个 git 源；(b) 不需要额外授权，但可能要等很久。
- `deny.toml` 的放行由 jason 决定，本线不抢先修改。

## 8. C2 分片计划

每片一个 PR，≤300 行（不含锁文件和夹具），Unix 全量测试保持全绿。**W0 属于 A7**：`.github/workflows/windows-check.yml` 已单独提交，同时支持 `workflow_dispatch` 和限定 paths 的 `pull_request`。

| 片 | 内容 | 依赖 | 规模 | 可证伪验收 |
|---|---|---|---|---|
| W1 | 平台基础：`state_dir`、`data_dir` 映射、service 平台门、信号、`pid_alive`、权限降级 | — | ~250 | 新单测：缺少 `LOCALAPPDATA` 时 `state_dir()==None`；非 macOS 上 `service install` 退出码非 0 且输出含 `unsupported` |
| W2 | 传输别名（§3.2）：endpoint/module/proxy/kernel_call 改用别名；只做重构 | — | ~150 | **Unix diff 只新增别名，且每个别名都解析到原类型**（`OwnedReadHalf` 等）；除别名替换外，协议常量和帧代码零改动；Unix 全量测试和黑盒全绿 |
| W3 | 编译门：Unix 依赖挪到 target 段；os-fd、跳板、`callback_root` 加 `cfg(unix)`；`launch` 返回 `Unsupported`；`getrandom`；`same_device` | W1、W2 | ~280 | windows-latest 上 `check --all-targets` + `clippy -D warnings` 变绿 → **CI 加 Windows check job**；反向验证：删掉一处 cfg，该 job 变红 |
| W4 | `agent24-os-win`（或 `interprocess`，看评估结论） | W3 | ~250 | 原生单测：对本进程 pid 的 SID 判断返回 true；对 pid 4（System）**返回 Err 或 false 都算通过**；对照组：用「Everyone 只读」的 DACL 建管道，以写权限打开时被拒 |
| W5 | 测试平台 cfg + `.gitattributes` | W3 | ~250（可拆两片） | **CI 加 Windows test**：只对进程密集型 crate（os-proto、agent24d、tools）用 `--test-threads=1`，其余并行；`--exclude agent24-sidecar-host --exclude agent24-sidecar-host-protocol` |
| W6 | Windows 回调和入站管道（§3.1、§3.2、§4）+ WIRE Windows 小节 | W2、W4 | ~300 | 原生测试：①伪模块完成握手，并收到代理过来的请求；②**内核 drop 回调连接（读写两半）后，模块读到 EOF**；③上游 SID 不符时拒绝连接并记录 `foreign_upstream`，用「降级成匿名身份」的夹具模拟 |
| W7 | 进程监督（§3.3）、`launch.rs` ①②③、升级/卸载前停模块 | **§7 门槛（a 或 b）**、W6 | ~300 | 原生测试：模块再起一个孙进程，stop 之后 `tree_is_empty()==true`；模块忽略 EOF 时，宽限期满被 `kill_all`，`killed_after_grace` 中有记录；`bin/sin90` 能解析到 `bin/sin90.exe`；env 键 `Path` 能被透传 |
| W8 | shell_exec/MCP 的 Windows 白名单 + `PATHEXT` + 对照测试 | W3 | ~200 | 对照测试的结论写进注释；`npx --version` 能经 MCP 构建路径启动；带密钥的 env 对子进程不可见 |
| W9 | 验收黑盒：**在 runner 上从源码构建 Sin90**（不依赖 A4），安装、挂载、收到调度回调 | W7 | ~200 | **C2 验收**：`os list` 显示 `[mounted]`，`fired` 回调到达，事件出现在 WS |

**推迟，不计入 C2 验收**（控制范围）：
- A3 附着管道（§5）；
- Node 模块：改为先 `listen(pipe)`，`'listening'` 回调触发后再 `initialize`；它用 libuv 的默认 DACL，属于 best-effort；
- Job 成员校验层：`IsolatedPipedChild` 不暴露 members 和 Job 句柄，需要 ProcessKit 先提供 API。

**CI 路线**：
- A7：W0 手动或 PR 触发跑一次，回填 §1；
- W3 起：每个 PR 跑 Windows check + clippy；
- W5 起：加 test；
- W9 起：加黑盒。

这些 job 并入 `ci.yml`，与 A1 的 macOS 矩阵并列。先不设为 required，连续 10 次为绿后由 jason 决定是否设为 required。

## 9. 摸底：W0 workflow（A7 交付，已落成文件）

文件：`.github/workflows/windows-check.yml`（单独提交，`actionlint` 通过）。要点：
- 触发：`workflow_dispatch`（可选 `--all-targets`），以及只对该 yml 自身生效的 `pull_request`，这样它的 PR 一开就会跑一次；
- 主步骤：`cargo check --locked --workspace --keep-going --message-format short`，结果 tee 到 `check.log`，步骤恒以 `exit 0` 结束；
- 分类步骤：`grep -oE '^(crates|apps)[\\/][^\\/:]+'`，`/` 和 `\` 两种分隔符都认，正反样例已在本机验证；每条 grep 后接 `|| true`；
- 分类和 upload 两步都加 `if: always()`；artifact 是 `check.log` + `summary.txt`；
- `--keep-going` 不会 check 依赖已失败的下游 crate，所以每合一片都要重跑，错误会一层层剥开。

## 10. 风险与不做的事

| 风险 | 缓解 |
|---|---|
| 范围失控 | **不承诺 Windows 功能完整**；C2 只验收「Rust 测试全绿 + Sin90 挂载 + 收到调度回调」；A3、Node、Job 层、Windows 服务都推迟 |
| W7 门槛迟迟不满足 | W1–W6、W8 照常合并；Windows 上模块显示 `unsupported_platform` |
| `PSModulePath` 假说不成立 | W8 的对照测试本身就是证伪手段；两组都挂说明是 launch 路径的缺陷，转交 sidecar 线一起查 |
| 原生测试不稳定 | 进程密集型 crate 串行跑（#553）；不靠放宽超时让测试变绿 |
| **Defender 实时扫描**新落盘的 exe 和 build 产物，拖慢首次 spawn 或测试，导致超时 | 测试里首次 spawn 的超时单独放宽并记录耗时；不要求关 Defender；如果反复超时，在 runner 上把 `rust/target` 加入扫描排除（只限 CI），并写进 workflow 注释 |
| SmartScreen 拦截未签名的 exe | **Windows 签名依赖 C1**（jason 核实 Trusted Signing 资格），属于 C3；C2 只在 CI 上验收 |
| 同用户进程冒充 | 与 Unix 相同，不在威胁模型内 |

**不做**：
- Windows 服务注册；
- AF_UNIX 和 loopback TCP 两条路线；
- 改 wire 帧；
- 动 sidecar-host 线的文件、PR，以及 `deny.toml`；
- ARM64 Windows；
- NSIS 安装包（C3）。

## 11. 请评审 / jason 确认

1. §7 的 W7 门槛选 (a) 还是 (b)。
2. 是否接受 §3.5 的 Windows 启动环境差异和其中的 MUST 条款。
3. `agent24-os-win` 与 `interprocess` 二选一（W4 评估）。

## 12. 评审处置表（第 1 轮，CHANGES → v2）

| 发现 | 处置 |
|---|---|
| H1 `spawn_isolated_piped` 消费了 ProcessGroup，拿不到 Job 成员；W6 依赖 W7，顺序反了 | 删掉 Job 校验层（推迟，见 §8），两个方向的鉴权都改为「DACL + 对端 SID」，删除「比 Unix 更强」的说法；W6 不再依赖 Job；W7 门槛改为可判定的二选一 (a)/(b)，(b) 列明 #275…#553；删除「工作区只能有一个 processkit」这条错误理由，只保留「不重复造轮子」 |
| H2 管道名可预测，可能被跨用户抢注 | 回调管道名加 128 位随机后缀，仍经 `A24_CALLBACK_SOCK` 传递（§3.1）；WIRE 的 Windows 小节写明 A3 客户端 MUST 先校验服务端 SID 再发 token，并带 `SECURITY_IDENTIFICATION`（§3.5）；attach 管道被占用时大声报错（§5） |
| H3「Unix 零改动」无法证明 | 读写两半按平台分别取别名，Unix 保留 `into_split`/`Owned*Half`（FIN 语义），签名已 check 通过（§3.2）；写明命名管道不支持半关闭，必须两半一起 drop；W2（原 W5）的验收改为「只新增别名，且别名解析到原类型」；W6 加 EOF 测试 |
| M1/M2 W0 划回 A7，YAML 有误 | YAML 修好（正则兼容 `\`、grep 后加 `\|\| true`、`if: always()`），落成 `windows-check.yml` 单独提交，同时支持 dispatch 和限定 paths 的 PR 触发；§1 留占位，跑完后回填 |
| M3/M5 `launch.rs` 在 Windows 上的问题；W10 依赖 A4 | 处置表补了 ① env 键大小写、② 路径判断 / `PATHEXT` / 只允许 `.exe` / `dunce`、③ `check_tree` 的降级方式；W9（原 W10）改为在 runner 上从源码构建 Sin90 |
| M4 软停顺序；stdin | 顺序写成 drain → 关回调 → 宽限 → `kill_all`；WIRE §7.3 补 Windows 小节；spawn 后立即关闭 stdin（§3.3） |
| M6 W5 应提到 W2 前 | 别名重构改为 W2，排在编译门 W3 之前 |
| Low：Node 顺序、tasklist、`.gitattributes`、dunce、升级前停模块、PowerShell 单脚本参数、只允许 `.exe`、daemon.json 写入 C3 契约、W4 Err/false 都算过、CI 只串行进程密集型 crate 并 exclude 两个 sidecar crate、Defender 风险、范围控制 | 都已写进对应章节：§8 推迟项、§6.1、§6.3、§6.2、§8 W4/W5、§10 |
