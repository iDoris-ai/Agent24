# iDoris v0.3.1 接线执行计划

本计划以 bootstrap task PR 合入受保护 `ab/idoris` 为生效点；未合入前是执行候选，不代表能力已实现或规范提案已接受。
关联：[跨仓协调 #974](https://github.com/iDoris-ai/Agent24/issues/974)、[iDoris #506](https://github.com/iDoris-ai/iDoris/issues/506)。

## 权威与基线

- 跨仓字段、隐私、凭证及进程边界只引用 [消费方指针](../iDoris-consumer.md) 所链接的唯一 iDoris 规范；本计划不复制接口契约。
- 硬件事实、整机预算分配由 Agent24 负责，反向权威是 [HW-TIERS](../design/HW-TIERS-capability-hardware-matrix.md) §3.3/3.4 与 [ADR-036](../decision.md)。不重新推导另一套预算公式。
- Agent24 起点：main `622be5d7df0387b91028820624a778cb99cd6d43`。#964/#966 已合并；硬件快照不是已接通的预算 sender。main 能力不等于 stable v0.5.1 已发布内容。
- iDoris v0.3.0 不包含随后主线修复；031 接线与发布需新验收。文档修订 1.2 不是 wire version；R9 的 launcher、安装执行、角色标签决定仍待 owner 审阅。

## 工作流

集成分支为 `ab/idoris`，已先加入 `ab-integration` 精确保护名单；保留全部现有保护。
任务使用 `ab/idoris-NN-*`，PR base 只能是 `ab/idoris`，worktree 位于 `~/Dev/auraai/`，仅在笔记本执行。
每周至少 merge 一次 main，不 rebase 或 force-push。每片默认不超过 300 行；确实不可拆时解释依赖。
普通 task 按 AGENTS 的本地门禁、CI 和机械检查合入；授权、wire、数据写入等 task 必须等精确 head PRD 批准。
集成到 main 的最终 release PR 由人开审，并由 Jason 真机验收；代理准备可复核成果，不能代开或绕过该 gate。

## 任务与可观察验收

| 任务 | 实际交付 | 验收及先后关系 |
| --- | --- | --- |
| IDORIS-01 | 本计划、短消费方指针、旧提案的历史标识 | 受保护集成线存在；历史不覆盖；链接与实现状态逐项核对；合入后才作为后续任务来源。 |
| IDORIS-02 | 在模型调用边界传递真实 TaskProfile，复用现有类型与授权门 | 真实 HTTP mock 先证明当前实现没有 iDoris 调用；privacy/complexity 与内核关联 ID 到达实际请求，未配置时旧 provider 行为不变。 |
| IDORIS-03 | 专用 iDoris provider 与显式配置的独占路由 | 配置 IDORIS_URL 后不得回落直连 oMLX/Ollama；URL、身份、key、服务或模型错误明确失败；no_proxy、禁重定向与 loopback 地址限制有实际网络负例。依赖02。 |
| IDORIS-04 | 数据面 virtual key 与响应证据验证 | 复用已接受安全凭证边界，四类凭证隔离；不得用 Admin token。LocalOnly 对缺失/lan/remote locality 拒绝；真实 Record-Id、response.model 能保留。缓存来源按唯一规范处理。与03同批或明确依赖。 |
| IDORIS-05 | 硬件已分配字节预算的实际交付 | 从已有 HW 快照/分配职责取数；经受支持的独立 iDoris 启动配置传入。零值是真零、缺省兼容、OS 预留只扣一次。推荐和并发 load/CAS 均观察实际额度；不发明 Admin write/动态 HTTP API。 |
| IDORIS-06 | 手动双进程初始化与真实组合验收 | 从隔离 HOME 初始化 key、后端、真实 ID 绑定和预算；Agent24→真实 Rust iDoris→loopback fixture 成功；超预算零 load/POST、缺 backend 无直连/云回退、取消/超时、凭证隔离有反例。依赖03–05。 |
| IDORIS-07 | 031 发布可审核证据包 | 031 实际行为 smoke、当前 main 完整门禁、版本/资产/校验和与已缓存小模型离线调用；最后人工 gate 有具体可审成果。不能用030 smoke或fixture替代真机验收。 |

## 不可跳过的实现约束

- 网关在 loopback 不证明实际推理在本机；不把普通 OpenAiCompatProvider 简单标为 Local 代替 Served-Locality 绊线。
- 请求保留现有 max_tokens、tools、response_format、disable_thinking 等受支持参数。先以真实调用证明兼容差距，再修接线或对应 iDoris 行为；不删除 Agent24 字段来制造通过。
- 实际 timeout/cancellation 要贯穿两进程；现有30s/120s差距需测量并修复，不用 builder 参数断言代替执行证据。
- 实现数据 key 前核对 shell_exec/MCP 子进程的 env_clear 与白名单前置，复用既有方向成果；不让 key 进入日志、renderer、argv 或无关子进程。未决定的 Keychain service/account、launcher 宿主或新 secret framing 不擅自定案。
- Agent24 不拥有或启动 iDoris daemon。031 可先交付受支持的手动双进程路径；新 launcher 宿主或管理写协议另行决策。
- 模型选择影响实际 chat 请求；gateway 模式不绕过 iDoris warmup/stop。静态 Download 按钮、推荐表或高亮不等于模型可调用。
- M5 REDACT 只保留 iDoris 本地用户单次确认；长期 caller 授权不在本期。

## 门禁与证据

提交前按 AGENTS 执行 Rust workspace fmt/clippy/test，使用主 checkout 共享 target，ulimit 4096；仅其明确列出的四个沙箱失败可按白名单处理。
修改 apps/packages 时额外运行 pnpm frozen install/typecheck/lint/test；修改 protocol 时再跑 OpenAPI/codegen 无漂移门禁。
每个实现 task 先用真实行为跑红，再修复；记录精确 SHA、实际命令、HTTP 收发计数、预算变化及未验证边界。
PR 已开、本地通过、候选参数存在或相邻 PR 已合入，都不能代替当前 head 外部审核与组合验收。
