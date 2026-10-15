# Agent24 的 iDoris 消费方指针

跨仓契约唯一权威是 iDoris 的 [边界与接口规范](https://github.com/iDoris-ai/iDoris/blob/main/docs/interfaces/iDoris-Agent24-边界与接口规范.md)。
本次执行读取 [PR516 的固定修订](https://github.com/iDoris-ai/iDoris/blob/1924dda0cd0b9aa6c4fb5057b3b6f48e5bcf9cee/docs/interfaces/iDoris-Agent24-边界与接口规范.md)：文档修订1.2整合R0–R8，修复B1时恢复既有显式Fallback语义并注明I0运行时差距；R9及新增031范围仍标OWNER REVIEW PENDING，不把它当新wire或已批准新能力。
硬件事实和整机分配的反向权威仍是 Agent24 [HW-TIERS §3.3/3.4](design/HW-TIERS-capability-hardware-matrix.md) 与 [ADR-036](decision.md)。本文件只记录消费状态，不复制字段表或预算公式。

## 当前能力状态

以 Agent24 main `622be5d7df0387b91028820624a778cb99cd6d43` 与2026-10-15交付队列为基线；未标已实现的能力不能在产品中宣称可用。

| 能力 | 状态 | 证据或剩余工作 |
| --- | --- | --- |
| Desktop main 只读 Admin consumer | 已实现于main | #772/#789及apps/desktop/src/main/idoris-admin；不等于已有共同launcher或可写Admin。 |
| HW事实/能力快照 | 已实现部分 | [#964](https://github.com/iDoris-ai/Agent24/pull/964)、[#966](https://github.com/iDoris-ai/Agent24/pull/966)已合并；HardwareReport和启动保存hardware.json不等于预算sender。 |
| iDoris数据面provider与独占配置 | 缺失 | ModelRouter::from_env当前只有omlx/ollama；按执行计划接入，不用Local标签代替网关响应绊线。 |
| 数据key与LocalOnly响应验证 | 缺失/待核对前置 | 需要独立virtual key、子进程隔离和实际响应证据；Admin token不是数据key。 |
| role→真实模型身份 | iDoris候选，消费接线缺失 | iDoris #510；Agent24模型选择必须影响实际chat，不能以Settings高亮代替。 |
| 硬件预算实际交付与准入 | iDoris基础已入main，启动接线候选，sender缺失 | iDoris #511已合入9395a967，#512仍候选；Agent24交付已分配字节值，推荐与真实load/CAS共用额度，不重复扣OS预留。 |
| 完整031调用/发布 | 未验收 | 手动双进程初始化、隔离HOME、真实Router/fixture、小模型离线调用及人工main gate仍需完成。 |

执行任务、先后关系和验收只维护在 [ab/idoris执行计划](agent/PLAN-IDORIS-V031.md)。
[07-30入口路由提案](iDoris-integration-and-entry-router.md) 与 [ADR-032集成材料](design/INTEGRATION-AGENTEAR-IDORIS.md) 保留历史价值，不作为当前API或已交付能力证明。
