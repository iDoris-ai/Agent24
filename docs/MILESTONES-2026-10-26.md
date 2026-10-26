# Agent24 里程碑、任务拆分与执行计划（2026-10-26）

> **地位**：自 2026-10-26 起，这是里程碑与任务的**唯一跟踪文档**。架构与技术方案见 [ARCHITECTURE-2026-10-26](ARCHITECTURE-2026-10-26.md)，实际进展见 [PROGRESS-2026-10-26](PROGRESS-2026-10-26.md)。
>
> **取代**：`docs/agent/tasks.md`、`roadmap.md`、`COMPONENT-ROADMAP.md`、`docs/PLAN.md`、`docs/ROADMAP.md`、`docs/specs/TASKS.md`、`docs/Deployment/TASKS.md` 的里程碑部分，以及各集成分支上 `PLAN-*` 的总排期部分。各线的**细粒度任务契约**仍留在原计划文档里，本文第 4 节逐条指向，例如 `PLAN-MEMORY-P1.md` 的 P1-01..11、`PLAN-KNOWLEDGE-TASKS.md`。旧文档的正文不删，逐份状态见附录 [SOURCE-C2](tracking/2026-10-26/SOURCE-C2-plans-milestones.md)。
>
> **依据**：
> - 架构文档。
> - ADR-037（🟡 待评审）。依赖它的里程碑标「⏳ADR-037」，ADR 批准前只做设计，不派生产实现。
> - 2026-10-26 对 git 的核实，见附录。

---

## 0. 排序原则

1. **先把已经做完的发出去，再开新战线。** v0.5.1（10-01）之后，main 已前进 1300 多个提交没发正式版；还有大量成果堆在 9 条集成分支上（kernel 10-16 后合了 40 个 PR、weknora 49 个、documenting 25 个）。积压越久，吸收 main 的冲突越大（M10 教训）。
2. **本体化（ADR-037）是新的主线。** 开发者 API、TUI 对齐、移动遥控、Win/Linux 都依赖「常驻 daemon + 分权」。在它之前，不对外宣传开发者 API。
3. **记忆 P1 照常推进**，它是已拍板的产品主干，不被本体化打断。
4. **暂停的线保持暂停**，除非 jason 在第 6 节拍板恢复。
5. 每个里程碑都分别记录四种状态：组件落地、产品路径接线、验收、发行。四者不得互相代替。

---

## 1. 统一编号

| 前缀 | 线 | 旧编号映射 |
|---|---|---|
| REL | 发布与收口 | DEP-A*/B*、release PR、v0.5.x |
| CORE | 本体化（ADR-037） | 新增；吸收 OD-M11 F11.0/F11.1 的 daemon 所有权部分、DEP-A5 |
| API | 开发者 API 与令牌 | 新增；吸收 K-3（OpenAPI 生成）、OD F11.0 路由级授权 |
| TUI | TUI 能力对齐 | 新增 |
| MEM | 记忆 | C3 P0（M1，已完成）、M2、P1-01..11、R01..R06、H01..H03、G1..G5、4a/4b、P2..P5 |
| KRN | 内核 K1 | K1-1..7、ADR-K1-01..04、K1-5.x/6a.x/6b.x/7.x、#735、#708 |
| DOC | Documenting | DOC-1..4、ADR-DOC-01..03（不变） |
| KB | 知识（WeKnora） | KB-C0..C4、KB-CT/C1/HOST/SEC/LITE…（不变） |
| COMM | 通信（Hyphae） | COMM-0..7、T01-E、DEP-C8/C9、DEP-HN0..6 |
| DEC | 决策服务 | D0..D4、D1-1..6 |
| IDR | iDoris 接入 | IDORIS-01..12、ID-1..6、ADR-032 P3..P5 |
| HW | 硬件分档 | HW-0..5（不变） |
| PLT | 平台扩展 | DEP-C0..C7、WINDOWS-PORT、移动端（DEP-C4..C6 被 ADR-037 改写） |
| OD / OA | Open Design / Office Assistant | OD-M11/M12、TPI-W、OC；OA-M1..M3、OA-K |
| DOCS | 文档治理 | 新增 |

编号撞车的完整对照见 [SOURCE-C2 §4.3](tracking/2026-10-26/SOURCE-C2-plans-milestones.md)。今后新任务一律用上表前缀。

---

## 2. 里程碑总览

| 里程碑 | 目标 | 出口条件 | 依赖 | 状态 |
|---|---|---|---|---|
| **M-REL-1 收口发布** | 集成分支积压按线发到 main，出 **v0.6.0 Mac 正式版** | 各线 release PR 由人开人审并真机验收；v0.6.0 带签名加公证的 arm64 dmg，CLI 也签名；CHANGELOG 补齐 | 各线 open PR 收尾 | 未开始 |
| **M-CORE-1 本体化（Mac）** | 每用户常驻 daemon 成为产品默认；一方外壳经登记握手接入 | ADR-037 决策 2、3 在 Mac 上落地，含生命周期表、版本协商、迁移备份回滚；桌面退出后日程照跑；TUI 接入同一个 daemon | ⏳ADR-037 | 设计中 |
| **M-API-1 开发者 API（本机）** | 第三方程序可用分权令牌调用本机 API | G-API-1..5 全过；令牌管理 CLI 和 UI；开发者文档；版本与弃用策略 | M-CORE-1 | 未开始 |
| **M-TUI-1 TUI 能力对齐** | TUI/CLI 覆盖全部对外能力，视觉功能给兜底 | OpenAPI 覆盖 CI 不降；豁免表经评审；有聊天输入和主要视图 | M-CORE-1（接入方式） | 未开始 |
| **M-MEM-P1 记忆可控** | 按 PLAN-MEMORY-P1：容量、附件、导出导入、更正、purge、无痕、回执 | P1-11 联合验收；release 由人开 | 两份 ADR 已接受（#1247、#1248） | 进行中 |
| **M-KRN-2 K1 第二批发布** | 授予路径、出站统一、出口矩阵进 main | ab/kernel 的 open PR 收尾；release PR；#708 真实路径验收 | — | 进行中（未发布） |
| **M-DOC-1 DOC-1 验收** | Documenting 聊天入口打通，DOC-1 整体验收 | chat 入口（#1142）；r6；真机 | M-KRN-2 | 进行中 |
| **M-KB-1 知识进入主对话** | KB-C1 打通；真实私有数据仍禁用 | KB-C1-04/05；KB-SEC-01b；首次 release | M-KRN-2 | 进行中 |
| **M-PLT-LNX Linux** | systemd `--user`；运行时 Ollama 或 llama.cpp；Secret Service；hw 探测 | 干净机验收 | M-CORE-1 | 未开始 |
| **M-PLT-WIN Windows** | 按 WINDOWS-PORT v2 与 ADR-037 落地 | 发布矩阵加入 Windows；干净机验收 | M-PLT-LNX；Windows 签名资格 | 阻塞 |
| **M-PLT-MOB 移动遥控** | 经 Hyphae 遥控：聊天、审批、状态、预览 | `REMOTE-CONTROL-HYPHAE.md` 回答 ADR-037 列出的必答题后获批；实现与安全验收 | M-API-1 的令牌模型；COMM | 未开始 |
| **M-DOCS-1 文档治理** | 三份文档成为唯一入口；旧文档标状态；外部链接更新 | 见 §4 DOCS | — | 进行中 |

建议节奏：

- **11 月上旬**：M-REL-1 与 ADR-037 评审并行。
- **之后**：M-CORE-1，接着 M-API-1 与 M-TUI-1。
- **M-MEM-P1** 全程并行。
- **Linux**：M-CORE-1 之后开始。
- **Windows、移动端**：排在 v0.7 之后。

日期只作参考，不承诺。

---

## 3. 执行规则（汇总现行约定）

规则来源：AGENTS.md、PLAN-MEMORY-P1 §5、HANDOFF 硬门、用户偏好。完整清单见 [SOURCE-C2 §6](tracking/2026-10-26/SOURCE-C2-plans-milestones.md)。

**分支**
- 每个方向一条 `ab/<方向>` 集成分支。task 分支命名 `ab/<方向>-NN-<短名>`，PR base 只能是本方向的集成分支。
- 约 10 个 task 开一次 release PR，由人开、人审、真机验收。
- 集成分支每周至少 merge 一次 main，不用 rebase。

**PR 大小**
- task PR 不超过 300 行，锁文件和生成文件不计。测试和文档是否计入，见 §6 待拍板第 1 条。

**评审与合并**
- PR-Daemon（clestons）对**精确 head** APPROVE，加上六项 required CI 全绿、`mergeable` 为真，才能合入。PR 被批准后不再追加 commit。
- PR 挂着 `pr-daemon:reviewing` 标签时不要推送。
- 开 PR 前跑 `pre-pr-check`，命中项在 PR 正文里逐条回应。

**分工**
- 规划、设计、验收：Opus 协调者。
- 开发：Sonnet 子代理，同时最多 3 个写码。
- 挑战评审：用 Mac mini 上的 Codex，以只读方式运行，不在 mini 上构建。
- 其他在 Mac mini 上执行的任务都要先经 jason 允许。

**代码与验证**
- 先写测试，新的回归测试做变异验证。
- 判据先用正对照验证过再用。
- 带状态机的改动先写一页语义说明，评审通过后再写代码。
- 所有 worktree 共用 `CARGO_TARGET_DIR=$HOME/Dev/auraai/Agent24/rust/target`。

**提交与门禁**
- commit 作者设为 `jhfnetboy <jhfnetboy@gmail.com>`，AI 只出现在 `Co-Authored-By` 行。
- 改法律单独开 PR。改变可观察行为的 PR 要点名受影响的法律。
- 设计最多一轮评审就冻结；出现 High 才复审；评审只修真 bug。

---

## 4. 各线任务与下一步

每条线给出：目标、当前位置、下一步、门。细粒度任务契约以「权威计划」一列为准。

### REL 发布与收口（M-REL-1）
| ID | 任务 | 门 |
|---|---|---|
| REL-1 | 各集成分支吸收 main（kernel、comm、decide、idoris、maint、office-assistant 已超过一周没吸收） | 每条分支一个 sync PR |
| REL-2 | K1 第二批 release PR（见 KRN） | 真机 |
| REL-3 | Documenting r6 release（见 DOC） | 真机 |
| REL-4 | iDoris v0.3.1：IDORIS-07 发布证据包，加上 R9 三项 owner 决策 | jason 决策 |
| REL-5 | COMM：5b、6b.1、6b.2 拆片带入 main；Hyphae lock 以 main 为准 | — |
| REL-6 | DEC：D1 L1/L2 发布到 main，或明确继续停在分支上（§6） | jason 决策 |
| REL-7 | 第二档分支里有价值的小件：`repro/1020` 报告进 main；MAINT-01 token 帧校验 cherry-pick；documents WAL 竞态测试修复；Linux 硬件测试 | 各自一个小 PR |
| REL-8 | v0.6.0 Mac 正式版：签名加公证的 dmg，CLI 和模块包签名（DEP-B3），CHANGELOG 补齐 v0.5.1 之后的内容，发布清单冻结到文件名 | jason 真机 |

### CORE 本体化（M-CORE-1，⏳ADR-037）
CORE-1 平台数据目录统一解析 → CORE-2 服务模式 `--enroll-endpoint` 与 macOS 登记握手（代码签名校验）→ CORE-3 桌面安装 LaunchAgent、退出不停 daemon、托盘「完全退出」→ CORE-4 `/health` 版本协商，`tryReuseExternal` 补验版本 → CORE-5 迁移前自动备份与失败回滚 → CORE-6 TUI 和 CLI 接入常驻 daemon，取消默认的 ephemeral → CORE-7 渲染进程隔离（`backendProxy` 改白名单，或签发页面令牌）→ CORE-8 `node-daemon` 移出安装包 → CORE-9 oMLX 管理从桌面迁回 daemon / iDoris → CORE-10 按子进程模式退路的定位，调整 KB-HOST-01 #1184 与 OD F11.1。
门：每片单独开 PR；CORE-2 和 CORE-7 属于安全类，需 PR-Daemon APPROVE 并配负向测试。

### API 开发者 API（M-API-1）
API-1 G-API-1 路由级资源授权 → API-2 G-API-2 Host/Origin 校验 → API-3 G-API-3 OpenAPI 一致性 CI（K-3 生成可后置）→ API-4 G-API-4 事件流按令牌过滤 → API-5 G-API-5 令牌治理 → API-6 令牌签发与管理 CLI 和 UI → API-7 开发者文档、版本与弃用策略、示例。全部完成之前不对外宣传。

### TUI 能力对齐（M-TUI-1）
TUI-1 覆盖 CI 与豁免表（现约 26/75）→ TUI-2 聊天输入 → TUI-3 记忆、日程、模块与 consent、通信、文档视图 → TUI-4 恢复诊断模式（桌面不可用时拉起并诊断 daemon）→ TUI-5 i18n（与桌面的英/中/泰一致）。与桌面共用 `@agent24/api-client` 的类型。

### MEM 记忆（M-MEM-P1）——权威计划 `PLAN-MEMORY-P1.md`（ab/memory-p1）
- **已合入 ab/memory-p1**：
  - L01/L02 证据与 main 同步；
  - H02 审计幂等；
  - R03 回归；
  - R05 来源契约：表、快照、D7/D7f、拒发门、恢复门、G3；
  - 两份 ADR 已接受：生命周期 #1247，R05 #1248；
  - P1-02a/b 容量探针。
- **进行中**：4a HostSystem 来源（迁移 0023）、4b 发送副本按来源 kind 整形（修掉最初的 P02 缺陷）。
- **下一步顺序**（按计划原序）：P1-02c F7 保留/压实 → P1-03 用量 API 与告警 → P1-04a/b 附件与冷归档 → P1-05 导出导入 → P1-06 更正与双时态（F11）→ P1-07 偏好 → P1-08a/b/c purge（全个人域）→ P1-09 无痕（零工具、单一本地 provider）→ P1-10 回执 UI → P1-11 联合验收。
- **推迟门**（各自触发时单开）：G1、G2、G4、G5、K1 同事务、冷恢复矩阵（见 R05-4）。
- **计划勾选**：R02/R05 与 R06-H03 待 4a/4b 合入后，另开一个 PR 核销。
- **之后**：P2（向量加 RRF、LLM 抽取）→ P3 共享空间 → P4/P5。

### KRN 内核 K1（M-KRN-2）——权威 `PLAN-KERNEL-K1.md` 与 ab/kernel 的 `design/K1-06*`
- **收尾**：#1243、#1244、#1239；#1183 改法律，要 jason 确认。
- **之后**：release PR，带上 ADR-K1-02 §2.1.1。
- **验收**：#708 macOS 真实路径验收；L-PRV 隐私法律单独开 PR。
- **台账**：tasks.md 里的 K1 台账作废，以本节为准。

### DOC Documenting（M-DOC-1）——权威 `docs/documenting/README.md`（ab/documenting，负责人 David）
#1240 收尾 → r6 release → chat 入口（DOC-1-06b / #1142，依赖 M-KRN-2）→ DOC-1 验收 → slice 2 / DOC-2..4。

### KB 知识（M-KB-1）——权威 `docs/knowledge/PLAN-KNOWLEDGE-TASKS.md`（ab/weknora）
#1241、#1242 → KB-C1-04/05 收尾 → KB-SEC-01b（与 K1-6b.5 出口矩阵合并验收）→ 首次 release。KB-C2..C4、LITE 留在 BACKLOG。KB-HOST-01 的 daemon 所有权按 CORE-10 调整。

### COMM 通信（Hyphae）——权威 `design/COMM-HYPHAE.md`、`PLAN-COMM6b.md`
REL-5 → DEP-C8 补齐四个平台的锁与 relay 锁（#751）→ COMM-6b.3/6b.4 → Hyphae 群聊界面（Hyphae 侧已给出 `groupchat --json` 契约）→ COMM-7 → DEP-C9、HN1..6。遥控通道归 PLT-MOB，不放宽 zero-run。

### DEC 决策服务——权威 `PLAN-DECIDE*.md`（ab/decide）
D0 已合 main，D0-6 还差 24GB 复测。D1-1、D1-2 L1/L2 在分支上；L3 只有契约。D1-3..6 是否恢复、何时恢复，见 §6。PLAN-DECIDE 里的五档硬件表已被 HW-TIERS 的 1–6 数字档取代，待确认。

### IDR iDoris——权威 iDoris 仓库的边界规范 1.2 与 `PLAN-IDORIS-V031.md`（ab/idoris）
REL-4 → ID-1 入口路由生成 TaskProfile → ADR-032 P3 流式 / P4 provider 接线 / P5。出境脱敏要等 L-PRV 法律。

### HW 硬件——权威 `design/HW-TIERS-*`、ADR-036
HW-2 能力梯 → HW-3 `/api/v1/hardware` 与首次运行页 → HW-4 唯一准入点加云端开关（ModelRouter 强制）→ HW-5 实测回填。

### PLT 平台
- **Linux**（M-PLT-LNX）：systemd `--user`、Ollama 或 llama.cpp 后端、Secret Service、agent24-hw 的 Linux 分支。
- **Windows**（M-PLT-WIN）：先定 DEP-C1 签名资格；之后按 WINDOWS-PORT v2 做命名管道、计划任务、ACL、keyring 的 Windows feature；发布矩阵加入 Windows。
- **移动遥控**（M-PLT-MOB）：先写 `REMOTE-CONTROL-HYPHAE.md`，回答 ADR-037 列出的必答题后再实施；取代 DEP-C4/C5 中的 PWA 加 tailscale 方案。
- **自动更新**（DEP-C0）：并入 CORE-5 的升级路径。

### OD / OA（暂停）
- **OD-M11/M12、TPI-W、OC**：保持暂停（jason 2026-10-03）。其中 daemon 所有权和路由级授权两部分已移进 CORE 和 API。
- **OA**：口径仍是暂停，但分支上有 4 个 open PR（泰语文案、样本），见 §6。

### DOCS 文档治理（M-DOCS-1）
| ID | 任务 |
|---|---|
| DOCS-1 | 三份文档及附录入库（本 PR） |
| DOCS-2 | 修正与代码不符的文档：ARCHITECTURE-LAYERS、README 架构部分、decision.md 中 ADR 的状态、laws/APPROVAL L-APPR-5/8（单独的法律 PR）、ADR-K1-03 链接 |
| DOCS-3 | 旧计划、进度文档的文件头加「已由 2026-10-26 三份文档取代，见…」，正文不动；建立「不变量 → 设计稿 → 测试」索引 |
| DOCS-4 | 外部仓库链接更新清单交给各仓：Brood INTERFACES/PROFILE（被本仓 CLAUDE.md 导入）、Pet0 镜像、Sin90 README、iDoris-Components 同步、iDoris 消费方指针、WeKnora fork 文档 |
| DOCS-5 | AGENTS.md 吸收散落在各处的协作规则（HANDOFF-ORCA 硬规则、评审门、`--no-prune` fetch、author 规则） |
| DOCS-6 | 以后进度只更新 PROGRESS 文档；每次 release 后刷新一次，用附录里的探针命令核对 |

---

## 5. 第二档分支处置（2026-10-26 调查结论）

详见 [SOURCE-BRANCH-REVIEW-A](tracking/2026-10-26/SOURCE-BRANCH-REVIEW-A.md)、[SOURCE-BRANCH-REVIEW-B](tracking/2026-10-26/SOURCE-BRANCH-REVIEW-B.md)。

- **已存档后删除**：本地 25 条、远端 20 条。这些都已被后续合并取代，或属于一次性验证、备份快照。存档 bundle 放在仓库外的 `Agent24-archive/20261026/`，保存两周。
- **有价值，排入 REL-7**：`repro/1020-orphan-module`、`ab/maint`（MAINT-01）、`baton/…wal-race-r2`、`baton/…linux-hw-r2`。
- **保留，作为契约文档引用的参考**：`ab/weknora-03a/03b/03c` 三条顶端分支。
- **待决**：见 §6。

---

## 6. 待 jason 拍板

1. **全局评审门**：AGENTS.md 的分级门槛（普通 task 的 CI 全绿即可先合），与 PLAN-MEMORY-P1 写的「所有 PR 都须 PR-Daemon 对精确 head APPROVE」（2026-10-10）不一致。另外，PR 的 300 行是否计入测试和文档？结论写进 AGENTS.md。
2. **ADR-037**：评审结论（单独 PR）。
3. **v0.6.0**：是否把签名预发布这套流程升格为正式版；是否出 Intel 包。
4. **暂停线是否恢复**：DEC（D1-3..6）、IDR（R9 三项决策、v0.3.1 发布）、COMM 6b、OA（4 个 open PR 是合是关）、OD 继续暂停？
5. **PLAN-DECIDE 五档表**：是否正式由 HW-TIERS 1–6 档取代。
6. **第二档分支**：
   - `feat/desktop-macos-dmg`：CI 里还要不要 mac 包；
   - `feat/me4-cos72-skeleton`：T10 是否还在计划里；
   - weknora 参考分支：保留为分支，还是改成 tag；
   - `ab/kernel-22`：分支是 David 要求保留的，要不要删，需先问他。
7. **CLAUDE.md 导入 Brood 本机文件**：保留并推动 Brood 更新，还是改为不导入。
