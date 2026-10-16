# OfficeAssistant 文档翻译说明

> 2026-10-16 立。适用于 [`OFFICE-ASSISTANT-INDEX.md`](OFFICE-ASSISTANT-INDEX.md) 所列文档。

## 1. 各版本来源

| 文档 | 中文版 | 英文版 |
|---|---|---|
| T007 调研报告 | `research/T007-office-assistant-report.zh-CN.md`（本 worktree 中为未跟踪文件，来源未在此核实） | `research/T007-office-assistant-report.md`：英文原报告，**英文术语基准** |
| 主计划 | `PLAN-OFFICE-ASSISTANT.md`：原文，由 Opus 5.5 起草（#1036） | `PLAN-OFFICE-ASSISTANT.en.md`：由 Opus 5.5 机器起草的译文，待母语者审校 |
| 技术方案 | `OFFICE-ASSISTANT-TECH-DESIGN.md`：原文，由 Opus 5.5 起草（#1036） | `OFFICE-ASSISTANT-TECH-DESIGN.en.md`：机器起草译文，待审校 |
| 设计讨论 | `OFFICE-ASSISTANT-DESIGN-DISCUSSION.md`：前四节由协调者起草，第五节由 Opus 5.5 撰写 | `OFFICE-ASSISTANT-DESIGN-DISCUSSION.en.md`：机器起草译文，待审校 |
| 实习生简报 | 无 | `OFFICE-ASSISTANT-INTERN-BRIEF.md`：英文原文 |

目前**没有任何一份译文经过人工审校**。

## 2. 冲突裁决

- **译文与中文版冲突时，以中文版为准。** 译文只为方便非中文读者，不是独立的事实源。
- 能力、交互、权限、验收的事实源仍是 T007 英文报告；计划与报告冲突时按计划正文所述处理。
- 译文中引用带行号的中文文档（例如 `PLAN-OFFICE-ASSISTANT.md:145-146`）时**保留中文路径**，因为行号只对中文原文有效；不带行号的链接指向对应的 `.en.md`。
- 中文图表（技术方案 §2、§3.1 的 ASCII 图，设计讨论 5.2 的 YAML 注释）在译文中原样保留，并附英文说明。

## 3. 术语对照

凡 T007 英文报告已有的说法，一律沿用报告原词。

| 中文 | 英文 | 备注 |
|---|---|---|
| 工作流 / 办公流程 | workflow / office workflow | 报告 §1 |
| 步骤 | step | |
| 动作 | action | 每步一个 `action_id` |
| 产物 | artifact | |
| 回执 | receipt | |
| 批准 / 审批 | approval | |
| 批准绑定 | approval binding | 报告 §4 数据契约 |
| 幂等键 | idempotency key | |
| payload 摘要 | payload digest | 代码字段 `payload_digest`、`effective_input_digest` |
| 进程外领域 OS | out-of-process domain OS | 报告用 "supervised out-of-process modules" 指同类选项 |
| 领域 OS | domain OS | |
| consent 门 | consent gate | |
| 内核 | kernel | |
| 控制面 | control plane | "sole execution control plane" |
| 工作区（L2 文件系统）/ 文件工作区 | workspace / file workspace | 与产品 Workspace、Cos72 工作台区分 |
| 运行 | run | |
| 准备 | preparation | 报告 §2 表 |
| 明确授权 | explicit authorization | |
| 有界委托 | bounded delegation | 报告 §5 |
| 长期授权 / 持续授权 | ongoing authorization | 报告 §2、§5 |
| 常驻授权 | standing authorization | 对应代码 `standing_target` |
| 待确认 | needs confirmation | 报告 §2 |
| 结果待核验 | result needs verification | 报告 §3 |
| 对账 | reconciliation / reconcile | |
| fail-closed（失败即拒绝） | fail-closed | |
| 出站 / 外发 / 出口 | outbound / egress | 门 = egress gate，判定 = egress decision |
| 来源标签 | source tag | |
| 业务编排层 | business orchestration layer | |
| 投影 | projection | |
| 负例矩阵 | negative-case (acceptance) matrix | 报告 §6 |
| 金标准 | gold standard | |
| 挂点 | attachment point | |
| 待拍板 | decisions pending | |
| 判窄 / 判错 | too narrow / wrong | 设计讨论第五节 |
| 主干 / 主线 | main track / mainline | |
| 台账 | ledger | |
| 红线 | red lines | |

## 4. 待人工定稿的译法

- 「拍板」：译文混用 decide / decision pending / sign-off，是否统一为 sign-off 待定。
- 「判窄」：译为 too narrow，母语者可能更习惯 incomplete / understated。
- 「挂点」：译为 attachment point，备选 integration point / hosting surface。
- 「出站 / 外发 / 出口」：译文按语境用 outbound（动作）与 egress（门、判定），是否全部统一为 egress 待定。
- 「主干 / 主线」：main track / mainline 并用，是否合一待定。
- 「闭集」：译为 closed set，备选 fixed enum。
- 主计划 §2.1 引用报告的那句话，中文版写的是「本文中译」；英文版直接引用报告英文原句，因此改注为 "quoted from the English original"。
- 主计划 §0.2 表中「见 §4.3『依赖』」，但「依赖」一节实际在 §4.2。译文照原文保留 §4.3，应在中文版里一并修正。
