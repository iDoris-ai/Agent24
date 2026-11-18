# ADR-KB-02：知识库宿主的凭据交接与授权归属

索引：[架构总入口](../architecture/README.md)・[ADR 索引](../adr/README.md)

- 状态：**已接受**（2026-11-17 按现行代码订正。初稿 2026-11-12，S0-8，[#1823](https://github.com/iDoris-ai/Agent24/pull/1823)。记录已经落地的 KB-HOST-01，不新开接口）。
- 编号：知识库独立编号，接在 [ADR-KB-01](ADR-KB-01-SINGLE-CONVERSATION.md) 之后。数字 ADR 的发号仍只在 [ADR 索引](../adr/README.md)。
- 上位：[架构优化方案](../tracking/2026-11-04/ARCH-OPTIMIZATION-PLAN.md) §4.1 S0-8、§5 Q16；ADR-KB-01a（单一主对话、只读检索）；ADR-K1-02 §2.1.1 第 6 条（Knowledge 同源回送）；ADR-K1-03（启用时逐工具同意）。
- 来源：设计正文原挂在 issue #1085。2026-11-17 `gh issue view 1085` 在 `iDoris-ai/Agent24` 上解析失败，记为未核实；仓内任务行和契约仍用这个号。本篇以当前代码为准。登记表下沉见 [#1792](https://github.com/iDoris-ai/Agent24/pull/1792)（`a8a749bd`）。服务模式宿主、序号进正文、在场确认随 [#1852](https://github.com/iDoris-ai/Agent24/pull/1852) 合入（`8aa0bee83`、`6e2175f91`）。
- 本篇不取代 [ADR-035](../decision.md) 的任何一节。Admin token 的交接在 [ADR-046](../adr/ADR-046-idoris-managed-host.md)。

## 1. 背景

知识库要有一处说清三件事：长期登录态放在哪、短期凭据 C 交给谁、谁有权激活绑定。契约把这三件留给 KB-HOST-01（`CONTRACT-UI-BRIDGE-v1.md` 文首对 #8、#9 的登记；`CONTRACT-READONLY-v1.md` 的 R1.4、R1.7）。实现分两处：桌面主进程持有长期登录态，daemon 的内存登记表持有 C。S2-3（#1792）把登记表和调用层放进 `agent24-knowledge`，路由仍留在 agent24d。

## 2. 决策

### D1 宿主登记表在 `agent24-knowledge`，路由留在 agent24d

`KnowledgeHostRegistry` 在 `rust/crates/agent24-knowledge/src/host/mod.rs:187`。agent24d 启动时构造一份，和 Knowledge 工具共用（`rust/apps/agent24d/src/serve_steps.rs:605-609`）。四条路由在 `rust/apps/agent24d/src/knowledge_routes.rs`，挂在 `server.rs:277-284`。

序号在 JSON 正文里，从 1 起，必须大于该绑定上一次接受的序号。查询串直接拒绝（`knowledge_routes.rs:16-20`、`:218`、`:267-270`）。迟到的变更不能把旧 C 请回来。

| 方法 | 路径 | 正文 | 作用 |
|---|---|---|---|
| PUT | `/api/v1/host/knowledge/bindings/{binding_id}` | `{"seq"}`，或再加 `endpoint` 与 `knowledge_base_ids` | 激活。没有端点时，这个绑定上的调用都会被拒 |
| DELETE | 同上 | `{"seq"}` | 停用，并丢掉 C |
| PUT | `.../credential` | `{"seq","kind","value","expires_at"}` | 推入或替换 C。`kind` 是 `bearer` 或 `api_key` |
| DELETE | `.../credential` | `{"seq"}` | 只丢掉 C |

端点必须是 `validate_base_url` 接受的规范串（https，或指向回环 IP 的 http），知识库 id 1 到 64 个、互不相同（`knowledge_routes.rs:315-333`；`05db4d3d0`）。激活会清掉当时的 C，桌面之后再推（`host/mod.rs:305-309`、`:337`）。

### D2 长期登录态只在桌面主进程，整文件加密落盘

Electron main 用 `KnowledgeLoginStore` 把每个绑定的登录态写进用户数据目录的 `knowledge-logins.bin`（`apps/desktop/src/main/knowledge-logins.ts:1-4`、`:38`）。整文件用 `safeStorage` 加密。解密或解析失败则全部登录态作废，用户重新登录（`:83-112`）。Linux 上 `basic_text` 和未知后端不算可用加密（`:41-49`）。

一条登录态是 `profileId` 加上 `material`（刷新令牌或设备授权，用来在用户不在场时换新的 C），外加 `unusable`、`temporary`（`:15-23`）。明文不进日志、不进任何 renderer IPC（`:5`）。收紧（标不可用、删除）立刻在内存生效；新增或替换要等写盘成功（`:6-9`、`:125-156`）。

`main.ts:559-568` 在 app ready 之后、其它 Knowledge 变更之前加载一次。

### D3 短期凭据 C 只活在 daemon 进程内存

C 是 `Bearer` 或 `ApiKey`，`Debug` 打成 `<redacted>`（`host/mod.rs:73-87`）。登记表以（宿主键，绑定 id）为键（`:7-9`、`:185-188`）。没有响应体、没有日志行带出 C（`knowledge_routes.rs:8-9`；解析失败也只回 `invalid_request`，`:157-164`）。上限 16 KiB，且只能是可见 ASCII（`host/mod.rs:68`；`knowledge_routes.rs:449-452`）。

出站时才放进头：Bearer 走 `Authorization`，ApiKey 走 `x-api-key`，头值标成 sensitive（`rust/crates/agent24-knowledge/src/http.rs:268-283`）。调用层只在 run 的 pin 与当前代际一致、且 C 未过期时组传输（`backend/mod.rs:176-203`）。代际变了，在途调用作废（`backend/mod.rs:325-350`）。`credential` 读取时丢掉过期的 C（`host/mod.rs:397-402`）。`binding_now` 只是不把过期的交出去，不在那里删存储（`:350-353`）。

`provider/` 与这张登记表并行，冻结、无生产调用，接线另立项（`rust/crates/agent24-knowledge/src/provider/mod.rs:1`）。本篇不把它当成凭据的第二存放处。

### D4 凭据交接：谁持有、谁传给谁、何时清除

| 材料 | 谁持有 | 传给谁 | 何时清除 |
|---|---|---|---|
| 长期 `material` | 桌面 main 的内存，以及加密文件 `knowledge-logins.bin` | 不交给 daemon，不交给 renderer。main 用它换 C（`knowledge-host.ts:263-275`） | `destroy`：先标 `unusable`（内存立即生效），再从 map 删除并重写加密文件（`knowledge-logins.ts:145-156`；`knowledge-host.ts:279-299`）。加密不可用或文件读不懂：内存里的登录态清空，视为全部丢失（`knowledge-logins.ts:84-86`、`:110-111`） |
| 短期 C | 换到之后，main 的 `wanted` 里留一份，供重连重放（`knowledge-host.ts:128-129`、`:321-325`）；daemon 登记表留正在生效的那一份 | main 以 D5 的宿主身份 `PUT .../credential` 交给 daemon（`knowledge-host.ts:321-326`、`:408-418`） | 激活会先丢掉 C，再由 main 重新推（`host/mod.rs:305-309`、`:337`；`knowledge-host.ts:213-214`）。停用、撤销、`destroy` 的 DELETE 绑定都丢掉 C（`host/mod.rs:311-316`；`knowledge_routes.rs:467-492`）。过期见 D3。daemon 重启后登记表是空的，旧 pin 对不上，不会凭空再绑（`knowledge_routes.rs:31-36`；`host/mod.rs:495-511`）；main 发现连接变了就按 `wanted` 重放未过期的 C（`knowledge-host.ts:167-180`、`:354-385`） |

序号用墙钟毫秒，且在本进程内只增不减，所以 main 重启后也不会用更小的序号覆盖 daemon（`knowledge-host.ts:6-7`、`:100-109`）。daemon 认为序号过期时，main 换新序号再送，最多三次（`:36`、`:406-429`）。

一个 run 只能由激活它的那个宿主指名绑定，并钉在当时的代际上（`knowledge_routes.rs:225-246`；`host/mod.rs:451-452`、`:495-511`）。同一会话不能换到另一次激活（`host/mod.rs:466-488`；`knowledge_routes.rs:247-261`）。

服务模式下，每一次绑定变更（含 daemon 换代后的重放）都要用户在场确认一次，经 `sendElevating`（`knowledge-host.ts:15-26`、`:458-481`）。用户拒绝得到 `refused`，自己不再重试，换一个 daemon 也不再问，直到用户改设置或应用重启（`:336-342`）。超时未答是 `unanswered`，10 分钟起、翻倍、最多 1 小时后再问（`:49-56`、`:343-347`）。子进程模式没有登记会话时，同一次调用就是普通请求，不再额外确认（`:458-460`）。

远端吊销登录态还没有服务端契约（`knowledge-host.ts:279-281`）。FU：远端吊销。

### D5 授权归属：桌面，两种宿主键

`host_capability` 只在 capabilities 模式成立。legacy 共享令牌和 Creative 都是 `None`（`knowledge_routes.rs:74-97`）。能通过的有两种，键不会撞车：

| 调用方 | 宿主键 | 何时 |
|---|---|---|
| `ProductHost` | `product:{capability_id}` | 子进程模式，桌面 main 持有宿主令牌 |
| 代码签名标识恰好是 `ai.auraai.agent24` 的 `FirstPartyShell` | `shell:ai.auraai.agent24` | 服务模式。这不是 ProductHost。续期换 capability id，宿主键不变（`knowledge_routes.rs:1-14`、`:54-70`、`:100-108`） |

其它受众，包括 CLI 的一方外壳，都不是宿主。四条路由过不了这道检查就是 `403 host_authority_required`（`:182-186`、`:216`）。服务模式下，中间件在弹出确认之前就用同一条桌面标识拦住 CLI（`capabilities/middleware.rs:566-578`）。因此 CLI、TUI 和其它一方外壳调不通知识库绑定与凭据交接。

这和 `AGENTS.md` 里「只许 ProductHost」差一句：服务模式下桌面不持有 ProductHost 令牌（`main.ts:547-548`），靠上面的 shell 键。桌面专属仍然成立。

路由目录把这四条标成 `Operation::KnowledgeHost`、`AssuranceLevel::Elevated`（`rust/apps/agent24d/src/capabilities/route_policy.rs:134-137`）。`capabilities/middleware.rs` 的 `host_only_route`（`:52-60`）管的是通信私钥导入导出、组件报告和入站名单，不包含这四条。知识库的宿主门在 `host_capability`。

查找必须带宿主键，不能只凭绑定 id（`host/mod.rs:11-14`）。没有绑定的 run 不广告这两个工具（见 D6）。

### D6 Q16：Knowledge 工具以 builtin 身份注册的例外

架构优化方案 C7 指出：按 [ADR-039](../adr/ADR-039-concept-model.md)，知识库检索是「知识库」能力的工具（ADR-039 §2.2，`:38`），却以本体的 builtin 身份注册。方案 §5 Q16 拍板 **(b)**：不改成组件工具、不走 ADR-K1-03 的逐工具同意，例外写进本篇。

现行注册就是这个例外，保持不变：

- `knowledge_search` 与 `knowledge_read` 的 `ToolInfo.source` 是 `"builtin"`，风险类是 `Read`（`rust/apps/agent24d/src/knowledge_tools.rs:88`）。
- 两者 `needs_knowledge_binding` 为真（`:97-99`）。没有指名绑定的 run，广告列表里没有它们（`rust/crates/agent24-tools/src/lib.rs:1993-2003`、`:2016-2025`）。
- 它们只向该 run 所钉的绑定端点发只读请求。早先由这两个工具带回的结果不算新的出站来源（`knowledge_tools.rs:112-124`）。

不走逐工具同意的理由与 Q16 相同：工具只读，而且只出现在已经指名绑定的 run 里。再加一次确认，对 ADR-KB-01a 的单一主对话代价过大。这条例外不把知识库收进本体，也不新造顶层名词。以后若要改成组件工具，另立 ADR。

## 3. 不在本篇

- ADR-KB-01b 的管理页、深度子任务、写操作、ContextPack、SOP。
- `provider/` 与 daemon 登记表的接线。
- 远端吊销登录态。
- [ADR-035](../decision.md) 的 Admin session token。

## 4. 证据

| 决策 | 证据 |
|---|---|
| D1 登记表下沉、序号在正文 | `agent24-knowledge/src/host/mod.rs:187`；`serve_steps.rs:605-609`；`knowledge_routes.rs:267-305`；`server.rs:277-284`；#1792、#1852 |
| D2 加密落盘、不进 renderer | `knowledge-logins.ts:1-5`、`:38`、`:41-49`、`:84-112`；`main.ts:559-568` |
| D3 C 只在内存、响应与日志不带出 | `host/mod.rs:68-87`、`:185-188`、`:397-402`；`knowledge_routes.rs:8-9`、`:157-164`；`http.rs:268-283`；`provider/mod.rs:1` |
| D4 交接、重放与在场确认 | `knowledge-host.ts:15-26`、`:100-109`、`:167-180`、`:263-299`、`:321-326`、`:406-429`、`:458-481`；`host/mod.rs:305-316`、`:337` |
| D5 两种宿主键 | `knowledge_routes.rs:54-108`、`:182-186`；`middleware.rs:52-60`、`:566-578`；`route_policy.rs:134-137`；`main.ts:547-548` |
| D6 builtin 例外 | `knowledge_tools.rs:88`、`:97-99`；`agent24-tools/src/lib.rs:1993-2025`；方案 §5 Q16 |
