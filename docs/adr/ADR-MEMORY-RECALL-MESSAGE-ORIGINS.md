# R05：记忆召回消息来源契约

**PROPOSED / 待外审接受，2026-10-26 修订（原 DRAFT 2026-10-23）。** 本修订在精确 head 上被 clestons APPROVE，即作为 R05 的外审接受记录。接受的是设计，不等于 H03 全部验收；第 6 节列出仍开着的硬门。

实现基线是 `origin/ab/memory-p1` 的 `b9e4eb5f059e1d406e562cfbd4ec368ef5b5fc1e`（#1205 merge）。原稿的源码快照 `c19771dc4b82e79c03313a6d9018b94d3f1be7ac` 已过期。本稿静态检查，不声称运行测试。旧 #1094 来源草案仅归档，不继承其批准或迁移编号。细节契约在 `docs/agent/R05-1-EXACT-CONTRACT.md` 到 `R05-5-SEND-TRIM-AND-HOSTSYSTEM.md`，本稿只引用，不复述。

## 1. 已确认缺陷与正交权威

[P1 探针报告](https://github.com/iDoris-ai/Agent24/blob/146dae3966b29096d0d13d841ae5fed426140952/docs/research/MEMORY-P1-READINESS-PROBES-20261023.md) 已由 #1148 入库：用户完整 marker prompt/history 被发送副本误裁，原 snapshot 不变；审批/冷恢复的 history 仍失败。报告覆盖不等于本方案获批。

- `rust/crates/agent24-agent/src/lib.rs::request_messages_for` 当前按第一个 user message 的 prefix/end marker 判断宿主 recall；`prune_stale_recall_message` 再按文本裁剪。完整同文用户输入仍会被误认，补更多 marker 不能提供来源证明。这一点今天仍成立；修复见 R05-5 第 2 节，尚未实现。
- 同文件 `normalize_for_provider` 合并 system 与相邻 user；来源校验必须在此之前处理原始消息实例，不以合并后索引或字节反推原实例。
- `rust/crates/agent24-store/migrations/0004_run_messages.sql` 的实例身份为 `(run_id,seq)`；后续 assistant/tool 也追加 seq，不能仅存一个“开头有 recall”布尔值。
- migration `0016_run_source_tags.sql` 主键 `(run_id,seq,source_id)`，JSON 是 `source_policy.rs::SourceRef`；kind 目前只有 UserInput/SelectedMaterial，主要约束 LocalOnly 等路由隐私。
- 原稿此处写 agent 入场先 `append_run_messages_tx`。现生产路径是 `append_snapshot_run_messages`：消息与来源同一事务写入（#1181）；之后仍独立调用 `tag_run_source` 给最后的 prompt 加 UserInput。K1 标签仍不在那个事务里。

**区分两件事**：谁构造某条消息，与该消息可送往哪里。UserInput 不等于允许云端，HostRecall 也不等于可信权限指令；来源分类不能授予工具权限或扩大现有 K1 路由。

## 2. 推荐决定：窄来源映射，不重载 K1 隐私标签

独立、版本化的逐消息来源映射 `run_message_origins`，复用 `(run_id,seq)` 身份及存储事务，不把 HostRecall 塞进现有 SourceKind 后依赖 LocalOnly fallback 猜来源。表、读写 API 与外键已落地：migration `0021`（#1168）、migration `0022`（#1195），实现在 `rust/crates/agent24-store/src/message_origin.rs`。

新消息实例的来源记录规则如下。暂停通知等宿主系统性通知的 `HostSystem` 见 R05-5 第 3 节；在其迁移落地前，暂停通知仍是无来源行，读出来是 Missing 并被拒发。

存储 kind 与历史分类（R05-2、R05-3）：快照里从宿主 session log 复制的历史行，不论 role 是 user、assistant、tool 或 system 摘要，都属 SessionHistory；本轮运行中新写入的 assistant 与 tool 行才是 Assistant 与 ToolResult。

| 来源 | 只能由谁构造 | 请求副本处理 |
|---|---|---|
| UserPrompt | 宿主用户输入入口 | 原字节保留；marker 无特殊权力 |
| SessionHistory | 宿主从已认证会话读取 | 原字节保留，不把历史 user 当 recall |
| HostRecall | 宿主真实 recall 返回并构造 | 仅此类按可信 assertion ID 集合重校验/裁剪 |
| Assistant / ToolResult | `persist_message` 写点 | 不按 recall marker 裁剪；原有隐私/工具边界仍生效 |
| HostSystem | 宿主系统性通知的写点（R05-5 第 3 节，待 4a 实现） | 逐字发送，不是 HostRecall |

模型/tool/客户端 body 不得指定这些来源；REST 的 role=system 或自报 source 不是宿主构造证明。HostRecall 的 `recall_ref` 是完整 assertion id 集合的字典序紧凑 JSON 数组（#1197），不能从用户文本提取 `id:` 后自证。digest 绑定版本化编码的 role、content（含 null）、tool_calls、tool_call_id；不对用户文本做 trim/Unicode 归一化，编码规范必须独立测试。

映射含 schema version、来源种类、实例 digest、可选 recall 引用；FK `(run_id,seq)` 指向消息主键、每实例唯一；无凭据/额外正文副本。digest 是一致性检查而非签名，不宣称防本机管理员伪造；DB 写入仍是可信内核边界。既有 privacy 标签保持原表、模式与读取 fail-closed 规则，新增映射不覆盖其授权。

## 3. 原子写入与每次发送前读取

1. 宿主在构造原始 snapshot 时生成来源，不在发送阶段猜测；消息、来源映射和适用的 K1 标签在同一存储事务提交。任一来源写入失败则整批 rollback，run 不进入 provider/工具执行。消息与来源已同事务（`append_snapshot_run_messages`）；K1 标签同事务仍未实现，见第 6 节。
2. 提供明确的 typed append 接口；旧通用 append 不可悄悄给消息默认 UserPrompt/NoRecall。更新全部新 run、恢复后 assistant/tool 追加路径；尚未迁移消费者拒绝启用新格式。
3. Fresh 与同进程恢复使用同一读取/验证函数；冷恢复必须从持久映射恢复实例。读取后逐项校验版本、唯一性、seq、digest、来源/role 及 recall 引用；不是只验证 tag 数量。
4. HostRecall 重校验只构造 provider 请求副本，不更新原 snapshot、用户输入或历史正文。活动 ID 读取失败时，不得删普通用户输入或按文本降级猜来源。
5. 来源映射读取失败/不完整/不匹配应返回类型化错误，阻止该次发送和后续工具动作；`request_messages_for` 需从单纯 Vec 返回可失败结果并由执行循环处理，不可用空 Vec 隐藏失败。
6. 校验及裁剪完成后才 normalize。迭代发送/fallback 的具体实时撤回承诺沿用当前产品契约；本来源修复不偷带 purge 发送许可线性化保证、不跨网络 await 持 SQLite 写锁。

## 4. 旧 run / 坏来源的兼容门（需明确决策）

缺映射不等于 NoRecall；现有 UserInput tag 也不证明整条 thread 都非 recall。禁止从 marker、位置0、role 或相同正文回填来源，禁止旧 snapshot 全部标 NoRecall 后继续执行。

建议：旧/坏来源且无法证明其来源闭包的 run 不自动恢复/发送；保留可见历史并返回“需新建会话”的明确状态。可选择由用户在新会话重新输入自己的正文，不能自动复制旧 recall/待执行工具成为新授权。旧审批 grant 不转移到新 run。

该策略会收紧旧审批恢复的可用性，**必须先外审和产品兼容决定**，不能用安全措辞绕过现行 snapshot 恢复承诺。若需无损升级旧 run，另提供可信旧版本写点凭证/迁移证明；不能因迁移工程量大恢复文本推断。本稿不自行选择“所有旧 run 立即失效”的上线开关。

## 5. 先验测试与分片

| 验收矩阵 | 必须证明 |
|---|---|
| HostRecall / UserPrompt / SessionHistory 三条内容完全相同 | 仅真实 HostRecall 的撤回 ID 被裁，其他原字节进 provider；snapshot 不变 |
| 同进程审批与独立进程冷恢复 | 由同一持久来源恢复，不因内存缓存恰好存在而绿；普通 history 完整保留 |
| 缺失/重复/错 seq/错 owner/错 digest/未知版本/读库失败 | 明确拒绝发送；fake HostRecall、fake UserInput 不能降级成功 |
| append 来源失败 / 并发追加 / rollback | 无半份消息/标签；seq 与实例绑定，不靠旧长度推新记录 |
| normalize 合并同 role / 纯工具消息 | 合并前验证，来源不串位，tool_calls/id 不被用户文本替代 |
| 既有 K1 隐私/审批控制 | LocalOnly/Unknown 保持原门，UserPrompt 不自动变 CloudAuthorized |
| 旧 run 兼容 | 按获批兼容策略拒绝/隔离且清晰回执；不把未知回填无召回 |

先对契约与兼容分歧取得精确版本外审；再独立做存储原子接口/迁移与阴性夹具，随后 fresh 请求副本、审批与冷恢复消费者分片。每片默认总变更不超过300行，过渡态受控不启用，不能把不完整来源格式上线。H02 审计/ID 修复不依赖此方案，purge/无痕也不在本片实施。

## 6. 已落地与仍开着的边界

已落地：来源表与读写（#1168、#1195）；快照与来源同事务（#1181）；Assistant 与 ToolResult 写点（#1195）；来源无法证明时整次拒发，含审批恢复入口（#1197）；非 assistant 回合拒绝（#1205）。合并明细见 R05-4（PR #1245，head `968d2db3`，未合并）。

仍开着的硬门，本稿被接受后依然开着：

- K1 同事务：消息、来源和适用的 K1 标签在同一事务。现状是 `append_snapshot_run_messages` 之后独立 `tag_run_source`。
- G1 可信引用校验：把 `recall_ref` 解析成 id 数组并对照账本做成员证明。
- 按来源裁剪：`request_messages_for` 改为按 kind 整形，即 R05-5 4b。
- 冷恢复矩阵测试：进程重启后按已落盘来源决定发送副本，即 R05-5 第 2 节的测试。
- HostSystem 迁移与写点，即 R05-5 4a。

接受本稿不勾 PLAN 的 R02、R05、H03，不关闭上述任何一项。

## 7. 决策记录

2026-10-26，产品负责人 jason 拍板两条：

1. 新增来源 kind `HostSystem`，首个写点是记忆暂停通知。这推翻 R05-2 D7d 中暂停通知保持无来源行的决定。契约见 R05-5 第 3 节。
2. 没有来源行的旧 run 继续停发，不回填。与第 4 节及 R05-1 的 D4 一致。
