# Agent24 M1：Agent Memory 学术调研

## ① 一句话结论

**建议 M1 继续以「原文 EventLog + 有来源的显式断言 + 可审计召回/撤回」交付，优先吸收分层证据、时间语义和分项评测；把自动抽取、图记忆与反思巩固留给本地模型实验，而不因论文总分扩张冻结范围。** 这是基于下文研究的设计判断；当前交付边界见 [M1-PLAN-v2 §0、§2、§4](../../agent/M1-PLAN-v2.md)。

核查日期：2026-10-02。已通读本地方案；论文与官方源码使用 `curl -L --fail --max-time 45` 读取，搜索仅用于定位。下列实验数字均为作者自报，本次未复现实验；「未核实」表示没有足够证据，不能解释成该能力必然不存在。论文机制与当前源码分开标注；建议不代表已修改冻结方案。本文的 M1 task 编号均来自 [M1-PLAN-v2 §2](../../agent/M1-PLAN-v2.md)。

## ② 逐项条目

### 1. MemGPT：上下文分页，不等于个人事实治理

- **是什么／单元与 schema：** 将 LLM 有限上下文类比为主存：main context 包含 system instructions、可写 working context 和 FIFO 消息队列；外部 recall storage 保存消息/交互日志，archival storage 保存可读写的长文本对象。working context 是文本块，不是带有效期的原子事实表。[论文 §2](https://arxiv.org/html/2310.08560)
- **写入／召回：** 新消息进队列并持久记录；内存压力触发提醒与队列 flush，递归摘要压缩进入 prompt 的历史，旧消息仍可搜索。模型通过工具调用编辑工作区、搜索历史和 archival；archival 用 embedding cosine similarity，并分页返回。[论文 §2.1–2.4](https://arxiv.org/html/2310.08560)
- **更新／遗忘／隐私：** 支持模型编辑可写上下文，但断言级 supersede、双时间、用户撤回协议未核实；从 prompt 驱逐不等于删除 recall storage 中的消息，不能把分页称为隐私擦除。[论文 §2](https://arxiv.org/html/2310.08560)
- **评测结果：** DMR 在 Multi-Session Chat 历史上提问。表 2 的 GPT-4 Turbo + MemGPT accuracy 为 **93.4%**，同模型摘要基线 **35.3%**；基线读压缩摘要，MemGPT 可访问历史，两者信息条件不同。不能把这组差值当作相对全量上下文的提升。[论文 §3.1、表2](https://arxiv.org/html/2310.08560)
- **优缺点／M1 关系（判断）：** 最可取的是存储与工作上下文分离；模型自主分页则增加控制路径。T03/T04 已选择更明确的 `covered_through_seq` 与原文永存契约，宜保持；无需为 M1 改成模型自主调用记忆工具。MemGPT 的问答实验不能替代 T03–T05 的事务、重启与原文完整性验收。[机制依据](https://arxiv.org/html/2310.08560)；[M1 F2、T03–T05](../../agent/M1-PLAN-v2.md)

### 2. Generative Agents：记忆流、反思与三因素排序

- **是什么／单元与 schema：** 用 memory stream、reflection、planning 驱动虚拟居民；自然语言记忆带创建及访问时间，既收录观察，也收录由多条记忆归纳出的 reflection。[论文 §4](https://arxiv.org/html/2304.03442)
- **写入／召回：** 观察即时写入；LLM 为重要性打 1–10 分。召回组合归一化后的 relevance、importance、recency；relevance 为 embedding 相似度，recency 按距上次检索的游戏小时指数衰减，文中系数 **0.995**，三项等权。反思在累积重要性达到阈值时生成，再写回记忆流。[论文 §4.1–4.2](https://arxiv.org/html/2304.03442)
- **更新／遗忘／隐私：** 反思增加高层推断，并非确定性的事实替换；衰减改变可访问性，不构成用户删除协议。断言冲突、supersede、物理擦除未核实；作者明确讨论 memory hacking 与编造经历风险。[论文 §4、§8.3](https://arxiv.org/html/2304.03442)
- **评测结果：** 25 个 agent 在 Smallville 运行两个游戏日；受控访谈的完整架构在 believability 排名上优于消融条件，另观察传播与协调。论文同时记录回忆时添加细节的失败。该实验衡量行为可信感，不能解读为个人事实准确率。[论文 §6.5、§7](https://arxiv.org/html/2304.03442)
- **优缺点／M1 关系（判断）：** 排序的三个因素便于拆分消融，但“经常被召回”会反过来提高 recency，不能充当真实性证据。T07 可借鉴多因素实验；M1 先保留 T07a 的 BM25 基线。反思应属于模型派生知识，不能绕过 T06 的 WriteGate 升格成 UserSaid。[机制依据](https://arxiv.org/html/2304.03442)；[M1 T06、T07a、T07、§4](../../agent/M1-PLAN-v2.md)

### 3. Reflexion：任务经验记忆，和用户事实记忆分开

- **是什么／单元与 schema：** Actor 执行任务，Evaluator 提供反馈，Self-Reflection 生成自然语言经验，保存在 episodic memory buffer；通过后续 trial 的上下文改进行为，不更新模型权重。[论文 §3、算法1](https://arxiv.org/html/2303.11366)
- **写入／召回／更新：** 在试验反馈后写反思，下次尝试读取缓冲；不是用户每轮发言自动抽事实。缓冲大小依实验配置，编程实验只保留一条经验。用户事实的有效区间、冲突更新、撤回与隐私擦除未核实。[论文 §3、§4.3](https://arxiv.org/html/2303.11366)
- **评测结果：** HumanEval Python 的论文 pass@1 为 **91.0%**，表 1 列出的 GPT-4 单次生成对照为 **80.1%**；但 Reflexion 过程允许测试反馈与迭代，不能等同一次无反馈生成。MBPP Python 为 **77.1%**，低于同表对照 **80.1%**；错误测试造成误判是文中明确限制。[论文 §4.3、表1–2、§5](https://arxiv.org/html/2303.11366)
- **优缺点／M1 关系（判断）：** 适合记录“哪种行动为何失败”，依赖反馈质量；不适合作为用户偏好的权威来源。未来可放进独立的 procedural/experience store，M1 的 T06/T06b 仍应区分用户陈述、模型反思和工具反馈；不把 pass@1 作为 T08 记忆效果指标。[机制依据](https://arxiv.org/html/2303.11366)；[M1 T06、T06b、T08、§4](../../agent/M1-PLAN-v2.md)

### 4. MemoryBank：衰减与强化的启发，不能照搬为删除规则

- **是什么／单元与 schema：** 面向长期陪伴，保存带时间戳的逐轮对话、每日事件摘要、全局摘要与用户画像；对话片段和事件摘要作为可检索 memory piece。[论文 §2.1](https://arxiv.org/html/2305.10250)
- **写入／召回：** 按轮积累对话，按日归纳事件及画像；双塔 dense retriever 编码记忆与当前上下文，使用 FAISS 相似性搜索。画像更新依靠新的归纳，而非 assertion 级事务更新。[论文 §2.1–2.2](https://arxiv.org/html/2305.10250)
- **更新／遗忘／隐私：** 简化 Ebbinghaus 模型 `R = exp(-t/S)`：初始强度 `S=1`，回忆后 `S` 加 1、计时重置。作者将其称为探索性简化；公式不能证明低频事实不再重要。落盘物理删除、备份擦除及事实冲突协议未核实。[论文 §2.3](https://arxiv.org/html/2305.10250)
- **评测结果：** 15 个模拟用户、10 天对话、194 道人工探测题，中英各 97 道。SiliconFriend-ChatGPT 的 retrieval accuracy：英文 **0.763**、中文 **0.711**；对应回答 correctness **0.716／0.655**，后者允许半分。检索正确与回答正确是两层指标，不应混用。[论文 §4.2、表2](https://arxiv.org/html/2305.10250)
- **优缺点／M1 关系（判断）：** 适合研究访问频率与摘要层次，模拟数据和探索性遗忘不足以支撑产品删除政策。可为 T03/T04 提供摘要分层参照；T09 必须继续由用户撤回控制失效。长期未提及的过敏等事实不应仅因衰减自动消失；原文删除更违反 M1 契约。[机制依据](https://arxiv.org/html/2305.10250)；[M1 F2、T09、§0](../../agent/M1-PLAN-v2.md)

### 5. A-MEM：Zettelkasten 式自组织笔记

- **是什么／单元与 schema：** 原子 note 为 `m={content, timestamp, keywords, tags, context, embedding, links}`；content 保留原始交互，keywords/tags/context 由 LLM 生成。这里的“原子”是笔记组织单位，不必然等于单个可验证命题。[论文 §3.1](https://arxiv.org/html/2502.12110v11)
- **写入／召回：** 新交互到达后建 note，用 embedding 查近邻，再让 LLM 判断是否建立 links；新 note 还可触发旧 note 的 context、keywords、tags 演化。§3.4 明确的查询步骤仍是 query/note cosine similarity top-k；不据 links 的存在推断另有图遍历检索。[论文 §3.1–3.4](https://arxiv.org/html/2502.12110v11)
- **更新／遗忘／隐私：** memory evolution 会改变旧笔记的派生描述；审计版本链、`supersedes`、事实有效区间和用户擦除协议未核实。语义丰富化不能自动视作来源真实性提高。[论文 §3.3、§6](https://arxiv.org/html/2502.12110v11)
- **评测结果：** 论文表 1 的 GPT-4o-mini、LoCoMo temporal F1 为 **45.85**，其 LoCoMo 基线 **18.41**；但 adversarial F1 为 **50.03**，低于该基线 **69.23**。均是百分制 F1，不是 LLM judge accuracy；关联和演化消融有收益，不代表每个题型都改善。[论文 §4、表1–2](https://arxiv.org/html/2502.12110v11)
- **优缺点／M1 关系（判断）：** 可以补足同义表达和跨记忆关联，代价是生成描述与链路的误差传播。T06b 可借 note 的结构化候选，但派生字段应链接原始 EventId，并保留 Model 来源；不能直接覆盖旧 UserSaid 断言。图式自组织留到 T07b 之后验证。[机制依据](https://arxiv.org/html/2502.12110v11)；[M1 T06、T06b、T07b、FU-M1-2](../../agent/M1-PLAN-v2.md)

### 6. HippoRAG／HippoRAG 2：用图关联检索原文证据

- **是什么／单元与 schema：** HippoRAG 用 LLM OpenIE 从文档抽实体与关系，知识图承担关联索引，原文 passage 仍是回答证据；查询实体映射到图节点，再用 Personalized PageRank 扩散到相关实体与 passages。[论文 §2](https://arxiv.org/html/2405.14831)
- **写入／召回／更新：** 核心流程是文档摄入建图与在线查询，并非规定“聊天会话结束才抽取”。召回把 query seed 的图传播得分汇总到 passage；个人事实冲突、用户撤回、双时间及隐私删除协议未核实。[论文 §2–3](https://arxiv.org/html/2405.14831)
- **评测结果：** 在 MuSiQue、2WikiMultiHopQA、HotpotQA 抽样 dev 集上评估。单步检索表 2：ColBERTv2 平均 R@5 **65.6**，HippoRAG + ColBERTv2 **72.9**；另报 QA EM/F1，检索 recall 不是答案准确率。结果来自文档多跳任务，不能直接与个人聊天基准对排。[论文 §3.1–3.5、表2–4](https://arxiv.org/html/2405.14831)
- **2025 补充：** HippoRAG 2 扩展为更强的 continual knowledge integration，评测区分 factual memory、sense-making 和 associativity；目前官方仓库 main 对应第二代，第一代放在 legacy。第二代的个人事实撤回语义本次未核实，不将“continual”理解为已解决时序冲突。[第二代论文](https://arxiv.org/html/2502.14802)；[官方 README](https://github.com/OSU-NLP-Group/HippoRAG)
- **优缺点／M1 关系（判断）：** 适合词面不重合但实体链路相关的多跳问题；OpenIE 与实体链接会引入额外摄入成本和错误。可借 T08 的 retrieval/QA 分层评价；对 M1 的个人小库，先验证 FTS 与本地向量的增益，再决定是否加图。它应是 T07b 后续候选索引，不替换 AssertionLedger。[机制依据](https://arxiv.org/html/2405.14831)；[M1 T07a、T07b、T08](../../agent/M1-PLAN-v2.md)

### 7. Mem0：增量抽取与四操作；2026 实现已有变化

- **是什么／单元与 schema：** 2025 论文基础版保存自然语言事实记忆，图版另用实体节点与有标签关系；实体含 type、embedding、创建时间，关系为 `(source, relation, destination)`。[论文 §2](https://arxiv.org/html/2504.19413)
- **写入／召回：** 每个新 message pair 触发抽取，输入包含 conversation summary 与近期消息；候选与向量召回的旧记忆比较，由 LLM 选择 `ADD / UPDATE / DELETE / NOOP`。论文实验近期消息数和比较记忆数均为 10，LLM 操作使用 GPT-4o-mini；summary 可异步刷新，不应把整个系统归类成只在会话后写入。[论文 §2.1](https://arxiv.org/html/2504.19413)
- **更新／遗忘／隐私：** 四操作使增量维护明确，但 LLM 判矛盾后的 DELETE 不等于用户授权撤回，也未证明 raw log、向量、备份会一起擦除；双时间账本及可回溯 supersede 链未核实。[论文 §2.1–2.2](https://arxiv.org/html/2504.19413)
- **评测结果：** 论文 LoCoMo 实验排除 adversarial；单独报告 F1、BLEU-1、LLM judge。摘要的 **26% 是相对该文 OpenAI 对照的 judge 增益**；**91% p95 延迟降幅**则相对 full-context，是另一比较对象。不能把这两项合成“所有指标全面优于所有方案”，更不能据此声称拒答能力得到验证。[论文 §3、§4及摘要](https://arxiv.org/html/2504.19413)
- **版本核查：** 当前官方 README 的 April 2026 更新描述 **ADD-only、无 UPDATE/DELETE、记忆累积**，并将 agent 生成事实纳入；README 明示榜单来自含专有优化的 managed platform，开源 SDK 不保证相同分数。因此本文四操作只指 2025 论文。[固定提交 README](https://github.com/mem0ai/mem0/blob/abb81c88e1f738a8117d8293530fbc31a5ef8fd9/README.md)
- **优缺点／M1 关系（判断）：** 四操作很适合设计未来候选变更接口，但不能直接授权模型删除或提升信任。T06b 宜输出候选和证据，再经 WriteGate；T09 继续用同事务失效与撤回事件。自动 UPDATE 留给 FU-M1-2；2026 README 的 agent facts 机制也不应改变 T06 的“只看用户 prompt”。[论文依据](https://arxiv.org/html/2504.19413)；[M1 T06、T06b、T09、FU-M1-2](../../agent/M1-PLAN-v2.md)

### 8. Zep／Graphiti：对 M1 最有价值的是双时间和 provenance

- **是什么／单元与 schema：** Zep 论文用 Graphiti 组织 episodic 原始输入、semantic 实体/事实边、community 摘要；事实可以追溯源 episodes。边上的两组时间区分系统何时记录/失效，以及现实中何时生效/失效。[论文 §2](https://arxiv.org/html/2501.13956v1)
- **写入／召回：** episode 摄入时结合近期消息抽实体/关系、解析相对日期；新边与相关旧边由 LLM 比较。查询结合 embedding、BM25 和 BFS 图扩展，再 rerank。[论文 §2.2、§3](https://arxiv.org/html/2501.13956v1)
- **更新／遗忘／隐私：** 时间重叠的矛盾使旧边有效期截止到新边生效时间，并保留历史；论文按摄入时间优先新信息，因此它仍不等价于真实性裁判。失效保留历史也不等价于隐私删除。当前 `EntityEdge` 确有 `episodes`、`created_at`、`expired_at`、`valid_at`、`invalid_at`；这是源码核验，不是把当前代码当成论文实验快照。[论文 §2.2.3](https://arxiv.org/html/2501.13956v1)；[固定提交 edges.py](https://github.com/getzep/graphiti/blob/3c427640abf909f12f71f963fce15eb514a3c493/graphiti_core/edges.py)
- **评测结果：** LongMemEval-S 表 2，GPT-4o full-context **60.2% → Zep 71.2%**，即 **+11.0 个百分点**；平均上下文从约 115k 到 1.6k tokens。分项却不全改善：GPT-4o-mini knowledge-update **76.9% → 74.4%**，GPT-4o single-session-assistant **94.6% → 80.4%**。图和时间字段存在，不保证模型正确使用它们。[论文 §4.3、表2–3](https://arxiv.org/html/2501.13956v1)
- **优缺点／M1 关系（判断）：** 原文、派生事实、时间窗口分离最值得借鉴；实体消歧、矛盾识别和图维护显著扩大实现面。M1 先澄清 `recorded_to` 是账本撤回时间，不把它当事实失效日期；有效区间与 supersede 是 FU-M1-2 的候选 schema，无需引图数据库进入 T06/T09。[机制依据](https://arxiv.org/html/2501.13956v1)；[M1 T06、T09、§4、FU-M1-2](../../agent/M1-PLAN-v2.md)

### 9. MemoryOS：分层巩固与热度管理

- **是什么／单元与 schema：** 此处特指 *Memory OS of AI Agent*，arXiv:2506.06326。STM 存近期 dialogue pages，MTM 以主题 segments 组织页面与摘要，LPM 保存用户及 agent 的长期特征/知识；不是把任意名为 MemOS 的项目视作同一工作。[论文 §3](https://arxiv.org/html/2506.06326)
- **写入／召回：** STM 按 dialogue-chain FIFO 更新到 MTM；`heat = α×检索次数 + β×segment页数 + γ×距上次访问的时间衰减`，超过阈值（文中为 5）触发 LPM 特征/知识更新。语义相关性用于另一条召回流程：先找相关 segment 再取 pages，并提供长期个人信息。巩固由容量/热度驱动，不限于 session close。[论文 §3.2–3.4](https://arxiv.org/html/2506.06326)
- **更新／遗忘／隐私：** MTM 超容量时淘汰最低 heat segment；这是论文明确的层内淘汰。是否同时擦除原始对话、其他副本及备份，以及双时间冲突解决、用户撤回后跨层清理，均未核实；不能将 eviction 解释为产品级隐私擦除。[论文 §3](https://arxiv.org/html/2506.06326)
- **评测结果：** 摘要自报 LoCoMo、GPT-4o-mini 下 F1/BLEU-1 平均相对增益 **49.11%／46.18%**，不是百分点。正文表 3 的平均 F1 为 **36.23**，同环境 A-Mem* **26.55**；平均 LLM calls 为 **4.9／13.0**，反映该文配置的质量/调用数权衡。跨论文转引的表格题型列序并不一致，本文不据这些数字制作统一排名。[MemoryOS §4、表2–3](https://arxiv.org/html/2506.06326)；[A-MEM 原表1](https://arxiv.org/html/2502.12110v11)
- **优缺点／M1 关系（判断）：** 有助于区分最近上下文、主题摘要和稳定偏好，代价是阈值调参和派生状态增多。T04 可借“摘要是派生视图”的组织思路；自动将摘要升为长期真值应留在 M1 之外。若以后巩固，必须保留 evidence 与失效传播能力，不能只复制多层名字。[机制依据](https://arxiv.org/html/2506.06326)；[M1 T03/T04、T06b、§4](../../agent/M1-PLAN-v2.md)

### 10. Hindsight：区分 evidence、observation 与 opinion

- **是什么／单元与 schema：** 找到了技术论文 *Hindsight is 20/20: Building Agent Memory that Retains, Recalls, and Reflects*。四个逻辑网络为 world、experience、observation、opinion；前两者记叙事实/经历，observation 是基于证据的实体归纳，opinion 带观点、置信度与行为倾向信息。不是把模型称作 world fact 的内容就视为经过验证。[论文 §3–5、附录A](https://arxiv.org/html/2512.12818v1)
- **写入／召回：** retain 把 chunk 提成带时间与实体的 narrative facts，后台形成 observation；recall 四路并行：semantic、BM25、graph spreading、temporal，再用 `RRF(f)=Σ 1/(k+rank_i(f))` 合并、cross-encoder rerank、token budget 截取。reflect 依据取回证据回答并可形成观点更新；实验按 session 摄入不表示产品只能会话后处理。[论文 §4、§6、§7.3](https://arxiv.org/html/2512.12818v1)
- **更新／遗忘／隐私：** 观点可被新证据强化、调整和后台合并；这是 belief evolution，不是用户事实的授权 supersede。用户 forget、物理擦除以及删除后 observation/opinion 级联失效保证未核实。[论文 §5–6](https://arxiv.org/html/2512.12818v1)
- **评测结果与可复现性：** LongMemEval-S 500 题，表 3 的 GPT-OSS-20B full-context **39.0% → Hindsight 83.6%**；该配置 retain/reflect 同用 20B，judge 为 GPT-OSS-120B。它比跨模型对比更有参考性，但仍非等调用成本实验。所核 v1 §7.3 的 retrieval token budget 仍是 `<add>` 占位；部分外部基线引自其他报告，不能当作统一复现。[论文 §7.3–7.4、表3](https://arxiv.org/html/2512.12818v1)
- **LoCoMo 口径限制：** v1 表 2 列 50 conversations，官方公开 release 为 10；不能据此确定其实际评测使用了哪套相同子集，匹配情况**未核实**，因此不采用其 LoCoMo 最高分作 M1 选型依据。[论文 §7、表2/4](https://arxiv.org/html/2512.12818v1)；[LoCoMo 官方项目页](https://snap-research.github.io/locomo/)
- **优缺点／M1 关系（判断）：** 最值得吸收的是证据与推断分开，及多路候选用 rank fusion 而非直接相加不同量纲分数；复杂抽取、图、reranker 则需本地实测。T06 的 Trust/Modality/provenance 应继续是写入边界；T07b 可试 FTS+vector RRF，M1 无需完整照搬四网络。[机制依据](https://arxiv.org/html/2512.12818v1)；[M1 T06、T06b、T07b](../../agent/M1-PLAN-v2.md)

### 11. LongMemEval：M1 评测形状的首选参照

- **是什么／数据单元：** 500 道人工设计 QA，覆盖 information extraction、multi-session reasoning、temporal reasoning、knowledge updates、abstention 五种能力。官方数据有 question/answer/question_date、带日期的 history sessions 与 answer session IDs；S 约 115k tokens，M 约 500 sessions，另有只含证据会话的 oracle 版。[论文](https://arxiv.org/html/2410.10813)；[官方数据 schema](https://github.com/xiaowu0162/LongMemEval/blob/9e0b455f4ef0e2ab8f2e582289761153549043fc/README.md)
- **写入／召回／更新／遗忘：** 它是 benchmark，要求系统随历史积累后答题，没有指定唯一存储或写入调度。knowledge-update 检验是否使用新信息；abstention 检验历史不足时能否承认未知，**不是用户撤回测试**。持久化事务、删除/隐私与权限隔离并非其官方五类能力。[论文 §3–4](https://arxiv.org/html/2410.10813)
- **指标：** 区分证据检索与最终 QA。官方 QA 脚本用 LLM judge 判 yes/no 并聚合 accuracy；temporal 允许天数等 off-by-one，knowledge-update 允许同时提到旧信息，只要正确回答新值；abstention 用专门 prompt，不能靠整体 accuracy 推知拒答率。[固定提交 evaluate_qa.py](https://github.com/xiaowu0162/LongMemEval/blob/9e0b455f4ef0e2ab8f2e582289761153549043fc/src/evaluation/evaluate_qa.py)
- **评测结果／失败模式：** 论文观察到受测商业系统及长上下文模型随历史增长出现明显记忆退化；将 indexing/retrieval/reading 分开分析，提出 session decomposition、fact-augmented keys 与 time-aware query expansion。关键问题不仅是没找到证据，也包括粒度过粗、压缩遗漏细节、日期/更新信息读错。[论文 §3.3、§4–5](https://arxiv.org/html/2410.10813)
- **优缺点／M1 关系（判断）：** 有证据定位与时间/更新题，最适合启发 T08；但 M1 仅召回显式断言、不搜索全部聊天历史，用官方原题直接测试会把范围差异当故障。仓内小集应保持“LongMemEval 形状”，另报 Hit@k 和失效泄漏；未来完整 QA benchmark 才用原数据、官方 judge 与相同预算。[benchmark 依据](https://arxiv.org/html/2410.10813)；[M1 T06–T08](../../agent/M1-PLAN-v2.md)

### 12. LoCoMo：观察长期对话中的证据粒度与回答失败

- **是什么／数据单元：** *Evaluating Very Long-Term Conversational Memory of LLM Agents* 的长期对话基准；对话附 session、时间及 turn IDs，QA 含答案和 evidence。官方公开 release 为 10 段长对话；题型包括 single-hop、multi-hop、temporal、open-domain、adversarial，另有事件图摘要与多模态对话任务。原论文总体统计与公开子集应分别记录，不把 7,512 题等总体数字自动套到所有后续实验。[论文 §3–4](https://arxiv.org/html/2402.17753)；[官方项目页](https://snap-research.github.io/locomo/)
- **写入／召回／更新／遗忘：** 原实验比较 dialog、observation、summary 等 retrieval unit；它不提供个人事实写入协议。时序问题关注事件顺序/日期，adversarial 关注未提供信息；两者都不能替代 supersede、撤回、权限与物理擦除验收。[论文 §4–5](https://arxiv.org/html/2402.17753)
- **指标与实现差异：** 原生 QA 主要用 normalized token F1，多跳答案做子答案匹配；RAG 另报 evidence recall。公开脚本的 adversarial 分支按特定拒答字符串评分，不能直接搬到中文；脚本缺少 context/evidence 时会将相应 recall 记为 1，接评测时必须显式提供证据字段。Mem0/Hindsight 的 LLM judge 分数不是这个原生 F1。[固定提交 evaluation.py](https://github.com/snap-research/locomo/blob/3eb6f2c585f5e1699204e3c3bdf7adc5c28cb376/task_eval/evaluation.py)；[Mem0 §3.2](https://arxiv.org/html/2504.19413)；[Hindsight §7.2](https://arxiv.org/html/2512.12818v1)
- **评测结果／失败模式：** 原论文表 3，GPT-3.5-turbo-16k 用 top-5 observations 的 overall F1 **41.4**，top-50 为 **37.8**；更多上下文未必更好。论文还观察到时序推理弱、事件归错说话人、长上下文下无答案题幻觉。不能把相关片段找到就视为任务成功。[论文 §6.1、表2–3](https://arxiv.org/html/2402.17753)
- **优缺点／M1 关系（判断）：** 优点是能测长程关联、证据粒度和干扰，限制是原生词面指标、数据子集与后续 judge 口径不同。T08 应保留说话人、干扰和无答案案例；T07 的 token 预算不能只追求塞满，需观察增加 k 是否提高误注入率。[benchmark 依据](https://arxiv.org/html/2402.17753)；[M1 T07、T08](../../agent/M1-PLAN-v2.md)

## ③ 对 M1 的启发

以下三栏均为**研究建议**。左栏优先补验收与契约说明；涉及产品行为的变化须另作方案修订，不以本报告改动已冻结的任务。T06b/T07b 仍是笔记本上的可选项，自动 supersede、consolidator、物理擦除仍在 M1 之外。[现有边界：M1 §0、§2、§4–5](../../agent/M1-PLAN-v2.md)

| 应该吸收（具体改法与 task） | 可以借鉴但 M1 不做 | 不适合我们（理由） |
|---|---|---|
| **证据与派生内容分层。T03/T04/T06：** 保留 `EventId → assertion` 证据链；验收断言来源可回查，摘要覆盖以 `covered_through_seq` 为准。模型生成描述不得因重复出现变成 UserSaid。[MemGPT](https://arxiv.org/html/2310.08560)、[Hindsight](https://arxiv.org/html/2512.12818v1)、[M1](../../agent/M1-PLAN-v2.md) | A-MEM links、Hindsight observations 可做未来派生索引；依赖原始 evidence，并记录生成配置，失效后可重建。[A-MEM](https://arxiv.org/html/2502.12110v11) | 允许反思或笔记演化直接覆盖用户权威事实；这会把推断和来源混淆，破坏当前 WriteGate 边界。[M1 T06](../../agent/M1-PLAN-v2.md) |
| **时间语义先讲清。T06/T08/T09：** fixture 区分“今天记录、上月发生”和“今天撤回”；`recorded_to` 只解释为账本失效。尚无有效期的场景标不支持，不能用新插入时间冒充事件时间。[Zep](https://arxiv.org/html/2501.13956v1)、[M1](../../agent/M1-PLAN-v2.md) | FU-M1-2 研究 `valid_from/valid_to`、`supersedes` 与原始日期文本；区分“事实变了”和“以前记错”。新事实、关闭旧窗口、审计事件应有原子提交边界。[Zep](https://arxiv.org/html/2501.13956v1) | 无条件 last-write-wins：晚录入的历史事实未必是当前状态；模型判矛盾也不能等同用户授权撤回。[Zep §2.2.3](https://arxiv.org/html/2501.13956v1) |
| **撤回验收覆盖重建。T09/T10/T07：** 在撤回前后、重启和 FTS rebuild 后检查返回/注入的 assertion IDs；分别验证 REST 搜索与真实 run，沿用事务失败整体回滚要求。[M1 T07、T09、T10](../../agent/M1-PLAN-v2.md) | 增加向量、摘要知识或图之后，建立派生依赖失效与缓存清理；另外设计物理擦除及备份策略，不混进当前 forget。[Hindsight 派生网络](https://arxiv.org/html/2512.12818v1)、[M1 §4](../../agent/M1-PLAN-v2.md) | 用 MemoryBank 衰减或 MemoryOS eviction 当用户“删除”；衰减控制访问，原文/副本是否擦除是另一契约。[MemoryBank](https://arxiv.org/html/2305.10250)、[MemoryOS](https://arxiv.org/html/2506.06326) |
| **写入时机与提交分开。T06：** 保持 `append_turn` 成功后才能提交带 evidence 的显式断言；失败时不能留下无来源记忆。T06b 若实验，单独统计摄入成本与候选积压，不改变 T06 成功语义。[M1 T06/T06b](../../agent/M1-PLAN-v2.md) | 实时抽取和会话后批量抽取做本地 A/B；抽取产物经 WriteGate，不自动获得 Commit。按 EventId 幂等，另考虑模型配置变化后的重提取。[Mem0 增量流程](https://arxiv.org/html/2504.19413) | 因论文常同时处理 user/assistant pair，就把助手自述当用户事实；它与 M1 明确只读用户 prompt 的约束冲突。[Mem0](https://arxiv.org/html/2504.19413)、[M1 T06](../../agent/M1-PLAN-v2.md) |
| **召回先测筛选正确性。T07a/T07/T08：** 记录 k、预算、候选/最终注入 IDs；增加词面重合却语义无关的负例。qualification、owner、撤回状态的过滤必须优先于排序，召回审计与实际注入一致。[M1 T07a–T08](../../agent/M1-PLAN-v2.md) | T07b 在同一 fixture 上对比 FTS、vector、FTS+vector RRF；实体扩展及 reranker 后置。不要直接把 BM25 与 cosine 原始分值相加。[Hindsight §4.2](https://arxiv.org/html/2512.12818v1) | 把 Generative Agents 的重要性评分、访问热度或图中心性当真实性/授权依据；这些是选择上下文的信号。[Generative Agents](https://arxiv.org/html/2304.03442)、[M1 WriteGate](../../agent/M1-PLAN-v2.md) |
| **按失败类型报告。T08：** 维持最多 20 个中文为主的手写样本及 5 条干扰断言，补 Hit@1/5、证据完整率、负例误注入率、撤回泄漏率，仍只记录不设统计门禁。[LongMemEval](https://arxiv.org/html/2410.10813)、[M1 T08](../../agent/M1-PLAN-v2.md) | 完整 LongMemEval-S/LoCoMo + 本地 LLM QA、固定 judge 与成本消融，等 T06b/T07b 有可测实现后做。[官方 LME](https://github.com/xiaowu0162/LongMemEval)、[LoCoMo](https://snap-research.github.io/locomo/) | 用不同题集、模型、judge、token budget 的最高分排名选型；把 retrieval hit 当最终答案质量，或把 F1 与 judge accuracy 混比。[LoCoMo 指标](https://arxiv.org/html/2402.17753)、[LME judge](https://github.com/xiaowu0162/LongMemEval/blob/9e0b455f4ef0e2ab8f2e582289761153549043fc/src/evaluation/evaluate_qa.py) |
| **把原文保留限制写进产品语义。T10/T11：** “撤回”确认文案明确不再参与断言召回，但对话日志仍在；错误时不把列表清空。验收区分跨会话断言召回与原会话上下文。[M1 §0、F2、T10/T11](../../agent/M1-PLAN-v2.md) | 长期事实更正、历史 as-of 查询、删除导出与隐私保留策略应独立设计。[Zep 双时间](https://arxiv.org/html/2501.13956v1)、[M1 §4](../../agent/M1-PLAN-v2.md) | 承诺“撤回后模型绝不再知道”：现方案保留原文，且摘要/同会话 tail 可能仍含内容；这种承诺超出当前实现边界。[M1 F2、T09](../../agent/M1-PLAN-v2.md) |

### 最小 schema 与生命周期判断

不建议 M1 立刻拆分用户明确让记住的一句话：T06 当前 `subject="user" / predicate="said_to_remember" / object=原句` 保留原意、确定性强，但一句可以包含多项事实，今后不能直接把它当可独立更新的属性槽。**M1 保留这个粒度，先在评测标出复合句；T06b/FU-M1-2 再研究拆分。** [M1 T06](../../agent/M1-PLAN-v2.md)；[A-MEM note 粒度](https://arxiv.org/html/2502.12110v11)；[LongMemEval value 粒度分析](https://arxiv.org/html/2410.10813)

未来候选 schema 建议至少区分：`owner / assertion_id / claim_text / evidence_event_ids / source_trust / modality / recorded_at / recorded_to`，以及可选的 `valid_from / valid_to / supersedes / derivation_config`。这是**建议字段清单，不是声称 M1 已有的类型定义**；未知日期留空并保留原话，不能自动补成摄入时间。Trust 表示来源类别，Modality 表示陈述性质，模型 confidence 则只是模型判断，三者不应相互替代。[M1 WriteGate 与 evidence 设计](../../agent/M1-PLAN-v2.md)；[Graphiti 时间字段](https://github.com/getzep/graphiti/blob/3c427640abf909f12f71f963fce15eb514a3c493/graphiti_core/edges.py)；[Hindsight opinion schema](https://arxiv.org/html/2512.12818v1)

值得提前补的两个生命周期案例：①“我以前住北京，现在住上海”晚于“现在住上海”摄入；②撤回后再次要求记住**完全相同**的句子。前者检验时间与摄入顺序混淆；后者针对 T06 的 `sha256(owner ‖ object)` 幂等 ID 与 T09 失效的组合，现方案未明确重新激活还是新建修订，**不能只凭论文替它作决定**，应记录成待定义语义与诊断用例。[Zep 双时间依据](https://arxiv.org/html/2501.13956v1)；[M1 T06/T09、FU-M1-2](../../agent/M1-PLAN-v2.md)

### T08 可直接采用的评测设计建议

建议保持 ≤20 例：4 例单事实/中文完整问句，3 例多事实或跨会话，3 例时间表达，3 例更新与撤回生命周期，3 例无答案或词面干扰，2 例来源信任，2 例 owner 隔离；共用至少 5 条干扰断言。前提是正例事实通过显式“记住”进入账本；自动提取、自动 supersede、历史有效期推理单列为**能力诊断**，不能按现冻结承诺要求它们全过。[M1 T06–T08](../../agent/M1-PLAN-v2.md)；[LME 能力分类](https://arxiv.org/html/2410.10813)

建议每例保存 `case_id、query、query_time、input_events、qualified_assertion_ids、gold_evidence_ids、expected_recalled_ids、forbidden_ids、answerable`；日期用明确值，避免测试执行日期改变语义。用以下指标把问题定位到检索、注入或阅读，而不是只报一个“命中率”。字段及公式是本报告建议，并非宣称官方 benchmark 原样使用。[设计参照：LME schema](https://github.com/xiaowu0162/LongMemEval/blob/9e0b455f4ef0e2ab8f2e582289761153549043fc/README.md)、[LoCoMo evidence evaluator](https://github.com/snap-research/locomo/blob/3eb6f2c585f5e1699204e3c3bdf7adc5c28cb376/task_eval/evaluation.py)

| 层次 | 建议指标与分母 | 能揭示的问题 |
|---|---|---|
| 候选召回 | Hit@k：可回答正例中至少一个 gold evidence 命中的比例；另报逐例 evidence recall | 单事实召回是否成功；避免将多跳“找到一半”当完成 |
| 证据完整 | All-evidence@k：需多条证据的正例中全部 gold evidence 命中的比例 | 跨会话、多事实和时序题缺少哪一环 |
| 过滤/注入 | 无相关断言负例中的误注入比例；撤回/held/跨 owner 用例中出现 forbidden ID 的比例 | 搜索排序不错却违反产品边界；候选与最终 prompt 不一致 |
| 生命周期 | 原文保存、重启、撤回、rebuild 的确定性断言 | 与模型无关的持久化和失效错误；沿用 T03–T10 现有正确性门禁 |
| 回答（后续独立实验） | 固定 reader/judge 的答案正确率、时间题正确率、更新题正确率；无答案题拒答率单列 | 证据已召回但读错、用旧值、编造答案；T08 单独跑搜索不能给出这些分数 |
| 资源（本地可选项） | 摄入与查询分别记录调用数、tokens、耗时；带模型/embedding 配置、k、预算、数据及 prompt 标识 | 将后台抽取成本漏计，或把云端论文延迟误用于 oMLX |

上述统计仍遵守 T08 的“只记录不设门”；事务、权限、来源与撤回的确定性测试继续按既有任务验收，不因统计命中率高而放宽。英文原生 F1 与特定拒答字符串不直接搬到中文，本地 QA judge 也不能未经校准就声称等价官方分数。[M1 T03–T10](../../agent/M1-PLAN-v2.md)；[LoCoMo evaluator](https://github.com/snap-research/locomo/blob/3eb6f2c585f5e1699204e3c3bdf7adc5c28cb376/task_eval/evaluation.py)；[LME judge](https://github.com/xiaowu0162/LongMemEval/blob/9e0b455f4ef0e2ab8f2e582289761153549043fc/src/evaluation/evaluate_qa.py)

## ④ 参考链接列表

以下为实际查阅的一手资料；论文数字以正文注明的表格/版本为准，GitHub 固定提交链接用于锁定本次实现核查。

1. 本地设计：[M1-PLAN-v2.md](../../agent/M1-PLAN-v2.md)。
2. MemGPT：[论文 HTML](https://arxiv.org/html/2310.08560)。
3. Generative Agents：[论文 HTML](https://arxiv.org/html/2304.03442)。
4. Reflexion：[论文 HTML](https://arxiv.org/html/2303.11366)。
5. MemoryBank：[论文 HTML](https://arxiv.org/html/2305.10250)。
6. A-MEM：[论文 v11](https://arxiv.org/html/2502.12110v11)、[当前实现 README（其中另指向论文评测代码，不是实验快照）](https://github.com/agiresearch/A-mem)。
7. HippoRAG：[第一代论文](https://arxiv.org/html/2405.14831)、[第二代论文](https://arxiv.org/html/2502.14802)、[官方仓库](https://github.com/OSU-NLP-Group/HippoRAG)。
8. Mem0：[2025 论文](https://arxiv.org/html/2504.19413)、[核查时 README 固定提交](https://github.com/mem0ai/mem0/blob/abb81c88e1f738a8117d8293530fbc31a5ef8fd9/README.md)。
9. Zep/Graphiti：[论文 v1](https://arxiv.org/html/2501.13956v1)、[EntityEdge 源码固定提交](https://github.com/getzep/graphiti/blob/3c427640abf909f12f71f963fce15eb514a3c493/graphiti_core/edges.py)。
10. MemoryOS：[论文](https://arxiv.org/html/2506.06326)、[官方仓库](https://github.com/BAI-LAB/MemoryOS)。
11. Hindsight：[技术论文 v1](https://arxiv.org/html/2512.12818v1)、[官方仓库](https://github.com/vectorize-io/hindsight)。
12. LongMemEval：[论文](https://arxiv.org/html/2410.10813)、[数据 schema](https://github.com/xiaowu0162/LongMemEval/blob/9e0b455f4ef0e2ab8f2e582289761153549043fc/README.md)、[QA judge 源码](https://github.com/xiaowu0162/LongMemEval/blob/9e0b455f4ef0e2ab8f2e582289761153549043fc/src/evaluation/evaluate_qa.py)。
13. LoCoMo：[论文](https://arxiv.org/html/2402.17753)、[官方项目页](https://snap-research.github.io/locomo/)、[评测源码固定提交](https://github.com/snap-research/locomo/blob/3eb6f2c585f5e1699204e3c3bdf7adc5c28cb376/task_eval/evaluation.py)。
