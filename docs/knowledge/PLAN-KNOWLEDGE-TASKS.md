# Knowledge 组件任务方案 v2.1（Claude × Codex gpt-6-astra 共同制订稿）

> **来源与适用范围**：本文是 2026-10-15 定稿的任务方案 v2.1 原文，首发于 [#986 的评论](https://github.com/iDoris-ai/Agent24/issues/986#issuecomment-6195290128)。KB-C0-01（[#1010](https://github.com/iDoris-ai/Agent24/issues/1010)）把它作为已评审的设计输入同步到 `ab/weknora`，正文未作改动。
> 正文是当时的状态，此后的变化以下列来源为准：
> - **现状与差异（§0、§6）**：当时核实的结果。之后的更正见 WeKnora [能力矩阵](https://github.com/iDoris-ai/WeKnora/blob/a24/develop/docs/agent24/CAPABILITY-MATRIX.md)和 [#989](https://github.com/iDoris-ai/Agent24/issues/989) 的回写评论。
> - **契约**：见本目录的 `CONTRACT-*.md`。
> - **任务状态**：见 [projects/2](https://github.com/orgs/iDoris-ai/projects/2) 和各 issue。
> - **§5 各项的裁决**：见 [#986](https://github.com/iDoris-ai/Agent24/issues/986) 和 #991–#997。

2026-10-15。v1 由 Claude 合并 Codex 独立方案（31 任务）与三份调研写成；Codex（gpt-6-astra，high）复核后 REQUEST_CHANGES，本稿逐条吸收其 D1–D8 和 37 个任务意见，Codex 引用的代码事实均已由 Claude 在源码复核。
性质：任务方案，不是实施授权；KB-C1–C4 仍 BACKLOG；不恢复 OD-M11/TPI-W/Line W；README §12"DOC-1 期间 Line K 只起草契约"的限制未被解除，§22 允许的探测/PoC 不等于产品实施。

## 0. 核实的现状（文档里没有或与文档不同的）

Agent24
- origin/main=6072fd37；ab/weknora=ab125de2，落后 main 68 提交；ruleset `ab-integration` 未含 ab/weknora。
- PR #906（head 0000d64c，jhfnetboy，base ab/documenting）：PR-Daemon 两次 APPROVE、CI 6/6、MERGEABLE，仍 OPEN。ab/documenting 领先 main 121 提交，不能整支带入。ab/kernel 的 K1 来源政策（source_policy）未到 main。
- 宿主代码：SidecarManager + agent24-sidecar-host 已实现但无产品调用方，TS↔Rust 胶水不存在（OD 走 CreativeServeWeb）；OD WebContentsView 无 preload、持久 partition、IPC 无 sender 校验、分区无 CSP，未达 ADR-004；RunManager 任务前注入写死 memory.recall（agent24-agent/src/lib.rs:1394），无 context hook；per-run privacy 缺口确认（agent loop/subagent/摘要/REST 均 TaskProfile::default，RunCreate 无 privacy/org/principal）；Read 级工具只在 requires_approval 时过审批门（agent24-tools/src/lib.rs:573）；fixtures_roundtrip.rs 只读 events 目录；MCP 只有 stdio；http_fetch 拒绝 loopback。

WeKnora fork（a24/develop=4b363d15 = v0.8.2 + 2 文档提交）
- 上游 10-14 发布 v0.8.3（无 assets）。同步工作流自 10-13 连续失败：令牌缺 `workflows` 权限，推镜像被拒；两条镜像停在旧位置。
- 已有可复用：scoped API key（25 个 capability、knowledge_base_ids 白名单、APIKeyGate 默认拒绝；**空白名单=全租户可见**，tenant_api_key.go:328）；signed_token 外部主体（HS256/aud/exp≤24h/tenant_id/sub，但**只返回 sub，不影响 ACL，不进审计**，auth.go:551-658）；POST /knowledge-bases/{id}/hybrid-search（请求 ctx、类型化错误、KBAccessRead）；GET /knowledge/{id}、chunks 路由、/download（retrieve key 可用）、/preview；/system/info、/system/capabilities（返回部署能力声明，不能证明依赖健康或业务就绪）；租户隔离、撤权下一请求生效。
- 缺：集成模式总开关（agents/sessions/chat/skills/sandbox/mcp/im/embed/memory/websearch/organizations+shares/local-browser 对 JWT 无法关闭；多条路由注册在 Auth 之前 router.go:174-205；.env 的 WEKNORA_SANDBOX_MODE 是死配置；RBAC 可配置为放行 rbac.go:89）；用户级授权与审计；/ready（/health 写死 ok）；无头 Lite 托管契约（默认 0.0.0.0:8080、:0 不回报端口、无 ready 行/--version、JWT_SECRET 空则重启随机；auto-setup 只限 Wails 桌面版）；检索契约（/knowledge-search 脱钩 ctx 且错误一律 500；SearchResult 无 revision，ContentRevision 不序列化，扩展后偏移可能失效 search.go:211/222）；主 SPA 无嵌入模式（frame-ancestors 'self'、token 在 localStorage）；**日志中间件记录请求/响应正文、hybrid-search 记录 query**（logger.go:180/253、knowledgebase.go:346）；release-lite.yml 不按 tag 检出、会发 Release 并推 Homebrew 到 main（:30/:73/:409/:451）；只读检索也会算 query embedding（knowledgebase_search.go:206）；无默认 KB 创建、无租户级配置档、key/principal 只能 JWT Owner 管理。

## 1. 架构决策（复核后定稿）

D1 **检索走 hybrid-search**。契约只规定授权范围、总预算（deadline/并发/返回量）、partial 与排序、引用语义；adapter 首版逐 KB 调用或按兼容 embedding 模型分组复用上游多库检索，不直接比较不同模型原始分数。精确引用需要索引修订 + 定位有效性，正文 read 与修订映射单列（KB-C1-01c）。
D2 **身份信任模型（改写）**。租户 HMAC **绝不下发 Desktop**（持有者可为任意 sub/kb_ids 签名）。remote：由可信签发服务验证登录/成员资格/授权后签发用户绑定、可撤销的受限凭据；local-managed：宿主只管理本实例专属秘密。服务端有效范围 = 权威用户授权 ∩ key 范围 ∩ 请求范围，**空交集拒绝，永不回写空数组**。区分一次性交换票据（原子消费）与可重复使用的短期访问凭据；jti 需配消费/撤销机制才算防重放。
D3 **组织 = 租户映射保留，删除"一组织一把 key"**。首版组织试点须先定：全员同权的共享集合，还是用户差异权限（后者先做 D2 权威授权）。集成模式禁止消费既存跨租户/Agent 共享授权（access/knowledgebase.go:117-127），并测同组织两用户 + 跨组织隔离。
D4 **Tool-first**。首版 knowledge.search / knowledge.read Tool；Read 分类不等于审批或出口许可，Provider 自己强制身份/范围/Off/处理目的地/预算。证据用独立 Knowledge 来源/引用标识，借鉴 M1 的"数据不是指令"包裹。明确：README §9 的任务前自动背景注入在 KB-CTX-02 之前**未交付**。
D5 **KnowledgeView 按 ADR-004**（:117 非持久分区、:118 无通用 preload、:120 分区 CSP、:145 sender/frame 校验）；分区身份含 app instance + 主体边界，不只靠 profile 名；view 崩溃恢复不顺带重启健康 sidecar（:112）。具体无 preload 消息通道由 KB-C2-04a 先验证。
D6 **fork 自建 Lite 制品**，但新建"按精确源码 SHA、仅无头制品、默认不发布"的构建入口，所有 job 同 SHA，不改 Homebrew/main、不吞错；签名/发布/安装验证另设步骤。macOS arm64 为工程验证平台，对外支持范围由产品定。
D7 **同步修复与 v0.8.2 固定 SHA 探测并行**；产品补丁开始前由工程按差异/迁移/回归定基线（仅涉及新费用/出口/许可/范围时升级为用户决策）；保留 UPSTREAM-BASELINE.json 的 initial_snapshot 语义，另记当前基线；升级走正常 merge。
D8 **契约 = 机器可验证 schema + 显式 Rust/Ajv/Go 测试接线**（新增 fixtures 目录要显式注册）；Agent24 对外 REST 仍在 protocol/openapi.yaml；/system/capabilities 的 integration 字段是拟新增扩展（协议版本 + 能力声明），业务就绪另行表达。

## 2. 工作流与任务

S≈1 个 ≤300 行 PR，M≈2–3 个，L≈4–6 个；repo：A=Agent24→ab/weknora，W=WeKnora→a24/develop。安全/身份/权限/wire/写入语义类 PR 等 PR-Daemon APPROVE。

### WS0 立即事项与 KB-C0 收尾
| ID | repo | 规模 | 内容与验收 | 依赖 |
|---|---|---|---|---|
| OPS-02 | 设置 | — | 把 refs/heads/ab/weknora 加进 ruleset ab-integration。**在第一次向 ab/weknora 写入前完成**；admin 身份≠授权，先由 jason 确认规则变更与 bypass 配置。验收：普通 task 流程不能直推 | 无 |
| OPS-01 | W | S | 修同步工作流：同时替换 checkout 的 git push 凭据和 GH_TOKEN（agent24-upstream-sync.yml:21/27/47），用最小权限 App/专用凭据；保持原子快进、不自动合并；验收同步 PR 的适用 CI 真的运行。不阻塞固定 SHA 探测 | 凭据所有者 |
| OPS-03 | A | S | ab/weknora 经受保护流程 merge main（不 rebase），CI 绿、冲突逐项审 | OPS-02 |
| KB-C0-01 | A | S | #906 合入后，独立 PR 只同步 ADR-KB-01、PLAN-KNOWLEDGE-CONVERSATION、tasks 台账 Knowledge 段 + 本方案（含 D2 修订、阶段门、DOC-1 期间实现限制）；不带 Line D 实现 | #906 合入, OPS-03 |
| KB-C0-02 | W | S | 产品基线决策与登记：留 v0.8.2 则直接登记（不依赖 OPS-01）；升 v0.8.3 则依赖同步 PR 并跑 Go/前端/契约回归；保留 initial_snapshot，另记当前基线与 PATCH-LEDGER | （升级时）OPS-01 |

### WS1 契约切片
| ID | repo | 规模 | 内容与验收 | 依赖 |
|---|---|---|---|---|
| KB-CT-01 | W | M（2 小 PR） | ①docs/agent24/CAPABILITY-MATRIX.md：固定受测 SHA 的上游事实（file:line）与缺口；②可复现脱敏探测脚本 + 本机标准栈单用户样本证据：scoped key、signed_token、hybrid-search、chunk 正文读取、同租户两主体、空范围、越界资源、取消、JWT/API-key 差异。报告是探测依据不是运行证据 | 无（不等 OPS-01） |
| KB-CT-02a | A | S | 只读语义与信任边界契约文档：D2 签发权威、范围空值语义、deny-all、read 与修订定义、索引状态/stale、错误分类、取消语义 | KB-CT-01 |
| KB-CT-02b | A+W | M | 机器 schema + 夹具 + 消费者接线：Rust（显式注册 knowledge fixtures 目录）、Ajv、Go 测试；夹具按版本/摘要固定 | KB-CT-02a |
| KB-CT-03 | A+W | M（3 PR，按消费方逐段冻结，各自独立完成条件） | ①profile/Off/撤权状态机；②UI 身份交换与消息桥；③初始化/ingest/job/ContextPack/SOP 的授权与幂等语义。首版只读实现不等待③ | KB-CT-02a |

### WS2 KB-C1 主对话只读
| ID | repo | 规模 | 内容与验收 | 依赖 |
|---|---|---|---|---|
| KB-C1-01 | W | M | 集成模式**显式允许列表**（不是黑名单）：覆盖 JWT、API key、鉴权前路由（router.go:174-205）与后台入口；禁止未批准写入/管理能力、禁止消费既存共享授权；RBAC 不可配置为放行；/system/capabilities 如实；处理死配置 WEKNORA_SANDBOX_MODE；Go 测试"直接调用被拒"。是开放真实数据/UI 的硬门，不阻塞隔离样本 transport PoC | KB-CT-02a |
| KB-C1-01b-a | W | S | 版本/协议发现：retrieve key 可读 version + integration 能力声明；cmd/server --version | KB-CT-02a |
| KB-C1-01b-b | W | M | 按操作就绪探针：分别表达进程存活、存储可用、指定 KB 可检索、ingest 可用；search 就绪不依赖 chat/docreader/默认 KB，避免与初始化互相等待 | KB-C1-01b-a |
| KB-C1-01c | W | L（分 PR） | search/read 一致性：①hybrid-search deadline/错误分类/统一错误信封；②正文读取（复用 GET /knowledge/:id 与 chunks 路由，字节/分页上限，校验检索时修订，旧版本不可取返回 stale，spans 不当正文）；③源修订—索引构建—chunk 修订映射（updated_at/file_hash 不充当索引修订） | KB-CT-02a |
| KB-C1-02a | A | S | crate `agent24-knowledge` 骨架：KnowledgeTransport trait + Mock + 连接/解码；目标只来自可信配置 | KB-CT-02b |
| KB-C1-02b | A | M | Http 实现完整边界：流式读取上限、超时、TLS、禁代理/重定向、凭据不跨 origin、取消；批准样本上可先于总开关做 PoC，但 PoC≠可开放产品 | KB-C1-02a |
| KB-C1-03 | W | L（授权与审计分评） | 按 D2 重写：权威主体绑定、权限收窄（∩，空拒绝）、撤销；key 与 JWT 一致的资源授权；jti 配消费/撤销机制；principal 进审计/访问日志，新增 search/read 审计事件；集成模式拒绝 direct_header | KB-CT-02a, 身份服务交付（见 §5） |
| KB-HOST-01 | A | M | Electron main ↔ Rust daemon 控制边界：系统凭据存储、可信 profile、短期凭据/本地端点私有交接、Provider 实例化、Tool 注册、run 绑定；禁止通用 renderer proxy 传 URL/token；验收重启/主体变化/切换/迟到响应。RunCreate 需补 run 身份字段 | KB-CT-02b, KB-CT-03①, KB-C1-02a |
| KB-OBS-01 | W | M | 服务端默认无正文日志/追踪：保留主体/资源/修订/结果/计量元数据；覆盖 logger 中间件、handler/service、审计、反代、Langfuse；哨兵秘密负例证明正文与凭据不落日志。真实数据接入前完成 | KB-CT-02a |
| KB-SVC-POLICY-01 | W | M | 服务端版本化处理政策：允许的 embedding/解析/存储目的地、是否允许外部处理、调用/输入/并发/deadline 限制、禁止未授权回退；按首版实际流水线实现，不造通用预算系统。验收：未授权端点零调用、后台作业继承授权、预算耗尽拒绝 | KB-CT-02a, KB-C1-01 |
| KB-C1-04 | A | L | KnowledgeProvider：消费用户绑定凭据（不再本机签组织 HMAC）；profile/主体/政策绑定到 run（不读全局活动 profile）；知识调用总预算/并发/字节上限；持久 Off（重启仍 Off）+ generation（先阻新调用、再取消在途、丢迟到响应）；首期不缓存证据；M1 开关独立 | KB-C1-02b, KB-CT-03①②, KB-HOST-01, KB-C1-03 契约 |
| KB-C1-05 | A | M（Tool / 引用 UI / DOC-2 测试客户端分 PR） | knowledge.search / knowledge.read Tool + 证据格式 + 主回答引用（精确修订、partial、失败态、打开时重新授权、拒绝错修订）；负例：断线/超时/撤权/取消/旧索引/提示注入。真实数据验收另依赖 **KB-C1-03 实现完成且授权/撤销负例通过**、KB-C1-01、KB-SEC-01b、KB-OBS-01、KB-SVC-POLICY-01；Mock 与批准样本开发可先行 | KB-C1-01b-a, KB-C1-01c, KB-C1-04, KB-HOST-01 |
| KB-SEC-01a | A | S | 早期登记：K1 精确依赖（ab/kernel source_policy 等）、各缺口 owner、私有数据禁用门；不等 C1-05 | KB-CT-02a |
| KB-SEC-01b | A | M | K1 经 main 发布后的消费与全路径出口验收（回答/摘要/续轮/恢复/委派/记忆/HTTP/MCP/Exec/后台）；LocalOnly 远端调用计数为零。通过前只用批准样本；内网服务器≠设备本地 | KB-SEC-01a, K1 发布, KB-C1-04 |

### WS3 KB-C2 隔离知识页
| ID | repo | 规模 | 内容与验收 | 依赖 |
|---|---|---|---|---|
| KB-C2-01 | W | M | 身份交换：消费 D2 的用户绑定凭据，换短期 session JWT，并在**每个资源入口**执行 KB 范围/角色/受众/撤销（不是只加 claims）；集成模式 CORS 收紧。frame-ancestors 不放宽（WebContentsView 是顶层承载，无 iframe 需求） | KB-CT-03②, KB-C1-03 |
| KB-C2-02 | W | M | `/embed-admin/knowledge-bases[/:kbId]`：复用 KnowledgeBaseList/KnowledgeBase（documents/wiki），无 Menu/⌘K/聊天；**首版只读**，上传/编辑待 C4 授权；复用前审计组件隐含写入与聊天调用；凭据走内存 session adapter，不沿用 localStorage/refresh；主题/消息不是凭据或权限来源 | KB-C2-01 |
| KB-C2-03 | A | M | KnowledgeView（ADR-004）：非持久分区（实例 + 主体）、无 preload、分区 CSP、导航/弹窗/下载策略、退出/切换清理、与 OD 零共享 | KB-CT-03② |
| KB-C2-04a | A | S | PoC：无 preload 页面如何进入受校验的宿主通道（window.postMessage 不到达 main）+ 身份交接；主题/状态 PoC 不等 C1-05 | KB-CT-03②, KB-C2-03 |
| KB-C2-04b | A | M | 受限桥产品化：版本化、只允许状态/引用/取消/回主对话意图；绑定实际 WebContents/frame/generation/请求 ID（不信消息自报 origin）；E2E 同窗口、无第二历史 | KB-C2-04a, KB-C2-01, KB-C2-02, KB-C1-05 |

### WS4 Lite、Sidecar 与切换
| ID | repo | 规模 | 内容与验收 | 依赖 |
|---|---|---|---|---|
| KB-LITE-01a | W | M | 无头托管运行档（managed 模式，不改 standalone 默认）：loopback 默认、`:0` 回报端口、专用受控握手通道（token 不进普通日志）、JWT_SECRET/SYSTEM_AES_KEY 持久化、env 文件；进程握手与模型就绪分别报告 | KB-CT-01, KB-C1-01b-a |
| KB-LITE-01b | W | M | 按 D6 新建固定 SHA 无头构建入口（不发 Release、不推 main、不吞错）；签名 manifest 绑定 SHA/架构/许可证；实测体积/解压/启动/峰值内存（模型另算） | KB-C0-02 |
| KB-LITE-02 | A | L（分 PR） | Knowledge SidecarLauncher/readiness adapter + **TS↔Rust 进程树控制胶水**（先决定是否采用已有 agent24-sidecar-host，再满足 SidecarManager 的 tree/进程组契约 sidecar-manager.ts:190/195）+ 私有 endpoint 交接 + 首次产品接线；负例：抢端口、旧实例、启动中取消、崩溃残留、Desktop 退出回收 | KB-LITE-01a, KB-HOST-01, KB-C1-04 |
| KB-LITE-03a | A | M | 按需下载、manifest/签名验证、原子安装；首次安装→开通→重启场景 | KB-LITE-01b, KB-LITE-02 |
| KB-LITE-03b | A | M | 数据迁移/备份/真实恢复演练；固定数据库与配置兼容策略；验证回滚二进制可读数据。完成前不宣称可升级 | KB-LITE-03a |
| KB-LITE-04 | A | M（两阶段） | ①远端 profile 切换/注销隔离（不依赖 Lite）：撤销宿主调用资格、销毁页面会话、处理既有会话/历史/引用可见性；②本地 ownership 停止语义随 LITE-02 接入。hide≠disable，断开远端不停服务器 | ①KB-C2-04b；②KB-LITE-02 |

### WS5 KB-C4 首版一站式（文字优先、固定配置档）
| ID | repo | 规模 | 内容与验收 | 依赖 |
|---|---|---|---|---|
| KB-C4-01 | W | M | 版本化配置档 + 能力预检：引用实际允许的模型/存储/处理端点/额度权威（builtin 条目≠已安装/已授权）；分别显示可检索/可上传/可解析；LocalOnly 不回退云端；不支持格式上传前显示 | KB-C1-01b-b, KB-SVC-POLICY-01 |
| KB-C4-02a | W | M | 可信服务侧 bootstrap：引导凭据与日常凭据分离；平台 key 不进 Desktop、不扩权 | KB-C1-03 |
| KB-C4-02b | W | M | 幂等默认库初始化：按已验证身份与配置版本建默认 KB 并绑定授权；并发/中断/重试唯一性、失败回收；初始化与 ready 不互相等待 | KB-C4-02a, KB-C4-01 |
| KB-C4-03 | A | M | 激活凭据桥 + 最多三问文字引导（Mock 可先做）；系统凭据存储经 KB-HOST-01；无共享根 Key；License 不授予成员/出口/执行权。**外部依赖：销售/激活凭据服务及其 owner**；SEC-01b 只阻塞真实敏感数据启用 | KB-C4-02b, KB-C2-04b, KB-HOST-01, 销售服务 |
| KB-C4-04 | W | L | 授权 ingest/job：状态 saved/processing/indexed/failed/cancelled；原件↔源修订↔hash↔index build；服务端按身份/目标库/内容做幂等，后台作业沿用授权；取消后已保存原件/已发生费用如实报告；公开样本开发不等 SEC-01b | KB-CT-03③, KB-C1-03, KB-C1-01 受审写入扩展, KB-SVC-POLICY-01, KB-C4-02b |
| KB-C4-05 | A | M | 产品验收分三档：远端单用户（C4-03/04/C1-05）、个人 Lite（+LITE-03a/b、LITE-04②）、组织（+ORG 门）。文字首次体验完成≠ContextPack/SOP/全部 KB-C4 完成 | 按档 |

### WS6 受审背景包与 SOP
| ID | repo | 规模 | 内容与验收 | 依赖 |
|---|---|---|---|---|
| KB-CTX-01 | W | M | 批准版本 ContextPack（受众、来源修订、授权版本、有效期、截断、发布/撤销/失效）；预置批准样本即可验证契约；1–2k tokens 为待验证预算；批准内容不获得系统指令优先级 | KB-C1-03, KB-C1-01c |
| KB-CTX-02 | A | L（先定入口契约再分 PR） | 自动背景注入门：统一 hook 覆盖 fresh/续轮/恢复/审批继续/委派；真实 run 主体/组织/隐私绑定（RunCreate 补字段）；当前政策重验、attached/empty/skipped/failed；组织身份缺失时只支持个人包。此项完成前 README §9 自动上下文不算交付 | KB-CTX-01, KB-HOST-01, KB-C1-04, KB-SEC-01b |
| KB-SOP-01 | A | M | 从已授权会话生成可编辑/可撤回 SOP 候选（样本可先验证）；候选默认不进可检索正式库；批准绑定主体/目标/内容 hash/基础修订/用途；SOP 发布批准与 ContextPack 批准分开 | KB-CT-03③, KB-SEC-01b |
| KB-SOP-02 | W | M | 服务端重新验证批准（主体/权限/目标/hash/基础修订/有效期，不信宿主 approved 布尔）；幂等写入、冲突、撤回；复用 ingest 提交机制 | KB-SOP-01, KB-C4-04, KB-C1-03 |

### WS7 组织放量
| ID | repo | 规模 | 内容与验收 | 依赖 |
|---|---|---|---|---|
| KB-ORG-01 | A | M | 组织身份契约与 D2 一起前移冻结；实现消费身份 owner 交付的主体/活动组织/成员资格、撤销事件/查询；只依赖 LITE-04①；机制缺失时组织模式不开放 | KB-C2-01, KB-LITE-04①, KB-SEC-01b, 身份服务 |
| KB-ORG-02a | W | M | 基本 ACL/撤权放量测试：同组织两用户 + 跨组织，覆盖搜索/计数/read/预览/下载/引用/缓存/后台作业；撤权后结果不得继续交付/发布；可先于 C4 ingest | KB-ORG-01 |
| KB-ORG-02b | W | 每连接器独立任务 | 连接器/后台同步逐个批准为能力扩展（不绕过 C1-01 对 datasource 的禁用）；内容同步≠源 ACL 同步 | KB-ORG-02a, KB-C4-04 |
| KB-ORG-03 | A+W | L | 放量门按实际开放功能列证据：身份、出口、容量（实测不承诺 16GB）、备份恢复、上游升级回归各有判定；基础只读组织试点不等 SOP/ContextPack/个人 Lite；jason 决定放量 | KB-ORG-02a（+按功能追加） |

### WS8 KB-C3
| KB-C3-EVAL | W | S | C1 稳定、SEC 门成立且服务端有步骤/费用/token/deadline/出口/取消/工具限制证据后才评估；评估通过也不自动授权实施 | KB-C1-05, KB-SEC-01b, KB-SVC-POLICY-01 |

## 3. 前两周（David 单人，五项优先工作，约 6 个 PR + 1 个设置）
0. 设置：OPS-02（jason 确认后由 David 改 ruleset）。
1. PR1 OPS-01 修同步凭据（需凭据所有者配合；与后面并行）。
2. PR2 OPS-03 ab/weknora 吸收 main。
3. PR3 KB-C0-01（仅在 #906 合入后；否则不空等，先做 4）。
4. PR4+PR5 KB-CT-01（两个小 PR：能力矩阵；脱敏探测脚本与证据）；若留在 v0.8.2，顺带完成 KB-C0-02 登记。
5. PR6 KB-CT-02a 只读语义/信任边界契约；schema 与跨语言测试接线（CT-02b）紧随其后。若严格只做 5 个 PR，顺延 CT-02a，不压缩 CT-01 的探测验收。
若决定升 v0.8.3，升级 PR 占一个名额并顺延。总开关、/ready 完整版、UI、Lite 都不在前两周。实施排期仍受 README §12 DOC-1 限制与 §5.1 裁决约束。

## 4. 风险（按严重度）
- 高：**身份权威错位**——租户签发密钥（HMAC）泄露允许伪造任意主体；共享组织访问 key 泄露暴露其整个授权范围。两者分别防护（签发权留在可信服务端，访问 key 按用户绑定收窄），不能据此否定用户绑定的受限凭据。→ D2；KB-C1-03 前只做单用户样本；远端签发服务的 owner 是外部依赖（§5.3）。
- 高：隐私保证只覆盖单次 HTTP；证据进入回答/摘要/委派/后台后外流；只读检索也会算 query embedding；日志中间件记录正文。→ KB-SEC-01a/b、KB-OBS-01、KB-SVC-POLICY-01 三道门；内网≠设备本地。
- 高：同租户无 per-user ACL；既存共享授权不因禁用路由而失效。→ D3；KB-C1-01 允许列表 + 禁消费既存共享；同组织两用户负例。
- 高：撤权/Off/切换只取消网络请求，已返回证据、缓存、后台作业继续被消费。→ 两端校验 generation；首期不缓存；如实说明已发送历史不可追回。
- 高：嵌入页绕过宿主审批（外链、下载、上游执行端点、鉴权前路由）。→ 服务端允许列表 + ADR-004 隔离 + 受限桥；验收含直接 API 调用。
- 高：revision/locator 缺失或失效导致引用错版本、SOP 覆盖人工规范。→ KB-C1-01c（L）先补映射；缺来源不生成确定引用。
- 高：David 同时承担 Line D/K，README §12 限制未解除，新增宿主/身份/销售职责无 owner。→ §5.1/5.3/5.6 必须拍板。
- 中：基线漂移（v0.8.2 / v0.8.3 / 研究用 main）与同步工作流失效。→ OPS-01 ∥ 固定 SHA 探测；KB-C0-02 定基线。
- 中：fork 补丁面扩大（允许列表、身份收窄、嵌入路由、Lite 托管档、日志政策），上游升级冲突增加。→ 全部进 PATCH-LEDGER；尽量做成配置开关与新增路由；每次升级验收"无第二对话"与既有能力。
- 中：Lite 体积/内存/签名/恢复未实测，auto-setup 不可用。→ KB-LITE-01a/01b/03b 分别实测。
- 中：SidecarManager 与 sidecar-host 从未接产品路径，胶水从零写。→ KB-LITE-02 标 L。

## 5. 必须由用户 / jason 拍板
1. David 在 DOC-1 期间能否进入 Knowledge 产品实现（README §12 vs #907）；Line D/K 与通用宿主工作的时间分配；平台缺口由谁承担。
2. 首个对用户开放的验收目标：远端单用户试点 / 个人 Lite / 组织；正式支持平台与部署运维边界。
3. **远端组织身份由谁验证，可信凭据签发服务部署在哪、由谁交付和运营**（D2 的外部依赖；不能用把主密钥放进 Desktop 来回避）。
4. 首版组织资料是否全员同权，还是必须支持个人/部门/项目差异权限；一组织一租户是否符合现有数据与身份归属。
5. 测试与正式资料允许经过哪些模型/解析/存储/追踪服务，费用上限由谁承担；LocalOnly 是否严格指设备本地，内网远端用何政策。
6. 销售/激活/额度/凭据服务的 owner、交付时间与吊销责任（Mock 不算商业闭环）。
7. 组织中谁可上传、下载原件、批准 SOP、发布 ContextPack（业务授权，不从 retrieve key 或页面角色推导）。
8. 成员退出/资料撤销/组织切换后，既有历史、引用、SOP、原件、审计的保留/删除要求；已发送给模型的内容不能承诺撤回。
9. OPS-02 ruleset 修改与 OPS-01 凭据的授权由谁给（admin 身份≠授权）；#907"全部 task 等精确 head APPROVE"与 AGENTS.md 普通 task 门槛若冲突，只需澄清一次。

工程可自行决定（不再上报）：#906 合并执行人按既有仓库责任；v0.8.2/v0.8.3 基线（除非涉及新费用/出口/许可/范围）；Tool-first 与 hook 后置；PAT/App 技术方案；macOS arm64 作为验证平台；schema 位置、token TTL、重放防护实现、检索分组、Sidecar adapter 方案。

## 6. 与现有文档的差异登记
- #907/#713 评论写 #906"无外部 approval"，实际已两次 APPROVE，待合并。
- #904/总体方案把身份交换、能力声明写成"待建"：上游已有 scoped key + signed_token + /system/capabilities，但 signed_token 不影响 ACL、不进审计，capabilities 不反映健康；差的是收窄、审计与真实就绪。
- 总体方案 §8"v0.8.2 无 Lite 产物"属实；fork 自带 release-lite.yml 但不能原样用（不按 tag 检出、发 Release、推 main、吞错）。
- 总体方案"Lite 默认仅本机"只对 Wails 桌面版成立，无头 cmd/server 默认 0.0.0.0。
- "借鉴 OD 隔离模式"：OD 现行实现未达 ADR-004，KnowledgeView 应按 ADR-004 新做。
- "复用 K1"：来源政策在 ab/kernel，未到 main/ab/weknora；HTTP/MCP/Exec 出口门仍后置。
- #713 引用 README §22 第 3 项，核对为第 4 项。
- FORK-MAINTENANCE 描述的"每 6 小时快进"自 10-13 起不成立。
- .env.lite.example 的 WEKNORA_SANDBOX_MODE 是死配置。

## 7. 共同制订记录
- Codex（gpt-6-astra, high）独立学习材料包 + Agent24 代码后给出 31 任务方案（session 01a13e33…，160k tokens）。
- Claude 合并为 v1（含三份调研的 file:line 事实）。
- 复核第一次误用 gpt-6.1-sol（408k tokens，REQUEST_CHANGES）；按项目要求用 gpt-6-astra 重跑（488k tokens，REQUEST_CHANGES：D1/3/4/6/7/8 CHANGE、D2 DISAGREE、D5 AGREE；34/37 任务要求改动；新增 KB-HOST-01/OBS-01/SVC-POLICY-01）。sol 另提的 KB-ID-01/READ-01/BUDGET-01/BUILD-01/SIDECAR-01/LOG-01 按 astra 意见分别并入 C1-03/C1-01c/C1-04+SVC-POLICY/LITE-01b/LITE-02/OBS-01。
- Codex 引用的 6 处关键代码事实（审批门、fixtures 目录、release-lite 副作用、日志记正文、空白名单语义、key 决定范围）由 Claude 在源码逐一核实。
- v2 回传 astra 最终确认（584k tokens）：D1–D8、任务拆分与三个新增任务已吸收，§5 一致；剩 3 条未吸收项（C1-05 真实数据验收须依赖 C1-03 实现完成；前两周 PR 计数；/system/capabilities 表述）和 1 条新引入表述问题（§4 首条混淆签发权与访问权），均已在 v2.1 修正。Codex 说明修完即可交付、无需再全面重审。
