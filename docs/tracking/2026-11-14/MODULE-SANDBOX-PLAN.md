# 模块沙箱 —— 方案与分片

2026-11-14，第 1 版。§5 已于 2026-11-15 拍板，方案冻结。原则：只推主干（AGENTS.md 开头）。

## 1. 问题

模块现在以用户本人的完整权限运行。OS 模块（`domain-os.yml` 的 `spawn`）由 `agent24-os-proto/src/launch.rs` 起进程，已有的隔离只有三样：清空环境变量（`env_clear` + 白名单）、独立进程组、工作目录设成包目录。**文件和网络都不受限**：一个被攻陷或写错的模块能读 `~/.ssh`、浏览器资料、Agent24 自己的数据库，也能直接联网把数据带走。

ADR-K1-01 §113、ADR-K1-02 §114 都写明「OS 进程分离不等于文件系统或网络沙箱」，这个缺口是已知的，只是一直没有做。

## 2. 目标（第一版）

只管 **OS 模块**（我们自己起进程、自己有跳板 trampoline 的那一类）：

| 资源 | 规则 |
|---|---|
| 写文件 | 只能写自己的数据目录 `A24_DATA_DIR`，以及系统临时目录 |
| 读文件 | 系统目录（`/usr`、`/System`、`/Library` 等）、自己的包目录、自己的数据目录；**其余都拒绝**，包括家目录 |
| 网络 | 默认拒绝。只放行内核回调用的 unix socket，以及 fd 3 上继承来的监听 socket |
| 模型 / 记忆 / 外部 API | 一律经内核调用（现有 kernel_call 与能力授权），不允许模块自己联网 |

做不到的部分，这一版明确不做：MCP 服务器（第三方，常用 npx/uvx，要联网、要读写大范围的文件）、`shell_exec` 工具、A3 attach 模块（进程由模块自己起，我们管不到）。

## 3. 机制

- **macOS**：跳板在 exec 模块之前改为 exec `/usr/bin/sandbox-exec -p <profile> <program> <args…>`。profile 是现生成的文本，经只给跳板看的环境变量交下去，exec 前删掉，不落盘。仓库里 `forbid(unsafe_code)`，所以不走 `sandbox_init` 的 FFI。`sandbox-exec` 不存在或 profile 生成失败时拒绝启动，不带隔离跑。
- **Linux**：第二步再做（Landlock 管文件，网络用 seccomp 或网络命名空间）。第一版在 Linux 上照常启动，不套 seatbelt。标「未隔离」是 SBX-2 的事。
- **Windows**：不做。

### 最小 profile（SBX-0，2026-11-15，本机 macOS）

用 `/usr/bin/sandbox-exec -p` 跑通过两类程序。父进程先把 unix 监听 socket 放在 fd 3 上再 exec。

- **node**（包外、PATH 解析）。本机 `command -v node` 是 fnm 的 multishell 符号链接，realpath 为 `~/.local/share/fnm/node-versions/v24.21.0/installation/bin/node`。必须 exec **canonical 后的路径**：符号链接那份路径即使目录在 subpath 里，`execvp` 仍是 EPERM。数据目录可写；读家目录文件 EPERM；TCP `127.0.0.1` EPERM；`connect` 回调 unix socket 成功；安装前缀下的 `lib/` 可读。这次探针里 node 用了错误的 `net.Server({fd})`（应是 `listen({fd:3})`），所以 **node 侧没有把 fd 3 的 accept 跑完**；fd 3 仍然有效是用 python 证明的（监听 socket 在 exec 前就在 fd 3 上时，沙箱里的 `accept` 会一直等到对端，而不是 EBADF）。
- **python3**（pyenv 3.11.9，同样在家目录里的安装前缀）。SQLite `journal_mode=WAL` 在数据目录写出 `probe.db`、`probe.db-wal`、`probe.db-shm`。读家目录 EPERM，TCP EPERM。
- **agent24-documents** 这次没有单独编出二进制再跑。它和上面一样是 libSystem 进程，SQLite WAL 落在数据目录，用的是同一份 profile。握手和 fd 3 上的工具调用由 SBX-1 的 macOS 测试走真实 `start()` 再验。

最小允许集合（最后匹配；`(deny default)` 打底）：

- 进程：`process-exec`、`process-fork`、`signal`、`sysctl-read`、`mach-lookup`、`file-ioctl`。不放 `process*`、`ipc-posix-shm`（WAL 是数据目录里的普通文件）。
- 读：`(literal "/")` 和 `(file-read-metadata (subpath "/"))` 都要有。没有前者 node 直接 SIGABRT（它要 stat 根目录）；没有后者，经 `/var` → `/private/var` 的路径 `lstat` 失败。这不放开文件内容。系统目录 `/usr`、`/bin`、`/sbin`、`/System`、`/Library`、`/private/etc`、`/dev`、`/opt/homebrew`、`/usr/local`，加上包目录、数据目录、临时目录，以及程序的父目录和祖父目录（canonical 与交给 exec 的原始路径各算一次）。祖父目录是 `/` 或家目录本身时不放行，所以 pyenv/fnm 能读到自己的库，家目录其余部分不行。`/usr/bin/python3` 是 Xcode 桩，会读 `/var/select/developer_dir`；链接在、且目标不在 `/usr`、`/Library`、`/System` 下时，把目标目录加进读（给系统 python 用）。
- 写：数据目录、临时目录，以及 `/dev/null`、`/dev/zero`、`/dev/dtracehelper`（literal，不是 subpath）。
- 网络：不放行 `network*`。`(network-inbound (local unix-socket))` 让 fd 3 `accept`；`(network-bind (local unix-socket))` 让 `connect` 能绑本地 unix 端（比「只绑回调 socket」宽，见文末 FU）；`(network-outbound (remote unix-socket (path-literal …)))` 只写回调 socket 的 canonical 路径和原样路径。TCP 返回 EPERM（errno 1）。
- 路径一律 canonical（`/var` → `/private/var`，`/tmp` → `/private/tmp`）。交给模块的 `A24_*` 仍是 daemon 原来的字符串，不改写。引号和反斜杠按 seatbelt 双引号规则转义。

## 4. 分片

| 片 | 内容 | 验收（主干） |
|---|---|---|
| SBX-0 | 探针，不出 PR：用 examples 里的 sin90 / 测试模块在沙箱下跑完握手 + 一次工具调用。重点确认继承的 fd 3 监听 socket 和回调 unix socket 能用，SQLite（sin90.db 的 WAL）能写 | 记录最小 profile |
| SBX-1 | `agent24-os-proto`：按模块生成 profile（纯函数 + 单测）；跳板在 macOS 上经 `sandbox-exec` 启动模块 | 测试模块：写数据目录成功；读 `$HOME` 下一个文件失败；连 127.0.0.1 一个 TCP 端口失败；握手和工具调用照常通过 |
| SBX-2 | `/components` 和桌面的组件页显示每个模块的隔离状态（已隔离 / 未隔离及原因） | 有一个页面能看到状态 |
| SBX-3 | 模块要更多权限时的出口 | **暂不做**（§5 问题 2 选了 A） |
| SBX-4（之后） | Linux Landlock | — |

## 5. 已拍板（jason 2026-11-15）

1. **范围**：第一版只管 OS 模块。MCP、`shell_exec`、A3 attach 往后放。
2. **模块要联网或读更多文件**：一律不给，必须经内核调用（方案 A）。清单里声明再放行（方案 B）以后再说。因此 **SBX-3 暂不做**。
3. **沙箱起不来时**（`/usr/bin/sandbox-exec` 不存在，或 profile 生成失败）：macOS 上拒绝启动，错误写清楚。Linux 第一版照常启动；标「未隔离」留给 SBX-2。

## 以后再说（FU）

- `file-read-metadata (subpath "/")` 让模块能看到整棵树的元数据（文件名、类型），内容仍然拒绝。
- `network-bind (local unix-socket)` 允许绑定任意 unix socket，不只是回调那一个。
- `(literal "/")` 允许读根目录这个目录本身。
- 程序前缀只到父目录和祖父目录；再深的运行时布局没测。
- 测试夹具的包内启动器把 `python3` 的绝对路径编进二进制再 `execv`。第一版只在该文件小于 1 MiB 且字节里含有这个路径时，额外放行该解释器的安装前缀。更大的二进制不扫描，别的解释器名字不放行。
- 本机这个会话继承了 fd 3 时，跳板测试可能失败；不为此改代码。
- `agent24-documents` 二进制没有在 SBX-0 里单独跑过。
- node 探针用错了 adopt-fd 的 API，fd 3 的 accept 是用 python 证明的，不是用 node。
