# ADR-040 技能加载器 v1 实施方案（第 2 版）

- 日期：2026-11-12
- 状态：已拍板（§8）。本版整篇替换同文件第 1 版（`4897a4f5e`）。只写文档，没有改代码。
- 依据：[ADR-040](../../adr/ADR-040-skill-contract-v1.md)（已接受，零代码）；上位 [ADR-039](../../adr/ADR-039-concept-model.md) §2.3、§3 规则 3、§6；[ADR-038](../../adr/ADR-038-component-contract-v1.md) D8、D9；[SPEC-SKILL-DISTRIBUTION](../../specs/SPEC-SKILL-DISTRIBUTION.md)；[REGISTRY.md](../../capabilities/REGISTRY.md)；[ADR-K1-03](../../adr/ADR-K1-03-enable-authorization-trust-revocation.md)；[ADR-K1-02](../../adr/ADR-K1-02-session-dataflow-security-modes.md)；[ADR-041](../../adr/ADR-041-inbound-events-admission-and-work-items.md)；[ADR-045](../../adr/ADR-045-route-naming.md)；issue #1037。
- 施工基线是 **C9 合入之后的 `main`**。本工作树还没有 C9 的方案文件，也没有 C9 的代码。C9 的目标语义仍以 ADR-038 D7 为准（`:288-295`）：组件 `requires` 接入授权。技能加载器不实现这套授权。
- 与 SPEC、#1037 冲突时，以已接受的 ADR-040 为准。冲突的准确说法写在 §3。不改 SPEC 原文。

## 1. 一页结论

技能今天仍然不能加载。ADR-040 把技能定义成一份带版本的 `SKILL.md`：声明依赖哪些已登记能力，本身不持有权限；安装、激活、执行分开；激活前要过评测和人的审批；改动可回滚。

v1 只做两条来源：**用户写在数据目录里的技能**，以及 **导入的外部正文**。路径是：校验清单、把不可变修订放进数据库、用同一个纯函数渲染技能段、用 `exact` / `contains` 做文本一致性检查、人在桌面点一次确认后把「当前」指针拨到该修订、用户在这个会话里点名的技能把正文注入这一次 run。导入的正文按外部文字处理：注入时写入来源标签，分类是 `Unknown`、处理方式是 `LocalOnly`，激活不把它升成可信文字。用户自己写的技能标 `Ordinary`。注入不增加工具，也不代替工具审批。

v1 不做分发来源、不做内置技能、不做默认提供者表、不做版本差异界面、不做主动技能、不做模型打分、不做从会话自动起草、不执行技能目录里的脚本。同一能力有多个提供者时直接拒绝并提示。

## 2. v1 范围

### 要做

- **D1 清单，来源只有 `user` 和 `imported`。** `name`、`version`、`description`、`requires`、`evals` 必填，缺任一项拒绝，报 `skill_manifest_invalid`。`requires` 只能是 `REGISTRY.md` 已登记的能力 id，或 `mcp-<server>`。不新增 `capabilities` 字段。原文：`docs/adr/ADR-040-skill-contract-v1.md:22-41`。
- **D2 运行前解析依赖。** 同一能力零个提供者：拒绝并提示登记表里的中文名和首个提供者。多于一个：拒绝并提示，不自动挑，也不提供默认选择（§4.6）。一次 run 在准入时钉死不可变修订。技能里的任何字段都不能放行工具。原文：`:43-48`。
- **D3 三阶段。** 安装后是 `installed`，不可运行。激活由服务端重跑评测（或用户按服务端给出的失败项逐条接受），再经桌面确认，然后指针指向这一对修订。执行只跑指针指向的修订。零依赖技能同样走三阶段。原文：`:50-55`。
- **D4 的 v1 子集。** 修订不可变，身份是 `revision_id`。回滚把指针拨回一条历史激活组合，本身要桌面确认，只影响之后的执行。原文：`:57-62`。v1 没有内置和分发，因此不做「旧默认 → 新默认」两份差异。
- **D5 的 v1 子集。** 评测文件是 jsonl，每条 `{ input, expect, judge }`，`judge` 只实现 `exact` 和 `contains`。界面把这件事标成「文本一致性检查」。评测用例的改动与正文分开审批。原文：`:64-68`。`local_model` 与自动生成草案不在 v1（`:66-69`）。
- **D7 技能栏要显示的字段。** 名称、版本、来源、依赖的能力（缺的标红）、模式（固定「手动」）、文本一致性结果、「评测集相对当前指针是否变化」。确认摘要另含内容哈希前 8 位。用户看不到 `base_hash`。原文：`:77-79`。

### 本版从第 1 版砍掉的

- 分发来源扫描（`distro`）、内置来源扫描（`bundled`）、`proposed`。
- 版本差异与「保留我的 / 用新默认」（第 1 版 SL-13）。
- 内置技能版本递增 CI（第 1 版 SL-14）。仓库里还没有要守的 `skills/`。
- 默认提供者表、`PUT` 选择提供者、能力中心的能力栏（第 1 版 SL-12）。同一能力多个提供者时拒绝并提示。
- 把技能路由放在 `/api/v1/capabilities/` 下。那个前缀是能力令牌（`route_policy.rs:80-81`，ADR-045 §4.4）。

### ADR 已经写明不做的

- **D6 主动技能。** 「D6 主动技能 v1 不做」（ADR `:4`）。`mode: proactive`、`off | observe | auto`、入站 `inferred` 触发，都不实现。见到 `mode: proactive` 拒绝安装，错误码 `skill_proactive_unsupported`。
- **D5 `local_model` 与自动生成草案。** 不接 ADR-043 D4，也不做 #1037 的 S3。
- **D2 的只读上下文。** ADR `:48` 允许技能在步骤里引用会话、工作区、入站事件、工单作为只读上下文。v1 不做，见 §7。

## 3. 现状盘点与差距

核对日期 2026-11-12，本工作树 `docs/skill-loader-v1-plan` @ `4897a4f5e`。行号在 C9 合入后可能移动，开工时按符号再对一次，不按本表的行号硬编码。

| 需要的东西 | 现状 | 证据 |
|---|---|---|
| 技能加载器 | 没有。`rust/` 里 `skill` 只出现在 Documents 占位和测试读 ADR 的断言 | `documents_catalog.rs:51`、`:235-236`；`component_routes/tests.rs:1155-1163`、`:3278-3279` |
| 清单占位 | Documents 的 `skills` 必须是空数组，非空则清单非法 | `component.json:27`；`documents_catalog.rs:235-236` |
| 组件锁 | `deny_unknown_fields`，没有 `skills` 字段 | `agent24-components/src/lock.rs:12-41` |
| 能力 id 登记表 | 已有六条：`model-gateway`、四个 `nostr-*`、`documents`。MCP 不进表 | `docs/capabilities/REGISTRY.md:8-14`、`:34-45` |
| 数据目录 | `<data_dir>` 默认 macOS/Linux 为 `$HOME/.agent24`，可用 `AGENT24_DATA_DIR` 覆盖 | `agent24-protocol/src/data_dir.rs:1-12` |
| run 怎么组上下文 | 快照是：记忆召回、可选暂停通知、会话历史、用户这句话。没有技能正文 | `agent24-agent/src/lib.rs:2439-2506` |
| 用户输入怎么打标签 | 普通 run 用 `SourceRef::user_input`（`Ordinary`）。入站 run 用 `SourceRef::inbound_external`（`Unknown` + `LocalOnly`）。写入失败则 run 以 `source_policy_unavailable` 结束，不调用模型 | `source_policy.rs:266-292`；`lib.rs:2527-2549` |
| 来源种类 | `SourceKind` 已有 `UserInput`、`SelectedMaterial`、`MemoryRecall`、`SessionContext`、`ScheduleOrigin`、`ToolResult`。`SelectedMaterial` 的构造函数固定 `Unknown`，并带 `revision_digest`；生产路径没有调用点 | `source_policy.rs:60-81`、`:299-313` |
| 消息来源 | 闭集，SQL `CHECK` 只允许 `HostRecall`、`UserPrompt`、`SessionHistory`、`Assistant`、`ToolResult`、`HostSystem`。`HostSystem` 只用于记忆暂停通知。与 K1 标签是两套表 | `message_origin.rs:23-33`；`0029_run_message_origin_host_system.sql:7` |
| 入站起 run | `start_run_inbound` 的 `origin` 不在 `RunCreate` 里，外部无法伪造。`execute` 据此打 `inbound_external` | `lib.rs:1490-1509`、`:2533-2536` |
| 定时自唤醒 | `start_run_with_schedule` 把 `schedule_id` 记在 run 上。自唤醒提示按模型产出处理，不能授权个人记忆 | `lib.rs:1463-1471`、`:1237-1256` |
| 外发门 | 非 `Ordinary` 的来源会挡住外发，并让 peer 视图隐藏内容。已有入站对照测试 | `lib.rs:6653` 附近 `an_ordinary_run_is_user_input_and_an_inbound_run_is_unknown_and_blocked_from_egress` |
| 桌面代理 | 渲染进程只能走 `ALLOWED_PROXY_ROUTES`。提升级操作不在这张表里，走主进程专用 IPC | `apps/desktop/src/main/backend-proxy.ts:1-55` |
| 第一方提升 | `resolve_target` 的 `supported` 列表目前只有模块管理的四条和 iDoris 生命周期。不在列表里的提升级路由直接 `NotElevatedOperation` | `first_party.rs:498-520` |
| CLI 日常令牌 | 注册时把路由表里的操作几乎全部放进令牌，只排除 presence 通道和 `InboundWrite`，再加 `ModuleRoute` | `first_party.rs:374-384` |
| 能力中心界面 | 一页导航目录。不读组件 `provides`，没有技能栏 | `apps/desktop/src/renderer/pages/Capabilities.tsx:42-89`；`capabilities/registry.ts:8-23` |
| 存储迁移 | 最新是 `0035_inbound_cursor.sql`。C9 合入后编号会继续涨。本方案不预定下一个号 | `rust/crates/agent24-store/migrations/` |
| C9 | 本树未合入。组件 `requires.operations` 今天只被解析和校验 | `documents_catalog.rs:228-233`；ADR-038 `:288-295` |

差距就这一句：契约和登记表在，运行时没有技能对象。run 循环只认本体提示、记忆和已注册工具。

和旧文档的冲突（实现时以 ADR-040 为准）：

1. **SPEC 已经有「用户改过则不动」和哈希保护。** §3.1 的四行是：不存在则写入；来源版本更高则覆盖；相等或更低则不动；用户改过则不动，并提示有新版本（`SPEC-SKILL-DISTRIBUTION.md:79-86`）。§4 用 `.seeded.json` 里的哈希判断用户改过，版本号决定要不要升，哈希决定能不能升（`:107-117`）。第 1 版把 SPEC 说成「来源版本更高就覆盖、因此和 ADR 的不覆盖相冲突」，这句不准确。仍然有效的差别是：SPEC 在哈希相同且来源版本更高时**会覆盖写入用户目录**；ADR-040 D4 还要求用户在两份差异里选择，并且不自动合并（ADR `:60-61`）。v1 两边都不做：不扫描内置和分发，不把更高版本写入用户目录，也不做差异界面。
2. SPEC §5 把 Skill 说成「能力」（`:129`）。ADR-039 把技能和能力分成两个概念（`ADR-039-concept-model.md:40-48`）。
3. #1037 的 S3 要做自动起草。ADR-040 D5 写明 v1 不做（`:69`）。

## 4. 目标设计

### 4.1 两条来源

| origin | 输入 | 谁写入修订 | 注入时的分类 |
|---|---|---|---|
| `user` | `<data_dir>/skills/<name>/`，`<name>` 等于 frontmatter 的 `name` | 扫描器。本体不替用户改这些文件 | `Ordinary`，`LocalOnly` |
| `imported` | `POST /api/v1/skills` 的正文，或桌面主进程读完目录后提交的正文 | 导入 API。不写回用户目录 | `Unknown`，`LocalOnly` |

默认 `<data_dir>` 是 `~/.agent24`（`data_dir.rs:8`）。设了 `AGENT24_DATA_DIR` 就只扫数据目录。

导入修订只存在数据库里。不把导入正文种进 `<data_dir>/skills/`，避免下一次扫描把它当成用户自己写的、标成 `Ordinary`。

`mode` 缺省当 `manual`。`origin` 不由请求体里的字段决定：这条 POST 一律是 `imported`；用户目录扫描一律是 `user`。调用方不能自报更可信的来源。

激活、回滚、评测通过，都不改修订的 `origin`，也不改注入时的分类。导入技能激活之后仍然是 `Unknown`。

### 4.2 修订身份

一条修订是不可变的一行。身份是独立的 `revision_id`。`version`（semver）只是清单上的元数据，不参与「是不是同一条修订」。

修订行至少含：`revision_id`、`skill_id`、`semver`、`kind`（`body` 或 `evals`）、`origin`（`user` 或 `imported`）、`content_digest`、正文或评测原文、`created_at`。来源和内容摘要都是这行的字段，也写进注入给模型的技能段开头（§4.4 的渲染结果），供冲突判定、确认摘要和评测使用。

`content_digest` 按 kind 分两种口径，不要混用：

- **正文行**（`kind=body`）：清单摘要，即 `agent24-skill` 的 `SkillManifest::content_digest`。它不含 `version`。只改 `version`，或改动落在解析的空白规范化范围内、解析结果不变，清单摘要就不变。store 不依赖 skill crate，不在入库时把正文摘要重算成字节 SHA-256。成对写入的路由层用这次解析结果再算一次清单摘要并断言；断言失败是内部错误，不入库。同一 `(skill_id, origin, semver, kind)` 命中且摘要相同，返回已有行，即使原始字节不同。
- **评测行**（`kind=evals`）：该行 `content` 字节的小写十六进制 SHA-256。成对写入在 store 内重算，与传入不一致则 `StoreError::Conflict`。

`insert_skill_revision` 是 SL-2 的单条入口，把两种行的 `content_digest` 都当成字节 SHA-256，不一致就 Conflict。目前只有 `agent24-store` 自己的测试调用它。安装和扫描的正文行今后只走成对写入 `install_skill_pair`。

导入响应里的 `semver` 取这次解析结果，不取被复用的旧行上的 semver。

判定：

- 先查 `(skill_id, origin, semver, kind)`。命中且摘要相同：返回已有 `revision_id`，不新插一行。命中且摘要不同：拒绝，错误码 `skill_revision_conflict`，提示升 `version`。不覆盖旧行，也不改用摘要去重绕过这次冲突。
- semver 没命中，才按 `(skill_id, origin, kind, content_digest)` 返回已有行。
- 用户目录里文件变了但 semver 没变、清单摘要也变了：走冲突，不自动变成新修订。用户升版本后才插入新行。新行不移动指针。
- `insert_revision_tx` 在去重命中之前，若请求的 `revision_id` 已经属于另一条修订：`StoreError::Conflict`，不把这次请求当成已有行。

正文修订和评测修订是两行。指针分别指向它们。

### 4.3 导入边界

`POST /api/v1/skills` 只收正文，不收路径。请求体是 JSON：`markdown`（`SKILL.md` 全文）和可选的 `evals`（`{ name, content }` 列表）。多出来的 JSON 字段拒绝（包括任何叫 `path`、`origin`、`allowed-tools` 的字段）。服务端不打开调用方给出的路径。

frontmatter：

- 必填字段按 D1 校验。导入时缺的 Agent24 字段，服务端补 `requires: []`、`mode: manual`、`version: 0.1.0`、`evals: []`（SL-1 提供带导入缺省值的解析入口，SL-3 调用它；用户自写技能仍走必填校验）。评测可以先是空的。去重键为 `(skill_id, origin, kind, content_digest)`：只升 semver 不改内容时返回已有修订；本次导入的「未生效」字段列表一律取这次解析结果，不从库里旧行拼。补完仍然是未激活候选。
- 未知字段丢弃。响应里列出字段名，界面展示「未生效」。
- `allowed-tools` 永远不读。它就是一个未知字段，展示「未生效」，不注册工具，不放宽白名单。

正文大小上限 **32 KiB**（32768 字节，按 UTF-8 计）。超过则 `skill_body_too_large`，不入库。单条评测文件同样不超过 32 KiB。

用户目录只可能由扫描器读取，规则和下面的目录快照相同。扫描器不写目录。

扫描从 `<data_dir>/skills` 起，用 rustix 的 `openat` 逐级打开，不按拼出来的路径打开子项。目录用 `O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC`，文件用 `O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC`。打开后 `fstat` 必须是目录或普通文件，否则拒绝该技能。macOS 对符号链接加 `O_DIRECTORY|O_NOFOLLOW` 返回 `ENOTDIR` 而不是 `ELOOP`；这时用父句柄上的 `statat(AT_SYMLINK_NOFOLLOW)` 把它和普通非目录分开，不打开链接目标。读取用 `take(剩余配额+1)`，读到的字节超过剩余配额即拒绝。目录配额按实际读到的字节累计，不按 `st_size`。`evals/` 相对已经打开的技能目录句柄打开。技能目录本身或 `evals/` 是符号链接时拒绝，并且不登记。

上限写在 `skill_routes`：`<data_dir>/skills` 下直接子目录最多 **64** 个；一个技能内打开的目录最多 **8** 个（含技能目录自己）；深度最多 **2**（技能目录为 1，`evals/` 为 2）。超过则拒绝该技能，不插入半份修订。原有的 32 个普通文件、合计 256 KiB 仍然有效。

`serve` 发出就绪行并开始监听之后，才用 `tokio::spawn` 在后台跑这次扫描。扫描失败只打 `warn`，不挡住就绪，也不让进程退出。

`scans` 是保留的 skill id（`POST /api/v1/skills/scans` 的集合名）。解析和安装都拒绝，错误码 `skill_id_reserved`。导入请求里的评测名和清单里的 `evals` 一样，必须通过同一条 `constrained_eval_key`：形状是 `evals/<file>.jsonl`，拒绝 `..`、绝对路径和空段。

桌面若让用户选一个目录来导入（仍然算 `imported`），由主进程做目录约束快照，再把读到的字节交给上面的 POST。快照规则：

- 从用户选定的那一个目录往下读。拒绝绝对路径、拒绝任何含 `..` 的路径、拒绝符号链接（任一路径分量是符号链接都不跟随）、拒绝非普通文件。
- 只接受该目录下的 `SKILL.md`，以及 `evals/` 里一层的 `*.jsonl`。其余条目不读进修订，响应里列为「未生效」。
- 最多 32 个普通文件，合计不超过 256 KiB。超出则整次导入失败，不插入半份修订。
- 不执行目录里的脚本、二进制或工具。

空评测可以导入，不能激活（`skill_eval_empty`）。

### 4.4 评测：同一个纯渲染函数

评测和运行注入共用一个纯函数，放在新 crate `agent24-skill` 里，不读盘、不调模型、不注册工具：

```text
render_skill_segment(body, origin, content_digest) -> String
```

输出就是将要放进 system 的那一段技能文字，开头带上来源和内容摘要。v1 只注入这段静态正文，不把 `input`、会话历史或工具结果拼进这段。

`judge`：

- `exact`：技能段与 `expect` 全等。
- `contains`：`expect` 是技能段的非空子串。`expect` 为空则整份评测非法（`skill_eval_invalid`），不能靠空串凑通过。

比较对象是技能段，不是整包提示，也不是 `input`。反例：`expect` 只出现在 `input` 里、技能段里没有，判定失败。变异：把比较对象改成 `input`，或改成「永远等于 expect」，这条反例会变绿，即变异失败。

这只证明「注入的就是这份文字」。界面文案用「文本一致性检查」，不用「模型会照着做」或单独的「评测通过率」充当能力证明。可以显示「N/M 条文本一致」。零条时不能激活。

渲染器版本是这个 crate 里的一个常量。改了渲染函数就必须升这个常量。

### 4.5 评测结果状态机，以及激活

评测由服务端在激活请求里重跑。请求体不带通过与否，也不带客户端算好的分数。多出来的这类字段直接拒绝。

一次服务端重跑写成一条结果绑定，字段缺一不可：

- 正文 `revision_id`
- 评测 `revision_id`
- 渲染段的内容摘要
- 渲染器版本
- 每条用例的摘要：稳定 id = `(评测名, judge, input, expect)` 的哈希（评测修订不可变，行号不进 id；同一文件内重复用例在解析时拒绝）、`line`（只作展示）、`judge`、是否通过、`expect` 的摘要

有失败时，请求里的「逐条接受」必须正好是这些失败 id（不能多，也不能少），并且和**这一次**绑定对得上。接受记录写在这条绑定上，不改评测原文，不把失败改写成通过。绑定对不上则拒绝，指针不动。store 写入绑定前再兜底：零条用例 → `skill_eval_empty`；有失败用例没被全部接受 → `skill_eval_failed`。两种都回滚本次事务，指针不动。服务端重跑仍由 SL-4 做。

一次激活相对当前指针只更换正文或评测其中之一：

- 还没有指针：这一次同时钉上正文修订和评测修订，建立第一对组合。没有指针行时当前代次是 0，`expected_generation` 不是 0 则 `skill_pointer_conflict`。
- 已经有指针：请求里的两个修订 id 相对指针恰好一个不同。两个都不同则 `skill_activation_mixed`，要分两次激活。例外：正文与评测同来源，且该来源与当前指针来源不同，视同第一对，允许两侧同时换，不报 `skill_activation_mixed`。同来源下两侧都变仍是 `skill_activation_mixed`。两个都相同、且指针已经停在这对上：幂等成功（§4.5 末）。
- 代次不符先于「两侧都换」：预期代次已过期，即使正文和评测相对指针都变了，也是 `skill_pointer_conflict`，不是 `skill_activation_mixed`。
- 正文修订与评测修订的 `origin` 必须相同。激活和回滚都要求。不同则 `skill_origin_mismatch`，指针不动。这和 `skill_activation_mixed` 不是同一条件。指针主键仍是 `skill_id`，不按来源拆行。来源切换不拆指针行：历史行带 `origin_from`（上一行来源，第一行为空）和 `origin_to`（这一行来源）。
- 每次重跑都针对**即将钉上的那一对**，不是针对旧的那一对。

激活请求体只有：`expected_generation`、正文 `revision_id`、评测 `revision_id`、接受的失败 id 列表。正文不在请求体里。服务端按修订 id 读库。

并发，整段在一个事务里：

1. 读指针和代次。
2. 代次不等于 `expected_generation`，且目标组合也不是指针上已有的那一对：回滚，`409 skill_pointer_conflict`，不写历史、不写审计。没有指针行时按代次 0 比较。这一步先于「两侧都换」。
3. 代次匹配：校验修订存在、属于该技能且种类正确、两侧 `origin` 相同、这一次只换一侧（或是第一对、或是回滚、或是来源切换）、重跑评测、接受列表正好是这一次的失败用例 id。零条用例拒绝。
4. CAS 更新指针（代次加一），插入激活历史，写审计。审计只记 `skill_id`、两个修订 id、内容摘要、结果码、来源切换（`origin_from` / `origin_to`），不记正文（ADR-K1-04）。
5. 提交。

重复提交：指针已经指向请求中的那一对修订时，返回成功，不新增历史，不增加代次。这覆盖桌面双击和 CLI 重试。目标不同而代次已旧，仍是 `409`。

回滚是同一种请求，但引用一条历史激活记录的 id。服务端核对那条记录还在，正文和评测原文仍能按修订 id 读到，然后用同一次 CAS 把指针拨回那一对。回滚不重跑评测，不接受客户端新算的结果，不改历史绑定。它一次恢复一整对已经批准过的组合；这和「新修订一次只能换一侧」不共用同一条规则。不撤销已经发生的工具副作用，不恢复已撤销的同意，不恢复已删除的记忆（ADR-040 `:62`）。

桌面确认见 §4.9。用户拒绝或关闭确认时，请求不会发出，指针不动。

### 4.6 依赖解析

技能 `requires` 的元素是能力 id（ADR-038 D9，`ADR-038:311-314`；登记表 `REGISTRY.md:3`）。组件锁里的 `requires` 是本体权限 `kernel` 与 `operations`（`lock.rs:110-117`）。两套字段同名，解析代码必须分开类型。

v1 不读组件锁的 `requires`，不实现 C9，不把技能依赖映射成 `Operation` 或模块同意。缺能力只表示没装，或同一能力有多个提供者。组件工具该被拒的，仍由现有审批和同意记录拒绝。

执行准入时，对指针上的正文修订：

1. 零个可用提供者：不起 run，`skill_capability_missing`，带登记表中文名和首个提供者。
2. 多于一个：不起 run，`skill_provider_ambiguous`，提示里写上能力 id 和这些组件 id。不自动挑，没有选择接口。
3. 恰好一个：把 `能力 id → 组件 id@版本` 记到这次 run 的绑定上。

**这份绑定只是记录，供审计和回放。** v1 不用它过滤工具列表，也不用它拒绝「调用了绑定之外的组件」。工具仍走 `ToolRegistry` 的白名单和审批门（`agent24-tools/src/lib.rs:1756-1803`）。技能版本、评测通过、激活审批都不是授权（ADR-040 `:47`；ADR-K1-03 `:19-32`）。

`mcp-<server>` 不在登记表里（`REGISTRY.md:34`）。解析时认组件清单里形态为 MCP、`provides` 含该 id 的条目。没有 `provides` 的 server（`REGISTRY.md:42-45`）就是缺能力。

### 4.7 注入、标签、冷恢复

用户在会话里选定一个技能后，这个会话的每一轮 `RunCreate` 都带上同一个 `skill_id`。服务端不另存一份会话级指针。换会话就不带。服务端忽略客户端传来的任何版本号，准入时只读当前指针，然后立刻把修订 id 钉在这次 run 上。

正文始终按钉住的 `revision_id` 读取，不在稍后按「当前指针」再读。指针在 run 开始之后被拨走，这次 run 仍用钉住的那一版。

注入点在 `execute` 组装首包快照处（`lib.rs:2439-2506`），放在暂停通知之后、会话历史之前。技能段用 §4.4 的纯函数从钉住的修订生成。不改每轮重算工具的逻辑（`:2940-2957`）。不提供 `use_skill` 一类工具。

两条记录都要写，缺一不可：

1. **消息来源。** 新的 `MessageOrigin` 种类（建议名 `SkillText`），带这次渲染段的 `instance_digest`。不复用 `HostSystem`（那是暂停通知，`message_origin.rs:31-32`）。SQL `CHECK` 要加这个种类，做法沿用 `0029` 的建新表再拷贝。只加消息来源不够：外发门读的是 K1 标签，不读这张表。
2. **K1 来源标签。** 注入的同一时刻调用 `tag_run_source`，参照 `lib.rs:2527-2549`。新的 `SourceKind::SkillText`（不复用 `SelectedMaterial`：那个构造函数把分类固定成 `Unknown`，且注释写明留给尚未存在的「选中文件」入口，`source_policy.rs:63-66`、`:299-313`）。`source_id` 形如 `skill_text:{skill_id}`。`revision_digest` 必须是钉住修订的内容摘要，不能空。`mode` 一律 `LocalOnly`。`imported` 的分类是 `Unknown`，构造方式对齐 `SourceRef::inbound_external`（`source_policy.rs:279-292`）。`user` 的分类是 `Ordinary`。

标签写入失败、或这次 run 钉了技能却读不到标签：run 以 `source_policy_unavailable` 结束，技能段不得送进模型。缺标签时不得当成 `Ordinary`。

冷恢复沿用已经落库的快照和标签，不重读指针，不重新分类。`execute` 的恢复路径今天也不会重打用户输入标签（`lib.rs:2529-2532`），技能照同一条。

验收时至少这三条：

- 导入技能的 run 不能走外发工具，也不能被转发。变异：把该标签改成 `Ordinary`（或删掉 `Unknown`），外发断言变红。
- 标签写入失败：不起模型调用。变异：忽略 `tag_run_source` 的错误并继续，测试变红。
- 冷恢复：进程重启后正文仍是钉住修订的字节，导入技能的标签仍是 `Unknown`。指针在重启前已被拨到别的修订，恢复出来的仍是旧的那一版。

没带 `skill_id` 的 run：快照与今天相同。这是回归哨兵。

### 4.8 谁能带 skill_id

`RunCreate` 增加可选 `skill_id`。省略或 null 等于没选技能。不增加 `skill_version`。旧客户端省略该字段仍能反序列化。

不能带 `skill_id` 的两条路径，在插入 run 之前拒绝，错误码 `skill_not_admitted`：

- 入站：`RunManager::start_run_inbound`（`lib.rs:1497`）。`RunCreate` 上就算有这个字段，这条路径也不接受。
- 定时自唤醒：`start_run_with_schedule`（`lib.rs:1465`）。

各有一条反例测试：构造带 `skill_id` 的调用，断言没有 run、没有技能修订被读、没有技能标签。普通 `start_run` 带同一个 `skill_id` 仍可准入（正对照）。

普通 `start_run` 带 `skill_id` 时，在插入 run 之前只读 `skill_pointer`：

- `skill_id` 不存在，或存在但没有指针行（只装未激活，导入后的默认状态）：一律拒绝，错误码 `skill_not_active`；不读任何修订、不打来源标签、不插入 run。**不得回落去读最新修订**——那会让导入的 `Unknown` 文本绕过「激活 = 用户确认」。
- 有指针：钉住指针指向的不可变修订，再走 §4.6 的依赖解析。
- 反例测试（SL-4 与 SL-5 各一条：SL-4 在 API 层、SL-5 在准入层）：装了、没激活、带 id → 无 run、无标签、无修订读取；变异：改成「无指针时读最新修订」→ 测试变红。

### 4.9 激活确认、API、CLI 令牌

桌面技能栏上点「激活」或「回滚」就是用户的确认。渲染进程不调用激活路由。主进程专用 IPC 做这几步：用自己的宿主凭据读取候选修订的字节，自己生成确认摘要，然后一次性提交 §4.5 的请求。摘要固定四项：技能名、semver、内容摘要的前 8 位十六进制、评测修订相对当前指针是否变化（回滚时按实际变化显示哪一侧换回）。生成这四项所用的数据还包含来源（`user` 或 `imported`）：store 返回的指针和激活历史带上这个来源，指针主键仍是 `skill_id`。候选修订的来源与当前指针来源不同时，摘要标明来源切换（from 为指针来源，to 为候选来源）。激活历史每一行的 `origin_from` / `origin_to` 是同一次切换的记录。摘要以主进程读到的字节为准，不采用渲染进程传来的摘要。

`POST /api/v1/skills/{id}/activate` 不进 `ALLOWED_PROXY_ROUTES`（`backend-proxy.ts:56`）。在该文件「有意不代理」的注释里写上这一条。

这条路由是 `skills.activate`，`Elevated`。把它加进 `resolve_target` 的 `supported` 列表（`first_party.rs:500-518`）。不在列表里时，第一方提升会得到 `NotElevatedOperation`，CLI 无法经桌面确认。daemon 自己的 `target_summary` 从库里的修订生成同一份四项摘要；修订读不到则不要发出确认（现有写法是 `NotElevatedOperation`，`first_party.rs:574-576`）。

CLI 可以激活，但只有桌面确认这一条路：CLI 发起提升，桌面出示 daemon 生成的摘要，人点头之后才带提升头调用。CLI 不自批。

路由（ADR-045：新前缀 `skills` 要先写进 ADR-045 §4.4，否则 N10 门禁失败。不要放在 `/api/v1/capabilities/`）：

| 方法与路径 | 操作 | 级别 | 作用 |
|---|---|---|---|
| `GET /api/v1/skills` | `skills.read` | Everyday | 列表。不含正文 |
| `GET /api/v1/skills/{id}` | `skills.read` | Everyday | 指针、代次、已安装修订、依赖是否只有一个提供者、文本一致性摘要 |
| `POST /api/v1/skills` | `skills.install` | Everyday | 导入正文，状态 `installed`。不移动指针 |
| `POST /api/v1/skills/{id}/activate` | `skills.activate` | Elevated | 激活或回滚。服务端重跑评测。要桌面确认 |

没有单独的「客户端上报评测结果」路由。激活失败且未接受时，响应带服务端刚算出的用例摘要，指针不动；客户端带上接受 id 再提交一次，服务端重跑并核对。

`{id}` 是这条路径上唯一的参数，符合 ADR-045 N3。`activate` 是路径末尾的动词，符合 N7。

这些操作不进 `creative_route_permitted`（`route_policy.rs:222`）。

**CLI 日常令牌会自动带上路由表里的新操作。** `FirstPartyAuthority::mint` 把 `policies()` 里的操作放进日常令牌，只滤掉 presence 和 `InboundWrite`（`first_party.rs:374-384`）。因此 `skills.read` 和 `skills.install` 会随注册发给 CLI。这是有意的：CLI 的 list / show / install 就靠这张令牌，不为技能单独开一张过滤表，也不要在 mint 里把 `skills.*` 抠掉。`skills.activate` 同样会出现在令牌上（现有过滤不按级别剔除），但没有提升头时中间件拒绝提升级调用（`middleware.rs:258-265`）。SL-3 加一条测试钉住「日常令牌含 `skills.read` 和 `skills.install`」，避免以后有人把它当成疏漏删掉。

**组件不能被授予这些操作。** C9 合入后，组件锁的 `requires.operations` 会有一份「可授予 / 排除」测试。SL-3 把 `skills.read`、`skills.install`、`skills.activate` 写进排除名单：组件清单声明其中任一项则非法。开工时若测试改了名，以这条语义为准。技能加载器自己不读 `requires.operations`。

错误码闭集（每个码都有唯一触发条件；测试按此表写）：

| 码 | 何时抛 | HTTP |
|---|---|---|
| `skill_manifest_invalid` | SL-1 解析失败（缺必填字段、字段类型错、能力 id 字符集不合法） | 400 |
| `skill_id_reserved` | skill id 是保留名 `scans` | 400 |
| `invalid_request` | 请求不是这条路由接受的 JSON（重扫带了路径，或正文无法解析） | 400 |
| `not_found` | `GET /api/v1/skills/{id}` 没有这个技能的修订 | 404 |
| `skill_proactive_unsupported` | frontmatter 声明主动技能（D6，v1 不做） | 400 |
| `skill_body_too_large` | 正文超过 32 KiB | 413 |
| `skill_revision_conflict` | 同来源同 semver 但内容摘要不同（提示升版本） | 409 |
| `skill_capability_unknown` | 依赖的能力 id 既不在 `docs/capabilities/REGISTRY.md` 登记表里，也不是 `mcp-<server>` 形态（安装时就拒）；`mcp-*` 一律放行到准入时按 §4.6 判，没有 `provides` 的 server 走 `skill_capability_missing` | 400 |
| `skill_capability_missing` | 登记过的能力当前零个可用提供者（准入时拒，§4.6 第 1 条） | 409 |
| `skill_provider_ambiguous` | 同一能力有多个提供者（准入时拒，§4.6） | 409 |
| `skill_not_active` | 普通 `start_run` 带的 `skill_id` 不存在或没有指针（§4.8） | 409 |
| `skill_not_admitted` | 入站或定时自唤醒路径带 `skill_id`（§4.8） | 400 |
| `skill_eval_invalid` | 评测文件解析失败 | 400 |
| `skill_eval_empty` | 评测零条用例时请求激活（空评测可以导入，不能激活） | 409 |
| `skill_eval_invalid` 补充 | 单条用例的 `expect` 为空（`exact` 与 `contains` 都拒）、同一评测文件内重复用例、JSON 重复键 | 400 |
| `skill_eval_failed` | 激活时有失败用例未被全部接受（服务端重跑，或 store 写入绑定时兜底）；响应带用例摘要，指针不动 | 409 |
| `skill_activation_mixed` | 已有指针，且请求里正文修订与评测修订相对当前指针**都**不同，并且与指针同来源（首次激活、回滚、来源切换除外：来源切换是正文与评测同来源且与当前指针来源不同，视同第一对，两侧同时换；正对照「同来源两侧都变 → mixed」）。代次已过期时不走这个码，走 `skill_pointer_conflict` | 400 |
| `skill_origin_mismatch` | 激活或回滚时，正文修订与评测修订的 `origin` 不同。指针不动。与 `skill_activation_mixed` 无关：两侧可以只换一个，来源仍然必须相同。指针主键仍是 `skill_id` | 400 |
| `skill_pointer_conflict` | CAS 失败：预期指针代次与当前不符。没有指针行时当前代次是 0 | 409 |

回滚恢复一整对历史上已批准的组合，以及来源切换（候选正文与评测同来源、且与当前指针来源不同），允许在已有指针时两侧同时变化。同来源的新修订一次只能换一侧。确认摘要按实际变化显示「换回正文」「换回评测」或「两侧均换回」；来源切换时标明 from/to。

激活的测试三条：渲染进程经普通代理调用被拒；legacy 共享令牌调用被拒；同一目标重复提交幂等（指针与代次都不动，历史不增加）。再加：不在 `supported` 列表里时 CLI 提升失败。变异是从 `supported` 去掉这一条，或把路由加回 `ALLOWED_PROXY_ROUTES`。

### 4.10 桌面技能栏

同一能力中心页加一个技能栏，不新开产品名词，也不做能力栏。

列：名称、semver、来源（用户 / 导入）、依赖的能力（缺的或不止一个提供者的标红）、模式（固定「手动」）、文本一致性（N/M）、评测集是否相对指针有变化。确认前能看到哈希前 8 位。不展示内部字段名 `base_hash`。

激活和回滚只走 §4.9 的主进程 IPC。侧边栏固定逻辑不动（`Capabilities.tsx:62-79`，`registry.ts:8-23`）。

会话里选中的技能记在桌面会话状态中，这个会话的每一轮发 run 都带上 `skill_id`。服务端不记得这件事。

### 4.11 存储

表都在 `agent24-store` 的新迁移里。编号在该片开工时取「当时最新 + 1」。本方案不写死 `0036`。不改旧迁移。

技能表一张迁移（SL-2）：

- `skill_revision`：§4.2 的列。唯一约束覆盖「同来源同 semver 同 kind」：摘要不同返回 `skill_revision_conflict`，不覆盖；摘要相同返回已有 `revision_id`（正文行的摘要是清单摘要，原始字节可以不同）。另有唯一索引 `(skill_id, origin, kind, content_digest)`（§4.3）：只在 semver 未命中时用来去重。`insert_revision_tx` 在采纳去重命中之前，若请求的 `revision_id` 已属于另一条修订，返回 Conflict。
- v1 没有「一次安装写了哪一对」的关联表。详情的 `text_consistency` 用同一 `skill_id + origin` 下按 `created_at` 最新的正文行，配合同来源下最新的评测行，不按 semver 配对。这是 v1 口径：它等于最近一次安装写入或复用的那一对，前提是两次安装不会把更新的评测行和更旧的正文行交错开。
- `skill_pointer`：每个 `skill_id` 一行（主键就是 `skill_id`），`body_revision_id`、`evals_revision_id`、`generation`。没有这行就是未激活，当前代次按 0。读出的视图带上两侧修订共同的 `origin`。
- `skill_activation`：历史组合，含 §4.5 的结果绑定。回滚按这张表的 `activation_id` 引用。自增 `seq` 是顺序，不按 `created_at` 排。
- `skill_eval_result`：绑定上的 `accepted_json` 是 JSON 数组，元素必须是这一次 `cases_json` 里失败用例的 id。
- `run_skill_pin`：`(run_id, skill_id, body_revision_id, content_digest, providers_json)`。`providers_json` 是 JSON 对象，只作记录。旧 run 没有这行，读取时当「这次没使用技能」。

消息来源的 `CHECK` 放另一张迁移，跟注入那片走（SL-5），不塞进技能表这张。来源种类沿用 `0027`–`0029` 的改表方式。`SourceKind` 存在标签 JSON 里，加变体不必改 `0016` 的表形状；旧 daemon 读到未知种类会走失败关闭，这是预期，不在 v1 里做兼容读取。

`skill_revision` 保存正文和评测原文，回滚才读得到。审计不保存原文。

这两张迁移都是数据写入语义，对应 PR 要等 PR-Daemon APPROVE 再合。

组件锁和 Documents 清单继续要求 `skills` 为空（`documents_catalog.rs:235-236`）。组件附带技能不在 v1。

## 5. 分片计划

集成分支用 `ab/skill`（本方案不创建分支）。task 分支 `ab/skill-NN-<短名>`，PR 基线是 `ab/skill`，不是 `main`。`ab/skill` 从 **C9 已合入的 `main`** 拉出。下表「规模」是该片产品代码的估计行数，不含测试。超过 300 就再拆；SL-5 若超过，是因为注入和 `Unknown` 标签必须同片，中间版本不能把外部正文标成普通用户输入，在 PR 正文说明为什么不拆。

没有一片去实现 C9。SL-2、SL-4、SL-5 合入集成分支前等 PR-Daemon APPROVE。其余片 CI 全绿加本地门禁后可合。

| 片 | 目标 | 涉及文件 | 规模 | 依赖 | 验收（含变异） | 风险 | 建议派给 |
|---|---|---|---|---|---|---|---|
| SL-1 | 纯解析与文本一致性检查。必填字段、semver 仅作元数据、能力 id 字符集、未知 frontmatter 丢弃、`allowed-tools` 不进入结果、32 KiB 上限、拒绝 `proactive`、`render_skill_segment`、`exact` / `contains`、拒绝空 `contains` | 新 `rust/crates/agent24-skill`。不读盘、不入库 | ~250 | 无 | 缺必填字段失败。未知字段出现在「未生效」列表且不出现在结构化结果里。`expect` 只存在于 `input` 时失败。删掉空 `contains` 拒绝后，空 `expect` 夹具变绿，即变异失败 | 低 | Rust 代理 |
| SL-2 | 技能表迁移与 store API。只插入不覆盖。同摘要幂等，同 semver 异摘要冲突。指针单独更新 | `agent24-store` 新迁移（编号开工时定）与 store API | ~220 | SL-1 的类型。**数据写入，等 PR-Daemon APPROVE** | 冲突插入失败并保留第一行。同摘要第二次返回同一个 `revision_id`。没有指针时当前查询为空。把冲突改成覆盖后，第一条测试变红 | 迁移写坏旧库；编号和 C9 撞车 | Rust 代理 |
| SL-3 | 导入 API 与用户目录扫描。POST 无路径。目录快照拒绝 `..`、绝对路径、符号链接、非普通文件，并限制 32 文件 / 256 KiB。导入恒为 `imported`，扫描恒为 `user`。不移动指针。改 ADR-045 §4.4 增加前缀 `skills`。新操作写入 C9 排除测试。钉住 CLI 日常令牌含 read/install | `agent24d` 路由、`operations.rs`、`route_policy.rs`、OpenAPI；ADR-045；C9 那份排除测试 | ~280 | SL-2。改了 `protocol/` 时跑 `pnpm lint:openapi` 与 `pnpm gen:api` | 请求体带 `path` 被拒。同内容重复导入不新行。同版本异内容 `skill_revision_conflict`。导入后指针仍空。组件清单写 `skills.activate` 被拒。把 POST 改成接受路径并打开该路径，目录逃逸测试变红 | 新 Operation 漏登记会误放行；导入正文被扫描成 `user` | Rust 代理 |
| SL-4 | 激活与回滚。服务端重跑、结果绑定、一次只换一侧、事务内 CAS、幂等、桌面主进程 IPC、`supported` 列表、代理表明确排除 | `agent24d` 激活路由；`first_party.rs`；`backend-proxy.ts` 与其测试；审计 action | ~280 | SL-1、SL-2。**授权与审计，等 PR-Daemon APPROVE** | 渲染进程直调被拒；legacy 令牌被拒；重复提交幂等。未接受的失败、两侧同时换新、代次不符，指针都不动。从 `supported` 去掉 activate 后，CLI 提升测试变红。回滚不重跑评测，不删除旧的 pin | 把激活做成授权；代理转发提升级请求 | Rust 代理做 daemon；桌面代理做 IPC |
| SL-5 | 准入解析、钉修订、注入、`SkillText` 消息来源与 K1 标签。入站和自唤醒拒绝 `skill_id`。多提供者拒绝。绑定只记录 | `agent24-agent` 快照与 `start_run_*`；`message_origin.rs`；`source_policy.rs`；来源 `CHECK` 的新迁移（编号开工时定）；协议 `RunCreate` | ~280 | SL-3、SL-4。**来源标签与数据写入，等 PR-Daemon APPROVE**。改 `agent24-agent` 后跑 `cargo test -j 4 -p agent24d --bin agent24d` | 导入 run 外发被拒（去掉 `Unknown` 则变红）。标签写失败不起模型。冷恢复仍是钉住的字节。`start_run_inbound` 与 `start_run_with_schedule` 带 `skill_id` 的反例不起 run。两个提供者不起 run，且工具列表与不带技能时相同。无 `skill_id` 的快照与基线一致 | 只加了消息来源、标签仍是 `Ordinary`，外发门失效 | Rust 代理 |
| SL-6 | 桌面技能栏。D7 的 v1 列、缺依赖标红、按钮走主进程 IPC、会话记住选择并每轮重发、文案用「文本一致性检查」 | `Capabilities.tsx` 加栏，不改固定逻辑；会话发 run 的那一处 | ~250 | SL-3、SL-4 的 API | 页面测试：不止一个提供者的行标红；未激活没有执行。代理测试：激活不在 `ALLOWED_PROXY_ROUTES`。合入前桌面手工点按钮，摘要四项来自主进程读到的字节，指针变化；再点一次指针代次不变 | 渲染进程直写指针，或会话只在第一轮带 `skill_id` | 桌面代理 |
| SL-7（可选） | CLI：`skill list`、`show`、`install`，以及经桌面确认的 `activate`。REST，不开库 | `agent24-cli` | ~200 | SL-3、SL-4 | 对临时 daemon：list 能看到 install；activate 在没有桌面确认时失败，确认后指针变化。本片不新增 daemon 环境变量。若误读新 env，再补 `agent24-cli` 的 env 扫描测试 | CLI 用日常令牌直接激活 | Rust 代理 |

顺序：SL-1 → SL-2 → SL-3 → SL-4 → SL-5 → SL-6。SL-7 在 SL-4 之后，可与 SL-5、SL-6 并行。首条闭环是 SL-1：解析和文本检查不依赖数据库。

## 6. 明确不做

- 不做 `distro`、`bundled`、`proposed`。不扫描安装目录和分发目录。不做版本差异界面，不做内置版本 CI，不把更高版本写入用户目录。
- 不做默认提供者表，不做 `PUT /api/v1/capabilities/{id}/provider`，不做能力中心的能力栏。同一能力多个提供者时拒绝并提示。
- 提供者绑定不做调用约束：不按绑定增减工具，也不在调用时核对组件 id。
- 不做 D6：不读入站事件来启动技能，不实现 `observe` / `auto`。`mode: proactive` 安装即拒绝。入站 run 与定时自唤醒不能带 `skill_id`。
- 不做 ADR-040 D2 的只读上下文（会话、工作区、入站事件、工单）。v1 注入的只有静态技能段。
- 不做 `local_model`，评测不调用 iDoris 或云端模型。
- 不做从会话蒸馏、不做审查器提案、不调用 `KnowledgeBase::propose`。
- 不执行技能目录里的脚本、二进制或工具调用。不把 `allowed-tools` 读成权限。
- 不把 `skills` 写进 `ComponentLock`，不放宽 Documents 的空 `skills`。
- 不实现 C9，不把技能 `requires` 映射成 `Operation` 或模块同意。
- 激活不把 `imported` 降级成 `Ordinary`，也不把用户技能升成更可信的种类。
- 不改无 `skill_id` 的 run，不改计划模式的工具子集，不改记忆召回顺序。
- 不做技能市场、签名验证、跨社区 Nostr 分发。
- 不改 `docs/tracking/2026-11-10/MILESTONES-AND-TASKS.md` 和 `followups.md`。
- 不在本方案里写代码。

## 7. 与 C9 的施工关系

- 开工基线是 C9 合入后的 `main`。迁移号在 SL-2 和 SL-5 各自开工时再定。本树最新是 `0035_inbound_cursor.sql`，不能据此预定 `0036`。
- 技能 `requires` 只解析能力 id。C9 接通的是组件锁那一套 `requires` ∩ `Grants` ∩ 用户确认。两套类型保持分开。
- 新的 `skills.*` 操作写进 C9 的「组件可授予操作」排除测试。CLI 日常令牌因为 `first_party.rs:374-384` 的路由映射会带上这些操作，这是有意的，SL-3 用测试钉住，见 §4.9。

## 8. 已拍板

jason 2026-11-12 拍板的三条：

1. **导入的技能正文按外部文字处理。** 注入时写 `tag_run_source`（`SourceRef`，对齐 `source_policy.rs:279-292` 的 `inbound_external` 与 `lib.rs:2527-2549` 的落标签）。`imported` 为 `Unknown` + `LocalOnly`，用户自写为 `Ordinary`。激活不降级。新增 `SourceKind::SkillText`，带 `revision_digest`。只加 `MessageOrigin` 不够。
2. **激活是桌面点按钮即确认。** 走桌面主进程专用 IPC：宿主读取候选内容，生成确认摘要（技能名、版本、内容哈希前 8 位、评测集是否变化），一次性提交。禁止 backend-proxy 转发 activate。activate 加入 `first_party.rs:500-518` 的 `supported` 列表。CLI 经桌面确认可以激活。
3. **v1 只做 `user` 和 `imported`。** 不做分发来源扫描，不做版本差异，不做内置版本 CI，不做默认提供者表和 `PUT` 提供者。同一能力有多个提供者时直接拒绝并提示。

ADR-040 §6 的四条，按评审推荐定为：

- **Q1 已定。** 用户技能放 `<data_dir>/skills/`。与 ADR-037 的数据目录是同一个根。不在 `AGENT24_DATA_DIR` 之外再写一份。导入正文不写入这个目录。
- **Q2 已定。** v1 只读 frontmatter 和正文，不执行附带脚本，不加载脚本、二进制和未声明的附件。未知 frontmatter 字段丢弃，并在响应和界面展示「未生效」。`allowed-tools` 永不读取。正文上限 32 KiB。目录导入使用 §4.3 的约束快照。
- **Q3 已定。** v1 不加「只在 LocalOnly 下运行」的清单字段。能不能出本机，由这次 run 的来源标签、用到的工具和 K1-02 决定。导入正文的 `Unknown` + `LocalOnly` 来自 §4.7 的标签，不是清单字段。
- **Q4 已定。** 激活至少一条 `exact` 或 `contains`，对象是技能段。空评测不能激活。空 `expect` 非法。界面称为文本一致性检查。

本版没有留下产品取舍。§9 只是开工时要再读一遍的事实，不是待选方案。

## 9. 开工时再核对的事实

各片验收按符号名定位代码（函数名、测试名），文中行号只作参考，C9 合入后会漂移。

- C9 的排除测试在本树不存在。合入后按「组件声明 `skills.*` 即非法」对上真实测试名，再改那一份夹具。
- 迁移的实际编号以开工时 `rust/crates/agent24-store/migrations/` 的最大号为准。
- `RunCreate` 的字段在 `agent24-protocol/src/types.rs:479`。加 `skill_id` 时确认省略仍能反序列化，并确认 OpenAPI 与 `pnpm gen:api` 无意外 diff。
- 外发门和 peer 隐藏以「标签分类不是 `Ordinary`」为准（入站测试 `lib.rs:6653` 附近）。SL-5 沿用这条门，不新写一套只认技能的门。冷恢复若发现恢复路径会重打 `user_input`，技能标签必须在那次重打之后仍是 `SkillText` 的原分类，不能被盖成 `Ordinary`。
- iDoris-Components 里 #1037 提到的五个技能样本不在本仓库。导入夹具用本方案自己的最小 `SKILL.md`，不假设那些文件的 frontmatter。
- 能力中心是否还有第二套组件列表页面，开工前在 `apps/desktop/src/renderer` 再搜一次。本版不做能力栏，搜到也不在 v1 改它。
