# ADR-KB-01：知识能力进入 Agent24 唯一主对话

2026-10-11｜用户确认产品方向；工程契约/任务分配待评审，本 PR 仅更新设计与验收计划。

## 决策

**迁移用户入口和编排权，复用 WeKnora 知识流程；不整套搬运或另建 Agent 引擎。** 普通用户只有 Agent24 主对话；KnowledgeView 只提供资料/Wiki/版本/同步管理，不托管第二聊天、第二个人记忆或第二套模型配置。

用户流程：主对话问题 → 判断授权知识需求 → 检索/读原文 → 必要时深度知识子任务 → 同一会话回答/引用。知识页的提问动作回到主对话，由宿主验证消息来源和用户动作；网页消息不能直接变成执行授权。

## 两仓职责

| Agent24 | WeKnora fork |
|---|---|
| 主对话与历史、M1 个人记忆、任务编排 | 原件/解析/索引/检索、出处与知识版本 |
| 已验证身份与范围、隐私/模型出口、预算/审批/取消/审计 | 后端范围/出口/预算强制执行、知识任务状态和证据 |
| ContextPack 消费、主回答、受审 SOP 候选 | Wiki/资料页、同步、获准知识发布、有界检索执行器 |
| 身份/激活桥、隔离知识页、连接 profile/Sidecar 生命周期 | 一站式配置档、能力预检与幂等默认库初始化 |

简单检索返回证据，主回答由 Agent24 生成，避免默认重复生成/计费。复杂检索可以留在 WeKnora 服务执行，但必须属于宿主任务，不维护另一个用户会话权威或个人长期记忆。底层可使用不同模型；对用户仍是一套配置/激活方案，每次后台模型调用计入统一预算和出口策略。

## 必须冻结的约定

- 用可信 adapter → REST；Agent24 现有 stdio MCP 不等于 WeKnora HTTP MCP 可直连。API/schema 是拟议契约，不声称已交付。
- 身份/组织/用途/资源范围由已验证宿主上下文决定，不信任模型自报字段；请求关联宿主 session/run、trace、deadline，知识侧临时任务不能成为第二会话权威。
- 明确知识执行器是否可强制限制步骤/token/出口/取消。禁用个人记忆和自主技能、浏览器、MCP、脚本/沙箱与跨应用动作；无法后端约束则不开放执行端点，仅用检索/读取。
- resource/revision/locator、权限版本、partial/error、取消和可关联状态；只做 health 200 不等于模型/身份/知识 ready。
- 资料和执行器输出是有出处的证据/建议，不是高优先级指令；提示注入不能升级工具权限。写入按宿主审批范围执行。
- LocalOnly 包含解析/OCR/Embedding/重排/生成/遥测。当前 Documenting §16 记录 per-run Privacy 尚有缺口；公开且获准外传的验证样本不能证明该门已经解决。
- Off/撤权/profile 切换停止在途调用和后续注入；已有历史不能假称已擦除，敏感跨组织任务新建会话。资料权限也覆盖缓存/预览/计数。
- 企业复用身份仍须服务端验证成员关系和 ACL，不能靠输入组织名或 License 绕过；密钥、销售授权、资料出口和工具执行许可分别管理。

## 计划衔接与非目标

[KB-C0–C4 验收计划](../agent/PLAN-KNOWLEDGE-CONVERSATION.md)是本决策的任务拆分，具体责任人/接口/排期在 [#904](https://github.com/iDoris-ai/Agent24/issues/904) 对齐；[Line K #713](https://github.com/iDoris-ai/Agent24/issues/713) 仍由 David 负责，宿主与销售服务新增职责不默认全部派给他。

首轮只读 REST 和专用隔离 KnowledgeView 不以通用 Workspace 为前置；这修订旧 T006/Line W 的首版接入顺序，不恢复 OD-M11/TPI-W/Line W，也不改变 DOC-1 当前关键路径。DOC-2 只在检索用户未明确提供的知识/KB 引用时依赖 Line K，原显式文档读取范围不扩大。本设计不能代替尚未完成的 per-run 隐私和组织身份门。

不删除上游 Agent 代码，不改 WeKnora 独立部署的开发验证用途，不修改用户本机已安装正式版，不启动代码/购买模型/发布或宣称企业可用。上游追踪和 reviewed release merge 继续保留。

## 来源

- 用户 2026-10-11 确认“一个助理、一处对话、一个知识管理入口”。
- [WeKnora 下游 ADR-001](https://github.com/iDoris-ai/WeKnora/blob/4b363d153122797c857871ce7e4bb63626efc637/docs/agent24/ADR-001-SINGLE-CONVERSATION.md)与 [TODO-B #1](https://github.com/iDoris-ai/WeKnora/issues/1)：普通用户无需另建库/选 Embedding，激活后最多 2–3 个自然语言问题。
- [Documenting 权威计划](../documenting/README.md) §14/§16、[ARCHITECTURE-LAYERS](../ARCHITECTURE-LAYERS.md)、[MEMORY](../laws/MEMORY.md)、[CONTEXT](../laws/CONTEXT.md)、[APPROVAL](../laws/APPROVAL.md)。
