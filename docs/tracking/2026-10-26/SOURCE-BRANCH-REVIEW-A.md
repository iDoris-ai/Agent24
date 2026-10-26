# GROUP-A：桌面、发布、预览、本地验证类分支调查（只读，2026-10-26）

方法：`git cherry -v <集成分支> <ref>` 取最小独有数；对每条分支触及的文件做 `git diff <集成分支> <ref> -- <file>`（脚本只比较该分支自 merge-base 以来改过的文件，标 IDENTICAL 表示与 origin/main 逐字节相同）；PR 状态用 `gh pr view N --json`。
注意：本仓库 PR 全是 squash 合并，所以 `git cherry` 会把早已合入的分支报成「有独有提交」。下面的结论以文件内容对比为准，不以 cherry 为准。

## 总表

| 分支 | 本地 tip / 远端 tip | 独有数 | 任务 | 停止原因 | 分类 | 替代者或建议 |
|---|---|---|---|---|---|---|
| ab/showcase-download-01-controls | e70bd3ec / 无 | 1 | Design 组件下载暂停/继续（HTTP Range 续传） | 链条第 1 环，继续长成后面几条 | SUPERSEDED | #1023（046c5ba3） |
| jhfnetboy/ab-showcase-download-04-r1-fix | 4915b6d2 / 无 | 2 | SD-01 R1：part/meta 符号链接逃逸、完成后取消 | 同上 | SUPERSEDED | #1023 |
| jhfnetboy/ab-showcase-download-05-r2-fix | db593ea9 / 无 | 4 | SD-01 R2：FIFO 安全 open、空闲推送；MiB 单位 | 同上 | SUPERSEDED | #1023 |
| jhfnetboy/ab-showcase-download-06-timer-finally | 990eaf25 / 无 | 6 | SD-01 R3：idlePushTimer 延后创建 | PR #967 已合入 ab/showcase-download | SUPERSEDED | #967 -> #1023 |
| jhfnetboy/ab-showcase-download-18-external-f1 | 785b5dc6 / 无 | 10 | SD-01 F1：writeDownloadMeta 放进 installOnce try、短写处理 | 外部复核修完 | SUPERSEDED | #1023；open-design-component.ts 与 main 逐字节相同 |
| jhfnetboy/ab-showcase-download-21-main-assembly | d843cf78 / 无 | 1 | SD-01 装配到 main 的快照 | 被 22 取代 | SUPERSEDED | #1023 |
| jhfnetboy/ab-showcase-download-22-composite-review | 0fd0a11d / 无 | 2 | 复核 C1/C2 文案回归修复（locale-only） | 修复已并入 #1023 的 F1 i18n | SUPERSEDED | #1023（Creative.i18n.ts 已含 pause/paused/MiB 三语） |
| jhfnetboy/ab-preview-hw-24-v1-contract | 607112c3 / 无 | 1 | decide/profile 恢复 v1 必需字段 free_disk_bytes | 本地私有暂存，commit 自述「不开 PR」 | SUPERSEDED | #1107（2a149cbe）；legacy_disk.rs、lib.rs 与 main 相同 |
| local/accidental-validation-staging-20261005 | 8e2812f6 / 无 | 2 | Hyphae 口令解锁流程 + lock 二进制 | 联调验证用的暂存 | SUPERSEDED | #676（fc862cf3） |
| local/agent24-joint-validation-20261005 | ac4917cb / 无 | 3 | 同上 + COMM-5b 零运行门禁 | 同上，是最早一版 | SUPERSEDED | #676 |
| local/agent24-joint-validation2-20261005 | ff6e7efa / 无 | 3 | 同上，较完整版 | 同上 | SUPERSEDED | #676（router.rs、unlock_real_binary.rs、comm_zero_run.rs 与 main 相同） |
| local/comm-round2 | 851a8576 / 无 | 6 | COMM-3 send/history、COMM-4a daemon 监管、PASSWORD_STORE 开关 | 本地 rebase 副本，PR 已走完 | SUPERSEDED | #626/#627/#628/#630 及后续 #633/#642/#645 |
| local/comm-round2-pre | c1e6ed33 / 无 | 9 | 上一条再加 COMM-2a 身份路由、#622 收口 | 同上 | SUPERSEDED | #622/#626/#627/#628/#630 |
| local/preview-v040 | 7b9f3b40 / 无 | 6 | v0.4.0 预览的本地合并分支（A3 竞态、FU-83、SDK 宽松解析、模型可见性） | 各部分已走自己的 PR | SUPERSEDED | #543、#544、#550 |
| pr601 | 67ddbce3 / 无 | 2 | Hyphae CLI 接线交接文档 | PR #601 已合 | SUPERSEDED | #601（92f844ee）；该文件内容与分支 tip 相同，之后又被 #634 更新 |
| pr671 | 90383d5d / 无 | 3 | 未签名 macOS dmg + mac 按需组件 tarball | #671 已关，它是 feat/desktop-macos-dmg 的前 3 个提交 | SUPERSEDED（被下一行包含） | 见 feat/desktop-macos-dmg |
| pr673 | ffd08b1c / 无 | 1 | M1-T11 桌面记忆页 | PR #673 已合入 ab/m1-memory | SUPERSEDED | #673（0b27c259），后到 main |
| t11 | 5f8cd1f4 / 无 | 2 | 记忆页 + 评审修复 H4/M3/M4 | 同上；worktree 干净 | SUPERSEDED | #673；Memory.tsx、Memory.test.tsx 与 0b27c259 逐字节相同 |
| ab/m1-memory-15-memory-page | c1f23b11 / 无（远端已 gone） | 2 | 同上 | 同上 | SUPERSEDED | #673；同上 |
| feat/desktop-macos-dmg | b585eba1 / b585eba1 | 8 | 未签名 macOS dmg（arm64+x64）CI + mac Open Design 组件 tarball + 真机闭环冒烟 | #671 被 jason 于 10-16 关闭，称已有签名公证包 | UNSURE | 见下，需要 jason 判断 mac CI 是否还要 |
| feat/me4-cos72-skeleton | 85d35a77 / 85d35a77 | 2 | ME-2b `agent24 os` CLI + Cos72 进程内骨架 | commit 自述「PARKED at user request」，未推送未开 PR 的 WIP | UNSURE | ME-2b 已在 #134；Cos72 骨架 main 没有，PLAN 里挂在 T10 |
| fix/1026-checker-tests | 94ccd137 / 无 | 3 | v1 硬件 schema 兼容 checker | PR #1026 已合 | SUPERSEDED | #1026，8 个触及文件全部与 main 相同 |
| fix/1030-release-notes | 10db5da9 / 无 | 2 | K1 发布说明 | PR #1030 已合 | SUPERSEDED | #1030（92f9f0cf），main 版本更新（#1031 之后又改过） |
| fix/consent-draft-consent-record | f194fc19 / f194fc19 | 1 | W10：草稿例外核验同意记录属性 | Grok 额度耗尽留下的 WIP | SUPERSEDED | #1108（53032dac）；迁移 0018 与 main 相同 |
| origin/ab/apple-preview-cloud-20261015-104002-rustdoc | 无 / c57e17e9 | 2 | rustdoc E0463 最小复现工装 | 后续合并到 #1027 | SUPERSEDED | #1027（5c7d8759） |
| origin/ab/apple-preview-cloud-20261015-w38-rustdoc-safe-monitor | 无 / aa81db1e | 5 | 上一条 + 安全只读 artifact 观察器 | 同上 | SUPERSEDED | #1027；工装已在 main 且更新（多出 test_r3_hardening.py） |
| origin/review/convergence-prs-20261016 | 无 / d0410cbe | 1 | 对 #1023–#1029 的时点独立复核报告 | 被复核的 7 个 PR 全部已合 | OBSOLETE | 无需保留；若想存档，报告是单文件 |
| origin/repro/1020-orphan-module | 无 / 466ed560 | 1 | #1020 复现报告（kill -9 后 documents 孤儿占 blobs 锁） | 报告写完，修复另走 #1105 | VALUABLE | 把 docs/agent/REPRO-1020-20261016.md 开 docs-only PR 到 main |

## VALUABLE / UNSURE 要点（重复放在这里）

- VALUABLE：origin/repro/1020-orphan-module。#1105 的 PR 正文明确引用 `docs/agent/REPRO-1020-20261016.md` 和这个分支名作为根因证据，但该文件不在 main（`git grep REPRO-1020 origin/main` 无命中）。只有 1 个提交、1 个新增文件，无冲突。建议 cherry-pick 466ed560 开 docs-only PR 到 main，之后可删分支。
- UNSURE：feat/desktop-macos-dmg（本地、远端同 tip，worktree agent-a97bbf8bf40a84877 干净）。唯一没落地的是 mac CI 流水线：release-desktop.yml 的 macOS job、`ci-on-demand-smoke.mjs` 冒烟、mac 架构自检、`pack-open-design-component.mjs` 的 mac 分支。卡在：main 的 release-desktop.yml 目前只有 Linux job，而 jason 关闭评论说「已有签名公证预览包，未签名路径被取代，如需 CI 请基于已签名流程另开」。要问 jason：是否还要在 CI 里出 mac 包？不要就归档，要就把这几个脚本拆出来在签名流程上重开。
- UNSURE：feat/me4-cos72-skeleton（本地、远端同 tip）。ME-2b 那个提交的内容已由 #134 落地（os_routes.rs 与 main 相同），所以真正独有的只有 Cos72 骨架（新 crate agent24-cos72-os，in-process）。main 的 PLAN-OOP-OS-AND-BACKLOG.md 把它列为 T10 的重做起点，写明「现在不合」。卡在：T10 是否还在计划里，以及骨架是否还要作为参考保留。

## 两个 worktree

- t11：/Users/jason/Dev/auraai/Agent24/.claude/worktrees/agent-aba391bcc8e277b20，HEAD 5f8cd1f4，`git status --short` 为空，无未跟踪文件。上游 origin/ab/m1-memory-15-memory-page 已 gone。
- feat/desktop-macos-dmg：/Users/jason/Dev/auraai/Agent24/.claude/worktrees/agent-a97bbf8bf40a84877，HEAD b585eba1，status 为空，无未跟踪文件，与 origin 同步。
- 两处 `git stash list` 显示的三条 stash 是整个仓库共享的，不属于这两个 worktree。

---

## 逐条证据

### SD 链：showcase-download-01 / 04 / 05 / 06 / 18 / 21 / 22
这是一条线性链：01 的提交 e70bd3ec 是 04、05、06、18 的共同祖先（`git cherry -v origin/main <ref>` 列出的前几条完全一致）。21、22 是另一个 base（6072fd37）上的重新装配。
- 06 对应 PR #967，`prs.tsv`：`jhfnetboy/ab-showcase-download-06-timer-finally 967 MERGED ... ab/showcase-download`。之后 `ab/showcase-download` 以 #1023 合入 main：`ab/showcase-download 1023 MERGED ... main`，squash 提交 046c5ba3「fix(creative): SD-01 —— 安全暂停续传与真实下载速度状态发布到 main (#1023)」。
- 对比：18 tip 触及的 10 个文件里，`open-design-component.ts`、`open-design-component.test.ts`、`independent-review.test.ts`、`r2-independent-review.test.ts`、`main.ts`、`preload.ts`、`ipc-types.ts`、PLAN 文档全部 IDENTICAL（脚本输出）。只有 `Creative.tsx`（+24/−38）与 `Creative.test.tsx` 不同，那是 #1023 合并时按 main 的 i18n 重写的（#1023 提交信息：「解冲突时把 download.size / download.prompt 改用 main 的 i18n 键」）。
- 22 的 C1/C2（文案 i18n、MB 改 MiB、th 英文兜底）：`git grep "action.pause" origin/main -- apps/desktop/src/renderer` 命中 Creative.i18n.ts 的 en/zh/th 三处，Creative.tsx:80 使用 `t('action.pause')`；`download.size` 在 main 为 `{received} / {total} MiB`。键名与 22 的不完全一样（main 用 `download.paused`，22 用 `download.paused.text`），但功能覆盖且 main 的 th 是真翻译。
- 复现：`bash d.sh jhfnetboy/ab-showcase-download-18-external-f1`（即对每个文件跑 `git diff --numstat origin/main <ref> -- <file>`）。

### jhfnetboy/ab-preview-hw-24-v1-contract
- 提交信息自述：「本轮仅本地私有暂存提交，不开 PR、不指定远程集成分支」。
- 替代：#1107 `feat/v1-compat-restore-free-disk-bytes 1107 MERGED 2a149cbe`，提交「feat(hw): 恢复 v1 兼容字段 free_disk_bytes（#1026 checker 由 RED 转 GREEN）」，同样的 5 类文件（多了 openapi.d.ts）。
- 对比：`git diff --numstat 607112c3 origin/main -- rust/crates/agent24-hw/src/legacy_disk.rs rust/crates/agent24-hw/src/lib.rs` 输出空（相同）；`decide_routes.rs` 仅 11/4 行差异（#1107 的 15 行后续打磨）。main 里 `HardwareProfileV1`（decide_routes.rs:27）和 `agent24_hw::legacy_cwd_free_bytes()`（:68）都存在。

### local/accidental-validation-staging-20261005、local/agent24-joint-validation-20261005、local/agent24-joint-validation2-20261005
- 都是 2026-10-05 联调暂存，三个版本逐步演进（2 / 3 / 3 个提交，最后一条都叫「feat(comm): add password unlock flow」）。无关联 PR。
- 替代：#676 `codex/agent24-hyphae-joint-20261005`，squash 为 fc862cf3「feat(comm): close Agent24 × Hyphae joint-debug gates (#676)」，`git log origin/main --grep='password unlock flow'` 和 `--grep='COMM-5b zero-run'` 都命中它。
- 对比（joint-validation2）：password.rs、router.rs、unlock_real_binary.rs、comm_zero_run.rs、comm_dependency_allowlist.rs、real_binary.rs 等 IDENTICAL；有差异的 main.rs（+28/−438）、Cargo.lock、hyphae.lock.json（3/3）是 main 之后继续演进的（现已锁到 Hyphae v0.26.3）。
- 分支名 accidental 说明那条是误起的暂存分支，同样没有保留价值。

### local/comm-round2、local/comm-round2-pre
- 内容是 #622/#626/#627/#628/#630 的 rebase 前后副本。pre 的 48c866ac 就是 #622 的 head（`622 MERGED 48c866ac`）。`prs.tsv` 中 #626 head c6d6688a、#627 6235eb8d、#628 b45ba08a 都是 MERGED。
- main 的对应提交：`git log origin/main --grep='COMM-4a'` 得 d5e8e513（#626）、a930f9ce（#642）、ec2773e4（#645）、9ed35496（#633）；COMM-3 得 af586214（#627）；PASSWORD_STORE 得 be365232（#628）；JOINT-ROUND2 文档 5ff5ea38（#630）。
- 对比：round2 tip 的 JOINT-ROUND2.md、restart_policy.rs、send_history_e2e.rs、comm_mount_smoke.rs IDENTICAL；daemon.rs main 比分支多约 2200 行（评审 3 轮后的版本），显示 main 是后继而非回退。

### local/preview-v040
- v0.4.0 预览用的本地合并分支（merge 了 feat/desktop-logo、fix/pre-release-a3-race 等）。
- 对比：分支的 3 条 fix 提交（4d761b7e、542681f0、c15180ae）合起来的变更行集与 #543（a53e8c12）的 rust 部分完全一致：215 行变更，215 行相同（`comm -12`）。#544（32072b02）覆盖另外 3 条，仅迁移文件被合并成 `0013_model_call_timings.sql`（分支里是 0009/0010）。v0.4.0 已打 tag（`git tag --list '*v0.4*'` 得 v0.4.0）。

### pr601、pr673、t11、ab/m1-memory-15-memory-page
- pr601：`docs/hyphae-cli-handoff 601 MERGED 67ddbce3`，与分支 tip 同一 sha。合并提交 92f844ee；`git diff --stat 92f844ee pr601 -- docs/design/HYPHAE-CLI-INTEGRATION.md` 为空；main 上该文件之后被 #634 改过。
- pr673 / t11 / m1-memory-15：#673 `ab/m1-memory-15-memory-page 673 MERGED a2c35121 -> ab/m1-memory`，合并提交 0b27c259。t11 和 m1-memory-15 的 Memory.tsx、Memory.test.tsx、api.ts、App.tsx 与 0b27c259 逐字节相同（`git diff --numstat 0b27c259 t11 -- <file>` 全空）；pr673 是只有第一个提交的旧版（Memory.tsx 差 7/35）。main 上 Memory.tsx 之后被 #950（i18n）改过。

### feat/desktop-macos-dmg 与 pr671（UNSURE，见上）
- #671：`CLOSED 2026-10-16T08:24:07Z`。关闭评论原文：「按用户 2026-10-16 决定关闭：本 PR 是未签名的 macOS dmg 构建……main 上已有带 Developer ID 签名 + 公证 + staple 的 Agent24 预览包（构建号 20261016.1）……未签名 dmg 路径已被取代。如需引入 macOS dmg 的 CI 流水线，请基于已签名流程另开新 PR。」
- 已落地的部分：分支里 creative-serve-web.ts / .test.ts 的 resourceRoot/resourceSafeBase 修复与 main 相同（`git diff --numstat origin/main b585eba1 -- apps/desktop/src/main/creative-serve-web.ts ...` 无输出）；仓库根 `.npmrc` 在 main 已删除（#670）。
- 没落地的部分：`git ls-tree origin/main apps/desktop/scripts/ | grep smoke` 为空（ci-on-demand-smoke.mjs 不在 main）；`git show origin/main:.github/workflows/release-desktop.yml` 只有 `Build + smoke test (linux x64)`；`pack-open-design-component.mjs` 与 main 差 20/2 行（mac 分支逻辑）。
- pr671 tip 90383d5d 是 feat/desktop-macos-dmg 的祖先（`git log --oneline origin/main..pr671` 的 5 个提交都在 dmg 分支里），无独立价值。

### feat/me4-cos72-skeleton（UNSURE，见上）
- d26821cf（ME-2b）：main 上是 `c5b3cb3c feat(domain): ME-2b — agent24 os CLI over a daemon-owned registry (#134)`，os_routes.rs 与分支相同。
- 85d35a77：「wip(domain): ME-4 Cos72 skeleton — PARKED at user request (stabilise architecture first). Not pushed, no PR.」新增 `rust/crates/agent24-cos72-os/`（Cargo.toml、domain-os.yml、migrations/0001_cos72.sql、src/lib.rs 342 行）。`git ls-tree --name-only origin/main rust/crates/ | grep cos72` 为空。
- main 的 docs/agent/PLAN-OOP-OS-AND-BACKLOG.md：第 62 行「main 里没有 cos72 crate，是真未合的在途工作」；第 73 行「3f 之后重做成进程外样例；现在不合」；第 218 行 T10「Cos72 进程外样例（feat/me4-cos72-skeleton 重做）」。

### fix/1026-checker-tests、fix/1030-release-notes
- 1026：`ab/apple-preview-cloud-20261015-104002-v1-guard 1026 MERGED 94ccd137`，和分支 tip 同 sha。8 个触及文件全部 IDENTICAL。
- 1030：`docs/k1-release-notes-20261016 1030 MERGED`（92f9f0cf）。分支 tip 10db5da9 是评审改写的中间稿；main 的同一文件还被 5e633270（#1031 之后）更新，差异就是「待 jason 决定」改成「已选路径 1，由 #1031 执行」（`git diff origin/main fix/1030-release-notes` 的 9/9 行）。

### fix/consent-draft-consent-record
- f194fc19 提交信息：「WIP……本机 Grok 车道在额度耗尽（HTTP 402）前留下……未完成」，任务为 #1031 评审 L2。
- 替代：#1108 `fix/consent-draft-requires-consented-attribute`，合并提交 53032dac，主提交 6e03c7ac「草稿例外必须核验同意记录里的属性，并收窄到第一方 Documenting（#1031 L2）」。
- 对比：迁移 0018_module_consent_reversible_draft.sql 与 main 相同；`git diff --numstat 6e03c7ac f194fc19 -- module_consents.rs` 空，consent_gate.rs 分支版比 6e03c7ac 少 71 行（WIP 未完成）。

### origin/ab/apple-preview-cloud-20261015-104002-rustdoc 与 ...-w38-rustdoc-safe-monitor
- 前者是后者的前 2 个提交。均为 baton 云任务产物（rustdoc E0463 复现工装 + 安全观察器）。
- 替代：#1027 `ab/apple-preview-cloud-20261015-w40-rustdoc-safe-monitor-r3 1027 MERGED`，`5c7d8759 test(qa): rustdoc E0463 最小复现工装与证据（收敛遗漏分支） (#1027)`。
- 对比：w38 tip 的 cloud-wave37 / wave38 文档、make-fixture.sh、run-experiments.sh、test_safety.py 与 main IDENTICAL；artifact_observer.py 与 README 不同是因为 main 是 w40-r3 的加固版（main 另有 tests/test_r3_hardening.py）。baton-result.json、baton-decisions.tsv 是任务回执，不需要保留。

### origin/review/convergence-prs-20261016（OBSOLETE）
- 单文件 docs/agent/REVIEW-convergence-prs-20261016.md（465 行），明确写「本报告不是审批」，是 2026-10-16 15:17 的时点快照，评审对象 #1023–#1029。
- 现状：#1023、#1024、#1025、#1026、#1027 合入 main，#1028、#1029 合入 ab/idoris（prs.tsv 均为 MERGED）。报告的结论（先删根回执、先吸收 main）已被各 PR 消化。main 上无该文件（`git grep REVIEW-convergence origin/main` 无命中）。

### origin/repro/1020-orphan-module（VALUABLE）
- 单文件 docs/agent/REPRO-1020-20261016.md（316 行）：复现 kill -9 后 documents 孤儿进程占 blobs/.lock，定位到死亡钩子在写日志时因 stderr 断管 panic，`exit(70)` 被跳过。
- #1105（53951f57，修复 `1004f8e2 fix(os-sdk): 死亡钩子不再被坏 stderr 的日志 panic 拦掉`）的正文写「复现报告（docs/agent/REPRO-1020-20261016.md，分支 repro/1020-orphan-module）」。
- `git grep -n REPRO-1020 origin/main` 无输出，即这份引用在 main 上是悬空的。`git diff --numstat origin/main origin/repro/1020-orphan-module -- docs/agent/REPRO-1020-20261016.md` 为 316/0（纯新增）。
