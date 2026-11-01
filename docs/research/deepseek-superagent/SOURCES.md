# DeepSeek Super-agent 研究：来源清单

> 2026-11-01，Fable 整理。本清单只记录来源、访问情况和客观要点，不含评价。
> 同一份清单原样交给 Codex（gpt-6-astra）做独立评估。
> 访问工具：WebFetch（网页）、`gh api`（GitHub 内容与元数据）、`curl` + 本地 HTML 转文本（文章正文）。下载件一律放在会话 scratchpad 的独立目录，未执行其中任何代码。

## 1. 访问总览

| # | 来源 | 类型 | 访问 | 备注 |
|---|---|---|---|---|
| S1 | 文章《DeepSeek Super-agent工程实践：从事件接入到可靠交付》 https://blog.mushroom.cv/blog/deepseek-superagent-engineering-guide/ | 博客文章（中英双语） | 成功 | 2026-10-31 发布，CC BY 4.0。正文用 curl 取回并转文本，中英两版都在 |
| S2 | `deepseek-ai/dsh-superagent-prototype` https://github.com/deepseek-ai/dsh-superagent-prototype | 仓库（研究原型） | 成功 | README、SETUP.md、docs/000–008 全部读完；源码只看目录树 |
| S3 | S2 的 `docs/001-domain.md` | 文档 | 成功 | 领域模型 |
| S4 | S2 的 `docs/002-authorization.md` | 文档 | 成功 | 授权 |
| S5 | S2 的 `docs/006-deployment.md` | 文档 | 成功 | 部署与权限 |
| S6 | S2 的 `docs/008-cookbook.md` | 文档 | 成功 | 新增 Source 的做法 |
| S7 | S2 的 `src/app/store.ts`、`src/app/effects.ts`、`src/sources/github-channel.ts` | 源码 | 存在（目录树核实），**未读源码** | 行为依据 `docs/007-architecture.md` 的描述 |
| S8 | DeepSeek Harness 官网 https://www.deepseek.com/en/harness/ | 产品页 | 成功 | |
| S9 | `deepseek-ai/deepseek-harness` https://github.com/deepseek-ai/deepseek-harness | 仓库（宿主） | 成功 | 只读 README 与 `packages/`、`apps/` 目录名 |
| S10 | `cordiverse/cordis` https://github.com/cordiverse/cordis | 仓库（插件框架） | 成功 | 只读 README |
| S11 | Cordis 论文 arXiv:2608.25512 https://arxiv.org/abs/2608.25512 | 论文 | 成功（仅摘要页） | 未读 PDF |
| S12 | Cordis primer https://deepseek-harness.github.io/deepseek-harness/reference/cordis-primer | 文档 | 成功 | |
| S13 | DeepSeek 原文公众号入口 https://mp.weixin.qq.com/s/zoS7GlReWPhfCjENGxXCdg | 文章 | **未尝试访问** | 文章 S1 自述直接抓取遇到访问验证 |
| S14 | 原文转载 https://www.chaincatcher.com/article/2300676 | 文章转载 | 成功（WebFetch 摘要） | 摘要由 WebFetch 的小模型生成，不是原文逐字；见 §3 |
| S15 | 离线练习包 https://blog.mushroom.cv/downloads/deepseek-superagent-engineering-lab.zip | 下载件 | 成功 | 6.5 KB，4 个文件；读了 README 与测试用例名，未运行 |
| S16 | 相关阅读 https://blog.mushroom.cv/blog/aws-infinity-agent-runtime/ | 博客文章 | 成功（摘要） | 2026-10-25 |
| S17 | 相关阅读 https://blog.mushroom.cv/blog/corespeed-tie-mac-studio-guide/ | 博客文章 | 成功（摘要） | 2026-10-31 |
| S18 | Harness 快速开始 https://deepseek-harness.github.io/deepseek-harness/en/guide/quickstart | 文档 | 未访问 | 官网页面列出，与本题关系弱 |
| S19 | GitHub topic `dsh-plugin` https://github.com/topics/dsh-plugin | 列表 | 未访问 | 同上 |
| S20 | CC BY 4.0 https://creativecommons.org/licenses/by/4.0/ | 许可证 | 未访问 | 许可证文本 |

成功 15、未尝试或未访问 5（其中 S7 为「存在但未读源码」，S13 为「未尝试」，S18–S20 与本题无关）。

## 2. 文章 S1 的忠实摘要（按章节，中文版）

来源：curl 取回的正文。括号内是文章里的原始链接。

**导语。** Super-agent 要解决的是让 Agent 持续接收事件、并行推进任务，同时减少人类维护会话和转述上下文的工作；工程落点是路由、授权、持久状态和交付恢复，不能只理解为多开几个子 Agent。DeepSeek 官方组织公开了 MIT 原型 `dsh-superagent-prototype`，它自述是研究代码、不是正式产品，与内部版本和文章描述不完全一致，部分设想未实现。相关底座：Harness 主仓库负责宿主与插件运行，Cordis 是它采用的插件框架上游；两者与研究原型不是同一个项目。固定核查提交 `6337fc823ce20251c8de8085948165c81aaa701e`，日期 2026-10-31。

**原文找到了吗，哪些内容能直接核实？** 原文《Super-agent设计探索：迈向更自主的Agent系统》的公众号入口在直接抓取和 Reader 读取时都遇到访问验证，没有取到正文；DeepSeek 官网新闻页和站点地图也没有独立全文页。全文对照用的是署名「DeepSeek产品部」、文末标明 Yujia Qiao 参与实现与写作的转载（S14），再与官方仓库 README、领域文档及源码交叉核对。文章明确不把转载域名说成 DeepSeek 官网，也不把设计讨论说成 Harness 已上线的能力。下文分三层：原文的架构思路、公开原型已落到代码的部分、本文新增的工程建议和离线练习。

**应该复制的是怎样的职责分工？** 公开原型把面向人的连续交互交给主 Session，把具体工作放进独立 Worker 上下文；事件先经程序筛选，再决定是否需要模型判断；已有任务订阅的后续事件直接送往相关 Worker，避免每条消息都让全局协调者重新分派。公开代码把概念细化为：Trigger（发生了什么事件，稳定 ID 是什么；`src/core/model.ts`）、Thread（哪些事件属于同一段持续讨论；`src/core/intake.ts`）、Policy / Eval（谁可以发起工作，是否值得接单；`src/core/ports.ts`、`evals.ts`）、Mandate（这项工作凭什么被允许，范围多大；`src/core/model.ts`）、Ticket（哪项业务工作正在推进；`src/core/tickets.ts`）、Assignment（当前哪次执行承接这张工单；`src/core/assignments.ts`）。文章认为这比保存一个聊天 Session 更重要：模型会话可以换，执行进程可以重启，但业务工单、授权依据和事件归属要保持连续；Worker 说「准备好了」不等于 PR 已合并或用户已验收。（链接：`docs/001-domain.md`）

**「订阅直通」如何避免变成绕过授权？** 新的工作请求经过准入与研判；已订阅 Thread 的事件可能作为上下文进入原有 Ticket。能补充上下文不等于能扩大授权：允许 Agent 读一个讨论串，不表示串中每个人都能要求它部署生产系统。代码与文档把 Subscription 和 Mandate 分开；要扩大范围应形成新的授权判断，而不是把新消息拼进提示词后默认执行。策略变更的语义：原型会使受影响的研判重新处理并通知运行中的任务，不能笼统宣称「改了策略就已经取消所有旧任务」；如果业务要求立即撤回某项写权限，生产实现还要把撤回落实到工具调用边界。建议保留三层独立控制：Source 识别事件与身份；Policy 决定能否发起或扩大工作；工具适配器限制实际可执行的动作；模型输出只能在这个范围内做选择。（链接：`docs/002-authorization.md`）

**为什么数据库和 Outbox 比更多 Prompt 更关键？** 原型的 Engine 是领域对象的统一写入口；Store 用 SQLite 事务保存状态；外部工作交给持久化 Effects；对外消息有 Outbox 收据。它们处理的是「模型已经决定了，但执行中断以后怎么办」。可靠路径：先在同一事务里保存决定和待执行动作，提交后再调用外部系统；不要先发评论再写数据库。故障表（故障 / 应保留的事实 / 恢复动作）：同一事件重复投递 / 稳定的 Source 事件 ID / 返回已有处理记录；工单已接、Worker 尚未启动 / Ticket 和待执行 Assignment / 继续原派工，不另建任务；Worker 会话被重启 / 执行记录、目录与外部产物 / 先核对再恢复；外部发送成功但响应丢失 / 稳定消息 ID 及待确认收据 / 查询或去重后重试；Worker 提交 ready / 报告和关联 PR 状态 / 分别等待交付或验收条件。公开 GitHub 发送适配器把消息 ID 放进 HTML 标记并在再次发评论前查找该标记，这是值得保留的对账机制，但不是跨 SQLite 和 GitHub 的全局 exactly-once 事务。多实例、网络分区或外部平台查询延迟会带来新的竞争条件；不能把单机 SQLite 的恢复测试直接换成「云端高可用已经完成」。（链接：`src/app/store.ts`、`src/app/effects.ts`、`src/sources/github-channel.ts`）

**上下文、后台执行与 Watchdog 如何落地？** 原文用 Inline、Spill 和链式召回讨论按需上下文；工程上可用有预算的读取接口：窗口内只保留必要证据与引用，完整材料留在外部，用稳定 ID 逐步取回。源码中的 MasterAssessor 已提供受限上下文读取和游标；适配时应让每个对象都有稳定 ID，读接口限制长度与页数，并把证据引用带回，而不是只给模型一个无法追溯的摘要。后台执行应有真实的作业 ID、取消接口与完成事件；前台返回「已启动」只说明接单，不代表任务成功；耗时命令的最终状态要重新进入事件流。Watchdog：文章讨论的是主系统之外的独立控制回路，还可检查任务是否取得实质进展；原型的健康状态、重试和恢复代码不能自动等同于这套语义级 Watchdog 已完整实现。建议先监控确定性指标（队列最老等待时间、重试次数、最近一次可验证产物、预算和任务阶段），需要模型判断偏航时再让独立审查器读取这些证据；对重启、停工和改策略设置权限与次数上限。这是文章建议的补齐路径，不是对原型现有功能的承诺。

**可以从哪些接口组装自己的框架？** 若继续用 DeepSeek Harness，可沿官方的 Runtime → Engine → Assessor / Executor / Outbox 结构扩展，新增来源参考 Cookbook；若换成其他模型或执行平台，先保持领域规则不动，再替换端口适配器。边界与责任：Source（事件 ID、游标、Thread 映射与来源身份）；Assessor（给定候选证据，返回受约束的研判结果）；Executor（排队、启动、追加输入、取消与汇报）；Repositories（事务、恢复与查询）；Outbox / Channel（记录意图、发送、去重及对账）；运行监督（并发、超时、预算和异常升级）。不要让 Router 直接跑长命令，也不要让 Worker 绕过交付记录直接向所有渠道发消息；角色边界应落实为接口和可见工具，而不是只在系统提示词里写一句「请遵守」。原型的目录与账号包装器不是操作系统沙箱；部署文档明确说明角色预设以 `danger-full-access` 运行，Worker 可以触达该 OS 账户可访问的资源；真正接入工作机器前应使用独立账户或隔离执行环境，并让凭据权限与任务范围匹配。（链接：`docs/006-deployment.md`、`docs/008-cookbook.md`）

**七个可运行练习验证了什么？** 文章提供一个离线工程练习，直接调用上游真实的 Engine、SQLite Store、Effects 和健康面板类；研判、执行与消息目的地用确定性替身；不连接真实模型、GitHub 或飞书。七个场景（场景 / 实际断言）：事件重放 / 重开数据库后同一 ID 仍是 duplicate；未授权发起人 / 不进入模型研判，不产生 Ticket；待派工恢复 / 数据库重开后继续原 Assignment；订阅跟进 / 后续上下文直达，Mandate 不变；ready 与 merged / PR 未合并时 ready 不自动等于合并；事务回滚 / 收据与待发送动作一起撤销；回执不确定 / 两次发送尝试对应一个带去重键的逻辑结果。最后一个场景的目的地是明确支持幂等键的替身，不能据此保证任意外部 API 不会重复执行；数据库重开不是断电实验；示例没有启动 OS Worker，不验证真实进程取消、沙箱或提示词注入抵抗。复现：检出原型固定提交，`pnpm install --frozen-lockfile`，把练习文件复制到 `tests/`，`pnpm typecheck`，`pnpm exec vitest run tests/mycelium-practice.spec.ts`。本地结果 7 项通过；上游完整 `pnpm check` 在加入练习前通过 59 个测试文件、1197 项测试。运行环境 Apple M4、16GB、macOS 26.6.2、Node 26.10.0、pnpm 11.7.0；仓库声明 Node 范围 `^22.19.0 || >=24.0.0`。这些结果验证的是离线代码行为，不是联网 Agent 完成真实研发任务的成绩。

**怎样分阶段进入真实工程？** 五步：先回放一份脱敏事件样本，只记录过滤与研判结果，检查哪些消息不应接单；接上独立 Worker 环境，只允许读取、运行测试和准备补丁，交付先保持草稿；为每种外部写操作明确去重与对账办法，加入中断、重启和响应丢失测试；接入真实模型后，单独评估误接单、漏接单、越范围动作和需要人类介入的次数；确认成本与质量后，再开放更多事件来源与并发，语义 Watchdog 放在有证据可读的基础上。原文的初步实验使用合成消息流，并以等待工具替代真实编码工作，所以它能帮助理解调度设计，不能证明真实修 bug 能力提升；订阅直通减少 Router 开销，不应被改写成已经提高了任务完成速度。建立验收指标时应同时记录端到端完成时间、有效产物、重试与外部写入次数、模型费用，以及人类花在确认和纠错上的时间；只看「开了多少 Worker」容易把高并发当成高产出。

**常见问题。** 这不是 DeepSeek Harness 的新产品功能说明：原文是架构探索，官方仓库是部分实现的研究原型，练习是离线验证材料。只多开 Worker 不能实现 Super-agent：还需要事件归属、授权依据、持久化工单、上下文预算、外部动作对账和异常恢复。原型通过测试不等于可以直接接生产账号：没有验证真实账户权限、联网模型判断、OS 隔离或生产外部写入。

## 3. DeepSeek 原文（经 S14 转载）的二手摘要

注意：以下来自 WebFetch 对转载页的模型摘要，不是逐字原文；摘要开头自述「原文并未出现 Ticket 和 Mandate 两个术语」。原文自述讨论的是工程架构思路，原型插件不是 DeepSeek 的正式产品。作者署名：DeepSeek Harness 团队共同实践，Yujia Qiao 完成系统实现与写作。

1. **Super-agent**：单任务 Agent 已能用工具解决复杂问题，人的角色上移到决策与验收；现有 Agent 以单一 Session 为单位，一个人同时只能跟进 3–5 个活跃 Session，多任务上限取决于人的注意力。定义：能跨会话保持上下文、同时推进多项任务且互不干扰的连续实体，四个特征：连续、高效、主动、持久。单个 ReAct Loop 做不到，需要五个模块：双层架构、事件驱动、面向人的交互、计算/状态/环境分离、Watchdog。
2. **双层架构**：Router 层常驻，承接人类交互与全局协调，不直接执行任务，可以不配 Bash；Router 任务通常不复杂，较轻量模型可胜任；Router 用单个常驻 LLM Session，上一轮推理期间到达的 Trigger 积压后在下一轮一并处理（批处理）；规模扩大后可做多层过滤或水平切分。Worker 层由上下文隔离的会话组成，挂载完整执行工具，通常配最强模型；Worker 不直接与人交互，向 Router 汇报，由 Router 统一回复；同层 Worker 通常无需直接通信；Worker 内部可再建 Subagent 或本身是 Agent Team。
3. **事件驱动**：输入从人逐条键入转为环境中的半结构化事件（Trigger）。3.1 两级过滤：程序层策略（Fast Path，来源、群组范围、白名单）和语义层策略（Slow Path，LLM 按提示词策略判断）。3.2 事件路由（订阅直通）：Thread 是同一 PR / 话题 / 邮件往来下的连续事件；已订阅事件由程序直接发往 Worker，超范围时 Worker 可取消订阅或交回 Router；Router 仍观测所有事件；多对多订阅让交集的 Worker 看到彼此操作。3.3 上下文组织：把上下文窗口当 Cache，两个操作 Inline（内联近期可能用到的信息）和 Spill（完整信息留在窗口外，只留引用或摘要）；渐进步骤：先给 ID 和摘要，再内联关联信息（关联对象 ID、前 50 字、相邻消息），超限则截断并保留续读入口；链式召回：所有对象有唯一 ID，召回结果也遵循 Inline & Spill，模型可逐跳展开。3.4 提前行动（投机执行）：根据环境信号在后台提前尝试，有价值则交付，无价值则丢弃；示例：会议邀约后台核对日程并备草稿；内部用户群「静默复现并修复」策略，只有确认问题并找到简单修复才交付。
4. **面向人的交互**：4.1 全双工：把耗时命令与子任务改为异步 start() / stop()，模型立即收到「已启动」，后台完成后结果作为新事件投递；该异步任务机制已在 DeepSeek Harness 实现但未默认开启。4.2 多信道：IM 轻量低打扰，单次回复不超过 100 词；GitHub、邮件、文档等异步信道结构化、高信息密度。
5. **计算、状态与环境的分离**：5.1 计算与状态分离：Agent Loop 做成云端无状态弹性容器，Session 等状态入数据库。5.2 计算与执行环境分离：Agent Loop 在云端，重状态执行环境留在原地，端侧只部署一个不保存会话状态的轻量 daemon 执行命令并返回结果；作者指出与 Anthropic「脑手分离」思路一致，但场景面向用户日常使用、可能间歇在线的个人机器；支持 BYOM，也支持云端托管；设备重新上线后云端 Agent 可自动触发该设备上的任务。
6. **Watchdog**：主系统之外的独立控制回路，由定时消息驱动；处理接口报错、状态恢复失败等基础设施故障，也由模型判断各 Worker 是否有实质进展、是否偏离目标；Watchdog 本身是具备完整工具能力的 Agent Session，维护者可用自然语言询问故障原因，也可让它在线修正主系统行为（增删提示词、调整触发规则）。
7. **初步实验**：7.1 消融：群聊与 GitHub CI 消息流驱动四种方案，每次约 300 条消息，其中约 20 条工作请求；编码工作用只消耗时间的工具替代；每种方案 5 次。结果：Super-agent p50 12.4 分钟、p95 15.7 分钟；中性提示的单 Session 5 次都没调用 Subagent，p95 29.5 分钟；提示优先委派 5 次中 4 次委派，仍慢于 Super-agent。去掉直通机制后完成时间基本一致，但 Router 决定增加约 30%，总读缓存 token 从 2323 万增至 3177 万（约 37%）。7.2 Router 批处理压力测试：Trigger 到达速率从每分钟约 1 条提升到约 40 条，观测 batch 大小、单次请求耗时、单个决定分摊的推理时间；文中未给具体数值。
8. **开放问题**：8.1 如何扩展双层架构（Router 何时切分；Worker 何时直接通信；系统与模型协同设计；跨月长程记忆是主要瓶颈）；8.2 脱离 Session 后如何协作（交互可更多发生在领域化界面；过渡期人类中途参与）；8.3 上下文边界与 A2A（跨边界协同即 Agent 间通信，不特指某协议）；8.4 安全与隐私（事件驱动增加不可信输入风险；云端与端侧默认互信，云端被入侵或提示词注入会波及所有端侧设备；端侧凭据进入模型上下文有泄露风险，需要让 Agent 不接触凭据本身就能使用凭据）。
9. **结语**：没有提出全新理论，是把经典工程思想用于 Agent 系统；内部 Super-agent 实例已持续运行，参与问题复现与修复、代码审查响应以及 Harness 自身迭代；生产力瓶颈转向人的注意力。

## 4. 原型仓库 S2 的客观要点

- 元数据：MIT；4827 stars（2026-11-01 查）；默认分支 main；只有一个提交 `6337fc8…`，提交信息 `feat: add the dsh-superagent research prototype`，时间 2026-10-31T12:05:39Z。
- README 自述：研究原型，非官方产品；大量工程由 coding agent 完成；为公开做了改编，与内部版本及文章描述不一致，文章中部分设计是未来方向。产品定位：为一个 GitHub 仓库服务的主动式 coding agent，打包为 DeepSeek Harness 的 bundle；监听 GitHub，可选飞书；有 Web 面板。工作原则：「尽量少占用人类注意力，尽量多给价值」。
- 目录：`docs/000–008`，`prompts/`（admission、cli、dispatch、handoff-*、orchestrator、replies、shared、subscriptions、team/{communication,delivery,scope}、work、worker），`skills/dsh-superagent-{feishu,github}/SKILL.md`，`src/core`（model、engine、intake、evals、tickets、assignments、replies、stage、ports、reasons、ids、memory），`src/app`（runtime、store、effects、outbox、sessions、prompts、settings、versions、assessment、notices、tools/、views/、api、http、migration、health 面板等），`src/executor`（dsh、background），`src/sources`（github*、feishu*、poll、registry、kind、host、admission-context），`src/client`（面板），`tests/` 59 个 spec，`locale/{zh,en}.json`，`cordis.patch.yml`，`scripts/dsh-superagent`。
- 000 Philosophy：行动只在被要求或书面规则允许时发生；只在人必须行动或可以用到结果时说话；一次只问一个具体问题；按影响而非努力程度停止；诚实报告。设计上：概念少、设置少、面板只显示需要人的、文档按序阅读。操作者责任：从窄开始、调成更少噪音、不用来制造数量、对它说的话负责。
- 001 Domain：流程 Source → Trigger → Thread → Eval（gate + assessment）→ Decision（accept/stop/ignore/unavailable）→ Ticket（带 Mandate）→ Assignment → Executor → Channel。Mandate 不可变，范围更宽意味着新 Ticket；只有人的 stop 撤回它，策略不再成立时吊销。Subscription 是 Ticket 与 Thread 的多对多关系，扇出不检查发送者，不授予权限，不唤醒被人停止的 Ticket。「人」不是领域概念，白名单是 Source 的 Policy 部分。被否决的概念表（Principal 实体、Eval 自调度、跨 Source 身份映射、每 Thread 最多一个 Ticket、策略变更取消运行中 Ticket 等）。
- 002 Authorization：宿主强制白名单、规则和 Mandate；消息、评论、引用文档只是输入，不能改变权限。强度 explicit / inferred / not admitted；准入表按接收范围 × 是否在名单 × 是否 @ 智能体；bot 只能产生 inferred 候选；接受模式 `off` / `observe`（默认，记录「会接受」但不接受）/ `auto`；策略版本变化丢弃运行中的 Eval，向受影响的 Ticket 发 Input，不立即吊销 Mandate；恢复前重新验证 Mandate。Eval 超时两次后第三次记 `unavailable`。
- 003 Execution：Ticket 阶段表（person/delivery、person/answer、person/review、person/blocked|failed、working/queued、idle、working）。每 Ticket 一个 Executor Session 和目录；Executor 以操作者 OS 用户身份无审批运行命令；前台 shell 5 秒后转后台作业；每轮后台工作最多 30 分钟；worker slot 并发限制，人的消息有一个额外槽。合并由「Delivery requirements」提示词管，默认不自动合并，宿主无合并逻辑。Ticket 结束条件：ready 无 PR 即完成；dropped；PR 全部关闭才结束。恢复：passing 失败重试三次；重启时运行中的 Assignment 以恢复说明重新开始（先核对目录、git、GitHub 再重复外部动作）；「先存后做」的 stored work；Channel 失败按 5 s 翻倍到 300 s 重试，同 Ticket 同会话消息保序；暂停处理。
- 004 Prompts：团队可编辑三个团队段落、添加 skills、覆盖所有内置段；覆盖记录它基于的默认文本，升级后默认变了标「Default updated」并显示差异；优先级从强到弱：工具与宿主强制的事实 > 内置与生成段 > 规则的过程要求 > 团队段与团队 skill > 外部文本（数据，永不改变规则或授权）。每次保存成为不可变版本（含完整配置、修订号、前驱、时间、来源），保存要带基线修订，冲突拒绝。
- 005 Panel：三个问题（现在有什么需要我；系统是否健康并做对的事；一件工作的历史）；标签 Overview / Source / Trigger / Eval / Execution / Settings；原则：聚焦、精确定义、一处一概念、领域结构、诚实状态、默认安静、一致控件。
- 006 Deployment：独立 Harness profile；agent 专用 GitHub 账号与只用该账号凭据的 `gh` 包装；可选飞书 bot；两个预设 `dsh-superagent-worker` / `-router` 都以 `danger-full-access` 以操作者 OS 用户运行；账号包装器不隔离进程；要隔离就用专用 OS 账户。状态目录含 SQLite、锁、命令 shim、git 配置；管理 API 在 `127.0.0.1:<port>/orchestrator/api/`，要求 `x-orchestrator-request: 1` 头与回环 Host；健康条目分组（runtime、models、evals、各 effect kind、worker、github、feishu、history、sync:<id>）；升级要先备份 SQLite，新版本迁移过的库旧版本打不开。setup 会移除名字含 KEY/PASSWORD/SECRET/TOKEN 的环境变量。
- 007 Architecture：Runtime 是组合根；Engine 是 Trigger/Thread/Decision/Ticket/Assignment 的唯一写者，方法全部同步，每个命令一个事务，端口在事务内 enqueue 外部工作、适配器在提交后执行；Engine 分五个 part（Intake、Evals、Tickets、Assignments、Replies），依赖单向；调用方只拿角色 `Pick`。Store 一个 SQLite 文件，`afterCommit` 只在提交后执行。Effects 持久副作用队列：kind/key 一行、同 key 覆盖、指数退避、`Later` 推迟不计失败；六种 kind（assessment、launch、delivery、relay、notice、reply）各有并发上限与重试表。Sources 经 `SourceKind` 协议扩展，`SourceRegistry` 持有实例，health 条目一写者。依赖规则：窄接口、`src/core` 不做 I/O、一种状态一个写者。持久化表：trigger、disposition、trigger_thread、thread、ticket、decision、effect、outbox、kv；旧记录不删除、无限增长；`SCHEMA_VERSION` 只增。模型面：主 Session 14 个应用工具，Executor 6 个；`orchestrator_read` 的资源按角色限读；文本页默认 4000、最多 8000 UTF-16 单元；列表默认 20、最多 50 项。开放问题：Trigger 哪些字段可信；部署隔离。
- 008 Cookbook：以虚构 `webhook` Source 为例，八步：映射概念、配置 schema/fields/health、翻译为 Trigger（稳定 id 经 `key`，`human`、`addressed`、`channel`、`meta` 不可信）、scope、Channel（幂等发送用 `message.id`，`sent`/`gone`/抛错重试）、threadContext（分页读取）、Source 的 Policy 部分、Executor 环境变量。
- SETUP.md：给 agent 的安装脚本，一次一个问题，收集完再给一份 300 词内的计划，用户确认后执行；默认接受模式 `observe`；不在聊天里要秘密。

## 5. 相关资源的客观要点

- **S8 Harness 官网**：开源 agent harness，MIT，public preview；macOS（Apple silicon）与 Windows 桌面版，或 `npx @deepseek-ai/dsh web`；「everything is a plugin」，可在 Creator mode 用聊天创建插件；官方插件含 Agent teams、Auto approval reviewer、Scheduled tasks、Voice input、Terminal、Agent loop、Subagents（多个标 experimental）；基于 Cordis。
- **S9 Harness 仓库**：MIT；267 854 stars（2026-11-01 查）；developer preview，自述会有破坏性变更；`packages/` 下有 acp、agents、attachment、boot、browser-use、bundle、client、compaction、computer-use、context、core、credentials、deliverables、document、experimental、extensions、feedback、fs、goal、guard、host、identity、interaction、jobs、llm、lsp、mcp、plan、preset、ptc-runtime、sandbox、schedule、sdk、session-query、session、settings、shell、skill、spill、ssh、storage、subagent、subprocess、telemetry、terminal、test-support、todo、typert、util、web、workflow、workspace；`apps/` 下 cli、desktop-host、desktop、plugin-market-generator、web。附 SAFETY.md、BENCHMARK.md。
- **S10 Cordis**：「A Meta-Framework of Spatiotemporal Composability」，MIT，9511 stars，自述 API 不稳定；文档「cordis-primer」尚在建设。
- **S11 Cordis 论文**：A Programming Paradigm for Spatiotemporal Composability，Yifan Shi、Wei Zhang、Tianyi Cui，2026-08-26 提交，92 页。摘要：把动态组合拆成时间可组合性（移除组件时完全撤销其副作用）与空间可组合性（声明并响应式管理组件间依赖）；提出可撤销效应（每次上下文变更带一个逆操作）与响应式余效应（按组件的余效应规格决定激活/停用）；合并为一种上下文类型，得到「上下文范式」与组合演算；在 Cordis 中实现效应跟踪、余效应解析、声明式组件加载、配置协调与热替换。
- **S12 Cordis primer**：插件实现 Service；Context 是容器，服务占稳定键（`ctx.tools`、`ctx.llm`、`ctx.sessions`）；`inject` 声明依赖，依赖就绪才启动；类型化事件五种派发模式（emit、waterfall、parallel、serial、bail）；所有注册可逆（`ctx.effect()`、`ctx.on()`），reload / teardown 时撤销。Harness 用法：按上下文键分组行为；拦截与策略用事件，直接能力调用用服务方法；每个注册要有 disposer。
- **S15 练习包**：`lab/README.md`、`lab/mycelium-practice.spec.ts`（8.2 KB）、`LICENSE.txt`（MIT，Mycelium Protocol）、`UPSTREAM_LICENSE.txt`（MIT，DeepSeek Harness Team）。测试文件导入上游 `Engine`、`Store`、`Effects`、`HealthBoard`；七个 `it(...)` 用例名与文章表格一一对应；README 声明 `stop()` 为空、未启动 OS Worker、数据库重开不是断电实验。
- **S16 AWS Infinity**（2026-10-25）：AWS 与伯克利 Hydro 项目发布的早期研究运行时（Apache 2.0），Agent 等待工具结果时保存状态并释放执行资源；「7.5 万 Agent 放进树莓派」是按每空闲 Agent 约 100 KB 推算的容量，不是真实推理；作者本地跑了四个上游测试，建议为有副作用的工具设计幂等。
- **S17 CoreSpeed TIE**（2026-10-31）：让 MCP 连接的远端 agent 使用已有电脑；公开仓库 `corespeed-io/cs`（Apache 2.0）主要是插件配置与 Skills，不能证明桌面应用、daemon 或云服务开源；「root 设置不是 OS 沙箱」。

## 6. Agent24 侧对照时用到的本仓库来源

| 来源 | 用途 |
|---|---|
| `docs/architecture/EVOLVABILITY-ASSESSMENT-2026-11-01.md`（终稿） | 现状结论与六条建议 |
| `docs/adr/ADR-039-concept-model.md`（已接受 r2） | 概念总纲 |
| `docs/adr/ADR-038-component-contract-v1.md`（草案 r3） | 组件契约 |
| `docs/ARCHITECTURE-LAYERS.md`（2026-11-01 修正版） | 分层与未完成清单 |
| `docs/decision.md` ADR-037 | 产品形态：daemon 是本体 |
| `docs/tracking/2026-10-26/SOURCE-ADR-037-facts.md` | 桌面健康检查、常驻方式等代码事实 |
| `docs/agent/CORE-5-MIGRATION-GUARD-DESIGN.md` | 5f–5i 分片 |
| `docs/adr/ADR-K1-01..04`、`ADR-MEMORY-P1-LIFECYCLE-AND-PURGE.md`、`ADR-MEMORY-RECALL-MESSAGE-ORIGINS.md` | 授权、数据流、审计、记忆边界 |
| `docs/specs/SPEC-SKILL-DISTRIBUTION.md`；issue #1037 | 技能分发设计与立项状态 |
| 代码：`rust/crates/agent24-agent/src/{self_wake,subagent,resume}.rs`、`rust/crates/agent24-protocol/src/types.rs`、`rust/crates/agent24-scheduler/src/lib.rs`、`rust/apps/agent24d/src/{scheduler_deliver,peer_runs,mcp,approvals}.rs`、`rust/crates/agent24-comm/src/router.rs`、`rust/crates/agent24-policy/src/{lib,guardian}.rs`、`rust/crates/agent24-store/src/audit.rs`、`packages/nostr-bridge/src/{main,inbound}.ts`、`protocol/events.schema.json`、`eval/decide/` | 逐项对照的代码依据（只 grep 与读文件头，未构建、未运行） |
