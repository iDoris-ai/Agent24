# K1 发布说明（2026-10-16）

- 发布对象：`#1022` 的 squash 合并提交 `56264ef92bd08b7bdcebc7aa45a78e8b7065bd9a`（`git log origin/main --grep=1022` 只有这一条；`git show -s --format='%H%n%s' 56264ef9` 的标题以 `(#1022)` 结尾）。父提交是 `600baba098a1b2f4e6ac6d39fe8d0883b06ea8c6`（`Merge pull request #979`）。树是 `d8c6de8f11ae9470dd93143ee836d22c55dde39c`。
- PR head：`815c526cd72785973fc466c94c4c7d197266c344`（`gh pr view 1022 --json headRefOid,mergeCommit`）。它的树是 `4957d29db21280c7334213e3acadae1e41e99ab4`，与 squash **不是同一棵树**。`815c526` 不是 `56264ef9` 的祖先。批准该 head 时，评审写的 main 基线是 `729a715`；实际 squash 落在已经前进的 `600baba0` 上。下文行号一律指 `56264ef9`，不要用 `refs/pull/1022/head` 对行号。两棵树上内容不同、因而行号不能互换的文件有：`rust/crates/agent24-agent/src/lib.rs`、`rust/crates/agent24-agent/src/subagent.rs`、`rust/crates/agent24-models/src/router.rs`、`rust/crates/agent24-models/src/lib.rs`、`rust/crates/agent24-domain/src/lib.rs`、`rust/apps/agent24d/src/model_callback.rs`、`rust/apps/agent24d/src/routes.rs`。
- 写本文时的 `origin/main` 尖端是 `ebc9b6e74ffaa05316cd1fd2043dab8ecf925d65`（`#1026`）。`56264ef9` 与这个尖端之间还有 `#1024`、`#1025`、`#1027`、`#1023`。上一条列出的、在两棵树上行号不能互换的文件，以及 `os_routes.rs`、`agent24-cli/src/main.rs`、`server.rs`、`os_config.rs`、`state_file.rs`、`Chat.tsx`、`Chat.i18n.ts`、`install.rs`、迁移、PLAN、ADR 与 `APPROVAL.md`，在 `ebc9b6e7` 上的 blob 与 `56264ef9` 相同，所以行号在这个尖端上也成立。尖端之后的新提交不在本文范围内。
- 范围：给已经合入 main 的 K1 写发布说明。停用/卸载时 consent 撤销失败的行为，以 `56264ef9` 上的代码为准。`docs/agent/REVIEW-kernel-to-main-20261016.md` 不在 main 上，也不在 `56264ef9` 上（`git cat-file -e` 对这两处都失败）。它只在 `origin/baton/grokbot-20261016-a24-k1review`（文档提交 `e24b2c84`，分支尖端 `3008c5aa`）。该报告针对的是修复前的 `571190cb`，其 `:21`、`:60` 写的是「只记日志、返回成功、fail-open」。这个结论提出的 fail-open 已在 `#1022` 内修复，见下文「fail-closed」。不要再把那份报告当作现行行为的依据。
- 本文是可复核的事实，不是审批，也不代替 PR-Daemon。法律流程的补救路径已由 jason 2026-10-16 选定为路径 1，执行 PR 为 #1031（见第三部分）。

---

## 第一部分：发布说明（给用户和部署方看）

### 这次 K1 带来什么

K1 把「模块工具能不能被调用」和「这段对话能不能送出本机」收成宿主侧的两道门。用户能对上号的是下面四块，再加上生命周期撤销的 fail-closed。

**授权 / consent。** 库里新增每工具一条的启用同意记录，以及单独的撤销记录。

- `rust/crates/agent24-store/migrations/0015_module_consents.sql:23-43` 建 `module_consents`：主键是 `(module, op)`，记下版本、权限指纹、来源、风险、决定（`granted` / `denied`）和到期时间。
- `rust/crates/agent24-store/migrations/0017_module_consent_revocations.sql:1-9` 建 `module_consent_revocations`。`op='*'` 覆盖该模块全部工具；后写的同意若 `decided_at` 更新，可以盖过更早的撤销（同文件 `:1-3` 的注释）。判定在 `module_consents.rs:563-568`：`revoked_at >= decided_at` 则该条同意视为已撤销。
- 调用时由 `StoreConsentGate::authorize` 查这两张表。查不到、查失败、被拒、过期、撤销、版本/指纹对不上，都是否决（`rust/crates/agent24-policy/src/consent_gate.rs:300-355`）。
- 宿主 HTTP 只有列表、导出和撤销，没有授予：
  - `GET /api/v1/module-consents`（`rust/apps/agent24d/src/server.rs:1081`）
  - `GET /api/v1/module-consents/export`（同文件 `:1083`）
  - `DELETE /api/v1/module-consents/modules/{module}`（`:1087`）
  - `DELETE /api/v1/module-consents/modules/{module}/tools/{tool}`（`:1091`）
- 生产代码里没有写入 grant 的路由。`grant_module_consent` / `deny_module_consent` 的调用点都在 `#[cfg(test)]` 之下（`module_tool_runtime.rs` 的测试模块自 `:177`，`os_routes.rs` 自 `:1080`，`server.rs` 自 `:2930`，`consent_gate.rs` 自 `:379`，`module_consents.rs` 自 `:610`）。因此升级之后，模块工具在这道门上是未授权。daemon 给每个已注册模块工具装配的请求把可逆草稿例外关掉了（`server.rs:1601-1604`：`reversible_draft_declared: false`，`draft_verification: None`）。用户这次看不到新的「同意」对话框。

**模块工具调用与审计。** 模块工具在进运行时之前先过同意门，并写类型化审计：调用前 `PreDispatch`，结束时 `Terminal`（`rust/crates/agent24-tools/src/lib.rs:601`、`:810`、`:1001`）。审计挂在内核 store 上，不靠模块自己的日志。

**来源分类 / 敏感资料。** 每个 run 可以挂来源标签，表是 `run_source_tags`（`0016_run_source_tags.sql`）。模式只有 `LocalOnly` 和 `CloudAuthorized`（`rust/crates/agent24-store/src/source_policy.rs:76-79`）。这条线实际写进去的用户输入标签固定是 `LocalOnly`（`source_policy.rs:111-124`；agent loop 在 `rust/crates/agent24-agent/src/lib.rs:1547-1552` 写入）。没有标签、标签互相冲突、或读失败，有效模式都是 `LocalOnly`（`source_policy.rs:182-205`、`:307-313`；`lib.rs:482-494`）。在 `56264ef9` 上对 `*.rs` / `*.ts` / `*.tsx` 检索 `CloudAuthorized`：生产路径里没有把一条标签写成 `CloudAuthorized` 的赋值。`source_policy.rs:209` 是折叠函数在「全部标签都不是 LocalOnly」时的结果；`source_policy.rs:357` 与 `lib.rs:2453`、`:2463` 都在测试里。获准把资料送到云端的入口，这次没有做。

**出站判定。** 已经接到产品路径上的，是模型路由这一道：`Privacy::LocalOnly` 的任务只在 `Local` 和 `Lora` 两档里选，本地都不可用就报错，不会改去远端（`rust/crates/agent24-models/src/router.rs:85-97`、`:424-437`）。ADR-K1-02 把 HTTP、MCP、Exec、记忆外流也列进出站门（`docs/adr/ADR-K1-02-session-dataflow-security-modes.md:57-65`），并把它记成后续切片 6b.3（同文件 `:106`）。这次合入没有一条统一的出站判定函数去包 `http_fetch` 或 `shell_exec`：在 `rust/crates/agent24-tools/src/net.rs` 和 `local.rs` 里检索 `LocalOnly` / `SourceMode` 没有命中。模块模型回调仍看 manifest 的 `model_access`，缺省是 `LocalOnly`（`rust/crates/agent24-domain/src/lib.rs:830-834`），写了 `remote_allowed` 才映射成 `Privacy::Any`（`rust/apps/agent24d/src/model_callback.rs:526-528`）。那条回调路径没有改成「一律 LocalOnly」。

另外，只要路由器里存在任何一个 Remote 档，`documents` 模块的工具就不对外通告（`rust/apps/agent24d/src/module_tool_runtime.rs:172-174`；装配点 `server.rs:1615-1618` 把 `router.has_remote_tier()` 传进去）。`has_remote_tier` 相对 squash 父提交是新增的 7 行（`router.rs:203-208`；`git diff --stat 600baba0 56264ef9 -- rust/crates/agent24-models/src/router.rs` 为 `7 +`）。

**生命周期撤销改为 fail-closed。** 修复在 PR head 的历史上是 `1fb7ec2f560476893cdbfa93acf9228356095b16`（合并 `238a18aa38a9c0605f492e7db0e50e01e55a0668`）。squash 不保留这条父链，所以 `1fb7ec2f` 不是 `56264ef9` 的祖先。落地证据是 blob：`815c526` 与 `56264ef9` 的 `rust/apps/agent24d/src/os_routes.rs` 都是 `80d53087097cb1ab13d113bb8995bd80a63faf4c`，`rust/apps/agent24-cli/src/main.rs` 都是 `f53cdd712056a98287a2258595d4a533e3cf394d`。行为见下一节。

### 停用、停止、安装、卸载：撤销失败不再报成功

常驻 daemon 在同一个进程里的 `PATCH /api/v1/os/{name}` 上，disable 与随后的 enable 已按 fail-closed 接上。范围就这么窄：CLI、短命 daemon、stop 之后重启，见后面的 F1 / F3 / F6。

**停用（`enabled: false`）。** `apply` 先把 `os.json` 写成停用并交出去停止，然后才撤销（`os_routes.rs:524-547`）。撤销失败不把 `os.json` 改回启用。路由在停止结果不是 `HotStop::Failed` 时返回 **HTTP 500**，`error.code` 为 `consent_revocation_failed`（`os_routes.rs:552-559` 构造响应，`:984-992` 调用）。正文是「这次请求已经在 os.json 里停用了该模块，但撤销 consent 失败」，`hint` 原文是：

> the module stays disabled/stopped and re-enabling it will require fresh consent; retry once the daemon's state store is writable

运维会看到：模块已经停用，接口不是 2xx；库恢复可写后需要再发一次停用（撤销是幂等的）或走下面的启用兜底。若监督器停止本身是 `HotStop::Failed`，响应走停止失败那条，仍然不是 2xx，但正文不提撤销失败（`os_routes.rs:975-980` 先于 `:984` 返回）。这是 `#1022` 评审里的 F4，这里只说明用户会看到哪一个码。

**立刻停止（`POST /api/v1/os/{name}/stop`，不改 `os.json`）。** 撤销失败且停止结果不是 `HotStop::Failed` 时，同样是 500 `consent_revocation_failed`，不会返回 `{"stopped": true}`（`os_routes.rs:1042-1066`）。停止本身失败时，响应是停止失败，不附带撤销失败说明。

**启用（`enabled: true`，且 `os.json` 里当前不是启用）。** 先撤销旧 consent，失败则拒绝启用，`os.json` 一个字节都不改（`os_routes.rs:460-465`、`:481-496`）。HTTP 仍是 500，但 `error.code` 是 **`internal`**，不是 `consent_revocation_failed`（`os_routes.rs:946-949`）。正文包含 `could not revoke earlier module consent for {name} before enabling it, so it was left disabled and os.json was not changed`。客户端不能靠 `consent_revocation_failed` 这一个码把「启用被拒」和「停用已落地但撤销失败」分开。

**CLI。** `agent24 os install` 在文件已经安装之后撤销；`agent24 os uninstall` 在文件已经删除之后撤销。任一失败都变成 `Err`，`main` 把它打成 `error: …` 并以 `ExitCode::FAILURE` 退出（非 0）（`main.rs:831-861`、`:881-901`、`:2254-2273`）。没有 state 目录、库打不开、写入失败，都是错误，不再静默跳过（`main.rs:914-941`）。终端上会先看到 `installed …` 或 `removed …`，然后是非 0 退出，以及这句补救：

> fix the state store and re-run the command

这句补救的实际效果见 F1。

今天生产代码写不出 `Granted` 行（见上，grant 只在测试里）。所以下面几条局限要等库里真有一条同意之后才会发生。`#1022` 的 APPROVE 评审（head `815c526`，2026-10-16）把期限记在事件上：第一个往生产代码里加 grant 路径的 PR，必须同时处理 F1、F3、F6。

### 已知局限（`#1022` 评审原文的编号）

出处：`gh pr view 1022` 的 APPROVE 评审，评审 head `815c526cd72785973fc466c94c4c7d197266c344`。F1、F3、F6、F7 的编号和内容按该评审。本轮用 `56264ef9` 的源码核对了位置和控制流。评审里那次「把 `agent24.db` 换成目录」的运行实验，本轮没有重跑；CLI 文件 blob 与评审时的 head 相同。

**F1 [Medium] CLI 报错给的补救办法不会重试撤销。**

- 位置：`rust/apps/agent24-cli/src/main.rs:831-861`（install）、`:881-902`（uninstall）、`:934-941`（错误信息）。「已经安装」的字符串在 `rust/crates/agent24-os-packages/src/install.rs:59`。
- 用户会看到：第一次 `os install` 或 `os uninstall` 在库打不开时非 0 退出，并被告知修好 store 再跑同一条命令。文件变更已经完成（安装成功的行、或 `removed` 的行，已经打印）。
- 再跑一次时撤销不会发生。`os install` 第二次在进入撤销之前就返回 `a domain OS named … is already installed; uninstall it first`。`os uninstall` 第二次走进 `Ok(false)`：打印 `{name} was not installed; nothing to remove`，返回 `Ok(())`，进程退出码 0（`main.rs:885-891`）。此时库可以仍是坏的，同意可以仍有效。CLI 没有 daemon 那种「启用前先撤销」的兜底。
- 评审记录的实验是：把 `agent24.db` 换成一个目录。`os install` 第一次 exit 1，第二次报 already installed。`os uninstall` 第一次 exit 1，第二次打印 `nothing to remove` 且 exit 0。本轮只核对了上述分支，没有复跑这个实验。

**F3 [Low] 「re-enabling it will require fresh consent」说多了。**

- 位置：这句 hint 在 `os_routes.rs:557-558`。它只在「同一个常驻 daemon 里，对 `os.json` 当前未启用的模块做 `PATCH` 启用」时成立，因为那条路会先撤销（`:481-496`）。
- 用户会看到的偏差有两条。一是 CLI 的离线说明让人改 `os.json`（`main.rs:846-848` 告诉用户下次启动生效）。手改文件会绕过启用前的撤销。二是 `stop` 共用这句 hint，但 stop 不改 `os.json`（`:1042-1066`）。撤销失败后如果包还在，daemon 重启会重新挂上这个模块，旧同意仍然有效；之后的 `PATCH` 启用也会因为模块原本就是启用而跳过兜底（`was_enabled == true` 时 `:490` 不撤销）。
- 所以看到 500 和这句 hint 时，不能据此认为「重启或手改 os.json 之后一定要重新同意」。

**F6 [Low，评审标明只从源码推出、未执行；本轮同样未执行]。**

- 短命 daemon 的 store 是 `Store::open_memory()`（`server.rs:1493-1495` 的注释写明 ephemeral 用 `:memory:`）。`PATCH /api/v1/os/{name}` 写的配置路径却是 `os_config::config_path()`，即 `state_dir()` 下的 `os.json`（`os_routes.rs:830`，`os_config.rs:118-119`），与常驻 daemon 是同一条路径。
- 用户会看到：对着短命 daemon 发的启用，只撤销那次进程内存里的库，撤销不到磁盘上的 `agent24.db`。磁盘上的旧同意还在，真实的 `os.json` 却可能已被写成启用。
- 触发条件是短命 daemon 真的挂了包并且有人直接打这个 HTTP。包根可以由 `A24_OS_PACKAGES` 指定（`agent24-os-packages` 的 `PACKAGES_ROOT_ENV`，`lib.rs:23`）。本轮没有起这样的进程去打 PATCH。

**F7 [Info，设计约束]。**

- 启用时的撤销写的是该模块 `op='*'` 的一行，`revoked_at` 为当时的时间（`module_consents.rs:402-439`）。查找时 `revoked_at >= decided_at` 的同意都算撤销（`:563-568`）。
- 用户会看到：一次成功的启用会作废这次启用之前记下的该模块全部同意。以后的授予必须发生在模块启用之后；启用之前写入的同意，会在这次启用时失效，工具继续是未授权，直到有一条更晚的 `decided_at`。今天没有生产授予路径，所以这条要等授予入口出现之后才碰得到用户。

### 行为变化：打开旧库会自动升级，而且不能按版本退回去

`Store::open` 在打开 SQLite 文件后立刻跑迁移（`rust/crates/agent24-store/src/lib.rs:153-166`），其中 `:159` 把日志设成 WAL，`:166` 是：

```rust
sqlx::migrate!("./migrations").run(&pool).await?;
```

`open_memory` 同样只调用 `.run()`（同文件 `:176-185`）。`56264ef9` 的 `*.rs` 里没有 `Migrator::undo`。

合入后 `agent24-store` 的迁移目录是 0001–0017。相对父提交 `600baba0` 只增加三个文件，0001–0014 的 blob 没有改（逐文件比对，没有 changed）：

```text
git diff --name-status 600baba0 56264ef9 -- rust/crates/agent24-store/migrations
A  0015_module_consents.sql
A  0016_run_source_tags.sql
A  0017_module_consent_revocations.sql
```

目录里没有 `*.up.sql` / `*.down.sql`。

锁定的 sqlx 是 0.8.6（`56264ef9:rust/Cargo.lock` 第 3338–3339 行；`agent24-store` 依赖 `sqlx` 0.8 并打开 `migrate` feature，`Cargo.toml:17`）。下面几条来自本机 cargo registry 里的 `sqlx-core-0.8.6` / `sqlx-macros-core-0.8.6`（本轮打开过这些文件）：

| 事实 | 证据 |
|---|---|
| `0015_*.sql` 这种单文件是 Simple 迁移，不可逆 | `migration_type.rs:5-7`：Simple 是 “single file migrations with no up / down queries”。`from_filename`（`:19-26`）只有以可逆后缀结尾才不是 Simple。`is_down_migration` 对 Simple 为 false（`:45-50`）。 |
| 编译期 `migrate!` 用的就是这套文件名规则，并且 `ignore_missing = false` | `sqlx-macros-core-0.8.6/src/migrate.rs:106` 调用 `resolve_blocking`，生成的 `Migrator` 以 `..Migrator::DEFAULT` 收尾（`:122-128`）。`migrator.rs:49-54` 的 `DEFAULT.ignore_missing` 是 `false`。 |
| 打开时只向前应用尚未记录的版本 | `migrator.rs:168-181`：跳过 down 文件；已应用且校验和一致则跳过；未应用则 `apply`。 |
| 已应用脚本被改过，下次打开直接失败 | `migrator.rs:175-176`：校验和不一致返回 `MigrateError::VersionMismatch`。 |
| `undo` 不会撤 Simple 迁移 | `migrator.rs:240-248` 只对 `is_down_migration()` 为真且版本高于 target 的脚本调用 `revert`。三个新文件都不是 down 脚本。应用代码也没有调用 `undo`。 |
| 换回只带 0001–0014 的旧二进制，旧库打不开，也不会把 schema 降回去 | `migrator.rs:28-41`：`ignore_missing` 为 false 时，已应用版本不在当前二进制的迁移集合中，就返回 `VersionMissing`。`error.rs:15-16` 的文案是 “migration {0} was previously applied but is missing in the resolved migrations”。 |

所以「打开旧库自动升级、不能按版本回退」成立。回退到合入前的 daemon 或旧 CLI 之前，需要自己留一份升级前的数据库副本。复制步骤见第二部分。本文没有在一份真实用户库上跑过「新二进制升级 → 旧二进制再打开」的实验。

### 用户能感知的其它变化

1. **桌面主对话和 `/api/v1/chat` 不再向外回退。** 桌面聊天页把本轮消息和 `session_id` POST 到 `/api/v1/chat`（`apps/desktop/src/renderer/pages/Chat.tsx:68-74`）。该接口不再使用「先本地、不行再远端」的默认画像，见第二部分。本地档都不可用时，HTTP 503，`code` 为 `provider_unavailable`，正文是 `All LLM providers unavailable. Last error: …`（`rust/apps/agent24d/src/routes.rs:682-686`）。桌面不把这段 `message` 原样当作唯一文案：`Chat.tsx:76-87` 先把它放进 `Error`，`Chat.tsx:111-116` 再套上 `reply.connectionError`。中文是「常见原因是本地模型服务未运行——请检查 oMLX（端口 8088）或 Ollama（端口 11434）」，服务器原文接在「错误：」后面（`Chat.i18n.ts:30`）。局域网或域名 URL 被标成 Remote 时，模型服务可以仍在跑，这句「未运行」会误导。请看错误尾部：出现 `no local provider available for a local-only task` 时，先去查 `OMLX_URL` / `OLLAMA_URL` 的主机是不是 loopback；**但这句不是「主机不是 loopback」的充分证据**——loopback 上的 Local 档一旦宕机就会进入冷却（`router.rs:362-380` 跳过冷却中的 provider，`:462-470` 记一次失败，`from_env` 的冷却从 2s 起、最长 60s，见 `router.rs:284`），冷却期内第二次调用也会报同一句。所以顺序是：先看进程/健康（本机 oMLX 是否在听），再看 URL 主机，不要只凭这句就断定是主机标错了。
2. **带会话的长对话可能停止压缩。** 压缩模型调用被固定成 LocalOnly。压缩失败不会把已经生成的回答作废，但摘要不会写入。条件见第二部分。
3. **agent run 同样不再向外回退。** 模型不可用时 run 以 `provider_unavailable` 失败，消息前缀是 `No provider could serve this request.`（`rust/crates/agent24-agent/src/lib.rs:1747-1750`）。
4. **explorer 子代理同样收窄。** 模型错误变成工具失败 `explorer model call: …`（`rust/crates/agent24-agent/src/subagent.rs:236-237`）。
5. **存在 Remote 档时，`documents` 模块工具不通告**（`module_tool_runtime.rs:172-174`）。两个 URL 都是 loopback 时这道守卫不触发。
6. **模块工具默认叫不通。** 没有同意记录就是 `NotGranted`。列表和撤销 API 已挂上，授予 API 没有。
7. **停用、停止、安装、卸载在撤销失败时不再报告成功。** 见上一节。停用/停止是 HTTP 500 `consent_revocation_failed`（停止本身失败时是另一条非 2xx）。启用前撤销失败是 HTTP 500 `internal`，并且不改 `os.json`。CLI 是非 0 退出。

---

## 第二部分：LocalOnly 收窄的实际影响清单

这是有意的产品行为，不是实现偏差。ADR-K1-02 规定：未知、缺失、过期或互相矛盾一律按 `LocalOnly`；`LocalOnly` 是出口约束，没有本地模型就返回 unavailable，不回退远端（`docs/adr/ADR-K1-02-session-dataflow-security-modes.md:15`、`:53`）。它删掉的是合入前 main 上已经在跑的回退。

合入前的父提交 `600baba0` 上，`/api/v1/chat` 的注释和调用是（`600baba0:rust/apps/agent24d/src/routes.rs:561-567`）：

```text
// Default profile: shareable + simple → local-first tier order, so everyday
// chat prefers the on-device model and only falls back outward (D2).
.complete_served(TaskProfile::default(), &request, &cancel)
```

`TaskProfile::default()` 是 `Privacy::Any` + `Complexity::Simple`（`56264ef9:router.rs:63-83`，两者的 `#[default]`）。`Any` + `Simple` 的档位顺序是 Local、Lora、Remote（`router.rs:93`）。Remote 因此是回退目标。

合入后同一处改为按 run 取政策（`routes.rs:561-570`）。agent loop 每轮同样改道（`lib.rs:1725-1726`）。政策来源如下，四条路径的结果都是 `Privacy::LocalOnly`：

| 路径 | 为什么是 LocalOnly | 位置 |
|---|---|---|
| agent loop（主对话 /runs） | 入口写入 `SourceRef::user_input`，模式固定 `LocalOnly`。一条 LocalOnly 标签让整个快照成为 LocalOnly。 | `lib.rs:1547-1552`；`source_policy.rs:111-124`、`:199-205`；`merge_source_policy` 在 `lib.rs:49-56` 把 LocalOnly 写成 `Privacy::LocalOnly` |
| `POST /api/v1/chat` | 这个 run id 是临时的，没有插入 `runs`，也没有写标签。`run_policy_snapshot` 对零行返回 fail-closed 的 LocalOnly。查询出错同样落到 LocalOnly。 | `routes.rs:498`、`:561-570`；`source_policy.rs:307-313`、`:199-205`；`lib.rs:482-494` |
| explorer | 有 store 时读同一快照；读失败或没有 store 都是 LocalOnly。 | `subagent.rs:217-231` |
| 会话压缩 | 不读 run 政策，直接把 `SourceMode::LocalOnly` 交进去。接口没有 run id。 | `lib.rs:163-166`、`:239-243` |

`LocalOnly` 的档位顺序只有 `Local` 和 `Lora`（`router.rs:91`）。路由结果为空时的错误字符串是 `no local provider available for a local-only task`（`router.rs:432-435`）。有本地档但全部 `Unavailable` 时，错误是 `all routed providers unavailable [...]`（`router.rs:478-481`），仍然不会去碰 Remote。

### 判定：什么 URL 算本地

生产 daemon 的路由器是 `ModelRouter::from_env()`（`rust/apps/agent24d/src/server.rs:1514`）。`56264ef9` 上这个函数的生产调用点只有这一处（`router.rs:1093` 在测试里；`agent24-cli/src/service.rs:130` 只是注释）。它只注册两个 provider：

| 环境变量 | 缺省 | 档位 |
|---|---|---|
| `OMLX_URL` | `http://127.0.0.1:8088` | `env_local_tier(&omlx_url)` |
| `OLLAMA_URL` | `http://127.0.0.1:11434` | 同一函数 |

位置：`router.rs:292-305`。`from_env` 还读 `OMLX_API_KEY` 和 `DEFAULT_MODEL`。本文不记录密钥的缺省值。`ProviderRegistry::from_env`（`rust/crates/agent24-models/src/lib.rs:823-844`）仍把 Ollama 硬编码成 `http://127.0.0.1:11434` 且不做 loopback 判定，但 daemon 的聊天和 run 不走它（全树没有 `ProviderRegistry::from_env` 的调用，只有 `router.rs:288` 的注释）。

`env_local_tier`（`router.rs:172-186`）要求下面三个 URL 全部通过 `is_loopback_url`，才标 `Local`：原 URL、`{url}/v1/chat/completions`、`{url}/v1/models`。否则打日志 `provider URL {url} is not loopback — labeling Remote so LocalOnly tasks won't route to it`，并标 `Remote`。标成 Remote 的 provider 仍然注册着，只是 LocalOnly 任务选不中它。

`is_loopback_url`（`router.rs:145-169`）的规则：

1. 用 `reqwest::Url`（WHATWG）解析。解析失败 → 不是 loopback。
2. scheme 必须是 `http` 或 `https`。`file://` 等不是。
3. 主机是 IP 时，调用 Rust `IpAddr::is_loopback()`。本轮没有打开 rustc 源码核对八位组实现；行为以同文件测试锁定的例子为准。
4. 主机不是 IP 时，只有与 `localhost` 忽略大小写完全相等才算。其它主机名一律不是，包括带尾点的 `localhost.`、`host.docker.internal`、`*.local`、机器名。

内网地址不算本地。路由器不调用 `is_private()`。单测已经把 `http://192.168.1.50:8088` 锁成 Remote（`router.rs:1023`，测试函数自 `:1000`）。

同文件 `:1000-1030` 的 `env_local_tier_uses_the_http_clients_own_parser` 把边界锁进测试。会被标成 **Local** 的例子包括：

- `http://127.0.0.1:8088`
- `http://localhost:8088`、`http://LOCALHOST:8088`
- `https://[::1]:8443`
- `http://user:pw@127.0.0.1:8088`（userinfo 不影响主机）
- `http://0x7f000001:8088`（WHATWG 归一成 `127.0.0.1`）

会被标成 **Remote** 的例子包括：

- `http://192.168.1.50:8088`（局域网）
- `http://0.0.0.0:8088`（未指定地址，不是 loopback）
- `http://[::ffff:127.0.0.1]:8088`（IPv4-mapped，测试注释写明不是 `is_loopback()`）
- `http://localhost.:8088`（尾点）
- `https://inference.example.com`
- `file:///tmp/x`、空串、解析失败的字符串
- `http://evil.example\@127.0.0.1:8088`（反斜杠在 WHATWG 里结束 authority，主机是 `evil.example`）

`10.0.0.0/8` 与 `172.16.0.0/12` 没有单独的测试行。它们走同一谓词：不是 loopback IP、也不是精确的 `localhost`，结果是 Remote。这个推断本轮没有用真实 URL 跑过 `env_local_tier`。

### 哪些部署配置合入后会失效

两个 URL 都会被注册。失效的是「LocalOnly 任务还能不能用这个端点」，以及「路由器里有没有 Remote 档」。

| 配置 | 合入后 |
|---|---|
| `OMLX_URL` 与 `OLLAMA_URL` 都缺省，或都指向 `127.0.0.1` / `localhost` / `::1` | 两档都是 Local。主对话仍走本机。 |
| 只有一个 URL 指向非 loopback，另一个保持 loopback 且进程真的在听 | 非 loopback 那个被标 Remote，主对话、chat、explorer、压缩都不会再落到它。loopback 那个仍可服务。合入前本地档宕机时会继续试 Remote；合入后本地档 `Unavailable` 即失败，错误为 `all routed providers unavailable [...]`。 |
| 两个 URL 都是非 loopback（局域网 IP、`0.0.0.0`、域名、Tailscale 主机名、`host.docker.internal`、远端 OpenAI 兼容网关） | 两个 provider 都是 Remote。LocalOnly 的路由列表为空，立即 `no local provider available for a local-only task`。`/api/v1/chat` 返回 503 `provider_unavailable`。agent run 失败。explorer 工具失败。压缩停在下面说的条件上。桌面中文提示仍可能写成「本地模型服务未运行」，以错误尾部的这句英文为准去查 URL。 |
| 任一 URL 非 loopback，因而 `has_remote_tier()` 为 true | `documents` 模块工具被守卫挡住，不通告（`module_tool_runtime.rs:172-174`）。把那个 URL 改回 loopback（或删掉覆盖、回到缺省 loopback）之后，守卫随 Remote 档消失而放开。同意门仍然单独生效。 |
| 模块 manifest 写了 `model_access = remote_allowed` 的模块回调 | 这条路径仍是 `Privacy::Any`（`model_callback.rs:526-528`），可以选中 Remote 档。它不受本次「主对话收窄」的影响。缺省 manifest 仍是 LocalOnly（`domain/lib.rs:834`）。 |
| `~/.pi/agent/models.json` 里的 provider | agent24d 的 `from_env` 不读这个文件。改它不会改变 daemon 的路由。 |

没有第三个「云 API」环境变量。想继续用远端模型，不能靠多设一个 key；`ModelRouter::from_env` 的两个 URL 就是 daemon 聊天和 run 的全部。

### 初稿当时看到的本机配置（本轮未重跑）

下面四条是写第一稿时对当时环境的观察。本轮修订没有重跑 `lsof`、`curl`，也没有重新检索 LaunchAgents。进程号和 HTTP 状态会变，不能当成现在仍是如此。

- 当时会话环境里 `OMLX_URL`、`OLLAMA_URL`、`DEFAULT_MODEL` 都未设置。缺省因此是 `http://127.0.0.1:8088` 和 `http://127.0.0.1:11434`，两者都是 Local。
- 在 `~/Library/LaunchAgents`、`~/Library/LaunchDaemons`、`/Library/LaunchAgents`、`/Library/LaunchDaemons`、`~/.agent24` 和当时的 worktree 里按文件名检索 `OMLX_URL` / `OLLAMA_URL`，没有命中。没有扫整个家目录，也没有读密钥。
- 当时 `lsof -nP -iTCP:8088 -sTCP:LISTEN` 显示 `127.0.0.1:8088` 在听（进程 `python3.1`，PID 27391）。`curl --max-time 3` 对 `http://127.0.0.1:8088/v1/models` 返回 `401`。未携带密钥。
- 当时 `~/.pi/agent/models.json` 的 `providers.omlx.baseUrl` 是 `http://127.0.0.1:8088/v1`。同文件 `providers.deepseek.baseUrl` 是 `http://127.0.0.1:8791`，是 Pi 自己的 provider。agent24d 的 `from_env` 不会注册它。apiKey 字段没有读取进本文。

用缺省环境变量启动的 agent24d，主对话会打 `127.0.0.1:8088` 的 oMLX。若某个未检索的启动脚本设置了非 loopback URL，结论要按那个值重算。

### 会话压缩被固定成 LocalOnly 之后会怎样

压缩器是 `RouterSummarizer`，daemon 在 `server.rs:522-527` 把它套在会话记忆上。`SessionMemory::new` 使用 `CompactionPolicy::default()`（`rust/crates/agent24-agent/src/session_memory.rs:82`）。默认阈值是：verbatim 尾部超过 40 条就压缩，保留最近 20 条，摘要最长 4000 字符（`rust/crates/agent24-memory/src/session.rs:40-47`）。这两个文件的 blob 在 `571190cb`、`815c526` 与 `56264ef9` 上相同。

`remember` 先把这一轮写进事件日志，再做压缩。压缩失败只打日志，不让已经完成的回答失败（`session_memory.rs:407-420`）：

- 成功：摘要写入。
- 摘要器返回错误：日志 `session {sid} compaction failed; originals retained: {err}`，原文留在事件日志。
- 超时：日志 `session {sid} compaction timed out; originals retained`。
- 写日志已经用完 30 秒预算（`lib.rs:136` 的 `MEMORY_WRITE_BUDGET`）时，根本不调用 `compact`（`session_memory.rs:412-414`）。

摘要器内部（`lib.rs:239-248`）固定 LocalOnly。下面任一情况都会让 `summarize` 返回 `Err`，从而走进上面的失败日志：

- 两个 URL 都是 Remote，或唯一的 Local 档不可用：`ModelError::Unavailable("no local provider available for a local-only task")`（`router.rs:432-435`）。
- Local 档都 `Unavailable`（进程没起来、连接被拒）：`all routed providers unavailable [...]`（`router.rs:478-481`）。
- 模型返回空摘要：`summarizer returned an empty summary`（`lib.rs:247-248`）。

`compact` 只有在尾部长度大于 `max_recent`（默认 40）时才调用摘要器（`session_memory.rs:462` 一带；该文件 blob 未改，阈值逻辑与合入前说明所对的树相同）。短会话看不出变化。

下一次加载上下文时，若尾部超过 `max_recent * 4`（默认 160）条，会从提示里丢掉最老的消息，并打错误日志；事件日志里的原文还在（`session_memory.rs:17`、`:279-291`）。对话回答本身已经在压缩步骤之前落盘，所以用户仍能看到这一轮回复；变长的是后续轮次送进模型的未压缩尾部，直到有一个 Local 档能完成摘要。

桌面聊天因为每次都带 `session_id`（`Chat.tsx:73`），走的是 `chat_remember_turn` → `remember_exchange`（`lib.rs:774-780`），和 agent loop 共用这段压缩。合入前父提交上的注释写明当时故意与聊天共用 `TaskProfile::default()`。合入后的注释改成了 fail closed（`56264ef9:lib.rs:163-166`）。

### 迁移建议（部署方要改什么）

这次没有「允许这块对话使用远端模型」的开关。`CloudAuthorized` 没有生产写入点（见第一部分）。在那个入口出现之前，主对话、`/api/v1/chat`、explorer 和会话压缩都不会选择 Remote 档。

要让现有对话继续工作，部署方改的是 URL 的主机，让 `env_local_tier` 把它标成 Local：

1. `OMLX_URL` 使用 `http://127.0.0.1:<port>`、`http://localhost:<port>` 或 `http://[::1]:<port>`。`OLLAMA_URL` 同样。不要用局域网 IP、`0.0.0.0`、机器名、`host.docker.internal`、`localhost.`（尾点）或 `::ffff:127.0.0.1`。
2. 模型进程若在另一台机器上，daemon 看到的 URL 主机仍然必须是上面三种 loopback。常见做法是在跑 daemon 的机器上做 SSH 转发或本机反向代理，把 loopback 端口转到那台机器。这样做之后，路由器会把该端点当成 Local，LocalOnly 流量会发过去。这和 ADR-K1-02:53「tier 必须是可信的本机端点」的语义不一致：隧道只骗过主机名检查，不构成获准云处理。把它当作维持服务的临时接法，并在后续 CloudAuthorized 入口落地后拆掉。
3. 只需要本机 oMLX、不需要局域网 Ollama 时：不要设置 `OLLAMA_URL`，或把它设回 `http://127.0.0.1:11434`。留一个局域网 `OLLAMA_URL` 会让 `has_remote_tier()` 为 true，`documents` 工具继续被挡住，而聊天也回退不到那台 Ollama。
4. 不要通过修改 `~/.pi/agent/models.json` 来修 agent24d。那个文件不是 daemon 的路由配置。
5. 升级前复制数据库。路径是 `$AGENT24_DATA_DIR/agent24.db`；该变量缺省、为空或不是绝对路径时，目录是 `~/.agent24`，文件是 `~/.agent24/agent24.db`（`rust/crates/agent24-protocol/src/state_file.rs:127-148`；daemon 在 `server.rs:1502` 拼接 `agent24.db`）。`Store::open` 使用 WAL（`lib.rs:159`）。daemon 还在跑时只拷贝 `.db` 会漏掉尚未 checkpoint 的写入。先停 daemon，再把 `agent24.db`、`agent24.db-wal`、`agent24.db-shm` 一起拷走；或者不停进程，用 `sqlite3 <路径> ".backup <副本>"`。旧版 daemon 和旧版 CLI 打开同一份库都会 `VersionMissing` 而拒绝打开：CLI 的 `os install` / `os uninstall` 在 `main.rs:923` 打开它，`decide` 导出/删除在 `main.rs:1135` 打开它。新二进制第一次 `Store::open` 就会应用 0015–0017，不会自动降级。

模块回调若必须继续打远端，保持该模块 manifest 的 `model_access` 为 `remote_allowed`。这只覆盖模块自己的模型回调，不覆盖桌面聊天和 agent loop。

---

## 第三部分：法律文本（合入之后的事实：jason 2026-10-16 选定路径 1，执行 PR 为 #1031）

`#1022` 已经合入。下面不再写「先于 #1022 合一个法律 PR」。那样做的基线（当时的 `origin/main` `72c2a580`）已经过时，而且会在 PLAN 里留下「法律文本由独立 PR 进入 main」的假记录。

### 核实

`62218c312db184dc0763b2c7b4eb55bd3dff1333` 是实现提交，不是法律专提交。它是 PR head `815c526` 的祖先，不是 squash `56264ef9` 的祖先。

```text
git show --stat --format=fuller 62218c31
commit 62218c312db184dc0763b2c7b4eb55bd3dff1333
Author:     jhfnetboy <jhfnetboy@gmail.com>
AuthorDate: Thu Oct 8 13:59:46 2026 +0700
    fix(kernel): K1-6a.5 —— 收窄可逆草稿写入审批例外

 docs/adr/ADR-K1-03-enable-authorization-trust-revocation.md |  13 ++
 docs/laws/APPROVAL.md                                       |  12 +-
 rust/crates/agent24-domain/src/lib.rs                       |  11 +
 rust/crates/agent24-domain/src/tool.rs                      |  15 ++
 rust/crates/agent24-policy/src/consent_gate.rs              | 229 ++++++++++++++++++++-
 5 files changed, 266 insertions(+), 14 deletions(-)
```

父提交只有一个：`6993613ffab301f70c9a9c0d00c808966d71c6f9`。

两个 blob，本轮用 `git rev-parse <tree>:<path>` 核对过：

| 对象 | `docs/laws/APPROVAL.md` 的 blob |
|---|---|
| `62218c31^` 与 squash 父提交 `600baba0` | `16db448916d984277314e66ea54eaa80e531e059` |
| `62218c31`、`815c526`、`56264ef9`、写本文时的 `origin/main`（`ebc9b6e7`） | `244429b037bb4a11de4ed0794823ea4ef6da139b` |

`git diff --stat 600baba0 56264ef9 -- docs/laws` 只有 `docs/laws/APPROVAL.md`（5 行增、7 行删）。main 上第一次出现 blob `244429b0` 的提交就是 `#1022` 的 squash `56264ef9`。L-APPR-5 在这棵树上从 `docs/laws/APPROVAL.md:87` 开始。

PLAN 的两条要求在 `56264ef9` 上仍然是：

- `docs/agent/PLAN-KERNEL-K1.md:24`：K1 ADR 若确立新的长期隐私/外传不变量，先单独提法律文件，按 laws README 闭环；不得在实现 PR 中顺手改法律。
- 同文件 `:84`：法律变更必须单独 PR；实现 PR 点名符合的 L-APPR、L-MEM、L-CTX 及新增隐私法律。

ADR 把实质决定和流程分开写了：

- `docs/adr/ADR-K1-03-enable-authorization-trust-revocation.md:120`：jason 于 2026-10-08 授权为第一方 Documenting 的可逆草稿设窄例外；该决定同步修订 L-APPR-5。同文件 `:118-127` 是 K1-6a.5 补遗。
- 同文件 `:105`：LocalOnly、授权撤销等若要成为新法律，必须另提独立 PR；本 ADR 不代替法律流程。

因此，2026-10-08 的授权是「这条例外的内容可以成立」。**`#1022` 没有满足 `PLAN-KERNEL-K1.md:84` 的「法律变更必须单独 PR」。** 法律正文是和实现一起，由同一个 squash 进入 main 的。ADR 自己写了它不代替法律流程，所以 2026-10-08 的实质授权本身还不是「接受这次不单独评审」。

同一历史提交里的另外四个文件是实现和 ADR，不是法律正文：`docs/adr/ADR-K1-03-enable-authorization-trust-revocation.md`、`rust/crates/agent24-domain/src/lib.rs`、`rust/crates/agent24-domain/src/tool.rs`、`rust/crates/agent24-policy/src/consent_gate.rs`。

分发路径仍然不提供草稿核验（`server.rs:1601-1604`）。法律文本把例外收窄到一组宿主核验都成立的情况；这条装配路径不提供该结构，所以合入本身不改变现在的逐次审批行为。

### 补救路径（已定：jason 2026-10-16 选定路径 1，由 #1031 执行）

本文不选路，不改 `PLAN-KERNEL-K1.md`，不改 `docs/laws/`，也不开下面任何一个 PR。

**路径 1：追溯性独立法律评审 PR。**

针对 blob `16db448916d984277314e66ea54eaa80e531e059` → `244429b037bb4a11de4ed0794823ea4ef6da139b` 这段 diff 单独开一个 PR，让 PR-Daemon 评审的是法律文本本身。

- 证据就是上一节的 blob 表：改前是 `62218c31` 的父提交，也是 squash 的父提交；改后是 `62218c31` 的结果，也是现在 main 上的文件。
- 从当时的 main 拉分支之后，`docs/laws/APPROVAL.md` 已经是 `244429b0`。**路径 1 的实际执行（PR #1031）并不是空操作**：它在 `APPROVAL.md` 的 L-APPR-5 「今天靠什么保证」之后新增溯源与处置记录。中间 head 的 blob 曾是 `38de9810`，**不要把这个中间值当成合入后的 blob**；#1034 已把 MUST 收窄进 main（写本句时 main 上该文件是 `6b87e274`），#1031 合入后的 blob 以合并提交为准。#1031 自己不改 MUST 表述。
- 评审材料要写在 PR 正文里，例如 `git diff 600baba0 56264ef9 -- docs/laws/APPROVAL.md`，或 `git show 62218c31 -- docs/laws/APPROVAL.md`。正文应写明：文本最初出现在实现提交 `62218c31`，随 `#1022`（`56264ef9`）进入 main；本 PR 是事后补上的独立法律评审；jason 2026-10-08 的实质授权见 ADR-K1-03:120；本 PR 不包含 `consent_gate.rs`。
- 事后补的是评审记录，不是内容：main 的历史仍然显示这段 MUST 表述第一次进入 main 是 `#1022`。**不要在 PLAN 里写成「已由该法律 PR 单独评审后进入 main」**——那句话是假的。能写的是「文本随 #1022 进入 main，事后由 #1031 补做独立评审」。

**路径 2（未采用）：jason 书面接受，并记入 PLAN。**

在 `docs/agent/PLAN-KERNEL-K1.md` 的 `:24` 和 `:84` 附近各加一句流程记录：L-APPR-5 的修订已随 `#1022`（squash `56264ef92bd08b7bdcebc7aa45a78e8b7065bd9a`）进入 main，jason 于某日书面接受这次不单独评审该 diff。日期由 jason 写，本文不填。这句话只记录接受，不要和内核实现放在同一个提交。

2026-10-08 的 ADR 授权不是这条接受。ADR-K1-03:105 写明 ADR 不代替法律流程。

**已定（2026-10-16）：走路径 1，执行 PR 为 #1031。** 两条路都承认同一件事：`#1022` 没有满足「法律变更必须单独 PR」。路径 1 补的是事后的独立评审记录（在 `APPROVAL.md` 里新增一段溯源，MUST 表述不变）；路径 2（写成 jason 的书面接受）**未采用**。

### 涉及的精确文件与提交

| 项 | 值 |
|---|---|
| 夹带法律的历史实现提交 | `62218c312db184dc0763b2c7b4eb55bd3dff1333`（`815c526` 的祖先，不是 `56264ef9` 的祖先） |
| 该提交的父提交 | `6993613ffab301f70c9a9c0d00c808966d71c6f9` |
| 作者 | `jhfnetboy <jhfnetboy@gmail.com>`，2026-10-08 13:59:46 +0700 |
| 提交说明 | `fix(kernel): K1-6a.5 —— 收窄可逆草稿写入审批例外` |
| 法律文件 | `docs/laws/APPROVAL.md`（L-APPR-5，自 `56264ef9` 的第 87 行起） |
| 改前 blob（`62218c31^` 与 `600baba0`） | `16db448916d984277314e66ea54eaa80e531e059` |
| 改后 blob（`62218c31`、`56264ef9`、`ebc9b6e7`） | `244429b037bb4a11de4ed0794823ea4ef6da139b` |
| 把该 blob 带上 main 的提交 | `56264ef92bd08b7bdcebc7aa45a78e8b7065bd9a`（`#1022`） |
| 规则出处 | `docs/agent/PLAN-KERNEL-K1.md:24`、`:84`（在 `56264ef9` 与当前 main 上） |
| 实质授权出处 | `docs/adr/ADR-K1-03-enable-authorization-trust-revocation.md:105`、`:118-127` |
| 路径选择 | jason 2026-10-16 选定路径 1；执行 PR 为 #1031（在 `APPROVAL.md` 新增一段溯源，MUST 表述不变） |

---

## 本文没有核实的部分

- 没有在一份升级后的用户 SQLite 上启动只含 0001–0014 的旧二进制，去观察 `VersionMissing` 的实际错误文本。不可回退的结论来自本轮读过的 sqlx 0.8.6 源码，以及仓库里只有向前的 `.run()`、没有 `Migrator::undo`。
- 没有跑 `cargo test`，没有启动合入后的 daemon，没有在浏览器里点桌面聊天。
- 没有重跑 `#1022` 评审记录的两处实验：SQLite trigger 让撤销写入失败后的 PATCH；把 `agent24.db` 换成目录后的 CLI 退出码。F1 的「第二次 uninstall 退出码 0」是该评审的实测，本轮只核对了 `Ok(false)` 分支不调用撤销。
- 没有执行 F6 的短命 daemon 场景。
- 没有重跑初稿里的本机探测（环境变量、LaunchAgents、`lsof`、`curl`、`models.json`）。那一节已标明。
- 没有扫描整个家目录，没有读取任何 API key。
- `10.0.0.0/8` 与 `172.16.0.0/12` 没有单独的单元测试行，本轮也没有调用 `env_local_tier` 去跑它们。`192.168.1.50` 在 `router.rs:1023` 有直接测试。
- 本分支没有 rebase 到 main。评审的 L4 仍在。本次只改这一份说明。
