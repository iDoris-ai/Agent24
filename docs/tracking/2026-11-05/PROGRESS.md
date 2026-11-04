# 进度（2026-11-05）

索引：[架构总入口](../../architecture/README.md)・同期另两份：[ARCH-AND-PLAN](ARCH-AND-PLAN.md)、[MILESTONES-AND-TASKS](MILESTONES-AND-TASKS.md)

| 项 | 内容 |
|---|---|
| 快照日期 | 2026-11-05（首期） |
| HEAD sha | `f818b0bfd7f53cdf15fd0265bbb56f4bb8cb9e8b`（`origin/main`，Merge #1591） |
| 统计区间 | 首期没有上一期 sha，取「最近两周」：`--since=2026-10-22`（区间内最早的主线提交 `2a67f8914`）到 `f818b0bf` |
| 数据来源 | 本地 git：`git log --first-parent --merges`（§1、§2）；`git tag`（§2）。没有访问 GitHub，所以不含评审状态、CI 结果和 issue |
| 生成方式 | §1 由脚本 `scripts/gen-tracking-merges.py` 生成；§2 的版本和 release 合并由 `git tag -l --format='%(refname:short)|%(creatordate:short)' 'v0.6.0-preview.*'` 与同一条 `git log` 中的 `release/*`、`integrate/*` 分支筛出；其余各节手写 |
| 节奏 | 每两周或每次 release 出一期（Q11）。下期用 `python3 scripts/gen-tracking-merges.py --from-sha f818b0bfd7f53cdf15fd0265bbb56f4bb8cb9e8b`（范围 `<上期sha>..HEAD`，首期没有上期所以用了 `--since`） |

## 0. 本期一句话

两周里主线合入 103 个合并（含各方向集成合并），发了 12 个 `v0.6.0-preview.20261028.N` 预览版（10-28 至 11-03）；11-04 预览版停发，转入组件整合与统一，架构优化方案入仓（#1581），C8（Open Design 由 `SidecarManager` 托管）合入（#1580、#1584），第 0 期文档收口开工。

## 1. 本期合入的 PR

由 `python3 scripts/gen-tracking-merges.py --since 2026-10-22 --until origin/main` 生成，未手改。分支前缀即方向（`feat` / `fix` / `docs` / `chore` / `test` / `ci` / `release` / `integrate` / `ab`）。「是否改变架构 / ADR 状态」不在脚本能判断的范围内，由 [ARCH-AND-PLAN](ARCH-AND-PLAN.md) §0 和 §2 人工记录，本表不填。

| PR | 日期 | 分支（前缀即方向） | 一句话（PR 标题） | 合并提交 |
|---|---|---|---|---|
| #1591 | 2026-11-04 | `integrate/documenting-20261105` | 集成合并 ab/documenting → main（不发版）。David 2026-11-04 授权不等 jason 验收；T1-1/T1-2 已 11-04 真机验收通过（t1b 包 53decca8）；CI 全绿；… | `f818b0bfd` |
| #1589 | 2026-11-04 | `integrate/weknora-20261104` | 集成合并 ab/weknora → main（不发版）。David 2026-11-04 授权不等 jason 验收；CI 8 项全绿；PR-Daemon 机械核对两轮并于 fad3bd23 APPROVE（树 == m… | `d0e34f96e` |
| #1582 | 2026-11-04 | `chore/hyphae-lock-v0.30.1` | chore(comm): Hyphae 锁升到 v0.30.1 | `2d5e8c026` |
| #1584 | 2026-11-04 | `fix/c8-e2e-followups` | fix(desktop): C8 真机 e2e 跟进 —— degraded 打开即懒重启、退出后不再启动、删掉 Electron 下走不到的信号处理 | `0f72eeb18` |
| #1581 | 2026-11-04 | `docs/arch-optimization-plan` | docs(tracking): 架构优化方案与 C8 设计入仓 | `29be5a80c` |
| #1580 | 2026-11-04 | `feat/c8-od-sidecar-manager` | feat(desktop): C8 —— Open Design 改由 SidecarManager 托管（独立进程组、健康检查、有界停止） | `81c00c92a` |
| #1578 | 2026-11-04 | `fix/l2-attach-reserved-ids` | fix(agentd): #1572/#1575 遗留 —— 失配核 token_id、保留能力 id 与内置组件名不可声明、能力 id 去重 | `9bfaa3786` |
| #1579 | 2026-11-04 | `fix/l3-knowledge-docs-leftovers` | fix(agent24d): #1571/#1570 遗留 —— Knowledge 业务调用闸与 §8 就绪探测，Documents 期望与实际版本对账 | `f241180cb` |
| #1577 | 2026-11-04 | `fix/l1-hyphae-leftovers` | fix(comm): #1574 遗留 —— 配置变更握手失败走重启策略、首次探测通过才 ready、降级阈值读锁 | `a5fe90180` |
| #1573 | 2026-11-04 | `fix/a6-review-lows` | fix(components): 清掉近期评审遗留的 Low（#1552/#1554/#1557/C3b/stop_grace_ms） | `163dd2d3d` |
| #1575 | 2026-11-04 | `feat/a5-os-mcp-registry` | feat(components): C7 —— 领域 OS 与 MCP 接入 /components，建立能力登记表 REGISTRY.md | `f9868aff3` |
| #1574 | 2026-11-04 | `feat/a1-hyphae-health` | feat(comm): C2 —— Hyphae health 接入组件状态（连续 3 次才降级）并在启动时校验 contract | `b7e00bfb4` |
| #1571 | 2026-11-03 | `feat/a3-knowledge-component` | feat(components): C5 —— Knowledge 能力探测映射到 D3/D5 | `88e97f50c` |
| #1572 | 2026-11-03 | `feat/a2-agentear-component` | feat(components): C4 —— AgentEar 登记进 /api/v1/components | `e75e9b622` |
| #1570 | 2026-11-03 | `feat/a4-documents-component` | feat(documents): C6 —— Documents 补 D9 清单，/components 用它填 provides | `933c3e1e5` |
| #1566 | 2026-11-03 | `docs/agents-md-author-email` | docs(agents): 编码代理提交作者改为 firsttorch66@gmail.com | `1df67264c` |
| #1557 | 2026-11-03 | `fix/comm-export-hang` | fix(desktop): 导出私钥备份后按钮不再一直变灰（保存对话框挂在禁用的父窗口上） | `c212e5914` |
| #1556 | 2026-11-03 | `fix/od-node-runtime-helper` | fix(desktop): OD 子进程改由 Electron Helper 运行，Dock 不再多出「exec」 | `9b107554f` |
| #1555 | 2026-11-03 | `feat/od-pin-odx3` | feat(open-design): pin 升到 f5beffcb —— 去云账户/去升级弹窗/跳过引导/小人，并把自带 agent24 CLI 交给 OD | `9865d7a03` |
| #1554 | 2026-11-03 | `fix/comm-forward-path-params` | fix(comm): 带路径参数的 comm 路由经 runtime_router 转发不再 500 | `74f012706` |
| #1532 | 2026-11-02 | `release/documenting-r8-20261101` | release(documenting): 第八次发布：桌面详情页原页面图（#1518） | `70bfa570b` |
| #1552 | 2026-11-02 | `feat/hyphae-rejection-signal` | feat(comm): Hyphae 启动验签拒绝信号（signature_rejected / hash_mismatch）+ #1551 评审 L1/L2 | `e8b58246c` |
| #1551 | 2026-11-02 | `feat/hyphae-v0.30.0-signed` | feat(comm): Hyphae 升到签名版 v0.30.0 —— schema 2 lock + macOS 验签（ADR-038 C1b） | `af4b1603f` |
| #1549 | 2026-11-02 | `feat/component-c3b` | feat(desktop): ADR-038 C3b —— 桌面上报 Open Design 组件状态，页面改读统一组件接口 | `01f49e3ae` |
| #1548 | 2026-11-02 | `design/idoris-mascot` | design: iDoris 像素小人（四态，照 logo 重描，jason 已确认） | `050350d5f` |
| #1546 | 2026-11-02 | `chore/hyphae-v0.29.0` | chore(comm): Hyphae lock 升到 v0.29.0（含 C2 contract/health） | `612f0f0fa` |
| #1545 | 2026-11-02 | `feat/component-c3a` | feat(components): ADR-038 C3a —— 组件状态模型 + GET /components（分受众脱敏）+ OD 上报 + 停止宽限 15s | `777908074` |
| #1543 | 2026-11-02 | `fix/comm-page-feedback` | fix(desktop): 通信页真机反馈 —— 备份导出挂死修复（窗口保证收敛并串行）、显示保存路径、绿色对勾、停止按钮按状态 | `0dca0885b` |
| #1544 | 2026-11-02 | `feat/odx1-pin-v0261` | feat(open-design): ODX-1 —— pin 升到 fork v0.26.1 | `22bb434cd` |
| #1542 | 2026-11-02 | `feat/pin-slots-shortcut` | feat(desktop): 钉选格数旁加「去能力中心选择」入口，能力中心满格提示可跳回并高亮设置项 | `29c5cba6b` |
| #1539 | 2026-11-02 | `feat/component-c1` | feat(components): ADR-038 C1 —— ComponentLock schema 2、通用下载器与签名校验（Install/AtRest）、依赖反转、iDoris 迁到托管根 | `7e9bc3b1c` |
| #1541 | 2026-11-02 | `docs/od-idoris-plan` | docs: Open Design iDoris 版改造计划（升级 v0.26.1、去账户系统、主题/三语同步、小人、命名；已拍板） | `3e1085afa` |
| #1540 | 2026-11-02 | `feat/core5-5f3-startup` | feat(core): CORE-5f-3 —— 停靠态启动接线、发现与 ready 管道、停靠登记通道（ENABLED=false 不变） | `e03749e07` |
| #1538 | 2026-11-02 | `fix/comm-1536` | fix(comm): #1536 备份/恢复加固 —— inode 核对不误删、恢复传内容、exFAT/FAT 发布、高权限路由移出代理、relay 规范化 | `0847744cb` |
| #1537 | 2026-11-02 | `docs/comm-friends` | docs: 好友机制设计（COMM-FRIENDS，已拍板）+ ADR-038 r4（contract/health、锁过渡、spctl 订正） | `56ec65b7f` |
| #1535 | 2026-11-02 | `feat/comm-page-usability` | feat(comm): 通信页可用性 + 私钥备份/恢复（NIP-49 ncryptsec，锁 Hyphae v0.28.1） | `63a480aef` |
| #1533 | 2026-11-01 | `fix/mac-release-open-design` | fix(release): Mac 出包补上 Open Design 按需组件（签名原生模块、清单入包、出处校验） | `931871c26` |
| #1534 | 2026-11-01 | `docs/architecture-index` | docs: 架构总入口与全仓 ADR 索引（60 份），architecture 与 adr 双向链接 | `1bcb8972d` |
| #1530 | 2026-11-01 | `docs/superagent-evolution` | docs: DeepSeek Super Agent 演进研究 + ADR-040..043（提议）；订正 relay 下线说法 | `e5a1c6713` |
| #1528 | 2026-11-01 | `feat/core5-5f2-dock-router` | feat(core): CORE-5f-2 —— 停靠诊断 router、/health 503 与 OpenAPI（未接启动） | `1d5607008` |
| #1529 | 2026-11-01 | `fix/relay-default-hyphae` | fix(desktop): 默认 relay 改为 Hyphae 自建 relay（wss://relay.aastar.io） | `6c759ab7c` |
| #1527 | 2026-11-01 | `docs/evolvability-assessment` | docs(architecture): 可进化性评估三步终稿 + ADR-039 概念模型 + ADR-038 组件契约 v1 | `09cdad8f5` |
| #1526 | 2026-11-01 | `docs/architecture-layers-refresh` | docs(architecture): ARCHITECTURE-LAYERS 对照 main@bb434b61 事实修正 | `6a009d1df` |
| #1515 | 2026-11-01 | `release/weknora-r2-20261030` | KB-R2：Knowledge 第二次发布（ab/weknora → main）—— 主对话检索知识库并给出引用 | `bb434b61b` |
| #1521 | 2026-10-31 | `fix/1498-k1-r3-followups` | fix(accept): K1-R3 发布后跟进 —— R1 判据补强、证据列按字符截断、0030→0033 联合测试、0033 大库实测 | `886b83db8` |
| #1522 | 2026-10-31 | `fix/1499-domain-stop-marker-wait` | fix(agent24d): 计数模块夹具用 wakeup fd 处理 SIGTERM —— 修复 stop 测试偶发丢 atexit 标记 (#1499) | `7caf97618` |
| #1520 | 2026-10-31 | `fix/chat-new-conversation` | fix(desktop): 聊天页新增「新对话」入口 —— 清空消息并换用新 session_id | `22272b817` |
| #1512 | 2026-10-30 | `ab/idoris-integration` | release(idoris): ab/idoris-integration → main（I-1 agent24 idoris 走 CORE-2 登记，preview.7） | `5bbdf3df3` |
| #1496 | 2026-10-30 | `feat/hyphae-onboarding-v2` | feat(desktop): HY-2 —— 通信设置引导（创建身份 + 连接 relay，基于 HY-1 v2） | `a9fdf8273` |
| #1489 | 2026-10-30 | `feat/hyphae-on-demand-install-v2` | feat(comm): HY-1 —— Hyphae 按需下载安装（移植到 main，替代 #1458） | `0b44ea062` |
| #1505 | 2026-10-30 | `release/documenting-r7-20261030` | release(documenting): 第七次发布 —— CLI 新增 find、extract、extraction、render，导入重试退避 | `be2169452` |
| #1497 | 2026-10-29 | `release/kernel-k1-r3-20261029` | release(kernel): K1 第三次发布 —— 工具结果来源标签、会话来源与 peer 视图、恢复与授予判定、run/schedule CLI（迁移 0031–0033） | `47f1b9dd2` |
| #1494 | 2026-10-29 | `ab/agentear` | release(agentear): ab/agentear → main（AgentEar 接入 A-1–A-3，preview.6） | `ae9e32b44` |
| #1488 | 2026-10-29 | `ab/desktop-ia` | release(desktop): ab/desktop-ia → main（侧边栏 + 能力中心改版，preview.5） | `453715b54` |
| #1486 | 2026-10-29 | `ab/idoris` | release(idoris): ab/idoris → main（M-IDL1 一站式 iDoris 安装，preview.4） | `5c3a98c92` |
| #1484 | 2026-10-29 | `ab/core` | release(core): ab/core → main（CORE-2 AgentEar 配对 + CORE-5 迁移守卫，preview.3） | `a179348fa` |
| #1449 | 2026-10-28 | `docs/code-review-2026-10-27` | docs(review): REVIEW-TODO 补 10-27/10-28 新增条目（REV-10、FLAKY-1、AE-PAIR、ID-HR、CORE-2-FULL 等） | `478868687` |
| #1466 | 2026-10-28 | `ci/hyphae-test-bin` | ci(comm): CI-HYB —— CI 下载并校验 Hyphae 二进制，真二进制测试在 CI 真跑 | `d9f66560f` |
| #1467 | 2026-10-28 | `fix/flaky1-attach-timeout` | fix(cli): FLAKY-1 续 —— ready 等待失败后终止并回收未就绪的 daemon | `5f315855e` |
| #1452 | 2026-10-28 | `fix/rev10-verified-binary-etxtbsy` | fix(comm): REV-10 —— Hyphae 安装改由 cp 子进程写入，根除 ETXTBSY 窗口 | `ea89ac2c9` |
| #1451 | 2026-10-28 | `fix/sw1-reclaim-completed-at` | feat(scheduler): SW-1 —— 回收已完成超过 7 天的一次性日程 | `e13efa3c4` |
| #1456 | 2026-10-28 | `fix/flaky1-timing-tests` | fix: FLAKY-1 —— 模块 socket 路径超长（真实产品 bug）、WAL 测试假设、attach 测试进程管理 | `b4aaab57d` |
| #1434 | 2026-10-28 | `fix/schedules-fired-one-time` | fix(schedules): 已触发的 one-time（self-wake）不再堆在列表里 —— 折叠进「已完成」并提供一键清理 | `931d81779` |
| #1442 | 2026-10-28 | `docs/idoris-launcher-headroom` | docs(idoris): 登记 iDoris v0.4.0 余量确认行为变化（503 headroom_confirmation_required） | `863e7b47b` |
| #1438 | 2026-10-28 | `fix/desktop-install-cli` | feat(desktop): 桌面端安装/更新 agent24 命令行工具 —— DMG 不再留下过期 CLI（jason 10-28 人工测试） | `1b8a25157` |
| #1437 | 2026-10-28 | `docs/idoris-launcher-design` | docs(idoris): 一站式 iDoris 启动器设计 —— 下载→安装→运行→连接（agent24d 托管，M-IDL1 = IL-01..13） | `ea06137a0` |
| #1436 | 2026-10-28 | `fix/preview-ux-not-configured-states` | fix(desktop): 预览版人工测试 —— 未安装/未配置状态不再显示为报错（Documents/Comms/Design/iDoris/oMLX key/托盘默认英文） | `24ca4796d` |
| #1439 | 2026-10-28 | `ab/idoris` | release(idoris): 契约版本检查放宽为 1.x 且 ≥1.0.1（为 iDoris v0.4.0 / 契约 1.1.0 做准备） | `2a116fe70` |
| #1431 | 2026-10-28 | `chore/hyphae-lock-v0.27.2` | chore(comm): lock Hyphae v0.27.2（076a5205）—— 退出码 4→1，groupchat 接线前置（#1346） | `025dc3ad2` |
| #1430 | 2026-10-28 | `ab/idoris` | release(idoris): ID-ERR-1 —— iDoris v0.3.2 的 502 upstream_auth_failed 按终态处理；配对基线升到 v0.3.2 | `0660d2e80` |
| #1429 | 2026-10-28 | `test/flaky-daemon-supervise-etxtbsy` | test(comm): 修复 daemon_supervise 在 Linux 上的 ETXTBSY 偶发失败（假 hyphae 脚本写入 fd 被并行 fork 继承） | `bfa590571` |
| #1421 | 2026-10-28 | `chore/release-0.6.0-preview-prep` | chore(release): v0.6.0-preview.20261028.1 发布准备 —— 版本号、CHANGELOG、验收与签名清单 | `4a2b55dd1` |
| #1420 | 2026-10-28 | `fix/idoris-error-hints` | fix(idoris): 401/403 提示指向 data key，503 local_only_unavailable 提示本地没有可用模型（ID-8 真机发现） | `4fb165ec7` |
| #1405 | 2026-10-27 | `ab/idoris` | release(idoris): iDoris v0.3.1 接线 —— 独占本地网关、回执、预算导出（v0.6.0-preview） | `956e9009c` |
| #1396 | 2026-10-27 | `release/weknora-r1` | release(weknora): KB-R1 —— Knowledge 线第一次发布（v0.6.0-preview） | `3659317d0` |
| #1381 | 2026-10-27 | `perf/p13-schedule-tick-index` | perf(scheduler): P-13 —— 日程 tick 改为 SQL 侧过滤 + 部分索引（迁移 0030） | `bb3fb6d83` |
| #1386 | 2026-10-27 | `release/documenting-slice1-r6` | release(documenting): DOC-1-52 —— 第六次发布（r6）：文档导入与操作走 CLI（与桌面共用）、导入取消与重试、文档详情与文字预览 | `8bba633eb` |
| #1377 | 2026-10-27 | `comm/groupchat-wiring` | feat(comm): COMM-G1 —— Hyphae 群聊命令接线（9 条 /comm/groupchat 路由）+ comm 全部 23 个路径补进 OpenAPI | `557c93410` |
| #1368 | 2026-10-27 | `chore/hyphae-lock-v0.27.1` | chore(comm): lock Hyphae v0.27.1 (859d8fb7) —— 仅 TUI 增量，命令契约不变（#1346） | `df8a9d96b` |
| #1341 | 2026-10-27 | `ab/memory-p1` | release(memory): P1 记忆线发布 —— R05 来源契约、H02 审计修复、P1-02c 配额/失败/磁盘门/留痕（v0.6.0-preview.20261028.1；待 #1280 先合） | `c98745b07` |
| #1364 | 2026-10-27 | `fix/pre-release-daemon-hardening` | fix(daemon): 发布前加固 PR-B —— REST Host/Origin 守卫、统一 loopback 客户端与 MCP 总时限、service 模式 ready 行不带 token、release 工作流… | `99715e05a` |
| #1358 | 2026-10-27 | `fix/pre-release-desktop-hardening` | fix(desktop): 发布前加固 PR-A —— backendProxy 白名单/超时/体限、IPC sender 校验与导航守卫、oMLX 加固、退役 node-daemon 模块入口 | `eec340aa1` |
| #1353 | 2026-10-27 | `ci/dep-b-sign-notarize` | ci(release): DEP-B2/B3 —— macOS 签名+公证可复现流水线（Developer ID + notarytool，含验证脚本） | `98e94bc8b` |
| #1366 | 2026-10-27 | `test/flaky-models-hangup` | test(models): 修复 a_backend_that_hangs_up_while_a_large_prompt_is_written 的偶发失败（根因：16 MiB 序列化+写入与 1s 首块计时器竞态） | `67ac831a9` |
| #1354 | 2026-10-27 | `chore/agentear-vendor-v0.28.0` | chore(agentear): vendored 契约重钉到 AgentEar v0.28.0（0b6409ba，#1347 联合发布宿主侧） | `82f745734` |
| #1352 | 2026-10-27 | `chore/hyphae-lock-v0.27.0` | chore(comm): lock Hyphae v0.27.0 (9126147e) —— #1346 第 1 项 | `15d5c036f` |
| #1350 | 2026-10-27 | `docs/code-review-2026-10-27` | docs(review): 2026-10-27 全代码评审报告 —— 架构/结构通信/安全/性能四维度，61 条发现归并 8 个根因与处置排期 | `d253bf2ff` |
| #1280 | 2026-10-27 | `release/kernel-k1-r2-20261026` | release(kernel): K1 第二次发布 —— 模块工具授予路径、协议 4（按引用停）、来源分类与 peer 视图（迁移 0019–0026） | `03bf1e17b` |
| #1304 | 2026-10-27 | `docs/docs-2-adr037-fixes` | docs: DOCS-2 —— ADR-037 状态与签名事实订正、ARCHITECTURE-LAYERS/README 架构对齐代码、ADR-K1-03 链接修复 | `982725e28` |
| #1287 | 2026-10-26 | `test/flaky-domain-disable-shutdown` | test(agent24d): 修复 disable 与 close 竞态断言的偶发失败 | `e0576841a` |
| #1286 | 2026-10-26 | `test/flaky-comm-zero-run` | test(comm): 修复 comm_zero_run 在 macOS 上的偶发失败 | `06a93beec` |
| #1288 | 2026-10-26 | `test/flaky-documents-cooldown` | test(documents): 冷却时长测试改用暂停的 tokio 时钟 | `809bc682d` |
| #1258 | 2026-10-26 | `test/hw-linux-probe` | test(hw): 补 Linux 运行时正向对照测试（9 个） | `12da3be8a` |
| #1257 | 2026-10-26 | `fix/maint-01-idoris-token-frame` | fix(desktop): MAINT-01 iDoris Admin token 帧按字节校验（安全） | `f72e76798` |
| #1259 | 2026-10-26 | `docs/repro-1020-report` | docs(agent): 补入 REPRO-1020 复现报告（#1105 引用） | `e5f7a487e` |
| #1256 | 2026-10-26 | `fix/documents-wal-race-test` | test(documents): 修复 WAL 复制的 exists→copy 竞态（偶发 CI 红） | `8dfe54424` |
| #1249 | 2026-10-26 | `docs/adr-037-product-form` | docs(adr): ADR-037 产品形态与进程模型 —— agent24d 是本体，外壳都是客户端（提议） | `3ec8d81df` |
| #1252 | 2026-10-26 | `fix/main-timings-relative-ts` | test(agent24d): timings 测试改用相对时间戳（main；同 #1246，cherry-pick ab/kernel 3103b6d8） | `29f7138cb` |
| #1193 | 2026-10-24 | `jhfnetboy/hyphae-lock-v0.26.3-20261024` | chore(comm): lock Hyphae v0.26.3 (144555ca) | `3a433bb0c` |
| #1137 | 2026-10-22 | `jhfnetboy/m1-joint-npub-class-20261022` | test(comm): joint_round1 —— 非法 npub 的 contact add 按锁定二进制记为 user_error | `f16e65a95` |
| #1136 | 2026-10-22 | `jhfnetboy/m1-a2-group-joint-20261022` | feat(nostr-bridge): A2 —— 三身份读回同一条群消息 | `060a30359` |
| #1135 | 2026-10-22 | `jhfnetboy/m1-a1-group-history-20261022` | feat(nostr-bridge): A1 —— 桥接读取群历史 | `61dc0d2a2` |
| #1120 | 2026-10-22 | `fix/794-outside-write-executes` | fix(tools): 根外写入在批准后只落在那个规范化目录（#794） | `2a67f8914` |

共 103 个合并（`--since=2026-10-22`）。

## 2. 发布

预览版标签（`git tag` 的创建日期）：

| 版本 | 日期 |
|---|---|
| `v0.6.0-preview.20261028.1` | 2026-10-28 |
| `v0.6.0-preview.20261028.2` | 2026-10-28 |
| `v0.6.0-preview.20261028.3` | 2026-10-29 |
| `v0.6.0-preview.20261028.4` | 2026-10-29 |
| `v0.6.0-preview.20261028.5` | 2026-10-29 |
| `v0.6.0-preview.20261028.6` | 2026-10-29 |
| `v0.6.0-preview.20261028.7` | 2026-10-30 |
| `v0.6.0-preview.20261028.8` | 2026-10-31 |
| `v0.6.0-preview.20261028.9` | 2026-11-01 |
| `v0.6.0-preview.20261028.10` | 2026-11-02 |
| `v0.6.0-preview.20261028.11` | 2026-11-03 |
| `v0.6.0-preview.20261028.12` | 2026-11-03 |

区间内的 release / integrate 合并（用例验收记录写在各 PR 正文，本表不复述）：

| release PR | 分支 | 日期 |
|---|---|---|
| #1591 | `integrate/documenting-20261105` | 2026-11-04 |
| #1589 | `integrate/weknora-20261104` | 2026-11-04 |
| #1532 | `release/documenting-r8-20261101` | 2026-11-02 |
| #1515 | `release/weknora-r2-20261030` | 2026-11-01 |
| #1505 | `release/documenting-r7-20261030` | 2026-10-30 |
| #1497 | `release/kernel-k1-r3-20261029` | 2026-10-29 |
| #1396 | `release/weknora-r1` | 2026-10-27 |
| #1386 | `release/documenting-slice1-r6` | 2026-10-27 |
| #1280 | `release/kernel-k1-r2-20261026` | 2026-10-27 |

按 `AGENTS.md`，release PR 正文必须列出跑过的用例、命令和结果；本期没有逐个核对，**未核实**。

## 3. 阻塞与风险

| 项 | 说明 | 来源 |
|---|---|---|
| 跟踪文档本身可能再次失去维护 | 缓解：节奏放宽到双周 / release，PROGRESS §1 由脚本生成，旧文档文头明确降级 | 方案 §6.1 |
| agent24d 拆分时启动顺序有隐含依赖 | 第 2 期才会遇到；只移动不重排，以启动黑盒测试为闸 | 方案 §6.1 |
| 签名收紧后本地开发 CLI 可能 enroll 失败 | S1-2；先按 Q2 (a)，PR 内附「被拒」「通过」两条对照 | 方案 §6.1 |
| OS 包没有完整性校验 | Q15 已定要补，尚未实现；R2 评为高 | 方案 Q15 |
| 评审带宽 | 第 0 期 7 个文档 PR 同时进评审队列；`docs/architecture/README.md` 是多片共改文件，合并顺序影响冲突 | S0-6 的判断 |

## 4. 需要 jason 做的事

| 事项 | 说明 |
|---|---|
| 评审并合并第 0 期的文档 PR | 本地 PR-Daemon 评审；S0-5 在分支 `docs/p0-layers-regen`，S0-6 在分支 `docs/p0-tracking-first-issue`，均尚未开 PR |
| 在 ARCH-OPTIMIZATION-PLAN.md 文首进度段追加「第 0 期 Sx-y 已由 PR #NNNN 完成」 | 方案原文只允许这一处追加，PR 号开 PR 后才知道 |
| 无其他待拍板问题 | 见 [ARCH-AND-PLAN](ARCH-AND-PLAN.md) §5 |

## 5. 下期计划

只写任务 id，定义在 [MILESTONES-AND-TASKS](MILESTONES-AND-TASKS.md)：

- 收尾第 0 期：S0-1 至 S0-7 合入；S0-8 视评审带宽。
- 起第 1 期可并行的片：S1-1、S1-3、S1-4、S1-5（最多 3 个 Grok 并发），S1-2、S1-7 在其后。
- 第 2 期：S2-1 开工前要求 S0-5 已合入。
