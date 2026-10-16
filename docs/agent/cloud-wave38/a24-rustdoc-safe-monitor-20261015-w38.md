# a24-rustdoc-safe-monitor-20261015-w38：独验退回后的修订说明

> **声明**：Linux 云端，test-only。不是 macOS 验收，也不是产品批准。**本机的实际根因仍然 unknown。** w37 的证据文件（`docs/agent/cloud-wave37/*`）原封未动，以下是对它们的修订与撤回。

## 1. 对 w37 结论的逐项撤回与更正（对应 Sol 的发现 F1–F6）

| 发现 | w37 原表述 | 更正 |
|---|---|---|
| F1 | E10 “只换 proc-macro dylib 在 Linux 不足以触发” | **撤回**。E10 的写者实际**新建了另一个哈希的 `.so`**，reader 引用的那个文件 inode 没变。也就是说，E10 根本没有测到“同名 proc-macro 被替换”，**不能据此排除 H2**。 |
| F2 | E5 “RUSTFLAGS 不同不会覆写，H3 推翻”；监视器“无 CHANGE 则 H1 被推翻”“CHANGE 即支持外部写者” | **撤回**。单个 `--cfg` 对照只覆盖一种 RUSTFLAGS 情形，不能排除整个 RUSTFLAGS 机制。0.5 秒采样会漏掉约 40ms 的暂缺，也会漏掉元数据不变的内容改动，所以**没看到 CHANGE 不能排除写者**。反过来，看到 CHANGE 也**不能归因于外部进程**：监视器不知道写者是谁，可能是 runner 自己，也可能是任何别的进程。 |
| F3 | 工装可安全运行 | **撤回**。runner 不加校验就 `rm -rf WORK`；generator 复用已有目录；watcher 会删除预先存在的 stop，还会跟随 OUT 符号链接覆写。w38 RED 中的 victim 测试已在私有 fake 目录里复现了这些行为（见 `raw/01-red.txt`）。三个脚本现在都 fail-closed。 |
| F4 | 文档里的诊断命令 | **撤回**。那条命令失败时总体仍可能 exit 0；没有 ready 握手、没有 trap、没有 wait；`PIPESTATUS` 只是 echo 出来，最终 exit 没有保留。 |
| F5 | “原始输出见 `*.linux-1.9x.out`” | **更正**：这两个 `.out` 是经过 grep/head 截取的**摘要，不是完整原始输出**。E12/E12b 的关键 stderr 只有一部分在交付物里；完整日志在已回收的云容器里，**已经无法取回**。 |
| F6 | E13 探针 | **撤回**。E13 用的是任意一个 first-glob，而且只看 `.so`，不代表 reader 实际引用的精确路径；macOS 上对应的应是 `.dylib`。 |

**仍然成立的有限表述**：在本 Linux fixture 上，人为注入的同名 rlib 偏斜（E9）和 rlib+rmeta 同时缺席（E12b），**足以**让只有 doctest 失败、报 E0460/E0463。这只是本 fixture 上的**充分条件演示**。它不说明本机确实发生了这件事，不构成 race 或编译器错配的证据，也不说明这是唯一的机制。

## 2. w38 修复内容
1. 历史脚本 `run-experiments.sh`、`make-fixture.sh`、`target-window-watch.sh` 改为第一行就 exit 3，**没有环境变量后门**。原文保留在拒绝语句之后，只作阅读材料。
2. 新增只读观察器 `tools/qa/rustdoc-repro/artifact_observer.py`（298 行）和 `observe-artifacts.sh`。用法与校验规则见该目录的 `README.md`。它不运行 Cargo、不杀进程、不删除也不修改 target。它的局限写在代码里，也会写进 `summary.json`：“未观测到变化”不等于“没有变化”，观测到的变化也不说明写者身份或因果。
3. 测试 `tools/qa/rustdoc-repro/tests/test_safety.py`（21 项）：

| 类别 | 覆盖内容 |
|---|---|
| 历史脚本 | 3 个脚本都拒绝执行，且 victim 目录与 stop 文件未被改动 |
| 观察器拒绝 | 已存在的 out-dir、预先存在的 stop、out-dir 本身或其祖先是符号链接、out-dir 在 target 内或与 target 重叠、target 不存在、artifact 是逃逸的符号链接、target 子目录是逃逸的符号链接、artifact 在 target 外、路径含 `..`、非法文件名前缀、父目录不可写或组可写、没有 artifact |
| 观察器运行 | ready 握手与 0700 权限；新 inode 替换和原地改内容都产生 change 事件并带 sha256；文件缺席被记录；stop 文件停止时 exit 0；SIGTERM 时 exit 0；超时 exit 4 |

## 3. RED → GREEN（完整原始输出见 `raw/`）
- `raw/01-red.txt`：修复前，21 项中 1 个 FAIL、20 个 ERROR，exit 1。
  - 历史脚本：make-fixture 复用了 victim 目录并返回 exit 0；run-experiments 和 target-window-watch 超时（危险行为发生在私有 fake 目录内）。
  - 观察器：尚不存在。
- `raw/02-syntax.txt`：`bash -n` 和 `py_compile` 全部 exit 0。
- `raw/03-green.txt`：21/21 OK，exit 0。
- `raw/04-scope.txt`：除允许的路径外，相对 c57e17e 的 diff 为 0 行；`docs/agent/cloud-wave37` 的 diff 为空，说明生产代码和旧证据都没动。
- 测试只用测试自建的私有临时目录和 fake target，没有运行 Cargo 或 rustup，也没有下载任何 toolchain。
- **测试局限**：
  - 只在 Linux 上以 root 运行；属主校验和 macOS 路径（`.dylib`、`/private`）没有实际验证。
  - 个别拒绝测试可能是被另一条规则先拦下的，例如 out-dir 与 target 重叠时，实际触发的是“out-dir 已存在”。测试只断言 exit 2 并且 victim 未变，没有断言具体的错误码。

## 4. 建议的下一步（仅建议；Parent 验收前不在本机使用）
不建议第三次原样完整重跑 Cargo。应当先确认**真正排它的写者窗口**，也就是确保没有其他 cargo 或工具写这个 target，然后：
- 只捕获那条失败 rustdoc 的**准确 argv**，以及其中的 `--extern`/`-L` 路径；
- 在调用前后各对这些精确路径做一次元数据和 sha256 快照（可以用本观察器的基线和结束事件）；
- 保存**完整 stderr**，不做 grep 截取。

在此之前，本机根因继续记为 unknown。
