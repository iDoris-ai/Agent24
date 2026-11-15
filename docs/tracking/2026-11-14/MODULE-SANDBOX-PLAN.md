# 模块沙箱 —— 方案与分片

2026-11-14，第 1 版（待 jason 拍板 §5 后冻结）。原则：只推主干（AGENTS.md 开头）。

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

- **macOS**：跳板在 exec 模块之前改为 exec `/usr/bin/sandbox-exec -f <profile> <program> <args…>`。profile 由内核按每个模块现生成，写到它的运行目录，只读。Chrome、Codex CLI 都用这套 seatbelt 机制。仓库里 `forbid(unsafe_code)`，所以不走 `sandbox_init` 的 FFI。
  - 2026-11-14 本机实验：profile `deny default` + 只放行数据目录写、系统目录读之后，写数据目录成功；读 `~/.zshrc`、写包目录、`curl https://example.com` 全部被拒。
- **Linux**：第二步再做（Landlock 管文件，网络用 seccomp 或网络命名空间）。第一版在 Linux 上照常启动，同时在 `/components` 上标「未隔离」。
- **Windows**：不做。

## 4. 分片

| 片 | 内容 | 验收（主干） |
|---|---|---|
| SBX-0 | 探针，不出 PR：用 examples 里的 sin90 / 测试模块在沙箱下跑完握手 + 一次工具调用。重点确认继承的 fd 3 监听 socket 和回调 unix socket 能用，SQLite（sin90.db 的 WAL）能写 | 记录最小 profile |
| SBX-1 | `agent24-os-proto`：按模块生成 profile（纯函数 + 单测）；跳板在 macOS 上经 `sandbox-exec` 启动模块 | 测试模块：写数据目录成功；读 `$HOME` 下一个文件失败；连 127.0.0.1 一个 TCP 端口失败；握手和工具调用照常通过 |
| SBX-2 | `/components` 和桌面的组件页显示每个模块的隔离状态（已隔离 / 未隔离及原因） | 有一个页面能看到状态 |
| SBX-3 | 模块真的需要更多权限时的出口（取决于 §5 问题 2） | 按拍板结果 |
| SBX-4（之后） | Linux Landlock | — |

## 5. 请 jason 拍板

1. **范围**：第一版只管 OS 模块，MCP 和 `shell_exec` 往后放？（推荐：是。OS 模块我们完全掌控启动，改动最小；MCP 第三方生态差异太大，硬套会把大部分服务器弄坏。）
2. **模块要联网或读更多文件怎么办**：A）一律不给，必须经内核调用；B）清单里声明 `network` / 额外目录，安装时让用户确认后放行。（推荐：先 A。现有模块都不需要；等真有模块要用再做 B。）
3. **沙箱起不来时**（profile 生成失败、`sandbox-exec` 不存在）：拒绝启动，还是不加隔离照常启动并标「未隔离」？（推荐：macOS 上拒绝启动，因为这时多半是被篡改了；Linux 第一版没有沙箱，照常启动并标「未隔离」。）
