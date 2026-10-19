# Agent24 × WeKnora：知识能力总体方案

2026-10-11｜讨论稿，不启动实现、部署或改变优先级。已核对 T002、T004、T005、T006、T008 和 Agent24 集成分支计划；未做运行实测。

## 1. 选型与产品定位

**采用 WeKnora 独立 fork，复用知识应用；Agent24 保留记忆、任务和执行权威。** 延续 T006 的既定方向，不重新研发 RAG，也不把第三方源码塞进内核。Haystack 仅作为权限/数据契约无法薄适配时的备选。

KB 有两面：KnowledgeProvider 提供上下文、搜索和引用；知识页承载管理 UI。后台不依赖打开页面或加载 OS。产品 Workspace、L2 文件目录、KB 空间不是同一授权边界；首版不依赖通用 Workspace，具体接入以第 8 节为准。

## 2. 四条链路、各自的权威

| 能力 | 归属与做法 |
|---|---|
| 默认上下文 | KB 保存人工批准的个人公开简介、组织背景、品牌规范；宿主加载小型版本化 ContextPack，不全文加载知识库。 |
| 个人偏好记忆 | Agent24 M1 记忆负责偏好、私有事实、撤回和来源；不在 WeKnora 再建第二份权威记忆。 |
| 自动回答 | 简单问题由 Agent24 回答；知识问题按权限检索、重排和读取，复杂问题交给有界知识子任务，返回答案及证据。 |
| 对话沉淀 SOP | Agent24 选择已授权会话、生成候选；人审后发布到 KB。原始聊天不是默认组织资料。 |

这里“公开”指对当前授权受众共享，不等于互联网公开。只加载当前活动组织/项目，不能把用户所属全部组织都注入。

全局包首版建议预算 1–2k tokens，正文与引用按版本缓存，任务相关证据另算预算；这是设计目标，非实测。任务开始、续轮、恢复与组织切换均重新判断范围。偏好不能覆盖组织安全政策，普通文档始终是证据而非系统指令。必需规范不可用就阻断相关动作，可选背景失败可见降级。

## 3. 技术接入：薄桥，不做双控制面

链路：**Agent24 → 可信 adapter → WeKnora REST → 知识/原件**。先开放 search、read、capabilities；再补 context_pack、ask、ingest/job，名称是拟议语义，不声称上游已提供同名接口。

请求绑定宿主确定的用户、组织、资源范围、用途、隐私政策、deadline 与 trace；响应携带资源/修订 ID、出处定位、权限版本、partial 与错误。模型不能自报身份扩大范围。当前 Agent24 MCP 基线是 stdio，不能把 WeKnora Streamable HTTP 当即插即用；REST 薄桥先行，必要时再暴露 stdio MCP。

WeKnora 当前 Agent 已有技能、浏览器、MCP、沙箱和记忆，**不是天然只读文档代理**。集成配置禁用其个人记忆与执行工具，知识任务限制 KB、模型出口、步数/token、超时和取消；写入另走授权。UI 与 API 必须同样受限，不能只隐藏按钮，也不能给它宿主全权 token。无法后端强制限制时只开放检索。

资料权限在召回、读取、计数、预览和缓存共同执行；撤权先停止提供，再异步清索引。LocalOnly 覆盖解析/OCR、embedding、重排、生成及遥测；不因“本地存储”自动认为无外传。知识 Off 取消查询、停止后续注入和缓存复用，不停共享服务器；记忆开关独立。已发送的历史内容不能宣称被抹除。

## 4. 来源、格式与 SOP

先做文件/Markdown＋飞书授权目录＋RSS，再逐个验收其他连接器，不一次接十个平台。上游已列出飞书知识库/云盘、Confluence、GitLab、IMA、Notion、语雀、钉钉、Seafile、RSS，但**内容同步不等于逐用户 ACL 同步**。首期只用明确共享集合；不能用管理员采集账号给所有人检索。

支持格式逐版验收：anydoc 的进程内 Office 解析不代表高保真编辑、扫描 OCR 或全部 Lite 格式能力。飞书 mindnote/slides 会跳过；Confluence 当前不导入附件；IMA 跳过 AI 会话和视频；RSS 淘汰不视为删除。界面如实显示未覆盖项，解析失败保留原件。

外部源继续是内容权威，KB 保存授权快照/索引；上传资料约定唯一托管方。统一 resource_id、revision、hash、来源时间和 Citation；换模型/解析器允许重建索引，不改原件。PDF/Office 保留原格式，Markdown 是交换及 SOP 格式，不是无损 Office 替代品。媒体只接授权 OCR/ASR 等投影并回源，不让 WeKnora 替代 T008 的媒体资产主库。

SOP 流程：**选会话 → 脱敏/抽取 → 待审草稿 → 审核 → 发布 → 入库 → 必要时另批准进入全局包**。记录适用场景、步骤、例外、会话/资料来源、负责人、版本和复核期限；冲突生成新修订，不能静默覆盖规范。去重和幂等防止反复入库；AI 答案不得循环变成证明自身正确的权威来源。Documenting 提供后续编辑/导出，不阻塞 Markdown 首轮闭环。

## 5. 个人与企业：一个 fork，两种部署

- **个人**：Lite 单进程＋SQLite/本地文件，按需下载，作为 Agent24 托管 sidecar 提供 Web UI；不用再套一层 Wails 桌面窗口。高级 OCR/模型分别授权安装。
- **组织**：独立内网 Linux 服务器＋标准 Compose，多空间、身份映射、强制 RBAC、备份和审计；Agent24 只连接。跨设备访问需 TLS/网关，数据库不直暴露。
- 默认提供知识入口和轻 adapter；首次使用选择本地安装或连接服务器。默认启用不等于下载运行时、模型、离线包或读取全盘。暂不默认捆绑 Lite：官方最新 release 无产物，实际体积尚未核实；待签名、体积与启动预算通过再决定。

一个 fork、两个部署档，共用 adapter；固定 SHA/镜像/schema/模型，升级经回归和恢复演练。保留 MIT，另审 ParadeDB 等依赖许可。

## 6. 16GB 与 Desktop 贡献判断

上游安装文档建议标准栈 **4 核/8GB 起步，不含本地模型权重**。16GB 可作为小规模试点机器，不等于企业容量承诺。只启动核心五服务，暂不开 Neo4j、Langfuse、额外向量库和执行沙箱；解析并发先设 1，获准模型放独立节点/服务。严格本地模式不能用远端模型替代，须单独测量。

先 20 份资料/10 个问题，再扩至千份/3 路并发；覆盖大文件、扫描件、撤权和恢复，记录峰值内存、OOM、积压、延迟与引用。建议全机留 4GB 余量，不足则移出重处理/扩容。仅试验方案，未实测。

**上游已有 Wails Lite Desktop 源码，但尚无正式安装包。** 不提“从零新增 Desktop”PR，也不把 Agent24 强制嵌入上游。优先贡献现有桌面的打包/签名、可靠更新、无头宿主 API 与权限/取消修复；先与维护者确认范围。Agent24 专属品牌和身份桥留在 fork。

## 7. 与里程碑对齐的最短路径

1. **不重做 M1**：任务台账与 PR #720 确认记忆已合入 main；组件路线图部分旧状态过时。K1 #792 已合入 ab/kernel，不等于完整产品验收/发行。
2. **验证期**：固定上游 release/main 差异，分别验证 Lite 与 16GB 标准栈；先只读 REST、公开样本、引用与撤权，无需等待完整 Workspace 通用化。
3. **能力闭环**：复用 K1 身份/隐私/审计，接小包、检索、Off、人工发布 SOP；部署顺序按第 8 节。
4. **界面集成**：知识页独立接入，不先做 Workspace 通用化；OD-M11/TPI-W 继续暂停，本稿不授权任何实现。
5. **组织放量**：两用户两组织隔离、来源撤权、后台同步、出口与恢复均通过才开放；不必等待个人记忆 P2/P3 全部完成，但必须交付本次组织使用所需授权。

首版只承诺：小包准确加载、问题带可核实出处、偏好不串入 KB、SOP 经审后可复用。跨租户泄漏、原件损坏、撤权失效或未经授权出口任一出现即停止放量。

## 依据

- [T002](https://github.com/jhfnetboy/researcher/tree/main/topics/T002-contextual-knowledge-bases)、[T006 既有计划](https://github.com/jhfnetboy/researcher/blob/main/topics/T006-knowledge-media-base/subtopics/weknora-workspace-development-plan.md)；另核对 T004/T005/T008。
- [WeKnora 安装](https://github.com/Tencent/WeKnora/blob/150d8cc605ed9305a5fc4ad1b8789dae238decce/website-docs/01-getting-started/02-installation.md)、[Desktop](https://github.com/Tencent/WeKnora/blob/150d8cc605ed9305a5fc4ad1b8789dae238decce/website-docs/05-clients/05-desktop.md)、[数据源](https://github.com/Tencent/WeKnora/blob/150d8cc605ed9305a5fc4ad1b8789dae238decce/website-docs/03-features/10-datasource.md)。本轮 main 为该 SHA；最新 release 是 v0.8.2，不能混称同一基线。
- Agent24：`origin/ab/kernel=dada7267` 的 `docs/agent/tasks.md`、`PLAN-KERNEL-K1.md`、`COMPONENT-ROADMAP.md`、`PLAN-OD-NEXT.md`。researcher 本轮 SHA：`5cb0ace0`。

## 8. 补充决策：分发、切换与嵌入实施规划

本节更新首版接入及先后建议，不改变暂停门或启动开发。

**分发与体积。** Lite 是 Go 可执行程序，不是 Node.js 包；现有 `scripts/package-lite.sh` 打成平台专用 `.tar.gz`，内含程序、web 静态页、配置、SQLite 迁移和许可证。单进程不等于单文件；基础运行不需 npm。独立 Wails 桌面壳另有 `.app` 等产物，企业为容器镜像。v0.8.2 无 release assets，本轮也未找到可用的 Lite 构建产物；准确 MB 尚未知。打固定版本后分别报压缩下载量、解压占用、运行内存，模型/用户资料另算；不拿源码大小代替。

**运行与界面分层。** 个人：SidecarManager＋WeKnora 启动/就绪适配器；企业：只连接服务器，不由 Desktop 启停。REST 可独立完成后台功能，不需通用 Workspace；无缝界面仍须专门承载。用独立 Knowledge WebContentsView 复用 OD 的隔离模式，不复用其全局状态、token 或分区，不等待上游桌面安装包。

**可以切换连接，不能原地切换数据库。** 设置保存个人本地/企业服务器两个 profile，按端点、用户、组织隔离登录与缓存；切换取消在途调用、重校验范围，不混搜。旧模型历史不会自动清除，跨组织敏感任务新建会话。SQLite→Postgres 是导出/导入迁移：原件、版本、引用与授权重映射，索引可重建；不直接复制数据库，不默认同步私有资料。回切也不自动下载企业资料。

**先企业部署验证，再个人 Lite 分发。** 先用内网标准栈、单用户授权样本跑 API 与嵌入页，减少本地打包/签名干扰；多人企业可用须另过隔离门。之后补 Lite 启动和安装，复用同一 UI/API；不是先做完整企业定制。

| 仓库 | 开发责任 |
|---|---|
| WeKnora fork | Vue 知识页、主题/无重复导航的嵌入模式、身份交换与后端权限；两部署共用。借鉴 Desktop 的主题/登录/导航，Wails 原生方法改为可选平台 adapter。 |
| Agent24 | React 菜单/状态壳、KnowledgeView、受限 IPC、provider、profile 和本地进程托管；不复制整套 Vue 页面。 |

**分三片冻结约定并验收：**
1. 契约片：双方确认版本、受信端点、local-managed/remote、ready/disabled/error、取消、引用及日志；本地就绪核验版本/实例/能力，不能只看 health。仓库 owner/name 立项时确定。
2. 远端 UI 片：fork 提供拟议嵌入路由；宿主关 Node、开 contextIsolation/sandbox，按 profile 独立 partition，限制导航/弹窗，不给页面宿主 preload 或管理员 token。SSO 用用户绑定的一次性凭据交换，写操作接受宿主批准的范围；桥消息校验 sender、origin、schema 和关联 ID。
3. 本地/切换片：签名校验安装包、适配 Sidecar 生命周期；关闭知识页只隐藏，停用取消调用，本地自有进程可停、远端仅断开。验收内嵌无新窗口、主题/尺寸、流式取消、两用户隔离、撤权、重启、profile 切换及 OD 无回归。

以上是拟议跨仓约定，尚未获双方工程验收；API 接通不等于界面、SSO或权限已经完成。

