# Knowledge 定位与差异登记

> **来源与适用范围**：KB-C0-01（[#1010](https://github.com/iDoris-ai/Agent24/issues/1010)）把两个 issue 的正文同步到 `ab/weknora`。验收清单没有带进来；原有的二级标题降为三级，并加了两个分节标题；正文句子未作改动，其中的“本 issue”指各自的来源 issue：
> - 定位摘要：来自 [#987](https://github.com/iDoris-ai/Agent24/issues/987)。同步时 #987 的验收项“jason 与 David 确认定位、边界与四条链路的权威划分”还没有完成，所以下面的定位和硬边界都是待确认的设计输入，确认结果以 #987 的评论为准。
> - 差异登记：来自 [#989](https://github.com/iDoris-ai/Agent24/issues/989)，是 2026-10-15 的核实结果。
>
> 后续的更正和新发现的差异以 #989 的回写评论和 WeKnora [能力矩阵](https://github.com/iDoris-ai/WeKnora/blob/a24/develop/docs/agent24/CAPABILITY-MATRIX.md)为准，本文不再随之更新。任务方案全文见 [PLAN-KNOWLEDGE-TASKS.md](PLAN-KNOWLEDGE-TASKS.md)。

## 一、定位摘要（#987）

来源：#904 §1–§2 · fork ADR-001 · fork TODO-B（iDoris-ai/WeKnora#1）· Documenting README §6/§9/§12 · 任务方案 v2.1

### 一句话

Knowledge 是 Agent24 的资料与知识服务组件。WeKnora fork 提供引擎能力（解析、OCR、切块、Embedding、hybrid 检索、重排、出处、Wiki、连接器），Agent24 决定谁、何时、在什么授权范围内使用这些知识，并自己生成主回答。它不是独立的知识库产品，也不是第二个聊天机器人。

### 要解决的用户问题

1. **唯一主对话**：普通用户不在两个机器人间切换、不重复登录、不配两套模型、不养两份记忆和历史（用户 2026-10-11 确认）。
2. **激活即用**：激活 → 最多 2–3 个自然语言问题 → 自动初始化 → 上传或提问。模型供应商、Embedding、向量库、分块、解析器只留给管理员诊断入口。
3. **四条能力链路**，各有权威：
   - 默认背景：KB 保存人工批准的个人共享简介、组织背景、品牌规范；宿主任务前加载小型版本化 ContextPack，不全文注入。
   - 个人偏好记忆：归 Agent24 M1，WeKnora 不建第二份权威记忆。
   - 知识问答：按授权检索并读原文，Agent24 生成回答并显示引用；无证据不假称有答案。复杂问题交给有预算的有界子任务（KB-C3，后置）。
   - SOP 沉淀：选已授权会话 → 脱敏抽取 → 草稿 → 人审 → 发布；聊天和 AI 回答不自动成为组织规范。

### 与相邻组件的边界

- **Documenting（Line D）**：Agent24 管"谁能做什么"，Documenting 管"文档发生了什么"，Knowledge 管"文档里有什么知识"。Documenting 只通过服务契约用 KB，不维护私有知识库；知识无关操作永远不依赖 KB；只有 DOC-2 检索用户未明确提供的材料时才用冻结后的契约。文档修订号与索引修订号必须分开报告。
- **Memory（M1）**：M1 记"用户告诉助理关于自己的话"，personal 空间、用户明说才写、撤回=失效加事件；KB 存"经批准可共享的资料与规范"，多级空间带 ACL、人审发布、撤权先停供再清索引。两边开关独立，不双写；集成模式关闭 WeKnora 上游记忆。两者之间只有 SOP 沉淀这一条受人审的单向通道。
- **RAG**：Knowledge 依赖上游的索引与检索两段，不依赖"生成"段（主回答由 Agent24 生成）；ACL、出处修订、LocalOnly、取消预算、契约 schema 是 Knowledge 自己的业务层。
- **Workspace / Line W**：首版用专用 REST 加隔离 KnowledgeView 接入，不等通用 Workspace；不恢复 OD-M11/TPI-W/Line W。

### 两种部署、一个 fork

个人：Lite 单进程加 SQLite，由 SidecarManager 托管；组织：连接内网标准服务，Desktop 只连接不启停。共用 adapter、契约与 KnowledgeView。

### 硬边界（任何阶段不得违反）

- 知识页关 Node、开 contextIsolation/sandbox、独立 partition，不拿宿主 token。
- 集成模式由服务端禁用上游的聊天、Agent、技能、浏览器、MCP、沙箱、记忆，不是只隐藏按钮。
- LocalOnly 覆盖解析、OCR、Embedding、重排、生成、遥测；做不到就 fail closed。
- ACL 同时覆盖搜索、计数、读取、预览、下载、缓存和在途结果。
- 文档、Wiki、子任务输出是证据不是指令，不得提升工具权限。
- 租户签发密钥绝不下发 Desktop；有效范围 = 权威授权 ∩ 凭据范围 ∩ 请求范围，空交集拒绝。

## 二、现状核实与差异登记（#989）

来源：任务方案 v2.1 §0/§4/§6（2026-10-15 在源码与 GitHub 核实）。本 issue 是事实台账，发现新差异追加评论。

### Agent24 侧核实

- `ab/weknora`=ab125de2，落后 main 68 提交；ruleset `ab-integration` 未含 ab/weknora → #1008 #1009。
- PR #906：PR-Daemon 两次 APPROVE、CI 6/6、MERGEABLE，仍 OPEN（#907/#713 评论写"无外部 approval"已过时）。ab/documenting 领先 main 121 提交，不能整支带入。ab/kernel 的 K1 来源政策未到 main。
- SidecarManager 与 agent24-sidecar-host 已实现但无产品调用方，TS↔Rust 胶水不存在。
- OD WebContentsView 无 preload、持久 partition、IPC 无 sender 校验、分区无 CSP，未达 ADR-004 → KnowledgeView 按 ADR-004 新做。
- RunManager 任务前注入写死 memory.recall（agent24-agent/src/lib.rs:1394），无 context hook。
- per-run privacy 缺口：agent loop/subagent/摘要/REST 均 TaskProfile::default，RunCreate 无 privacy/org/principal。
- Read 级工具只在 requires_approval 时过审批门（agent24-tools/src/lib.rs:573）。
- fixtures_roundtrip.rs 只读 events 目录；MCP 只有 stdio；http_fetch 拒绝 loopback。

### WeKnora fork 侧核实（a24/develop=4b363d15 = v0.8.2 + 2 文档提交）

- 上游 10-14 发 v0.8.3（无 assets）。同步工作流自 10-13 连续失败：令牌缺 `workflows` 权限 → iDoris-ai/WeKnora#3。FORK-MAINTENANCE 写的"每 6 小时快进"当前不成立。
- 可复用：scoped API key（25 个 capability、knowledge_base_ids 白名单、默认拒绝；**空白名单=全租户可见**，tenant_api_key.go:328）；signed_token 外部主体（HS256/aud/exp≤24h，但只返回 sub，不影响 ACL、不进审计，auth.go:551-658）；POST /knowledge-bases/{id}/hybrid-search；GET /knowledge/{id}、chunks、/download、/preview；/system/info、/system/capabilities（能力声明≠依赖健康）；租户隔离、撤权下一请求生效。
- 缺：集成模式总开关（agents/sessions/chat/skills/sandbox/mcp/im/embed/memory/websearch/organizations+shares/local-browser 对 JWT 无法关闭；多条路由注册在 Auth 之前 router.go:174-205；WEKNORA_SANDBOX_MODE 是死配置；RBAC 可配置为放行 rbac.go:89）；用户级授权与审计；/ready（/health 写死 ok）；无头 Lite 托管契约（默认 0.0.0.0:8080、:0 不回报端口、无 ready 行/--version、JWT_SECRET 空则重启随机；auto-setup 只限 Wails）；检索契约（/knowledge-search 脱钩 ctx 且错误一律 500；SearchResult 无 revision；ContentRevision 不序列化）；主 SPA 无嵌入模式（frame-ancestors 'self'、token 在 localStorage）；**日志中间件记录请求/响应正文、hybrid-search 记录 query**（logger.go:180/253、knowledgebase.go:346）；release-lite.yml 不按 tag 检出、会发 Release 并推 main；只读检索也算 query embedding；无默认 KB 创建、无租户级配置档。

### 与现有文档的差异

- #904/总体方案把身份交换、能力声明写成"待建"：上游已有，差的是收窄、审计与真实就绪。
- 总体方案"Lite 默认仅本机"只对 Wails 桌面版成立，无头 cmd/server 默认 0.0.0.0。
- "借鉴 OD 隔离模式"：OD 现行实现未达 ADR-004。
- "复用 K1"：来源政策在 ab/kernel，未到 main/ab/weknora。
- #713 引用 README §22 第 3 项，核对为第 4 项。

### 风险（高）

- 身份权威错位：租户 HMAC 泄露可伪造任意主体 → D2；KB-C1-03 前只做单用户样本。
- 隐私只覆盖单次 HTTP：证据进入回答/摘要/委派/后台后外流；只读也算 embedding；日志记正文 → KB-SEC-01a/b、KB-OBS-01、KB-SVC-POLICY-01。
- 同租户无 per-user ACL；既存共享授权不因禁用路由失效 → D3、KB-C1-01。
- 撤权/Off/切换只取消网络请求 → 两端校验 generation，首期不缓存。
- 嵌入页绕过宿主审批 → 服务端允许列表 + ADR-004 + 受限桥。
- revision/locator 缺失导致引用错版本 → KB-C1-01c 先补映射。
- David 同时承担 Line D/K，新增宿主/身份/销售职责无 owner → #990 #992 #995。

### 风险（中）

基线漂移与同步失效；fork 补丁面扩大（全部进 PATCH-LEDGER，尽量做成配置开关与新增路由）；Lite 体积/内存/签名/恢复未实测；Sidecar 胶水从零写。
