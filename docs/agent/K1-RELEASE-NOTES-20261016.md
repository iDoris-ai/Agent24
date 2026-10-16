# K1 发布说明与落地前核查（2026-10-16）

- 对象：`origin/merge/kernel-to-main-20261016` @ `571190cb`（父提交 `9d1062cc` = `origin/ab/kernel`，`729a7159` = 当时的 main）。`ab/kernel` 相对其合并基线为 78 个提交（`git rev-list --count origin/main..origin/ab/kernel` = 78；`origin/ab/kernel` = `9d1062cc`）。
- 对照：写这份说明时的 `origin/main` 是 `72c2a580`。它比 `729a7159` 只多一个文档提交（`git show --stat 72c2a580` 只改 `docs/`），下文引用的 `origin/main` Rust 行号与 `729a7159` 相同。
- 行号：除特别标明 `origin/main` 或本机工具链外，`文件:行号` 都指 `571190cb` 那棵树。用 `git show 571190cb:<path>` 可以复核。
- 范围：发布说明草稿、LocalOnly 收窄的部署影响、L-APPR-5 的流程拆分。复核报告问 1（停用/卸载时 consent 撤销失败仍返回成功）不在这里重判，仍以 `docs/agent/REVIEW-kernel-to-main-20261016.md`（分支 `baton/grokbot-20261016-a24-k1review`）为准。
- 本文是可复核的事实和给发布方的建议，不是审批，也不代替 PR-Daemon。

---

## 第一部分：发布说明草稿（给用户看）

### 这次 K1 带来什么

K1 把「模块工具能不能被调用」和「这段对话能不能送出本机」收成宿主侧的两道门。用户能对上号的是下面四块。

**授权 / consent。** 库里新增每工具一条的启用同意记录，以及单独的撤销记录。

- `rust/crates/agent24-store/migrations/0015_module_consents.sql:23-43` 建 `module_consents`：主键是 `(module, op)`，记下版本、权限指纹、来源、风险、决定（`granted` / `denied`）和到期时间。
- `rust/crates/agent24-store/migrations/0017_module_consent_revocations.sql:4-9` 建 `module_consent_revocations`。`op='*'` 覆盖该模块全部工具；后写的同意若 `decided_at` 更新，可以盖过更早的撤销（同文件 `:1-3` 的注释）。
- 调用时由 `StoreConsentGate::authorize` 查这两张表。查不到、查失败、被拒、过期、撤销、版本/指纹对不上，都是否决（`rust/crates/agent24-policy/src/consent_gate.rs:300-355`）。
- 宿主 HTTP 只有列表、导出和撤销，没有授予：
  - `GET /api/v1/module-consents`（`rust/apps/agent24d/src/server.rs:1081`）
  - `GET /api/v1/module-consents/export`（同文件 `:1082-1085`）
  - `DELETE /api/v1/module-consents/modules/{module}`（`:1086-1089`）
  - `DELETE /api/v1/module-consents/modules/{module}/tools/{tool}`（`:1090-1093`）
- 生产代码里没有写入 grant 的路由。因此升级之后，模块工具在这道门上是未授权。daemon 给每个已注册模块工具装配的请求把可逆草稿例外关掉了（`server.rs:1601-1604`：`reversible_draft_declared: false`，`draft_verification: None`）。用户这次看不到新的「同意」对话框。

**模块工具调用与审计。** 模块工具在进运行时之前先过同意门，并写类型化审计：调用前 `PreDispatch`，结束时 `Terminal`（`rust/crates/agent24-tools/src/lib.rs:601`、`:810`、`:979-1001`）。审计挂在内核 store 上，不靠模块自己的日志。

**来源分类 / 敏感资料。** 每个 run 可以挂来源标签，表是 `run_source_tags`（`0016_run_source_tags.sql:19-28`）。模式只有 `LocalOnly` 和 `CloudAuthorized`（`rust/crates/agent24-store/src/source_policy.rs:74-79`）。这条线实际写进去的用户输入标签固定是 `LocalOnly`（`source_policy.rs:111-124`；agent loop 在 `rust/crates/agent24-agent/src/lib.rs:1545-1550` 写入）。没有标签、标签互相冲突、或读失败，有效模式都是 `LocalOnly`（`source_policy.rs:182-205`、`:307-313`；`lib.rs:480-492`）。`SourceMode::CloudAuthorized` 在生产路径里没有写入点：全树检索只命中折叠函数本身和测试（`source_policy.rs:209`，`lib.rs:2449` 与 `:2459` 都在测试里）。获准把资料送到云端的入口，这次没有做。

**出站判定。** 已经接到产品路径上的，是模型路由这一道：`Privacy::LocalOnly` 的任务只在 `Local` 和 `Lora` 两档里选，本地都不可用就报错，不会改去远端（`rust/crates/agent24-models/src/router.rs:80-91`、`:414-430`）。ADR-K1-02 把 HTTP、MCP、Exec、记忆外流也列进出站门（`docs/adr/ADR-K1-02-session-dataflow-security-modes.md:57-65`），并把它记成后续切片 6b.3（同文件 `:106`）。这次合入没有一条统一的出站判定函数去包 `http_fetch` 或 `shell_exec`：在 `rust/crates/agent24-tools/src/net.rs` 和 `local.rs` 里检索 `LocalOnly` / `SourceMode` 没有命中。模块模型回调仍看 manifest 的 `model_access`，缺省是 `LocalOnly`，写了 `remote_allowed` 才映射成 `Privacy::Any`（`rust/crates/agent24-domain/src/lib.rs:825-840`，`rust/apps/agent24d/src/model_callback.rs:497-500`）。那条回调路径没有改成「一律 LocalOnly」。

另外，只要路由器里存在任何一个 Remote 档，`documents` 模块的工具就不对外通告（`rust/apps/agent24d/src/module_tool_runtime.rs:121-128`、`:172-174`；装配点 `server.rs:1615-1618` 把 `router.has_remote_tier()` 传进去）。`has_remote_tier` 是这次在路由器上新增的 7 行（`router.rs:198-203`；`git diff --stat origin/main 571190cb -- rust/crates/agent24-models/src/router.rs` 为 `7 +++++++`，其余路由判定代码与 main 相同）。

### 行为变化：打开旧库会自动升级，而且不能按版本退回去

`Store::open` 在打开 SQLite 文件后立刻跑迁移（`rust/crates/agent24-store/src/lib.rs:151-165`）：

```rust
sqlx::migrate!("./migrations").run(&pool).await?;
```

`open_memory` 同样只调用 `.run()`（同文件 `:175-184`）。仓库里没有第二处生产代码去调用 `Migrator::undo`。

合入后 `agent24-store` 的迁移目录是 0001–0017。相对 `origin/main` 只增加三个文件，0001–0014 的 blob 没有改：

```text
git diff --name-status origin/main 571190cb -- rust/crates/agent24-store/migrations
A  0015_module_consents.sql
A  0016_run_source_tags.sql
A  0017_module_consent_revocations.sql

git ls-tree --name-only 571190cb rust/crates/agent24-store/migrations | wc -l
17
```

目录里没有 `*.up.sql` / `*.down.sql`（`git ls-tree -r --name-only 571190cb rust/crates/agent24-store/migrations | rg 'up.sql|down.sql'` 无输出）。

锁定的 sqlx 是 0.8.6（`571190cb:rust/Cargo.lock` 第 3338–3339 行 `name = "sqlx"` / `version = "0.8.6"`；`agent24-store` 依赖 `sqlx` 0.8 并打开 `migrate` feature，`571190cb:rust/crates/agent24-store/Cargo.toml:17`）。下面几条来自本机 cargo registry 里的 `sqlx-core-0.8.6` / `sqlx-macros-core-0.8.6`（与锁文件版本一致）：

| 事实 | 证据 |
|---|---|
| `0015_*.sql` 这种单文件是 Simple 迁移，不可逆 | `migration_type.rs:5-7`：Simple 的定义是 “single file migrations with no up / down queries”。`from_filename`（`:19-26`）只有以 `.up.sql` / `.down.sql` 结尾才是可逆；其余（含 `.sql`）是 Simple。`is_down_migration` 对 Simple 为 false（`:45-49`）。 |
| 编译期 `migrate!` 用的就是这套文件名规则，并且 `ignore_missing = false` | `sqlx-macros-core-0.8.6/src/migrate.rs:105-128` 在编译期调用 `resolve_blocking`，生成的 `Migrator` 以 `..Migrator::DEFAULT` 收尾。`migrator.rs:49-54` 的 `DEFAULT.ignore_missing` 是 `false`。 |
| 打开时只向前应用尚未记录的版本 | `migrator.rs:168-181`：跳过 down 文件；已应用且校验和一致则跳过；未应用则 `apply`。 |
| 已应用脚本被改过，下次打开直接失败 | `migrator.rs:175-176`：校验和不一致返回 `MigrateError::VersionMismatch`。 |
| `undo` 不会撤 Simple 迁移 | `migrator.rs:240-248` 只对 `is_down_migration()` 为真且版本高于 target 的脚本调用 `revert`。三个新文件都不是 down 脚本，循环体为空，函数返回 `Ok(())`，表还在。应用代码也没有调用 `undo`。 |
| 换回只带 0001–0014 的旧二进制，旧库打不开，也不会把 schema 降回去 | `migrator.rs:28-41`：`ignore_missing` 为 false 时，`_sqlx_migrations` 里任何一个版本不在当前二进制的迁移集合中，就返回 `VersionMissing(版本号)`（`error.rs:15-16` 的文案是 “migration {0} was previously applied but is missing in the resolved migrations”）。旧二进制的集合止于 14，库里已有 15/16/17，打开在 `validate_applied_migrations` 处失败，不会执行 `DROP`。 |

所以「打开旧库自动升级、不能按版本回退」成立，依据是：应用只调用向前的 `run()`，磁盘上没有 down 脚本，sqlx 把这些文件标成 Simple，旧二进制遇到更高版本号会 `VersionMissing` 而拒绝打开。回退到合入前的 daemon 之前，需要自己留一份升级前的数据库文件副本。本文没有在一份真实用户库上跑过「新二进制升级 → 旧二进制再打开」的实验，见文末。

### 用户能感知的其它变化

1. **桌面主对话和 `/api/v1/chat` 不再向外回退。** 桌面聊天页把本轮消息和 `session_id` POST 到 `/api/v1/chat`（`apps/desktop/src/renderer/pages/Chat.tsx:68-74`）。该接口不再使用「先本地、不行再远端」的默认画像，见第二部分。本地档都不可用时，HTTP 503，`code` 为 `provider_unavailable`，正文是 `All LLM providers unavailable. Last error: …`（`rust/apps/agent24d/src/routes.rs:680-684`）。桌面把这段 `message` 显示出来（`Chat.tsx:76-87`）。
2. **带会话的长对话可能停止压缩。** 压缩模型调用被固定成 LocalOnly。压缩失败不会把已经生成的回答作废，但摘要不会写入。条件见第二部分。
3. **agent run 同样不再向外回退。** 模型不可用时 run 以 `provider_unavailable` 失败，消息前缀是 `No provider could serve this request.`（`rust/crates/agent24-agent/src/lib.rs:1744-1754`）。
4. **explorer 子代理同样收窄。** 模型错误变成工具失败 `explorer model call: …`（`rust/crates/agent24-agent/src/subagent.rs:234-235`）。
5. **存在 Remote 档时，`documents` 模块工具不通告**（见上，`module_tool_runtime.rs:172-174`）。两个 URL 都是 loopback 时这道守卫不触发。
6. **模块工具默认叫不通。** 没有同意记录就是 `NotGranted`。列表和撤销 API 已挂上，授予 API 没有。

main 上已有的、这次核对过并保留的行为（复核报告问 3 的点名项，此处只复述结论，不展开）：`PASSTHROUGH_VARS` 仍是 19 项、`state_dir()` 仍是 main 的实现、#1014 与 #1015 的文件相对 main 无差异。那些结论的命令在复核报告第 3.1 节，本文没有重跑。

---

## 第二部分：LocalOnly 收窄的实际影响清单

这是有意的产品行为，不是实现偏差。ADR-K1-02 规定：未知、缺失、过期或互相矛盾一律按 `LocalOnly`；`LocalOnly` 是出口约束，没有本地模型就返回 unavailable，不回退远端（`docs/adr/ADR-K1-02-session-dataflow-security-modes.md:15`、`:53`）。它删掉的是 main 上已经在跑的回退。

main 上 `/api/v1/chat` 的注释和调用是（`origin/main:rust/apps/agent24d/src/routes.rs:559-565`）：

```text
// Default profile: shareable + simple → local-first tier order, so everyday
// chat prefers the on-device model and only falls back outward (D2).
.complete_served(TaskProfile::default(), &request, &cancel)
```

`TaskProfile::default()` 是 `Privacy::Any` + `Complexity::Simple`（`router.rs:58-77`，`Privacy` 与 `Complexity` 的 `#[default]`）。`Any` + `Simple` 的档位顺序是 Local、Lora、Remote（`571190cb:router.rs:87-88`）。Remote 因此是回退目标。

合入后同一处改为按 run 取政策（`571190cb:routes.rs:559-568`）。agent loop 每轮同样改道（`lib.rs:1721-1722`）。政策来源如下，四条路径的结果都是 `Privacy::LocalOnly`：

| 路径 | 为什么是 LocalOnly | 位置 |
|---|---|---|
| agent loop（主对话 /runs） | 入口写入 `SourceRef::user_input`，模式固定 `LocalOnly`。一条 LocalOnly 标签让整个快照成为 LocalOnly。 | `lib.rs:1545-1550`；`source_policy.rs:111-124`、`:199-205`；`merge_source_policy` 在 `lib.rs:49-56` 把 LocalOnly 写成 `Privacy::LocalOnly` |
| `POST /api/v1/chat` | 这个 run id 是临时的，没有插入 `runs`，也没有写标签。`run_policy_snapshot` 对零行返回 fail-closed 的 LocalOnly。查询出错同样落到 LocalOnly。 | `routes.rs:498`、`:559-568`；`source_policy.rs:307-313`、`:199-205`；`lib.rs:480-492` |
| explorer | 有 store 时读同一快照；读失败或没有 store 都是 LocalOnly。 | `subagent.rs:215-229` |
| 会话压缩 | 不读 run 政策，直接把 `SourceMode::LocalOnly` 交进去。接口没有 run id。 | `lib.rs:161-166`、`:237-238` |

`LocalOnly` 的档位顺序只有 `Local` 和 `Lora`（`router.rs:85-86`）。路由结果为空时的错误字符串是 `no local provider available for a local-only task`（`router.rs:424-428`）。有本地档但全部 `Unavailable` 时，错误是 `all routed providers unavailable [...]`（`router.rs:456-459`），仍然不会去碰 Remote。

### 判定：什么 URL 算本地

生产 daemon 的路由器是 `ModelRouter::from_env()`（`rust/apps/agent24d/src/server.rs:1514`）。它只注册两个 provider：

| 环境变量 | 缺省 | 档位 |
|---|---|---|
| `OMLX_URL` | `http://127.0.0.1:8088` | `env_local_tier(&omlx_url)` |
| `OLLAMA_URL` | `http://127.0.0.1:11434` | 同一函数 |

位置：`router.rs:285-299`、`:339-347`。注释里写的「daemon 另加 remote/lora provider」（`router.rs:282-284`）在 `server.rs` 的生产装配里没有对应代码；`ModelRouter::from_env` 的调用点只有 `server.rs:1514`（另有测试）。`ProviderRegistry::from_env`（`rust/crates/agent24-models/src/lib.rs:707-728`）仍把 Ollama 硬编码成 `127.0.0.1:11434` 且不做 loopback 判定，但 daemon 的聊天和 run 不走它。

`env_local_tier`（`router.rs:171-185`）要求下面三个 URL 全部通过 `is_loopback_url`，才标 `Local`：原 URL、`{url}/v1/chat/completions`、`{url}/v1/models`。否则打日志 `provider URL {url} is not loopback — labeling Remote so LocalOnly tasks won't route to it`，并标 `Remote`。标成 Remote 的 provider 仍然注册着，只是 LocalOnly 任务选不中它。

`is_loopback_url`（`router.rs:145-164`）的规则：

1. 用 `reqwest::Url`（WHATWG）解析。解析失败 → 不是 loopback。
2. scheme 必须是 `http` 或 `https`。`file://` 等不是。
3. 主机是 IP 时，调用 Rust `IpAddr::is_loopback()`：
   - IPv4：第一个八位组是 127，即 `127.0.0.0/8`（本机 rustc 1.95.0 源码 `library/core/src/net/ip_addr.rs:702-704`：`self.octets()[0] == 127`）。`127.0.0.1` 算，`127.0.0.2` 也算。
   - IPv6：只等于 `::1`（同文件 `:1412` 的 `LOCALHOST`，以及 `:1532-1534`）。
4. 主机不是 IP 时，只有与 `localhost` 忽略大小写完全相等才算。其它主机名一律不是，包括带尾点的 `localhost.`、`host.docker.internal`、`*.local`、机器名。

内网地址不算本地。路由器不调用 `is_private()`。RFC1918（`10.0.0.0/8`、`172.16.0.0/12`、`192.168.0.0/16`）的第一个八位组不是 127，`is_loopback()` 为 false。单测已经把 `http://192.168.1.50:8088` 锁成 Remote（`router.rs:880`、`:886`）。

同文件 `:856-887` 的 `env_local_tier_uses_the_http_clients_own_parser` 把边界锁进测试。合入后会被标成 **Local** 的例子：

- `http://127.0.0.1:8088`
- `http://localhost:8088`、`http://LOCALHOST:8088`
- `https://[::1]:8443`
- `http://user:pw@127.0.0.1:8088`（userinfo 不影响主机）
- `http://0x7f000001:8088`（WHATWG 归一成 `127.0.0.1`）

会被标成 **Remote** 的例子：

- `http://192.168.1.50:8088`（局域网）
- `http://0.0.0.0:8088`（未指定地址，不是 loopback）
- `http://[::ffff:127.0.0.1]:8088`（IPv4-mapped，`Ipv6Addr::is_loopback()` 只认 `::1`）
- `http://localhost.:8088`（尾点）
- `https://inference.example.com`
- `file:///tmp/x`、空串、解析失败的字符串
- `http://evil.example\@127.0.0.1:8088`（反斜杠在 WHATWG 里结束 authority，主机是 `evil.example`）

`10.x` / `172.16.x` / 域名网关没有逐条出现在这个测试里。它们走同一谓词：不是 `127.0.0.0/8`、不是 `::1`、也不是精确的 `localhost`，结果是 Remote。

### 哪些部署配置合入后会失效

两个 URL 都会被注册。失效的是「LocalOnly 任务还能不能用这个端点」，以及「路由器里有没有 Remote 档」。

| 配置 | 合入后 |
|---|---|
| `OMLX_URL` 与 `OLLAMA_URL` 都缺省，或都指向 `127.0.0.1` / `localhost` / `::1` | 两档都是 Local。主对话仍走本机。这是本机的情况，见下节。 |
| 只有一个 URL 指向非 loopback，另一个保持 loopback 且进程真的在听 | 非 loopback 那个被标 Remote，主对话、chat、explorer、压缩都不会再落到它。loopback 那个仍可服务。main 上本地档宕机时会继续试 Remote；合入后本地档 `Unavailable` 即失败，错误为 `all routed providers unavailable [...]`。 |
| 两个 URL 都是非 loopback（局域网 IP、`0.0.0.0`、域名、Tailscale 主机名、`host.docker.internal`、远端 OpenAI 兼容网关） | 两个 provider 都是 Remote。LocalOnly 的路由列表为空，立即 `no local provider available for a local-only task`。`/api/v1/chat` 返回 503 `provider_unavailable`。agent run 失败。explorer 工具失败。压缩停在下面说的条件上。 |
| 任一 URL 非 loopback，因而 `has_remote_tier()` 为 true | `documents` 模块工具被守卫挡住，不通告（`module_tool_runtime.rs:172-174`）。把那个 URL 改回 loopback（或删掉覆盖、回到缺省 loopback）之后，守卫随 Remote 档消失而放开。同意门仍然单独生效。 |
| 模块 manifest 写了 `model_access = remote_allowed` 的模块回调 | 这条路径仍是 `Privacy::Any`（`model_callback.rs:497-500`），可以选中 Remote 档。它不受本次「主对话收窄」的影响。缺省 manifest 仍是 LocalOnly（`domain/lib.rs:828-829`）。 |
| `~/.pi/agent/models.json` 里的 provider | agent24d 不读这个文件。改它不会改变 daemon 的路由。 |

没有第三个「云 API」环境变量。想继续用远端模型，不能靠多设一个 key；`from_env` 的两个 URL 就是全部。

### 本机当前配置

本机不受这次收窄的影响，前提是启动 daemon 的环境没有把 `OMLX_URL` / `OLLAMA_URL` 指到非 loopback。本次核对看到的是：

- 本会话环境里 `OMLX_URL`、`OLLAMA_URL`、`DEFAULT_MODEL` 都未设置（对当前进程环境按这三个名字取值，三个都缺席）。缺省因此是 `http://127.0.0.1:8088` 和 `http://127.0.0.1:11434`，两者都是 Local。
- 在 `~/Library/LaunchAgents`、`~/Library/LaunchDaemons`、`/Library/LaunchAgents`、`/Library/LaunchDaemons`、`~/.agent24` 和本 worktree 里按文件名检索 `OMLX_URL` / `OLLAMA_URL`，没有命中。没有扫整个家目录，也没有读密钥。
- `lsof -nP -iTCP:8088 -sTCP:LISTEN` 显示 `127.0.0.1:8088` 在听（进程 `python3.1`，PID 27391）。`curl --max-time 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:8088/v1/models` 返回 `401`。未携带密钥。这说明端点绑在 loopback 上并且有 HTTP 服务；`401` 只说明未带密钥，不说明模型名或密钥内容。
- `~/.pi/agent/models.json` 的 `providers.omlx.baseUrl` 是 `http://127.0.0.1:8088/v1`，主机是 `127.0.0.1`，按上面的谓词是 loopback。同文件 `providers.deepseek.baseUrl` 是 `http://127.0.0.1:8791`，也是 loopback，但是 Pi 自己的 provider。agent24d 的 `from_env` 不会注册它。apiKey 字段没有读取进本文。

因此：用缺省环境变量启动的 agent24d，主对话继续打 `127.0.0.1:8088` 的 oMLX。局域网回退本来就不在本机这条配置里。

### 会话压缩被固定成 LocalOnly 之后会怎样

压缩器是 `RouterSummarizer`，daemon 在 `server.rs:522-527` 把它套在会话记忆上。`SessionMemory::new` 使用 `CompactionPolicy::default()`（`rust/crates/agent24-agent/src/session_memory.rs:82`）。默认阈值是：verbatim 尾部超过 40 条就压缩，保留最近 20 条，摘要最长 4000 字符（`rust/crates/agent24-memory/src/session.rs:40-47`）。

`remember` 先把这一轮写进事件日志，再做压缩。压缩失败只打日志，不让已经完成的回答失败（`session_memory.rs:407-420`）：

- 成功：`Ok(Ok(()))`，摘要写入。
- 摘要器返回错误：日志 `session {sid} compaction failed; originals retained: {err}`，原文留在事件日志。
- 超时：日志 `session {sid} compaction timed out; originals retained`。
- 写日志已经用完 30 秒预算（`lib.rs:136` 的 `MEMORY_WRITE_BUDGET`）时，根本不调用 `compact`（`session_memory.rs:412-414`）。

摘要器内部（`lib.rs:237-243`）固定 LocalOnly。下面任一情况都会让 `summarize` 返回 `Err`，从而走进上面的失败日志：

- 两个 URL 都是 Remote，或唯一的 Local 档不可用：`ModelError::Unavailable("no local provider available for a local-only task")`（`router.rs:424-428`）。
- Local 档都 `Unavailable`（进程没起来、连接被拒）：`all routed providers unavailable [...]`（`router.rs:456-459`）。
- 模型返回空摘要：`summarizer returned an empty summary`（`lib.rs:245-246`）。

`compact` 只有在尾部长度大于 `max_recent`（默认 40）时才调用摘要器（`session_memory.rs:459-463`）。短会话看不出变化。

下一次加载上下文时，若尾部超过 `max_recent * 4`（默认 160）条，会从提示里丢掉最老的消息，并打错误日志；事件日志里的原文还在（`session_memory.rs:17`、`:279-291`）。对话回答本身已经在 `remember` 的压缩步骤之前落盘，所以用户仍能看到这一轮回复；变长的是后续轮次送进模型的未压缩尾部，直到有一个 Local 档能完成摘要。

桌面聊天因为每次都带 `session_id`（`Chat.tsx:73`），走的是 `chat_remember_turn` → `remember_exchange`（`lib.rs:772-779`），和 agent loop 共用这段压缩。main 上的注释写明当时故意与聊天共用 `TaskProfile::default()`，并警告强制 LocalOnly 会让「只有远端模型」的部署悄悄停止压缩（`origin/main:rust/crates/agent24-agent/src/lib.rs:153-156`）。合入后的注释改成了 fail closed（`571190cb:lib.rs:163-166`）。

### 迁移建议（部署方要改什么）

这次没有「允许这块对话使用远端模型」的开关。`CloudAuthorized` 没有生产写入点（见第一部分）。在那个入口出现之前，主对话、`/api/v1/chat`、explorer 和会话压缩都不会选择 Remote 档。

要让现有对话继续工作，部署方改的是 URL 的主机，让 `env_local_tier` 把它标成 Local：

1. `OMLX_URL` 使用 `http://127.0.0.1:<port>`、`http://localhost:<port>` 或 `http://[::1]:<port>`。`OLLAMA_URL` 同样。不要用局域网 IP、`0.0.0.0`、机器名、`host.docker.internal`、`localhost.`（尾点）或 `::ffff:127.0.0.1`。
2. 模型进程若在另一台机器上，daemon 看到的 URL 主机仍然必须是上面三种 loopback。常见做法是在跑 daemon 的机器上做 SSH 转发或本机反向代理，把 loopback 端口转到那台机器。这样做之后，路由器会把该端点当成 Local，LocalOnly 流量会发过去。这和 ADR-K1-02:53「tier 必须是可信的本机端点」的语义不一致：隧道只骗过主机名检查，不构成获准云处理。把它当作维持服务的临时接法，并在后续 CloudAuthorized 入口落地后拆掉。
3. 只需要本机 oMLX、不需要局域网 Ollama 时：不要设置 `OLLAMA_URL`，或把它设回 `http://127.0.0.1:11434`。留一个局域网 `OLLAMA_URL` 会让 `has_remote_tier()` 为 true，`documents` 工具继续被挡住，而聊天也回退不到那台 Ollama。
4. 不要通过修改 `~/.pi/agent/models.json` 来修 agent24d。那个文件不是 daemon 的路由配置。
5. 升级前复制 state 目录里的 SQLite 文件。新 daemon 第一次 `Store::open` 就会应用 0015–0017。旧二进制对升级后的文件会 `VersionMissing`，不会自动降级。

模块回调若必须继续打远端，保持该模块 manifest 的 `model_access` 为 `remote_allowed`。这只覆盖模块自己的模型回调，不覆盖桌面聊天和 agent loop。

---

## 第三部分：法律文本流程修正方案

### 核实

`62218c312db184dc0763b2c7b4eb55bd3dff1333` 是实现提交，不是法律专提交。

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

`git merge-base --is-ancestor 62218c31 571190cb` 与 `git merge-base --is-ancestor 62218c31 origin/ab/kernel` 都成功。`git log --oneline 62218c31..571190cb -- docs/laws/APPROVAL.md` 为空：这之后没有提交再改过该法律文件。

相对 main，`docs/laws/` 只有这一处差异：

```text
git diff --name-status origin/main 571190cb -- docs/laws
M  docs/laws/APPROVAL.md

git rev-parse origin/main:docs/laws/APPROVAL.md
16db448916d984277314e66ea54eaa80e531e059
git rev-parse 62218c31^:docs/laws/APPROVAL.md
16db448916d984277314e66ea54eaa80e531e059
git rev-parse 62218c31:docs/laws/APPROVAL.md
244429b037bb4a11de4ed0794823ea4ef6da139b
git rev-parse 571190cb:docs/laws/APPROVAL.md
244429b037bb4a11de4ed0794823ea4ef6da139b
```

main 的法律文件与 `62218c31` 的父提交是同一个 blob。release 树上的文件与 `62218c31` 的结果是同一个 blob。L-APPR-5 在 release 树上从 `docs/laws/APPROVAL.md:87` 开始。改动把「`write_local` 类工具 MUST 永远逐次询问」收成：`exec` 仍永远逐次询问；`write_local` 仍逐次询问，但第一方可逆草稿在一组宿主核验都成立时 MAY 免逐次询问。同节写明：当前工具分发路径尚未接入该核验结构，缺失时按普通 `WriteLocal` 逐次审批。这与 `server.rs:1601-1604`（这条装配路径不提供草稿核验）一致，所以合入本身不改变现在的逐次审批行为。

PLAN 的两条要求：

- `571190cb:docs/agent/PLAN-KERNEL-K1.md:24`：K1 ADR 若确立新的长期隐私/外传不变量，先单独提法律文件，按 laws README 闭环；不得在实现 PR 中顺手改法律。
- 同文件 `:84`：法律变更必须单独 PR；实现 PR 点名符合的 L-APPR、L-MEM、L-CTX 及新增隐私法律。

ADR 把实质决定和流程分开写了：

- `docs/adr/ADR-K1-03-enable-authorization-trust-revocation.md:120`：jason 于 2026-10-08 授权为第一方 Documenting 的可逆草稿设窄例外；该决定同步修订 L-APPR-5。同文件 `:118-127` 是 K1-6a.5 补遗，列出宿主必须同时核验的条件。
- 同文件 `:105`：LocalOnly、授权撤销等若要成为新法律，必须另提独立 PR；本 ADR 不代替法律流程。

因此，2026-10-08 的授权是「这条例外的内容可以成立」。它没有满足 PLAN `:84` 的「法律变更单独一个 PR」。ADR 自己写了它不代替法律流程。

同一提交里的另外四个文件是实现和 ADR，不是法律正文：

| 文件 | 角色 |
|---|---|
| `docs/adr/ADR-K1-03-enable-authorization-trust-revocation.md` | 记录授权和补遗。ADR 不是 `docs/laws/`。 |
| `docs/laws/APPROVAL.md` | 唯一的法律正文改动。 |
| `rust/crates/agent24-domain/src/lib.rs` | 实现。 |
| `rust/crates/agent24-domain/src/tool.rs` | 实现。 |
| `rust/crates/agent24-policy/src/consent_gate.rs` | 实现（`ConsentGate` 的草稿资格）。 |

### 建议：在 main 上先合一个只含法律文件的 PR，不改写 62218c31

采用这个顺序，因为 PLAN `:84` 要求法律变更以单独 PR 进入，而 `62218c31` 已经是 `origin/ab/kernel` 和 `571190cb` 的祖先。改写该提交需要 force-push `ab/kernel`，已有评审所指向的提交哈希会变。发布约束也不允许动 `ab/*`、force-push 或 main。

具体步骤（本文没有执行）：

1. 从当前 `origin/main`（`72c2a580`）拉一条新分支。不要用 `ab/*`，不要基于 `merge/kernel-to-main-20261016` 再改法律文件。
2. 只取出法律结果，不取出同提交的 Rust 和 ADR：

   ```text
   git checkout 62218c31 -- docs/laws/APPROVAL.md
   ```

   提交前 `git diff --cached --name-only` 必须只有 `docs/laws/APPROVAL.md`。结果 blob 应为 `244429b037bb4a11de4ed0794823ea4ef6da139b`。
3. PR 标题按法律变更写，例如「docs(laws): L-APPR-5 —— 第一方可逆草稿的逐次审批例外」。正文贴上 `git show 62218c31 -- docs/laws/APPROVAL.md`，并写明：文本最初出现在实现提交 `62218c31`，本 PR 是补上的独立法律评审；jason 2026-10-08 的实质授权见 ADR-K1-03:120；本 PR 不包含 `consent_gate.rs`。
4. 这个 PR 先于 #1022 合入 main。合入后：

   ```text
   git diff origin/main origin/merge/kernel-to-main-20261016 -- docs/laws/APPROVAL.md
   ```

   应为空（两边都是 `244429b0…`）。#1022 的 diff 不再携带法律变更。历史里 `62218c31` 仍同时含有代码和法律，但 main 上第一次引入该法律文本的 PR 是这个新 PR。
5. #1022 不要再改 `docs/laws/APPROVAL.md`，也不要在 release 分支上把该文件还原成 `16db4489…`。
6. 法律 PR 合入之后，另用一个只改 `docs/agent/PLAN-KERNEL-K1.md` 的文档 PR，在 `:24` 和 `:84` 各加一句记录：L-APPR-5 的文本曾夹在 `62218c31`，已由该法律 PR 单独评审后进入 main；以后的法律变更仍须单独 PR。这句话是流程记录，不要和内核实现放在同一个提交。

这样拆的理由：

- main 与 `62218c31^` 的 `APPROVAL.md` 是同一个 blob，所以从 main 检出 `62218c31` 的该文件是一次干净的法律 diff，没有夹带 0001–0014 时代之后的其它法律编辑。
- 运行行为不依赖这次先合法律。分发路径尚未把草稿核验接上（法律文本自身的「今天靠什么保证」段，以及 `server.rs:1601-1604`）。先有法律、后有实现，中间不会放开 `write_local`。
- ADR-K1-03:120 已经写「同步修订 L-APPR-5」。先把修订合进 main，再合 #1022，main 上不会出现「ADR 声称已经修订、法律文件仍是旧文」的窗口。

### 另外两个做法为什么不采用

**回退 `62218c31` 再在新 PR 里重放法律。** 祖先提交不能改。若只在 release 分支上把 `APPROVAL.md` 还原成 `16db4489…`，#1022 会把 ADR-K1-03:120 和一份旧的 L-APPR-5 一起带进 main，直到下一个 PR 再把法律改回来。窗口里的 main 自相矛盾。先合法律 PR 没有这个窗口。

**只在 PLAN 里登记例外、让法律继续只随 #1022 进入 main。** 这在 jason 书面接受「这次不单独评审法律 diff」时才成立。ADR-K1-03:105 写明 ADR 不代替法律流程，所以 2026-10-08 的实质授权本身还不是那个接受。未得到该接受之前，按上一节做独立 PR。

### 涉及的精确文件与提交

| 项 | 值 |
|---|---|
| 夹带法律的实现提交 | `62218c312db184dc0763b2c7b4eb55bd3dff1333` |
| 该提交的父提交 | `6993613ffab301f70c9a9c0d00c808966d71c6f9` |
| 作者 | `jhfnetboy <jhfnetboy@gmail.com>`，2026-10-08 13:59:46 +0700 |
| 提交说明 | `fix(kernel): K1-6a.5 —— 收窄可逆草稿写入审批例外` |
| 法律文件 | `docs/laws/APPROVAL.md`（L-APPR-5，release 树自第 87 行起） |
| 改前 blob（main 与父提交） | `16db448916d984277314e66ea54eaa80e531e059` |
| 改后 blob（该提交与 `571190cb`） | `244429b037bb4a11de4ed0794823ea4ef6da139b` |
| 同提交中的非法律文件 | `docs/adr/ADR-K1-03-enable-authorization-trust-revocation.md`、`rust/crates/agent24-domain/src/lib.rs`、`rust/crates/agent24-domain/src/tool.rs`、`rust/crates/agent24-policy/src/consent_gate.rs` |
| 规则出处 | `docs/agent/PLAN-KERNEL-K1.md:24`、`:84`（只在 `571190cb`，不在 `origin/main`） |
| 实质授权出处 | `docs/adr/ADR-K1-03-enable-authorization-trust-revocation.md:105`、`:118-127` |
| 已含该提交的分支 | `origin/ab/kernel` @ `9d1062cc`；`origin/merge/kernel-to-main-20261016` @ `571190cb` |
| 发布 PR | #1022，base `main`，head 为上述 release 分支 |

---

## 本文没有核实的部分

- 没有在一份升级后的用户 SQLite 上启动只含 0001–0014 的旧二进制，去观察 `VersionMissing` 的实际错误文本。不可回退的结论来自 sqlx 0.8.6 源码和仓库里的调用点。
- 没有跑 `cargo test`，没有启动合入后的 daemon，没有在浏览器里点桌面聊天。
- 没有扫描整个家目录。只确认了本会话环境、上述 LaunchAgents/LaunchDaemons、`~/.agent24` 和本 worktree 里没有 `OMLX_URL` / `OLLAMA_URL`。若某个未检索的启动脚本设置了非 loopback URL，结论要按那个值重算。
- 没有读取任何 API key。`models.json` 的 `apiKey` 只确认字段存在。
- 没有重跑复核报告第 3.1 节的 blob 对比（`PASSTHROUGH_VARS`、`state_dir()`、#1014、#1015）。
- 复核报告问 1（撤销失败仍返回 2xx）没有在这里复现实验，也没有改代码。
- `10.0.0.0/8` 与 `172.16.0.0/12` 没有单独的单元测试行。它们的 Remote 结论是从 `Ipv4Addr::is_loopback`（第一个八位组等于 127）和 `is_loopback_url` 的主机名分支推出来的；`192.168.1.50` 有直接测试。
