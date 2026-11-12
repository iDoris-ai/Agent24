# 进度（2026-11-10）

索引：[架构总入口](../../architecture/README.md)・同期另两份：[ARCH-AND-PLAN](ARCH-AND-PLAN.md)、[MILESTONES-AND-TASKS](MILESTONES-AND-TASKS.md)

| 项 | 内容 |
|---|---|
| 快照日期 | 2026-11-10（第二期） |
| HEAD sha | `c396478c9f8797c339096dbcdbf4d01b242d756f`（`origin/main`，Merge #1780） |
| 统计区间 | 上期 HEAD `f818b0bfd7f53cdf15fd0265bbb56f4bb8cb9e8b`（Merge #1591，2026-11-04）到 `c396478c`，即 `f818b0bf..origin/main` |
| 数据来源 | 本地 git：`git log --first-parent --merges`（§1、§2）；`git tag`（§2）；`gh pr view` 与 `gh release view`（§0 外仓部分）。没有核对 CI 结果和评审状态 |
| 生成方式 | §1 由脚本 `scripts/gen-tracking-merges.py --from-sha f818b0bfd7f53cdf15fd0265bbb56f4bb8cb9e8b --until origin/main` 生成，未手改；§2 的 tag 由 `git tag --sort=-creatordate --format='%(refname:short)|%(creatordate:short)'` 得到，release / integrate 合并从 §1 的分支前缀筛出；其余各节手写 |
| 节奏 | 每两周或每次 release 出一期（Q11）。下期用 `python3 scripts/gen-tracking-merges.py --from-sha c396478c9f8797c339096dbcdbf4d01b242d756f` |

## 0. 本期一句话

五天（2026-11-05 至 11-10）主线合入 43 个合并：第 0 期 S0-1 至 S0-7 与第 1 期 S1-1 至 S1-5、S1-7 全部合入（S1-6、S1-8 按拍板取消，S1-6 由此前的 #1580 取代），S1-9（OS 包完整性）从设计到 macOS 验签（#1611 至 #1780）全部合入；同期 ab/weknora、ab/ui 各有若干批集成合并，没有发新预览版（最近的预览版 tag 仍是 `v0.6.0-preview.20261028.12`）。外仓 Sin90 v0.5.2、Cos72 v0.1.2 于 2026-11-09 发布，已按新规则带完整性清单（数据来源 `gh release view`）。第 2 期（agent24d 瘦身）尚未开始。

## 1. 本期合入的 PR

由脚本生成，未手改。分支前缀即方向（`feat` / `fix` / `docs` / `chore` / `test` / `ci` / `refactor` / `release` / `integrate` / `ab`）。「是否改变架构 / ADR 状态」不在脚本能判断的范围内，由 [ARCH-AND-PLAN](ARCH-AND-PLAN.md) §0 和 §2 人工记录，本表不填。日期取 git 合并提交的作者日期，与 `gh` 给出的 UTC 合并时间可能差一天（如 #1592～#1596 在 git 里是 11-05，GitHub 上是 11-04）。

| PR | 日期 | 分支（前缀即方向） | 一句话（PR 标题） | 合并提交 |
|---|---|---|---|---|
| #1780 | 2026-11-10 | `feat/s1-9-pr3d-macos-signature` | feat(os): S1-9 PR-3d macOS 上 OS 包验签（受信 Team ID，可 trust-team） | `c396478c9` |
| #1756 | 2026-11-10 | `feat/s1-9-pr3c-require-integrity` | feat(os): S1-9 PR-3c 完整性清单改为必需 + 类型化拒绝原因 + 桌面提示 | `1ff792305` |
| #1755 | 2026-11-10 | `ci/rust-macos-timeout-45` | ci: macOS Rust 作业超时 30 → 45 分钟 | `1b5685429` |
| #1754 | 2026-11-09 | `feat/s1-9-pr3a-test-packages-sealed` | test(os): S1-9 PR-3a 测试包改为原生可执行并封完整性清单（强制仍关闭） | `7b4ddbc9c` |
| #1753 | 2026-11-09 | `feat/s1-9-pr3b-documents-integrity` | feat(desktop): S1-9 PR-3b 随包 documents 模块安装时生成完整性清单 | `ccf2b534b` |
| #1751 | 2026-11-09 | `feat/s1-9-integrity-verify-arch` | feat(os-packages): package-integrity verify --arch（发布脚本跨架构自检） | `6b9851fc5` |
| #1750 | 2026-11-09 | `feat/s1-9-integrity-generate-arch` | feat(os-packages): package-integrity generate --arch 指定目标架构 | `a55ce6645` |
| #1739 | 2026-11-09 | `feat/s1-9-n4-hash-cancel-cache` | feat(os): S1-9 N4 完整性校验随 daemon 关闭可取消 | `6dcc12f34` |
| #1749 | 2026-11-09 | `ci/rust-linux-timeout-30` | ci: Linux Rust 作业超时 20 → 30 分钟 | `21e338f30` |
| #1737 | 2026-11-09 | `feat/s1-9-n3a-native-exec` | feat(os-packages): S1-9 N3 安装包可执行文件须为本平台原生程序 | `b36589eb5` |
| #1733 | 2026-11-09 | `feat/s1-9-n3b-node-attach` | feat(examples): S1-9 N3 Node 参考模块改走 A3 attach | `925141343` |
| #1730 | 2026-11-09 | `feat/s1-9-n6-sdk-manifest-selfcheck` | feat(os-sdk): S1-9 N6 握手前清单自检（SDK 0.2.0，未发布） | `46a401176` |
| #1728 | 2026-11-09 | `integrate/ui-9b5a23f2` | chore(ui): ab/ui 第二批进 main —— M4 收尾（日程对话滚新、补录状态机）+ M5 B1 服务模式 LaunchAgent（#1691/#1708） | `b3bd541c9` |
| #1723 | 2026-11-09 | `docs/s1-9-design-r2` | docs(S1-9): 设计 r2 —— N3/N4/N6 前置、模块读磁盘清单、新上线顺序 | `1b660a191` |
| #1716 | 2026-11-09 | `integrate/ui-54fa22d5` | chore(ui): M4 助理合并 A 期进 main —— 命名与 id、对话文件 v2、四筛选、进度卡、日程归属对话、三页折进（#1691） | `b63062064` |
| #1697 | 2026-11-09 | `integrate/weknora-c198309c` | chore(weknora): ab/weknora 第四批进 main —— M3 本地模型抽取与草稿界面、验收脚本、脱敏残留 held_back（#1666 判据 2） | `8c88bc062` |
| #1693 | 2026-11-08 | `integrate/weknora-c1303160` | chore(weknora): ab/weknora 第三批进 main —— M3 契约 v2、准入（只读 floor 路由）、脱敏与本地草稿、READONLY r12（#1666 判据 1/2） | `930fbf70f` |
| #1665 | 2026-11-08 | `integrate/weknora-7af5c902` | chore(weknora): M2 第二批进 main —— 04b-2 回主对话：intent 准入、确认面板、草稿附件（#1627 判据 5） | `723674543` |
| #1663 | 2026-11-08 | `ab/weknora` | chore(weknora): M2 第一批进 main —— KnowledgeView 容器、入口、UI 会话、桥通道与 citation（#1627 判据 0/1/2/4） | `09dc26036` |
| #1658 | 2026-11-08 | `ab/ui` | integrate(ui): ab/ui 合入 main —— 对话标题、固定文案、对话分组/折叠/拖拽/归档（集成合并，不发版） | `db5d86b11` |
| #1650 | 2026-11-08 | `ci/consent-test-expiry-far-future` | test(agent24d): 模块授权导出测试的到期时间改为远期常量，修全仓 Rust CI | `0351ca4f1` |
| #1637 | 2026-11-07 | `integrate/ui-20261108` | integrate(ui): ab/ui 合入 main —— 主对话 Chat 增强：回合卡表、历史存储、历史界面（集成合并，不发版） | `498ac5642` |
| #1629 | 2026-11-07 | `ci/cla-only-on-pr` | ci: CLA Assistant 只在 PR 事件或 PR 上的评论时运行 | `9343a902d` |
| #1620 | 2026-11-07 | `integrate/weknora-20261107` | integrate(knowledge): ab/weknora 合入 main —— M1「自己的知识库」验收脚本与文档（集成合并，不发版） | `6630926a5` |
| #1614 | 2026-11-05 | `feat/s1-9-pr1-package-integrity` | feat(integrity): S1-9 PR-1 —— 共享文件校验、生成工具与旧包兼容读取 | `b059da114` |
| #1615 | 2026-11-05 | `fix/idoris-keys-etxtbsy` | test(idoris): keys 夹具预热遇 ETXTBSY 时重试（修今天 3 次 CI 偶发红） | `babe734b0` |
| #1613 | 2026-11-05 | `ci/dormant-features` | ci: 额外编译测试 S1-7 门控的休眠 feature（#1612 M1） | `d5d715cd6` |
| #1612 | 2026-11-05 | `refactor/s1-7-dormant-code` | refactor(store): S1-7 —— 收窄休眠代码 | `10cf4bdea` |
| #1611 | 2026-11-05 | `docs/s1-9-os-package-integrity-design` | docs(os-packages): S1-9 —— 明确 OS 包文件完整性与强制拒绝上线方案 | `78b3a3616` |
| #1609 | 2026-11-05 | `refactor/s1-3-daemon-json-parser` | refactor(S1-3): TS 端 daemon.json 只用 @agent24/data-dir 一个解析器 | `27e934ad3` |
| #1610 | 2026-11-05 | `chore/mbx-build-cache` | chore(build): 笔记本改由 mbx 接管 cargo 缓存，不再设 CARGO_TARGET_DIR | `ce85cdffc` |
| #1608 | 2026-11-05 | `refactor/s1-2-signing-team-id` | refactor(sign): enroll 签名要求与安装同严,Team ID 单一常量 + 一致性测试 (S1-2) | `e14164d0d` |
| #1606 | 2026-11-05 | `refactor/s1-1-openapi-drift` | refactor(api): OpenAPI 漂移收口（S1-1） | `6b099a0ac` |
| #1605 | 2026-11-05 | `refactor/s1-4-state-mapping` | refactor(components): S1-4 —— 状态映射与契约阈值残项 | `49277c2f2` |
| #1607 | 2026-11-05 | `fix/idoris-keys-flaky-macos` | test(idoris): 修 macOS/Linux CI 上 idoris keys 与 idoris_host 的偶发超时 | `03bc5cf4d` |
| #1602 | 2026-11-05 | `refactor/s1-5-od-pin-single-source` | refactor(desktop): S1-5 —— 统一 Open Design pin 来源 | `3b02f040b` |
| #1601 | 2026-11-05 | `docs/adr-044-045-accept` | docs(adr): ADR-044、ADR-045 改为已接受 | `d8e47df4a` |
| #1600 | 2026-11-05 | `docs/p0-adr-045-route-naming` | docs(adr): S0-7 —— ADR-045 路由命名约定 | `9dcdbf6b2` |
| #1595 | 2026-11-05 | `docs/p0-tracking-first-issue` | docs(tracking): S0-6 —— 三份跟踪文档首期（2026-11-05） | `7ea8944b2` |
| #1596 | 2026-11-05 | `docs/p0-adr-status-numbering` | docs(adr): S0-1 + S0-3 —— ADR 状态一次性订正与编号别名 | `5c64ce3cb` |
| #1593 | 2026-11-05 | `docs/p0-adr-044-communication` | docs(adr): 新增 ADR-044 通信（Hyphae/Nostr）草案（S0-4） | `7a2867605` |
| #1592 | 2026-11-05 | `docs/p0-adr-038-r5` | docs(adr): ADR-038 r5 订正稿，状态改为已接受（S0-2） | `6465d1811` |
| #1594 | 2026-11-05 | `docs/p0-layers-regen` | docs(arch): S0-5 —— ARCHITECTURE-LAYERS 依赖表由 cargo metadata 重新生成 | `5008f7ba1` |

共 43 个合并（`--from-sha=f818b0bfd7f53cdf15fd0265bbb56f4bb8cb9e8b`）。

## 2. 发布

预览版标签：本期**没有新的预览版**，最近一个仍是 `v0.6.0-preview.20261028.12`（2026-11-03）；预览版自 2026-11-04 停发。其余本期新 tag：

| tag | 日期 | 说明 |
|---|---|---|
| `agent24-os-sdk-v0.2.0` | 2026-11-09 | os-sdk 0.2.0（握手前清单自检，#1730）。PR 标题里的「未发布」指合入时尚未打 tag；合入后于 2026-11-09 在 #1730 的合并提交 46a40117 上打 annotated tag 并推送，Sin90#81、Cos72#14 已按此 tag 引用 |

外仓 Release（`gh release list` 核对）：

| 仓库 | 版本 | 日期 | 备注 |
|---|---|---|---|
| iDoris-ai/Sin90 | v0.5.2 | 2026-11-09 | 4 个包（linux-arm64、linux-x64、macos-arm64、macos-x64）加 `SHA256SUMS`；正式 Release 只上传 Linux 包，macOS 包由本机签名上传（Sin90 #82 标题） |
| MushroomDAO/Cos72 | v0.1.2 | 2026-11-09 | 同上（Cos72 #15 标题） |

区间内的 release / integrate 合并（用例验收记录写在各 PR 正文，本表不复述）：

| PR | 分支 | 日期 |
|---|---|---|
| #1728 | `integrate/ui-9b5a23f2` | 2026-11-09 |
| #1716 | `integrate/ui-54fa22d5` | 2026-11-09 |
| #1697 | `integrate/weknora-c198309c` | 2026-11-09 |
| #1693 | `integrate/weknora-c1303160` | 2026-11-08 |
| #1665 | `integrate/weknora-7af5c902` | 2026-11-08 |
| #1637 | `integrate/ui-20261108` | 2026-11-07 |
| #1620 | `integrate/weknora-20261107` | 2026-11-07 |

另有 #1663（`ab/weknora`）、#1658（`ab/ui`）两个直接以集成分支名合入的合并。按 `AGENTS.md`，release PR 正文必须列出跑过的用例、命令和结果；本期没有逐个核对，**未核实**。

## 3. 阻塞与风险

| 项 | 说明 | 来源 |
|---|---|---|
| `ab/comm`、`ab/decide` 严重落后 | 落后 main 2939、2914 个提交（首期 2487、2462），越拖越难吸收；本期已列为尾巴 2 | MILESTONES §1、§2.4 |
| `ab/weknora` 又出现 49 个未进 main 的提交 | 首期为 0；内容未核实 | MILESTONES §1 |
| X11 已收口；X7 不按「并入 comm」做 | X11 ②③④ 的入站准入已实现（PR 见 MILESTONES §2.4）。入站适配在 agent24d inbound，不在 comm。X7 的身份仍由 comm 管，旧 nostr-bridge 已删 | ADR-044；MILESTONES §2.4；INBOUND-ADMISSION-PLAN |
| agent24d 拆分时启动顺序有隐含依赖 | 第 2 期 S2-1 才会遇到；只移动不重排，以启动黑盒测试为闸。`serve()` 约 1,536 行，目标 < 300 行 | 方案 §6.1 |
| Rust CI 作业超时 | 本期把 Linux 超时放到 30 分钟、macOS 放到 45 分钟（#1749、#1755）；说明作业时长已逼近旧上限，**原因未核实** | #1749、#1755 |
| 82 条 followups 无人认领 | 只做了分类（MILESTONES §4），不逐条修，是 jason 的决定 | jason 2026-11-10 |
| ADR 索引与正文不同步会再发生 | 本期发现 ADR-038 一例；缓解：改正文状态时同 PR 改索引 | ARCH-AND-PLAN §2 |

## 4. 需要 jason 做的事

| 事项 | 说明 |
|---|---|
| 评审并合并本期跟踪文档 PR | 含 `docs/adr/README.md` 对 ADR-038 的订正（本 PR 唯一的非 tracking 改动） |
| 在 ARCH-OPTIMIZATION-PLAN.md 文首进度段追加第 0、1 期完成的 PR 号 | 方案原文只允许这一处追加；PR 号见 MILESTONES §2.1。本期**未代做** |
| 无待拍板问题 | 好友机制、C9 已由 jason 定为之后 / 未排期；闭环计划不再逐项问 |

## 5. 下期计划

只写任务 id，定义在 [MILESTONES-AND-TASKS](MILESTONES-AND-TASKS.md)：

- 清尾巴：尾巴 2（`ab/comm`、`ab/decide` 盘点存档）、尾巴 3（X11、X7）。
- 然后第 2 期从 S2-1 拆 `server.rs` 开始；S2-2 随后，S2-4 排在 S2-1、S2-2 之后。
- **2026-11-10 晚补记**：第 2 期已推进到 S2-4b。合入 #1788（S2-6 降级文档）、#1789（S2-7 桌面例外）、#1790（S2-2）、#1791（S2-1）、#1792（S2-3）、#1793（S2-4a），#1794（S2-4b）已合。本机 Codex 额度到 11-15，补审记为 FU-113；当天新增跟进项 FU-113 至 FU-119。
- **2026-11-12**：X11 ②③④ 收口（X11-2h）。入站表与准入、微信桥只传输已落地；防绕过测试与文档回写在本片。2e 的迁移 #1820 已合（bb03081e）；2e 第 2 部分 Hyphae 轮询走 #1821，尚未合入 main。
- S0-8 视评审带宽。
- 不在本轮：好友机制、C9；followups 82 条不逐条修。
