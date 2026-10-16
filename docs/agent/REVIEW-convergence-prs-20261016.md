# 独立复核：2026-10-16 分支收敛 PR

复核人：只读。工作树 `review/convergence-prs-20261016`，基线 `origin/main` = `72c2a5802640ffdd341eb160f5d189cdb3ecde22`（2026-10-16 15:17 +0700，`docs: 跟踪交接、M1 记忆研究与 v0.5.0 桌面审计资料 (#965)`）。

远端引用在写报告前又 fetch 了一次。除 `#1023` 外，下面各尖端与第一次 fetch 相同。`#1023` 在复核期间多了一个提交，数字以第二次 fetch 为准。

本报告**不是审批**，不替代外部 PR-Daemon 评审，也不代表 CI 已通过。没有跑测试套件。结论只来自 `git fetch`、`git diff`、`git merge-tree --write-tree`、`git show`、`gh pr view`。没有 checkout 这些分支，没有推送到 `ab/*`。

## 判定方法

对每个分支记 base 为 `B`、尖端为 `H`：

1. **分支自带的改动**：`git diff --stat B...H` 与 `git diff --name-status B...H`（merge-base 之后、只在 `H` 历史上的差异）。这个数字会把「base 尖端已经有同样内容、但提交不是同一个」的文件也算进去。
2. **合进去之后 base 上真正会变的东西**：`git merge-tree --write-tree B H` 得到树 `T`，再 `git diff --stat B T`。干净合并（退出码 0）且这份 stat 与三方 diff 一致时，差异是分支的增量，不是「分支少了 base 的新提交」造成的假差异。
3. **是否已在 base 上**：比较 blob。`git rev-parse H:path` 与 `B:path` 相同，则三方 diff 里的该文件不会被这次合并改写。再对特征符号做 `git grep`（例如 `OpenDesignComponentPause`、`SourceClassification`）。
4. **会不会把 main 的新行为盖掉**：对合并树 `T` 比较四个敏感路径的 blob 是否仍等于 `origin/main`：
   - `rust/crates/agent24-protocol/src/state_file.rs` 的 `state_dir()`（`AGENT24_DATA_DIR`，非空绝对路径原样使用，否则 `$HOME/.agent24`）。main 上由 `#1015` `729a7159` 引入，函数在 `state_file.rs` 约 127–146 行。
   - `rust/apps/agent24-cli/src/service.rs` 的 `PASSTHROUGH_VARS`。main 上是 **19 项**，第一项是 `AGENT24_DATA_DIR`（`#1015` 把 18 改成 19）。
   - 桌面错误码文案：`apps/desktop/src/renderer/pages/Chat.tsx` 与 `Chat.i18n.ts`，来自 `#1014` `fdcf84d2`。
   - `agent24-hw`：`rust/crates/agent24-hw/src/disks.rs`、`probe.rs`。
5. **任务 PR 合到了哪**：`gh pr view`。`git merge-base --is-ancestor <mergeCommit> origin/main`。

main 比 hw / idoris 这一簇只多 3 个提交，这 3 个就是上面的敏感面加一份文档：

| 提交 | 说明 |
|---|---|
| `fdcf84d2` | `#1014` 对话失败文案按错误码区分（只动 `Chat.tsx` / `Chat.i18n.ts` / `Chat.test.tsx`） |
| `729a7159` | `#1015` `AGENT24_DATA_DIR`：`state_file.rs`、`service.rs`（透传列表 18→19）、`backend-manager.ts`、daemon 测试、文档 |
| `72c2a580` | `#965` 文档与审计资料 |

`agent24-hw` 自这些分支的 merge-base `622be5d7` 到 main **没有再改**（`git diff --stat 622be5d7 origin/main -- rust/crates/agent24-hw` 为空）。

## 总表

| PR | 尖端 | 相对 base | 合并到声明的 base | 判定 |
|---|---|---|---|---|
| #1023 | `221cfd43` | main `0` 后 / `3` 前 | 干净，结果等于三方 diff | **可直接合** |
| #1024 | `86808d30` | main `73` 后 / `2` 前 | 干净，只剩 2 个 memory 文件 | **可直接合** |
| #1025 | `a9b36cee` | main `3` 后 / `1` 前 | 干净，只剩 `disks.rs` + `probe.rs` | **可直接合** |
| #1026 | `1a37d25c` | main `3` 后 / `3` 前 | 干净，但含仓库根回执，且 HW 与 #1025 同 blob | **先删根回执再合** |
| #1027 | `3a0c6a6d` | main `3` 后 / `10` 前 | 干净，但含仓库根回执，且 HW 与 #1025 同 blob | **先删根回执再合** |
| #1028 | `8a17a382` | `ab/idoris` `2` 后 / `7` 前 | 对 idoris 干净；对 main 在 `service.rs` 冲突 | **需先吸收 main 再合** |
| #1029 | `127ded38` | `ab/idoris` `0` 后 / `8` 前 | 对 idoris 干净；对 main 在 `service.rs` 冲突 | **需先吸收 main 再合** |

「后 / 前」是 `git rev-list --left-right --count origin/main...origin/<分支>`（或 idoris 为 base 时换成 `origin/ab/idoris`）：左边是 base 有而分支没有的提交数，右边是分支独有提交数。

---

## #1023 `ab/showcase-download` → main

- PR：<https://github.com/iDoris-ai/Agent24/pull/1023>，OPEN，`MERGEABLE` / `BLOCKED`，GitHub 计数 +1706 / −38，10 文件，`headRefOid` = `221cfd432e92bb45cc14a682d9f2465f0344f6db`。
- 尖端包含 main（`git merge-base --is-ancestor origin/main origin/ab/showcase-download` 为真）。独有提交 3 个：

```
221cfd43 test(showcase): 对齐 i18n 后的期望文案（MB 而非 MiB），修合并后 CI 红
aff0450b chore(showcase): SD-01 —— 吸收 main（2026-10-16），解冲突：保留暂停/续传功能 + 采用 main 的 i18n
9038c594 fix(creative): SD-01 —— 安全暂停续传与真实下载速度状态 (#967)
```

`aff0450b` 的父提交是 `9038c594` 与 `72c2a580`（当前 main）。

### #967 合到了哪里

`gh pr view 967`：MERGED，base = `ab/showcase-download`，head = `jhfnetboy/ab-showcase-download-06-timer-finally`，`mergedAt` = 2026-10-15T05:40:41Z，`mergeCommit` = `9038c59424ef1ec38ddf7a72c970fdc13d08cef3`。

`9038c594` **不是** main 的祖先。`git branch -r --contains 9038c594` 只有 `origin/ab/showcase-download`。main 上 `git grep OpenDesignComponentPause` 为 0。暂停/续传不在 main。

### 三方 diff（`origin/main...origin/ab/showcase-download`）

```
 apps/desktop/src/main/independent-review.test.ts   | 266 +++++++
 apps/desktop/src/main/main.ts                      |   4 +
 apps/desktop/src/main/open-design-component.test.ts| 799 ++++++++++++++++++++-
 apps/desktop/src/main/open-design-component.ts     | 431 ++++++++++-
 apps/desktop/src/main/preload.ts                   |   2 +
 apps/desktop/src/main/r2-independent-review.test.ts| 109 +++
 apps/desktop/src/renderer/pages/Creative.test.tsx  |  48 +-
 apps/desktop/src/renderer/pages/Creative.tsx       |  50 +-
 apps/desktop/src/shared/ipc-types.ts               |  11 +-
 docs/agent/PLAN-SHOWCASE-DOWNLOAD.md               |  24 +
 10 files changed, 1706 insertions(+), 38 deletions(-)
```

`merge-tree` 树 `26c05aa3ba2564e43b7f1593719147421571a598`，相对 main 的 stat 与上面相同。没有冲突标记。

| 文件 | 归类 |
|---|---|
| `apps/desktop/src/main/open-design-component.ts` | 生产代码。暂停保留已刷盘前缀、`O_NOFOLLOW`/`O_NONBLOCK`、Range 续传、空闲时把速度推成「等待网络」 |
| `apps/desktop/src/main/main.ts`、`preload.ts`、`apps/desktop/src/shared/ipc-types.ts` | 生产代码。IPC `OpenDesignComponentPause` |
| `apps/desktop/src/renderer/pages/Creative.tsx` | 生产代码。暂停/继续、速度与剩余时间 |
| `independent-review.test.ts`、`open-design-component.test.ts`、`r2-independent-review.test.ts`、`Creative.test.tsx` | 测试 |
| `docs/agent/PLAN-SHOWCASE-DOWNLOAD.md` | 文档 |

### 回归

合并树里下面这些 blob 与 main 相同：`Chat.tsx`、`Chat.i18n.ts`、`Creative.i18n.ts`（`2e503c637632`）、`backend-manager.ts`、`state_file.rs`、`service.rs`。`#1014` 的错误码文案和 `#1015` 的 `state_dir()` / 19 项透传都还在。

需要知道的取舍：`221cfd43` 把三条测试从字面量 `MiB` 改成 main 的 i18n 输出 `MB`，并删掉 `Creative.tsx` 里多出来的三行闭合（吸收 main 时留下的重复 `</div>` / `)` / `}`）。暂停卡片仍写死「MiB」（`formatMb` 按 1024² 取整，main 的 `download.size` 文案是 `MB`）。这是新 UI 与 main 文案的单位不一致，不是把 `#1014` 改回去。

### 结论

**可直接合。** main 已在分支里。合入只增加这 10 个文件。`#967` 没有进过 main。

---

## #1024 `ab/m1-memory-hardening` → main

- PR：<https://github.com/iDoris-ai/Agent24/pull/1024>，OPEN，`MERGEABLE` / `BLOCKED`，+370 / −2，2 文件。
- 尖端 `86808d30f683262fcf7bc92d6d666fbede5850f6`。相对 main：73 后 / 2 前。merge-base `ab125de262827bd0f1c6680b427d9c241ccff785`。
- 独有提交：`9e74882f` `fix(memory): H01 —— 修复重复撤回审计标识碰撞`，以及把它合进来的 `86808d30`。

### #910 合到了哪里

`gh pr view 910`：MERGED，base = `ab/m1-memory-hardening`，head = `ab/m1-memory-hardening-02-retract-cycle`，`mergedAt` = 2026-10-13T06:12:45Z，`mergeCommit` = `86808d30`（就是本分支尖端）。

`86808d30` **不是** main 的祖先。含有它的远端分支只有 `origin/ab/m1-memory-hardening`。

### 三方 diff

```
 rust/crates/agent24-memory/src/assertion.rs       |  27 +-
 rust/crates/agent24-memory/tests/retract_cycle.rs | 345 ++++++++++++++++++++++
 2 files changed, 370 insertions(+), 2 deletions(-)
```

`merge-tree` 树 `ede336ff48ca29ddecc9aae5d7592f1c034a7d04`，相对 main 的 stat 与上面相同。

| 文件 | 归类 |
|---|---|
| `rust/crates/agent24-memory/src/assertion.rs` | 生产代码 |
| `rust/crates/agent24-memory/tests/retract_cycle.rs` | 测试 |

`forget` 里撤回事件的 id 不再永远是 `checksum(json(["retract", owner, id]))`。已占用时在同一写事务里从 `mem_events` 取出 `base` 与 `base-%`，再选第一个空的 `base` 或 `base-N`。main 的 `assertion.rs` 第 70 行仍是静态 `event_key`。

`git diff --stat ab125de2 origin/main -- rust/crates/agent24-memory` 为空。main 这 73 个提交（文档抽取、桌面 i18n、`agent24-hw`、`#1014`、`#1015`、`#965`）没有改 memory crate。两方 diff 里大片桌面文件不同，是分支尖端还停在旧树上；合并树不带这些文件。

### 回归

合并树的 `Chat.tsx`、`Chat.i18n.ts`、`state_file.rs`、`service.rs` 与 main 同 blob。分支尖端的 `PASSTHROUGH_VARS` 仍是旧的 17 项，但该文件不在三方 diff 里，合并留下 main 的 19 项。

### 结论

**可直接合。** 落后 73 个提交不构成行为回退：合并结果只有 H01 的两个文件。H01 不在 main。

---

## #1025 `ab/preview-hw-23-probe-disk` → main

- PR：<https://github.com/iDoris-ai/Agent24/pull/1025>，OPEN，`MERGEABLE` / `BLOCKED`，+121 / −9，2 文件。
- 尖端 `a9b36ceee8a74f0c477b37836506933b09aafcf5`。相对 main：3 后 / 1 前。merge-base `622be5d7`。
- 唯一独有提交：`a9b36cee` `fix(hw): HW-1 —— 限制硬件探测最小刷新并修复磁盘最长挂载点匹配`。

### 三方 diff

```
 rust/crates/agent24-hw/src/disks.rs | 90 ++++++++++++++++++++++++++++++++++---
 rust/crates/agent24-hw/src/probe.rs | 40 ++++++++++++++++-
 2 files changed, 121 insertions(+), 9 deletions(-)
```

`merge-tree` 树 `85608a31dcc5d67ef7d21fa84b2aa647db04b76c`，stat 相同。

| 文件 | 归类 |
|---|---|
| `disks.rs` | 生产代码（同文件含测试）。挂载点按路径分量个数取最长匹配；过滤掉的更长挂载点仍能赢过较短的可见项 |
| `probe.rs` | 生产代码（同文件含测试）。`hardware_refresh_kind()` 只刷新 CPU 与内存；禁止 `System::new_all()` / 刷新进程 |

blob：`disks.rs` main `1dd17d29` → 分支 `07e49963`；`probe.rs` main `50917fc4` → 分支 `d4cc7df1`。main 上没有这段。

### 和现有 `agent24-hw`、以及 `ab/idoris` 的关系

main 自 `622be5d7` 没有再改 `agent24-hw`。这 3 个落后提交不碰 hw。合并树的 `service.rs`、`state_file.rs`、`Chat.tsx`、`backend-manager.ts` 与 main 同 blob。

`ab/idoris` 的 `probe.rs` 是另一 blob `5179e2a5`（提交 `dd10562e`，`fix(hw): IDORIS-04b —— 隔离硬件探测子进程的环境凭据`：`env_clear()`，只留 `PATH` 与 `LC_ALL=C`）。`disks.rs` 与 main 相同。

`git merge-tree --write-tree origin/ab/preview-hw-23-probe-disk origin/ab/idoris` 干净（树 `17be9eaf`），输出只有 `Auto-merging probe.rs`。该树相对 HW 分支只多 idoris 的 `probe.rs` +45；相对 idoris 正好是 HW-1 的 `disks.rs` +90 与 `probe.rs` +40。两处行为落在不同 hunk，自动合并同时留下。

### 结论

**可直接合。** 这是 HW-1 相对当前 main 的真实增量。`#1026` / `#1027` 携带同一对 blob，不要当成第二份、第三份独立修复。

---

## #1026 `ab/apple-preview-cloud-20261015-104002-v1-guard` → main

- PR：<https://github.com/iDoris-ai/Agent24/pull/1026>，OPEN，`MERGEABLE` / `BLOCKED`，+448 / −9，9 文件。
- 尖端 `1a37d25c23100ce085aa1fea46a78480032d8eb0`。相对 main：3 后 / 3 前。merge-base 仍是 `622be5d7`。
- 提交：`a9b36cee`（与 #1025 同一提交）、`1ee3b411` baton 回执、`1a37d25c` v1 checker 与单测。

### 三方 diff

```
 baton-result.json                                  |  64 +++++++++++
 docs/agent/cloud-wave37/a24-v1-guard-20261015-104002.md |  31 ++++++
 rust/crates/agent24-hw/src/disks.rs                |  90 ++++++++++++++--
 rust/crates/agent24-hw/src/probe.rs                |  40 ++++++-
 scripts/contract-v1-hardware/checker.mjs           |  62 +++++++++++
 scripts/contract-v1-hardware/checker.test.mjs      | 118 +++++++++++++++++++++
 scripts/contract-v1-hardware/cli.mjs               |  33 ++++++
 scripts/contract-v1-hardware/run-history.sh        |  12 +++
 scripts/contract-v1-hardware/test.sh               |   7 ++
 9 files changed, 448 insertions(+), 9 deletions(-)
```

`merge-tree` 树 `11daf63d`，stat 相同。`disks.rs` / `probe.rs` blob 与 #1025 相同（`07e49963` / `d4cc7df1`）。

| 文件 | 归类 |
|---|---|
| `disks.rs`、`probe.rs` | 生产代码。与 #1025 同一内容，不是另一份 HW 修复 |
| `scripts/contract-v1-hardware/checker.mjs`、`cli.mjs` | 脚本。纯函数，比较 `GET /decide/profile` 200 schema 是否只做加法 |
| `checker.test.mjs`、`test.sh` | 测试 |
| `run-history.sh` | 脚本。文档写明不接入默认 CI；对 `6072fd37` → `a9b36cee` 的预期是 RED（`free_disk_bytes` 被从 required 和 properties 删掉，`violations=2`，exit 1） |
| `docs/agent/cloud-wave37/a24-v1-guard-20261015-104002.md` | 文档 / 证据。写明 test-only，没有改 handler、schema、SDK，没有修 API |
| `baton-result.json` | 仓库根任务回执。`state: blocked`。main 上不存在。回执正文说 push 403、提交未到远端；该说法已过时，分支现在在 origin 上 |

checker 不在 main。它记录的 RED 对应当前 main 上已经存在的 openapi（`a9b36cee` 的三方 diff 不含 `protocol/openapi.yaml`，main 自 `622be5d7` 的 3 个提交也不改 openapi）。合入 checker **不会**把 schema 修绿。

### 回归

合并树的 `service.rs`、`state_file.rs`、`Chat.tsx`、`backend-manager.ts` 与 main 同 blob。不需要为了这 3 个 main 提交先 rebase。

### 结论

**先删仓库根 `baton-result.json` 再合。** 不是因为落后 main。HW 行为以 #1025 为准；本 PR 多出来的是 checker、文档，以及不该进 main 的回执。不要把 `run-history.sh` 接进默认 CI，除非另有提交把 v1 加法兼容补上。文档里的「预期 RED」应保持为 RED，不要改成假绿。

---

## #1027 `ab/apple-preview-cloud-20261015-w40-rustdoc-safe-monitor-r3` → main

- PR：<https://github.com/iDoris-ai/Agent24/pull/1027>，OPEN，`MERGEABLE` / `BLOCKED`，+2771 / −9，27 文件。
- 尖端 `3a0c6a6d57964cef4d52a05f27fc07e1217c5fd7`。相对 main：3 后 / 10 前。历史里包含 `a9b36cee`（HW-1）以及 w37–w40 的工装、证据、baton 回执。

### 三方 diff

`merge-tree` 树 `59eca84366a1b6ce707a499c6fc3fdba2464d992`，相对 main 27 文件，+2771 / −9，与三方 diff 一致。`disks.rs` / `probe.rs` 再次与 #1025 同 blob。

| 文件 | 归类 |
|---|---|
| `rust/crates/agent24-hw/src/disks.rs`、`probe.rs` | 生产代码。与 #1025 相同 |
| `tools/qa/rustdoc-repro/artifact_observer.py`、`observe-artifacts.sh` | 脚本。只读观察器（O_NOFOLLOW、精确 artifact 名、新建 0700 输出目录） |
| `run-experiments.sh`、`make-fixture.sh`、`target-window-watch.sh` | 脚本。README 写明历史工装已 fail-closed：第一行 exit 3，没有开关能重新打开 |
| `tools/qa/rustdoc-repro/tests/test_r3_hardening.py`、`test_safety.py` | 测试 |
| `tools/qa/rustdoc-repro/README.md` | 文档 |
| `docs/agent/cloud-wave37/*.md`、`*.linux-*.out` | 文档 + 证据 |
| `docs/agent/cloud-wave38/**`、`docs/agent/cloud-wave40/**` | 文档 + 原始 RED/GREEN 输出 |
| `baton-result.json`、`baton-decisions.tsv` | 仓库根回执。main 上不存在。`baton-decisions.tsv` 是观察器的取舍记录（不 `rm -rf`、不杀进程、不删 target） |

这些工装和证据文件在 main 上都不存在。

### 回归

合并树的 `service.rs`、`state_file.rs`、`Chat.tsx`、`backend-manager.ts` 与 main 同 blob。

### 结论

**先删仓库根 `baton-result.json` 和 `baton-decisions.tsv` 再合。** 工装与 `docs/agent/cloud-wave*` 证据是独有内容，可以留。HW-1 不要在这支上单独再审一遍。历史脚本保持 fail-closed 即可，不需要为了「能跑起来」重新打开 `rm -rf`。

---

## #1028 `ab/idoris-04-receipts` → `ab/idoris`

- PR：<https://github.com/iDoris-ai/Agent24/pull/1028>，OPEN，`MERGEABLE` / `BLOCKED`，base `ab/idoris`，+1124 / −34，14 文件。
- 尖端 `8a17a382e0fcf9772394980a8a3cca837c1463c4`。相对 `ab/idoris`：2 后 / 7 前。相对 main：3 后 / 12 前。
- `ab/idoris` 尖端 `f315630f`（`#984`，把探测子进程凭据隔离合进 idoris）。

`git merge-base --all` 给出两个 merge-base：`c145632f` 与 `dd10562e`。三方 diff 使用 Git 选中的 `c145632f`。

### 三方 diff（相对 `ab/idoris`）

```
 docs/agent/IDORIS-MANUAL-BUDGET.md                 |  20 ++
 docs/iDoris-consumer.md                            |   2 +
 rust/Cargo.lock                                    |  12 +
 rust/apps/agent24-cli/src/main.rs                  |  27 +-
 rust/apps/agent24-cli/src/service.rs               |  16 +-
 rust/apps/agent24-cli/tests/idoris_budget_env.rs   |  72 ++++
 rust/apps/agent24d/src/routes.rs                   |  97 +++++-
 rust/crates/agent24-comm/tests/daemon_supervise.rs | 163 +++++++--
 rust/crates/agent24-hw/src/probe.rs                |  45 +++
 rust/crates/agent24-models/Cargo.toml              |   1 +
 rust/crates/agent24-models/src/idoris.rs           | 378 +++++++++++++++++++++
 rust/crates/agent24-models/src/lib.rs              |   2 +
 rust/crates/agent24-models/src/router.rs           |  36 +-
 rust/crates/agent24-models/tests/idoris_gateway.rs | 287 ++++++++++++++++
 14 files changed, 1124 insertions(+), 34 deletions(-)
```

其中 `probe.rs` 的 +45 **已经在 base 上**。尖端 blob `5179e2a5` 与 `origin/ab/idoris:probe.rs` 相同。`merge-tree` 进 idoris 的树 `8b1627db` 不再改 `probe.rs`，有效增量是 13 文件、+1079 / −34。这就是「三方 diff / 试合看起来有这个文件，其实只是分叉历史里的同一内容」的例子。分支在历史上落后 `#984` 的两个合并提交，文件内容并不缺这次凭据隔离。

| 文件 | 归类 | 合入 idoris 后是否还改 base |
|---|---|---|
| `agent24-models/src/idoris.rs` | 生产代码。新网关客户端 | 是 |
| `agent24-models/src/lib.rs`、`router.rs`、`Cargo.toml` | 生产代码 | 是 |
| `agent24d/src/routes.rs` | 生产代码。把网关真实回执留在聊天路径 | 是 |
| `agent24-cli/src/main.rs`、`service.rs` | 生产代码。见下方透传列表 | 是 |
| `Cargo.lock` | 锁文件 | 是 |
| `agent24-cli/tests/idoris_budget_env.rs`、`agent24-models/tests/idoris_gateway.rs`、`agent24-comm/tests/daemon_supervise.rs` | 测试 | 是 |
| `docs/agent/IDORIS-MANUAL-BUDGET.md`、`docs/iDoris-consumer.md` | 文档 | 是 |
| `agent24-hw/src/probe.rs` | 生产代码 | **否**（blob 已等于 idoris） |

### 透传列表：这是会盖掉 main 的地方

`ab/idoris` 的 `PASSTHROUGH_VARS` 是 **18 项**，没有 `AGENT24_DATA_DIR`（停在 `#1015` 之前）。

`#1028` 把数组改成 **19 项**，第一项换成 `IDORIS_URL`，并明确 **不** 把 `IDORIS_API_KEY` 写入 plist（测试 `service_snapshot_never_persists_idoris_data_key`）。其余 18 项与 idoris 相同。

main 的 19 项是在同一位置插入 `AGENT24_DATA_DIR`，没有 `IDORIS_URL`。

`git merge-tree --write-tree origin/main origin/ab/idoris-04-receipts` 退出码 1，**唯一冲突文件**是 `rust/apps/agent24-cli/src/service.rs`。

`ab/idoris` 自己合进 main 是干净的（树 `f7dcec03`），只动 6 个文件：`PLAN-IDORIS-V031.md`、两份 iDoris 文档、`INTEGRATION-AGENTEAR-IDORIS.md`、`probe.rs` +45、`agent24-models/src/lib.rs`。**不含** `service.rs`。所以冲突是 #1028 新改的透传列表，不是 idoris 整支都合不进 main。

`state_dir()` 这两支都没改。丢掉的是 launchd 快照里的 `AGENT24_DATA_DIR`：守护进程仍会读 `~/.agent24`，而 CLI / 桌面去覆盖目录找它。用分支侧整文件覆盖 `service.rs` 就会发生这件事。正确结果是 **20 项**：保留 main 的 `AGENT24_DATA_DIR`，再追加 `IDORIS_URL`，并继续排除 `IDORIS_API_KEY`。

桌面 i18n / `Chat.tsx` 不在本 PR 的文件列表里。冲突只有 `service.rs`，其余 main 改动可以自动留下。

### 和 #1029 的关系

两支互合干净（树 `3a50daee`）。相对 #1029，合并结果多出本 PR 的 `routes.rs`、`agent24-cli/src/main.rs`、`docs/iDoris-consumer.md`。相对本 PR，多出 #1029 的 `model_callback.rs`、`idoris_wire_probe.rs`、双进程脚本和更厚的网关测试。可以先后合，后合的那次不必在这两支之间再解一次行为冲突。两支对 `service.rs` 的改法相同（都是把第一项换成 `IDORIS_URL`）。

### 结论

**需先吸收 main 再合**（目标若是最终发布到 main）。顺序：先把 main 合进 `ab/idoris`（这一步今天是干净的），再把本 PR 合上去，手改 `service.rs` 为 20 项。现在直接合进 `ab/idoris` 在 Git 上是干净的，但会把「没有 `AGENT24_DATA_DIR` 的 19 项」冻在 idoris 线上，下一步撞 main 时仍是同一处冲突。

---

## #1029 `ab/idoris-11-pair-acceptance` → `ab/idoris`

- PR：<https://github.com/iDoris-ai/Agent24/pull/1029>，OPEN，`MERGEABLE` / `BLOCKED`，base `ab/idoris`，+1420 / −33，14 文件。标题是 IDORIS-06 配对验收。
- 尖端 `127ded38dda15d760d2b41dfdd20b66130c45cbd`。相对 `ab/idoris`：0 后 / 8 前（idoris 的提交都在这支里）。相对 main：3 后 / 15 前。

### 三方 diff（相对 `ab/idoris`）

```
 docs/agent/IDORIS-MANUAL-BUDGET.md                 |  20 ++
 rust/Cargo.lock                                    |  12 +
 rust/apps/agent24-cli/src/main.rs                  |  19 +-
 rust/apps/agent24-cli/src/service.rs               |  16 +-
 rust/apps/agent24-cli/tests/idoris_budget_env.rs   |  72 ++++
 rust/apps/agent24d/src/model_callback.rs           |  79 ++++-
 rust/crates/agent24-comm/tests/daemon_supervise.rs | 163 +++++++--
 rust/crates/agent24-models/Cargo.toml              |   1 +
 rust/crates/agent24-models/examples/idoris_wire_probe.rs |  56 +++
 rust/crates/agent24-models/src/idoris.rs           | 388 +++++++++++++++++++++
 rust/crates/agent24-models/src/lib.rs              |   2 +
 rust/crates/agent24-models/src/router.rs           |  49 ++-
 rust/crates/agent24-models/tests/idoris_gateway.rs | 376 ++++++++++++++++++++
 scripts/test_idoris_two_process.py                 | 200 +++++++++++
 14 files changed, 1420 insertions(+), 33 deletions(-)
```

`merge-tree` 进 idoris 的树 `c3969a73`，stat 与三方 diff 一致。14 个文件的 blob 都与 idoris 不同，没有「已经在 base 上」的假差异。

| 文件 | 归类 |
|---|---|
| `agent24-models/src/idoris.rs`、`lib.rs`、`router.rs`、`Cargo.toml` | 生产代码 |
| `agent24-models/examples/idoris_wire_probe.rs` | 示例 / 探针 |
| `agent24d/src/model_callback.rs` | 生产代码。回调路径上的网关守卫 |
| `agent24-cli/src/main.rs`、`service.rs` | 生产代码。透传列表与 #1028 同一改法 |
| `Cargo.lock` | 锁文件 |
| `idoris_budget_env.rs`、`daemon_supervise.rs`、`idoris_gateway.rs` | 测试 |
| `scripts/test_idoris_two_process.py` | 脚本。双进程验收 |
| `docs/agent/IDORIS-MANUAL-BUDGET.md` | 文档 |

### 回归

与 #1028 同一处：`merge-tree` 进 main 只在 `service.rs` 冲突。尖端透传列表是 `IDORIS_URL` + 原来的 18 项，没有 `AGENT24_DATA_DIR`。`state_dir()`、`Chat.tsx`、桌面 i18n 不在改动里。

本支包含 idoris 的 `probe.rs`（`5179e2a5`），三方 diff 不再改它。与 #1025 的 probe 改动仍是「自动合并能同时留下」的那一对，见 #1025。

### 结论

**需先吸收 main 再合。** 与 #1028 同一条透传约束。两支互补，互合干净；先合哪一支都可以，但都不要在未吸收 `#1015` 的树上把 `service.rs` 合进 main。

---

## `feat/me4-cos72-skeleton`：应继续暂停

尖端 `85d35a77fe4f3ea25684f619624d6da2940f94bd`，2026-08-22。相对 main：1390 后 / 2 前。merge-base `3c6c3edb`。

`gh pr list --head feat/me4-cos72-skeleton --state all` 为空。没有开过 PR。

独有提交两个：

1. `d26821cf` `feat(domain): ME-2b — agent24 os CLI over a daemon-owned registry`。**不是** main 的祖先。
2. `85d35a77` 提交说明原文：

> wip(domain): ME-4 Cos72 skeleton — PARKED at user request (stabilise architecture first)
>
> Not pushed, no PR. Kept on this branch so the work is not lost: a second DomainModule (agent24-cos72-os) plus three acceptance tests that mount BOTH real OSes and check store isolation, independent enable/disable, and per-module event attribution. Resume after the ME-1/ME-2 follow-ups land.

「Not pushed」已经过时：`origin/feat/me4-cos72-skeleton` 就在这个提交上。「no PR」仍然属实。「PARKED at user request」是提交本身写下的暂停决定。

`85d35a77` 自己的文件（9 个，+689 / −12）：`agent24-cos72-os` crate（`Cargo.toml`、`domain-os.yml`、`migrations/0001_cos72.sql`、`src/lib.rs`）、`agent24d` 的 `domain.rs` / `server.rs` / `Cargo.toml`、工作区 `Cargo.toml` / `Cargo.lock`。main 上没有 `rust/crates/agent24-cos72-os`。`docs/reference-notes/macro.md` 只在路线图里点了这个名字。

`git merge-tree --write-tree origin/main origin/feat/me4-cos72-skeleton` 冲突，不能当一次恢复：

| 冲突 | 路径 |
|---|---|
| content | `rust/Cargo.lock`、`rust/Cargo.toml`、`rust/apps/agent24-cli/src/main.rs`、`rust/apps/agent24d/Cargo.toml`、`domain.rs`、`main.rs`、`os_config.rs`、`server.rs`、`rust/crates/agent24-protocol/src/types.rs` |
| add/add | `rust/apps/agent24d/src/os_routes.rs` |
| modify/delete | `rust/crates/agent24-sin90-os/src/lib.rs`：main 已删除，分支仍修改。main 上已没有 `agent24-sin90-os` 目录 |

暂停应保持。Cos72 骨架还没进 main，但要恢复就得在当前领域 OS 上重做，而不是把 2026-08-22 的分支合进来。不要为它开 PR。

---

## `ab/kernel-27-source-classification` 与 #874

#874：<https://github.com/iDoris-ai/Agent24/pull/874>，OPEN，base `ab/kernel`，head `ab/kernel-27a-source-classification-contract`，尖端 `cf8ccd48557dc82b9a7acc86de5d788c90e02312`，GitHub `MERGEABLE` / `CLEAN`，+149 / −18，8 文件。标题是 ADR-K1-02 **P1a**（存储契约，不接线 agent 运行时）。

`ab/kernel-27-source-classification` 尖端 `6ad5e7d9564207311ea415e212061a528e12076d`，提交说明 `feat(kernel): ADR-K1-02-P1 —— 记忆来源分类与引用传播`。`gh pr list --head ab/kernel-27-source-classification --state all` 为空。**没有 PR。**

两者相对 `ab/kernel`（`9d1062cc`）都是 7 后 / 1 前。kernel 上多出来的 7 个提交包括 `#1016`（吸收 main）和 K1-7.2 审计接线。main 相对 kernel 是 289 后 / 78 前。main 上没有 `source_policy.rs`，也没有 `0018_assertion_source_ref.sql`。`git grep SourceClassification origin/main` 无匹配。两支都不在 main。

### 文件谁更完整

相对 `ab/kernel` 的三方 diff：

**#874 / 27a（8 文件）** 与 **27（12 文件）** 重叠的 8 个文件 **blob 完全相同**：

- `rust/crates/agent24-memory/migrations/0018_assertion_source_ref.sql`
- `rust/crates/agent24-memory/src/assertion.rs`
- `rust/crates/agent24-memory/src/lib.rs`
- `rust/crates/agent24-memory/src/retriever.rs`
- `rust/crates/agent24-memory/src/vector.rs`
- `rust/crates/agent24-memory/src/writer.rs`
- `rust/crates/agent24-store/src/lib.rs`
- `rust/crates/agent24-store/src/source_policy.rs`

`git diff 27a 27` 只剩 4 个文件，+99 / −7，全是 agent 接线（#874 正文里拆出去的 P1b）：

| 文件 | 27 比 27a 多出来的 |
|---|---|
| `agent24-agent/src/lib.rs` | 导出 / 接线 |
| `agent24-agent/src/retain.rs` | 写入 `source_ref` |
| `agent24-agent/src/session_memory.rs` | 召回路径把 kind 标成 `MemoryRecall` |
| `agent24-agent/src/session_memory_tests.rs` | `recall_returns_persisted_source_ref_as_memory_recall` 等 |

**存储契约 27a 与 27 一样完整。运行时接线只有 27 有。** #874 故意不带这 4 个文件。用 27 替换 #874 会把未在该 PR 里审查的 P1b 一起带上。

### 落后 kernel 的 7 个提交会不会被盖掉

kernel 自 merge-base `933d0381` 改过这 8 个文件里的 `agent24-store/src/lib.rs`（+36 / −3）：`Store` 增加 `terminal_commit_ack_gate`，`test_hooks` 增加 `pause_next_terminal_commit_ack`（K1-7.2）。27a 对同一文件只改 `source_policy` 的 `pub use`，加上 `SourceClassification`。

`merge-tree` 把 27a 合进 kernel 是干净的（树 `aa5d485d`，有一行 `Auto-merging lib.rs`）。相对 kernel 尖端，`lib.rs` 的 diff **只有** 那一行 `pub use` 增加 `SourceClassification`，`terminal_commit_ack_gate` 还在。27 合进 kernel 同样干净（树 `8d5a45af`），自动合并的也是这个 `lib.rs`。P1b 的 4 个 agent 文件没有和这 7 个提交冲突。

不要把 27 或 27a 直接合进 main。那是整条 kernel 栈（相对 main 78 个独有提交、289 个落后提交），不是这一片分类契约。

---

## 必须修掉才能合的清单

1. **#1026**：删掉仓库根 `baton-result.json` 再合。不要把 `scripts/contract-v1-hardware/run-history.sh` 接进默认 CI（对当前 openapi 它是预期 RED）。`disks.rs` / `probe.rs` 与 #1025 同 blob，HW 行为只审 #1025。
2. **#1027**：删掉仓库根 `baton-result.json` 和 `baton-decisions.tsv` 再合。`tools/qa/rustdoc-repro` 与 `docs/agent/cloud-wave*` 可以留。历史脚本保持 exit 3。HW 同样以 #1025 为准。
3. **#1028 与 #1029**：在这两支的 `service.rs` 进入 main 之前，`PASSTHROUGH_VARS` 必须同时包含 main 的 `AGENT24_DATA_DIR` 和分支的 `IDORIS_URL`（20 项），并继续保证 `IDORIS_API_KEY` 不进 plist。禁止用分支侧文件覆盖 main 的 `service.rs`。`state_dir()` 本身不用改；丢的是 launchd 透传。建议先把 main 合进 `ab/idoris`（今天干净），再合这两支并手解这一处。
4. **#1025 与 `ab/idoris` 的 `probe.rs`**：自动合并已经能同时留下 HW-1（最长挂载点、`hardware_refresh_kind`）和 IDORIS-04b（`env_clear`）。若改成人工只留一边，另一边的行为会丢。这不是现在合 #1025 的代码修改，而是以后两线相遇时的解冲突约束。

#1023、#1024、#1025 没有必须先改才能合的代码。#1023 的暂停卡片仍写「MiB」，进行中进度走 main i18n 的「MB」（同一 `formatMb`，除数是 1024²）。这是文案不一致，不把 main 的错误码或数据目录改回去；要不要在合入前把暂停句也改成 i18n，由产品决定。

`feat/me4-cos72-skeleton` 继续暂停，不要开 PR。`ab/kernel-27-source-classification` 不要拿来替换正在审的 #874；若要 P1b，另开审查。

## 声明

本报告是独立只读复核，不是审批，不替代外部 PR-Daemon 评审，也不代替 CI。GitHub 上这些 OPEN PR 的 `mergeStateStatus` 都是 `BLOCKED`（需要规定的检查与评审）。`MERGEABLE` 只表示 Git 能合，不表示可以发布。
