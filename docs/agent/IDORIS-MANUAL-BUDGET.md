# IDORIS-05：手动交付已分配预算

此路径基于 [已生效计划](PLAN-IDORIS-V031.md) 05，接口权威仍为 [consumer pointer](../iDoris-consumer.md)。它生成既有启动环境配置，不启动、管理或动态修改 iDoris。

1. 用 `agent24 hw probe` 查看本机快照。`profile.total_mem_bytes` 保持物理 RAM，`usable_bytes` 已扣系统与 App 预留。
2. 汇总 Agent24 本次整机分配中语音、决策、嵌入等 core 能力的实际预留字节。传入真实数值；本命令不推测模型用量，也不承诺该预留之外的动态加载有共享 lease。`0` 只适用于确实未给 core 分配本地模型的隔离场景。
3. 生成并交付唯一非敏感配置：

```sh
# CORE_RESERVED_BYTES 必须来自本次实际 core 分配；不提供默认值。
allocation=$(agent24 hw idoris-env --core-reserved-bytes "${CORE_RESERVED_BYTES:?}") || exit
# 由用户独立启动已支持该输入的 iDoris；不交给 Agent24 托管。
env "$allocation" idoris serve
```

输出只有 `IDORIS_ALLOCATED_BUDGET_BYTES=<十进制整数字节>`。从实时 HardwareReport 的 usable 扣除显式 core 预留，预留达到/超过 usable 时输出真实零；不再次扣 OS，不把剩余额度伪装成物理 RAM。独立 iDoris 启动后此配置固定，改变分配需用户重新启动；不能把启动配置当动态预算 API。

iDoris 必须包含 #511/#512 的实际预算消费路径；单独的 v0.3.0 不保证该能力。后端、模型绑定和 virtual key 的初始化依照唯一规范；本命令不读写 key、不使用 Admin 写接口。Agent24 的网关进程另行配置非敏感 URL 与数据 key，key 不进入 argv、plist、日志或无关子进程。

本片 CLI 测试仅证明精确预算进入独立进程，不代替真实 iDoris 的推荐/加载 CAS、零 load/POST、双进程取消、031 新行为及最终真机验收。当前 core 预留须由实际分配显式提供；完整自动能力计划与跨运行时动态账本仍属后续实现。
