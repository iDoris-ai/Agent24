# ADR-038：组件契约 v1

索引：[架构总入口](../architecture/README.md)・[ADR 索引](README.md)

- 状态：**草案 r3**（2026-11-01）。r2 按 ADR-039 修订：范围扩大到所有运行形态，加入签名验证，加入组件清单；r3 按 Fable 评审修订，见下方修订记录
- 上位：[ADR-039 概念模型](ADR-039-concept-model.md)。按 ADR-039 的定义，组件是**能力**（Capability）的安装包；对用户只讲能力，不讲组件
- 依据：[可进化性评估最终版](../architecture/EVOLVABILITY-ASSESSMENT-2026-11-01.md) 建议第 2 条
- 相关：[ADR-K1-01 模块工具契约](ADR-K1-01-module-tool-contract.md)、[ADR-K1-03 启用/授权/信任/吊销](ADR-K1-03-enable-authorization-trust-revocation.md)、CORE-2（capability token 与提权）、`agent24-os-proto` 握手

## r3 修订记录（Fable 评审 2026-11-01）

| 编号 | 改了什么 |
|---|---|
| B1 | `capabilities` 一词三义。锁和清单拆成 `provides`（提供的能力）、`requires`（需要本体给的权限）、`assurance`（iDoris 的 L1–L6 保障等级）；D7 重写为按 `requires` 授权；D4 attach 行改正：`initialize.capabilities` 是请求项不是提供项 |
| （MCP 豁免） | D1 加 MCP 行：D2/D2a/D3 不适用；D5 只用子集；D9 清单由 `tools/list` 自动生成 |
| H4 | D9 加能力 id 规则、登记表、三份清单的权威顺序、同一能力多个提供者的解析规则 |
| H5 | D2a：锁里的 sha256 是签名后制品；可复现比对在签名前做。C1 涉及 Hyphae 发布流水线，Hyphae 签名校验挪到 Hyphae 发出签名版之后 |
| H6 | Open Design 的下载和状态在 Electron main；§1 补第三个下载器；C8 改为桌面上报状态；OD 版本目录键保留 `<odSha7>-<platform>-<arch>` |
| H7 | D5 状态机补 `stopping`、卸载、重启、崩溃、重试等转移；`incompatible`/`failed` 不是终态；加与 `DrainState`/`SidecarState`/`OpenDesignComponentState` 的映射表；远端服务与 attach 的状态子集 |
| M1 | D4：iDoris 没有 `/capabilities` 端点，`contract_version` 在 `/health` |
| M2 | D6：`$/cancel` 改为 `$/cancelRequest` |
| M3 | D5：宽限 15 秒要把 `lifecycle.rs` 的 `GRACE_MAX_MS` 抬到 ≥15 000，统一两处默认值和 env 名；D6 单次调用取消宽限单独定 2 秒 |
| M4 | D2：iDoris 从 `~/Library/Application Support/Agent24/idoris` 迁出的做法（重下、旧目录可删）；平台键 Rust triple → `<os>-<arch>` 转换 |
| M5 | D2a：定义「每个可执行文件」；bundle 用 `--deep`；Hardened Runtime 判定方法；`check_platform` 放开到所有 macOS 架构 |
| M6 | D5：`GET /api/v1/components` 登记 Operation、不进 creative allowlist、非 host audience 脱敏 |
| M7 | D7：首次启用的同意内容按 ADR-K1-03 §1、§4，不用「现有系统确认框」 |
| M10 | D3/D4：进程内组件免 D3 运行时检查，只有 D9 清单和 manifest `version` |
| M11 | §4：C3 在 C2 之前，Hyphae 的状态过渡期从 pid 存活判定，没有 `degraded` |
| L1 | D4 CLI 绑定：选定 `HYPHAE_OUTPUT=json` 环境变量加子命令 `contract` / `health`，不加 `--json`，与 `runner.rs` 现有约定一致 |
| L2 | C4：AgentEar 不下载、不锁 → D2/D2a 不适用，明说这是接受的缺口 |
| L3 | C1：`agent24-components` 与 `agent24-idoris` 依赖方向反转；`run_install_child` 隐藏子命令由宿主二进制分发 |
| L4 | D1：三类免锁条件合成一句 |
| L5 | D2a：公证票据只用 `spctl --assess --type execute` |

## 1. 背景

Agent24 现在交付能力的方式各不相同。其中进程外、独立发版的几个组件情况如下：

| 组件 | 传输 | 版本锁 | 契约版本协商 | 能力声明 |
|---|---|---|---|---|
| iDoris | HTTP（OpenAI 兼容 + Admin） | `agent24-idoris/idoris.lock.json`：tag、commit、各平台 sha256、团队 ID | **有**：`contract_major` / `min_contract_minor`，`check_contract` 拒绝不兼容 | 锁里有 `capabilities`，但内容是 **L1–L6 保障等级**（L6 的来源是 Developer ID 公证），不是「能做什么」 |
| Hyphae | 子进程 CLI + JSON envelope | `agent24-comm/hyphae.lock.json`：source_sha、构建配方、各平台 sha256 | **无**：靠 Hyphae 文档约定 | 无 |
| AgentEar | attach socket，JSON-RPC（`os-proto` initialize） | Agent24 不锁，AgentEar 自己装 | **有**：`protocol_versions` 区间协商，`manifest_digest` 校验 | 清单 + `Offer` 授权（`initialize.capabilities` 是模块**请求**内核给的 events/models…，不是它提供的能力） |
| Knowledge（WeKnora） | HTTP | 无锁（外部部署） | **有**：`/system/capabilities` 的 `protocol_versions` | `supported_operations`、`limits` |
| Documents | 桌面 main 进程拉起 `agent24-documents` CLI（`documents-cli.ts`） | 同仓库构建，不需要锁 | 无 | 无 |
| Open Design | sidecar，`CreativeServeWeb` 拉起，经 `agent24 acp` 桥接入 | 桌面打包只带一个小 manifest（sha256、大小、URL）；`open-design-component.ts` 按需下载 | 无 | 无 |

后果：
- 每接一个新组件，下载、校验、握手、错误处理都要重新发明一遍。
- 下载器有三个。`agent24-components::fetch_verified` 克隆 iDoris 的锁，再改写它的字段，借用 iDoris 的下载器，缓存目录还必须放在 `idoris` 目录下——Hyphae 的安装就是走的这条路；Open Design 在 Electron main 里另有一套 TS 下载器（host 白名单、sha256、断点续传），daemon 看不到它。
- 换成另一个品牌的组件，要重写适配层；对端版本不兼容时，有的组件能在握手时拒绝，有的要到调用出错才发现。

已有的好做法要保留，并推广到所有组件：
- iDoris 的「锁 + 契约版本 + 保障等级」三件套；
- `os-proto` 的版本区间协商和清单摘要；
- Knowledge 的能力文档；
- `open-design-component.ts` 的断点续传和逐跳重验 HTTPS/host。

另外几种运行形态也各有一套：
- 进程内 DomainModule（`agent24-domain`）：有挂载缝，但没有生产模块；
- 领域 OS（Sin90 / Cos72）：走 `os-proto` 握手；
- MCP server：从 `~/.agent24/mcp.json` 挂载，工具经 `agent24-mcp` 包装成普通 Tool；
- Open Design：sidecar，由 `CreativeServeWeb` 托管；`SidecarManager` 已实现但产品路径未使用。

## 2. 决策

### D1 范围：所有组件，所有运行形态

**覆盖**：所有交付「能力」的组件（ADR-039 §2.2），不论它怎么运行：
- 进程内；
- 子进程 CLI；
- 常驻服务（HTTP）；
- 附着（attach）；
- sidecar；
- MCP server；
- 远端服务。

现有的 iDoris、Hyphae、AgentEar、Knowledge、Documents、Sin90/Cos72、Open Design，以及今后的 OCR、图片处理、CAD、Open Creator，都在范围内。

各条款的适用范围不同：

| 条款 | 适用于 |
|---|---|
| D2 锁、D2a 签名 | **只适用于 Agent24 自己下载的组件**。以下三类免锁：随本体一起构建（同仓库，如 Documents）、组件自己安装（attach，如 AgentEar）、远端服务（不下载，如外部部署的 WeKnora）。免锁的组件一旦改为由 Agent24 下载，必须补锁 |
| D3 契约版本 | 进程外组件。**进程内组件免**（同仓库编译，不可能握手失配；只要 D9 清单和 manifest `version`）。**MCP 免**（见下行） |
| D5 生命周期、D6 错误与取消、D7 授权、D9 清单 | 所有组件 |
| D4 绑定 | 按运行形态选一种 |
| **MCP server** | D2/D2a/D3 不适用：它来自用户的 `~/.agent24/mcp.json`，没有锁，`protocolVersion` 是日期串不是 `major.minor`。D5 只用 `installed / starting / ready / failed / stopped`；D6、D7 照常；D9 清单由 `tools/list` 自动生成，能力 id 固定为 `mcp-<server>`，不进登记表 |

**不覆盖**：
- 本体自己的内部接口，比如 `ModelProvider`、memory 各 trait、存储层。这些属于本体，不属于组件；
- 客户端 ↔ daemon 的 v1 REST/WS（已有 `protocol_compatibility`）；
- 内部存储替换和数据迁移格式（另立 ADR）；
- MCP 的 wire 协议：照 MCP 规范来；
- 技能（ADR-040 待写）；
- 组件内部的业务语义，比如 iDoris 的路由策略、Hyphae 的群聊语义。

### D2 统一锁格式 `ComponentLock`（schema 2）

每个需要 Agent24 下载的组件，在自己的 crate 里放一份 `<id>.lock.json`，结构统一：

```json
{
  "schema": 2,
  "id": "hyphae",
  "version": "v0.28.1",
  "source": { "repo": "https://github.com/iDoris-ai/Hyphae", "commit": "44189b8e…" },
  "contract": { "name": "hyphae-cli", "major": 1, "min_minor": 0, "tested": "1.0" },
  "release_url_base": "https://github.com/iDoris-ai/Hyphae/releases/download",
  "assets": {
    "darwin-arm64": { "name": "hyphae-darwin-arm64", "sha256": "…", "files": ["hyphae"], "executables": ["hyphae"] }
  },
  "signing": { "macos_team_id": "HYAP444FN9", "notarized": true },
  "reproducible": { "toolchain": "go1.26.4", "recipe": "…", "unsigned_sha256": { "darwin-arm64": "…" } },
  "stop_grace_ms": 15000,
  "provides": [
    { "id": "nostr-dm", "name": { "zh": "Nostr 私信", "en": "Nostr DM", "th": "…" },
      "summary": { "zh": "…", "en": "…", "th": "…" },
      "tools": ["hyphae.dm_send", "hyphae.dm_list"] }
  ],
  "requires": { "kernel": ["events"], "operations": [] },
  "assurance": []
}
```

- `id`、`version`、`contract`、`assets`、`provides`、`requires` 是必填字段。`signing` 的必填规则见 D2a；`reproducible` 可以不填，填了就要校验；`assurance` 是 iDoris 那种保障等级（L1–L6 带来源），不是能力，可以为空。
- **字段名**：提供的能力叫 `provides`，需要本体给的权限叫 `requires`。**锁和清单里不再有叫 `capabilities` 的字段**；ADR-039 §4.1 的消歧规则同样适用于这里。
- `provides[].id` 是**能力 id**（D9），技能按它声明依赖。`requires.kernel` 对应 `DomainOsManifest.kernel_capabilities`（`events/models/scheduler/policy/memory/approval`），`requires.operations` 对应 CORE-2 的 `Operation`。
- `assets.<platform>.executables` 列出 D2a 要验签的文件。**r4 订正（§7.7）：必填，不再有「不填等于全部 Mach-O / ELF」的默认值，且每一项必须出现在 `files` 里。**
- 平台键统一用 `<os>-<arch>`（`darwin-arm64`、`linux-x64` …）。某个平台没有资产，就不写这个键，不写 `null`。读 schema 1 时把 Rust triple（`aarch64-apple-darwin`）转成这个形式，Hyphae 锁里的 `null` 键删掉。
- `agent24-components` 拥有 `ComponentLock` 类型和通用下载器：GitHub 主机白名单、大小上限、sha256 校验、取消、进度、每个组件一把安装锁。iDoris 和 Hyphae 都改用它，`fetch_verified` 不再借用 iDoris 的锁。
- **托管根**统一为 `~/.agent24/components/<id>/<version>/`（jason 2026-11-01 拍板）。现状对照：
  - Hyphae 已在 `state_dir/components/hyphae/<tag>`，只差把 `state_dir` 钉到 `~/.agent24`；
  - Open Design 已在 `~/.agent24/components/open-design/<odSha7>-<platform>-<arch>/`。它的版本目录名**保留**这个形式（同一 odSha 换 Electron 版本会换原生模块，目录必须区分），锁里 `version` 记 odSha；
  - iDoris 现在在 `~/Library/Application Support/Agent24/idoris/<version>`，`install.rs` 的目录权限（0o700）和符号链接拒绝都绑在这个根上。**迁移做法：不搬，重下。** r4 订正（§7.6）：不是「第一次启动自动重下」，而是**下一次用户显式点安装时**重下；只有 release 和下载缓存搬到新根，状态文件留在原处；旧目录只登记、不删。模型文件不在这里（在 `~/.omlx/models/`），不受影响。
  - 下载缓存的根与权限见 §7.8（r4 补充）。
- 现在的 schema 1 锁文件要能读：先在加载时转换成 schema 2，迁移完成后再删掉转换。

### D2a 签名验证（v1 就做）

- **macOS**：
  - 锁里有 `signing.macos_team_id` 时，下载后、安装前必须验证**每个可执行文件**：Developer ID 签名有效、团队 ID 与锁一致、启用了 Hardened Runtime；`notarized: true` 时还要验证公证。任何一项不过，都拒绝安装，不留下半安装的目录。
  - **「每个可执行文件」的定义**：`assets.<platform>.executables` 列出的文件；不填时是 `files` 里所有 Mach-O 文件，加上 `.app` / `.framework` / `.xpc` 目录。单文件用 `codesign --verify --strict -R '=anchor apple generic and certificate leaf[subject.OU] = "<team_id>"'`；bundle 加 `--deep`。Hardened Runtime 看 `codesign -dv` 输出的 `flags=…(runtime)`。公证用 `spctl --assess`，**裸可执行文件用 `--type install`，`.app` bundle 用 `--type execute`**（r4 订正，2026-11-02 实测：`--type execute` 对已公证的裸 CLI 二进制一律拒绝，报「does not seem to be an app」；`--type install` 返回 `accepted, source=Notarized Developer ID`，未 staple 的已公证裸二进制也通过）。不用 `stapler validate`（它只验票据存在，不是 Gatekeeper 判定）。配置文件、模型、数据文件只靠 sha256。`spctl` 的实测依据与判定条件见 §7.5；锁的过渡、签名流程和 `unsigned_sha256` 的产出方式见 §7.4。
  - 实现推广 `agent24-idoris/src/install.rs` 现有的 `SignatureVerifier` / `Codesign`，归到 `agent24-components`。它现在做的是 `codesign --verify --strict` 加团队 ID 要求，并读取 `codesign -dv` 的详情，只验 `idoris` 一个文件，`check_platform` 只接受 `aarch64` macOS。要改三处：按 `executables` 逐个验；加 Hardened Runtime 和 `spctl`；`check_platform` 放开到所有 macOS 架构（darwin-x64 现在会直接 `UnsupportedPlatform`）。
  - 我们自己发布的 macOS 资产（iDoris、Hyphae 等）必须填 `signing`，不填锁就不合法。
- **签名与可复现构建的关系**：签名会改变字节。锁里 `assets.<platform>.sha256` 是**签名后**制品的摘要，下载校验用它。可复现比对在**签名前**做：发布 CI 记录签名前的摘要到 `reproducible.unsigned_sha256.<platform>`，复现 CI 用同一配方构建后和它比；或者本地 `codesign --remove-signature` 后再哈希。两者都写进锁，各比各的，不混用。Hyphae 现在的锁哈希的是未签名裸二进制，签名后要同时换 `sha256` 和填 `unsigned_sha256`。
- **Linux / 其他平台**：没有对应的系统签名，靠锁里的 sha256，加上可复现构建（`reproducible`，CI 复现比对，Hyphae 已在做）。
- **AgentEar**：Agent24 不下载、不锁，所以 D2/D2a 不适用。Agent24 对 AgentEar 二进制**没有任何完整性校验**，只有 attach 握手的 `manifest_digest`。这是 v1 接受的缺口，由 AgentEar 自己的安装器负责制品校验（ADR-K1-03 §4 的摘要校验在它那边做）；AgentEar 改为由 Agent24 下载时再补。
- **不在 v1 里做**：第三方发布者的信任根、密钥轮换、已发布版本的吊销。这些放到 ADR-K1-03 / ME-6。v1 的信任只来自「锁里写死的 sha256 和团队 ID」，锁随 Agent24 代码一起评审。

### D3 契约版本：语义化的 major/minor，握手时检查

- 每个进程外组件都要声明自己对外的契约名和版本 `major.minor`。规则：major 变化表示不兼容；minor 只能做加法；未知字段必须忽略。
- Agent24 在锁里写下可以接受的 `major` 和 `min_minor`。版本检查在**第一次交互时**进行，按传输方式不同（见 D4），不能等到业务调用出错才发现。
  - 常驻型：每次启动后、接业务调用前；
  - 按次调用型 CLI：安装时和每次 daemon 启动时各跑一次 `contract`，不是每次调用都跑；
  - 远端服务：每次建立连接时。
- 版本不兼容时，统一报错 `component_contract_mismatch`，带上 `{id, expected, actual}`。组件状态置为 `incompatible`，不尝试降级运行。
- 在版本检查之前，组件不得接到任何业务调用。
- **进程内组件免本条运行时检查**：它和本体一起编译，失配编不过去。只要 D9 清单和 manifest `version`。
- **MCP 免本条**：见 D1。

### D4 一份契约，多种运行形态的绑定

不强求统一成一种传输方式。每种运行形态都实现同一组语义：版本、能力、健康、错误、取消。

| 绑定 | 版本与能力从哪里来 | 现有对应 |
|---|---|---|
| **HTTP** | `GET /health` 或 `GET /capabilities`（组件已有的同类端点），返回 `contract_version`；能力清单以锁里的 `provides` 为期望，组件端点返回的为实际（D9 权威顺序） | iDoris `/health`（`supervisor.rs` `validate_health` 读 `contract_version`；iDoris **没有** `/capabilities` 端点，能力列表待 iDoris 补）、Knowledge `/system/capabilities` |
| **CLI** | 子命令 `<bin> contract`：stdout 输出一个 JSON 对象（含 `contract_version`、`provides`），退出码 0 | Hyphae（新增 `contract`、`health` 两个子命令） |
| **JSON-RPC attach / 领域 OS** | 版本来自 `initialize.protocol_versions` 区间；**能力清单来自 manifest（D9）**。`initialize.capabilities` 是模块向内核**请求**的 `events/models/…`，对应 `requires.kernel`，不是提供项 | `os-proto` initialize（AgentEar、Sin90/Cos72），不改 |
| **进程内** | 组件 crate 导出 D9 清单常量，启动时登记到 `/api/v1/components`；无运行时版本检查（D3） | `agent24-domain` DomainModule 挂载缝 |
| **MCP** | 版本：MCP `initialize` 的 `protocolVersion`（只记录，不比对 `major.minor`）；能力：`tools/list` 自动生成 D9 清单 | `agent24-mcp` |
| **sidecar** | 托管进程就绪后，走 HTTP 绑定或 ACP 握手；安装状态由桌面上报（D5） | Open Design（`CreativeServeWeb` / `agent24 acp`），目标是 `SidecarManager` |

CLI 绑定额外规定（和 `agent24-comm/src/runner.rs` 现有约定一致）：
- JSON 输出由环境变量 `HYPHAE_OUTPUT=json`（泛化为 `<ID>_OUTPUT=json`）打开，**不加 `--json` 参数**；`contract`、`health` 是普通子命令；
- 每次调用的 stdout 只能输出一个 JSON envelope：`{ "ok": true, "data": … }` 或 `{ "ok": false, "error": { "code", "message", "retryable" } }`；告警走 stderr；
- 退出码只区分成功（0）、业务错误（1）、契约或用法错误（2）（见 §7.1 订正：Agent24 只按 envelope 的 `ok` 判断，组件保留自己的退出码语义）；
- stdin 用来传秘密（现有 `--password-stdin` 模式），秘密不得出现在 argv 或环境变量里。

### D5 生命周期与健康

所有组件对 Agent24 都呈现同一组状态：

`absent | installing | installed | starting | ready | degraded | stopping | stopped | incompatible | failed`

允许的转移：

| 从 | 到 | 触发 |
|---|---|---|
| absent | installing | 用户确认安装 |
| installing | installed | 下载、sha256、D2a 验签全过 |
| installing | failed / absent | 任一项不过（不留半安装目录，所以也可以直接回 absent） |
| installed | starting | 托管启动 |
| installed | absent | 卸载 |
| installed | installing | 升级（改锁） |
| starting | ready | 健康探测首次通过，且 D3 版本检查通过 |
| starting | incompatible | D3 失配 |
| starting | failed | 启动超时或进程退出 |
| ready | degraded | 健康探测连续失败 N 次（N 锁里可配，默认 3） |
| degraded | ready | 健康探测恢复 |
| ready / degraded | stopping | 收到停止请求；或 daemon 退出 |
| ready / degraded | failed | 进程崩溃（非我方停止） |
| stopping | stopped | 进程在宽限期内退出，或被 SIGKILL |
| stopped | starting | 重启 |
| failed | starting | 重试 |
| failed | installing | 重装 |
| incompatible | installing | 锁更新后重装 |

`incompatible`、`failed` **不是终态**：它们需要人工或升级干预才能离开，但离开的路是定义好的。

各运行形态只用其中的子集：
- 常驻型（iDoris serve、Hyphae daemon、sidecar）：全集。
- 按次调用型 CLI：没有 `starting / ready / degraded / stopping / stopped`，只用 `absent / installing / installed / incompatible / failed`。
- attach（AgentEar）：没有 `absent / installing / installed`（Agent24 不管安装）；未连接显示为 `stopped`，连接中 `starting`，握手过 `ready`，连接断开回 `stopped`。
- 远端服务（Knowledge）：没有 `absent / installing / installed / stopping`；未配置显示为 `stopped`，连接失败 `failed`，探测失败 `degraded`。
- MCP：`installed / starting / ready / failed / stopped`。

与现有三套状态的映射（C3 实现时用这张表，不另造）：

| 现有 | 来源 | 映射到 |
|---|---|---|
| `DrainState::Starting` | `os-proto/drain.rs` | starting |
| `DrainState::Running` | 同上 | ready |
| `DrainState::Draining`、`Revoked` | 同上 | stopping |
| `SidecarState` `stopped / starting / ready / degraded / stopping / failed` | `sidecar-contract.ts` | 同名 |
| `SidecarState::healthy` | 同上 | ready |
| `OpenDesignComponentState` `not-installed` | `open-design-component.ts` | absent |
| `paused`（断点续传中） | 同上 | installing |
| `installed / failed` | 同上 | 同名 |
| `unavailable`（本构建没带 manifest、平台不匹配） | 同上 | absent，附 `reason` |

健康与停止：
- 常驻型组件由 Agent24 托管，必须提供健康探测：HTTP 绑定用 `/health`，CLI 绑定用 `<bin> health`，attach 绑定用连接存活。
- 停止时先发 SIGTERM，等宽限期，之后才允许 SIGKILL。宽限期锁里可配（`stop_grace_ms`），**默认 15 秒**（jason 2026-11-01 拍板）。组件要保证在宽限期内落盘。
  - 现状：`agent24d/src/lifecycle.rs` `GRACE_DEFAULT_MS = 500`、`GRACE_MAX_MS = 5_000`；`agent24-comm::stop_grace()` 默认 5 秒，env `A24_MODULE_STOP_GRACE_MS`。C3 要做三件事：`GRACE_MAX_MS` 抬到 **≥ 15 000**；两处默认值统一为 15 000；env 名统一为 `A24_COMPONENT_STOP_GRACE_MS`（旧名保留一个版本做别名）。
  - 本条是**停组件**的宽限。单次调用超时后的取消宽限另见 D6（2 秒）。

状态暴露：
- 状态统一通过 `GET /api/v1/components` 对客户端暴露。能力中心、通信页、本地模型页都从这里读状态，不再各写一套（现有 `/api/v1/idoris/status`、`/api/v1/comm`、`/api/v1/os` 保留为兼容别名，内部读同一份）。
- 这条路由登记 CORE-2 `Operation::ComponentsRead`，`AssuranceLevel::Standard`，**不进** `creative_allowlist`。
- **按 audience 脱敏**：host audience 看全量；其他 audience 只返回 `{ id, version, state, provides }`。安装路径、pid、端口、错误详情（`message` 可能带 relay URL）、团队 ID、commit 只给 host。
- **Open Design 的状态由桌面上报**：Electron main 的 `open-design-component.ts` 和 `CreativeServeWeb` 掌握 OD 的安装与运行状态，daemon 不知道。桌面通过 `POST /api/v1/components/{id}/report`（host audience 专用，`Operation::ComponentsReport`）把状态推给 daemon，daemon 只汇总。v1 不把 OD 的下载迁进 daemon；迁移另立任务。

### D6 错误、超时与取消

- 错误统一成 `{ code, message, retryable }` 三元组。`code` 用组件自己的命名空间，例如 `hyphae.relay_unreachable`。Agent24 只认 D3/D5 定义的通用码，其余原样透传给上层；`message` 不得包含秘密，脱敏规则见 D5。
- 每次调用都有超时，由 Agent24 设定。超时后：
  - HTTP 绑定断开连接；
  - CLI 绑定先发 SIGTERM，**2 秒**后发 SIGKILL（这是单次调用的取消宽限，不是 D5 的停组件宽限）；
  - JSON-RPC 绑定发 `$/cancelRequest`（`os-proto/module.rs` 现有名字）。
- 超时后结果算「未知」，处理方式沿用 ADR-K1-01 §2.5：会改变状态的调用不自动重试。

### D7 授权

- 组件能调用的宿主接口 = 锁 / 清单里 `requires` 的声明 ∩ 宿主愿意给的（`Grants::granting` 现有逻辑）∩ 用户按 ADR-K1-03 确认的。三者任一缺失即拒绝。令牌由 CORE-2 按组件发放，组件不得持有 legacy 全权 token。
- `provides` **不参与授权**。它只用于能力中心展示和技能依赖解析（D9）。
- 组件暴露给 agent 的工具，走 ADR-K1-01 的模块工具契约。本 ADR 不重复规定。
- 安装、升级走现有系统确认框。**首次启用**的同意内容与记录按 ADR-K1-03 §1（逐工具列出可读 / 可改 / 可外发 / 风险）和 §4（第一方、第三方、升级重同意），本 ADR 不另设。只读的状态查询不需要确认。

### D8 升级与替换

- 升级同一个组件：改锁，跑契约兼容用例（§3），通过后才能合入。major 升级必须同时提交适配层改动和迁移说明。
- 换成另一个品牌、同类的组件：只要它实现了同一个契约名和 major，Agent24 就不改代码，只改锁。做不到时，在 `agent24-components` 外面写一个适配器，把它包成同一个契约名。不允许改调用方。
- 两个组件同时提供同一能力 id 时（例如两个 OCR 引擎），解析规则见 D9。

### D9 组件清单：组件提供哪些能力

每个组件都要有一份清单。需要下载的组件，清单就是锁里的字段；随本体构建的组件，清单编译进二进制；MCP 组件的清单由 `tools/list` 自动生成。清单内容：
- 组件 `id`、`version`、显示名（中、英、泰）；
- `provides`：它提供的**能力**列表。每项有能力 `id`、显示名（三语）、一句话说明（三语），以及这项能力暴露的工具名（工具本身走 ADR-K1-01）；
- `requires`：它需要本体给的权限（`kernel`、`operations`），见 D2；
- `skills`：它附带的技能列表（ADR-039 §2.2 领域包）。v1 只占位为 `[]`，内容由 ADR-040 定。

**能力 id 规则**：
- 字符集 `[a-z0-9][a-z0-9-]{1,31}`，全局唯一。
- 在 Agent24 仓库 `docs/capabilities/REGISTRY.md` 登记：id、三语名、首个提供者、登记日期。锁合入时 CI 校验 `provides[].id` 都已登记且没有两个锁登记同一个 id 为「首个提供者」。
- MCP 的 `mcp-<server>` 不进登记表（用户自配，Agent24 管不到）。
- 工具名按 ADR-K1-01 为 `<组件 id>.<operation>`；能力只**引用**工具名，不另起名。MCP 工具沿用 `mcp_{server}_{tool}`，不改现有代码。

**三份清单的权威顺序**（锁、组件运行时报告、本体编译进的）：
- 锁 / 编译进的 `provides` 是**期望**，随 Agent24 代码一起评审；
- 组件运行时报告的（HTTP `/capabilities`、CLI `contract`、MCP `tools/list`）是**实际**；
- 实际 ⊂ 期望：期望里多出的能力标 `unavailable`，技能依赖它时提示「该组件当前版本不提供」；
- 实际 ⊃ 期望：多出的忽略，不展示、不可被技能依赖；
- 期望里某能力的工具名在实际里找不到：`component_contract_mismatch`，状态 `incompatible`。

**同一能力有多个提供者**：
- `~/.agent24/components.json` 记录用户为每个能力 id 选的默认提供者；
- 没选时，技能依赖解析**拒绝**并提示用户选，不自动挑；
- 能力中心按能力展示时，一项能力下列出所有已安装的提供者，默认者打标。

用途：
- 能力中心的「能力」栏从 `GET /api/v1/components` 读清单，按能力展示，不按组件展示；
- 技能按能力 `id` 声明依赖（ADR-039 规则 2）。缺哪项能力，本体就能告诉用户该装哪个组件（查登记表的「首个提供者」）。

## 3. 验收与反例测试

每个组件迁到 v1 时，必须带上这些测试，并证明修复前失败：

1. 锁里的 sha256 和下载的资产不一致 → 拒绝安装，不留下半安装的目录。
2. 对端报的 major 不一致，或 minor 低于 `min_minor` → 状态变成 `incompatible`，不发任何业务调用。
3. 对端返回未知字段 → 照常工作。
4. 调用超时 → 按 D6 取消（CLI：SIGTERM 后 2 秒 SIGKILL），状态变化可观测，不自动重试会改变状态的调用。
5. 常驻组件健康探测失败 → `degraded`，恢复后回到 `ready`；进程崩溃 → `failed`，重试后回到 `ready`。
6. 秘密只经 stdin 传递：在 argv、环境变量、日志里都找不到。
6a. macOS：团队 ID 不符、签名被篡改、缺 Hardened Runtime、`spctl` 不过（`notarized: true` 时）→ 拒绝安装；`executables` 里任一文件不过即整体拒绝。
6b. 签名后制品的 sha256 与锁一致，签名前的 `unsigned_sha256` 与复现构建一致；两者各比各的。
7. 用一个 mock 的第二实现跑通全部用例，证明替换边界成立。
8. 运行时报告的 `provides` 少于锁 → 缺的标 `unavailable`；多于锁 → 多的不展示；工具名对不上 → `incompatible`。
9. 两个组件提供同一能力 id 且用户未选默认 → 技能依赖解析拒绝并提示。
10. 非 host audience 读 `GET /api/v1/components` → 看不到路径、pid、端口、错误详情；Creative 令牌读该路由 → 拒绝。
11. 停组件：SIGTERM 后 15 秒内退出 → `stopped`，没有 SIGKILL；超过 15 秒 → SIGKILL，`stopped`。

## 4. 迁移顺序

| 片 | 内容 | 涉及仓库 |
|---|---|---|
| C1 | 在 `agent24-components` 里加 `ComponentLock`（schema 2，`provides` / `requires` / `assurance`）、通用下载器和签名验证（D2a，推广 `SignatureVerifier`：按 `executables` 逐个验、Hardened Runtime、`spctl`、放开架构）；`fetch_verified` 改用它；iDoris 锁升到 schema 2（行为不变，L1–L6 移到 `assurance`），iDoris 迁到新托管根（重下，旧目录标记可删）。**依赖方向反转**：现在 `agent24-components` 依赖 `agent24-idoris`，C1 后 `agent24-idoris` 依赖 `agent24-components`；`run_install_child` 隐藏子命令随下载器一起搬，仍由宿主二进制（`agent24d`）分发，不另起启动器 | Agent24 |
| C1b | Hyphae 发布流水线加 Developer ID 签名与公证；锁同时换 `sha256`（签名后）和填 `reproducible.unsigned_sha256`（签名前）；Agent24 侧对 Hyphae macOS 资产启用 D2a。**排在 Hyphae 发出签名版之后**，可与 C2 同期 | Hyphae + Agent24 |
| C2 | Hyphae 新增 `contract`、`health` 子命令（`HYPHAE_OUTPUT=json` 下输出 envelope），统一 envelope 和退出码。Hyphae 锁**仍是 schema 1、不加字段**；Agent24 在 `agent24-comm` 代码里写死对 Hyphae 的契约期望（`name=hyphae-cli`、`major=1`、`min_minor=0`），`contract` 字段等 C1b 的 schema 2 锁再进锁（§7.4）。**排在 Hyphae relay 上线之后**（jason 拍板） | Hyphae + Agent24 |
| C3 | 新增 `GET /api/v1/components`（含 D9 清单、audience 脱敏、`Operation::ComponentsRead`）和 `POST /api/v1/components/{id}/report`；统一状态机（D5 转移表与映射表）；`GRACE_MAX_MS` 抬到 ≥15 000、默认值与 env 名统一；能力中心、通信页、本地模型页改为读这个接口。**过渡期**：C2 之前 Hyphae 没有 `health`，它的状态由现有 pid 存活判定，只有 `ready / stopped / failed`，没有 `degraded` | Agent24 |
| C4 | AgentEar：只在 `/api/v1/components` 里登记（attach 子集状态），握手不变；D2/D2a 不适用（见 D2a 缺口说明）；补 §3 用例 2、3、4 | Agent24 |
| C5 | Knowledge：把 `/system/capabilities` 映射到 D3/D5（远端服务子集） | Agent24 |
| C6 | Documents：补 D9 清单；等它独立发版时再补 D2 的锁 | Agent24 |
| C7 | 领域 OS、进程内 DomainModule、MCP：补 D9 清单（MCP 自动生成），状态接入 C3；`REGISTRY.md` 建表 | Agent24 |
| C8 | Open Design：桌面经 `POST /api/v1/components/{id}/report` 上报安装与运行状态；随 `SidecarManager` 落地迁到 sidecar 绑定。OD 下载迁进 daemon 不在 v1 | Agent24 |

C1 最先做：它不改任何外部行为，而且能马上去掉 `fetch_verified` 借用 iDoris 锁的问题。C1b、C2 要和 Hyphae 一起做，排在 relay 上线并接好线之后。

## 5. 后果

- **好处**：
  - 接一个新组件，只需写一份锁、选一种绑定，测试照 §3 抄；
  - 换品牌只改锁或者加适配器；
  - 三个页面共用一套状态，不再各说各话；
  - 能力 id 有登记表，技能依赖解析有唯一答案。
- **代价**：
  - Hyphae 要新增两个子命令，发布流水线要加签名；
  - iDoris 和 Hyphae 的锁格式迁移要过渡一段时间；iDoris 用户首次升级要重下一次；
  - `GET /api/v1/components` 和 `POST /api/v1/components/{id}/report` 是新的公开接口，要进 v1 契约测试；
  - `lifecycle.rs` 的宽限上限要抬，影响现有领域 OS 的停止时长。
- **风险**：契约名和 major 的划分如果太粗，「换品牌」仍然要写适配器。v1 接受这一点，不追求零适配。

## 6. 已拍板（jason，2026-11-01）

1. 托管根统一为 `~/.agent24/components/<id>/<version>/`（Open Design 的版本目录名保留 `<odSha7>-<platform>-<arch>`）。
2. 停止宽限期默认 15 秒（单次调用取消宽限 2 秒另计）。
3. 签名验证放进 v1（D2a）。信任体系（第三方发布者、密钥轮换、吊销）放到 ADR-K1-03 / ME-6。
4. Hyphae 的 `contract`、`health` 排在 relay 上线之后（C2）。

## 7. r4 补充（2026-11-02，与 Hyphae 对齐 C2 / C1b 时定下的细节）

本节补全 D2、D2a、D4、D5 没写的具体字段。以下内容由 Agent24 作为契约使用方确定，Hyphae 会话已确认，并按此实现 C2。

### 7.1 D4 退出码订正
D4 原文写「退出码只区分 0、1、2」，这和 Hyphae 现有的语义码冲突：Hyphae 用 2 表示 `network_error`，用 3 表示 `auth_error`。现订正为：**Agent24 只按 envelope 的 `ok` 判断成败，退出码只作参考**；组件保留自己现有的退出码语义。`ok:true` 时退出码必须是 0。

### 7.2 CLI `contract`（D3、D4）
`<ID>_OUTPUT=json <bin> contract` 的 `data` 字段：
- `name`：契约名，例如 `hyphae-cli`。
- `contract_version`：字符串 `"major.minor"`，从 `"1.0"` 开始。删除字段或命令、改名、改变语义时升 major；只做加法时升 minor。
- `binary_version`：二进制自报的版本。Agent24 **不依赖**它等于 tag，版本判断只看锁里的 sha256 和 `contract_version`。Hyphae 在 C1b 之前固定输出 `"dev"`，C1b 起由发布配方用 `-X` 注入真实 tag，锁里的 `recipe` 同步改。
- `provides`：能力 id 字符串数组。
- `commands`：`[{path, flags}]`，`path` 是完整的子命令路径（如 `"identity export"`），`flags` 只写带 `--` 的主名。组件可以输出全部可见子命令，Agent24 按 path 查找，多出的忽略。这项用来替代解析 `--help` 的能力探测。
- `error_codes`：`[{code, exit}]`，必须是全集。
- 以上全部必填。输出必须确定：`commands` 按 path 排序，`flags`、`error_codes`、`provides` 按字典序。Agent24 解析 JSON，不做逐字比较。

### 7.3 CLI `health`（D5）
`data = {status, checks}`：
- `status` 取 `ok`、`degraded`、`failed` 之一。
- `checks.keystore = {ok, exists, encrypted, identities}`：只读元数据，不要口令，不加锁，不创建文件。
- `checks.relays = {ok, items:[{url, reachable, latency_ms?, error?}]}`：检查配置里的全部 relay，并行；单个超时 3 秒，整条命令上限 5 秒。`error` 只能取 `timeout`、`refused`、`dns`、`tls`、`other`。
- `checks.daemon = {running}`：可选，读取时不能有副作用，检测不了就省略。
- **只要产出了报告，envelope 就是 `ok:true`，退出码为 0**，即使 `status` 是 `degraded` 或 `failed`。只有体检本身没能跑起来时才返回 `ok:false`。
- 状态映射（**只对常驻型组件**，即由 Agent24 托管 Hyphae daemon 的情形；按次调用型 CLI 没有 `ready / degraded`，D5 已定，`health` 对它只是诊断信息）：
  - `health` 的结果是 D5「健康探测连续失败 N 次 → degraded」那条规则的**输入**，不直接改状态：`status:ok` 算一次探测成功；`degraded` 或 `failed` 算一次探测失败，连续 N 次（锁里可配，默认 3）才从 `ready` 进 `degraded`，一次成功就回 `ready`。
  - `checks.daemon.running=false` 时映射为 `stopped`（daemon 没在跑，不是健康问题），并与 pid 存活判定取一致。
  - 为什么 `failed` 不直接映射成 D5 的 `failed`：`health` 返回了报告就证明 CLI 本身能跑；`status:failed` 说的是 relay 不可达或 keystore 有问题，这是「组件在跑但服务降级」，不是「组件崩了」。D5 的 `failed` 只留给进程崩溃和启动失败。
  - `ok:false`（体检没跑起来）按 D6 的错误处理，同样只计一次探测失败。

### 7.4 锁的过渡（D2、D2a）
- C2 那一版的 Hyphae 锁保持 schema 1，**不加任何字段**（Agent24 用严格解析读 Hyphae 锁）。schema 1 带不了 `contract`，所以 **C2 期间 Agent24 把对 Hyphae 的契约期望写死在 `agent24-comm` 代码里**：`name=hyphae-cli`、`major=1`、`min_minor=0`；D3 的握手检查照常跑，只是期望值来自代码常量而不是锁。锁要到 C1b 的 schema 2 才有 `contract` 字段，届时删掉代码常量。
- **锁文件归 Agent24**：`rust/crates/agent24-comm/hyphae.lock.json` 仍由 Agent24 拥有，做法和今天一样——逐字节复制 Hyphae release 里发布的 `hyphae.lock.json`，Agent24 侧不改内容，只在 PR 里说明来源。C1b 起复制的是 schema 2 版本。
- C1b 签名版直接发 schema 2：
  - **release 资产仍是裸二进制** `hyphae-darwin-arm64`（以及 `hyphae-linux-x64`），和现在一样；tar.gz 只是附加的打包形式，**不进锁**；
  - `assets.<platform>.sha256` 记**签名后的裸二进制**的摘要，下载校验用它；
  - `reproducible.unsigned_sha256.<platform>` 记签名前的哈希。**这是对 D2a 的一处明确修订**：D2a 原文要求「发布 CI 记录签名前的摘要」，Hyphae 现在没有能签名的 CI，改为由 Hyphae 本地两次独立构建、结果逐字节一致后产出这个值，复现 CI 比对的仍是它；
  - `signing = {macos_team_id: "HYAP444FN9", notarized: true}`。
- 签名位置（jason 2026-11-02 拍板）：在 jason 的笔记本上，用 Agent24 出包的同一套凭据（Developer ID HYAP444FN9，notary profile `apple-notary-HYAP444FN9`），凭据不离开本机。
- 签名流程：
  1. Hyphae 产出未签名资产，建 draft release；
  2. Agent24 在本机给裸二进制签名（`codesign --force --options runtime --timestamp`，纯 Go 二进制不需要 entitlement），zip 后提交 notarytool 公证；签名后的裸二进制替换 draft release 里的同名资产（tar.gz 若要附带也用签名后的二进制重新打），再把 sha256 回传给 Hyphae；
  3. Hyphae 填写 schema 2 锁，重新生成 SHA256SUMS，转为正式 release；
  4. Agent24 把发布的 `hyphae.lock.json` 原样复制进 `agent24-comm`。
- 裸 Mach-O 不能 staple，这不影响校验：D2a 对裸可执行文件用 `spctl --assess --type install`，会在线查公证票据；2026-11-02 实测未 staple 的已公证裸二进制通过（`accepted, source=Notarized Developer ID`），而 `--type execute` 对任何裸 CLI 二进制都报「does not seem to be an app」拒绝，所以 D2a 已订正为按文件类型选 `--type`。
- 排期（jason 2026-11-02）：C2 先做，C1b 紧随其后。

### 7.5 D2a 公证判定：按文件类型选 `spctl --type`（C1 实现中发现）
- 裸可执行文件：`spctl --assess --type install <file>`，结果必须同时满足 `accepted` **和** `source=Notarized Developer ID`；少一项都拒绝安装。
- `.app` bundle：`spctl --assess --type execute <bundle>`，判定条件相同。
- 实测（2026-11-02）：`--type execute` 对**任何**裸 CLI 二进制都拒绝，包括系统自带的 `/usr/bin/true`，报「does not seem to be an app」；所以 D2a 原文「公证只用 `--type execute`」对裸二进制不可用，已订正。未 staple 的已公证裸二进制走 `--type install` 通过（在线查票据）。
- `executables` 里每一项按它是文件还是 bundle 分别选 `--type`。

### 7.6 D2 迁移：不自动重下，显式安装时才重下
- D2 原文「C1 落地后第一次启动把新根建好、重新下载并验签」会绕过安装同意的凭据（ADR-K1-03：安装是用户一次性确认的动作，自动重下没有这份确认）。订正为：**在用户下一次显式点「安装」时重下**。
- 切换规则：旧副本在新副本校验（sha256 + D2a）通过之前**继续服务**；新安装失败（下载、校验、验签任一不过）时保留旧副本，不留半安装目录，状态不变。

### 7.7 D2 `executables` 必填
- 去掉「不填等于 `files` 里所有 Mach-O / ELF」的默认值：隐式枚举会让锁里没写的文件被静默验签或静默跳过，评审时看不出来。
- `executables` 必填；每一项必须出现在同一平台的 `files` 里，否则锁不合法（加载时拒绝）。没有可执行文件的资产（纯数据）写 `[]`。

### 7.8 托管根下的下载缓存
- 下载缓存的根是 `~/.agent24/components/<id>/`（和 release 目录同根）；当安装目录不归我们管、不能 chmod 时（例如组件自己掌握安装目录），改用 `~/.agent24/components/<id>-cache/`。
- 缓存根必须以 0700 创建，属主是当前用户；不满足就拒绝使用，不降级。
- 现状：Hyphae 的缓存原在 `<state_dir>/idoris/component-cache/hyphae`（COMM-HYPHAE §3），C1 起因为上述权限原因搬到 `components/hyphae-cache/`。

### 7.9 iDoris 的迁移边界
- 只有 **release 目录和下载缓存**搬到 `~/.agent24/components/idoris/`。
- 状态不搬：`installation.json`、`data/`、`instances/`、`operations.json` 留在原处（`~/Library/Application Support/Agent24/idoris/`）。任何 gc 都必须排除这些状态文件。
- 旧 release 路径登记到 `legacy-install.deletable.json`，**不删**；删除另由用户触发的 `agent24 idoris gc` 执行，且只删登记过的路径。

### 7.10 开放项（r4 登记，不阻塞 C1）
- iDoris 的 schema 2 锁现在是 `notarized: false`。打开它要先解决：tar.gz 里的裸二进制没有 staple，`spctl --type install` 需要在线查票据，离线首次安装会失败；要定「在线查不到时是拒绝还是降级为只验签名」的规则，再把 `notarized` 置 true。作为 C1 之后的跟进项。
