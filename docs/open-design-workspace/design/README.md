# P0 设计冻结索引

> 状态：P0 frozen / SOL Gate PASS
>
> 日期：2026-09-19
>
> Agent24 基线：`69baf50d62e6e3db5ca41a8882d35deb45ea3454`
>
> Open Design 基线：`open-design-v0.22.2@73953213a6fec2c8092e8e77d229a3074aa828a9`

本目录是 Agent24 × Open Design 集成的契约权威来源。实现分支只能消费这里冻结的接口；不能在代码中另行发明 workspace 字段、路径传递、审批 authority 或 sidecar 生命周期语义。

## 文档

- [ADR-001：集成边界与 authority](ADR-001-INTEGRATION-BOUNDARIES.md)
- [ADR-002：workspace contract](ADR-002-WORKSPACE-CONTRACT.md)
- [ADR-003：ACP bridge contract](ADR-003-ACP-BRIDGE.md)
- [ADR-004：Electron Creative host contract](ADR-004-DESKTOP-HOST.md)
- [ADR-005：Creative capability 与 authority 隔离（已批准）](ADR-005-CAPABILITY-AUTHORITY.md)
- [兼容矩阵](COMPATIBILITY-MATRIX.md)
- [风险登记](RISK-REGISTER.md)
- [P0 SOL 门禁记录](P0-REVIEW.md)
- [Open Design pin](open-design.pin.json)

## 变更规则

契约冻结后，字段和行为按以下等级管理：

- **兼容变更**：增加可选字段、增加新的枚举值、加强内部实现且不改变公开语义；普通 review 后可合入。
- **迁移变更**：重命名字段、改变状态机、改变 session/workspace 绑定；必须提供双读或迁移窗口、fixture 与回滚说明。
- **边界变更**：允许裸路径、改变审批 authority、引入第二个 control plane、把 source writeback 交给 Open Design；属于重大设计变化，必须暂停并重新获得批准。

每个实现 PR 必须引用依赖 ID（`A24-OD-*` 或 `OD-*`）、本目录对应 ADR，以及兼容矩阵中的确切版本行。
