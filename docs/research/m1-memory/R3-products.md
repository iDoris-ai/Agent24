# R3｜商业产品中的记忆功能调研

调研日期：2026-10-02。范围：消费级助手、陪伴产品、生活记录与知识工作产品。**以下是官方公开行为的案头核查，不是登录各套餐后的实机测试。**

## ① 一句话结论

**Agent24 M1 应把「用户授权的、可追溯且可撤回的记忆」做扎实：保留显式写入路线，优先补齐真实写入回执、召回来源和关闭控制，并明确“撤回断言≠删除原文”，不能以本地存储代替用户的数据处置权。**这是基于下述产品证据和 [M1 冻结方案][M1] 的研究判断。

### 阅读口径

- 先通读了 [M1-PLAN-v2.md][M1]。本文引用的是计划，不声称已经核验实现：T06 显式写入、T07 personal FTS 召回、T09 断言撤回、T10 REST、T11 桌面页；编辑、物理擦除、导出及 shared space 明确不在 M1。T06b/T07b 是笔记本侧可选工作。[M1]
- 采用 `curl -L --max-time 40 -sS <官方 URL>` 读取正文，HTML 用 Python 清洗。Claude、Google、Microsoft、Pi、Character.AI 博客、Limitless、Apple、Notion、Mem 的主要页面及 arXiv/官方事故页成功读取；OpenAI 帮助/博客、Character.AI 部分帮助页的 curl 被拦截，改用网页读取工具核查官方正文。未把 HTTP 成功、搜索摘要或验证页当作正文证据。
- 文中「未核实」表示本次资料不足，不等于产品没有该能力；「评测未核实」表示没找到可支撑该项结论的公开结果，不把演示、满意度宣传或模型通用 benchmark 当记忆评测。
- 产品行为以当前帮助正文优先；发布博客保留发布日期和历史范围。厂商未公开的数据库、embedding、排序、冲突消解实现不作推测。研究者实测单列为补充证据，不冒充厂商承诺。

## ② 逐项调研

### 1. ChatGPT Memory：从 saved memories / reference chat history 到 memory summary

- **是什么、核心机制。** Saved memories 与原聊天分开保存，可由显式要求或系统自动产生；reference chat history 从旧聊天提取可用上下文。当前文档还区分 legacy 条目列表与 improved memory summary；后者只是摘要，未必展示所有可用信息。2026-06-04 官方发布的 Dreaming 在后台跨聊天整理并更新记忆，处理信息过时问题；不能把它等同于单纯增大 context window。[O1][O2]
- **写入、召回、更新。** 既支持“记住”，也支持自动记忆。2024-09 的更新公告明确加入 “Memory updated → Manage memories” 入口；当前 Sources 可显示部分个性化依据，但不保证展示全部影响因素。对记错/过时信息可对话纠正，或使用可用的记忆编辑控件；可用功能随套餐、地区、平台变化。[O3][O1]
- **遗忘与用户控制。** 删聊天不自动删 saved memory；彻底移除相关个性化信息还需处理其聊天、文件、连接应用等来源。关闭 memory 不删聊天；关闭 reference chat history 后，其提取信息安排在 30 天内删除，原聊天保留。删除 saved memory 的日志可能保留最多 30 天。**不要把管理页当成所有可用个人信息的完整账本。**[O1]
- **临时聊天、训练、导出。** 当前 Temporary Chat 可在开始前选 Personalized / Unpersonalized，前者可读既有记忆，两者在保持临时状态时都不新增/更新记忆、不用于模型改进；安全副本可保留最多 30 天，保存为普通聊天后适用普通设置。旧发布博客“临时聊天不使用记忆”的说法不宜直接沿用。[O4][O3] 消费账号的训练使用受 Improve the model for everyone 控制，Business/Enterprise/Edu 等默认不用于训练。[O1] 官方支持合资格账号导出聊天及账号数据；完整记忆内部状态的可迁移格式、导出后无损恢复能力**未核实**。[O5]
- **评测、失败。** Dreaming 官方评测分“延续上下文、遵循偏好、随时间更新”三类并报告改善；本次未取得可复现数据集及可比较的准确率，不自行读图估数。[O2] 官方承认某些情况下 memory 会加剧 sycophancy，但没有证据说明普遍如此；另有 2025-11-06/07 记忆缺失事故，状态页最终标记恢复，不能写成持续丢失。[O6][O7]
- **优缺点及 M1 关系（判断）。** 优点是写入通知、来源与管理入口形成闭环；弱点是派生信息及多来源删除使用户难以判断“究竟忘掉了什么”。M1 的独立 AssertionLedger 更适合列出确定的有效断言；T06/T11 应借通知，T07/T11 应借来源，T09 必须坚持准确的“撤回”语义。[M1][O1][O3]

### 2. Claude：主题记忆、Projects、chat search 与记忆迁移

- **是什么、数据模型。** 当前新版按独立 Topics 在聊天中保存记忆，既可自动写入也可显式要求；每个 Project 有独立记忆空间和摘要。旧版每天合成 summary 的说明仍留在同一帮助页的 legacy 小节，不能当作所有账号现状。Chat search 是独立的 RAG 搜索，显示工具调用和原聊天引用；Projects 的文件知识也有 RAG，两者不等于个人画像。[C1][C2]
- **写入、召回、更新、关闭。** Settings > Memory 可查看、编辑、删除 Topics，也可自然语言修改。Pause 保留既有记忆但停读停写，暂停期间聊天不会在恢复后补成记忆；Reset 清除记忆。单次聊天可在首条消息前关闭记忆，但聊天仍留在历史，可能被其他聊天搜索。Incognito 才是不进普通聊天历史与记忆的独立模式。[C1][C3]
- **敏感信息与通知。** 当前帮助页规定敏感主题默认不保存；用户另行开启后，每次保存敏感主题有专门提示；关闭该选项会移除已存敏感项，另有一些信息即使请求也不保存。消费版记忆默认开启，Team/Enterprise 受组织及成员设置控制；精确地区可用性**未核实**。[C1]
- **遗忘、保留、训练。** 新版删除来源聊天不自动删除相关记忆，必须另删条目。Incognito 不用于改善 Claude；组织账号的 Incognito 仍可进入组织导出并受留存规则约束。消费数据是否用于改进受设置控制，安全系统标记内容另有安全用途；不要把“不训练”写成“不留存”或“组织管理员不可导出”。[C1][C3][C4]
- **导入导出。** 官方流程让用户从其他助手取出记忆文本再粘贴导入，由 Claude 提取条目；可复制导出，账号导出也包含记忆。导入属实验功能，可能遗漏；导出说明还混有旧设置路径，应以账号实际界面为准。**让模型口述“全部记忆”并非无损备份的证明。**[C5][C1]
- **评测、优缺点、M1 关系。** 专项量化评测**未核实**；官方帮助承认迁移/导入可能漏项。[C1][C5] 判断：Project 隔离、Pause/Reset 区分及敏感项通知值得借鉴。M1 的 personal space 暂不具备 Project scope；迁移、编辑和自动抽取应留到后续，来源与状态展示可落在 T10/T11。[M1]

### 3. Gemini：Saved Info / Instructions、过去聊天 Memory 与 Connected Apps

- **是什么、数据模型。** 当前 Instructions for Gemini（部分地区仍称 Saved Info）是显式添加的偏好/事实/回答指令，可新增、编辑、删除、停用；过去聊天 Memory 是另一种自动个性化来源。Personal Intelligence 还涉及 Connected Apps，不能把这些都算作一个用户可逐条编辑的记忆表；内部存储结构**未核实**。[G1][G2]
- **写入、召回、更新。** Instructions 由用户配置；Memory 从聊天中学习、可开关，纠错可直接在聊天里提出。要移除过去聊天中的信息，官方要求删除含该信息的所有聊天；连接应用也有同一信息时，还需断开该应用。更新或删除连接源后，Gemini 的体验可能延迟数日才变化。[G1][G2]
- **用户可见性与地区。** 可询问“是否使用过去聊天”，但本次未核实每次自动记忆都有可靠的写入通知。Memory 当前要求个人账号、成年、Keep Activity 开启，不适用于工作/学校/受监管账号，部分聊天形态亦不适用；不要复用早期发布时的国家名单作为当前全量可用性。[G2]
- **隐私、保留、导出。** Temporary Chat 不进入 Activity/近期列表，不作个性化或模型训练，但仍可保留 72 小时。Keep Activity 默认自动删除期限为 18 个月，可调整；关闭后未来聊天通常不用于改进模型，但提交反馈有例外。已经人工审阅、与账号脱钩的数据可保留最多三年，删除 Activity 不会一并删除它。[G3][G4] 本次未核实完整派生个人画像的独立导出格式，不能把通用账号数据导出等同于记忆状态迁移。
- **评测、优缺点、M1 关系。** 公开记忆准确率**未核实**；官方明确纠正不一定总能成功、来源删除有传播延迟。[G2] 判断：值得吸收“显式指令”和“历史自动个性化”分开控制；对 M1 最重要的是把 T09 撤回后的即时召回禁用测清楚，并在以后接入连接器时处理源数据重复进入的问题。[M1]

### 4. Microsoft Copilot：个人版与 Microsoft 365 必须分开

- **是什么、版本范围。** Microsoft 明确把 2026-08-18 起的新个人 Copilot 与旧版隐私文档区分。新版个人版 Settings > Personalization 有 Saved memories，可逐条/全部删除、停用；停用不自动删除。One shared experience 又是另一个开关，可控制 Copilot 与 Bing/Edge/MSN 的跨产品个性化。[MS1][MS2]
- **核心机制、写入/召回/更新。** Microsoft 365 账号对应的 Copilot Memory 文档说明：从聊天推断工作相关信息作为上下文；发现值得记的信息会询问是否保存，也可显式要求；写入显示 “Memory updated”，可合并、更新、移除 saved memories。过去聊天个性化另行管理。**这些具体自动写入规则不能未经验证就推广到所有个人版场景。**[MS3]
- **管理、删除、导出与训练。** 新个人版公开了独立记忆管理和隐私仪表盘中的活动历史导出/删除。删除记忆、删除聊天历史、停用跨产品个性化是不同操作；活动导出是否完整包含可重建的记忆状态**未核实**。新版活动历史页明确 prompts、responses、文件内容不用于训练 foundation models，自愿反馈也不用于训练这些基础模型；不能沿用旧个人版训练说明。新版独立记忆编辑、临时聊天、确切记忆保留期限及地区矩阵，本次**未核实**。[MS2][MS4][MS1]
- **评测、优缺点、M1 关系。** 专项评测和官方确认的具体记忆错误事故**未核实**。判断：365 版本的保存询问与提交后通知值得 T06/T11 借鉴；个人版的跨产品个性化提醒 Agent24：未来 OS 模块或社区代理不能因为同一身份就自动共用个人记忆，继续保持 T01/T02/T07 的作用域边界。[MS3][MS2][M1]

### 5. Pi / Inflection：陪伴中的稳定个人事实

- **是什么、机制。** Pi 当前记忆说明强调跨聊天保存较稳定的信息，如偏好、工作、长期目标，从普通聊天收集，也可显式说 Remember；临时计划和一次性事件被设计为跳过。内部抽取/检索算法**未核实**。[P1]
- **写入、召回、纠错、遗忘。** 官方承认显式请求有时要重复才记牢。可对话要求忘记，或 Settings > Account > Manage Memories 新增、更新、删除；网页 thumbs-down 是准确性反馈，**不会删除记忆**。全局关闭记忆、临时聊天及逐次写入回执，本次**未核实**。[P1]
- **导出、训练、保留。** 可导出完整聊天 JSON，单条/单会话导出不受支持；该导出是否覆盖独立记忆账本**未核实**。[P2] Inflection 政策允许用数据改进/训练，账号设置可退出。删除后 inputs 通常最多再留 15 天，但有安全/法律等例外；outputs 可为条款限定用途无限期保留。权利依地区而异，不能把“删账号”概括为所有输入输出立即擦除。[P3]
- **评测、优缺点、M1 关系。** 官方量化记忆评测**未核实**；“可能需重复请求”是官方限制，不是已证明的大规模故障。[P1] 判断：陪伴产品也需要结构化纠错入口；M1 应借列表管理，但保留显式写入，不能用温暖的“我记住了”代替 T06 已提交的真实状态。[M1]

### 6. Character.AI：角色故事、Pinned 信息与自动 Facts

- **是什么、数据模型。** 2026-05 公告区分用户写的 Story Memory、Pinned messages、自动捕捉的 Facts（persona、角色、侧角色等），还有 Memory Usage 展示上下文构成。自动整理历史时优先保护手写/固定信息；这是角色叙事与关系连续性，不是事实经过验证的个人档案。[CA1]
- **写入、召回、更新/遗忘。** Facts 可编辑、添加、关闭，开新聊天可复制或从空白开始；手写/Pin 比自动推断更可控。官方有套餐分层。2025 年旧公告明确“不保证每次准确使用记忆”，旧版字符上限不应再套到当前产品。[CA1][CA2] 自动 Facts 是否每条都有持久化确认通知**未核实**。
- **导出、训练、地区。** 官方提供账号数据导出。[CA3] 官方训练文档承认用户文本/互动用于 post-training；训练设置 FAQ 的 EEA/UK 退出适用于未来内容，且不代表搜索、推荐、安全分类等所有用途都停止。不可把地区性控件概括成全球一致；精确统一保留期与导出能否完整迁移全部记忆层**未核实**。[CA4][CA5]
- **评测、失败。** 官方未提供本次可核验的记忆 benchmark；2026-04 更新承认收到可靠性、胡言和截断等反馈，并称在改进。这是厂商确认的用户反馈，不能全部归因于记忆模块。[CA6]
- **优缺点及 M1 关系（判断）。** 可借“用户明确固定的信息比自动提取更权威”和“新会话是否沿用上下文”的可见选择；不适合把角色创作中的虚构经历自动写进 user 事实。T06 的 UserSaid 只能证明谁说的，仍需在 T08 测角色扮演、引用、假设，不应把它解释为事实真值。[CA1][M1]

### 7. Rewind / Limitless：原始生活记录与服务退出权

- **Rewind 是什么、机制。** 官方退役公告确认其原有屏幕/音频捕获能力，当前帮助仍给出 Mac 本地库位置和 Delete all data / 卸载方式。[L7][L2][L3] 本次发现旧官网域名已展示不同产品，旧发布页也无法读取；因此历史 OCR/ASR 实现、应用排除规则及 Ask/Summarize 外发边界标为**未核实**，不沿用旧宣传作隐私保证。
- **Limitless 是什么、机制。** Pendant 经手机把录音上传云端处理/存储，再提供转写、摘要、搜索、Ask AI；它不是 Rewind 本地架构的同义名称。用户可以编辑转写/说话人、导出转写/摘要、下载音频、删除记录或账号；操作及限制随界面/数据类型不同。[L4][L5][L6]
- **当前状态必须限定。** 官方 2025-12-05 公告宣布加入 Meta、停止出售 Pendant，承诺支持既有 Pendant 用户至少至 2026 全年；非 Pendant 服务退役，桌面/web 不再新录音，旧会议可访问至 2026 年末；新版 Rewind 于 2025-12-19 停止屏幕/音频捕获。公告另列巴西、中国、欧盟、以色列、韩国、土耳其、英国停止服务和限期导出安排。**本报告日期不能称它仍是完整可购买的生活记忆产品，也不能称所有 Pendant 服务已结束。**[L7]
- **隐私与遗忘。** Limitless 对旁观者的官方说明允许为了改进/训练服务使用收集数据，涉及第三方及关联方；录制者控制分享/删除，但并非绝对零保留。官方要求录制前告知并取得同意，LED 不替代告知。这里陈述的是厂商要求，不替各司法辖区作录音合法性判断。[L8][L9]
- **评测、优缺点、M1 关系。** 记忆检索量化评测**未核实**。判断：原始证据可回看、记录与摘要分层值得借；服务停用更说明持续可访问和可导出的价值。全时录音、默认云上传不适合 M1；AgentEar 本就不在 M1。Rewind 的“留原文”不能证明 EventLog 应永久禁止用户擦除，二者目的不同。[M1][L7]

### 8. Apple Intelligence personal context：设备资料检索，不是已公开的画像账本

- **是什么与上线边界。** 2024 年 Apple 把 Siri personal context、屏幕感知和跨 App 动作列为后续能力；截至本次调研，2026-09-14 官方宣布 Siri AI 已开始英语 beta rollout，能够跨消息、邮件、照片等找信息。应标“beta”，不能继续写成全部未上线，也不能写成所有地区正式可用。[A1][A2]
- **机制与写入/召回。** 新架构使用设备端 Spotlight/App Toolbox 与 Apple Foundation Models，需要时调用 Private Cloud Compute（PCC）。用户应用里的内容是主要上下文来源；具体长期画像结构、自动写入规则、冲突更新算法**未核实**，不可套用 ChatGPT 的 saved-memory 模型。[A2]
- **可控性、遗忘、导出。** Apple Intelligence Report 可查看/导出 PCC 请求报告；这是处理活动审计，不是完整个人记忆导出。逐条画像编辑、forget 如何传播至派生状态、每次“记住”通知，均**未核实**。源 App 数据管理与系统智能开关不能替代上述证据。[A3]
- **隐私与地区。** Apple 对 PCC 的承诺是请求所需数据不持久存储、不可被 Apple 访问，并允许外部验证；这不等于所有 Apple Intelligence 请求都本地运行。Siri AI 初始英语 beta，公告明确欧盟部分平台及中国的限制；个人上下文数据是否在所有情形下排除训练，不能从 PCC 承诺直接外推。[A2][A3]
- **评测、优缺点、M1 关系。** personal-context 专项公开准确率**未核实**。判断：设备内索引与处理审计可借；M1 的 recall 事件可发展成“本轮用了哪些本地数据、发给谁”的面板。无需在 T07 模仿操作系统全域读取；oMLX 本地抽取/embedding 也不能自动保证最终回答所用云模型不接收召回内容。[M1][A3]

### 9. Notion AI：有权限边界的工作空间知识检索

- **是什么、数据模型。** Notion AI 的知识来自页面及连接器，用户显式提问，后台索引资料；官方披露页面 embedding、向量检索、相关页面精排后生成回答。它是工作空间 RAG，不应包装为聊天中自动识别用户身份和偏好的 memory。[N1]
- **写入、召回、更新。** 知识随页面编辑、连接器同步更新；Enterprise Search 可限定来源，AI 应尊重用户已有访问权限。某些模型/模式的可搜索范围不同。连接器断开后停止检索、删除连接数据有传播窗口，不是任意源修改均瞬时生效。[N1][N2][N3]
- **遗忘与保留。** 官方安全页同时写页面/workspace 有 30 天恢复期、向量库 embedding 在删除后 60 天内移除；另有“30 天后删除含 embeddings”的表述。**文档口径存在不够清晰之处，本次不能核实所有向量副本到底统一遵循哪一期限**；产品比较应保留 60 天上界说明，而非承诺第 30 天所有派生数据都消失。[N1]
- **训练、界面与导出。** 官方称默认不以客户数据训练，AI 子处理方有合同限制；Enterprise 默认 LLM 零保留，其他套餐默认最长 30 天，部分启用功能另有例外。[N1] 用户主要编辑/删除源页面、配置连接器与搜索范围；普通 workspace 可导出，**未核实其能导出并复原 AI 向量索引**。地区数据驻留与全部子处理方位置本次未逐项核实。[N2][N3][N4]
- **评测、优缺点、M1 关系。** 公开的可比召回准确率**未核实**；文档公开了检索范围和同步限制。[N2][N3] 判断：值得借的是源权限与引用，不是 M1 立即接入工作空间全量索引。T01/T07 保持 owner 隔离；T07b 后续向量索引必须服从 T09 的有效性过滤，不能让旧 embedding 绕过撤回。[M1]

### 10. Mem.ai：可编辑、可迁移的个人知识库

- **是什么、机制。** Mem 以用户笔记为核心，Chat 可查笔记、总结并创建/更新笔记，可限定具体 note/collection；用户可查看和修改知识载体。官网的全库理解宣传不等于已经公开了召回算法或完整事实账本。[ME1]
- **写入、召回、更新、导出。** 显式记录和自然语言编辑是主要用户入口；支持一键 Markdown 导出 notes/collections。笔记内容可移走是可验证的产品承诺，但与 Chat 内部记忆/索引状态无损迁移不同。独立“记忆写入成功”回执和临时无记忆模式**未核实**。[ME1][ME2]
- **遗忘、训练、保留。** Privacy 声明不以个人信息、笔记等训练通用 AI/ML 模型；会使用受信任服务商处理数据。官方明确并非端到端加密，因为 AI 处理需要访问内容。[ME3][ME2][ME4] Terms 对内容删除写的是先不可访问、90 天内删除，活动日志可含部分已删内容并保留最多 400 天，曾共享内容可能继续可访问。**只引用“90 天删除”会遗漏重要边界。**[ME5]
- **评测、优缺点、M1 关系。** 专项召回质量数字、官方确认的当前系统性记忆故障、用户可选地区驻留均**未核实**。判断：Markdown 导出与可编辑源对象最贴近用户所有权；M1 目前不做编辑和导出，应把它们列为后续数字主权交付，而不是用“数据在 SQLite 里”当作用户已能迁移。[M1][ME2]

### 11. 已知投诉与失败：把证据等级分开

| 问题 | 核实到的证据及边界 | 对 M1 的含义（建议） |
|---|---|---|
| 记错、记忆过时 | OpenAI 用旅行结束仍按旅行地推荐的例子说明 stale memory；它是厂商演示，不是统计投诉率。[O2] | 时间性断言展示记录时间；M1 不做自动更新时，不宣称当前状态始终正确。 |
| 记了用户未明确要求的内容 | 独立研究分析 80 位真实用户的 2,050 条 ChatGPT memory，约 96% 未检测到显式记忆命令，28% 含研究标注的个人数据；论文使用自动标注及人工抽查。样本不代表全体用户，也不能把“没有逐条命令”直接等同于违法或没有账号级同意。[R1] | 支持 T06 显式写入；T06b 将来先产候选，不悄悄变成 qualified。 |
| 过度个性化、迎合 | OpenAI 复盘确认 memory 在一些案例中加重 sycophancy，同时明确无普遍因果证据。[O6] | T08 不只看 recall hit；另测无关偏好是否污染回答、用户观点是否被误当事实。 |
| 说记住却没可靠记住 | Pi 官方说明请求可能要重复；Character.AI 旧公告不保证每次引用正确。[P1][CA2] | 持久化成功与模型“会正确用”分开验收，回执由系统产生。 |
| 记忆丢失 | OpenAI 官方状态页记录真实缺失事故与恢复；不能据此推算年度丢失率。[O7] | T03–T05 的事务、重放及可见写失败有产品价值；后续还需用户备份/导出。 |
| 不喜欢某记忆，但反馈没让它消失 | Pi 明说 thumbs-down 只反馈准确性，不删除；Claude 新版删聊天不删记忆。[P1][C1] | UI 区分“反馈错误”“撤回”“删除原文”，不让一个按钮承担不同语义。 |
| 用户反馈可靠性差 | Character.AI 官方承认收到可靠性、胡言、截断反馈；没有证据把全部问题定位为记忆实现。[CA6] | 收集带 assertion id / source 的反馈，避免只能听到“你怎么又忘了”。 |
| 服务变化导致失去访问 | Limitless 官方列出退役、地区退出与导出截止日，这是服务事实，不是记忆准确率事故。[L7] | 数据可脱离供应商读取与迁移，应成为后续明确交付。 |

以上未列举未核实的论坛传言，也未将厂商“可纠错”界面推导成“已发生某类大规模事故”。研究 [R1] 仅作为产品官方资料之外的补充；本次已用 curl 读取其 arXiv 摘要和论文 HTML。

## ③ 对 M1 的启发

### 三栏决策

以下全部是**研究建议，未修改冻结计划**。“沿用范围”表示强化已有语义/验收；“需变更”表示当前计划没有承诺，若吸收需重新估算和登记，不假定已获实现授权。[M1]

| 应该吸收：具体改法与受影响 task | 可以借鉴但 M1 不做 | 不适合我们：理由 |
|---|---|---|
| **提交后才显示“已记住”〔需小幅变更〕**：T06 在 WriteGate commit 后发结构化成功回执，携 assertion id；T11 展示原句和管理入口。重复写、Held、失败分别反馈；结合 T04 的 `memory.write_failed`，不能由模型自由生成成功状态。依据 ChatGPT/Copilot 的通知交互。[O3][MS3][M1] | 自动抽取先进入候选清单，用户确认后晋升；留给 T06b 与后续候选管理 UI。 | 从模型回复、网页或工具输出直接生成“用户事实”；这会绕过既定 Trust/provenance 边界。[M1] |
| **记忆有来源〔需小幅变更〕**：T10/T11 展示内容、记录时间、来源类别/会话、有效状态；T07 的 `memory.recalled{ids}` 在回答旁显示“本轮引用了哪些记忆”。只显示真实注入项，不能声称列出模型全部影响因素。[O1][C1][M1] | 类 Apple 的请求审计报告、Notion 的跨源引用，以及有权限检查的完整原文回看。[A3][N1] | 把 LLM 总结“我了解的你”作为唯一管理页；摘要可能省略信息，不能代替有效断言清单。[O1] |
| **撤回语义准确且可验证〔沿用范围〕**：T09/T10/T11 保留“撤回”；确认框写清“停止作为跨会话记忆调用，原始聊天仍保留在本机”。T09/T07a 测 rebuild 后仍不可召回；T10 测新会话不注入。[M1] | 原文/派生摘要/索引/备份的一体化擦除、密钥销毁、用户导出；单列后续任务，不能仅给 forget 改名。 | “原文永不删”成为永久产品原则。M1 可以诚实限制能力，但最终数字主权必须包含用户处置数据的权利。参照产品删除边界与退出案例。[ME5][L7] |
| **关闭要有明确作用域〔需变更〕**：T06/T07 增加持久化的 personal-memory 总开关，T10/T11 展示状态；关闭停写断言和跨会话召回，已有断言保留。测试重启仍关闭。文案明确会话 EventLog 仍记录，不能叫“无痕”。[C1][MS2][M1] | 真正不落 EventLog、不入摘要、不进 durable thread 的临时会话，需要重新设计 T03–T05 的日志契约，M1 不临时拼接。[M1] | 一个“隐私模式”同时含混代表不写、不读、不训练、不留日志。各产品临时模式的保留例外已说明其歧义。[O4][G3] |
| **纠错路径可完成〔沿用范围〕**：T11 清楚指引“先撤回，再重新说记住”；T06/T09 补设计验收：撤回后同句再次授权能否重记。当前 `sha256(owner‖object)` 幂等规则可能与重记冲突，需验证，本文不宣称已有 bug。[M1] | 编辑与 supersede、有效时间、旧值/新值冲突解释；对应 FU-M1-2，采用账本关系而非静默覆盖。 | 自动把后一次提及无条件当新事实。引用、试探、角色扮演和“以前”不等于用户更新身份。 |
| **评测覆盖“不该记/不该用”〔沿用范围+增补门禁〕**：T08 保留命中率基线，同时增无关问句、撤回、重启、跨 owner、模型自述、引用“记住”、敏感内容误写用例；隔离/撤回测试放 T06/T07/T09/T10 硬门禁，不跟召回命中率一样仅记录。[R1][O6][M1] | 真实长周期用户研究、自动摘要忠实度、向量 top-k 的跨语言/时间更新评测；为 T06b/T07b 建独立基线。 | 用回答点赞率或“感觉更懂我”代替写入准确、权限隔离和遗忘测试。迎合复盘显示满意度信号可能误导。[O6] |
| **记忆当数据而非高优先级指令〔T07/T08 需补〕**：注入块标注“用户曾提供的资料”、来源和状态，明确内容不是执行授权；测被记住的恶意指令不会覆盖当前任务规则。当前把原句放 system 消息的模板值得复核。[M1] | project/shared space 的独立上下文、每个社区代理单独授权；延后身份与 grant 管理。 | 同一账号下所有模块自动分享 personal space。产品个性化不构成跨模块授权。[M1] |
| **本地与外发分别披露〔T07/T11 需补〕**：记忆页说明存储位置；调用远程 provider 时，明确召回内容可能进入该 provider 的 prompt。T06b/T07b 保持笔记本本地处理，但不把这宣传为整个链路无外发。[M1] | 每 provider/每类记忆的外发策略、导出格式、可验证备份与恢复；以后应进入数字主权验收。 | 默认全时麦克风/屏幕采集及云端生活画像。它引入旁观者同意和数据流范围，超出显式 M1 与 AgentEar 后续路线。[L8][L9][M1] |

### M1 现有设计最需要防止的四种误读

1. **“撤回成功，所以本轮/旧会话也忘了。”** T09 修改 AssertionLedger，不重写 T03/T04 的 message/summary；旧 session 仍可能带入相同信息。验收应区分“新会话不再从账本召回”与“所有上下文已抹去”，UI 只能承诺前者。[M1]
2. **“原文完整，所以摘要不会记错。”** EventLog 保留原文不等于 Summarizer 输出忠实；M1 能回溯依据，但还需要对错误摘要的处置路线。建议 T04 增摘要污染反例；自动纠正摘要不在冻结交付中。[M1]
3. **“UserSaid，所以是真实且应永久有效的个人事实。”** T06 保存的是用户明确要求记住的原句，predicate 为 `said_to_remember`；建议展示为“你要求记住的内容”，保留其引用/假设语义，别在 UI 升格为已验证事实。[M1]
4. **“本地优先，所以没有隐私债务。”** 本地 FTS/本地抽取只限定这些处理阶段；计划没有承诺端到端无外发、物理擦除或用户可用的导出。对外承诺必须限定到真实实现。[M1]

### Agent24 记忆产品的交互与隐私原则（建议清单）

1. **用户拥有数据，也拥有停止处理和迁移数据的能力。** M1 诚实列明暂不支持物理擦除/导出；后续把可读导出、恢复验证、擦除范围作为交付，不以内部库文件存在代替用户控制。[M1][ME2][L7]
2. **默认显式记，自动记另行选择。** 普通对话、角色扮演、外部文档不是长期写入授权；自动候选显示来源与推断性质，未经授权不进入有效个人断言。[M1][R1]
3. **“已记住”是一张系统收据。** 展示确实提交的内容、来源与管理入口；失败明确说未保存，同句幂等说明已存在，不能由语言模型承诺持久化。[O3][MS3][M1]
4. **看得见的是完整有效断言清单。** 摘要可辅助浏览，不能替代账本；还应让用户区分“已保存”“这次被检索”“这次确实注入”。[O1][M1]
5. **改错不抹历史，但历史不能冒充现状。** M1 走撤回再记；后续 supersede 保留出处及时间关系，召回只用有资格的当前记录。[M1]
6. **暂停、撤回、删除、清空历史、退出训练各有含义。** 按钮说明停止哪种用途、保留什么、何时生效；“无痕”必须覆盖整个持久化链路才使用此名称。[O4][C1][G3][ME5]
7. **本地优先覆盖数据流，不只存储位置。** 明确哪些步骤本地、哪些发远端；远端失败不静默换一家提供商；本地日志和诊断也避免无必要复制敏感记忆。PCC/处理审计可作交互参考。[A3]
8. **身份和空间归用户授权管理。** 个人、项目、模块、社区分别授权，不能因同一用户名或代理可访问某工具就共享记忆；导入资料也要保留外部来源，不能自动视为用户当前自述。[C1][C5][M1]
9. **敏感度和相关性都要约束召回。** 用户允许保存不代表允许每次提起、对旁人展示或发给外部工具；默认避免不必要的健康、关系、身份等内容进入无关回答。M1 先测不相关不注入，后续再补细分用途授权。[C1][O6][M1]
10. **可控性是验收指标。** 既测“记得住”，也测“不会乱记、关闭有效、撤回不复活、跨空间不泄漏、失败不假装成功”；模型人格和功能升级不应悄悄改变这些契约。[O6][O7][M1]

## ④ 参考链接列表

以下编号同时对应正文的可点引用；除 R1 为独立研究外，均为产品官方资料。链接内容可能继续更新，本文结论以本次抓取为准。

- **M1**：[Agent24 M1 冻结方案][M1]。
- **O1–O7｜OpenAI**：[Memory in ChatGPT][O1]；[Dreaming：记忆架构与评测目标，2026-06-04][O2]；[Memory and new controls，含 2024/2025 更新][O3]；[Temporary chat 当前说明][O4]；[账号数据导出][O5]；[Sycophancy 复盘，2025-05-02][O6]；[Memory 缺失官方事故记录][O7]。
- **C1–C5｜Claude**：[Chat search and memory，含新版/legacy 分界][C1]；[Projects RAG][C2]；[Incognito chats][C3]；[消费版隐私与模型改进][C4]；[Memory 导入导出][C5]。
- **G1–G4｜Gemini**：[Instructions / Saved Info][G1]；[过去聊天 Memory][G2]；[Privacy Hub][G3]；[Temporary Chat][G4]。
- **MS1–MS4｜Microsoft**：[新个人 Copilot 隐私总览及版本范围][MS1]；[新个人 Copilot 隐私控制][MS2]；[Microsoft 365 账号的 Copilot Memory][MS3]；[个人 Copilot 活动历史/导出][MS4]。
- **P1–P3｜Pi**：[Pi’s Memory][P1]；[聊天导出][P2]；[Inflection Privacy Policy][P3]。
- **CA1–CA6｜Character.AI**：[Smarter Memory，2026-05][CA1]；[Chat Memories 历史公告，2025][CA2]；[账号数据导出][CA3]；[Training Data Documentation][CA4]；[训练设置与地区限制][CA5]；[模型/记忆可靠性更新，2026-04][CA6]。
- **L2–L9｜Rewind / Limitless**：[Rewind 本地数据位置][L2]；[Rewind 删除/卸载][L3]；[Pendant 存储与云处理][L4]；[搜索/Ask AI/摘要/导出][L5]；[Limitless 账号与数据删除][L6]；[Meta 收购、产品退役与地区变更公告][L7]；[旁观者数据用途和保留][L8]；[录制同意说明][L9]。不引用目前已无法支撑原产品事实的旧官网。
- **A1–A3｜Apple**：[2024 年初始发布与后续能力承诺][A1]；[Siri AI 英语 beta 发布，2026-09-14][A2]；[Apple Intelligence 隐私及报告导出][A3]。
- **N1–N4｜Notion**：[AI 安全实践、检索机制与保留期][N1]；[Enterprise Search][N2]；[AI Connectors][N3]；[导出内容][N4]。
- **ME1–ME5｜Mem**：[Mem Chat][ME1]；[Pricing FAQ：Markdown 导出与加密边界][ME2]；[Privacy Policy][ME3]；[Security][ME4]；[Terms：删除与日志保留][ME5]。
- **R1｜独立实证研究**：[The Algorithmic Self-Portrait: Deconstructing Memory in ChatGPT，arXiv 摘要][R1]；[论文 HTML，v3][R1HTML]。仅用于样本内隐私/用户控制观察，不用于宣称当前全量产品指标。

[M1]: ../../agent/M1-PLAN-v2.md
[O1]: https://help.openai.com/en/articles/8590148-memory-in-chatgpt
[O2]: https://openai.com/index/chatgpt-memory-dreaming/
[O3]: https://openai.com/index/memory-and-new-controls-for-chatgpt/
[O4]: https://help.openai.com/en/articles/8914046-temporary-chat-in-chatgpt
[O5]: https://help.openai.com/en/articles/7260999-exporting-your-chatgpt-history-and-data
[O6]: https://openai.com/index/expanding-on-sycophancy/
[O7]: https://status.openai.com/incidents/01K9D7DASB76TK1DEGPMG6ZAM4
[C1]: https://support.claude.com/en/articles/11817273-use-claude-s-chat-search-and-memory-to-build-on-previous-context
[C2]: https://support.anthropic.com/en/articles/11473015-retrieval-augmented-generation-rag-for-projects
[C3]: https://support.claude.com/en/articles/12260368-use-incognito-chats
[C4]: https://support.claude.com/en/articles/8325621-i-would-like-to-input-sensitive-data-into-my-chats-with-claude-who-can-view-my-conversations
[C5]: https://support.claude.com/en/articles/12123587-import-and-export-your-memory-from-claude
[G1]: https://support.google.com/gemini/answer/16598625?hl=en
[G2]: https://support.google.com/gemini/answer/16598469?hl=en
[G3]: https://support.google.com/gemini/answer/13594961?hl=en
[G4]: https://support.google.com/gemini/answer/13275745?hl=en
[MS1]: https://support.microsoft.com/en-gb/privacy/microsoft-copilot/overview
[MS2]: https://support.microsoft.com/en-gb/privacy/microsoft-copilot/privacy-controls
[MS3]: https://support.microsoft.com/en-us/microsoft-365-copilot/manage-copilot-memory-in-microsoft-365-copilot
[MS4]: https://support.microsoft.com/en-gb/privacy/microsoft-copilot/activity-history
[P1]: https://help.pi.ai/en/articles/15425697-pi-s-memory
[P2]: https://help.pi.ai/en/articles/13147160-how-do-i-copy-export-or-share-a-conversation-safely
[P3]: https://inflection.ai/privacy-policy
[CA1]: https://blog.character.ai/memory/
[CA2]: https://blog.character.ai/helping-characters-remember-what-matters-most/
[CA3]: https://support.character.ai/hc/en-us/articles/30299431702555-How-do-I-export-all-my-data-How-can-I-export-my-chats
[CA4]: https://support.character.ai/hc/en-us/articles/47703013822875-Training-Data-Documentation
[CA5]: https://support.character.ai/hc/en-us/articles/42788047758747-How-do-I-manage-update-my-model-training-settings
[CA6]: https://blog.character.ai/pipsqueak2-and-more/
[L2]: https://help.limitless.ai/en/articles/13048802-where-can-i-find-my-rewind-data
[L3]: https://help.limitless.ai/en/articles/13024699-how-do-i-delete-uninstall-rewind
[L4]: https://help.limitless.ai/en/articles/10761340-pendant-storage
[L5]: https://help.limitless.ai/en/articles/10546658-interacting-with-the-pendant-search-ask-ai-summaries
[L6]: https://help.limitless.ai/en/articles/13005203-how-to-delete-your-limitless-account-and-data
[L7]: https://www.limitless.ai/
[L8]: https://help.limitless.ai/en/articles/13004190-talking-to-someone-wearing-the-pendant-what-to-expect-and-how-we-handle-your-information
[L9]: https://help.limitless.ai/en/articles/10540861-how-to-ask-for-consent-and-let-others-know-you-are-recording
[A1]: https://www.apple.com/newsroom/2024/10/apple-intelligence-is-available-today-on-iphone-ipad-and-mac/
[A2]: https://www.apple.com/newsroom/2026/09/siri-ai-a-profoundly-more-capable-and-personal-assistant-is-here/
[A3]: https://support.apple.com/guide/iphone/apple-intelligence-and-privacy-iphe3f499e0e/26/ios/26
[N1]: https://www.notion.com/help/notion-ai-security-practices
[N2]: https://www.notion.com/help/enterprise-search
[N3]: https://www.notion.com/help/notion-ai-connectors
[N4]: https://www.notion.com/help/export-your-content
[ME1]: https://get.mem.ai/features/chat
[ME2]: https://get.mem.ai/pricing
[ME3]: https://get.mem.ai/pages/privacy
[ME4]: https://get.mem.ai/pages/security
[ME5]: https://get.mem.ai/pages/terms-of-service
[R1]: https://arxiv.org/abs/2602.01450
[R1HTML]: https://arxiv.org/html/2602.01450v3
