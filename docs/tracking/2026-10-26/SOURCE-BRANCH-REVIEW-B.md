# GROUP-B 调查报告（baton / 备份 / weknora / kernel / idoris / comm / decide）

调查日期 2026-10-26。只读，没有删、没有 checkout、没有 fetch。main 指 origin/main@3a433bb0。

"独有提交"口径：`git log <ref> --not origin/main origin/ab/{memory-p1,kernel,weknora,documenting,office-assistant}`。括号里 `非merge` 指去掉 merge commit 后的数。tier2-uniq.txt 里的数字用的是 git cherry 取最小值，对 baton 分支（建在 K1 release 之前的旧树上）会虚高，比如 consent-fix 写 48，实际独有只有 3 个非 merge 提交。

判据验证：先拿已知样本验证。`git cherry -v origin/ab/decide origin/baton/...d1-2b` 把已知被 #1033 squash 的 feat 提交标成 `-`（等价），把只属于 baton 的回执标成 `+`，判据可用。"文件逐字节相同"用 `git diff --quiet <分支tip> <集成分支> -- <文件>`，对已知不同的文件（`idoris.rs`，13 行不同）确实返回非 0，所以不是恒真。

## 总表

| 分支 | 本地/远端 tip | 独有提交 | 任务一句话 | 停止原因 | 归类 | 替代者或建议 |
|---|---|---|---|---|---|---|
| ab/decide（本地） | 37f9fde8 / 远端 1ea40c62 | 10（均在远端内） | D1-1 ONNX spike、D0-8、三语评测等的旧快照 | 本地没推进，远端往前走了 | SUPERSEDED | 本地 tip 是 origin/ab/decide 的祖先，落后 9 个提交 |
| ab/idoris-09-gateway-review（本地） | eb2c2eb9 / 无远端 | 1 个 merge，非 merge 0 | 本地评审用合并：把 `refs/idoris-validation/ab-5287`（#984 merge）并进 gateway 分支 | 评审完成，无 PR | SUPERSEDED | 内容全在 origin/ab/idoris（01c7c6f2 是其祖先，之后又有 e629d7fb、0780c4ca 改了 idoris.rs） |
| ab/idoris-13-cla-allow-baton（本地） | 028a00ef / 远端 028a00ef，一致 | 7（只有 028a00ef 是自己的） | 往 cla.yml allowlist 加 `baton*` | #1067 被负责人主动关闭：决定是改写 bot merge commit 作者，不放宽 CLA | OBSOLETE | 决定相反，不要恢复 |
| baton/grokbot-20261016-a24-consent-fix | 远端 966af090 | 3 非 merge（1 个 fix + 2 个回执） | K1 生命周期 consent 撤销失败改为 fail-closed | baton 完成回执后无人开 PR；修复随 K1 release #1022 进了 main | SUPERSEDED | main 的 os_routes.rs、cli/main.rs 与 238a18aa 逐字节相同 |
| baton/grokbot-20261016-a24-k1review | 远端 3008c5aa | 3 非 merge（1 份报告 + 2 个回执） | 对 K1→main release 合并树的独立只读复核报告 | 一次性复核，报告交 PR-Daemon，无 PR | OBSOLETE | 报告里的主要发现已被 consent-fix 修掉。报告文档 `docs/agent/REVIEW-kernel-to-main-20261016.md` 不在 main，若要留档请先存档 |
| baton/grokbot-20261016-a24-d1-2b | 远端 33c212ed | 18 非 merge（其中 ab/decide 已有的 15 个，自己的 3 个） | D1-2b L2 答案校验实现 | baton 完成回执；同一补丁由 #1033 合入 ab/decide | SUPERSEDED | #1033 / ab/decide@1ea40c62（cherry 标 `-`，涉及的 5 个文件与 ab/decide 逐字节相同） |
| baton/grokbot-20261016-a24-d1-2c | 远端 6cebbfb4 | 18 非 merge（自己的 3 个） | D1-2c L3 layers[] 契约提案文档 | 同上 | SUPERSEDED | #1035 / ab/decide@9b6f6866（文件逐字节相同） |
| baton/grokbot-20261016-a24-idoris-sync | 远端 022ba6c0 | 5 非 merge（只有 2 个回执是自己的） | 把 main 吸收进 ab/idoris-04-receipts 与 -11-pair-acceptance，PASSTHROUGH_VARS 变 20 项 | 工作推到了另外两个分支，这个分支只剩回执 | OBSOLETE | 实际内容在 #1028、#1029（已合 ab/idoris） |
| baton/grokbot-20261016-a24-showcase-i18n | 远端 b7b1d702 | 5 非 merge | 创作页暂停/续传/速率/ETA 的 i18n，统一 MiB | baton 完成；同内容经 ab/showcase-download #1023 进了 main | SUPERSEDED | main 的 Creative.i18n.ts、Creative.tsx、Creative.test.tsx 与分支 tip 逐字节相同（#1023 / 046c5ba3） |
| baton/grokbot-a24-20261015-104002-linux-hw-r2 | 远端 48d4ce04 | 4 非 merge（HW-1 已在 main，另 3 个独有） | 为 agent24-hw 补 Linux 运行时正向对照测试，生产代码字节不变 | baton 只推送、不开 PR，之后没人接 | VALUABLE（低） | 开 PR 到 main；`git merge-tree` 对 main 无冲突。9 个新增测试函数在 main 里均为 0 |
| baton/grokbot-a24-20261015-104002-wal-race-r2 | 远端 333382a4 | 7 非 merge（HW-1 已在 main，另 6 个独有） | 修 documents 测试里 WAL 复制的 TOCTOU 竞态，改用 `VACUUM INTO` | 同上 | VALUABLE（低，修 flaky 测试） | 开 PR 到 main（或 ab/documenting）；main 的 tests.rs:168 仍是竞态写法，合并无冲突 |
| backup/ab-idoris-04-receipts-pre-cla-20261019 | 远端 27b1a66b | 9 非 merge | #1028 在重写 bot 作者之前的快照 | 备份，已完成使命 | OBSOLETE | `git cherry origin/ab/idoris` 为空；树与 #1028 的 PR head 3042dfb2 相同，#1028 已合入 ab/idoris |
| backup/ab-idoris-11-pair-acceptance-pre-cla-20261019 | 远端 6eb147ab | 8 非 merge | #1029 同上 | 同上 | OBSOLETE | cherry 为空；树与 #1029 的 head 7bd54ce3 相同 |
| ab/weknora-03a-state-machine | 远端 9e7d01c5 | 6 非 merge | KB-CT-03① 状态机契约完整版（539 行） | #1070–#1072 于 10-20 被负责人关闭，改用 #1113 的压缩核心。分支被明确要求保留 | VALUABLE（保留，勿删） | ab/weknora 的 CONTRACT-STATE-v1.md §11 点名引用此分支作"非规范材料" |
| ab/weknora-03a-state-machine-1 | 远端 c6a54ec2 | 2 | 同上第一片 | 同上 | SUPERSEDED | 是 03a-state-machine 的祖先 |
| ab/weknora-03a-state-machine-2 | 远端 fc488efd | 4 | 同上前两片 | 同上 | SUPERSEDED | 是 03a-state-machine 的祖先 |
| ab/weknora-03b-ui-bridge | 远端 56839fcc | 4 非 merge | KB-CT-03② UI 桥契约完整版（409 行） | #1076、#1077 关闭，改用 #1115。分支保留 | VALUABLE（保留，勿删） | CONTRACT-UI-BRIDGE-v1.md §10 点名引用此分支 |
| ab/weknora-03b-ui-bridge-1 | 远端 05bbcb8b | 2 | 同上第一片 | 同上 | SUPERSEDED | 是 03b-ui-bridge 的祖先 |
| ab/weknora-03c-ops | 远端 0f18a044 | 12 非 merge | KB-CT-03③ OPS 写操作语义契约完整版（549 行） | #1073–#1075 关闭，改用 #1114。分支保留 | VALUABLE（保留，勿删） | CONTRACT-OPS-v1.md §11 点名引用此分支 |
| ab/weknora-03c-ops-1 | 远端 acee7b52 | 8 | 同上第一片 | 同上 | SUPERSEDED | 是 03c-ops 的祖先 |
| ab/weknora-03c-ops-2 | 远端 819f70d9 | 10 | 同上前两片 | 同上 | SUPERSEDED | 是 03c-ops 的祖先 |
| ab/kernel-22-egress-exec-memory | 远端 042451f3 | 1 | K1-6b.4 Exec 与会话来源 taint 出口门 | #793 的代码被 David 明确"不会合入"，拆成五片重做；注明"分支保留不删除" | SUPERSEDED | #1180、#1199、#1200、#1203、#1204 均已合入 ab/kernel |
| ab/kernel-27-source-classification | 远端 6ad5e7d9 | 1 | ADR-K1-02-P1：记忆来源引用持久化与传播 | 无独立 PR；前半（memory/store）以 cf8ccd48/#874 合入 ab/kernel，后半被 6b.4b 重做 | SUPERSEDED | #874（cf8ccd48）+ #1199 + #1200 |
| codex/comm5b-zero-run-20261005 | 远端 2aafcfda | 1 | COMM-5b 零运行回归门禁测试（4 个文件） | #830 的前身；10-09 为"换场排空"关闭 | SUPERSEDED | 四个文件与 origin/main 逐字节相同（经 #676 进 main），之后 #857 在 ab/comm 上加固 |
| origin/ab/comm | ca86c0b5（无本地） | 7 非 merge | COMM 线集成分支 | 暂停（最后合并 10-21 的 #857），无 release PR | 线暂停，有未发布价值 | 见"集成分支"一节 |
| origin/ab/decide | 1ea40c62（本地 37f9fde8 落后） | 17（含 merge），非 merge 约 10 | 决策模型 D0/D1 线 | 暂停（最后合并 10-19 #1035），D1-3 起未做 | 线暂停 | 见"集成分支"一节 |
| origin/ab/idoris | fa517176（无本地） | 36（含 merge），非 merge 18 | iDoris 网关接线（IDORIS-01 至 -12） | 暂停（最后 10-20），IDORIS-07 发布证据包未做 | 线暂停 | 见"集成分支"一节 |
| origin/ab/maint | 8aaf474a（无本地） | 1 非 merge | 维护线，只有 MAINT-01 一个修复 | 修复已合入 ab/maint，但没到 main | VALUABLE（小） | 把 30bf89b8 开 PR 到 main，无冲突 |

---

## 本地与远端同名分支

- `ab/decide`：本地 37f9fde8，远端 1ea40c62。`git merge-base --is-ancestor refs/heads/ab/decide origin/ab/decide` 为真；`rev-list` 显示远端比本地多 9 个，本地比远端多 0 个。本地只是旧快照。
- `ab/idoris-13-cla-allow-baton`：本地与远端同为 028a00ef，无差别。
- `ab/idoris-09-gateway-review`：仅本地，无远端。

---

## 各分支证据

### ab/decide（本地）
```
$ git merge-base --is-ancestor refs/heads/ab/decide origin/ab/decide && echo ancestor
ancestor
$ git rev-list --count refs/heads/ab/decide..origin/ab/decide ; git rev-list --count origin/ab/decide..refs/heads/ab/decide
9
0
```
归类 SUPERSEDED。删本地分支不丢任何东西。

### ab/idoris-09-gateway-review（本地）
```
$ git log --no-merges <ref> --not origin/ab/idoris | wc -l     -> 0
$ git merge-base --is-ancestor 01c7c6f2 origin/ab/idoris       -> 真（IDORIS-02-04 网关）
$ git show -s --format='%h %P' refs/heads/ab/idoris-09-gateway-review
eb2c2eb9 30308d6f f315630f      # 第二父 f315630f = refs/idoris-validation/ab-5287 = #984 的 merge
```
唯一的独有提交 eb2c2eb9 是 merge。它相对 merge-base 只改了 7 个文件（idoris.rs、tests/idoris_gateway.rs 等）。逐文件对比 origin/ab/idoris，只有 `idoris.rs`（13 行）、`idoris_gateway.rs`（9 行）、`router.rs`、`service.rs`（各 1 行）有出入，且都是 ab/idoris 后来的改进（`correlation_trace_id`、`request_id` 参数，来自 e629d7fb、0780c4ca）。
归类 SUPERSEDED（本地评审用的一次性合并）。注意本地有 `refs/idoris-validation/{ab-5287,gateway-5287}` 两个自定义 ref，删分支时它们不会被带走。

### ab/idoris-13-cla-allow-baton（本地与远端）
```
$ git cherry -v origin/ab/idoris origin/ab/idoris-13-cla-allow-baton
+ 028a00ef ci(cla): 允许 Baton GrokBot 机器人身份     # 其余 6 个提交都是 ab/idoris 的祖先
$ git grep -n -i baton origin/main -- .github/workflows/cla.yml      # 无输出
$ gh pr view 1067 --json state,closedAt  ->  CLOSED 2026-10-20T16:25:00Z
```
#1067 的关闭评论："用户已明确：Baton GrokBot 没有、也不需要 CLA 身份。#1028 / #1029 的处理是重写那一个 bot merge commit 的 author/committer 为 jhfnetboy（树不变）……放宽 CLA 会把「机器人可以当提交作者」写成长期例外"。另有 PR-Daemon 的 REQUEST_CHANGES（F1：两个 allowlist 条目也匹配不到 Baton 的提交）。
归类 OBSOLETE。是被否决的方案，不是未完成。

### baton/grokbot-20261016-a24-consent-fix
```
$ git log --no-merges --oneline origin/baton/...consent-fix --not <6 个集成分支>
966af090 baton: completed receipt + decisions (consent-fix)
238a18aa fix(k1): fail closed when lifecycle consent revocation fails
ec9bf389 baton: started receipt (consent-fix)
$ git diff --stat 238a18aa origin/main -- rust/apps/agent24d/src/os_routes.rs rust/apps/agent24-cli/src/main.rs
(无输出，逐字节相同)
$ git log origin/main --oneline -S'consent_revocation_failed' -- rust/apps/agent24d/src/os_routes.rs
56264ef9 release(kernel): K1 —— ab/kernel 发布到 main（迁移 0015–0017 + consent/工具调用/审计） (#1022)
```
`consent_revocation_failed` 在 main 的 os_routes.rs 里出现 8 次，cli/main.rs 里 4 次。`module_consents.rs` 在 main 里有后续改动（`reversible draft` 等），所以不相同，但 238a18aa 改的那一处早已在 main。后来 ab/kernel 还有更多改进（#1157：启用纪元接线，stop 的 500 带停止结果）。
归类 SUPERSEDED。

### baton/grokbot-20261016-a24-k1review
```
$ git show --stat e24b2c84    -> docs/agent/REVIEW-kernel-to-main-20261016.md | 194 +
$ git ls-tree -r --name-only origin/main | grep REVIEW-kernel-to-main     # 无输出
```
报告结论：问 1 生命周期 consent 撤销"目前潜伏的 fail-open"，违反 ADR-K1-03 §3；问 2 迁移 0015–0017 逐 blob 一致；问 3 PASSTHROUGH_VARS 等均保留，并列出"合前清单 3 项"。第一项已由 consent-fix 修掉（见上）。baton-result.json 写明"本报告不是审批"。
归类 OBSOLETE：一次性复核，结论已被吸收。报告文本不在 main，若 jason 想留证据，只需要这一个文件，可单独 `git show e24b2c84:docs/agent/REVIEW-kernel-to-main-20261016.md` 存档后再删分支。

### baton/grokbot-20261016-a24-d1-2b、-d1-2c
```
$ git cherry -v origin/ab/decide origin/baton/grokbot-20261016-a24-d1-2b
+ 8bb00d20 baton: started D1-2b L2 validation (grokbot lane)
- b0e579a5 feat(decide): D1-2b L2 answer validation (InvalidRequest, Outcome::InvalidResponse)
+ 33c212ed baton: D1-2b completed — decisions + receipt (grokbot lane)
$ git cherry -v origin/ab/decide origin/baton/grokbot-20261016-a24-d1-2c
+ 587f86af baton: started D1-2c L3 layers contract (grokbot lane)
- 9322a9ad docs(decide): D1-2c —— 提出 L3 逐层记录与日志 layers[] 契约（提案，待外审）
+ 6cebbfb4 baton: D1-2c completed — decisions + receipt (grokbot lane)
$ git diff --stat b0e579a5 origin/ab/decide -- <b0e579a5 的 5 个文件>      # 空
$ git diff --stat 9322a9ad origin/ab/decide -- docs/agent/PLAN-DECIDE-D1-2C-L3-LAYERS.md   # 空
```
`-` 表示补丁等价，对应 ab/decide 里的 1ea40c62（#1033）与 9b6f6866（#1035）。range-diff 显示 b0e579a5 与 1ea40c62 只差作者元数据。分支只多回执。
归类 SUPERSEDED。注意：这些 D1-2b/2c 实现只在 ab/decide 里，**还没进 main**（见集成分支一节）。

### baton/grokbot-20261016-a24-idoris-sync
```
$ git log --no-merges --oneline <ref> --not <6 个集成分支>
022ba6c0 baton: complete a24-idoris-sync-20261016
5d2bcc25 baton: start a24-idoris-sync-20261016 (PASSTHROUGH_VARS main absorb)
dd10562e / 3a006c97 / 3016aa6a     # 这三个是 ab/idoris 里已有的 IDORIS-01/02a/04b
$ git cherry -v origin/ab/idoris <ref>    -> 只有上面两个回执为 +
```
回执 JSON 写明实际工作落在 ab/idoris-04-receipts 与 ab/idoris-11-pair-acceptance 两个分支（"Pushed both heads only"），这两个分支后来合成 #1028、#1029。`IDORIS_URL` 在 ab/idoris 的 service.rs:130 的 PASSTHROUGH_VARS 里。
归类 OBSOLETE。

### baton/grokbot-20261016-a24-showcase-i18n
```
$ git diff --quiet origin/baton/...showcase-i18n origin/main -- apps/desktop/src/renderer/pages/Creative.i18n.ts Creative.tsx Creative.test.tsx && echo IDENTICAL
3 Creative files IDENTICAL to main
$ git log origin/main -1 -- apps/desktop/src/renderer/pages/Creative.i18n.ts
046c5ba3 fix(creative): SD-01 —— 安全暂停续传与真实下载速度状态发布到 main (#1023)
```
分支里的 9038c594（#967）来自 ab/showcase-download，该线经 #1023 发布到 main，F1 修复（3540a06e）随之合入。
归类 SUPERSEDED。

### baton/grokbot-a24-20261015-104002-linux-hw-r2
```
$ git cherry -v origin/main <ref>
- a9b36cee fix(hw): HW-1 ...             # 等价于 main 的 0b8325b0 (#1025)
+ 893dd9a7 baton: 回执 started
+ 5fb5e12f test(hw): 新增 Linux 运行时正向对照 ...   # disks.rs +102, probe.rs +137，仅测试
+ 48d4ce04 baton: 回执 completed 与 Linux 硬件验证中文证据
$ for fn in <9 个新测试名>; do git grep -c "fn $fn" origin/main -- rust/crates/agent24-hw; done    -> 全为 0
$ git merge-tree --write-tree --merge-base=a9b36cee origin/main <ref>   -> clean
```
测试名举例：`linux_home_vs_homework_boundary_counterexample`、`linux_factory_system_has_cpus_ram_and_no_processes`、`linux_production_probe_is_bound_to_factory`。baton 流程只推分支、不开 PR，所以没有关闭原因，只是没人接手。
归类 VALUABLE（低）：覆盖 HW-1 的 Linux 路径边界，还差：开 PR、在 CI 的 Linux runner 上跑一次。建议 base = main。

### baton/grokbot-a24-20261015-104002-wal-race-r2
```
$ git cherry -v origin/main <ref>      # a9b36cee 为 -，其余 6 个为 +（2f3c225f RED 测试、408d414d GREEN、335c2585 证据报告、3 个回执）
$ git show origin/main:rust/apps/agent24-documents/src/state/tests.rs | sed -n 168,172p
    for suffix in ["", "-wal"] {
        ...
        if made.path().join(&name).exists() {
            fs::copy(...).unwrap();          # 仍是 exists→copy 的 TOCTOU 写法
$ git grep -n 'VACUUM INTO' origin/main -- rust/apps/agent24-documents      # 无输出
$ git ls-tree -r --name-only origin/main | grep -i wal-race                 # 无输出
$ git merge-tree --write-tree --merge-base=a9b36cee origin/main <ref>       # clean
```
证据报告 `docs/agent/cloud-wave37/a24-wal-race-20261015-104002.md` 的实测：300 次 close 后 `-wal` 仍在 241 次，其中 215 次在 20ms 内自行消失，即 CI 偶发 `os error 2` 的机制。
归类 VALUABLE（低）：只改测试代码，修一个真实的偶发 CI 红。建议 base = main（或 ab/documenting，二选一）。分支里的 `.md` 报告和 baton 回执文件不必带入 PR。

### backup/ab-idoris-04-receipts-pre-cla-20261019、backup/ab-idoris-11-pair-acceptance-pre-cla-20261019
```
$ git cherry -v origin/ab/idoris origin/backup/ab-idoris-04-receipts-pre-cla-20261019             # 空
$ git cherry -v origin/ab/idoris origin/backup/ab-idoris-11-pair-acceptance-pre-cla-20261019     # 空
$ git diff --stat 3042dfb2 origin/backup/ab-idoris-04-receipts-pre-cla-20261019         # 空（#1028 PR head）
$ git diff --stat 7bd54ce3 origin/backup/ab-idoris-11-pair-acceptance-pre-cla-20261019  # 空（#1029 PR head）
$ git merge-base --is-ancestor 3042dfb2 origin/ab/idoris  -> 真；7bd54ce3 同
```
这两个备份是"重写 bot merge commit 作者之前"的快照，树与合并进 ab/idoris 的 PR head 一致。ab/idoris 上对应 merge 是 597beaaa（#1028）、fa517176（#1029），作者已是 jhfnetboy。
归类 OBSOLETE。

### ab/weknora-03a / 03b / 03c 及其 -1、-2
```
$ git merge-base --is-ancestor origin/ab/weknora-03a-state-machine-1 origin/ab/weknora-03a-state-machine-2   # 真
  ...-03a-state-machine-2 → -03a-state-machine；-03b-ui-bridge-1 → -03b-ui-bridge；-03c-ops-1 → -2 → -03c-ops   # 均真
$ git diff --stat <merge-base> origin/ab/weknora-03a-state-machine  -> docs/knowledge/CONTRACT-STATE-v1.md | 539 +
  同样 03b: CONTRACT-UI-BRIDGE-v1.md 409 行；03c: CONTRACT-OPS-v1.md 549 行
$ gh pr view 1070 --json comments    # 1070–1077 均有同一类评论
"关闭：按负责人 2026-10-20 的决定（Grok、Codex 盲选一致），这份契约压成规范核心，改由 #1113 承载。……作为非规范参考保留，供实现任务以测试证明……分支不删除。"
```
替代者：#1113（STATE，合并 2026-10-20）、#1114（OPS）、#1115（UI-BRIDGE），PR 标题明写"取代 #1070–#1072"等。ab/weknora 上的行数：STATE 202、OPS 172、UI-BRIDGE 197（对比分支 539/549/409）。
**关键点**：ab/weknora 上的核心文档直接引用这些分支名：
```
$ git grep -n '保留在分支' origin/ab/weknora -- docs/knowledge
CONTRACT-OPS-v1.md:172: ……保留在分支 `ab/weknora-03c-ops`（#1075 的分支）
CONTRACT-STATE-v1.md:202: ……保留在分支 `ab/weknora-03a-state-machine`（#1072 的分支）
CONTRACT-UI-BRIDGE-v1.md:197: ……保留在分支 `ab/weknora-03b-ui-bridge`（#1077 的分支）
```
所以三条顶端分支不是"被代码替代"，而是被设计成长期保留的参考。归类 VALUABLE（保留，勿删）。-1、-2 中间分支是顶端的祖先，内容完全包含在顶端中，SUPERSEDED，可删。
建议：如果将来要清分支，先把三条顶端打成 tag（例如 `archive/weknora-03a`），并把核心文档里的分支名改成 tag；这是 jason 的决定，我没有动。

### ab/kernel-22-egress-exec-memory
```
$ git show --stat 042451f3   -> agent24-agent/src/lib.rs、domain/egress.rs、store/source_policy.rs、tools/src/local.rs  (+245 -65)
$ gh pr view 793 --json comments   (closed 2026-10-25T08:26:09Z)
"本 PR 的代码不会合入。……shell_exec 的语义：David 已确认，保留 ab/kernel 现有做法……分支保留，不删除。"
"本 PR 的范围（K1-6b.4）已由下面五片重新实现，并已合入 ab/kernel"：#1180 #1199 #1200 #1203 #1204
$ gh pr view <n> --json state   -> 五个均 MERGED，base ab/kernel（10-24 至 10-25）
$ git grep -n list_session_source_tags origin/ab/kernel -- rust     # 无输出（沿用的思路改名/改实现）
```
评论里说明沿用的部分：撤销后重查的测试进了 #1180，召回打标签进 6b.4b（改为带存储分类），会话标签继承进 6b.4d。"所有 run 一律拒绝"的改法被明确否决。
归类 SUPERSEDED。注意评论写了"分支保留，不删除"，是 David 的明确意愿，删前先问他。

### ab/kernel-27-source-classification
```
$ git show --stat 6ad5e7d9   -> 12 个文件：memory/{assertion,writer,...}.rs、store/source_policy.rs、migrations/0018_assertion_source_ref.sql、agent/{lib,retain,session_memory}.rs
$ git show --stat cf8ccd48   (#874，已在 ab/kernel)   -> 8 个文件，含同一个 0018_assertion_source_ref.sql
$ diff <(git show origin/ab/kernel-27-source-classification:.../0018_assertion_source_ref.sql) <(git show origin/ab/kernel:.../0018_...sql)  -> 无差异
$ git merge-base --is-ancestor 6ad5e7d9 origin/ab/kernel    -> 否
```
6ad5e7d9 的 memory/store 半边 = #874（P1a）；agent 半边（recall 返回 `Vec<SourceRef>`、`tag_run_source`）被 #1200 用"保存的分类打标签"重做，ab/kernel 里已有 `with_source_ref`（agent/lib.rs、retain.rs、memory/writer.rs）、`unknown_memory`（store/source_policy.rs）、`revision_digest`。ab/kernel 现在的迁移已到 0025，并有 0023/0024。
归类 SUPERSEDED。没有对应 PR，所以不是"被关闭"，而是被后续切片取代。

### codex/comm5b-zero-run-20261005
```
$ git diff --stat origin/codex/comm5b-zero-run-20261005 origin/main -- rust/crates/agent24-comm/tests/comm_zero_run.rs rust/crates/agent24-comm/tests/comm_dependency_allowlist.rs rust/apps/agent24d/tests/comm_blackbox.rs rust/crates/agent24-comm/tests/fixtures/comm_inbound_six.json
(空：四个文件与 main 逐字节相同)
$ git log origin/main -S'six_inbound_shapes_and_read_routes_do_not_dispatch_work' --oneline
fc862cf3 feat(comm): close Agent24 × Hyphae joint-debug gates (#676)
$ gh pr view 830 -> CLOSED 2026-10-09，"配合协调者换场排空指令，关闭 PR 并保留本地脏工作区。"
```
#830 之后在 ab/comm 上重做为 #857（MERGED 2026-10-21，更严格的版本），ab/comm 的 comm_zero_run.rs 函数名与分支不同，说明加固过。
归类 SUPERSEDED。

---

## 四条长期没动的集成分支

四条都**没有**从自己名字发往 main 的 release PR：`awk -F'\t' '$5=="main"' prs.tsv` 里 release/merge 类的只有 documenting-slice1-r1..r5、kernel-to-main（#1022）、m1-sync、open-design-m10、v0.2.0、v0.4.0，没有 comm、decide、idoris、maint。也没有任何 OPEN PR 以这四条为 base 或 head。
与 main 的关系（`git rev-list --count`）：

| 线 | tip（远端） | 最后提交 | 落后 main | 领先 main | 合并进 main 的试合并 |
|---|---|---|---|---|---|
| ab/comm | ca86c0b5 | 2026-10-21 | 528 | 11 | `git merge-tree` 报 84 行 auto-merge/冲突，含 Communication.tsx、Creative.tsx 等 |
| ab/decide | 1ea40c62 | 2026-10-19 | 503 | 25 | 79 行，含 Creative.*、docs/agent/tasks.md |
| ab/idoris | fa517176 | 2026-10-20 | 239 | 36 | 11 行，冲突在 rust/Cargo.lock 与 models/src/lib.rs |
| ab/maint | 8aaf474a | 2026-10-14 | 480 | 2 | 0 行冲突 |

### origin/ab/comm：COMM（Hyphae）线，暂停，未结束
- 已合入该线的 PR：#741 plan、#768 local-config、#776 relay-config、#857 5b 零运行门禁，#1017 吸收 main。
- 独有（非 merge）提交：027eed7b COMM-6b 规划（docs/agent/PLAN-COMM6b.md）、5757e054 COMM-6b.1 桌面"新增联系人"表单、e5c5d2ad COMM-6b.2 Relay 编辑与手动探测、21f5ed63/430b457f/cf85b236 COMM-5b 加固（含 hyphae.lock 升级和 macOS 握手修复）、5f27fcac 吸收 main。
- 是否已在 main：`git cherry origin/main origin/ab/comm` 7 个均为 `+`。main 的 Communication.tsx 是 219 行，ab/comm 的是 313 行，两者相差 343 行删除，说明 6b.1/6b.2 的 UI 不在 main。main 里没有 `docs/agent/PLAN-COMM6b.md`、`docs/comm/COMM-5b-zero-run.md`。
- 这条线不是被放弃，而是 main 上继续用别的方式推进：Hyphae lock 升到 v0.26.3 是直接开到 main（#1193，10-24，source_sha 144555ca；ab/comm 上是旧的 65d27380），joint 测试在 main（#1137，10-22）。main 的 `docs/agent/tasks.md` 仍把 COMM-5b 标为 BACKLOG（第 71 行）。
- 判断：暂停。独有价值是 COMM-6b.1/6b.2 UI 与 5b 加固，但 `hyphae.lock.json` 和 `binary.rs` 的改动要丢弃（main 更新），Communication.tsx 要在 #950 的 i18n 基础上重新适配。**不要直接合并该线**，要拆片 cherry-pick。

### origin/ab/decide：决策模型线，暂停，未结束
- 已合入该线的 PR：#726 三语评测、#727 jev-study、#734 多尺寸、#754 D1 计划、#760 ONNX spike、#854/#855 D1-2a、#908 D1-2a 实现、#909 D1-2b 契约、#1033 D1-2b 实现、#1035 D1-2c 契约、#1018 吸收 main。最后一次合并 10-19。
- 独有内容：上述全部，**均未进 main**（main 上只有更早的 D0 PR：#686/#687/#688/#689/#717/#718/#722）。
- 计划里剩下的未做：`docs/agent/PLAN-DECIDE-D1.md` 的 D1-2c 实现（L3 layers、L4）、D1-3 组件目录与按需下载、D1-4 retain.intent、D1-5 recall.gate、D1-6 真机验收。`git ls-tree origin/ab/decide` 没有这些任务的分支或 PR。
- 判断：暂停，按负责人记忆里的排期（M2 发布 → K1 → 决策服务）是等待状态，不是结束。baton 的 d1-2b/2c 分支只是这条线上的旁支副本，可删。

### origin/ab/idoris：iDoris 网关线，暂停，未结束
- 已合入该线的 PR：#980（IDORIS-01）、#981（02）、#982（05）、#983（04 gateway）、#984（04 probe env）、#985（08 comm fixtures）、#1028（04 receipts）、#1029（11 pair acceptance）、#1040（12 CI fixture diagnostics）、#1100（test/comm-daemon-supervise-determinism）。最后 10-20。
- 独有内容：`docs/agent/PLAN-IDORIS-V031.md` 里 IDORIS-01 至 -06、-08、-11、-12 对应的提交，未进 main。`git cherry origin/main origin/ab/idoris` 17 个 `+`，1 个 `-`。
- 计划里未做：IDORIS-07"031 发布可审核证据包"（含真机 smoke、版本/校验和）。计划第 19 行写明最终 release PR "由人开审，并由 Jason 真机验收"。
- 判断：暂停，等 IDORIS-07 和 jason 的真机 gate。本分支同时是 #1067 被否决后的 CLA 作者改写目标，作者已改好。
- 注意：该线会与 main 在 `rust/Cargo.lock`、`agent24-models/src/lib.rs` 冲突，release 前需要先吸收 main。

### origin/ab/maint：维护线，一次修复后停
- 独有提交：30bf89b8 `fix(desktop): MAINT-01 —— iDoris Admin 的 token 帧按字节校验，admission_status 必须是字符串`（#949，已合入 ab/maint，10-14）。
- 在 main 里**没有**这个修复：
```
$ git show origin/main:apps/desktop/src/main/idoris-admin.ts | grep -n -E "toString\('ascii'\)|String\(entry.admission_status\)"
69:  const token = frame.subarray(0, 64).toString('ascii')            # 先 ascii 解码再校验，0xE1 会被清高位后通过
130:      if (!['ready', 'requires_eviction', 'blocked'].includes(String(entry.admission_status))) return null
$ git show origin/ab/maint:apps/desktop/src/main/idoris-admin.ts | grep -n -E "0x30|typeof entry.admission_status"
60:  return (byte >= 0x30 && byte <= 0x39) || (byte >= 0x61 && byte <= 0x66)
128:      if (typeof entry.admission_status !== 'string' || ...) return null
$ git merge-tree --write-tree --merge-base=30bf89b8~1 origin/main 30bf89b8   -> clean
```
#949 的 PR 正文自己写"`ab/maint` 和 `main` 上都存在"，所以这是一个 main 也有的输入校验漏洞，只在 ab/maint 修了。
- 判断：这条线只有这一个修复且已经没有后续。VALUABLE（小）：建议 cherry-pick 30bf89b8（2 个文件，+18 -3，含测试）到新分支，PR 到 main。

---

## VALUABLE / UNSURE 要点

VALUABLE（建议继续）：
1. `ab/maint`：idoris-admin 的 token 帧与 admission_status 校验修复，main 仍有漏洞，cherry-pick 30bf89b8，PR base = main，无冲突。
2. `baton/...wal-race-r2`：修 documents 测试的 WAL TOCTOU 偶发红，PR base = main，无冲突。需去掉 baton 回执文件。
3. `baton/...linux-hw-r2`：9 个 Linux 硬件测试，PR base = main，无冲突，价值低。
4. `ab/weknora-03a-state-machine`、`-03b-ui-bridge`、`-03c-ops`：不是待办，是被 CONTRACT-*-v1.md 引用的参考，不能删。
5. 集成线 `ab/comm`：COMM-6b.1/6b.2 UI 与 5b 加固未发布，需拆片重新适配；`ab/decide`、`ab/idoris`：整条线未发布，是暂停而非结束。

UNSURE（需要 jason 判断）：
- 无严格意义上的 UNSURE 分支，但有三个需要确认的问题：
  1. weknora 三条顶端分支要不要保留为分支，还是换成 tag（核心文档里按分支名引用）。
  2. `ab/kernel-22-egress-exec-memory` 评论里 David 写了"分支保留，不删除"，删之前要问他。
  3. k1review 的报告文档不在 main，是否要先存档。
