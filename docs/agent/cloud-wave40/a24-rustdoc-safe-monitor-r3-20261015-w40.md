# a24-rustdoc-safe-monitor-r3-20261015-w40 证据

> Linux 云端 test-only；**不是 macOS 验收**，不是 E0463 根因结论，不是产品批准。测试以 root 运行。通过不同模型验收之前不得用于真实 target。
> **超时说明**：started 回执 12:57Z，收口时已逾 20 分钟预算；收口阶段未再扩探索。

## 范围
base `aa81db1e`（w38 分支 tip）→ 新分支 `ab/apple-preview-cloud-20261015-w40-rustdoc-safe-monitor-r3`。修复提交 `34377b20e572ba2b61e071c711f1a90794a10bd4`（生产 diff +139/−49，base 上 298→388 行；≤300 行预算内）。只改 `artifact_observer.py`、README、新增 `tests/test_r3_hardening.py`、本目录、baton。旧 wave37/38、legacy 脚本、原 `test_safety.py` diff 均为 0 行（raw/05、raw/06）。

## 四项修复（对应测试）
- R1：启动时记录整条目录链 dev/inode；每次快照前后从 `/` O_NOFOLLOW 重走并比对，失效记 `path_detached`、exit 5，不再把旧目录数据标作当前路径。
- R2：meta 与 sha256 来自同一 O_NOFOLLOW regular fd（fstat→hash→fstat，再确认路径仍绑定该 fd）；变化只标 `unstable`。baseline/change/final 共用 `snap`。
- R3：`--artifacts-file` 用 O_NOFOLLOW|O_NONBLOCK 打开后 fstat 必须 regular，≤1MiB、≤4096 条、严格 UTF-8，否则 exit 2；`--max-seconds` 范围 (0,7200]，计时自 ready 起（README/--help 已写明）。
- R4：`NAME_RE.fullmatch`；任一路径组件含控制字符一律拒绝。

## RED → GREEN（原始输出见 raw/）
- RED（base 观察器 + 新测试，raw/01-red-base.txt）：Ran 34，**failures=11、errors=0**，原 21 + 2 个对照用例通过。失败均为行为偏差：R1 两例在父目录被永久替换/移走后 base 仍运行、exit0、final sha=旧目录 sha；3 个调度负控得到混合/过期快照（如旧 ino/size 配新内容 hash）；FIFO 列表启动 3s 内不返回；非 UTF-8 列表未结构化 exit 2；超大/超行列表被接受；结尾换行等名称被接受。
- GREEN（raw/02-green-final.txt 与提交后复跑 raw/03-green-committed.txt）：Ran 34，OK，exit 0。原 21 条断言未改动、未 skip。语法检查 raw/04-syntax.txt exit 0。
- 命令统一以 allowlist env + 0700 fake HOME/TMPDIR/CFFIXED/XDG 运行：`python3 -I -B -m unittest discover -s tools/qa/rustdoc-repro/tests -v`。

## 诚实记录 / 限制
- raw/05-scope.txt 的"源库存 ls-tree sha256"段因 git pathspec 不被 `ls-tree` 支持而失败（输出为空串 hash，**无效**）；同文件还暴露 py_compile 生成的 `__pycache__/*.pyc` 被误提交到 `34377b20e572ba2b61e071c711f1a90794a10bd4`。已在后续提交删除，并用 raw/06-scope-corrected.txt 重做：范围外 diff=0 行、pycache=0。raw/06 里两个"库存 sha256"因输入格式不同而不相等，**不作为证据**，以 diff=0 为准。未声称 raw-index 字节级库存已证，也未使用 git write-tree。
- 目录链/文件快照校验只在采样时刻发生：校验间隔内的来回替换瞬态、同 mtime 粒度的同尺寸原地写入仍可能漏检；不证明没有写者，不推断因果。
- 调度负控是确定性安排的调度，不是真实并发概率测量。
- 未跑 macOS（.dylib、/private 路径、非 root 属主）；未运行 Cargo/rustc；E0463 本机根因仍 unknown。
