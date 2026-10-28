# Agent24 的 iDoris 消费方指针

跨仓契约唯一权威是 iDoris 的 [边界与接口规范](https://github.com/iDoris-ai/iDoris/blob/main/docs/interfaces/iDoris-Agent24-边界与接口规范.md)。
本次执行读取 [PR516 的固定修订](https://github.com/iDoris-ai/iDoris/blob/1924dda0cd0b9aa6c4fb5057b3b6f48e5bcf9cee/docs/interfaces/iDoris-Agent24-边界与接口规范.md)：文档修订1.2整合R0–R8，修复B1时恢复既有显式Fallback语义并注明I0运行时差距；R9及新增031范围仍标OWNER REVIEW PENDING，不把它当新wire或已批准新能力。
硬件事实和整机分配的反向权威仍是 Agent24 [HW-TIERS §3.3/3.4](design/HW-TIERS-capability-hardware-matrix.md) 与 [ADR-036](decision.md)。本文件只记录消费状态，不复制字段表或预算公式。

## 当前能力状态

以 `ab/idoris`（`abb1ef98`，已吸收 main，release PR 前）与 iDoris v0.3.1（tag `8e3a216`，`contract_version` 1.0.1）为基线；未标已实现的能力不能在产品中宣称可用。端到端验收以 v0.6.0-preview 的 ID-1..ID-8（`docs/release/ACCEPTANCE-v0.6.0-preview.md`）为准。

| 能力 | 状态 | 证据或剩余工作 |
| --- | --- | --- |
| Desktop main 只读 Admin consumer | 已实现于main | #772/#789及apps/desktop/src/main/idoris-admin；不等于已有共同launcher或可写Admin。 |
| HW事实/能力快照 | 已实现部分 | [#964](https://github.com/iDoris-ai/Agent24/pull/964)、[#966](https://github.com/iDoris-ai/Agent24/pull/966)已合并；HardwareReport和启动保存hardware.json不等于预算sender。 |
| iDoris数据面provider与独占配置 | 已实现于ab/idoris | IDORIS-02a/02-04：设 `IDORIS_URL` 时 `ModelRouter` 只走 idoris-local 网关，网关失败原样返回、不回落 omlx/ollama/云端（`router.rs` complete/models 两处早返回）；网关请求不带 temperature（#1365）。 |
| 数据key与LocalOnly响应验证 | 已实现于ab/idoris | IDORIS-03：`/health` 身份校验（`service`、`contract_version`=1.0.1、非 dev 无 key），每个响应校验 `X-iDoris-Served-Locality`=loopback，Record-Id 形状校验；硬件探测子进程不继承凭据（IDORIS-04b）；Admin token 不是数据key。 |
| role→真实模型身份 | 已实现于ab/idoris，待ID-3/ID-8验收 | IDORIS-04：chat 回执保留网关真实 `response.model`、Record-Id、locality；Request-Id 每次调用单独签发，准入 id 走 Trace-Id。Settings 改动进入实际 chat 由 ID-3/ID-8 验证。 |
| 硬件预算实际交付与准入 | 手动交付已实现于ab/idoris | IDORIS-05：`agent24 hw idoris-env` 导出扣除 core 后的 `IDORIS_ALLOCATED_BUDGET_BYTES`，由用户启动 iDoris 时设置（[手动预算](agent/IDORIS-MANUAL-BUDGET.md)）；不启动、不动态修改 iDoris，共同 launcher 未做。 |
| 完整031调用/发布 | 待验收 | ID-1..ID-8 在发布候选上执行（ID-8：jason 笔记本 M1 Max、隔离 HOME、on_demand 卡、断网）；iDoris 侧证据见 #1370。已知限制：resident 卡不带 oMLX key（iDoris#582，v0.3.2）；服务模式不持久化 `IDORIS_API_KEY`，仅手动启动。 |

执行任务、先后关系和验收只维护在 [ab/idoris执行计划](agent/PLAN-IDORIS-V031.md)。
[07-30入口路由提案](iDoris-integration-and-entry-router.md) 与 [ADR-032集成材料](design/INTEGRATION-AGENTEAR-IDORIS.md) 保留历史价值，不作为当前API或已交付能力证明。

接线成功时，聊天API转交经校验的 `X-iDoris-Record-Id`，CLI打印同一回执供查询/报障。直连调用不补造iDoris回执；此标识不是凭据，不替代实际模型与locality校验。
