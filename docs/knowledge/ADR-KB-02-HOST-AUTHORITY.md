# ADR-KB-02：知识库宿主的凭据交接与授权归属

索引：[架构总入口](../architecture/README.md)・[ADR 索引](../adr/README.md)

- 状态：**已接受**（2026-11-12，S0-8。记录已经落地的 KB-HOST-01，不新开接口）。
- 编号：知识库独立编号，接在 [ADR-KB-01](ADR-KB-01-SINGLE-CONVERSATION.md) 之后。数字 ADR 的发号仍只在 [ADR 索引](../adr/README.md)。
- 上位：[架构优化方案](../tracking/2026-11-04/ARCH-OPTIMIZATION-PLAN.md) §4.1 S0-8、§5 Q16；ADR-KB-01a（单一主对话、只读检索）；ADR-K1-02 §2.1.1 第 6 条（Knowledge 同源回送）；ADR-K1-03（启用时逐工具同意）。
- 来源：KB-HOST-01 的设计正文原在 issue #1085，仓内只有任务行和契约里的交叉引用。本篇以当前代码为准。

## 1. 背景

知识库要有一处说清三件事：长期登录态放在哪、短期凭据 C 交给谁、谁有权激活绑定。契约把这三件留给 KB-HOST-01（`CONTRACT-UI-BRIDGE-v1.md` 文首对 #8、#9 的登记；`CONTRACT-READONLY-v1.md` 的 R1.4、R1.7）。实现已经分两处落地：桌面主进程持有长期登录态，daemon 的内存登记表持有 C。S2-3（#1792）把登记表和调用层放进 `agent24-knowledge`，路由仍留在 agent24d。

## 2. 决策

### D1 宿主登记表在 `agent24-knowledge`，路由留在 agent24d

`KnowledgeHostRegistry` 在 `rust/crates/agent24-knowledge/src/host/mod.rs:182`。agent24d 启动时构造一份，和 Knowledge 工具共用（`rust/apps/agent24d/src/serve_steps.rs:602-609`）。四条私有路由仍在 `rust/apps/agent24d/src/knowledge_routes.rs`，挂在 `server.rs:1007-1014`：

| 方法 | 路径 | 作用 |
|---|---|---|
| PUT | `/api/v1/host/knowledge/bindings/{binding_id}?seq=n` | 激活；正文可带端点 |
| DELETE | 同上 | 停用，并丢掉 C |
| PUT | `.../credential?seq=n` | 推入或替换 C |
| DELETE | `.../credential?seq=n` | 只丢掉 C |

序号从 1 起，必须大于该绑定上一次接受的序号；迟到的变更不能把旧 C 请回来（`host/mod.rs:11-16`、`422-423`）。

### D2 长期登录态只在桌面主进程，整文件加密落盘

Electron main 用 `KnowledgeLoginStore` 把每个绑定的登录态写进用户数据目录的 `knowledge-logins.bin`（`apps/desktop/src/main/knowledge-logins.ts:1-4`、`:38`）。整文件用 `safeStorage` 加密；解密或解析失败则全部登录态作废，用户重新登录（同文件 `:83-112`）。Linux 上 `basic_text` 和未知后端不算可用加密（`:41-49`）。

一条登录态是 `profileId` 加上 `material`（刷新令牌或设备授权，用来在用户不在场时换新的 C），外加 `unusable`、`temporary`（`:15-23`）。明文不进日志、不进任何 renderer IPC（`:5`）。收紧（标不可用、删除）立刻在内存生效；新增或替换要等写盘成功（`:6-9`、`:120-156`）。

`main.ts:515-522` 在 app ready 之后、其它 Knowledge 变更之前加载一次。

### D3 短期凭据 C 只活在 daemon 进程内存

C 是 `Bearer` 或 `ApiKey`，`Debug` 打成 `<redacted>`（`host/mod.rs:68-82`）。登记表以（ProductHost 的 capability id，绑定 id）为键（`:7-9`、`:180`）。没有响应体、没有日志行带出 C（`knowledge_routes.rs:2-5`；解析失败也只回 `invalid_request`，`:106-113`）。上限 16 KiB（`host/mod.rs:63`）。

出站时才放进头：Bearer 走 `Authorization`，ApiKey 走 `x-api-key`，头值标成 sensitive（`rust/crates/agent24-knowledge/src/http.rs:268-283`）。调用层只在 run 的 pin 与当前代际一致、且 C 未过期时组传输（`backend/mod.rs:189-237`）。代际或 C 的序号变了，在途调用作废（`backend/mod.rs:334-350`）。

`provider/` 与这张登记表并行，**冻结、无生产调用**，接线另立项（`rust/crates/agent24-knowledge/src/provider/mod.rs:1`）。本篇不把它当成凭据的第二存放处。

### D4 凭据交接：谁持有、谁传给谁、何时清除

| 材料 | 谁持有 | 传给谁 | 何时清除 |
|---|---|---|---|
| 长期 `material` | 桌面 main 的内存，以及加密文件 `knowledge-logins.bin` | 不交给 daemon，不交给 renderer。main 用它换 C（`knowledge-host.ts:190-202`） | `destroy`：先标 `unusable`（内存立即生效），再从 map 删除并重写加密文件（`knowledge-logins.ts:145-156`；`knowledge-host.ts:206-226`）。加密不可用或文件读不懂：内存里的登录态清空，视为全部丢失（`knowledge-logins.ts:84-86`、`:110-111`） |
| 短期 C | 换到之后，main 的 `wanted` 里留一份，供重连重放（`knowledge-host.ts:89-90`、`:248-252`）；daemon 登记表留正在生效的那一份 | main 以 ProductHost 身份 `PUT .../credential` 交给 daemon（`knowledge-host.ts:178-179`、`:306-308`） | 激活会先丢掉 C，再由 main 重新推（`host/mod.rs:157-158`、`:332`；`knowledge-host.ts:150-151`）。停用、撤销、`destroy` 的 DELETE 绑定都丢掉 C（`host/mod.rs:306-311`；`knowledge_routes.rs:271-273`）。过期的 C 在读取时丢掉（`host/mod.rs:381-398`）。daemon 重启后登记表是空的，旧 pin 对不上，不会凭空再绑（`host/mod.rs:26-31`）；main 发现连接变了就按 `wanted` 重放未过期的 C（`knowledge-host.ts:120-130`、`:262-279`） |

序号用墙钟毫秒，且只增不减，所以 main 重启后也不会用更小的序号覆盖 daemon（`knowledge-host.ts:6-7`、`:67-76`）。daemon 认为序号过期时，main 换新序号再送，最多三次（`:291-313`）。

一个 run 只能由激活它的那个 ProductHost 指名绑定，并钉在当时的代际上（`knowledge_routes.rs:180-196`；`host/mod.rs:121-127`、`:446`）。同一会话不能换到另一次激活（`knowledge_routes.rs:212-217`；`host/mod.rs:185-188`）。

### D5 授权归属：只有桌面 ProductHost

`host_capability` 只在 capabilities 模式、且受众是 `ProductHost` 时返回 capability id；legacy 共享令牌和 Creative 都是 `None`（`knowledge_routes.rs:48-62`）。四条路由都先过这道检查，否则 `403 host_authority_required`（`:163-169`、`:136-140`）。因此 CLI、TUI 和其它一方外壳调不通知识库绑定与凭据交接。这是桌面专属，与 `AGENTS.md` 里「Knowledge 绑定与凭据只许 ProductHost」一致。

路由目录把这四条标成 `Operation::KnowledgeHost`、`AssuranceLevel::Elevated`（`rust/apps/agent24d/src/capabilities/route_policy.rs:129-132`）。`capabilities/middleware.rs` 的 `host_only_route`（`:52-60`）管的是通信私钥导入导出、组件报告和入站名单，**不包含**这四条；知识库的宿主门在 `host_capability`，不在那张表里。

查找必须带宿主 capability id，不能只凭绑定 id（`host/mod.rs:7-9`）。没有绑定的 run 不广告这两个工具（见 D6）。

### D6 Q16：Knowledge 工具以 builtin 身份注册的例外

架构优化方案 C7 指出：按 [ADR-039](../adr/ADR-039-concept-model.md)，知识库检索是「知识库」能力的工具（ADR-039 §2，`:38`；§5 对照表把「知识库检索」归成该能力的工具，不单列），却以本体的 builtin 身份注册。方案 §6 写的是归类后果，没有单独的 Q16 小节；Q16 在方案 §5，拍板 **(b)**：不改成组件工具、不走 ADR-K1-03 的逐工具同意，例外写进本篇。

现行注册就是这个例外，保持不变：

- `knowledge_search` 与 `knowledge_read` 的 `ToolInfo.source` 是 `"builtin"`，风险类是 `Read`（`rust/apps/agent24d/src/knowledge_tools.rs:64-88`）。
- 两者 `needs_knowledge_binding` 为真（`:97-99`）。没有指名绑定的 run，广告列表里没有它们（`rust/crates/agent24-tools/src/lib.rs:1972-1982`、`:1995-2004`）。
- 它们只向该 run 所钉的绑定端点发只读请求；早先由这两个工具带回的结果不算新的出站来源（`knowledge_tools.rs:112-124`）。

不走逐工具同意的理由与 Q16 推荐相同：工具只读，而且只出现在已经指名绑定的 run 里；再加一次确认，对 ADR-KB-01a 的单一主对话代价过大。这条例外不把知识库收进本体，也不新造顶层名词。以后若要改成组件工具，另立 ADR。

## 3. 不在本篇

- ADR-KB-01b 的管理页、深度子任务、写操作、ContextPack、SOP。
- `provider/` 与 daemon 登记表的接线。
- 远端吊销登录态。`destroy` 注释写明还没有那份服务端契约（`knowledge-host.ts:206-211`）。

## 4. 证据

| 决策 | 证据 |
|---|---|
| D1 登记表下沉、路由留下 | `agent24-knowledge/src/host/mod.rs:182`；`agent24d/src/serve_steps.rs:605-609`；`knowledge_routes.rs:222-224`、`:306-307`；`server.rs:1007-1014` |
| D2 加密落盘、不进 renderer | `knowledge-logins.ts:1-5`、`:38`、`:41-49`、`:84-112`；`main.ts:515-518` |
| D3 C 只在内存、响应与日志不带出 | `host/mod.rs:68-82`、`:180`；`knowledge_routes.rs:2-5`、`:106-113`；`http.rs:268-283`；`provider/mod.rs:1` |
| D4 交接与清除 | `knowledge-host.ts:89-90`、`:150-151`、`:178-202`、`:206-226`、`:262-279`；`host/mod.rs:26-31`、`:157-158`、`:306-311`、`:332`、`:381-398`；`knowledge-logins.ts:145-156` |
| D5 只许 ProductHost | `knowledge_routes.rs:48-62`、`:136-140`、`:163-169`；`route_policy.rs:129-132`；`capabilities/middleware.rs:52-60` |
| D6 builtin 例外 | `knowledge_tools.rs:88`、`:97-99`；`agent24-tools/src/lib.rs:1972-1982`、`:1995-2004`；方案 §5 Q16 |
