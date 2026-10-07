# Agent24 判断/分类决策清单

## 表格：所有 JUDGMENT / CLASSIFICATION 决策点

| # | 判断点 | 位置 | 现在怎么判 | 判断类型 | 触发频率 | 判错后果 | 已知问题 |
|---|---|---|---|---|---|---|---|
| 1 | **记住意图提取** (explicit_remember) | `rust/crates/agent24-agent/src/retain.rs` | 规则匹配：「记住…」/「请记住…」/「remember that…」等句式；拒绝疑问句 (ends with 吗/?/了吗) | 二选一 | 每轮用户消息 | 误记住垃圾内容/误拒合法记忆 | T13 补记「你帮我…记住没有」否定句，T06 仅检查用户 prompt 不检查模型回答 |
| 2 | **WriteGate：记忆优先级决策** | `rust/crates/agent24-memory/src/writer.rs` | 确定性策略：`Trust::UserSaid + explicit_remember → Commit`；`Trust::UserSaid/Model/ToolOutput → Hold`；`Trust::WebFetch/Unknown → Reject`；无 evidence → 降级到 Hold | 三层 | 每条断言候选 | Commit 无证据被误记；Reject 的内容漏掉重要信息 | 同义更新/supersede 推迟到 P1；需反向验证 trust 与 evidence 的对应 |
| 3 | **ModelRouter 提供商选择** | `rust/crates/agent24-models/src/router.rs:tier_order()` | 按 `TaskProfile{privacy, complexity}` 矩阵决策：LocalOnly → Local/Lora only；Simple+Any → Local/Lora/Remote；Complex+Any → Remote/Local/Lora | 排序 | 每次推理调用 | LocalOnly 漏掉被错标为 Remote 的提供商、泄露敏感信息；Complex 无远端时降级体验 | 主对话路径全用默认 TaskProfile，尚无画像生成（ID-1 待实现）；iDoris 接线待拍板 |
| 4 | **Guardian 安全门禁** | `rust/crates/agent24-policy/src/guardian.rs` | 查询本地模型判断风险等级：模型答 `low` → AutoApprove；`high` 或模型不可用/无法解析 → Escalate；`always_review` 清单硬否 | 二选一 (含escalate原因分类) | 每次工具调用前 | `low` 误判导致危险操作通过；模型故障误导/丢失风险评估 | 可选功能（默认关闭）；always_review 清单可能不全；评估 prompt 未明确说明哪些操作最危险 |
| 5 | **消息事件来源判定** | `rust/crates/agent24-memory/src/event.rs` `Trust` enum | 调用方标记：UserSaid/Model/System/ToolOutput/WebFetch/Unknown | 枚举分类 | 每条消息入库时 | 错标 trust 导致 WriteGate 决策全盘错 | Trust 是"输入不变量"，调用方负责诚实标记；无验证机制 |
| 6 | **召回内容过滤** | `rust/crates/agent24-memory/src/retriever.rs` `search_any()` | SQL 过滤：owner 作用域（只召回 personal space）；qualified 状态（已提交的不是 hold）；active 状态（未被撤回）；被撤回的 `recorded_to` 非空 → 跳过 | 多选一 | 每个新 run 前 | 召回他人数据/模块记忆；调出失效断言；搜索噪音过多 | 暂无向量召回（T07b 可选）；CJK 二元组检索质量未评测（T08 基线手写≤20例） |
| 7 | **摘要触发决策** | `rust/crates/agent24-memory/src/condenser.rs` | 启发式：未覆盖消息数 > `max_recent` 时触发；头部消息折叠（保留最后 `keep_recent` 条），避免拆 tool_calls 对 | 二选一+参数 | 每轮消息超阈值时 | 摘要抽取错误丢失重要上下文；频繁折叠降低性能 | `LlmSummaryCondenser` 用 LLM 不稳定；T04 改为显式 `append_summary`，摘要失败不删消息 |
| 8 | **会话导入触发** | `rust/crates/agent24-memory/src/session_log.rs` | 每会话锁内首次读写时检查：事件日志无此会话事件 && KV 有旧 blob → 触发 `import_legacy` | 二选一 | 仅首次读写该会话 | 导入漏掉旧数据；导入与新写混合导致不一致 | 导入一个事务完成（原子性保证）；旧压缩已删原文无法恢复 |
| 9 | **疑问句拒绝** | `rust/crates/agent24-agent/src/retain.rs:is_question_sentence()` | 规则：末尾含吗/?/了吗/没/呢 或 以「了」开头 → 拒绝提取 | 二选一 | explicit_remember 匹配后 | 误拒「你帮我记住没什么答案…」这样的陈述句；误收「我能过敏吗」这样的问句内容 | T13 补充「你帮我…记住没有」与「记住没有…」的区分（前者是陈述，后者是疑问） |
| 10 | **模块权限检查** (Authorizer) | `rust/apps/agent24d/src/authz.rs` | 确定性策略：`ModulePrivateOnly`：allow ⟺ `scope.owner == SpaceId::module_private(actor_module)` | 二选一 | 每次 `MemoryLease::lend` 调用 | Deny 时模块无法访问自有记忆；Allow 时泄露他模块数据 | M1 占位实现（无复杂授权），Authorizer 接口预留扩展点（P1+ `delegation`/`policy_epoch`） |
| 11 | **会话可视性** | `rust/crates/agent24-memory/src/session_log.rs:load_view()` | 过滤逻辑：加载最新 `session.summary` + `seq > covered_through_seq` 的全部 `message` 事件；超过 4×`max_recent` 仅截视图 | 多条件过滤 | 每次 `session_context` 调用 | 漏掉早期消息；展示冗余消息膨胀上下文 | 摘要失败时消息全留（可用，但未折叠）；超硬上限截视图不删事件（数据安全） |
| 12 | **审批类型判定** | `rust/apps/agent24d/src/approvals.rs` | 查询 approval 的 `decision_type` 字段；按工具类型或操作类型进行分类（如工具调用、写操作等） | 枚举分类 | 每次需审批的操作 | 错分决策类型导致用户看不到正确选项；丢失审计信息 | 决策类型池的定义与扩展机制不明确 |
| 13 | **任务属性路由** (TaskProfile 生成) | `docs/iDoris-integration-and-entry-router.md` ID-1 | 规则版：入口路由器（Semantic Router，~0.1B）按用户输入决策「纯 agent / 搜索 / 发邮件 / 本地模型 / 多模型链」；后续升级到语义路由 | 多选一 | 每个新 run 入口 | 误路由导致低效/不可用的解决方案选择 | 规则版尚未实现（ID-1 待排期）；与 iDoris 网关的字段对应有出入；画像生成位置待拍板 |
| 14 | **语音命令意图识别** (AgentEar) | `rust/apps/agent24d/src/agentear_timings.rs` | 定时器驱动的状态机：检测 `type=="local"/"remote"`，判定是完整发言还是续句 | 状态转移 | 每次语音输入块 | 误判段落边界导致提交不完整命令/重复提交 | P0-P2 非流式单轮；流式多轮、中断恢复、降噪预处理均未实现 |
| 15 | **条消息 origin 来源标记** | `rust/crates/agent24-memory/src/event.rs` `Origin { trust }` | 调用方选择并初始化 `Trust` 枚举值；每次消息/输出进入 SessionLog 时必须设置 | 枚举 | 每条消息 | 错标导致后续 WriteGate 决策全错；无自动验证 | 同第 5 项（trust 是输入不变量，无验证） |
| 16 | **访问控制决策** (capability token) | `rust/apps/agent24d/src/capability_registry.rs` | 基于 token 的 bearer token 模式；路由级资源授权与高权限接口隔离待实现（A-1） | 基于凭证 | 每次需权限的 API 调用 | 泄露 bearer token 等于全权；高权限接口暴露 | Desktop 自有 daemon 与能力隔离尚未落地；iDoris 客户端令牌体系待定 |
| 17 | **入站消息网关** (Hyphae F4b) | `docs/design/HYPHAE-CLI-INTEGRATION.md` | 白名单入站 gated-run 路径冻结；门禁 / PII 检测 / 意图判定具体规则待 T01-E 收口 | 多级网关 | 每条入站消息 | 漏掉恶意消息；误拒合法请求 | 密码进钥匙串（暂未锁版本）；R3 (headless 加密解锁) 挂账；持久化规则未明确 |
| 18 | **PII/敏感内容检测** | 计划中（无实现）见 C5 A-3b | 未实现，规划为：消费方进程自带启动断言 (egress guard) | 待设计 | 待实现 | 敏感数据外泄 | 尚无代码实现；法律框架冻结见 C5 A-3；Agent24 需实现自身非 LLM 出站的启动断言 |
| 19 | **召回停用词/噪音过滤** | 可选：`retriever.rs` 中的 `search_any` | 未实现停用词；仅依赖 FTS 的 BM25 排序与用户 k 参数限制 | 多选一 | 召回时 | 汉语停用词漏掉、英文停用词也无，导致召回被「的」「是」刷屏 | 中文评测集仅≤20 例（T08）；向量召回与去重均未做（T07b 可选） |

---

## 关键观察（≤10 行）

1. **记忆判断分三层**：显式提取（规则）→ 写入策略（WriteGate 确定性）→ 召回过滤（SQL 条件）。规则和策略都硬编码，无 LLM 参与（T06b LLM 抽取是 optional 增强）。

2. **模型选择基于 TaskProfile 矩阵**，但主对话路径全用默认值，真实画像生成（ID-1）尚未排期。iDoris 接线（ID-2）涉及字段对应有待确认。

3. **Guardian 安全门禁** 可选启用，用本地小模型做风险评估，fail-closed 到人审批；always_review 硬清单二次兜底，但清单的完备性未有评测。

4. **信任链** 依赖调用方诚实标记 origin/trust，无反向验证机制。一旦 trust 标错，WriteGate 决策全盘脱轨——属设计契约假设而非防线。

5. **摘要折叠启发式** 按消息计数阈值和时间窗口；失败不删消息（no-loss）但也不召回（影响体验）；向量召回与 LLM 抽取均为可选增强（P2/P1）。

6. **会话导入一原子性**：旧 blob → 事件一个事务，保证一致性；但 M1 前已被压缩删除的原文无法恢复——是已知、记录在案的恢复边界。

7. **疑问句拒绝规则** 覆盖常见粒子（吗/？/了吗），但对复杂句式（「你帮我记住…没有…」结构）有边界缺陷，T13 补充但未完全闭合。

8. **权限与授权** 两层不同：Authorizer（模块空间）基于 ModulePrivateOnly；capability token（API 级）基于 bearer；两层实现与对齐现状不对等（M1 仅 token 基础，高权限隔离未落地）。

9. **入站网关与 PII 检测** 约束尚多是文档层，代码实现零散：Hyphae 白名单冻结，但 PII 启动断言完全缺位（待 A-3b 实现）。

10. **缺失评测** 最明显：召回中文质量（T08 仅≤20 例手写）；Guardian 评估 prompt 的危险识别能力；摘要折叠的语义保留率；规则解析边界情况覆盖度（疑问句、地址前缀等）。

---

## 未确认的判断点

- AgentEar 声称 TTS 集成但 Agent24 侧不依赖，具体实现状态待核实。
- 出境脱敏执行由 iDoris 出口网关负责，Agent24 侧仅负责 LocalOnly 贯穿与自身非 LLM 出站启动断言（实现未开工）。
