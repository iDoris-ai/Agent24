**结论：Agent24 有支撑长期演进的基础，适合继续增量建设；但组件替换仍有条件，记忆升级已有落点，自进化和模型训练尚未形成完整闭环。** 以下仅依据 `bb434b61` 静态核查，未改文件、构建、测试或切换检出。

优点：

- **能力边界已经落实到接口。** `ModelProvider`、`Tool`、`ScopedMemory` 分别约束模型、工具和模块记忆；后者不接受 owner/space 参数，作用域由宿主提供。这能限制替换实现的影响范围。证据：`agent24-models/src/lib.rs::ModelProvider`、`agent24-domain/src/memory.rs::ScopedMemory`。
- **通信有明确契约。** 客户端使用 REST/WS，`agent24-protocol::protocol_compatibility` 检查版本；进程外模块通过 `agent24-os-proto/src/initialize.rs` 协商版本；iDoris 锁文件同时固定制品摘要和契约范围。因此同契约替换有基础，但业务语义、权限和数据迁移也必须兼容。
- **记忆具备可演进的数据基础。** `SessionLog` 保存事件，`WriteGate` 原子提交断言与审计，`FtsRetriever::rebuild` 可重建索引；`SessionMemory::recall` 已进入实际调用链。原文与派生索引分离，有利于以后升级检索算法。

问题和风险，按优先级排序：

1. **高：升级恢复保护尚未进入生产启动流程。** [migration_coordinator.rs](/Users/jason/Dev/auraai/Agent24/rust/apps/agent24d/src/migration_coordinator.rs:20) 明确 `ENABLED=false`；`server.rs::open_memory_base` 失败后返回 `None`，继续无记忆运行。已有恢复代码不能算产品保证，长期升级仍存在部分库升级成功、记忆不可用的风险。
2. **中高：内部存储替换成本明显高于外部模型替换。** [SessionMemory](/Users/jason/Dev/auraai/Agent24/rust/crates/agent24-agent/src/session_memory.rs:60) 直接持有 `KvStore`、`SessionLog`，召回直接调用具体 FTS 实现；`RunManager` 持有具体 `Store`。迁往其他数据库或替换检索服务，会涉及调用方，不能仅靠实现已有 trait 完成。
3. **中：外部组件接入方式多样，不能统一视为即插即用。** Hyphae 使用 CLI JSON envelope，iDoris 使用 HTTP，领域 OS 使用握手与代理。`agent24-components::fetch_verified` 还依赖 iDoris 的锁结构及缓存目录约束。替换不同品牌组件通常需要适配器；下载统一不等于运行契约统一。
4. **面向未来的缺口：训练与自进化仍缺执行闭环。** [MlWorker](/Users/jason/Dev/auraai/Agent24/rust/crates/agent24-worker/src/lib.rs:138) 仅有 embedding、转写和健康接口，应用依赖中未见消费者，也没有训练接口。现有结构可以承载独立训练进程，但不能据此宣称已有训练、评测、发布、回滚能力。个人向量召回仍未接入当前 `SessionMemory` 路径。

建议按顺序推进：

1. 先完成启动迁移保护接线，以跨库失败、崩溃恢复和明确降级状态作为验收。
2. 先从召回入口注入检索接口，用第二种实现验证替换边界，避免全面抽象重写。
3. 为各外部组件列出版本、能力、错误、取消及数据迁移契约；先建立新旧版本兼容用例。
4. 训练先做一个人工批准的小模型微调实验，串通数据来源、离线评测、模型登记和回滚，再考虑自动触发。
5. 更新 `docs/ARCHITECTURE-LAYERS.md`：其中 iDoris 未接线、记忆召回未消费等描述已与代码不符。

不确定项：未验证外部组件实际运行兼容性、长期容量和性能；未核查外部仓库训练能力。`MEMORY-STRATEGY.md` 与生命周期 ADR 中的规划不作为已实现证据；自进化能否提高质量，仍须真实评测证明。