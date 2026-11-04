# 能力登记表

ADR-038 D9 与 ADR-039 的能力 id 登记表。能力 id 的字符集是 `[a-z0-9][a-z0-9-]{1,31}`（长度 2–32），全局唯一。锁里的 `provides[].id`、随仓库提交的 `domain-os.yml` 里的 `provides[].id`，以及 `GET /api/v1/components` 里不是 MCP 自动生成的能力 id，都要在下表出现。

漂移校验是 `agent24d` 的 `registry_lists_every_capability_id_the_components_emit`：它读本文件表格第一列、`rust/**/*.lock.json` 的 `provides[].id`、仓库里带 `provides:` 的 `domain-os.yml`，以及一次不挂额外模块的 `GET /api/v1/components`。

| 能力 id | 中文名 | English | ไทย | 提供方 | 形态 | 契约来源 | 状态 | 登记日期 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `model-gateway` | 本地模型网关 | Local model gateway | เกตเวย์โมเดลภายในเครื่อง | idoris | 常驻 HTTP（resident） | `rust/crates/agent24-idoris/idoris.lock.json` | 已接入 | 2026-11-03 |
| `nostr-dm` | Nostr 私信 | Nostr DM | ข้อความส่วนตัว Nostr | hyphae | 常驻 CLI（resident） | `rust/crates/agent24-comm/hyphae.lock.json` | 已接入 | 2026-11-03 |
| `nostr-group` | Nostr 群组 | Nostr group | กลุ่ม Nostr | hyphae | 常驻 CLI（resident） | `rust/crates/agent24-comm/hyphae.lock.json` | 已接入 | 2026-11-03 |
| `nostr-identity` | Nostr 身份 | Nostr identity | ตัวตน Nostr | hyphae | 常驻 CLI（resident） | `rust/crates/agent24-comm/hyphae.lock.json` | 已接入 | 2026-11-03 |
| `nostr-relay` | Nostr 中继 | Nostr relay | รีเลย์ Nostr | hyphae | 常驻 CLI（resident） | `rust/crates/agent24-comm/hyphae.lock.json` | 已接入 | 2026-11-03 |
| `documents` | 文档 | Documents | เอกสาร | documents | 常驻进程外（resident） | `rust/apps/agent24-documents/domain-os.yml`（免锁，ADR-038 D2；能力 id 见 ADR-040） | 已登记。本切片不改 Documents 的清单输出 | 2026-11-03 |

Hyphae 的锁只带 id，没有三语名。上表的中文、英文、泰文是登记表补上的显示名，不回写锁（ADR-038 §7.4：锁按发布字节复制）。

## 领域 OS 与进程内 DomainModule

领域包（Sin90、Cos72 以及以后的进程内模块）是组件。清单写在它的 `domain-os.yml` 里，可选字段 `provides`。没有这个字段就是「还没有声明能力」，不是从包名编一个 id。`requires.kernel` 来自清单里已有的 `kernel_capabilities`，`requires.operations` 在这条上是空的。

`GET /api/v1/components` 给每个非内置模块一条 resident 记录。`detail.binding` 是 `in_process` 或 `package`（只给 host）。进程内模块没有自己的进程：挂上之后是 `ready`。进程外模块跟监管进程的状态走，和 Documents 同一张 D5 映射；用户关掉的是 `stopped`，原因 `disabled`。

保留 id 与重名：`domain-os.yml` 的 `provides[].id` 不能是 `mcp-*`、内置组件 id，也不能是上表里提供方不是本模块的 id。能力 id 全局唯一：两个用户包声明同一个普通 id 时，目录顺序里先出现的保留它，后一个被拒绝。

- 能力 id 违规：挂载时拒绝，`/api/v1/components` 里是 `failed`，`reason` 为 `reserved_capability_id`（保留）或 `duplicate_capability_id`（重复），不列任何 `provides`。被拒绝的模块不占 id。
- 用户包不能叫内置组件的名字（`idoris`、`hyphae`、`knowledge`、`open-design`），否则用户可以靠包名冒充登记表上的提供方。这样的包在准入阶段被拒绝，`reason` 为 `reserved_component_name`。它仍出现在 `/components`，组件 id 与内置行重名，按 `mcp-bad-<hash>` 规则改写，原包名在 `detail.package`。例外是 `documents`：它由桌面从安装包装入，可能与 daemon 不是同一次构建，所以照常准入、不比对 digest；但它声明的每个能力 id 必须在编译进来的 Documents D9 清单（`component.json`）的 `provides` 里，超出的按 `reserved_capability_id` 拒绝。
- 禁用的包同样被拒绝（准入在启停之前）。进程内模块要先 `build()` 才拿得到 manifest，而禁用的模块不调 `build()`，所以进程内模块的能力 id 检查只在启用时进行。
- 代码里的提供方对照表是 `component_manifest::REGISTERED_PROVIDERS`，由测试 `registered_providers_match_registry_md` 与本表的「提供方」列对账。

内置 id（`idoris`、`hyphae`、`documents`、`knowledge`、`open-design`）不从模块报告再列一次。Documents 的条目仍由它自己的映射负责。

## MCP

MCP server 来自用户的 `~/.agent24/mcp.json`，没有锁，不进上表（ADR-038 D1、D9）。每个已配置的 server 自动得到一条组件，形态 `mcp`。

能力 id 规则：

1. 把 server 名变成小写。
2. 不是 `[a-z0-9-]` 的字符换成 `-`。
3. 去掉首尾的 `-`。
4. 剩下的一段叫 body。body 为空，或长于 28，就不是合法能力 id。这种 server 仍然连接，工具照常注册，名字仍是 `mcp_{server}_{tool}`。它不写入 `provides`。组件 id 用 `mcp-bad-` 加一个稳定的 8 位十六进制，避免和合法 id 抢同一个名字。这个备用 id 也占住名字：后面再出现同名的合法 id，按重复处理。
5. 否则能力 id 是 `mcp-` 接上 body。组件 id 与能力 id 相同。每个 server 只提供这一项能力。
6. 两个 server 规范化成同一个 id 时，按 `mcp.json` 里的键序，先写的保留该 id 和 `provides`。后一个仍然连接并注册工具，组件 id 用 `mcp-bad-<hash>`，`reason` 为 `duplicate_capability_id`，不写 `provides`。用户关掉的后一个不拉起进程，状态是 `stopped`，reason 仍是 `duplicate_capability_id`。不按字典序。

`provides[].tools` 是现有的限定名 `mcp_{server}_{tool}`，来自该 server 的 `tools/list`。工具名不改。还没拿到列表（关掉、启动失败）时 tools 为空。名字不合法或能力 id 重复时，连上了的工具已经注册，只是不写入 `provides`。启动成功之后进程自己退出，列表保留，状态改掉。

`GET /api/v1/components` 列出的组件 id 不重复。和已经出现的 id 撞车时（例如领域包名叫 `mcp-files`，MCP server 又叫 `files`），后出现的那条改用 `mcp-bad-<hash>`。它原来没有 reason 时记 `duplicate`。这不改能力 id：MCP 那条的 `provides` 仍可以是 `mcp-files`。

D5 只用 MCP 子集：`installed / starting / ready / failed / stopped`。这条接入里的对应关系：

| 发生了什么 | 状态 | `reason`（只给 host） |
| --- | --- | --- |
| `enabled: false` | `stopped` | `disabled` |
| 启动超时，或总预算用完，且取消令牌没有触发 | `failed` | `start_timeout` |
| 进程起不来 | `failed` | `spawn_failed` |
| 握手或 `tools/list` 失败 | `failed` | `handshake_failed` |
| 已经 `ready`（或仍在 `starting`）时进程自己退出 | `failed` | `process_exited` |
| 本进程取消它（daemon 关闭，或取消发生在握手、`tools/list`） | `stopped` | `stopped` |

`protocolVersion` 只记在 host 的 `detail` 里，不比较 major.minor（D3 对 MCP 不适用）。`reason` 里不放路径。配置文件解析失败则整份忽略，不产生组件行。
