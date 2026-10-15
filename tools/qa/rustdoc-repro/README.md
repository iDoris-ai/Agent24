# rustdoc E0463 研究工装（test-only）

## 历史脚本：已停用（fail-closed）
`run-experiments.sh`、`make-fixture.sh`、`target-window-watch.sh` 是 w37 的历史研究脚本，**未通过安全验收**。已知问题：不加校验就 `rm -rf`、复用已有目录、删除预先存在的 stop 文件、跟随 OUT 符号链接覆写。现在三者第一行就以 exit 3 拒绝执行，**没有任何环境变量或参数能解除**。原文保留在拒绝语句之后，只作为阅读材料。

## `artifact_observer.py` / `observe-artifacts.sh`：只读观察器（w38）
**Parent 不同模型验收之前，不得用于本机目标。**

```bash
tools/qa/rustdoc-repro/observe-artifacts.sh \
  --target /ABS/NO-SYMLINK/rust/target \
  --out-dir /ABS/PRIVATE/PARENT/new-obs-dir \
  --artifact /ABS/.../target/debug/deps/libagent24_store-<16hex>.rlib \
  --artifact /ABS/.../target/debug/deps/libasync_trait-<16hex>.dylib \
  --interval-ms 10 --max-seconds 600
# 等 stdout 出现一行 {"status":"ready",...}（同时会创建 <out-dir>/ready）后再启动 reader；结束时：touch <out-dir>/stop
```

- **artifact 列表**：只接收精确的绝对路径，应取自 reader 那条 rustdoc argv 的 `--extern` 值。文件名必须 **完整匹配**（fullmatch，结尾换行或任何控制字符都拒绝）`lib<crate>-<16hex>.(rlib|rmeta|so|dylib)` 或 `<crate>-<16hex>.d`；任一路径组件含控制字符也拒绝。`--artifacts-file` 必须是 regular 文件（以 `O_NOFOLLOW|O_NONBLOCK` 打开后 fstat；FIFO/设备在启动时立即以 exit 2 拒绝，不会挂死），UTF-8，≤1 MiB，≤4096 个条目，否则 exit 2。路径必须位于 target 内，且任何一级都不能是符号链接、`.` 或 `..`。启动时该文件必须存在且是普通文件。
- **target**：必须是绝对路径、每一级都不是符号链接，属主是当前用户。macOS 上的 `/tmp` 本身是符号链接，所以要传 `/private/...` 这样的真实路径。
- **out-dir**：必须是尚不存在的新目录。它的父目录必须属当前用户、对属主可写、对组和其他用户不可写，而且不能位于 target 内。目录用 `mkdir(0700)` 独占创建；里面所有文件都以 `O_CREAT|O_EXCL|O_NOFOLLOW` 加 dirfd 创建。stop 文件只读取、从不删除。
- **采样与输出**：每个间隔记录一次元数据（inode、nlink、size、mtime_ns、ctime_ns），附带单调时钟和墙钟时间。元数据与 sha256 都来自**同一个** `O_NOFOLLOW` regular fd：fstat → hash → fstat，并确认路径仍绑定该 fd；任何变化只标 `unstable`（带原因），不形成混合快照。每次快照前后都从 `/` 重走整条目录链并核对启动时记录的 dev/inode；target 或其子目录被改名、替换、变成符号链接时记录 `path_detached` 并以 exit 5 停止，不再把旧目录的数据标作当前路径。在基线、每次元数据变化、结束时计算 sha256；加 `--hash-every-sample` 则每次采样都 hash。结果写入 `events.jsonl`（每行一个事件）和 `summary.json`（含 `max_observed_sample_gap_ns`）。
- **退出码**：0 表示通过 stop 文件或 SIGTERM/SIGINT 正常停止；4 表示达到 `--max-seconds`；5 表示目录链被替换（`path_detached`）或最终快照不稳定（summary 的 `final_stable=false`）；2 表示参数或校验错误，此时不会创建任何输出；3 表示运行期错误，会尽量写 `error.json`。所有错误都会在 stderr 打一行 JSON。
- **`--max-seconds`**：范围 (0, 7200]，nan/inf/越界一律 exit 2。计时从 stdout 的 ready 行之后开始；启动校验与基线 hash 不计入。
- **不做的事**：不运行 Cargo，不杀进程，不删除、不修改 target 里的任何东西，不扫描 target 的其他文件。
- **局限**：两次采样之间的短暂变化可能漏检；目录链校验也只发生在采样时刻，校验间隔内来回替换仍可能漏检，不证明没有写者；同 mtime 粒度内的同尺寸原地写入也可能不被 fstat 比较发现。"未观测到变化"不等于"没有变化"；观测到的变化也不说明是谁写的，更不说明因果。

测试（只用标准库，在自建的私有临时目录里运行，不运行 Cargo）：
```bash
python3 -I -m unittest discover -s tools/qa/rustdoc-repro/tests -v
```
