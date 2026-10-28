# CORE-2：本机登记握手、身份校验、两级权限与在场证明（设计说明）

> 2026-10-26。代码核对基线：`e0576841af3d3e8e3e2c2f474b90493dd48eb46c`。事实清单的调查基线同为 `origin/main` `e0576841`。清单开头所述旧工作区 HEAD `3ec8d81d` 是调查时的记录，不是本稿基线。
>
> **这是设计说明，不改代码。** 外审 APPROVE 只接受本稿的语义与分片，不批准实现。各实现片另开 PR，按授权类变更等待 PR-Daemon APPROVE。
>
> 上游为 ADR-037 决策 2、3（任务指定已于 #1249 接受）。本分支 `docs/decision.md` 仍标「提议」，差异见 §8。D1–D9 由协调者拍定，等级表逐项接受外审调整。
>
> 代码事实见 [CORE-2 事实清单](../research/CORE-2-FACTS-20261026.md)。下文 facts 指该清单。`d/` 指 `rust/apps/agent24d/src/`，`cap/` 指其中的 `capabilities/`。未注明现状的协议、字段和 Operation 名均为本稿设计，不代表已经实现。

## 1. 背景与现状

常驻 daemon 必须允许第二个外壳登记。可执行文件身份与用户对本次操作的授权必须分开。

| # | 现状与缺口 | ADR-037 要求 | 证据 |
|---|---|---|---|
| V1 | capability 启动必须有父管道，EOF 会关机，无法独立常驻 | 服务模式不随外壳退出 | facts §1.8、§2.2；`d/server.rs:1323-1361,2571-2589` |
| V2 | ProductHost 全权，TTL 为 `u64::MAX`；没有按进程登记、续期与退出失效 | 一方外壳按进程持有短期令牌 | facts §1.2–1.4；`cap/types.rs:13-17,93-106`、`cap/validate.rs:90-108`、`d/server.rs:1483-1487` |
| V3 | 路由只特判 models、mint、revoke，其余落入 ModelsAdmin；Creative 实际只能读 models | 每条路由分权 | facts §1.5、§1.6、§6；`d/server.rs:1016-1031` |
| V4 | 没有登记端点、身份校验或在场证明；审批仅靠 bearer | 身份通过只给日常级，提升另需证明 | facts §3–5；`d/attach_listener.rs:162-175` 只比 uid |
| V5 | CLI 遇 capability 报 host authority unavailable；桌面和服务均走 legacy；renderer 可经 backendProxy 借全权 token | 外壳统一发现与登记，renderer 不借全权 | facts §1.7、§7；`agent24-cli/src/main.rs:644-662`、`apps/desktop/src/main/ipc/index.ts:316-325` |
| V6 | HTTP 用 `Resource::global()`；WS 未接吊销通知；缺少 REST Host、Origin 门 | G-API 完整授权后才能宣传开发者 API | facts §1.1、§1.6；ADR-037 G-API-1–5 |
| V7 | daemon.json 无登记路径；签名配置未入库 | 可发现的登记端点；可核验的发布身份 | facts §4、§8.2；`agent24-protocol/src/state_file.rs:31-48` |

这些是现状与目标的差距。CORE-2 不靠切换默认模式来掩盖缺口。legacy 继续保持现有行为。

## 2. 决定

### D1 分片顺序与默认模式

顺序固定为 **2a → 2e → 2c → 2b → 2d → 2f**。先把路由与 Operation 列全，再允许服务启动，再做登记身份、令牌、证明和外壳接入。

capability 在 2f 与 G-API 全部完成前一直可选。CORE-2 不切默认值。默认切换另行决定。2c 可先交付身份握手与测试夹具，2b 接上真实令牌前不宣称客户端已经可用。2b 到 2d 之间，提升级操作拒绝执行，不临时放宽。

### D2 全量映射与缺省拒绝

`required_operation(method, path)` 按方法和匹配后的路由模板查表。§3 每行都有独立映射，包括免鉴权 health 的显式标记。同一路径不同方法不能共用一个权限结论。

补齐至少 MemoryRead、MemoryForget、MemorySettingsWrite、ChatCreate、CommRead、CommWrite、ModuleRead、ScheduleRead、UsageRead、ConfigRead。其余新增项见 §3。新增名要同时进入 action 解析与等级目录。新增 Operation 不自动加入 Creative 白名单。

CI 从实际注册路由得到方法与模板集合，双向比较映射表。少映射、多映射、重复映射或内核路由落入兜底都失败。comm 虽经 merge 加入，也属于固定产品路由，必须逐行列举。兜底仅用于已挂载模块的命名空间，不能把任意未知路径当模块。未知路径返回 404，已知路径的未注册方法返回 405。axum 自动 HEAD 采用对应 GET 权限，覆盖门另测 HEAD，不能绕过认证。

2a 只接 capability 判定。legacy 的单 token 逻辑不变。2a 不等于 G-API-1 完成，资源提取与 session、run 校验仍须补齐。

### D3 等级目录

§3 是建议等级表，外审可逐项调整。日常级包括 chat，会话与 run 的创建、读取、取消，事件与记忆读取，forget，审批状态与各类设置读取。forget 和 consent 撤销缩减权限，归日常级。

提升级包括审批决定、standing grant 与 override 管理、模块与 attached 写操作、停机、记忆设置写入、令牌签发与吊销、日程写操作、comm 写操作。日程删除也归 ScheduleManage。日程能产生未来自主动作，不因当前只改一个字段就降级。

模块自带路由暂归日常级，因为工具调用另有 consent 门。**这一条待外审裁决**，不据此声称模块的任意 HTTP 副作用都被 consent 覆盖。

等级表示一方外壳执行操作所需的最低证明。它不扩大令牌的 Operation 或资源范围，不替代工具 consent，也不把 Creative 变成一方外壳。开发者令牌高权作用域的签发确认与调用语义见 §8 Q6。

### D4 一次性提升授权

一方令牌的基础等级始终是日常级。daemon 不签发一段时间内全权的会话。

1. 外壳提交目标 Operation、可选资源及操作摘要。daemon 解析目标请求，生成随机 nonce，记录一方令牌摘要、generation、目标和请求内容摘要。客户端自报的摘要不能替代 daemon 的摘要。
2. 在场证明必须对应此挑战。涉及审批、设置、令牌作用域等可变 payload 时，挑战同时绑定 payload 摘要或版本，用户确认之后不能换参数。
3. 证明成功后，daemon 签发不透明的提升授权，只存摘要。绑定一方令牌、Operation、可选资源与 nonce，签发后 60 秒到期。挑战本身也设 60 秒期限，过期后重新发起。
4. 目标请求同时携带一方令牌与提升授权。daemon 先复验令牌存活、TTL、generation、Operation 与资源，再核对目标摘要，以原子操作消费 nonce，之后才进入有副作用的处理器。
5. 并发消费最多一个成功。授权已消费、过期、拒绝、令牌撤销或换代都不能重放。执行失败也不恢复已消费授权；再次尝试须重新证明。授权不代替业务幂等或审批 pending 状态检查。

资源可选表示全局操作可不带资源。资源型操作不能用空资源代替目标。未取得证明返回可识别的 `presence_required`，不触发副作用。取消、超时、辅助进程故障均按未证明处理。

### D5 登记端点、握手与身份

Unix 端点固定为 `<data_dir>/enroll.sock`，socket 权限明确设为 0600，父目录 0700。它不是现有 attach socket。单例锁持有者负责创建、发布、accept 和清理。先验证目录非符号链接且属主正确，再处理陈旧 socket；不能覆盖活端点或删除别的 generation 的端点。退出只清理本实例拥有的发现记录与 socket。

服务模式与 capability 子进程模式都开端点。换代清空登记、挑战、登记权和提升授权。先持单例锁、完成迁移、绑定端点，再发布 daemon.json 与 ready。登记端点或发现发布失败时，enroll 服务启动失败，不发布半就绪实例。此为服务路径对 facts §8.3「write 失败只 warn」的修订，legacy 不随之改变。

握手采用 UDS 上的独立版本化协议，不占 HTTP 路由。建议一行 JSON 一帧，上限 16 KiB，普通握手阶段超时 5 秒。只有等待人工配对码的阶段可延长到 5 分钟。未知版本、超长帧、断线、超时均关闭连接，不发令牌。

1. accept 后用 tokio `peer_cred()` 取 uid 与 `UCred::pid()`；macOS 同时用 `getsockopt(fd, SOL_LOCAL, LOCAL_PEERTOKEN)` 捕获 audit token（含 pidversion），核对其 uid/pid 与凭据一致。uid 必须等于 daemon 的 euid，pid 缺失即拒绝，不能采信客户端 JSON 里的 pid。
2. daemon 读取进程启动时间，生成连接 id 与随机挑战。响应包含协议版本、generation 和挑战。客户端核对发现文件 generation，禁止跨代继续握手。
3. daemon 解析可执行路径并校验身份，生成身份摘要。校验来源包括以下平台分支或一次性退路，摘要记录采用哪一种。
4. 发令牌前再读启动时间，并用 accept 时捕获的同一 audit token 复核身份。pid、启动时间与 pidversion 必须与初次观察一致。登记绑定此连接、audit token 和 `(pid, start_time)`。2b 在该连接返回一方令牌。
5. 登记通道可承载续期、提升挑战和确认消息。连接断开不等于进程退出，也不停止 daemon；重连必须重新核验，不能凭旧连接 id 恢复权限。

macOS 路径解析固定用 `proc_pidpath`。仿照 `agent24-os-fd` 新建独立小 crate，默认 deny unsafe，只在这个调用的窄封装允许 unsafe。其他 crate 保持 forbid。签名校验直接声明 `security-framework` 依赖，用 `kSecGuestAttributeAudit` 按原 audit token 构造 `SecCode`，执行 requirement：

```text
anchor apple generic and certificate leaf[subject.OU] = "HYAP444FN9"
```

同时要求运行进程的签名 flags 含 `CS_RUNTIME`，并检查签名 identifier 在白名单内：桌面 `ai.auraai.agent24`、`agent24`、`agent24d`。这是签名 identifier，不是文件名匹配。路径与 SecCode 指向的对象必须一致。文件改名、处于安装目录或同 Team ID 的其他程序均不足以登记。发布产物的实际 identifier 要由 DEP-B 验证，不能反向扩大白名单。

Linux 读取 `/proc/<pid>/exe`，对所指对象计算哈希，比较安装包附带的签名哈希清单。先验证清单签名与可信签发者，再比较二进制哈希。只有路径属主正确不够。清单验签与可信公钥部署属于发布依赖，不从待校验清单自取信任根。Windows 的 Authenticode、命名管道和 PID→SID 留给 PLT-WIN，不用 Unix 桩宣称已支持。

身份不通过有两条退路，二者都只给日常级，不充当提升证明：

- 可信启动者经匿名 FD 交付一次性登记权。沿用 `idoris-admin.ts` 的 65 字节帧：64 个小写 hex 加换行。argv 只含 FD 编号选择器，拒绝重复和 `=` 写法，范围 3..0x7fffffff。读取上限 65 字节、5 秒超时，等 EOF，成败都关闭；非法选择器关闭可识别候选 FD。写端交付完立即关，防后续子进程继承。登记权由 daemon 记录摘要并绑定 generation、目标连接挑战，可信启动者通过已认证通道取得，不能由任意客户端自己制造。连接重建就重新授权。登记权不可用来签发下一枚登记权。
- 无可信启动者时，daemon 在控制台或日志打印一次性配对码。daemon 为这条待配对连接生成码，5 分钟有效，核对后原子消费。其他连接即使提交同码也失败，断开即作废。配对提示显示连接和进程身份摘要，用户手动输入。码使用安全随机数，重试限频，不记录用户输入。日志输出是明确退路，具有同 UID 可读风险，见 §4。

FD 格式依据 facts §2.4；这里交付的是登记权，不是 iDoris Admin token。首个可信启动者如何取得连接绑定登记权，见 §8 Q3，不能暗中引入共享秘密文件。

### D6 一方令牌与 ProductHost 修订

新增 `Audience::FirstPartyShell`。claims 增加 pid、start_time、audit token（macOS 含 pidversion）、身份摘要和基础等级。身份摘要包括平台验证结果或退路类型，不包含配对码。令牌 TTL 为 1 小时，以单调时钟判定，可续期，内存只存摘要，不落普通文件，不进 argv、env、ready stdout、日志或 renderer。

FirstPartyShell 必须检查 allowed_operations 与资源，不能沿用 ProductHost 跳过这两项检查的分支。允许某个 Operation 只表示可以请求该操作，提升级仍须 D4 授权。每次请求和续期都检查绑定进程仍存活（排除僵尸）、启动时间一致，并复核原 audit token 的 pidversion，无法确认就拒绝。进程退出或 exec 后拒绝并删除令牌，不等到 TTL。续期前重验身份，签发新令牌并原子撤销旧令牌，旧提升授权随之失效；续期不能提升基础等级。事件流每次发送前的复验和断开仍由 G-API-4 交付。

进程绑定约束令牌的有效生命周期。现有回环 HTTP bearer 不能证明当前发送者就是绑定 pid，不能把这个字段宣传为防转交机制，见 §4。身份变化检测也不等于无竞态的进程隔离。

ProductHost 只留给子进程模式的父进程，由 `--host-bootstrap-stdio` 交付。服务模式不铸它，也不能经登记换取它。现状的全权与 `u64::MAX` 是待修订行为（facts §1.4），不是一方令牌的模板。ProductHost 补有限 TTL，建议同为 1 小时，父管道关闭仍关机；长时运行的续期通道与期限须在 2b 前裁决，见 Q4。

现有 Creative mint 仅允许 ProductHost（facts §1.4）。2b 需增加已登记外壳持一次性提升授权的签发与撤销授权路径。不能为了复用旧方法把 FirstPartyShell 伪装成 ProductHost。

### D7 在场证明通道

macOS 首选 daemon 启动 Swift 辅助进程调用 LocalAuthentication，策略允许 Touch ID 或系统密码。认证在 LaunchAgent 的 gui 会话显示系统界面。没有 gui 会话、策略不可用、拒绝或超时都不给提升授权。每次使用新的认证上下文，不借用上次成功结果形成提升窗口。

参照 Documenting 的进程外 Swift 辅助进程组织方式，不复用文档解析程序。已核对的先例为 `rust/apps/agent24-documents/engines/pdfkit/build.sh:1-12`，以 `xcrun swiftc` 构建系统框架 helper。它只证明打包方式已有先例，不证明 LocalAuthentication 已实现。

daemon 用受控路径启动签名 helper，经私有管道交付 nonce、Operation 与摘要，读取有界结构化结果。只有本次子进程成功退出且返回匹配结果才算成功。不能让调用者上传一个 `authenticated:true` 代替认证。helper 的签名、打包与 gui 会话真机验收是投产门。

桌面原生确认框为第二通道。daemon 生成 nonce，经已登记桌面主进程的通道送达操作摘要，桌面显示原生框，回传 nonce、摘要和确认结果。daemon 校验待处理挑战、桌面登记与目标令牌的关联，拒绝 renderer 直接提交确认。CLI 发起的操作也可由桌面确认，但授权绑定发起 CLI 的令牌，不转授给桌面。未连接桌面时不能伪造此通道。

headless 只定义提供者接口：输入挑战、目标与摘要，输出提供者身份、绑定证明、结果与时间。FIDO2、已配对手机的实现、首次配对和信任根属后续 PLT 线。接口保留 payload 版本绑定。TTY、父进程名、TOTP 或本机口令文件均不是证明。没有可用证明方式时只能使用日常级。

### D8 服务模式启动

新增 `serve --auth-mode capabilities --enroll-endpoint`。`validate_auth_startup` 与 serve 内重复校验保持一致。建议启动矩阵如下，组合互斥不靠参数顺序决定。

| 模式 | bootstrap | enroll | 结果 |
|---|---|---|---|
| legacy | 否 | 否 | 保持现状 |
| legacy | 是 | 否或是 | 拒绝 bootstrap 参数 |
| legacy | 否 | 是 | 拒绝 enroll 参数 |
| capabilities | 是 | 否 | 子进程模式，自动开登记端点，父 EOF 关机 |
| capabilities | 否 | 是 | 服务模式，无 bootstrap，无父 EOF 关机任务 |
| capabilities | 否 | 否 | 拒绝，无授权交付路径 |
| capabilities | 是 | 是 | 建议拒绝歧义组合，待 Q5 裁决 |

服务生命周期受服务管理器与已授权 `POST /shutdown` 控制，保留正常系统退出处理。`daemon.json` 增加 `enroll_endpoint`，不含 token。它仍包含 generation、端口、pid、版本与 auth_mode。子进程 capability 也发布登记路径。协议版本在握手协商，CORE-2 不捎带实现 CORE-1 的迁移水位或停靠态。

### D9 签名事实订正与发布前置

ADR-037 决策 5 写「签名加公证流程已跑通」。facts §4 只证实预发布说明声称成功，仓库未找到签名、公证、entitlements 和内嵌 daemon 签名配置。Team ID `HYAP444FN9` 只出现在预发布说明中。不能据此认定可重复的流水线已经存在。

**补齐签名流水线 DEP-B 是 macOS 身份校验投产前置。** 它必须提供桌面、CLI、daemon 与认证 helper 的实际签名 identifier、Team ID、签名验证和分发验证证据。开发构建、ad-hoc 和未签名 CLI 走 D5 的退路，不放宽 requirement，也不凭安装路径默认信任。本文不修改 ADR 原文，不替 DEP-B 宣布完成。

## 3. 完整路由 → Operation → 等级表

每行只写一个方法。所有路径省略共同前缀 `/api/v1`。日常、提升分别表示两级最低要求，health 是唯一现有免鉴权例外。Operation 是目标映射，不是当前实现。facts §1.5 未列出的名称均需新增。

内核依据 facts §6，并已核对 `d/server.rs:1056-1222`。comm 已核对 `rust/crates/agent24-comm/src/router.rs:158-176`。后者修正 facts §6 的方法概写：relay 写入为 PUT，现有 inbox 路径是 POST pull，没有 GET inbox。

| 方法 | 路径 | Operation | 等级 |
|---|---|---|---|
| GET | /health | HealthRead（显式免鉴权标记） | 公开 |
| POST | /chat | ChatCreate | 日常 |
| GET | /models | ModelsRead | 日常 |
| GET | /decide/profile | ConfigRead | 日常 |
| POST | /capabilities/creative | CapabilityMint | 提升 |
| POST | /capabilities/{capability_id}/revoke | CapabilityRevoke | 提升 |
| GET | /usage | UsageRead | 日常 |
| GET | /timings | UsageRead | 日常 |
| GET | /timings/summary | UsageRead | 日常 |
| GET | /tools | ConfigRead | 日常 |
| GET | /module-consents | ConfigRead | 日常 |
| GET | /module-consents/export | ConfigRead | 日常 |
| DELETE | /module-consents/modules/{module} | ConsentRevoke | 日常 |
| DELETE | /module-consents/modules/{module}/tools/{tool} | ConsentRevoke | 日常 |
| POST | /module-consents/modules/{module}/tools/{tool} | ApprovalDecision | 提升 |
| GET | /module-consents/modules/{module}/tools/{tool}/card | ConfigRead | 日常 |
| GET | /tool-overrides | ConfigRead | 日常 |
| GET | /standing-grants | ConfigRead | 日常 |
| DELETE | /standing-grants/{id} | GrantManage | 提升 |
| PUT | /tool-overrides/{pattern} | OverrideManage | 提升 |
| DELETE | /tool-overrides/{pattern} | OverrideManage | 提升 |
| GET | /memory/assertions | MemoryRead | 日常 |
| DELETE | /memory/assertions/{id} | MemoryForget | 日常 |
| GET | /memory/settings | MemoryRead | 日常 |
| PUT | /memory/settings | MemorySettingsWrite | 提升 |
| GET | /approvals | ApprovalStatusRead | 日常 |
| GET | /approvals/{id} | ApprovalStatusRead | 日常 |
| POST | /approvals/{id} | ApprovalDecision | 提升 |
| GET | /module-approvals | ApprovalStatusRead | 日常 |
| GET | /module-approvals/{id} | ApprovalStatusRead | 日常 |
| POST | /module-approvals/{id} | ApprovalDecision | 提升 |
| GET | /schedules | ScheduleRead | 日常 |
| POST | /schedules | ScheduleManage | 提升 |
| GET | /schedules/{id} | ScheduleRead | 日常 |
| PATCH | /schedules/{id} | ScheduleManage | 提升 |
| DELETE | /schedules/{id} | ScheduleManage | 提升 |
| POST | /schedules/{id}/run_now | ScheduleManage | 提升 |
| POST | /schedules/{id}/suspend | ScheduleManage | 提升 |
| POST | /schedules/{id}/resume | ScheduleManage | 提升 |
| GET | /events | EventsRead | 日常 |
| POST | /attached | ModuleAdmin | 提升 |
| GET | /attached | ModuleRead | 日常 |
| DELETE | /attached/{name} | ModuleAdmin | 提升 |
| PATCH | /attached/{name} | ModuleAdmin | 提升 |
| GET | /os | ModuleRead | 日常 |
| PATCH | /os/{name} | ModuleAdmin | 提升 |
| POST | /os/{name}/stop | ModuleAdmin | 提升 |
| POST | /os/{name}/commands/{command} | ModuleAdmin | 提升 |
| POST | /shutdown | Shutdown | 提升 |
| GET | /shutdown | ConfigRead | 日常 |
| POST | /sessions | SessionCreate | 日常 |
| GET | /sessions | SessionRead | 日常 |
| GET | /sessions/{id} | SessionRead | 日常 |
| POST | /runs | RunCreate | 日常 |
| GET | /runs | RunRead | 日常 |
| GET | /runs/{id} | RunRead | 日常 |
| GET | /peer/runs | RunRead | 日常 |
| GET | /peer/runs/{id} | RunRead | 日常 |
| POST | /runs/{id}/cancel | RunCancel | 日常 |
| GET | /comm/identity | CommRead | 日常 |
| POST | /comm/identity | CommWrite | 提升 |
| POST | /comm/identity/default | CommWrite | 提升 |
| POST | /comm/unlock | CommWrite | 提升 |
| GET | /comm/contact | CommRead | 日常 |
| POST | /comm/contact | CommWrite | 提升 |
| GET | /comm/relay | CommRead | 日常 |
| PUT | /comm/relay | CommWrite | 提升 |
| POST | /comm/relay/probe | CommWrite | 提升 |
| POST | /comm/import | CommWrite | 提升 |
| GET | /comm/daemon | CommRead | 日常 |
| POST | /comm/daemon/start | CommWrite | 提升 |
| POST | /comm/daemon/stop | CommWrite | 提升 |
| POST | /comm/send | CommWrite | 提升 |
| GET | /comm/history | CommRead | 日常 |
| POST | /comm/inbox/pull | CommWrite | 提升（见 §10 R2） |
| GET | /comm/outbox | CommRead | 日常 |
| POST | /comm/outbox/{event_id}/retry | CommWrite | 提升 |
| POST | /comm/outbox/clear | CommWrite | 提升 |
| GET | /comm/groupchat | CommRead | 日常 |
| POST | /comm/groupchat | CommWrite | 提升 |
| GET | /comm/groupchat/{group_id}/members | CommRead | 日常 |
| GET | /comm/groupchat/{group_id}/invites | CommRead | 日常 |
| GET | /comm/groupchat/{group_id}/messages | CommRead | 日常 |
| POST | /comm/groupchat/{group_id}/messages | CommWrite | 提升 |
| GET | /comm/groupchat/{group_id}/messages/{logical_id} | CommRead | 日常 |
| POST | /comm/groupchat/{group_id}/messages/{logical_id}/retry | CommWrite | 提升 |
| POST | /comm/groupchat/invites/{invite_id}/accept | CommWrite | 提升 |
| PUT | /host/knowledge/bindings/{binding_id} | KnowledgeHost | 提升 |
| DELETE | /host/knowledge/bindings/{binding_id} | KnowledgeHost | 提升 |
| PUT | /host/knowledge/bindings/{binding_id}/credential | KnowledgeHost | 提升 |
| DELETE | /host/knowledge/bindings/{binding_id}/credential | KnowledgeHost | 提升 |

模块动态路由另列，不混入上述固定路由覆盖率：

| 方法 | 路径 | Operation | 等级 |
|---|---|---|---|
| GET/HEAD | /<已委托模块命名空间>/** | ModuleRoute | 日常 |
| 其余方法 | /<已委托模块命名空间>/** | ModuleRoute | 日常；无委托记录或路由集摘要不符时拒绝（见 §10 R1） |

ModuleRoute 是新增的模块专用兜底，不复用 ModelsAdmin 或 ModuleRead。模块路由的具体方法由模块注册记录决定，不声称枚举所有未来模块。未知命名空间没有此兜底。现有 WorkspaceCreate、WorkspaceRelease、HostLease、WorkspaceResolve、SessionTranscript 等无路由项不人为补造路由。开发者令牌路由尚不存在，未来签发映射 CapabilityMint、吊销映射 CapabilityRevoke，均提升级；列表等新路由随 G-API 设计补表。

## 4. 威胁模型与残余风险

沿用 [ADR-031](../decision.md) 的边界：同 UID 进程未被沙箱隔离。UDS 0600 只能限制其他 UID，不能隔离同 UID。登记与两级权限主要挡住合作但不受信的 sidecar 和插件自动提权。

| 威胁 | 本稿处理 | 残余风险 |
|---|---|---|
| 同 UID 恶意程序读日志、注入进程或伪造界面输入 | 不放共享 token 文件；独立证明；连接绑定与限频 | 配对码日志可读，bearer 可被窃取或转交，原生确认可被输入自动化攻击；真正防护需 OS 隔离 |
| PID 复用 | 握手前后读启动时间，请求与续期检查 pid、start_time | 读取与使用之间仍有窗口；时间分辨率不足会削弱检查，2c 不可使用模糊时间字符串充当可靠标识 |
| fork + exec 换壳 | accept 后用 `getsockopt(SOL_LOCAL, LOCAL_PEERTOKEN)` 读取 audit token；SecCode 用 `kSecGuestAttributeAudit` 校验，并要求 hardened runtime。连接与令牌绑定同一 token（含 pidversion），首次校验、recheck、renew、elevate 和每次 HTTP 请求均复核；另对 enroll 连接的每一帧重读令牌并比对（§10 R7） | exec 改变 pidversion 后拒绝并删除令牌。此攻击可由调用方控制步骤顺序，不是无法控制的竞态。bearer 仍可转交，不声称隔离同 UID 程序。见 #1448 评审 M1 与 §10 R6 |
| 插件调用合法 CLI | CLI 只自动取得日常级，审批等逐次要求证明，不看 TTY | 插件仍可读记忆、建 run、取消 run；这正是 ADR-037 已接受的残余，不宣传成所有第三方只能用开发者令牌 |
| 开发或未签名构建 | FD 登记权或人工配对，只给日常级 | 可信启动者和人工确认承担身份判断；不能拿自动路径失败当作自动放行 |
| 模块 HTTP 副作用 | 暂按日常级，工具调用仍走 consent | 不经工具入口的模块路由可能无 consent，Q1 必须外审 |
| 旧代 socket、发现记录或授权重放 | 单例锁与 generation 一起管理，消费 nonce 原子化 | 崩溃留下节点需要谨慎清理，不能仅凭 pid 删除新代对象 |

审计记录登记结果、身份来源、令牌 id、Operation、目标摘要、证明通道、nonce 消费结果与拒绝原因。不记 bearer、提升授权、配对码、FD 秘密或原始敏感 payload。配对码的指定控制台或日志提示是唯一显式例外，不再复制到普通审计。

## 5. 测试门（实现片先 red → green，再做变异）

下表是未来实现的可证伪验收条件。本设计提交不声称这些测试已实现或已经通过。每片先展示缺行为的失败，再修复，保留变异导致失败的证据。

| 门 | 可证伪断言 | 对应变异 | 片 |
|---|---|---|---|
| T1 全覆盖 | 实际注册的每个内核与 comm 方法有唯一映射，未知路径无模块兜底；HEAD 受 GET 权限保护 | 新增路由但漏表；删除一个方法映射；将未知命名空间当模块；免鉴权 HEAD | 2a |
| T2 方法与等级 | §3 每行检查 Operation 与等级；GET shutdown 与 POST 分离，GET memory settings 与 PUT 分离 | 去掉方法条件；把日程写或审批降为日常；把 consent 撤销提级 | 2a |
| T3 legacy 回归 | 原有单 token 请求集合的成功、失败与 ready 行不变 | 让 legacy 经过新的等级判定 | 2a–2f |
| T4 启动矩阵 | D8 每行结果固定；非 ephemeral 服务可发现，关闭任何外壳不关服务；子进程父 EOF 仍关机 | 保留重复 bootstrap 拒绝；服务仍装 EOF 任务；删除子进程 EOF 任务 | 2e |
| T5 生命周期 | socket 0600、父目录0700；第二实例失败；换代旧登记与 nonce 无效；绑定或发布失败无 ready | 不 chmod；忽略单例锁；保留上一代记录；发布失败仍 ready | 2e、2c |
| T6 对端身份 | uid 错、pid 缺、伪造 JSON pid 均失败；符号链接父目录被拒 | 用请求 pid；跳过 uid；跟随目录 symlink | 2c |
| T7 macOS 身份 | 同 Team 且白名单 identifier 才通过；错 Team、错 identifier、未签名必须进入显式退路 | 仅比路径或文件名；删 requirement；白名单改前缀匹配 | 2c |
| T8 Linux 身份 | 有效清单签名且运行对象哈希相符才通过 | 跳验签；只比路径；校验清单自带公钥；篡改一字节仍成功 | 2c |
| T9 竞态与退出 | 注入前后 start_time 变化拒绝，身份复核变化拒绝；进程退出或 pid 复用后原令牌不可用 | 删除第二次读取或复核；只用 pid；退出后等 TTL 才拒绝 | 2c、2b |
| T10 FD 退路 | 65 字节、EOF、5 秒、重复选择器、非法 FD、超长、所有关闭路径都有负例；另一连接或二次兑换失败 | 接受64字节；不等 EOF；超时不关；去连接绑定或原子消费 | 2c |
| T11 配对码 | 绑定连接，只兑一次；5 分钟到期，断线作废，重试受限；只取得日常级 | 改为全连接通用；忽略到期；复用码；配对直接给提升 | 2c |
| T12 令牌 | 新 Audience、pid、start_time、身份摘要和日常等级齐全；3600 秒到期；续期撤旧，不落普通文件 | 复用 ProductHost；TTL 无穷；续期旧 token 可用；写入 daemon.json 或日志 | 2b |
| T13 提升绑定 | 同令牌、Operation、资源、nonce 与 payload 才执行；60 秒边界拒绝；并发消费恰一成功，失败不恢复 | 去任一绑定；跳过到期；非原子消费；异常回填 nonce | 2b、2d |
| T14 证明独立 | TTY、调用来源、renderer 布尔值均不能证明；拒绝、超时、错误结果无副作用 | 用 isatty 放行；信任 authenticated 字段；复用上次成功 | 2d |
| T15 两个 macOS 通道 | gui LaunchAgent 真机分别完成系统认证与原生框；确认带 nonce 和摘要；SSH 无 gui 无桌面时只日常 | 接受错 nonce、错摘要或未登记桌面；helper 非零退出仍成功 | 2d |
| T16 外壳迁移 | CLI、TUI、桌面各自登记、续期；一方退出只失效自身令牌，服务仍在；renderer 无任意全权转发；发现失败不默认起 ephemeral | 共用令牌；外壳退出杀服务；恢复任意 backendProxy；恢复自动 ephemeral | 2f |
| T17 ProductHost | 服务不铸 ProductHost；只有父管道交付；有限 TTL 到期拒绝；Creative mint 新外壳路径逐次要求证明 | 服务铸宿主；TTL 回 u64::MAX；FirstPartyShell 冒充宿主；mint 无证明 | 2b |
| T18 发布阻断 | DEP-B 产物签名验证、identifier 与 helper gui 验收失败时不能标投产 | 缺签名证据仍标通过；未签名包走自动身份成功路径 | 2c、2d，DEP-B |

每个实现 PR 按 AGENTS.md 跑 Rust fmt、clippy 与 workspace tests，使用共享 target，测试前 `ulimit -n 4096`。改 apps 或 packages 再跑 pnpm 门禁，改 HTTP 契约再跑 OpenAPI 门禁。本文只列测试设计，不制造测试结果。G-API 的资源隔离、事件逐次复验、Host 与 Origin、令牌配额等负向测试仍需独立交付。

## 6. 实现分片（外审接受后再派）

| 片 | 依赖 | 预计改动面与完成边界 |
|---|---|---|
| 2a 路由映射与等级表 | 无 | `cap/operations.rs`、`d/server.rs`、路由覆盖 CI 与测试。全量方法映射与等级元数据，不切 legacy，不宣称资源授权完成 |
| 2e 服务模式 | 2a | `d/main.rs`、serve 启动校验与生命周期、`agent24-protocol/state_file`、端点配置与启动测试。允许 enroll 服务启动，无父 EOF 任务；2c 接入前端点不发权，不作产品交付 |
| 2c 登记与身份 | 2e；macOS 投产依赖 DEP-B | 新登记 listener、独立进程路径 crate、macOS SecCode 与 Linux 清单验证、FD 与配对码、生命周期测试。先完成身份结果，真实令牌由 2b 接入 |
| 2b 一方令牌与两级权限 | 2c；Q4、Q6 先裁决 | claims、Audience、mint、validate、renew、进程存活、提升授权存储与原子消费、ProductHost TTL、Creative mint 授权入口。无证明时提升操作拒绝 |
| 2d 在场证明，先 macOS | 2b；DEP-B；Q1、Q6 外审 | daemon 提供者接口、Swift helper 构建与签名、gui 验收、桌面主进程原生确认通道。headless 仅接口，不实现设备协议 |
| 2f 外壳迁移 | 2d | CLI、TUI、桌面发现与登记、续期、逐次提升、LaunchAgent 参数、backendProxy 收窄、端到端测试。服务安装需征得用户同意，保留显式子进程退路 |

表按执行顺序排列，编号沿用任务编号。2e、2c 的中间状态只供可选模式验证，不能宣传为完整服务。每片内部若超过 300 行则拆 PR，仍保留相同依赖与授权门，不能借拆分跳过证明。G-API 全部门禁与 2f 验收全部完成后，默认模式切换仍另行决定。

## 7. 不在本稿

- 切换默认 auth 模式、创建 release PR、批准实现或上线。
- CORE-1 的停靠态、迁移恢复与版本水位；全面补齐 TUI 功能。
- G-API 的完整资源提取、REST Host 与 Origin、OpenAPI 覆盖、事件过滤和令牌治理实现。
- 开发者令牌治理 API 的具体 wire 格式，远程遥控协议及移动设备配对协议。
- Windows 实现、Linux polkit、FIDO2、手机在场证明；它们由 PLT 与遥控线设计。
- OS 沙箱隔离、防恶意同 UID 程序、按 CLI 调用来源自动降级。
- DEP-B 签名流水线本身。本文只把它列为前置，不代做、不宣布已完成。

## 8. 疑问（交协调者与外审裁决）

| # | 冲突或未闭合处 | 本稿保留的决定与待裁决点 |
|---|---|---|
| Q1 | D3 假设模块工具有 consent，但 facts §6 只证实模块 HTTP 路由挂载，不能推出任意路由都有 consent | 保留模块路由日常级。外审逐项裁定副作用路由是否需要更细映射；接受前不声称完整权限隔离 |
| Q2 | D3 说 comm 的 inbox 只读，但实际只有 POST `/inbox/pull`；outbox clear 也未在已定清单点名。facts §6 把 relay 写方法概写成 POST，代码为 PUT | 表按实际方法列。pull 与 clear 暂列 CommWrite 提升，重试属于出站提升。协调者须确认这两个未点名操作，不私称只读 |
| Q3 | D5 的匿名 FD 是交付格式；facts §2.4 明说 daemon 侧没有登记权签发协议。服务首次启动未必已有可信登记外壳 | 保留 FD 退路。2c 前定可信启动者取得并绑定目标连接挑战的协议。无可信启动者时只用连接绑定配对码，不新增共享磁盘秘密 |
| Q4 | D6 要 ProductHost 有 TTL，但 facts §1.4 的代码无续期 API，现有 ready 管道只交付一次；硬到期会影响长期子进程模式 | 有限 TTL 必须落实。本稿建议 1 小时，但具体期限与父进程续期通道交 2b 前裁决，不保留 u64::MAX |
| Q5 | ADR-037 服务无 bootstrap，但没定义 enroll 与 bootstrap 同开、legacy 带 enroll 的参数组合 | 本稿建议 D8 矩阵拒绝歧义组合。子进程 capability 自动开登记端点，不能要求同时开启服务模式开关 |
| Q6 | D4 每次提升操作都证明；ADR-037 同时允许开发者令牌含高权作用域，写的是签发时单独确认，未说每次使用如何证明 | D4 对一方外壳逐次执行固定不变。开发者高权调用是否也需独立逐次证明，或是另一种明确委托语义，须在高权开发者令牌实现前裁决，不能借它绕开 D4 |
| Q7 | 本分支 ADR-037 仍标「提议」；决策5声称签名公证流程已跑通，facts §4 未发现可重复流水线，也未确认内嵌 daemon 签名 | 按任务给定 #1249 已接受使用 ADR，保留状态与签名事实差异。DEP-B 必须补流水线及白名单实际 identifier 证据；协调者决定何处订正 ADR 状态和原文 |
| Q8 | `(pid,start_time)` 不能识别同进程 exec；#1448 评审 M1 确认 fork + exec 可稳定冒充一方客户端，且 `LOCAL_PEERTOKEN` 可直接取得 audit token | 改为 audit token 绑定，SecCode 使用 `kSecGuestAttributeAudit`；exec 改变 pidversion 即失效。§9 Q8 的接受结论由 §10 R6 更正。HTTP bearer 仍不证明发送者 PID，不声称防令牌转交 |

## 9. 协调者裁决（2026-10-26，Opus）

| # | 裁决 |
|---|---|
| Q1 | 模块路由维持日常级。理由：启用模块（`PATCH /os/{name}`、`POST /attached`）是提升级操作，用户在启用时已证明在场。这一步视为把该模块已注册的 HTTP 面委托出去，委托的边界就是模块注册的路由集合。模块新增路由要随模块版本变更重新启用。2a 测试未启用的模块命名空间返回 404，不进兜底。此条不声称模块路由内部的副作用受工具 consent 约束。 |
| Q2 | `POST /comm/inbox/pull` 归日常级，新增 Operation `CommSync`。它只把远端已到达的消息拉到本地，不对外发送，也不删除本地数据。`POST /comm/outbox/clear` 归提升级 `CommWrite`，因为它会丢弃待发消息，不可恢复。 |
| Q3 | 可信启动者限定为：已通过平台身份校验（D5 第 3 步）并持有一方令牌的外壳，例如通过 SecCode 校验的桌面主进程。它可以在已登记的通道上为自己启动的子进程申请一枚登记权，再经 FD 交付。登记权绑定目标连接挑战，只能换取日常级，不能链式签发。这在日常级内完成，因为不超出申请者自身的基础等级。没有任何已登记外壳时（首次启动、纯 headless），只走配对码。不引入共享秘密文件。 |
| Q4 | ProductHost 的 TTL 定为 1 小时。续期通道复用现有 `--host-bootstrap-stdio` 父管道：父进程写入续期帧，daemon 在同一管道回送新令牌并原子撤销旧令牌。帧格式在 2b 定义。父管道 EOF 仍然关机。不保留 `u64::MAX`。 |
| Q5 | 采纳 D8 矩阵：capabilities 模式下 bootstrap 与 enroll 同时开启时拒绝启动，legacy 模式下带 enroll 时拒绝启动。capability 子进程模式自动开登记端点，不需要另外的开关。 |
| Q6 | 开发者高权令牌按「签发时证明、使用时委托」处理：签发和扩权走 D4 的一次性提升授权；使用时不逐次证明。补偿措施有三条：高权作用域的令牌 TTL 上限 24 小时，不能无限续期；可以列出、可以吊销；每次高权调用都写入审计。一方外壳的 D4 逐次证明保持不变，开发者令牌不能用来替一方外壳绕过 D4。 |
| Q7 | ADR-037 的状态行和决策 5 的签名表述由 DOCS-2 单独订正（PR 进 main）。本稿保留差异说明。DEP-B 仍是 macOS 身份校验投产的前置。 |
| Q8 | 原接受结论已由 §10 R6（#1448 评审 M1）更正：audit token 可得，必须绑定并复核 pidversion。bearer 转交与 OS 隔离仍不在本片范围。 |

本稿评审至多一轮，之后冻结。评审意见中的真问题在本文订正，其余登记到实现片。

## 10. 外审修订（2026-10-26，Mac mini Codex + Grok 各一轮；本节优先于 §9，修订后冻结）

| # | 来源 | 问题 | 修订 |
|---|---|---|---|
| R1 | Codex High、Grok High | §9 Q1 的前提是「启用模块是提升级操作」，但 `os_config.rs` 对缺失或空配置默认全部启用，记录里也只有 enabled。首次启动或旧配置升级后，日常令牌可以直接调用未经委托的模块副作用路由 | Q1 改判。capability 模式下新增**模块委托记录**：绑定模块名、模块版本和已注册路由集的摘要，只能经提升级操作（D4）建立。GET/HEAD 在委托存在时归日常级。其余方法在没有委托记录、或路由集摘要与当前注册不一致时一律拒绝，返回可识别的 `module_delegation_required`。legacy 模式不变。首次启动和旧配置升级都**不自动生成**委托记录。新增 T19：首次启动、升级、路由集变化、版本变化四个负例，以及委托后通过的正例。实现排进 2a（映射）和 2b（委托记录的存储与 D4 建立），必须在 2f 向外壳发放日常令牌之前交付 |
| R2 | Grok High、Codex Low | §9 Q2 的前提是「拉取不对外、不删数据」，但 `inbox/pull` 会用钥匙串口令、按调用方指定的 `as` 解密并写入 history，用的是和 unlock、send 同一份凭据 | Q2 改判。对外的 `POST /comm/inbox/pull` 维持提升级 `CommWrite`，§3 已同步。`CommSync` 只给 daemon 内部轮询用，不挂 HTTP 路由，忽略 `as`，只拉默认身份。T2 以 §3 修订后的表为准 |
| R3 | Grok Medium | Q6 没有禁止开发者令牌带签发权限，24 小时上限可以被无证明续签绕过 | 开发者令牌的作用域**不得包含** `CapabilityMint`、`CapabilityRevoke` 或任何令牌治理 Operation。签发端点只接受带当次 D4 授权的 `FirstPartyShell`。补负例：开发者令牌调用签发返回拒绝；带签发作用域的签发请求被拒绝 |
| R4 | Grok Medium | Q3 的登记权没有核对目标确实是申请者的子进程；续期只检查进程还活着，转交出去的 bearer 也能拿来续期，并把原令牌顶掉 | 登记权兑换时，daemon 用 `peer_cred` 取目标 pid，核对它的父进程 pid 就是申请者绑定的 pid，并且两边的启动时间一致，不一致就拒绝；登记权换到的令牌只有日常级。续期只能在原 UDS 登记连接上进行，并核对 `peer_cred` 的 pid 与令牌绑定一致；HTTP 上不提供续期。T10、T12 各补一个负例 |
| R5 | Grok Medium | 分片中间态会放大权限：2a 先把 Creative 白名单里的 Session、Run、Cancel、Events 接到新路由，而收紧要到 2b；2e 先开服务模式，但永不过期的 ProductHost 要到 2b 才禁止 | 2a 新接的路由对 Creative **一律不放行**，白名单维持现状，扩充留到 G-API 设计。2e 的服务模式在启动时就不铸 ProductHost；`--enroll-endpoint` 之下，ready 行和 daemon.json 都不含任何秘密。T17 的「服务不铸宿主」断言提前到 2e |
| R6 | #1448 PR-Daemon 评审 M1（2026-10-28） | fork 后由子进程保留连接、父进程 exec 真签名程序，可稳定绕过按 PID 的校验；「无法控制的竞态」「拿不到 audit token」两个前提均不成立 | **（「accept 时取令牌即可」的说法已被 R7 更正：令牌按读取时刻求值。）** accept 后通过 `getsockopt(fd, SOL_LOCAL, LOCAL_PEERTOKEN)` 捕获 `audit_token_t`，SecCode 使用 `kSecGuestAttributeAudit` 并要求签名 flags 含 runtime。连接与令牌保存同一 audit token（含 pidversion）。首次校验、recheck、renew、elevate 及 HTTP 每次请求的存活检查都复核，exec 或进程退出（含僵尸）即拒绝并删除令牌。补 fork + exec 登记拒绝和已登记进程 exec 后 HTTP 拒绝的负向测试。此条覆盖 §4、§8 Q8 与 §9 Q8 的旧结论；不改变 bearer 可转交的边界 |
| R7 | #1448 复审 M1′/M2/M3（2026-10-28，含 R4 独立复现） | R6 的前提「accept 时取一次令牌即可封住」不成立：`LOCAL_PEERTOKEN` 在 `getsockopt` 调用时求值，不在 connect 时固定。攻击序列：同 UID 的未签名进程 A connect，fork 出持有者 B（继承 fd），A exec 成已签名的 Agent24 程序，此时 daemon 才 accept 并读令牌，读到的是 A exec 之后的签名身份，校验通过；B 随后在继承的 socket 上写 `hello`，令牌与 `LOCAL_PEERPID` 都变成 B 的，但此后没有任何地方重读令牌，B 取得 bearer。窗口可构造：同 UID 攻击者可 SIGSTOP daemon、connect、fork、exec 再 SIGCONT（SIGSTOP 对真实 agent24d 未实测，不改变结论） | **逐帧复核**：enroll 连接上每收到一帧（hello、renew、elevate、未知帧乃至无法解析的帧）都重读 `LOCAL_PEERTOKEN`，必须与 accept 时的 audit token 完全一致（含 pidversion），不一致立即关闭连接、不签发，已签发的令牌立即吊销。`enrolled`（含续期）与 `elevated` 帧写出前再读一次比对。持有者一旦写帧，令牌就变成它的身份，因此被拒绝。测试：`forked_child_cannot_enroll_after_connecting_parent_execs`（accept 后 exec）、`forked_child_cannot_enroll_when_parent_execs_before_accept`（connect → fork → exec → 之后 accept，M2 缺的顺序）、`inherited_holder_frames_revoke_even_while_original_peer_is_live`（renew/elevate/未知/畸形帧，原进程仍存活，吊销一次）、`verify_peer_rejects_exec_even_when_start_time_is_unchanged`；变异（去掉逐帧复核、去掉 `audit_token_is_live`、去掉 recheck 的启动时间比对）各使至少一个测试失败（M3）。**剩余风险**：(1) 原进程 exec 成签名程序后，若由它本身继续使用该连接，属于该签名程序自身的行为，不在本机制防护范围；(2) 同 UID 可 SIGSTOP daemon，只改变时序，不改变上述结论，因为任何帧都会重读令牌；(3) bearer 转交与 OS 隔离仍不在本片范围；(4) 若要彻底免除对 socket 令牌的依赖，需 XPC 或带签名 ACL 的 Keychain 绑定，留待后续 |

R7 更正 R6 与 §4 中「accept 时取令牌」的表述。修订后 §9 的 Q1、Q2、Q8 以本节为准，Q3、Q6 按本节收紧。本稿到此冻结，后续意见登记到实现片。
