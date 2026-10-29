# Changelog

All notable changes to Agent24 are documented here. This project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

- **内核迁移 0031、0032（升级后不能回退）**：升级到带这两条 store 迁移的版本以后，之前的版本会拒绝打开这个库
  （`migration 31 was previously applied but is missing`），库本身不动。要回退，只能恢复升级前的备份，光换回旧二进制不行，
  所以升级前请先备份 `~/.agent24`。升级前就停在审批上、升级后才批准的 run 不再恢复，会以 `source_unproven` 结束，
  因为旧版没有给它的工具结果打来源标签。

- **kernel 迁移改号（开发机注意）**：`ab/kernel` 在吸收 main（#1433）时，为避开 memory-p1 已占用的 0027–0029、
  以及日程索引 0030，把自己的两条 store 迁移从 `0027_session_tool_result_origins`、`0028_backfill_session_floor`
  改为 `0031_session_tool_result_origins`、`0032_backfill_session_floor`，内容不变。跑过 #1433 之前 `ab/kernel`
  开发版的机器，store 库里记着旧编号，新二进制会按「已记录的未知迁移」拒绝打开；**请备份后重置本地开发库**
  （`~/.agent24` 另存后删除 `agent24.db*`）。正式发布版本与 main 上的预览版不受影响。

- **iDoris v0.4.0 余量确认（行为变化提示）**：设置了 `IDORIS_ALLOCATED_BUDGET_BYTES` 且额度紧张时，以前能直接加载的模型可能返回 503 `headroom_confirmation_required`，需要用户确认后带 `X-iDoris-Headroom-Breach: confirmed` 重发。Agent24 的确认交互登记为 ID-HR，在此之前这类请求会显示为 iDoris 不可用。

- **iDoris 契约版本检查放宽为 1.x 且 ≥1.0.1**（`agent24-models`）：原先要求 `contract_version` 恰为 `1.0.1`，现解析为三个整数，接受 major==1 且 ≥1.0.1，为 iDoris v0.4.0（契约 1.1.0，仅在 `/capabilities` 加可选字段）做准备；major≠1、低于 1.0.1、解析失败与预发布后缀仍拒绝，错误消息带收到的版本与期望范围（不含 key）。
- **iDoris v0.3.2 的 `502 upstream_auth_failed` 映射为终态（ID-ERR-1）**：`IdorisClient::send()` 对 502
  可取消地读至多 4 KiB body，仅看 `error.reason_code`/`error.type`；等于 `upstream_auth_failed` 时返回
  `ModelError::Rejected { status: 502 }`（不重试、不换 provider），文案指向 iDoris 运营方的上游密钥或 provider
  权限，不回显 body、不带任何 key；其它 502 仍为可重试的 `Unavailable`。503/502 共用同一个读 body 的 helper。

- **DOC-1 第 1 片骨架（Documenting，`docs/documenting/`，ADR-DOC-01 / ADR-DOC-02）**：新增
  `agent24-documents` crate —— 第一方进程外 domain OS `documents`（`domain-os.yml`：
  `events` + `models`，`model_access: local_only`，不申请 `memory`），目前只有
  `GET /api/v1/documents/capabilities`，所有第 1 片操作都报不可用并给出类型化原因。
  **不随安装包分发、不默认安装**，开发期用 `agent24 os install` 手动安装；版本号 `0.1.0`
  是这个 OS 自己的起始版本，不代表一次发布。

- **D0-1（决策服务，`docs/agent/PLAN-DECIDE.md`，ADR-033）**：新增 `agent24-decide` crate
  —— `DecisionRequest`/`Question`/`Decision` 等类型、`DecisionBackend` trait、`RuleBackend`、
  级联 `DecisionService`。只是接口与一个确定性规则后端，**不接入 `agent24d`，不改任何现有
  调用点的行为**；版本号 `0.5.1` 只是跟随当前 workspace 其它 crate 的统一版本号起步，本身
  不代表一次发布。

## [0.6.0-preview.20261028.1] — 2026-10-28（预发布）

**预发布（pre-release）体验版，不是 0.6.0 正式版。** 一次带入五条里程碑线，按合入 main 的顺序：
kernel（K1 第二次发布）、memory-p1、documenting、weknora（Knowledge）、idoris（iDoris v0.3.1 接线）；另有发布前加固 #1358/#1364 和日程 tick 索引 P-13 #1381。清单里的版本号统一
升到 `0.6.0`，`-preview.20261028.1` 只出现在 tag 和发布物文件名里（与 `v0.5.1-preview.20261016.2`
同做法）。`agent24-os-sdk`、`agent24-os-fd`、`agent24-os-cwd`、`agent24-workspace`、
`agent24-sidecar-host*`、`agent24-documents` 仍是各自独立的 `0.1.0` 版本线。

**数据库迁移（升级前先复制 `~/.agent24`；这些迁移都不可回退）**

| 来源 | 库 | 迁移 |
|---|---|---|
| kernel（#1280） | memory | `0018_assertion_source_ref`（`mem_assertions` 只加一列 `source_ref`，旧断言为 NULL，读作 Unknown） |
| kernel（#1280） | store | `0019_module_consent_scope` … `0026_backfill_self_wake_origin`（共 8 个） |
| documenting | documents（`agent24-documents` 自带库） | `0007_job_acceptances`、`0008_job_revocations` |
| weknora | knowledge（新库 `knowledge.db`，目录 `rust/apps/agent24d/knowledge_migrations/`） | `0001_log`（Provider 日志与 seal，#1336）；新库，不影响既有库 |
| memory-p1（#1341） | memory | `0019_retract_quota_exempt`、`0020_write_rejections` |
| memory-p1（#1341） | store | `0027_run_message_origins`、`0028_run_message_origin_kinds`、`0029_run_message_origin_host_system` |
| main（P-13，#1381） | store | `0030_schedules_tick_index`（日程 tick 的两个部分索引，只建索引不改数据） |

合入后迁移目录连续：store `0001–0030`，memory `0001–0020`，documents `0001–0008`。

**升级注意：开发机需要重置本地库的情形**
- 跑过 **memory-p1 改号（#1298）之前** 的 `ab/memory-p1` 开发版的机器，库里记着旧编号
  （memory `0018`、store `0021–0023`），与本次发布的同号迁移内容不同。**store 库不一致时启动直接失败**；**memory 库不一致时 daemon 照常启动，但记忆被关闭，只在日志里留一条 WARN**，界面上表现为「记忆不可用」，不会弹明确报错（10-27 验收 X-04 实测）。
  **重置本地库**（或用 `AGENT24_DATA_DIR` 隔开）。
- 跑过 `ab/kernel` 开发版中间提交（`cf8ccd48` 到 `2d078753` 之间）的库同理（K1 矩阵 §6 第 4 条，
  David 决定不做兼容，清库）。
- 从 `v0.5.1` 或 `v0.5.1-preview.20261016.2` 升上来的库正常迁移，无需重置。

**本版已知缺口（按验收第 7 节逐条处置；这些能力本版不对用户宣称）**
- **模块工具授予（kernel，G-K1）**：授予 POST 默认关闭（要设 `A24_MODULE_TOOL_GRANTS=1`），桌面和 CLI 都没有授予入口，仓库里也没有协议 4 的示例模块。相关行为只有 CI 测试覆盖。
- **run 路径的记忆回执（memory，G-M5）**：配额满时 run 照常 `completed`，桌面和 TUI 都不提示记忆没写进去（P1-03，下一期）。
- **普通轮次记忆写失败（memory，G-M1）**：桌面 Chat 不显示「本轮未保存到记忆」。
- **文档的对话入口（documenting，G-D1/G-D2）**：七个文档工具已实现，但 `domain-os.yml` 还没声明 `tools:`（协议仍为 2），agent 调不到；文档导入、操作、预览经 CLI 和桌面照常可用。

### kernel —— K1 内核第二次发布（#1280；上次进 main 是 #1022，2026-10-16）

固定在 `release/kernel-k1-r2-20261026`（`27acdd72`，#1278 之后）。#1277、K1-6b.5f 系列不在本次。

**新功能**
- **K1-6a-G 模块工具的内核授予路径**（设计 #1121、#1125、#1202）：授予带资源集合、制品身份与
  写入代次（#1141、#1143、#1144）；模块包有制品身份（#1133、#1138）；启用纪元，重新启用后旧授予判
  Stale（#1155、#1157）；逐次审批绑定 `authorization_ref`（#1140）；撤销、到期、换发、拒绝时停掉在途
  调用（#1134、#1139）；撤销和到期会通知模块并记录它是否停下（#1182、#1190、#1191）；结束通知带
  引用（发件箱、补发、只发给本次运行：#1222、#1227、#1231、#1236）；确认卡片与授予 POST
  （#1160、#1172、#1176）；收尾修复 #1237、#1239。
- **协议**：manifest 的 `resource_kinds` 为协议 3，按引用停为协议 4，`DAEMON_PROTOCOL_VERSION = 4`
  （#1132、#1227）。协议 1、2 的模块照常连接；只有声明协议 4 的模块才能拿到授予。
- **K1-6b 来源分类与出口门**：统一出站判定（#783）、记忆来源引用与独立敏感分类（#874、#1199、
  #1200）、工具出口按来源分类判定（#1180）、会话来源下限（#1203）、self-wake 带上创建它的那次运行
  的分类（#1204、#1278）、peer 视图只放行自成一体的 run（#1243）、MCP server 与 ACP 桥改读 peer
  视图（#1266）、逐出口矩阵与测试（#1209、#1216、#1220、#1235、#1244）。
- **根外写入**：取定目录时任何一级都不跟随符号链接，恢复路径重新询问（#1270、#1274）。
- **法律与设计文档**：L-APPR-7 就地修订（#1183，jason 2026-10-24 同意）；ADR-K1-01..04 已接受
  （#766）；ADR-K1-03 断链修正（#1081）；K1-6b.1/.2 与 K1-6a 前序（#743–#792、#856）。

**行为变化（需要知道）**
1. 没有制品身份的旧授予（`artifact` 为 NULL）派发时一律按 Stale 拒绝，要重新确认。从 main 升级的
   库里没有这类授予，只影响 `ab/kernel` 开发版留下的库。**已知局限 #1290**：授予列表仍显示
   `granted`，派发门照样拒绝（失败即关闭）；修复进下一次发布。
2. **授予 POST 默认关闭**，宿主设 `A24_MODULE_TOOL_GRANTS=1` 才打开（上限 `A24_MODULE_GRANT_MAX_DAYS`，
   默认 30 天，最多 90 天）；临时（ephemeral）daemon 一直关闭。
3. **MCP server 与 ACP 桥只看得到自成一体的 run**。用过记忆、工具或来源不明的 run 不给外部工具看，
   提示 `content is not shared with connected tools; open it in Agent24 to see it`。
4. 升级前留下的 self-wake（未触发的，或已删除只剩 run 的）按来源 unknown 处理（0026）；出口门收紧，
   peer 视图里看不到。
5. 没有 HOME 时，`agent24 os install` / `uninstall` 照常完成并说明授予没有撤销（main 上会报错退出）。
   启用时先推进启用纪元，停用时先撤销再交出停止；停用撤销失败仍返回 `consent_revocation_failed`。

### documenting —— Documenting 第 1 片之后（上次进 main 是 r5，#1068）

> 任务描述里写的上次发布是 #979（r4）；git 上 r5 `#1068`（`release/documenting-slice1-r5`）之后才是
> 实际的上一次进 main，范围按 r5 之后算。

**新功能**
- **文档模块工具**（DOC-1-25..34）：设计与 ADR-DOC-01/02 契约修订（#1082）；入口骨架
  `POST /_a24/tools/{op}`（#1090）；`documents.list / get / job_get`（#1097）、`read_range / find`（#1098）、
  `extraction_get`（#1104）、`extract`（#1109，oplog 与认领同一事务，#1099）；工具按
  `authorized_resources` 校验（#1126）；存储重开与调用方脱钩（#1092）。
- **授权结束时停下**（DOC-1-37..39）：文本层构建记下请求方并可按请求方中止（#1194）；每个 job
  attempt 记下接受它的工具调用（#1201，迁移 `0007`）；在途调用登记、构建停止优先（#1206）；接收授权
  结束通知并停掉在途调用、作业与构建（#1213，迁移 `0008`）；撤销通知按引用停（#1234、#1240）。
  **依赖 kernel 的协议 4**，所以必须在 kernel 之后合入。
- **导入体验**：导入的取消与重试（#1186）；文档详情与文字预览（#1192、#1218）。
- **CLI 与桌面端共用文档逻辑**（DOC-1-41..46）：新增 `agent24-documents-client` crate（#1313）；
  `agent24 documents import` 与本机 HTTP transport（#1321）；documents 其余操作的 CLI 入口与共享
  路由（#1334）；桌面端文档导入和其余操作改为调用 CLI，**删除桌面端 TS 里的路由与 HTTP 实现**
  （#1325、#1343）；#1345 收 PR-Daemon 的 Low。这是 #1293 完整用例验收的前提：CLI 通过即视为
  桌面端这部分通过。
- **其他**：统一主对话与知识验收里程碑设计 KB-C0（#906）；稳定偶发测试（#1106、#1212、#1214、#1217）。

**破坏性变更**：桌面端不再带 TS 版文档路由；自建脚本如果依赖 `documents-import.ts` 的内部实现，改走
`agent24 documents ...`。

**本次不对外（#1386）**：上面「文档模块工具」与「授权结束时停下」两条已合入，但 `domain-os.yml` 没声明
`tools:`（#705 余下部分），内核授予也默认关闭，agent 看不到这些工具；什么时候开放单独走 #705。

**其他变化（#1386）**：mac 包把 `agent24` 也列进 `mac.binaries`，release 脚本与签名校验两个后端二进制
都处理（#1375）；桌面包多一个 `backend/agent24`。**不要退回 r5**：r5 打不开 r6 迁移过的库（文档请求
返回 `storage_unavailable`，数据未被改动，装回 r6 即恢复）。

### weknora —— Knowledge（私有知识库）首次进 main

**新功能**
- **只读契约 `CONTRACT-READONLY-v1`**（r8 → r10）与机器 schema、夹具、TS/Rust 双边校验
  （KB-CT-02a/b，#1044、#1053–#1065、#1128、#1131）；状态机、UI 桥、写操作语义三份契约规范核心
  （KB-CT-03，#1113–#1115）；Knowledge 对 K1 的依赖与私有数据禁用门（#1091）。
- **新 crate `agent24-knowledge`**（KB-C1-02a，#1110–#1112、#1122、#1124、#1127）：wire 类型、
  `KnowledgeTransport` trait 与 `MockTransport`（#1156）；`HttpTransport` 禁代理与重定向、字节上限流式
  读取、deadline、凭据只发往 base URL（#1161、#1169）。
- **宿主侧私有 Knowledge 接口**（KB-HOST-01，#1177、#1184、#1196、#1198、#1207、#1215、#1219、
  #1224）：daemon 凭据与端点分离、桌面以 capabilities 模式拉起 `agent24d`、binding 激活与停用、短期
  凭据推送与作废、长期登录态存储（`safeStorage` 加密文件）、`RunCreate.knowledge_binding_id`、
  没有 binding 的 run 不广告需要 binding 的工具。
- **Provider 状态机**（KB-C1-04，#1221、#1223、#1225–#1233、#1238、#1260、#1265、#1267、#1276、
  #1279、#1289、#1297、#1303、#1305、#1307、#1312、#1315、#1318、#1320、#1327–#1330、#1336–#1340）：
  服务状态派生、generation 与作废、撤权窗口、持久化账本与按序重放、重启判定、证据纪元与删除墓碑、
  Provider 编排（Off/On、选用、新建 binding、身份变化、签发）、`knowledge.db` 日志与 seal。
- **工具层**（KB-C1-05，#1232、#1242、#1264、#1269）：`knowledge.search / read` 的调用层，结果按
  「数据不是指令」包裹，分块续读拼成整段引用，发送前长度上限。
- 设计带入与任务方案 v2.1（KB-C0-01，#1171、#1174）；吸收 main（#1032、#1102）；稳定
  `get_timings` 测试（#1211）。

**行为变化（需要知道，#1396）**
1. **桌面托管的 daemon 改为 capabilities 模式**（macOS、Linux；Windows 仍 legacy）：桌面开着时，CLI 的
   `models / chat / tui / daemon stop` 报 `host authority unavailable`，`daemon status` 拿不到关停报告。
   这是对用户可见的退步，是否接受由 David 定。
2. 桌面崩溃或被强杀时，它托管的 daemon 随之停下（stdin 即父进程租约，EOF 即关机）。
3. `POST /api/v1/runs` 的 `RunInput` 新增 `knowledge_binding_id`；非宿主调用方指名 binding 返回 403
   `host_authority_required`，binding 未激活返回 409 `knowledge_binding_inactive`。新增宿主私有接口
   `/api/v1/host/knowledge/*`（渲染进程代理拒绝转发，403 `host_interface_private`）与 capability
   `knowledge.host`。
4. 新保留路径段 `host`：叫 `host` 的 attached 模块注册会被拒（400 `invalid_manifest`），升级前已有的同名模块
   也不再加载，需改名后重装。
5. 单实例：数据目录被另一宿主占用时桌面托盘显示「被占用」。
6. **不能退回旧版本**：本次带着 store 迁移 0030（P-13，#1381），旧内核读不了迁移后的库。

**迁移 / 破坏性变更**：只新增 `knowledge.db`（`0001_log`），不动既有库。新增 crate 与 `agent24-protocol`
的 Knowledge 类型是增量，不改现有接口。

### memory-p1 —— 记忆 P1（#1341）

**新功能**
- **R05 来源契约**：消息与来源、快照与来源同一事务（#1168、#1181）；运行中消息写入 Assistant 与
  ToolResult 来源（#1195）；HostSystem 来源（store 迁移 `0029`，原编号 `0023`，#1251）；发送副本按
  已落盘来源 kind 整形（#1250）；**来源无法证明时不发送**（#1197）；**非 assistant 回合不执行工具**
  （#1205）；冷恢复矩阵补测（#1261）。
- **H02 重申审计修复**：同秒重申分配独立事件 ID（#1152）、新重申事务补齐当前审计证明（#1153）、
  存量重申缺当前审计时按有界事件链兼容（#1154）、chat 产品路径回归（#1263）。
- **P1-02c 配额与失败**：c0 容量实测（#1273）；c1 满额后撤回仍可用，personal 撤回豁免配额
  （memory 迁移 `0019`，原编号 `0018`，#1275）；c2a 写失败类型化错误码，撤回 507/422、不泄露分区键
  （#1284）；c2b chat/run 记忆回执三态 `not_saved / partial`、`memory_persisted`（#1285）；c3 写入拒绝
  留痕，`mem_write_rejections` 限频环形加有界异步队列（memory 迁移 `0020`，#1302；会话缺口表接线留
  c3b，不在本次）；c4 写前磁盘门 `memory_disk_low`（#1296）；c5 空闲时 WAL checkpoint（#1283）。
- 迁移改号（#1298）：吸收 main 并避开 kernel 占用的编号。设计与 ADR 文档：P1-01 生命周期（#1149、
  #1247）、P1-02b/c（#1165、#1271）、R05 系列、PLAN-MEMORY-P1 核销（#1253、#1268）。

**行为变化 / 迁移说明**
- chat/run 的记忆回执新增类型化三态；`write_failed` 保留过渡兼容，调用方应改读 `memory_persisted`。
- 记忆写失败返回类型化错误码（撤回 507/422，磁盘不足 `memory_disk_low`），不再是笼统错误。
- 满额后撤回不再失败（ADR 要求），撤回不占配额。

**不在本次**：c3b（会话缺口接线）、c6（依赖 CORE-2 在场证明）、P1 剩余条目；kernel 的 #1277、K1-6b.5f
系列与 #1290 修复。

### idoris —— iDoris v0.3.1 接线（#1405，与 iDoris v0.3.1 联合发布）

**新功能**
- **独占本地网关**：设置 `IDORIS_URL` 和 data key（`IDORIS_API_KEY`，必须是 `idk_` 开头的 virtual key，Admin token 不行）后，所有模型调用只走 iDoris 网关。网关失败原样报错，**不回落**到 oMLX/Ollama 直连或云端（IDORIS-02a/02-04）。
- **本地证据校验**：启动时校验 `/health` 身份（`service=idoris`、`contract_version=1.0.1`）；每个响应都校验 `X-iDoris-Served-Locality=loopback` 和 Record-Id 的格式；证据头重复或不是合法 UTF-8 一律拒绝（IDORIS-03、#1409）。
- **回执**：chat 保留网关返回的真实 `response.model`、Record-Id 和 locality，CLI 打印 Record-Id，方便查询和报障（IDORIS-04）。每次调用单独签发 `X-iDoris-Request-Id`，准入 id 走 `X-iDoris-Trace-Id`。
- **预算导出**：`agent24 hw idoris-env` 计算扣除 core 预留后的字节预算，交给手动启动的 iDoris（`IDORIS_ALLOCATED_BUDGET_BYTES`，IDORIS-05；手动启动步骤见 `docs/agent/IDORIS-MANUAL-BUDGET.md`）。硬件探测子进程不继承凭据（IDORIS-04b）。
- 错误提示：401 指向 data key，403 指向 role 权限，503 `local_only_unavailable` 提示本地没有可用模型（#1420，ID-8 真机发现）。

**配对与验收**：iDoris v0.3.1（tag `8e3a216`，`idoris-v0.3.1-aarch64-apple-darwin.tar.gz`，sha256 `1125301fc32f9e44d4d06e5127137ba14196a32f9274b7416a345e690b2ffea1`）。Agent24 不打包 iDoris，需要用户自己安装。ID-8 已在 M1 Max 上接真实 oMLX 0.6.4 跑通：`idoris/daily` → `Qwen3-0.6B-4bit`，locality=loopback，日志里没有 key。iDoris 侧证据见 #1370。

**已知限制**
- iDoris 的 resident 卡不带 oMLX key（iDoris#582，v0.3.2 修）；oMLX 开了认证时，role 卡请保持 `on_demand`。
- 网关忽略 `temperature`（网关路径不发送这个字段）。
- 服务模式（`agent24 service install`）不保存 `IDORIS_API_KEY`，只支持手动启动。
- 共同 launcher、Admin token FD 交接、`idoris-any`、流式输出：本期都不做。
- iDoris v0.3.2 的 502 `upstream_auth_failed` 终态映射（ID-ERR-1）下一期做。

## [0.5.1] — 2026-10-01

**DEP-A「现在就能做」阶段收口：CLI 四平台发布流水线 + 桌面端 Linux 包**。自 0.5.0 起主要是
Deployment 多平台发布基础设施（DEP-A1/A3/A5/A6）、DEBT-10 补审修复、桌面端验收修复，以及
COMM 线（Hyphae 基础通信）设计冻结与首个落地 crate。

**补记（CHANGELOG 撰写时尚未合并、但已包含在 `v0.5.1` tag 中）**
- COMM-1b（#621）：`agent24-comm` 新增 `PasswordStore`（macOS 钥匙串 / Linux Secret Service / 内存）、`KeystoreWriteLock`（串行化 Hyphae keystore 写入，修复并发创建身份时后写覆盖前写导致私钥丢失）、`hyphae-lock-verify` CI。新增依赖 `keyring` 3.6（纯 Rust 后端）。
- COMM 第一轮联调记录与 harness（#620）：`docs/comm/JOINT-ROUND1.md`。

**行为变化 / 迁移说明**
- **F4b 入站执行默认冻结**（COMM-5a，#613，收口见 #618）：即使配置了
  `A24_NOSTR_ALLOWED_NPUBS`，Nostr 入站消息也不再自动触发 `agent24d` run——`pollOnce`
  仍然轮询、仍然喂给 FU-32 的活性探针（`liveness.observe`），只是不再对消息调用
  `bridge.handle` / `runToCompletion`。#618 把这个冻结从「行为上恰好不触发」收口成
  **结构性保证**，同时修正了冻结状态下的日志措辞（不再暗示仍在处理），并补上解冻后
  不重放积压消息的防护。**迁移**：依赖旧的「配置白名单即自动执行」行为的部署，升级后
  入站消息不会再被执行；需要临时恢复可设置 `A24_NOSTR_F4B_INBOUND=1`，但这只是过渡开关，
  后续入站执行将改走 T01-E 的高层授权，不建议长期依赖该环境变量。

**新功能**
- **Deployment 多平台发布基础设施（DEP-A，v0.5.1 范围：CLI macOS/Linux 四平台 + 桌面端
  Linux）**：
  - DEP-A1：CI 新增 `macos-latest` 上的 Rust fmt/clippy/test job，与 sidecar 线的
    `sidecar-windows.yml` 不重复（#607）。
  - DEP-A3：手写 CLI 发布流水线 `release.yml`，只在 `v[0-9]+.[0-9]+.[0-9]+` tag 上触发，
    4 个目标（macOS arm64/x64、Linux x64/arm64）各自在原生 runner 上构建，产出
    `agent24-<ver>-<os>-<arch>.tar.gz` + `SHA256SUMS`，由本流水线创建 GitHub Release
    （#608）；随后补齐 tag 与 crate 版本一致性校验、daemon health 上报版本（#609），
    并进一步收紧发布条件为显式要求 tag ref + 事件类型，防止 `workflow_dispatch` 配合
    同名分支绕过版本校验（#619）。
  - DEP-A5：桌面端启动时复用已在运行的 `agent24d`（先读 `daemon.json` 并检查 health），
    托盘「停止」只作用于桌面端自己拉起的 daemon，不再误杀外部已运行的 daemon（#602）。
  - DEP-A6：新增 `release-desktop.yml`，在 `ubuntu-22.04` 上构建 AppImage + deb 并追加
    到 DEP-A3 创建的同一个 Release（不自行创建 Release）；xvfb 冒烟测试验证 sidecar
    `/health` 返回 200，并检查 AppImage 注入的 `LD_LIBRARY_PATH` 是否污染 sidecar 环境；
    deb 在 `ubuntu-22.04` 上验证可 `dpkg -i` 安装（#617）。
  - 调研与任务拆分：`docs/Deployment/RESEARCH.md` + `docs/Deployment/TASKS.md`，三阶段
    规划（A 现在可做 → B Apple 签名后 → C 依赖其他条件），经 1 轮 Opus 评审后冻结（#599）。
- **COMM 线（Hyphae 基础通信）起步**：
  - COMM-0：`docs/design/COMM-HYPHAE.md` 设计文档，经评审后冻结（#612）。
  - COMM-1a：新增 crate `agent24-comm`，落地 `HyphaeRunner`——单次调用 Hyphae CLI，
    含 sha256 校验内嵌二进制哈希、子进程环境变量白名单、口令只走 stdin（不经参数/环境
    变量）、envelope 解析（#614）。
- **DEP-A7 Windows 移植设计**：`docs/design/WINDOWS-PORT.md`（命名管道传输，W1–W9 分片），
  经评审后冻结（#611）；配套 W0 摸底 workflow，在 `windows-latest` 上跑
  `cargo check --workspace --keep-going` 并按 crate 对错误分类（只产日志、不判红）（#610）。

**修复（ME4-CODEX-DEBT-10 补审）**
- `agent24d` 稳定哈希 + FU-105 握手竞态收口，补审 #528/#543（#600）。
- `model-timings` 保留期清理改为非全表扫描、修正 summary 混算、修正超时丢记录、修正演示
  脚本误杀（#598）。
- `os-proto` 握手残留字节不再误杀同一帧的首帧，补审 #515（#597）。

**修复（v0.5.0 验收发现）**
- 修复桌面端 3 处验收发现的问题（#594）。

**其他**
- `docs(open-design)`：M7 收口、main 落地策略刷新、叠加 PR 评审 playbook 等一批不属本轮
  主线的文档提交，随 main 一并带入，不在此逐条列出。

## [0.5.0] — 2026-09-30

**ME-3 完整收口 + 调度/推理回调 + 模块 SDK 正式落地 + Sin90/Cos72 独立可装**。自 0.4.0
起主要是 ME4-M5（SDK → Sin90 迁移 → Cos72 落地 → wire 文档）与发版前 Codex 补审修复；
v0.4.0 已交付的 ME-3/ME-4/A3 主体本版不再重复列出。

**破坏性变更 / 升级须知**
- **`shell_exec` 与外部 MCP server 子进程不再继承 daemon 的全部环境变量**（ADR-032 J-6，
  FU-103，#571）：改为显式白名单 `CHILD_ENV_WHITELIST`（`PATH`/`HOME`/`USER`/
  `LANG`/`LC_ALL`/`LC_CTYPE`/`TMPDIR`/`SHELL`/`TERM`），不放行任何 `*_KEY`/`*_TOKEN`/
  `*_SECRET`/`A24_*`/`OMLX_*`/`IDORIS_*`。依赖 daemon 环境变量透传的 `shell_exec` 命令或
  MCP server，**升级后可能拿不到之前隐式可见的变量**——需要的变量要么已在白名单内，要么
  在 `~/.agent24/mcp.json` 对应 server 的 `env` 字段里显式声明（该字段此前被静默丢弃，
  本版起生效，只注入给声明它的那个 server，不再进程级共享）。

**新功能**
- **模块 SDK 正式迁移落地**（ME4-M5）：
  - Sin90（独立仓库）线协议金样 `TS.1.0`（出站 `(method,params)` 序列 + 入站回放，Sin90 #73）
    与迁移到 `agent24-os-sdk` v0.1.0 `TS.1.1`（`src/adapter_agent24/` 手写握手客户端替换为
    SDK，真实挂载黑盒零行为变化，Sin90 #74）。
  - **Cos72 最小样例模块首次落地**（独立新仓库 `MushroomDAO/Cos72`，ME4-5.3.x）：
    pilot 规划七件套（Cos72 #3）→ 骨架（manifest + SDK 挂载 + SQLite 迁移 + 事件，Cos72 #4）
    → `mytask` 实体与路由（发布/认领/提交，Cos72 #5）→ 审批发积分（advise + 模块轮询
    `status`，幂等入账，Cos72 #6）与积分账本回放 → 任务完成摘要经 `remember_once`
    写入内核私有记忆（outbox 对账，Cos72 #7）→ 真实挂载黑盒，含与 Sin90 同时挂载时
    互相读不到对方记忆与 schedules 的隔离验证（Cos72 #8）。
- **进程外模块 wire 规范 + Node.js 参考实现**（ME4-5.4.1 / T14，#585）：
  `docs/specs/WIRE-OOP-MODULE.md` 逐方法记录 params/result/18 个错误闭集，每条事实标注
  file:line；`examples/node-module/`（零 npm 依赖的纯 Node 标准库参考模块，接管 fd 3、
  UDS 握手、events/memory 往返）；`rust/apps/agent24d/tests/me4_node_module_blackbox.rs`
  真实黑盒（挂载→代理→事件→记忆，10/10 连跑绿，无 node 时 SKIP 不 panic）。**范围收窄**：
  参考模块只演示 events/memory，scheduler upsert/fired 与 model/approval 只有文档、无参考
  实现，记 FU-104。

**安全修复**
- **ADR-032 J-6 子进程环境变量白名单**（见上「破坏性变更」，#571）：新增
  `agent24-tools::env_whitelist`；`shell_exec` 改为 `env_clear()` + 白名单；外部 MCP
  server 子进程同一白名单，按 server 隔离 `env` 声明。变异验证（删掉 `env_clear`）
  →两条测试变红，失败输出真实打印出本机 shell 里的 token，印证漏洞存在。

**修复（Codex 补审 ADR-032/A3，ME4-CODEX-DEBT-9 收口）**
- **A3 附着模块原子写顺序**（#588）：`attached.rs::write_atomically` 改为先 chmod 0600 →
  写入 → fsync → 再 rename；rename 后目录 fsync 失败只 warn 并返回 `Ok`（rename 已对外
  可见，不能让调用方误以为未提交而跳过 `on_commit`，此前的顺序会导致 DELETE/rotate/
  disable 之后旧 token 在活注册表里仍然有效）。
- **A3 空文件 fail-closed**（#588）：`attached.rs::AttachedStore::load` 遇到存在但为空的
  `attached.json` 现按 malformed 处理返回错误，不再静默当成全新空注册表。
- **A3 关机期间握手竞态**（#588）：`AttachRegistry` 新增 `is_closed()`；关机期间的握手回
  新增的 `HandshakeError::ShuttingDown`（复用既有 `unavailable` ErrorKind，未新增闭集
  条目），不再误判 `auth_failed` 导致模块停止重连。
- **A3 启动/关机竞态**（#588）：`server.rs` 里 `attach_registry_cell` 填好之后立即重查一次
  `cancel.is_cancelled()`，命中就立刻 `revoke_all()`，覆盖「stopping 任务先查到空
  registry 从而跳过撤销」的交错顺序。
- **A3 `serve_attached` 任务泄漏**（#588）：三个 `tokio::spawn` 的 `Teardown`（负责
  AbortOnDrop）改为紧跟 spawn 之后、在 `async move` 块构造之前建好再整体 move 进
  future，future 首次 poll 之前被丢弃时三个任务不再泄漏；#543 钉住的 wire 顺序语义不变。
- **桌面语音面板 AgentEar payload 校验**（#587）：`VoicePanel.tsx` 对 `payload.phase` /
  `speech.state` / `transcript.text` 加类型守卫，恶意/畸形 payload（如
  `{"phase":"__proto__"}`）不再让 React 因非法子节点抛错导致整个桌面 UI 卸载；新增
  面板级 `VoicePanelErrorBoundary`，崩溃只降级本面板。

**已知限制（如实列出）**
- FU-105（C 级，#588 范围排除）：`attach_listener.rs::handle_connection` 里握手成功帧
  `enqueue_raw` 入队与 `attach_kernel_calls` 装配 `KernelCalls` 之间仍有一个极窄窗口，
  该窗口内并发调用 `commands/*` 可能拿到 `503 module_not_ready`——修复需要改
  `AttachRegistry::attach_kernel_calls` 签名且跨两个调用点，本轮范围排除，留作后续。
- 本轮 Codex 补审只覆盖 A3 的 5 个实现 PR（#526/#527/#529/#532/#534），发现的 Medium
  及以上问题均由 #587/#588 修复合并。#515/#523/#528/#543/#544 仍未经 Codex 补审
  （ME4-CODEX-DEBT-10），按 v0.4.0 先例不阻塞发版。
- 其余延续自 v0.4.0 的已知限制（流式未支持、A3 仅 P2、FU-71/73/75/77 等）不变，
  见 `[0.4.0]` 一节，本版未处理。

**其他**
- `docs(open-design)`：叠加 PR 评审 playbook 文档（#572），不属本轮主线，随 main 一并带入。

---

## [0.4.0] — 2026-09-28

**进程外领域 OS + 内核回调面 + AgentEar 附着**。自 0.3.0 起约 150 个合并（约六成是 ME-3 进程外领域 OS 全套，含 T11 破坏性变更）；最后一周补上内核回调面（ME-4 S1/S2）、模块 SDK 原型、AgentEar 附着（ADR-032/A3）与发版前修复。

**⚠️ 破坏性变更 / 升级须知**
- **Sin90 不再编译进 agent24d**（T11，#342）。`agent24-sin90{,-os,-store}` 三个 crate 已删除；`/api/v1/sin90/*` 只在安装了进程外 Sin90 包（`agent24 os install <dir>`，仓库 `iDoris-ai/Sin90`）之后才会经内核代理出现。本版**没有** Sin90 的预编译发布物（那是 ME4-6.0.2 / v0.5.0 的事），需要从源码构建。
- 存储迁移 0007（schedules 的 owner/key/revision/三态暂停 + `schedule_deliveries`）、0008（`module_model_usage`）、**0013（`model_call_timings`，模型调用延迟明细账本）**会在首次启动时自动执行，不可回滚到 0.3.0。
- Schedule 视图新增 `owner`/`effective_enabled`/`disabled_by` 字段，**`action` 变为可空**（模块行）。直接消费 REST 的客户端要判空（桌面端已在 #465 前向兼容）。
- RPC `ErrorKind` 从 17 种变成 18 种（新增 `unavailable`，#451）。

**行为变化**
- 模块经 `_a24/model/complete` 调用推理回调、且**显式**传 `complexity: simple` 时，若由本地 loopback 的 oMLX 服务，会自动带 `chat_template_kwargs.enable_thinking=false`（Qwen3 系推理模型不再先输出大段思考）——AgentEar 语音场景的动机，但对**任何**显式 simple 的模块调用都生效（#544）。未传 `complexity` 与 `complex` 不受影响；非本地或非 oMLX 的 provider 从不发送该字段。

**新功能**
- **进程外领域 OS（ME-3，ADR-031）**：
  - `agent24 os install/uninstall`，含包根即执行边界和原子安装（#157–#161、#170）
  - manifest `spawn` 与两步解析（#156、#167）
  - 版本协商、NDJSON 帧、`initialize` 握手（#162、#164、#166）
  - 受约束代理（#173）
  - Supervisor 持有并监督模块进程，热 disable 先撤后杀（#171、#175、#178–#184）
  - 回调通道（#176、#177），入站改走 Unix 域套接字（#192）
  - 能力授予与事件回调（#199）、模块审批 gate/advise/status（#201、#203）、启用准入校验（#196）
  - 请求生命周期信号、权威配额、分页游标、OOP 记忆挂载（#205、#207、#210–#212、#217、#218、#224、#225）
  - 运行失败分类（#191）、包在运行中被改或被删（#193）、连接复用竞态与可操作报错（#194）
  - 停机可观测：`GET /api/v1/shutdown`、`daemon status` 的停机段（#187–#189）
  - 仓外包端到端黑盒（#262）
- **ME-4 内核回调**：
  - 调度回调 `_a24/scheduler/*`，含模块行所有权、REST 护栏、fired 投递泵、重启续投（#444、#447、#453–#456、#462、#465、#469–#472、#478、#479、#487、#488、#500–#502、#504）
  - 推理回调 `_a24/model/complete`，含 manifest `model_access`（缺省 `local_only`）、准入与公平、按模块用量、`GET /api/v1/usage?module=`（#446、#448、#451、#457、#461、#464、#505、#506、#510–#512）
- **模块 SDK 原型**：新 crate `agent24-os-sdk` 0.1.0（五个客户端：events/memory/approval/scheduler/model，外加 fired）和 `agent24-os-fd`（#514–#516）。它是原型，接口可能变。
- **AgentEar 附着（ADR-032 A3，P0–P2 单轮端到端）**：
  - 附着模块注册：`POST/GET/PATCH/DELETE /api/v1/attached`，CLI `agent24 os attach add/list/revoke`，token 只存哈希（#526）
  - 握手、代际、附着监听与生命周期（#527、#529）
  - 反向命令 `POST /api/v1/os/{name}/commands/{speak|stop_playback}`（#532）
  - 桌面端「语音」面板，转写只在内存（#534）
  - 需要 AgentEar ≥ v0.25.2，推荐 v0.26.1（附着 token 改存 `~/.agentear/agent24/token`，升级不再弹钥匙串授权提示）
- **桌面端**：
  - 「OS 模块（内核挂载）」视图（FU-90，#528）
  - sidecar 生命周期、进程树所有权、有界就绪与健康检查（#253–#255、#259、#260）
  - 新 logo：替换为 iDoris 像素风女孩（#541）
  - 顶栏真实默认模型名、回复耗时后缀（首字/总计 ms）、`model.call` WS 事件、语音面板延迟展示（#544）
- **CLI**：能力发现 fail-closed（#226、#227）
- **可观测性**：`model_call_timings` 原始账本（迁移 0013）+ `GET /api/v1/timings`（可按 source/since 过滤）+ `GET /api/v1/timings/summary`（count/p50/p95/max），同时记录 AgentEar 自报的分段耗时（record/asr/llm/tts/…），不落任何 prompt/response/transcript 内容（#544）

**修复**
- 模型路由：回环判定改用 `reqwest::Url`，Local provider 不走代理、不跟随重定向（FU-72，#448）。CLI 和 worker 访问本机时不走代理（FU-74，#473）。
- Nostr 入站加正向活性信号（FU-32，#147）。
- 启动时清掉 `write_durable` 崩溃后留下的孤儿临时文件（FU-67，#197）。
- 桌面端：
  - 默认模型不再选中生图、语音或嵌入模型（FU-89）
  - dev 渲染地址读 `VITE_DEV_SERVER_URL`（FU-91）
  - 侧栏显示真实端口（FU-93）（均 #528）
- HOME 路径较长时，回调 socket 回退到有防劫持检查的 `/tmp/a24-run-<hash>`（FU-92，#528）。
- **A3 握手→命令可用竞态（C1）**：握手成功行改走出站队列且在 `attach_kernel_calls` 装配 `KernelCalls` 之前入队，消除窗口期内 `commands/*` 误判 503 `module_not_ready` 的竞态（很可能是 AgentEar 真机 E2E 首轮失败的根因，#543）。
- **FU-83 探针原子写（C2）**：两处测试探针改 `.tmp` + `os.replace` 原子写，消除 CI 偶发失败（#543）。
- **SDK `FiredBody` 宽松解析（E4）**：去掉 `deny_unknown_fields`，避免内核以后给 fired body 加字段时，所有旧 SDK 编译的模块回 400、定时任务静默失败（#543）。
- 若干测试稳定性修复（#186、#214、#476、#482）。

**文档**
- ADR-031（进程外协议）、**ADR-032（AgentEar/iDoris 接入）已接受**，P0–P2 已交付
- ME-3 设计与 SPEC-ME3 改写
- PLAN-ME4 与 S1/S2/S3 设计
- 2026-09-27 现场演示记录（#523）
- L3 轨迹改为 ATIF v1.8 交换格式（#540）
- README：两套扩展机制与接口面（#149），本版按 T11/ME-3/A3 现状重写（架构图、组件表、CLI 列表、里程碑表）
- iDoris 集成设计（#98）、SOAK-F5 实测（#154）

**AgentEar 兼容性**：需要 AgentEar ≥ v0.25.2，推荐 v0.26.1（附着 token 改存 `~/.agentear/agent24/token`，0600 权限，升级不再弹钥匙串授权提示）。

**已知限制（如实列出）**
- **评审深度**：ME4 全部实现和 A3 系列只经过本地 Opus 与 PR-Daemon 评审，**没有经过 Codex 对抗评审**（`ME4-CODEX-DEBT-1~9`，额度 09-29 19:28 恢复后补审）。
- 推理回调和反向代理都**不支持流式**，非流式首字约 4.9s。P3（流式）排在 v0.5.0 之后。
- A3 只做到 P2：`proposal`/`confirm_reply` 只展示不执行；gate 可执行集合为空。
- AgentEar 真机 E2E 5 次里 1 次首轮失败，很可能已由本版 C1（#543）修复，待复验（`docs/agent/followups.md` FU-99）。
- **FU-71**：REST 和 self-wake 路径上的 cron 星期字段按 1=周日解释，`0 7 * * 1-5` 实际是周日到周四，会**静默错一天**。
- FU-75：`/api/v1/usage` 的 `cost_usd` 恒为 0.0。FU-77：`remote_allowed` 模块不能逐次收窄到 local-only。FU-73：卸载模块留下的 schedules 不会自动清理。
- `protocol/openapi.yaml` 没有收录 `/api/v1/os*`、`/api/v1/attached*`、`/commands/*`、`usage?module=`。它还保留着 7 条 `/sin90/*`，这些路径现在只在装了 Sin90 包时存在。
- F5 7×24 泡测仍未跑。FU-34（上游二进制改名 `hyphae`，默认 `A24_SPEAKER_BIN=agent-speaker`）和 FU-38（默认 relay `relay.aastar.io` 下线）让 Nostr 渠道开箱不可用。FU-29、FU-31 延续。
- `agent24-os-proto` 没有拆 kernel/module feature，SDK 用户要连带编译整个 proto（FU-86）。
- 本版发布未等 Codex 补审（jason 拍板，2026-09-2x）；本地门（fmt/clippy/test/pnpm）与 pre-pr-check 全绿是发版前提，Codex 补审后若发现 High 级问题将出 v0.4.1。

## [0.3.0] — 2026-09-02

M-E：领域 OS 成为一等公民，M-D 记忆底座重做，Nostr 渠道收官。
自 0.2.1 起 72 个合并。

### 领域 OS（可插拔架构，ADR-029）

- `DomainModule` + `KernelCtx` 契约 crate；内核不再按名字认识 Sin90（#127）
- 内核侧挂载器 + Sin90 成为第一个 `DomainModule`：自己的 DB、自己的路由
  命名空间、自己的 event module 名（#131 #132）
- 配置驱动的领域 OS 注册表 + `agent24 os` CLI（daemon 拥有注册表）（#133 #134）
- `domain-os.yml` 清单带 `deny_unknown_fields` —— 拼错字段名报错，而不是
  静默的空能力集

### 记忆（M-D 重做：可进化 / 可替换 / 可组合）

- MD-1 Condenser 缝 + 崩溃重放，签名已冻结（#113 #116）
- MD-2 EventStore（情节权威）+ ArtifactStore（markdown-CAS）+ 双谱系对账，
  checksum 移动检测、**无静默删**（#114 #115）
- MD-3 AssertionStore 双时相（矛盾=新版本非删）+ FTS5 Retriever，owner 隔离
- MD-4 MemoryWriter 写门：WebFetch/Unknown 默认不落持久，投毒语料测试
- MD-5 Consolidator：幂等 + **增量 == 全量重跑**
- MD-6 向量检索机制 + 换模型 reindex 状态机 + FTS 兜底
- MD-7 知识层：层级合并 + 触发注入 + **审核门控 auto-memory inbox**
- MD-8 长任务符号轨迹：压缩率 >99% 且 **100% 可恢复**
- **F1 `ScopedMemory`**：两个领域 OS 不再共享记忆底座（#139，六轮对抗复审）
- **F8 所有权改为 (org, space)**，org 成为一等实体；分区目录 + v1→v2 re-key，
  全程一个事务（#140，六轮对抗复审）
- F5 两处排序 tie-breaker + 一条比字面更弱的 CHECK 约束（#138）

### 渠道

- **F4 Nostr 收官**（#85–#95）：出站 register/say/search + 入站 gated +
  npub 白名单；与 agent-speaker 双向真联调；strfry 真 NIP-33 relay 覆盖定论
- **两条会让 7×24 静默失效的缺陷**（#142）：
  - Nostr 桥的 `execFile` 无 deadline —— 子进程挂起会让入站轮询循环**永久停摆**
    （`tick()` 是串行自调度，promise 不 settle 就没有下一轮）。加 60s deadline
    + SIGKILL。**注意这只关上了「子进程挂起」那一支，不含 relay 静默，见下方
    已知缺口 FU-32。**
  - 微信会话映射改为原子写：temp → `fsync` → `rename` → `fsync` 父目录，
    外加一代 `.bak` 回退与损坏时的回退读取。**限定（FU-31）**：macOS 的
    `fsync` 不保证驱动器刷新自身写缓存（那需要 `F_FULLFSYNC`，Node 不暴露），
    所以这关上的是 page cache 那个窗口，**不是「抗断电」**

### 生态

- 模块发现服务 + 浏览过滤（#90 #91 #94）

### 文档

- ADR-030 + SPEC-ORG-SPACE + M1 规划层（#141）
- 四份 vendor 研读笔记 + `docs/laws/` + Skill 分发规格 + 三档能力边界（#143）

### 已知缺口（如实列出，不粉饰）

- **FU-32（A 级）**：入站 relay 静默 —— 桥无法区分「收件箱为空」与「relay
  连接已死」。#142 只关上了子进程挂起那一支。**F5 泡测前必须处理。**
- **FU-29**：Nostr 回复是 **at-most-once**，且子进程超时被杀时**投递状态不确定**
  —— 一条回复可能被静默丢弃，且不会重试（盲重试可能重发）。这是本次发布的 F4
  渠道的用户可见行为。修法需持久化 outbox + 发送侧幂等键。
- **FU-31**：`fsync` 在 macOS 上不含 `F_FULLFSYNC`，见上方微信桥条目的限定。
- F5 7×24 泡测**尚未跑过**（物理任务）
- `agent24 os` 只有 `list`/`enable`/`disable`，**没有 `install`** —— 第三方
  领域 OS 仍需编进二进制（ME-3 未开工）
- 没有 web UI

### 一个会被当成 bug 的正常现象

`agent24 os list` 在 v0.3.0 里仍显示 `sin90 v0.2.1`。**这不是漏改的版本号** ——
领域 OS 模块有**独立的版本线**：真值来源是 `agent24-sin90-os/domain-os.yml` 的
`version` 字段，`identity_matches_the_manifest` 断言 `MANIFEST_VERSION` 等于解析
出的 manifest，`os list` 显示的是**模块版本，不是产品版本**。此前两者恰好都是
`0.2.1` 只是巧合，本次发布把巧合打破了。

## [0.2.1] — 2026-07-26

Hardening of the H9 explorer subagent (found in an adversarial re-review of
v0.2.0) plus protocol/changelog corrections.

### Fixed

- **Explorer is now truly network-free** (H9 security): the read-only registry
  the `explore` subagent runs against no longer includes `http_fetch`. `Read`
  class means "no side effect on the machine", not "no egress" — a GET could
  still send bytes an `fs_read` just returned to an arbitrary URL, which in an
  ungated, model-spawned helper is an exfiltration channel. The explorer now
  has `fs_read` only; a network-capable researcher, if ever wanted, must be a
  separate gated tool.
- **Explorer fanout is bounded** (H9): a single model turn is capped at 16 tool
  calls (mirroring the main loop) and each `explore` call has a 120s wall-clock
  ceiling, so no input shape can make one exploration run for hours.
- **Explorer panics are contained** (H9): the sub-loop runs in its own
  supervised task; a panic becomes a `ToolError` instead of unwinding past the
  caller and leaving a dangling `running` tool-call row.
- **Empty exploration answers are distinct** (H9): an explorer that produces no
  text returns an explicit sentinel, so the caller can tell "found nothing"
  from "produced nothing".
- **Protocol doc**: `approve_for_target` (H4) is now documented in the
  `Decision` type in `openapi.yaml`, and the note that `approve_for_session` is
  not offered for `external` tools is recorded there.

## [0.2.0] — 2026-07-26

**M-H — the human boundary.** Everything a person needs to stay in control of an
agent that runs while they're away: what needs asking, how far a "yes" reaches,
and what an error actually tells them. Studied from Andrew Ng's OpenWorker and
put through an adversarial review before landing (see
`docs/reference-notes/openworker.md`). Protocol changes are additive (a pre-0.2
client keeps validating), with one deliberate behaviour change: `external`-risk
tools no longer offer the broad `approve_for_session` grant — see H4.

### Added

- **Declared risk classes** (H1): a tool's `risk_class`
  (`read`/`write_local`/`exec`/`external`) is now the one property the approval
  path reads, and `requires_approval` is derived from it — two hand-maintained
  lists can no longer drift. Additive protocol field; gating outcomes are
  byte-for-byte unchanged. Each class earns a different exemption path, which is
  what the next two features are built on.
- **User-local risk overrides** (H2): a glob rule the machine's owner writes to
  relax (or tighten) an individual tool's class — the release valve that makes
  MCP's conservative `external` default actually usable. A user may correct a
  guess (third-party `external`) but never overrule knowledge (a builtin's
  class). The store is user-local and is never written by a module, persona, or
  MCP server. `GET/PUT/DELETE /api/v1/tool-overrides`.
- **Target-scoped standing grants** (H4): "always allow" for an external tool
  now binds to an **exact target** (`send → #ops`), owned by the session or the
  schedule that fired the run, matched exactly and revoked when its schedule is
  deleted. The broad whole-tool `approve_for_session` is no longer offered for
  external tools — the safe option is the only option. `Approval.standing_target`
  labels the choice; `GET/DELETE /api/v1/standing-grants`.
- **Read-only explorer subagent** (H9): the `explore` tool delegates a bounded,
  read-only investigation to a fresh sub-agent with its own context, so the
  dozens of file reads it takes to answer "where is X handled?" never crowd the
  main transcript. Read-only and no-recursion are structural — the sub-run's
  registry simply never contains a write/exec tool or `explore` itself.

### Changed

- **Provider errors say what to do** (H12): `openai returned HTTP 429` becomes a
  named cause plus the provider's own message — bad key vs wrong model vs spent
  quota. A rate-limited or 5xx primary now falls through to a healthy backup
  provider (previously every HTTP error stopped there); auth/model errors still
  stop, since a different provider can't fix a config mistake.

## [0.1.0] — 2026-07-24

The first release of the **Rust-core Agent24**: a 24/7 personal/community
workflow agent (not a coding agent). The Electron desktop shell now ships the
Rust `agent24d` daemon as its default backend, speaking the frozen v1 protocol.

### Added — Rust core (agent24d + agent24 CLI)

- **Domain state machines + store** (C1): exhaustive Run / Approval / ToolCall
  state transitions; SQLite persistence via sqlx with `BEGIN IMMEDIATE`
  transactions and a hash-chained (sha256) tamper-evident audit log.
- **Agent loop v1** (C2): `POST /api/v1/runs` → background execution with
  first-class cancellation woven through every await point; full WS lifecycle
  events; fail-closed orphan-run sweep on startup.
- **Tool system** (C3): `Tool` trait + registry with a fixed dispatch pipeline
  (capability whitelist → approval gate → timeout). Builtins: `http_fetch`
  (SSRF-guarded, resolve-then-pin against DNS rebinding), `fs_read`/`fs_write`
  (cap-std dirfd-anchored, beneath-only traversal), `shell_exec` (argv
  execution, never a shell string).
- **Approval system** (C4): fail-closed approval broker — every non-answer path
  (timeout, run-cancel, dropped channel) resolves negative; the store row is
  the single arbiter; `approve_for_session` grants scoped to (session, tool);
  runs enter `awaiting_approval` while a decision is pending.
- **Wall-clock scheduler** (C5): cron / every / at schedules with pre-advance
  (crash cannot double-fire), skip-missed (no replay bursts), and fail-safe
  disable after 5 consecutive failures. Timezone/DST-correct cron.
- **`agent24 tui`** (C6): a ratatui operator client — runs · event stream ·
  approval queue — with WS streaming, auto-reconnect, and REST reconciliation.

### Added — Desktop

- **Runs / Schedules / Approvals pages** (C7): live REST-polling views;
  Schedules form with an instant next-fire preview; desktop notifications on
  new approvals rendering the server's `available_decisions`.

### Changed

- The desktop shell defaults to the Rust `agent24d` backend
  (`AGENT24_BACKEND=node` opts back into the legacy mock).

### Protocol

- Contract-first v1 API frozen in `protocol/` (openapi.yaml +
  events.schema.json), enforced by dual-backend contract tests and a CI
  zero-drift gate.
