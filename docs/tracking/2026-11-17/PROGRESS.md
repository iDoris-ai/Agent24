# 进度（2026-11-17）

索引：[架构总入口](../../architecture/README.md)・同期另两份：[ARCH-AND-PLAN](ARCH-AND-PLAN.md)、[MILESTONES-AND-TASKS](MILESTONES-AND-TASKS.md)

| 项 | 内容 |
|---|---|
| 快照日期 | 2026-11-17（第三期） |
| HEAD sha | `b7eef06f2b6cde7189eb90d9e0a4840c0b202f1f`（`origin/main`，Merge #1881） |
| 统计区间 | 上期 HEAD `c396478c9f8797c339096dbcdbf4d01b242d756f`（Merge #1780，2026-11-10）到 `b7eef06f2`，即 `c396478c..origin/main` |
| 数据来源 | `git log --first-parent --merges`（§1，经脚本）；`git tag` 与 `gh release view`（§2）；`gh pr list` / `gh pr view`（§1 后的对照）。没有核对 CI 和评审结论 |
| 生成方式 | §1 由 `python3 scripts/gen-tracking-merges.py --from-sha c396478c9f8797c339096dbcdbf4d01b242d756f --until origin/main` 生成，未手改。其余各节手写 |
| 节奏 | 每两周或每次 release 出一期（Q11）。下期用 `python3 scripts/gen-tracking-merges.py --from-sha b7eef06f2b6cde7189eb90d9e0a4840c0b202f1f` |

## 0. 本期一句话

七天（2026-11-10 快照之后到 2026-11-17）主线合入 71 个合并：第 2 期瘦身收完，X11 入站准入收口，C9 与技能加载器 v1 合入，向量召回接进对话，四个大文件拆开，macOS 模块沙箱和默认可选模型合入。预览版发了 `20261114.1` 和 `20261117.1`。`ab/ui` 上还有 5 个 PR 没进 main。

## 1. 本期合入的 PR

由脚本生成，未手改。日期是 git 合并提交的作者日期，和 `gh` 的 UTC 合并时间可能差一天。已核对的一例：#1811 的作者日期在表里是 2026-11-12，`gh pr view` 的 `mergedAt` 是 `2026-11-11T17:24:52Z`。

| PR | 日期 | 分支（前缀即方向） | 一句话（PR 标题） | 合并提交 |
|---|---|---|---|---|
| #1881 | 2026-11-17 | `fix/ms-1b-default-model-service-mode` | fix(agent24d): MS-1b —— 服务模式下桌面也能保存默认 chat 模型 | `b7eef06f2` |
| #1879 | 2026-11-17 | `feat/ms-1-default-chat-model` | feat(models): MS-1 —— 默认对话模型可选可保存，oMLX 密钥不再写死 | `83f5df8bf` |
| #1865 | 2026-11-17 | `feat/sbx-1b-module-tmpdir` | fix(os-proto): SBX-1b —— 每个模块用自己的临时目录，不再放行用户 TMPDIR | `0c5dc7ac2` |
| #1880 | 2026-11-17 | `fix/ms-2-startup-error-visible` | fix(desktop): MS-2 —— 后台起不来时显示原因，安装服务前先解除禁用 | `0c525c076` |
| #1873 | 2026-11-15 | `integrate/ui-63c1eda6` | chore(ui): ab/ui 63c1eda6 进 main —— 0.6.1 §3c 登记被拒 UX（#1871） | `de28cd24e` |
| #1872 | 2026-11-15 | `integrate/weknora-30616f6a` | chore(integrate): ab/weknora 30616f6a → main —— K1–K3（只改测试和注释） | `77031b53f` |
| #1864 | 2026-11-15 | `feat/sbx-2-isolation-status` | feat(components): SBX-2 —— 组件列表显示每个模块的隔离状态 | `e718d7b34` |
| #1863 | 2026-11-15 | `feat/sbx-1-macos-seatbelt` | feat(os-proto): SBX-1 —— macOS 上用 seatbelt 隔离 OS 模块 | `b4ad663ee` |
| #1858 | 2026-11-15 | `docs/module-sandbox-plan` | docs(tracking): 模块沙箱方案与分片 | `92a14b87b` |
| #1862 | 2026-11-15 | `refactor/split-4-server` | refactor(agent24d): SPLIT-4 —— 按职责拆分 server.rs 业务代码（纯移动） | `31fc8de43` |
| #1861 | 2026-11-15 | `refactor/split-3-agent-lib` | refactor(agent24-agent): SPLIT-3 —— 按职责拆开 lib.rs 业务代码（纯移动） | `348bb9609` |
| #1856 | 2026-11-15 | `feat/vr6-query-rewrite` | feat(memory): VR-6 —— 差一点的问题先改写成短检索语 | `cd20a5ca6` |
| #1860 | 2026-11-15 | `refactor/split-2-server-tests` | refactor(agent24d): SPLIT-2 —— server.rs 内联测试搬到 server/tests（纯移动） | `037775351` |
| #1859 | 2026-11-15 | `refactor/split-1-agent-tests` | refactor(agent24-agent): SPLIT-1 —— lib.rs 内联测试搬到独立文件（纯移动） | `a35c85c98` |
| #1857 | 2026-11-14 | `fix/od-first-start-timeout` | fix(desktop): 放宽 Open Design 首次启动就绪时限（冷启动超 15 秒） | `25c496daf` |
| #1854 | 2026-11-13 | `integrate/weknora-088cfd47` | 集成合并：ab/weknora（088cfd47）→ main（F1：Knowledge endpoint 按传输层规范串存储与显示） | `6a90c52f4` |
| #1852 | 2026-11-13 | `integrate/weknora-002f6c4d` | 集成合并：ab/weknora（002f6c4d）→ main（含 K-B6/K-B3 随 S2-1/S2-3 移植与 M5 B6 经 elevate） | `971805ae0` |
| #1849 | 2026-11-13 | `integrate/ui-4e6d70e5` | 集成合并：ab/ui（4e6d70e5）→ main（临时门禁方案 5，含合并引入的 oMLX 退出冲突 F1 修复） | `7cfe3ebb6` |
| #1851 | 2026-11-13 | `ab/vector-recall` | release: 向量召回接进对话循环（VR-1…VR-5） | `69271b050` |
| #1846 | 2026-11-13 | `docs/mainline-first-principle` | docs: AGENTS.md / CLAUDE.md 加最高原则「只推主干」 | `4124c402d` |
| #1843 | 2026-11-13 | `docs/vector-recall-plan` | docs(memory): 向量召回接进对话循环 —— 方案与分片 | `0734bc50b` |
| #1842 | 2026-11-13 | `feat/sl7-skill-cli` | feat(cli): SL-7 —— agent24 skill list/show/install/rescan/activate | `87c602d84` |
| #1841 | 2026-11-13 | `feat/c9-closeout` | fix(c9): C9 收口 —— 桥游标失败处理收紧、重签先落盘再撤旧、文档订正 | `ee5d6c360` |
| #1840 | 2026-11-13 | `feat/sl6-skill-bar` | feat(desktop): SL-6 —— 能力中心技能栏与会话技能选择 | `97be80317` |
| #1835 | 2026-11-13 | `feat/sl5-skill-inject` | feat(skill): SL-5 —— 技能准入、钉修订、注入与 SkillText 来源标签 | `466a92be3` |
| #1833 | 2026-11-13 | `feat/c9c-bridge-token` | feat(wechat-bridge): C9-C —— bridge-token CLI、桥只读令牌文件与去 legacy 入站 | `8044b691a` |
| #1836 | 2026-11-13 | `fix/sl3-followups` | fix(skill): SL-3 后续 —— 根目录坏条目、守卫测试、去重 id 占用、413 文案 | `87b4f52a7` |
| #1839 | 2026-11-13 | `fix/clippy-1-100` | fix(hw): 适配 Rust 1.100 clippy needless_bool（修复所有 PR 的 macOS CI） | `82d1566c8` |
| #1834 | 2026-11-12 | `feat/sl4-skill-activate` | feat(skill): SL-4 —— 技能激活与回滚 | `7ac2b186b` |
| #1832 | 2026-11-12 | `feat/sl3-skill-install` | feat(skill): SL-3 —— 技能导入 API 与用户目录扫描 | `4227c89f8` |
| #1831 | 2026-11-12 | `feat/c9d-components-grants` | feat(components): C9-D —— /components 展示挂载后的实际内核授予 | `ed86a29dc` |
| #1829 | 2026-11-12 | `feat/sl2-skill-store` | feat(store): SL-2b —— 技能修订存储与指针移动 | `f2409a2a8` |
| #1830 | 2026-11-12 | `feat/c9b-component-runtime` | feat(agent24d): C9-B —— ComponentRuntime 开发桥令牌、显式路由门与审计 | `612b64d1f` |
| #1828 | 2026-11-12 | `feat/sl2a-skill-migration` | feat(store): 技能加载器 SL-2 第 1 部分 —— 迁移 0036 技能修订、指针、激活与评测结果 | `d647327b4` |
| #1827 | 2026-11-12 | `feat/sl1-skill-parse` | feat(skill): 技能加载器 SL-1 —— 纯解析与文本一致性检查（新 crate agent24-skill） | `2f158dd8e` |
| #1826 | 2026-11-12 | `feat/x11-2h-inbound-closeout` | test(inbound): X11-2h —— 入站准入收口：防绕过测试、文档回写与遗留登记 | `b2b2ce636` |
| #1824 | 2026-11-12 | `docs/skill-loader-v1-plan` | docs(tracking): ADR-040 技能加载器 v1 实施方案（第 2 版，已拍板） | `04915289c` |
| #1821 | 2026-11-12 | `feat/x11-2e-hyphae-source` | feat(inbound): X11-2e 第 2 部分 —— Hyphae 入站适配（默认不拉取、按身份游标） | `a93f5df36` |
| #1825 | 2026-11-12 | `feat/c9a-requires-resolver` | feat(capabilities): ADR-038 C9 第 A 片 —— requires 声明与解析（三处授予行为不变） | `755961564` |
| #1823 | 2026-11-12 | `docs/s0-8-adr-closeout` | docs(adr): S0-8 —— ADR-KB-02 知识库宿主、ADR-046 iDoris 托管、ADR-035 部分取代 | `74b4e5465` |
| #1822 | 2026-11-12 | `docs/c9-requires-authz-plan` | docs: ADR-038 C9 requires 接入授权实施方案；ADR-038 状态订正与组件路线图回写 | `cd80b3a74` |
| #1820 | 2026-11-12 | `feat/x11-2e1-cursor-migration` | feat(store): X11-2e 第 1 部分 —— 迁移 0035 入站群聊游标表（按身份分开） | `bb03081e8` |
| #1819 | 2026-11-12 | `feat/x11-2g-wechat-bridge` | feat(wechat-bridge): X11-2g 第 2 部分 —— 微信桥改为只传输（不再直接起 run、去掉 y/n 审批） | `bc923205f` |
| #1818 | 2026-11-12 | `feat/x11-2g1-wechat-ingest` | feat(inbound): X11-2g 第 1 部分 —— 开发态微信入站事件路由与名单种子 | `0fc3c3607` |
| #1816 | 2026-11-12 | `feat/x11-2c-inbound-service` | feat(inbound): X11-2c 第 3 部分 —— InboundService、启动对账与入站接线闸门 | `5d5839c61` |
| #1813 | 2026-11-12 | `feat/x11-2c2-inbound-agent` | feat(agent): X11-2c 第 2 部分 —— 入站起 run、Unknown 标签，外部「记住」不写成用户断言 | `864bce4f1` |
| #1814 | 2026-11-11 | `ab/ui-cla-allowlist-main` | ci(cla): M6-C0-pre —— allowlist 加上已签过 CLA 的旧账号 stoptheworld-java（进 main，一次性例外） | `97b38b35e` |
| #1811 | 2026-11-12 | `feat/x11-2f-inbound-routes` | feat(inbound): X11-2f 第 2 部分 —— 入站名单、接受模式与事件列表 API（host-only 写） | `3753ddc79` |
| #1812 | 2026-11-11 | `feat/x11-2d-inbound-scrub` | feat(inbound): X11-2d 第 2 部分 —— 入站正文 180 天清理后台任务与接线 | `b6b87fc00` |
| #1810 | 2026-11-11 | `feat/x11-2c1-inbound-store` | feat(store): X11-2c 第 1 部分 —— 入站起 run 的事务、读取与终态 | `2be381d08` |
| #1808 | 2026-11-11 | `feat/x11-2d1-scrub-store` | feat(inbound): X11-2d 第 1 部分 —— 入站正文到期清理（store 与记忆层，连副本） | `8c5c6f223` |
| #1809 | 2026-11-11 | `feat/x11-2f1-inbound-store-api` | feat(store): X11-2f 第 1 部分 —— 入站名单、接受模式与事件页 store API | `acea53cad` |
| #1806 | 2026-11-11 | `feat/x11-2a-inbound-store` | feat(store): X11-2a 入站事件 store 层与日程内部记录 | `ace762be0` |
| #1805 | 2026-11-11 | `feat/x11-2a1-migration` | feat(store): X11-2a 迁移 0034 入站事件表 | `7c6050721` |
| #1803 | 2026-11-11 | `feat/x11-2b-admit` | feat(inbound): X11-2b 准入纯函数 admit 与策略版本哈希 | `c40e9ad4d` |
| #1800 | 2026-11-11 | `fix/fu114-omlx-stop` | fix(desktop): FU-114 停止 oMLX 只停本应用启动的进程，不再误杀终端里的 omlx | `81116879b` |
| #1802 | 2026-11-11 | `fix/fu118-119-attach` | test(attach): FU-118 锁住 attach 对 scheduler/approval 的拒绝；FU-119 文档警告 | `ba17d9060` |
| #1799 | 2026-11-11 | `fix/fu115-116-idoris` | fix(idoris): FU-115 注入的上游密钥生效；FU-116 外来 gateway 返回错误不 panic | `cf40fd905` |
| #1798 | 2026-11-11 | `chore/fu117-layer-check` | ci(layers): FU-117 依赖表漂移检查接进 CI；跟踪文档回写 S2-4b 已合 | `9d1660af8` |
| #1797 | 2026-11-11 | `docs/inbound-admission-plan` | docs(tracking): 入站准入（X11 ②③④）实施方案 | `113a53af4` |
| #1795 | 2026-11-10 | `docs/tracking-s2-status` | docs(tracking): 第 2 期 11-10 状态回写，登记 FU-113–119 | `2fa86db3b` |
| #1794 | 2026-11-10 | `refactor/s2-4b-attach-crate` | refactor(attach): S2-4b 新建 agent24-attach，搬入 attach 注册表/监听/计时桥（纯移动） | `378dbca6d` |
| #1792 | 2026-11-10 | `refactor/s2-3-knowledge-host` | refactor(knowledge): S2-3 Knowledge 宿主下沉到 agent24-knowledge（凭据类，provider 冻结） | `a8a749bd8` |
| #1793 | 2026-11-10 | `refactor/s2-4a-attach-seams` | refactor(agent24d): S2-4a attach 四文件与 daemon 内部解耦（行为不变） | `3e07baf3c` |
| #1791 | 2026-11-10 | `refactor/s2-1-split-server` | refactor(agent24d): S2-1 拆 server.rs——serve() 1536→267 行，中间件与 handler 移出 | `d76189a6e` |
| #1790 | 2026-11-10 | `refactor/s2-2-idoris-host` | refactor(idoris): S2-2 iDoris 托管下沉到 agent24-idoris（纯移动） | `a50963602` |
| #1789 | 2026-11-10 | `docs/s2-7-desktop-only` | docs(S2-7): Comm 私钥备份恢复与 Knowledge 绑定写为桌面专属例外 | `78ca91a7f` |
| #1788 | 2026-11-10 | `docs/s2-6-stop-semantics` | docs(S2-6): 降级为进程组停止语义盘点（不做统一原语） | `1b38dbcf2` |
| #1786 | 2026-11-10 | `feat/comm-contacts-relay` | feat(comm): COMM-6b 抢救——概览页整表替换 Relay + 手动探测（从 ab/comm 重做） | `bed5ef4c9` |
| #1785 | 2026-11-10 | `feat/x11-1-bridge-isolation` | feat(comm): X11 ① 微信桥产品隔离 + X7 删除旧 nostr-bridge | `31b916b97` |
| #1782 | 2026-11-10 | `docs/tracking-2026-11-10` | docs(tracking): 三份跟踪文档第二期（2026-11-10）+ ADR 索引 ADR-038 状态修正 | `ab7901da9` |

共 71 个合并（`--from-sha=c396478c9f8797c339096dbcdbf4d01b242d756f`）。

### 搜索对照（不改上表）

`GH_FORCE_TTY=0 gh pr list --state merged --search "merged:>=2026-11-10" --limit 200` 返回 106 条。其中 `baseRefName=main` 的 71 条，和上表不是同一集合：

- 搜索多了 **#1780**。它就是上期 HEAD，合并时间在 2026-11-10，不在 `c396478c..origin/main` 里。
- 搜索少了 **#1811**。`gh pr view 1811`：MERGED，`mergedAt` `2026-11-11T17:24:52Z`，base `main`，merge `3753ddc79`。再搜 `1811 merged:>=2026-11-10` 得到空列表。原因未核实。上表按 git 保留了它。

另外 35 条的 base 不是 main（`ab/ui` 19、`ab/weknora` 11、`ab/vector-recall` 5）。用 `git merge-base --is-ancestor <该 PR 的 merge sha> origin/main` 核对：

已在 main 里（随后来的集成或发布合并进来）：#1759、#1777、#1778、#1779、#1781、#1783、#1784、#1787、#1796、#1804、#1807、#1815（以上 `ab/ui`）；#1762、#1763、#1817、#1837、#1838、#1853、#1855、#1866、#1868、#1869、#1870（以上 `ab/weknora`）；#1867、#1871（`ab/ui`）；#1844、#1845、#1847、#1848、#1850（`ab/vector-recall`，随 #1851）。

**不在 main 里**：#1874、#1875、#1876、#1877、#1878，都在 `ab/ui`。#1877 是设置里填本地 OpenAI 兼容服务，#1878 是在对话里问这份文档。

## 2. 发布

| tag | 日期 | 指向 | 说明 |
|---|---|---|---|
| `v0.6.0-preview.20261117.1` | 2026-11-17 | `b7eef06f2`（#1881） | 预发布。`gh release view`：距 20261114.1 合入 17 个 PR。默认模型、启动失败原因、问句改写、macOS 模块沙箱、拆大文件、Design 首启 60 秒。已知问题：Sin90 页是占位、不随包装；按内存推荐等 iDoris #602 |
| `v0.6.0-preview.20261114.1` | 2026-11-14 | `6a90c52f4`（#1854） | 预发布。正文写 main@6a90c52f，与 tag 一致。语义召回、技能栏与 CLI、知识库与主对话、Hyphae v0.30.1、C1–C9、OS 包完整性。已知问题里的 Design 冷启动超时，由下一版的 #1857 收掉 |

两个 release 都是 macOS arm64，正文写已签名并公证。公证票据本期没有另验。

上期写预览版自 2026-11-04 停发。本期这两个 tag 是停发之后的新发布。谁批准恢复，仓库文档里没找到；ADR-044 第 173 行仍写停发。

区间内直接合进 main 的 integrate / release 合并：#1873、#1872、#1854、#1852、#1849、#1851、#1814。用例验收在各 PR 正文里，本期没有逐份复跑，**未核实**。

外仓 Sin90 v0.5.2、Cos72 v0.1.2 沿用上期，本期没有重跑 `gh release view`。

## 3. 阻塞与风险

| 项 | 说明 | 来源 |
|---|---|---|
| 模型推荐接不进来 | iDoris #602、#600 都是 OPEN。本仓只能手动选默认模型 | `gh pr view`；release 20261117.1 |
| Sin90 仍是占位 | 侧边栏不反映安装状态，安装包也不带 Sin90。按需安装还在本地工作区，没提交 | release 20261117.1；ARCH §5 |
| `ab/ui` 又领先 15 个提交 | 其中 #1874–#1878 确认不在 main | MILESTONES §1 |
| `ab/comm`、`ab/decide` 没了 | 上期要盘点存档。现在分支 404，存档没找到 | GitHub API |
| 旧集成分支继续落后 | `ab/kernel` 落后 1409，`ab/maint` 落后 3391，`ab/office-assistant` 落后 2957。内容未读 | 文末命令 |
| 预览版停发写着，发布已经恢复 | 两处文档还停在 2026-11-04 的裁决 | ADR-044:173；上期 §3 |
| followups 仍多 | open `FU-` 85。本期关掉 7 条，新开 12 条。分批清理还没写 | MILESTONES §4 |
| 沙箱不覆盖 Linux，也不宣称隔绝 mach | 方案自己写了边界。只读且超深的临时目录树不处理 | 沙箱方案 §5 与文末 |
| 技能片没走集成分支 | 方案写 `ab/skill`，实际直接进 main | 技能方案 §5；本期 `ab/` 列表 |

## 4. 需要 jason 做的事

| 事项 | 说明 |
|---|---|
| 看这期三份文档 | 本任务只 commit，不开 PR、不 push |
| 预览版停发要不要改口 | 发布已经有两版。ADR-044 那句还没改。本期没有代改 |
| iDoris #602 / #600 | 不在本仓。推荐模型要等它们 |
| 无新的产品拍板卡在 Agent24 main 上 | 沙箱、C9、只推主干都已写进已合入的文档 |

## 5. 下期计划

定义在 [MILESTONES-AND-TASKS](MILESTONES-AND-TASKS.md) §2.8：

- OSI-1：Sin90 按需安装（本地 `feat/sin90-dynamic-install`，未提交）。
- 第三方模块同意卡：先写方案。
- followups 分批清理（现在 85 条 open `FU-`）。
- iDoris §2.9 模型推荐：等 #602 与 #600。
- S0-8 的计划范围已经合入（#1823）。`docs/s0-8-adr-cleanup` 没有提交。
- 好友机制不排进 Agent24。
- `ab/ui` 的 #1874–#1878 还在集成分支上。
- 不在本轮：SBX-3、SBX-4、手机遥控、ADR-041b、自动回发。
