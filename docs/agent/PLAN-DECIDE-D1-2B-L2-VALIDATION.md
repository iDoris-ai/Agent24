# D1-2b-F — L2 答案校验契约提案

状态：纯文档待外审，以下具体选项是推荐提案，尚非用户批准的政策。基线：`ab/decide@0ae8571a79800f071aa712845f7329e50114e8a2`。
依据：[总计划](PLAN-DECIDE.md) D1 前置 L2、[D1 计划](PLAN-DECIDE-D1.md) D1-2；[L1 契约](PLAN-DECIDE-D1-2A-L1-UNAVAILABLE.md) 已由 [#855](https://github.com/iDoris-ai/Agent24/pull/855) 精确 head `390ab014f017ba6add5251965f820a58d22feea8` APPROVE。
任务提供的 [PR #908](https://github.com/iDoris-ai/Agent24/pull/908) 缓存：head `26d17f21c37bce9b2fdb988d1eaf8f76d72fdfb1` OPEN、CI 全绿、reviews 空；未实时查询，不视为已批准/合并。本规格可独立评审，service 接入等待 L1 实现合入。

## 推荐规则

service 独占信任边界；先校验请求，再对每层返回的答案集整体校验。字段来自 `rust/crates/agent24-decide/src/{types,backend,service}.rs`，不是新造的题型。

| 对象 | 精确约束 / 稳定错误 code |
|---|---|
| `DecisionRequest.questions` | 非空，否则 `empty_questions`；即使 service 无后端也先检查 |
| `Question::{Choice,Noul,Score}.id` | `trim().is_empty()` 禁止：`blank_question_id`；原字符串重复：`duplicate_question_id` |
| `Question::Choice.labels` | 非空：`empty_labels`；逐项禁止 trim 后空白：`blank_label`；原字符串重复：`duplicate_label`；单标签允许 |
| `Question::Score.{min,max}` | 均为有限 `f32` 且 `min <= max`，否则 `invalid_score_bounds`；零宽区间允许 |
| 其他请求字段 | `point` 保持开放字符串（包括空串），不查注册表；`input`、`prompt` 可空；`context` 保持任意 `serde_json::Value`；`side_effect` 不新增政策校验 |
| `Answer.question_id()` | 必须精确匹配请求 ID：`unknown_question_id`；同集不得重复：`duplicate_answer_id` |
| `Answer.value()` | Choice→`AnswerValue::Label`、Noul→`Bool`、Score→`Score`；不符：`answer_kind`；Bool 本身已是 Rust bool |
| Label / Score 值 | 标签精确属于该题 `labels`：`label_not_allowed`；Score 有限且在闭区间 `[min,max]`：`score_out_of_range` |
| `Answer.p()` | `None` 允许且保留；`Some(p)` 必须有限且 `0 <= p <= 1`，否则 `invalid_probability`；含 NaN、±Inf，规则层也不豁免 |
| `BackendOutcome::Decided(Vec<Answer>)` | 每题恰一答案；在单答案检查后检查完整覆盖，否则 `answer_coverage`；空答案非法 |
| `BackendOutcome::NoConclusion { floor }` | 空集合法；非空允许任意无重复子集（包括全集），执行相同单答案检查，不要求完整覆盖 |

请求按题面顺序检查，每题先 ID 空白/重复，再 labels 或 bounds；标签按原顺序检查。响应按收到顺序检查，每项依次 ID 存在/重复、题型、值、p，最后才检查 Decided 覆盖；只报首个错误。所有匹配区分大小写、空格及 Unicode 字节，不裁剪、不排序、不转值、不截断、不补答案、不重归一化概率。

校验通过后才沿用 `Answer::normalized_for(kind)`：真实 `BackendKind::LlmSimulation` 一律 `calibrated=false`，其余原样保留。`None` 与任意 calibrated 标志组合均允许，但无可用概率；`Some(p)` 合法不证明校准成立，`calibrated=true` 不豁免范围检查。校准证明与阈值动作留给后续任务。

## 错误与兼容性

推荐新增下列公共形状（均为提案类型/字段），不把输入错误塞进 L1 `Unavailable`：

```rust
DecisionService::decide(&self, request: &DecisionRequest) -> Result<Decision, InvalidRequest>
InvalidRequest { code: String } // 新公共 struct，crate 根 re-export；Serialize，不加 Deserialize
Outcome::InvalidResponse { code: String, failed_stage_index: usize, failed_backend_kind: BackendKind }
```

非法请求返回 `Err`，不调用任何后端的 kind/evaluate，不构造 Decision、不填写尝试下标。非法响应返回 `Ok(Decision)`，outcome 为 InvalidResponse；真实尝试下标/种类采用 L1 的配置 vector 零基身份。立即终止，不调用下一层、重试或自动回退。

保留最近有效非空 floor 的全部答案、顺序与 backend；新有效非空 floor 整体替换，不合并；空 floor 不清除。畸形 Decided/floor 整体丢弃，不抢救其中合法项。有旧 floor 时 `Decision.backend` 是其来源；无 floor 时 answers 空、backend 是非法响应层；model 仍 None，latency_ms 仍总耗时。floor 永远不升级为 Decided，错误/地板不授权写入、注入、日志写入或下载。

有意变更 Rust 返回签名、新错误类型和 Outcome 穷举匹配；畸形数据由旧成功/保留变为拒绝。保持有效答案原顺序，可免去规范排序造成的字节变化。相同字段值（固定 latency）下，有效 Decided/Abstain 与 L1 获批后的 Unavailable JSON 逐字节不变；L1 自身新增字段不是本片承诺的旧基线字节兼容。实际耗时不承诺相同。请求、Answer、trait、profile 和日志 v1 形状保持；不新增 Deserialize/迁移。

JSON fixture 序列化的是取出的 Decision 或 InvalidRequest，**不直接序列化 Result**（否则 serde 会额外包 `Ok`/`Err`）。本仓没有推理 REST 端点，不定义 HTTP 状态或 transport envelope。另选把非法响应复用 Unavailable 会混淆“已运行但畸形”与不可用，故不推荐。

## 固定 fixtures 与验收矩阵

Q 为以下请求；通过 `Answer::new` 构造 Rust 答案。N=`("n",Bool(true),None,false)`，C=`("c",Label("yes"),Some(1.0),true)`，S=`("s",Score(0.0),Some(0.0),false)`；括号最后两项为 p/calibrated，backend 参数使用测试层 kind。所有数值仅是 fixture。

```json
{"point":"retain.intent","input":"","context":{},"questions":[{"kind":"noul","id":"n","prompt":""},{"kind":"choice","id":"c","prompt":"","labels":["yes","no"]},{"kind":"score","id":"s","prompt":"","min":-1.0,"max":1.0}],"side_effect":"reversible"}
```

除最后的多错优先级行外，负例逐一独立改变一个条件；响应负例对 Decided 和非空 floor 两模式都跑（覆盖缺失仅针对 Decided）。层序列使用可报告任意 kind 的计数 fake backend（Rule floor 不是现有 RuleBackend 的 miss）；请求负例预置会成功的后端，kind/evaluate 调用均为零；响应负例本层 evaluate 恰一次，后置成功层调用零次。

| 输入 / 层返回 | 必须观察到的结果 |
|---|---|
| Q.questions=[]；ID 分别为 `""`、`" \t"`；重复 `n` | 分别 Err(empty_questions/blank_question_id/duplicate_question_id)，所有后端调用 0 |
| Choice labels=[]；含 `""`、`" "`；重复 yes | 分别 Err(empty_labels/blank_label/duplicate_label)，所有后端调用 0 |
| Score bounds=(1,-1)、(NaN,1)、(-1,+Inf)、(-Inf,1) | 每例 Err(invalid_score_bounds)，所有后端调用 0 |
| 合法未知 point、空 input/prompt、context=null；单标签；min=max=0 | 请求通过；对应有效全答案 Decided；不查询 point 政策 |
| ID `n` 与 ` n`；标签 yes 与 ` yes` | 视为不同值，不裁剪；答案须原字符串精确匹配 |
| Decided=[S,C,N] | Decided，保持 S,C,N 顺序和原值；已有 floor 不影响完整终值 |
| Decided=[] 或 [N,C] | InvalidResponse(answer_coverage)，不是空/partial 成功 |
| [N,C,S,答案x]；[N,N] | 分别 unknown_question_id、duplicate_answer_id，覆盖检查不掩盖前者 |
| N 值换 Label；C 换 Bool；S 换 Bool | 每例 answer_kind |
| C 标签换 YES、` yes`、maybe | 每例 label_not_allowed（Q 仅含 yes/no） |
| S 值分别 -1、1；-1.01、1.01、NaN、±Inf | 两端通过；其余每例 score_out_of_range |
| p=None（calibrated 各 true/false）；p=0、1（各 true/false） | 全通过且原样保留；None 不伪造为 0/1 |
| p=-0.01、1.01、NaN、±Inf（calibrated 各 true/false） | 每例 invalid_probability；Rule/Encoder/Deep/LlmSimulation 都覆盖 |
| LlmSimulation 构造时谎报 Encoder、calibrated=true | 合法值通过，但真实 kind 重盖为 false；非法 p 仍被拒绝 |
| NoConclusion.floor=[]、[N]、[S,N]、[S,C,N] | 均合法，非空集保持顺序；耗尽链为 Abstain，全集 floor 也不变 Decided |
| stage0 Rule floor=[N]；stage1 Encoder floor=[]；stage2 Deep 畸形返回 | InvalidResponse(index=2,kind=deep)，保留 [N]/rule；stage3 调用 0 |
| stage0 Rule floor=[N]；stage1 Encoder floor=[S]；stage2 Encoder 畸形 floor=[C,答案x] | 保留 [S]/encoder，失败 index=2/kind=encoder；不抢救 C、不合并 N |
| stage0 Encoder 畸形返回，无旧 floor | answers=[]、backend=encoder；失败 index=0；下一层调用 0 |
| 有效 floor 后真实 Unavailable；仅空 floor；无后端 | 前者遵循 L1 terminal/身份；空 floor 不造答案；无后端合法 Q 保持旧 Abstain/Rule |
| 同时错：未知 ID+p=NaN；重复 ID+错类型；缺覆盖+非法 p | 分别优先 unknown_question_id、duplicate_answer_id、invalid_probability |
| 固定 latency_ms=0 的序列化快照 | 有效三种旧 outcome 的字节对照、以下新错误快照；旧日志/profile fixtures 无 diff |

非法请求仅序列化 Err 内的 InvalidRequest；stage1 Encoder 畸形响应则保留 stage0 Rule 的 N（第二例为完整 Decision fixture）：

```json
{"code":"empty_questions"}
{"answers":[{"question_id":"n","value":{"type":"bool","value":true},"p":null,"calibrated":false}],"backend":"rule","model":null,"latency_ms":0,"outcome":{"kind":"invalid_response","code":"answer_coverage","failed_stage_index":1,"failed_backend_kind":"encoder"}}
```

旧 Outcome 快照：前两例与基线字节相同；第三例沿用获批 L1 形状，区别于基线的 reason-only Unavailable。

```json
{"kind":"decided"}
{"kind":"abstain","reason":"no backend reached a conclusion"}
{"kind":"unavailable","reason":"encoder unavailable","failed_stage_index":1,"failed_backend_kind":"encoder"}
```

## 消费者与后续切片

本基线搜索 `rust/ apps/ packages/ protocol/`：结论不涵盖未知仓外消费者。

| 表面 | 实际消费者 / 后续影响 |
|---|---|
| decide / Outcome / Answer | crate `src/service.rs`、`src/types.rs` 测试；RuleBackend closure；没有产品推理调用。S 片更新测试取出 Ok、新穷举/JSON fixtures |
| ONNX | `tests/onnx_spike.rs` 直接 predict，不实现 DecisionBackend、不消费本 API |
| profile REST / TS | `agent24d/src/decide_routes.rs` + `server.rs:1063`；`protocol/openapi.yaml:117`、`packages/api-client/src/openapi.d.ts:46`；apps 未见 getDecideProfile 调用，本片无协议改动 |
| 日志 DTO / JSONL | `src/log.rs` 测试；`agent24-store/src/decision_log.rs` opaque JSON；`agent24-cli/src/main.rs:1131` export mapper；无生产 DecisionLog writer，不消费 Outcome enum，不变 schema_version=1 |
| 未来 retain/recall | `agent24-agent/src/{retain,session_memory,lib}.rs` 的 chat/run；目前不依赖 decide；现有 WriteGate/pause/owner/active 等确定性过滤不动 |

| 后续 task（不是本次授权） | 范围与预算 / 门 |
|---|---|
| D1-2b-V | 契约精确 APPROVE 后：新私有纯 validator，≤180 生产行；不改公共错误/API 或 service 热点；纯数据矩阵 test-first |
| D1-2b-S | F/V 获批且 L1 实现已合入后：公共错误/返回签名/各层接入及 crate 根导出，≤100 生产行；fake-backend 边界与序列化 test-first |

实施前必须获得外部 PR-Daemon 对**此具体契约精确 head APPROVE**；之后各实现 PR 另行外审、CI/本地门禁及 pre-pr-check，base 仅 ab/decide、生产代码 ≤300 行（测试、文档、锁文件及生成物不计），超预算继续拆；文件/目录与其它风险仍按原版机械预检。反面对照先证明旧 service 接受畸形 Decided/保留畸形 floor，再修复；本次未写/运行 L2 代码或测试。

独立审计 `/tmp/a24-decide-start/D0-EXIT-AUDIT.{md,json}` §7 支持本切片，不是实现授权。B/A 五档设计已接受；生产运行时、资源与副作用门仍未验证。本片不改变 L3/L4、阈值/日志/组件/调用点或 David 的 K1/KB/Workspace，也不宣称 D1 完成。
