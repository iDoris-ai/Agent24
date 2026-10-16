# OfficeAssistant 文档索引

> 2026-10-16 立。全部为规划草案，未实现、未立项进 [`tasks.md`](tasks.md)。

| 文档 | 语言 | 内容 |
|---|---|---|
| [`research/T007-office-assistant-report.md`](research/T007-office-assistant-report.md) | 英文 | 调研报告（唯一事实源）：五条 workflow、七项语义、四条需授权的出站能力、R0–R3、负例矩阵 |
| [`PLAN-OFFICE-ASSISTANT.md`](PLAN-OFFICE-ASSISTANT.md) | 中文 | 主计划：定位与边界、需求初稿、OA-M1/M2/M3、人员分工、门禁与红线、待拍板清单 |
| [`PLAN-OFFICE-ASSISTANT.en.md`](PLAN-OFFICE-ASSISTANT.en.md) | 英文（machine-drafted, native review pending） | 主计划英文版；与中文版冲突时以中文版为准 |
| [`OFFICE-ASSISTANT-TECH-DESIGN.md`](OFFICE-ASSISTANT-TECH-DESIGN.md) | 中文 | 技术方案：分层、数据契约、批准绑定/幂等/回执/恢复/出站、与 K1 的关系、测试、风险 |
| [`OFFICE-ASSISTANT-TECH-DESIGN.en.md`](OFFICE-ASSISTANT-TECH-DESIGN.en.md) | 英文（machine-drafted, native review pending） | 技术方案英文版；与中文版冲突时以中文版为准 |
| [`OFFICE-ASSISTANT-DESIGN-DISCUSSION.md`](OFFICE-ASSISTANT-DESIGN-DISCUSSION.md) | 中文 | 设计讨论：四个判断对照代码、挂点 A/B/C、第五节 Opus 5.5 评估（不是审批） |
| [`OFFICE-ASSISTANT-DESIGN-DISCUSSION.en.md`](OFFICE-ASSISTANT-DESIGN-DISCUSSION.en.md) | 英文（machine-drafted, native review pending） | 设计讨论英文版（含第五节）；与中文版冲突时以中文版为准 |
| [`OFFICE-ASSISTANT-INTERN-BRIEF.md`](OFFICE-ASSISTANT-INTERN-BRIEF.md) | 英文 | 给泰国实习生的简报与结构化反馈模板 |
| [`OFFICE-ASSISTANT-TRANSLATION-NOTES.md`](OFFICE-ASSISTANT-TRANSLATION-NOTES.md) | 中文 | 各语言版本来源、术语中英对照、冲突裁决规则 |
| [`research/T007-office-assistant-report.zh-CN.md`](research/T007-office-assistant-report.zh-CN.md) | 中文 | 调研报告官方中文版（来自 jhfnetboy/researcher，与英文版同源） |
| [`OFFICE-ASSISTANT-INTERN-BRIEF.zh-CN.md`](OFFICE-ASSISTANT-INTERN-BRIEF.zh-CN.md) | 中文 | 实习生简报中文版（给中国侧团队看） |
| [`OFFICE-ASSISTANT-INTERN-BRIEF.th.md`](OFFICE-ASSISTANT-INTERN-BRIEF.th.md) | 泰文（machine-drafted, 欢迎本人修正） | 给泰国实习生看；技术词保留英文并在括号内给泰文；三语冲突以英文原版为准 |
| [`OFFICE-ASSISTANT-INTERN-BRIEF-TRANSLATION-NOTES.md`](OFFICE-ASSISTANT-INTERN-BRIEF-TRANSLATION-NOTES.md) | 中文 | 简报的中/泰译本来源与状态、刻意保留的英文技术词 |
| [`SKILL-FEATURE-PROPOSAL.md`](SKILL-FEATURE-PROPOSAL.md) | 中文 | **独立议题**：全局 Skill 能力（加载器/三源分发/外部导入/快速创建）的现状核对、三块切片、三条硬约束、待拍板清单；配套 issue [#1037](https://github.com/iDoris-ai/Agent24/issues/1037) |
| `OA-K-KERNEL-GATE-DESIGN.md`**（待产出）** | 中文 | **内核门 OA-K 的详细设计**（模块提交 `(tool, 确切 input, request_id)` → 内核跑完整流水线）；含四类扩展面的相容性矩阵、安全论证、备选与落地切片 |

> **已定决策（2026-10-16，作者拍板）**：挂点采用 **B**——新建第一方进程外领域 OS 模块 `office`（与 `agent24-documents` 同构）承载业务状态；**有外部效果的动作仍由内核执行**，两者之间新增内核门 **OA-K**。详见主计划 §0.4。

阅读顺序：报告「Conclusion」与 §6 → 主计划 §0、§7（待拍板）→ 技术方案 §4、§5 → 实习生简报。
