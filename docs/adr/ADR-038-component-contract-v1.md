# ADR-038：组件契约 v1

索引：[架构总入口](../architecture/README.md)・[ADR 索引](README.md)

- 状态：**已接受**（r5 订正稿，2026-11-05）。r2 按 ADR-039 修订：范围扩大到所有运行形态，加入签名验证，加入组件清单；r3 按 Fable 评审修订；r4 补充见 §7；r5 把 §7 的订正回写进正文，C1–C8 全部完成，见下方「r5 订正记录」。2026-11-12 只回写与代码不符的现状（D3 的 iDoris 映射、D10、§4 的 PR 号与 C9、§7.11 的锁版本），决策未改
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

## r5 订正记录（2026-11-05）

r5 不改任何决策，只做三件事：把 §7（r4）的订正回写进 D2 / D2a / D4 / D5；删掉与实现矛盾的旧句；对仍未接线的条款明确标注。依据是 R3 §1.A.1.5 的 16 条（分析报告 `docs/architecture/review-2026-11-03/R3-ADR-AND-DOCS.md`，在分支 `docs/arch-review-r3-adr-docs`）和 [架构优化方案](../tracking/2026-11-04/ARCH-OPTIMIZATION-PLAN.md) S0-2。jason 2026-11-04 拍板：Q1 = (b)（Open Design 由 `SidecarManager` 托管，#1580、#1584）；Q13 = (b)（D6 正文改为直接 SIGKILL 进程组）；Q15（OS 包纳入同一完整性机制，见 D10）。

| R3 # | 原问题 | 处置 | 落点 |
|---|---|---|---|
| 1 | 状态行「草案 r3」落后正文 | 状态改为「已接受」，写明 r5 | 文首 |
| 2 | D2 写「schema 1 要能读」，§7.11 已删 | 删除该句，改写为「schema 1 一律拒读」（`rust/crates/agent24-components/src/lock.rs:3-4`） | D2 末条 |
| 3 | D2a「`executables` 不填时是所有 Mach-O」与 §7.7 必填冲突 | 删默认值，写必填、且 ⊂ `files` | D2a |
| 4 | D2a「发布 CI 记录签名前摘要」与 §7.4 本地两次构建冲突 | 改为本地两次独立构建逐字节一致后产出 | D2a |
| 5 | `provides` 三语 `name`/`summary`/`tools` 与 §7.11「可选」冲突 | D2 示例与 D9 标可选；`tools` 缺失时 D9 的工具名比对跳过（Hyphae 现状），不判 `incompatible` | D2、D9 |
| 6 | `notarized` 示例可选，D2a「`true` 时还要验公证」；§7.11 已禁止关闭 | macOS 资产必须 `notarized: true`，写 `false` 直接拒绝（`sign.rs:62-66`） | D2、D2a |
| 7 | `component_contract_mismatch` / `incompatible` | 已落地一部分：Hyphae（C2）、Documents（C6）、AgentEar（C4）、Knowledge（C5）会产出。**iDoris 契约失配仍映射为 `failed`**（`component_routes.rs` 的 iDoris 分支没有 `Incompatible`），这是已知缺口，正文标注，归方案 S1-4，不阻塞接受 | D3 |
| 8 | `degraded` 没有来源 | 已落地：Hyphae 由 `hyphae health` 连续失败产出（C2/C3），Knowledge 缺能力产出（C5）。无需改正文，C 表已记录 | §4 |
| 9 | 映射表落点：Rust 映射函数是死代码，实际在 TS | D5 映射表标注「`downloading`/`verifying` 在桌面 `open-design-report.ts` 映射为 installing」；Rust `from_sidecar` / `from_open_design`（`state.rs:114,130`）只被测试引用，保留为对照用例，是否删除归 S1-7 | D5 |
| 10 | 宽限期「锁里可配」但锁字段没有消费方 | Hyphae 已消费（§7.11）；iDoris 的同名字段未用。正文改为「按组件接入，iDoris 暂未」 | D5 |
| 11 | D6 CLI「先 SIGTERM，2 秒后 SIGKILL」，代码直接 SIGKILL 进程组 | 按 Q13 (b) 改正文：按次 CLI 不需要落盘，直接 SIGKILL 进程组（`agent24-comm/src/runner.rs:269-285`） | D6、§3 用例 4 |
| 12 | D7 授权按 `requires`，无代码读取 | D7 标「待接线」：`requires` 目前只被解析和校验（`lock.rs`、`documents_catalog.rs`），不参与授权，授权仍走 `Grants` 现有逻辑 | D7 |
| 13 | `skills: []` 占位、「技能（ADR-040 待写）」 | ADR-040 已存在，D1 改链接；`skills` 在锁里不合法（`deny_unknown_fields`），D9 改为「v1 不进锁，等 ADR-040 实施时再加」 | D1、D9 |
| 14 | §1 背景的 Hyphae「schema 1、无契约版本」已不成立 | §1 表头加注「r4 前现状」 | §1 |
| 15 | D2「iDoris 和 Hyphae 都改用 `agent24-components`」与 §7.11 `agent24-comm` 边界冲突 | D2 改为：Hyphae 的锁由 agent24d 解析，`agent24-comm` 只嵌入锁文本 | D2 |
| 16 | `AssuranceLevel::Everyday` 一致 | 无需改，备查 | — |

另外三处与 R3 清单无关的订正：
- **C8**：按 Q1 (b) 和 #1580、#1584 的实际实现重写，见 §4。
- **D10（新增）**：OS 包（domain OS 包）纳入同一完整性机制，对应 Q15，实现留给第 1 期。
- **`docs/capabilities/REGISTRY.md`**：`documents` 行的契约来源改指 `rust/apps/agent24-documents/component.json`。

2026-11-12 核对（`origin/main` `bb03081e8`）在 r5 记录之上补了两件当时还没发生的事，不改上表：S1-4（#1605）已把 iDoris 的 `ContractMismatch` 映射为 `incompatible`（`rust/apps/agent24d/src/component_routes.rs` 的 `from_idoris`）；D10 已由 S1-9 落地，见该条现状。

2026-11-13：ADR-038-C9 完成（A #1825、B #1830、C #1833、D #1831），见 D7 与 §4 的 C9 行。

## 1. 背景

Agent24 在 ADR-038 起草时（r4 之前）交付能力的方式各不相同。下表是**当时**的现状，C1–C8 之后 Hyphae 已是 schema 2 并有契约版本，Open Design 由 `SidecarManager` 托管，见 §4：

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
- Open Design：sidecar，打包模式由 `SidecarManager` 托管（C8，#1580、#1584，2026-11-04）；`CreativeServeWeb` 只选启动方式和下载前置。Rust 的 `agent24-sidecar-host` 不在产品路径，Windows 暂不支持（Q1 = (b)）。

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
- 技能（见 [ADR-040](ADR-040-skill-contract-v1.md)）；
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
      "tools": ["hyphae.dm_send", "hyphae.dm_list"] }  // name、summary、tools 均可选；Hyphae 的锁只带 id
  ],
  "requires": { "kernel": ["events"], "operations": [] },
  "assurance": []
}
```

- `schema`、`id`、`version`、`source`、`contract`、`release_url_base`、`assets`、`provides`、`requires` 是必填字段（`lock.rs:15-37`）。可选字段另有 `health_failures_before_degraded`（D5 健康探测连续失败次数，缺省 3，消费方夹到 1–10）和 `stop_grace_ms`。`signing` 的必填规则见 D2a（macOS 资产必须填，且 `notarized` 必须为 `true`，写 `false` 直接拒绝）；`reproducible` 可以不填，填了就要校验；`provides[].name`、`summary`、`tools` 可选（r4 §7.11）；示例里的 `// …` 只是注释，真实锁是纯 JSON；示例里的 `version`、`commit`、哈希等都是示例值，实际见 `rust/crates/agent24-comm/hyphae.lock.json`；`assurance` 是 iDoris 那种保障等级（L1–L6 带来源），不是能力，可以为空。
- **字段名**：提供的能力叫 `provides`，需要本体给的权限叫 `requires`。**锁和清单里不再有叫 `capabilities` 的字段**；ADR-039 §4.1 的消歧规则同样适用于这里。
- `provides[].id` 是**能力 id**（D9），技能按它声明依赖。`requires.kernel` 对应 `DomainOsManifest.kernel_capabilities`（`events/models/scheduler/policy/memory/approval`），`requires.operations` 对应 CORE-2 的 `Operation`。
- `assets.<platform>.executables` 列出 D2a 要验签的文件。**r4 订正（§7.7）：必填，不再有「不填等于全部 Mach-O / ELF」的默认值，且每一项必须出现在 `files` 里。**
- 平台键统一用 `<os>-<arch>`（`darwin-arm64`、`linux-x64` …）。某个平台没有资产，就不写这个键，不写 `null`。Rust triple（`aarch64-apple-darwin`）在需要时转成这个形式；锁里不写 `null` 键。
- `agent24-components` 拥有 `ComponentLock` 类型和通用下载器：GitHub 主机白名单、大小上限、sha256 校验、取消、进度、每个组件一把安装锁。iDoris 直接用它；Hyphae 的锁由 agent24d 解析（r4 §7.11）：`agent24-comm` 不依赖带 reqwest 的 `agent24-components`，只嵌入锁文本，comm 的测试以 dev-dependency 使用同一解析器。`fetch_verified` 已删除。
- **托管根**统一为 `~/.agent24/components/<id>/<version>/`（jason 2026-11-01 拍板）。现状对照：
  - Hyphae 已在 `state_dir/components/hyphae/<tag>`，只差把 `state_dir` 钉到 `~/.agent24`；
  - Open Design 已在 `~/.agent24/components/open-design/<odSha7>-<platform>-<arch>/`。它的版本目录名**保留**这个形式（同一 odSha 换 Electron 版本会换原生模块，目录必须区分），锁里 `version` 记 odSha；
  - iDoris 现在在 `~/Library/Application Support/Agent24/idoris/<version>`，`install.rs` 的目录权限（0o700）和符号链接拒绝都绑在这个根上。**迁移做法：不搬，重下。** r4 订正（§7.6）：不是「第一次启动自动重下」，而是**下一次用户显式点安装时**重下；只有 release 和下载缓存搬到新根，状态文件留在原处；旧目录只登记、不删。模型文件不在这里（在 `~/.omlx/models/`），不受影响。
  - 下载缓存的根与权限见 §7.8（r4 补充）。
- **schema 1 一律拒读**（r4 订正，§7.11）：`lock.rs:3-4`，没有转换层。原计划的「加载时转换」随 Hyphae 锁升到 schema 2 取消。

### D2a 签名验证（v1 就做）

- **macOS**：
  - 锁里有 `signing.macos_team_id` 时，下载后、安装前必须验证**每个可执行文件**：Developer ID 签名有效、团队 ID 与锁一致、启用了 Hardened Runtime，并验证公证。锁里 `notarized` 必须为 `true`，写 `false` 的锁直接以 `ManifestViolation` 拒绝，安装和启动都一样（`sign.rs:62-66`，§7.11）。任何一项不过，都拒绝安装，不留下半安装的目录。
  - **「每个可执行文件」的定义**：`assets.<platform>.executables` 列出的文件，**必填**，每一项必须在同平台的 `files` 里（§7.7）；没有默认值，纯数据资产写 `[]`。单文件用 `codesign --verify --strict -R '=anchor apple generic and certificate leaf[subject.OU] = "<team_id>"'`；bundle 加 `--deep`。Hardened Runtime 看 `codesign -dv` 输出的 `flags=…(runtime)`。公证用 `spctl --assess`，**裸可执行文件用 `--type install`，`.app` bundle 用 `--type execute`**（r4 订正，2026-11-02 实测：`--type execute` 对已公证的裸 CLI 二进制一律拒绝，报「does not seem to be an app」；`--type install` 返回 `accepted, source=Notarized Developer ID`，未 staple 的已公证裸二进制也通过）。不用 `stapler validate`（它只验票据存在，不是 Gatekeeper 判定）。配置文件、模型、数据文件只靠 sha256。`spctl` 的实测依据与判定条件见 §7.5；锁的过渡、签名流程和 `unsigned_sha256` 的产出方式见 §7.4。
  - 实现推广 `agent24-idoris/src/install.rs` 现有的 `SignatureVerifier` / `Codesign`，归到 `agent24-components`。它现在做的是 `codesign --verify --strict` 加团队 ID 要求，并读取 `codesign -dv` 的详情，只验 `idoris` 一个文件，`check_platform` 只接受 `aarch64` macOS。要改三处：按 `executables` 逐个验；加 Hardened Runtime 和 `spctl`；`check_platform` 放开到所有 macOS 架构（darwin-x64 现在会直接 `UnsupportedPlatform`）。
  - 我们自己发布的 macOS 资产（iDoris、Hyphae 等）必须填 `signing`，不填锁就不合法。
- **签名与可复现构建的关系**：签名会改变字节。锁里 `assets.<platform>.sha256` 是**签名后**制品的摘要，下载校验用它。可复现比对在**签名前**做：由 Hyphae 本地两次独立构建、逐字节一致后产出签名前摘要，写入 `reproducible.unsigned_sha256.<platform>`（§7.4），复现 CI 用同一配方构建后和它比（`codesign --remove-signature` 后整文件哈希**不**等于未签名构建，实测不可行，见 §7.11 (a)）。两者都写进锁，各比各的，不混用。Hyphae 的锁在 C1b（v0.30.0）已换成签名后的 `sha256` 并填了 `unsigned_sha256`。「签名资产 = 这份构建重签」目前 CI 不证明，靠 Developer ID 加公证，code-page 比对是后续项（§7.11 信任假设 (a)）。
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
  - 落地情况（2026-11-12）：Hyphae、Documents、AgentEar、Knowledge 按此产出。iDoris 的 `/health` 在 `ContractMismatch` 时也是 `incompatible` / `component_contract_mismatch`（S1-4，#1605；`from_idoris`，`component_routes.rs:159-166`）。其它 `Failed` 仍映射为 `failed`。
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
| **sidecar** | 托管进程就绪后，走 HTTP 绑定或 ACP 握手；安装状态由桌面上报（D5） | Open Design（`SidecarManager` 托管 / `agent24 acp`） |

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
- 远端服务（Knowledge）：没有 `absent / installing / installed / stopping`；未配置显示为 `stopped`，连不上、超时或 HTTP 非 2xx 为 `failed`（一次即 `failed`，不适用常驻型「连续失败 N 次才 degraded」），缺 search/read 为 `degraded`。有凭据时，能力声明兼容且完整之后再探 §8 就绪（`POST /agent24/knowledge/ready`，R8.2）：`live` 且 `search`、`read` 都 `ready` 才是 `ready`（host 的 `detail.basis` 为 `ready`）；就绪探针失败、非 2xx、`not_ready` 或 `disabled`、回显不符，统一为 `degraded`（reason `not_ready`）。没有凭据不探 §8，`ready` 只表示能力声明完整且契约兼容（`detail.basis` 为 `capabilities`），不代表依赖健康（READONLY R9.3）。能力声明为 `incompatible` 的服务，D3 要求在版本检查通过前不接业务调用：search/read 读同一份探测缓存，`incompatible`、缺该操作、或还没拿到能力声明时在本地拒绝（`incompatible` / `not_ready`），不发远端请求。
- MCP：`installed / starting / ready / failed / stopped`。

与现有三套状态的映射。**落点（r5）**：OD 的 `not-installed / paused / downloading / verifying / unavailable` 由桌面 `open-design-report.ts` 映射后再上报；Rust 的 `state.rs` `from_sidecar` / `from_open_design`（`:114,130`）没有产品调用，只被 `state/tests.rs` 引用，不是权威映射，是否删除归 S1-7（休眠代码收窄）；进程组终止原语统一见 S2-6：

| 现有 | 来源 | 映射到 |
|---|---|---|
| `DrainState::Starting` | `os-proto/drain.rs` | starting |
| `DrainState::Running` | 同上 | ready |
| `DrainState::Draining`、`Revoked` | 同上 | stopping |
| `SidecarState` `stopped / starting / ready / degraded / stopping / failed` | `sidecar-contract.ts` | 同名 |
| `SidecarState::healthy` | 同上 | ready |
| `OpenDesignComponentState` `not-installed` | `open-design-component.ts` | absent |
| `paused`（断点续传中） | 同上 | installing |
| `downloading`（下载中） | 同上 | installing |
| `verifying`（校验中） | 同上 | installing |
| `installed / failed` | 同上 | 同名 |
| `unavailable`（本构建没带 manifest、平台不匹配） | 同上 | absent，附 `reason` |

健康与停止：
- 常驻型组件由 Agent24 托管，必须提供健康探测：HTTP 绑定用 `/health`，CLI 绑定用 `<bin> health`，attach 绑定用连接存活。
- 停止时先发 SIGTERM，等宽限期，之后才允许 SIGKILL。宽限期锁里可配（`stop_grace_ms`），**默认 15 秒**（jason 2026-11-01 拍板）。组件要保证在宽限期内落盘。接入情况（r5）：Hyphae 守护进程已消费锁里的值（§7.11），iDoris 锁的同名字段暂未使用，其余走全局默认。
  - C3a（#1545）已完成：`lifecycle.rs` `GRACE_DEFAULT_MS = 15_000`；env 名统一为 `A24_COMPONENT_STOP_GRACE_MS`，旧名 `A24_MODULE_STOP_GRACE_MS` 作别名；`shutdown_bounds.rs` `GRACE_MAX_MS = 30_000`。
  - 本条是**停组件**的宽限。单次调用超时后的取消宽限另见 D6（2 秒）。

状态暴露：
- 状态统一通过 `GET /api/v1/components` 对客户端暴露。能力中心、通信页、本地模型页都从这里读状态，不再各写一套（现有 `/api/v1/idoris/status`、`/api/v1/comm`、`/api/v1/os` 保留为兼容别名，内部读同一份）。
- 这条路由登记 CORE-2 `Operation::ComponentsRead`，`AssuranceLevel::Everyday`，**不进** `creative_allowlist`。
- **按 audience 脱敏**：host audience 看全量；其他 audience 只返回 `{ id, version, state, provides }`。安装路径、pid、端口、错误详情（`message` 可能带 relay URL）、团队 ID、commit 只给 host。
- **Open Design 的状态由桌面上报**：Electron main 的 `open-design-component.ts` 和 `CreativeServeWeb` 掌握 OD 的安装与运行状态，daemon 不知道。桌面通过 `POST /api/v1/components/{id}/report`（host audience 专用，`Operation::ComponentsReport`）把状态推给 daemon，daemon 只汇总。v1 不把 OD 的下载迁进 daemon；迁移另立任务。

### D6 错误、超时与取消

- 错误统一成 `{ code, message, retryable }` 三元组。`code` 用组件自己的命名空间，例如 `hyphae.relay_unreachable`。Agent24 只认 D3/D5 定义的通用码，其余原样透传给上层；`message` 不得包含秘密，脱敏规则见 D5。
- 每次调用都有超时，由 Agent24 设定。超时后：
  - HTTP 绑定断开连接；
  - CLI 绑定**直接对进程组发 SIGKILL**，再有界等待回收（`agent24-comm/src/runner.rs:269-285` `kill_and_reap`，回收窗口 2 秒）。r5 按 jason 2026-11-04 的 Q13 (b) 订正：按次调用的 CLI 不需要落盘，现行行为稳定，不引入 SIGTERM 阶段。方案 S2-6 统一进程组终止原语时再评估；
  - 需要落盘的常驻组件走 D5 的停组件宽限（SIGTERM → 15 秒 → SIGKILL），与本条无关；
  - JSON-RPC 绑定发 `$/cancelRequest`（`os-proto/module.rs` 现有名字）。
- 超时后结果算「未知」，处理方式沿用 ADR-K1-01 §2.5：会改变状态的调用不自动重试。
- **关停时的模型调用**：daemon 关停后，模块和 attach 模块在途的模型调用在 `drain + 2 秒`（本条的取消宽限，`lifecycle::CALL_CANCEL_GRACE`）被取消，早于 D5 的 15 秒停组件宽限。所以进程外包在停止宽限期内**不能**再发起模型调用，这是有意的：宽限期只用来落盘，不用来继续推理；attach 模块不会收到 SIGTERM，更不该被 15 秒拖住。

### D7 授权（**已接线**）

> r5 当时：`requires` 只被解析和校验，没有授权路径读取它。2026-11-12 ADR-038-C9 已接线（A #1825、B #1830、C #1833、D #1831）：内核能力经解析器进入三处 `Grants::granting`；`GET /api/v1/components` 展示挂载后的实际授予。开发态微信桥改为组件令牌，不再持有 legacy 全权令牌。第三方同意卡仍不做。

- 组件能调用的宿主接口 = 锁 / 清单里 `requires` 的声明 ∩ 宿主愿意给的（`Grants::granting` 现有逻辑）∩ 用户按 ADR-K1-03 确认的。三者任一缺失即拒绝。令牌由 CORE-2 按组件发放，组件不得持有 legacy 全权 token。本期只有开发态微信桥拿组件令牌。
- `provides` **不参与授权**。它只用于能力中心展示和技能依赖解析（D9）。
- 组件暴露给 agent 的工具，走 ADR-K1-01 的模块工具契约。本 ADR 不重复规定。
- 安装、升级走现有系统确认框。**首次启用**的同意内容与记录按 ADR-K1-03 §1（逐工具列出可读 / 可改 / 可外发 / 风险）和 §4（第一方、第三方、升级重同意），本 ADR 不另设。只读的状态查询不需要确认。
- 订正（C9 方案 §2.3，2026-11-12）：**第一方签名、随桌面安装的模块，其内核能力（`requires.kernel` ∩ 宿主上限）以「启用」为同意，不另设确认；宿主只在能力中心与 `GET /api/v1/components` 展示实际授予了哪些。第三方与手动安装的模块不适用本句，仍按 §1、§4 逐项确认。** 本句的 §1、§4 指 ADR-K1-03。它只收窄内核能力，不取消逐工具同意。

### D8 升级与替换

- 升级同一个组件：改锁，跑契约兼容用例（§3），通过后才能合入。major 升级必须同时提交适配层改动和迁移说明。
- 换成另一个品牌、同类的组件：只要它实现了同一个契约名和 major，Agent24 就不改代码，只改锁。做不到时，在 `agent24-components` 外面写一个适配器，把它包成同一个契约名。不允许改调用方。
- 两个组件同时提供同一能力 id 时（例如两个 OCR 引擎），解析规则见 D9。

### D9 组件清单：组件提供哪些能力

每个组件都要有一份清单。需要下载的组件，清单就是锁里的字段；随本体构建的组件，清单编译进二进制；MCP 组件的清单由 `tools/list` 自动生成。清单内容：
- 组件 `id`、`version`、显示名（中、英、泰）；
- `provides`：它提供的**能力**列表。每项有能力 `id`（必填）；显示名（三语）、一句话说明（三语）、这项能力暴露的工具名（工具本身走 ADR-K1-01）都是可选（r4 §7.11）。没写 `tools` 的，下面「工具名对不上」的比对跳过；
- `requires`：它需要本体给的权限（`kernel`、`operations`），见 D2；
- `skills`：它附带的技能列表（ADR-039 §2.2 领域包）。**r5：v1 不进锁**（锁用 `deny_unknown_fields`，没有该字段；API 也不输出），等 ADR-040 实施时再加。

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

### D10 OS 包的完整性（Q15）

- 领域 OS 包（Sin90、Cos72 等）与其他组件**用同一套完整性机制**：包带文件清单和每个文件的哈希，加载前逐个校验。**哈希不对就拒绝加载**，不做「先只记录」的过渡期（jason 2026-11-04 拍板）。
- 校验对象是 D2 的 `files` / sha256 概念在 OS 包上的对应物，不另造格式；包的清单仍是 `domain-os.yml`，清单里的 `provides` / `requires` 照 D9。签名要求（D2a）是否适用于 OS 包，由第 1 期设计时定，不在本条里预设。
- 现状（2026-11-12 核对，`origin/main` `bb03081e8`）：S1-9 已完成（#1614 至 #1780）。包带 `files.json` 与逐文件 sha256，加载前校验，哈希不对即拒绝（`hash_mismatch`），没有「先只记录」的过渡。五类拒绝原因是 `integrity_manifest_missing`、`hash_mismatch`、`integrity_manifest_invalid`、`signature_rejected`、`integrity_check_failed`（`rust/crates/agent24-components/src/integrity.rs:92-110`）。macOS 验签可 `agent24 os trust-team` 增加受信 Team（#1780；`rust/apps/agent24-cli/src/main.rs:2479`）。os-sdk 0.2.0 在握手前读磁盘 `domain-os.yml` 做清单自检，`manifest_digest` 用这份字节（#1730，tag `agent24-os-sdk-v0.2.0`；`rust/crates/agent24-os-sdk/Cargo.toml` 版本 `0.2.0`；`rust/crates/agent24-os-sdk/src/module.rs:132-147`）。子任务 PR：设计 #1611、r2 #1723；PR-1 #1614；N6 #1730；N3 #1737、#1733；N4 #1739；`--arch` #1750、#1751；PR-3a #1754；PR-3b #1753；PR-3c #1756；PR-3d #1780。握手本身仍发 `manifest_digest`（`os-proto`），文件级清单是它上面的这一层。

## 3. 验收与反例测试

每个组件迁到 v1 时，必须带上这些测试，并证明修复前失败：

1. 锁里的 sha256 和下载的资产不一致 → 拒绝安装，不留下半安装的目录。
2. 对端报的 major 不一致，或 minor 低于 `min_minor` → 状态变成 `incompatible`，不发任何业务调用。
3. 对端返回未知字段 → 照常工作。
4. 调用超时 → 按 D6 取消（CLI：直接 SIGKILL 进程组并有界回收），状态变化可观测，不自动重试会改变状态的调用。
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
| C1 | 在 `agent24-components` 里加 `ComponentLock`（schema 2，`provides` / `requires` / `assurance`）、通用下载器和签名验证（D2a，推广 `SignatureVerifier`：按 `executables` 逐个验、Hardened Runtime、`spctl`、放开架构）；`fetch_verified` 改用它；iDoris 锁升到 schema 2（行为不变，L1–L6 移到 `assurance`），iDoris 迁到新托管根（重下，旧目录标记可删）。**依赖方向反转**：现在 `agent24-components` 依赖 `agent24-idoris`，C1 后 `agent24-idoris` 依赖 `agent24-components`；`run_install_child` 隐藏子命令随下载器一起搬，仍由宿主二进制（`agent24d`）分发，不另起启动器。**已完成**（#1539，2026-11-02） | Agent24 |
| C1b | Hyphae 发布流水线加 Developer ID 签名与公证；锁同时换 `sha256`（签名后）和填 `reproducible.unsigned_sha256`（签名前）；Agent24 侧对 Hyphae macOS 资产启用 D2a。**排在 Hyphae 发出签名版之后**，可与 C2 同期。**已完成**（Agent24 侧 #1551，2026-11-02；启动验签拒绝信号 #1552，2026-11-02） | Hyphae + Agent24 |
| C2 | Hyphae 的 `contract`、`health`（`HYPHAE_OUTPUT=json` envelope）已接入 Agent24。启动时按锁里的 schema 2 `contract`（`name=hyphae-cli`、`major=1`、`min_minor=0`、`tested=1.0`）做一次握手：不兼容则组件 `incompatible`（reason `contract_mismatch`）且不启动 daemon；兼容但 minor 高于 `tested` 只记日志。daemon `running` 期间每 45 秒调一次 `hyphae health`（不拿 keystore 锁，调用有超时）；relay 等探测连续 3 次失败才把组件标 `degraded`，一次成功回到 `ready`。单次 failed 不改变状态。**已完成**（Hyphae 侧锁升级 #1546，2026-11-02；Agent24 侧握手与 `hyphae health` 探测 #1574，2026-11-04） | Hyphae + Agent24 |
| C3 | 新增 `GET /api/v1/components`（含 D9 清单、audience 脱敏、`Operation::ComponentsRead`）和 `POST /api/v1/components/{id}/report`；统一状态机（D5 转移表与映射表）；`GRACE_MAX_MS` 抬到 ≥15 000、默认值与 env 名统一；能力中心、通信页、本地模型页改为读这个接口。C3b：桌面上报 + 心跳 30s。Hyphae 的 `degraded` 已由 `hyphae health` 接入（见 C2）：进程 `running` 且已挂探测时，首份探测成功前是 `starting`（D5「健康探测首次通过才 ready」；L1），头几份就失败也按连续 N 次进 `degraded`；未挂探测（fixture、旧路径）仍直接映射为 `ready`。N 取锁的可选字段 `health_failures_before_degraded`（缺省 3，范围夹到 1–10）；一次成功回到 `ready`。host 视图 `detail.health` 带各探测项的状态和连续失败次数；redacted 视图没有这些。**已完成**（C3a #1545，C3b #1549，均 2026-11-02；Hyphae `degraded` 随 #1574） | Agent24 |
| C4 | AgentEar：只在 `/api/v1/components` 里登记（attach 子集状态），握手不变；D2/D2a 不适用（见 D2a 缺口说明）；补 §3 用例 2、3、4。**已完成**（#1572，2026-11-03）：已登记模块出现在 `GET /api/v1/components`（`form=attach`，attach 子集，清单 version，`provides` 留空——无 D9 目录）；协议区间失配记 `incompatible`，握手线仍是 `version_mismatch`；超时发 `$/cancelRequest` 且不重试。未注册不补 `not_attached` 占位 | Agent24 |
| C5 | Knowledge：把 `/system/capabilities` 映射到 D3/D5（远端服务子集）。**已完成**（#1571，2026-11-03）：已配置 binding 在一个总时限内并发探测该端点，结果按 TTL 缓存且同一指纹只打一次；契约 major 不兼容为 `incompatible`，缺 search/read 为 `degraded`（`missing_capability:<名>`），连不上、超时或 HTTP 非 2xx 为 `failed`（一次即失败，不是连续 N 次），未配置为 `stopped`。有凭据时再探 §8 就绪（R8.2）：`live` 且 search/read 都 `ready` 才是 `ready`（`detail.basis` 为 `ready`），否则 `degraded`（`not_ready`）；没有凭据不探，`ready` 只表示能力声明完整、契约兼容（`detail.basis` 为 `capabilities`），不代表依赖健康（READONLY R9.3）。D3 业务调用闸：search/read 在本地读同一份探测缓存，`incompatible`、缺该操作、没拿到声明时不发远端请求。闸门只看这次调用所属 endpoint（base URL）的那一份读数，缓存里除合并视图外还存逐 URL 的结果，一个 binding 的失败或失配不会拒绝另一个 binding 的调用；组件列表仍显示合并视图。闸门读到本 endpoint 的 `failed` 时，缓存只沿用 4 秒，不是 30 秒。**§8 就绪探测会让服务端留下审计记录（READONLY R10）**，探测频率受 TTL 约束：每个已激活且有凭据的 endpoint 至多每 30 秒一次（`failed` 的短窗口只重探能力声明，只有声明兼容才发 §8）；同一轮探测由 singleflight 合并，组件列表与闸门共用 | Agent24 |
| C6 | Documents：补 D9 清单。**已完成**（#1570，2026-11-03）：清单是模块自带的 `rust/apps/agent24-documents/component.json`，编译进 agent24d（不写入 `domain-os.yml`，那份是握手 manifest）。`/components` 用清单的 provides 与 version；形态按 D5 为常驻。D2 的锁等独立发版再补。**D9 expected-vs-actual 已落地**：`version` 显示 mount report 的运行时版本，清单版本在 host 的 `detail.expected_version`；版本不一致**不改状态**（按需下载的包可能比本体新或旧），只在 `detail` 加 `actual_version` 和 `update_available: true`；manifest 声明了该能力却缺清单里的工具名，为 `incompatible` / `component_contract_mismatch`（`detail` 带 `expected`/`actual`/`missing`），且只覆盖实时状态为 `ready` 或 `degraded` 的读数；清单期望的能力不在 manifest 的非空 `provides` 里，该项标 `availability: unavailable`，manifest 多出的能力忽略。manifest 没声明 `provides`（Documents 现状）不做能力比对 | Agent24 |
| C7 | 领域 OS、进程内 DomainModule、MCP：补 D9 清单（MCP 自动生成），状态接入 C3；`REGISTRY.md` 建表。**已完成**（#1575，2026-11-03）：登记表见 `docs/capabilities/REGISTRY.md` | Agent24 |
| C8 | Open Design：桌面经 `POST /api/v1/components/{id}/report` 上报安装与运行状态；打包模式迁到 `SidecarManager`（TS）托管。**已完成**（2026-11-04，#1580 主片，#1584 e2e 跟进；jason 拍板 Q1 = (b)，设计见 [C8-SIDECAR-DESIGN](../tracking/2026-11-04/C8-SIDECAR-DESIGN.md)）。Rust 的 `agent24-sidecar-host` 这次不接：打包模式的进程由 `SidecarManager` 托管（`open-design-sidecar.ts`：launcher / health / POSIX 进程组 tree controller），上报的运行态由 manager 状态派生（`healthy` 映射为 `ready`，`degraded` 照实报）。崩溃后懒重启，自动有界重启另开一片。SIGTERM / SIGINT / SIGHUP 由 Electron 转为 app.quit，经 before-quit / will-quit 共享同一次 22 s 有界 stop，不使用 128+n 退出码（只在 macOS 打包版上实测；应用就绪前或主进程卡死时收到的信号不保证走到退出流程，遗留进程按崩溃处理）。degraded 时再次打开视图会先停止旧实例再启动新实例。运行目录记录 launcher pgid；下次启动时发现上次留下的 launcher 只记日志、不发信号（pid 文件加 ps 快照证明不了信号落地时组号还属于我们）；新实例照常启动并覆盖 pgid 记录，所以应用崩溃或被 SIGKILL 后遗留的 OD 进程在 sidecar-host 接管前只能手工清理。重复退出事件仍等待同一次有界 stop，不会因第二次信号立即退出。另一个已知风险：强杀走 POSIX 进程组，组长退出后，「组非空」探测和 SIGKILL 之间组号可能被复用，没有原子手段消除；普通桌面负载下概率很低，彻底解决要靠生命周期绑定的监督进程（sidecar-host，Linux 可用 pidfd）。SIGKILL / 崩溃无法执行退出处理；身份无法确认（含 launcher 已退出仅剩子进程）时宁可保留，绝不猜测误杀。checkout 模式仍在 `CreativeServeWeb`（可选的 PR-2）。OD 下载迁进 daemon 不在 v1 | Agent24 |
| C9 | D7 `requires` 授权接线：内核能力经解析器进入三处 `Grants::granting`；`GET /api/v1/components` 展示挂载后的实际授予。开发态微信桥只拿写死的 dev-bridge 档 `ComponentRuntime` 令牌，不是从 `requires` 推出来的，不再持有 legacy 全权 token。第三方同意卡仍不做。**已完成**（ADR-038-C9：A #1825、B #1830、C #1833、D #1831）。方案见 [C9-REQUIRES-AUTHZ-PLAN](../tracking/2026-11-12/C9-REQUIRES-AUTHZ-PLAN.md) | Agent24 |

**C1–C9 全部完成**（r5，2026-11-05；C4–C7 的 PR 号于 2026-11-12 补记：#1572、#1571、#1570、#1575；ADR-038-C9 为 #1825、#1830、#1833、#1831）。没有挂账的迁移片。仍开放的缺口：code-page 比对（§7.11 (a)）、`agent24 idoris gc`（CLI 尚无该子命令，§7.9）。iDoris 契约失配已由 S1-4（#1605）映射为 `incompatible`。D10 已由 S1-9 落地。D7 已接线，见该条。

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

## 6. 已拍板（jason，2026-11-01 与 2026-11-04）

1. 托管根统一为 `~/.agent24/components/<id>/<version>/`（Open Design 的版本目录名保留 `<odSha7>-<platform>-<arch>`）。
2. 停止宽限期默认 15 秒（单次调用取消宽限 2 秒另计）。
3. 签名验证放进 v1（D2a）。信任体系（第三方发布者、密钥轮换、吊销）放到 ADR-K1-03 / ME-6。
4. Hyphae 的 `contract`、`health` 排在 relay 上线之后（C2）。
5. （2026-11-04）Q1 = (b)：Open Design 由 `SidecarManager` 托管，C8 按 #1580、#1584 收口。
6. （2026-11-04）Q13 = (b)：D6 的 CLI 超时直接 SIGKILL 进程组。
7. （2026-11-04）Q15：OS 包纳入同一完整性机制，哈希不对即拒绝加载（D10）。
8. （2026-11-04）接受本 ADR 为「已接受」。

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
  - **「连续」按整份 health 样本计数，不按单个 check 计数。** 一次 `hyphae health` 调用是一份样本；命令没跑起来（超时、崩溃、没有可读报告）也是一份样本。`status:ok` 把聚合失败次数清零；`status:degraded`、`status:failed`，或命令本身失败，各把聚合次数加一。同一份样本里多个 check 同时失败只加一。下一份样本换成另一个 check 失败（先 `relays` 后 `keystore`）仍接在同一个聚合次数上，不因换了 check 而从零重计。单个 check 自己的连续失败次数只写进宿主视图，不单独触发 `ready → degraded`。
  - `checks.daemon.running=false` 时映射为 `stopped`（daemon 没在跑，不是健康问题），并与 pid 存活判定取一致。
  - 为什么 `failed` 不直接映射成 D5 的 `failed`：`health` 返回了报告就证明 CLI 本身能跑；`status:failed` 说的是 relay 不可达或 keystore 有问题，这是「组件在跑但服务降级」，不是「组件崩了」。D5 的 `failed` 只留给进程崩溃和启动失败。
  - `ok:false`（体检没跑起来）按 D6 的错误处理，同样只计一次探测失败。

### 7.4 锁的过渡（D2、D2a）
- Hyphae 锁已是 schema 2（C1b），带 `contract`。Agent24 用严格解析读嵌入的 `hyphae.lock.json`，握手期望来自这份锁（`name=hyphae-cli`、`major=1`、`min_minor=0`、`tested=1.0`），不在 `agent24-comm` 里写死。C2 早期曾计划锁保持 schema 1、期望写死在代码里；schema 2 落地后那条不再适用。
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
- iDoris 的 schema 2 锁现在是 `notarized: true`（已落地）。规则：公证检查（`spctl --type install`，需在线查票据）只在**安装时**做（`VerifyMode::Install`）；启动、恢复、状态复查只做 sha256 与离线 codesign 检查（`VerifyMode::AtRest`），离线也能启动。离线首次安装会失败（下载本来就要网络，接受）。
- 例外：旧回执没有 `notarized_verified_at`（安装时公证校验的记录）时，启动只做 AtRest 检查，不会因此被拦；公证检查在**下一次显式 Install** 时补做并写入回执。

### 7.11 C1b 落地（Agent24 侧，2026-11-02）
- Hyphae 锁在 C1b 换成 v0.30.0 发布的 schema 2 锁（逐字节复制）。2026-11-12 核对已是 v0.30.1（#1582；`rust/crates/agent24-comm/hyphae.lock.json` 的 `version`），仍用 `ComponentLock` 严格解析。`hyphae.release.json` 删除，schema 1 读取（`migrate_schema1`）与 `fetch_verified` 一并删除。平台键仍只有 `darwin-arm64` 与 `linux-x64`。`agent24-comm` 只嵌入锁文本：它不能依赖带 reqwest 的 `agent24-components`（COMM-5b 边界），解析在 agent24d；comm 的测试以 dev-dependency 使用同一解析器。
- D2 订正两处，均为对已发布锁的兼容，不放宽安全判定：`provides[].name` / `summary` 改为可选（Hyphae 的锁只带 id）；`contract.tested` 与契约检查接受 `major.minor`（§7.2 CLI 的 `contract_version` 格式）和 `major.minor.patch` 两种写法。
- 验签点：安装（含重装已存在的副本）走 `VerifyMode::Install`；agent24d 每次构建 comm 状态（启动、安装后热重建）对实际执行的那份副本走 `VerifyMode::AtRest`。`GET /comm/daemon` 的状态复查只做 sha256，不跑 codesign（每次轮询起子进程不划算；sha256 等于锁里签名后的摘要已蕴含签名字节未变）。Linux 返回显式的 `UnsignedPlatform`。
- `stop_grace_ms`（15000）已按组件接入 Hyphae 守护进程的停止宽限。agent24d 的关停总预算仍按全局 `lifecycle::Params::stop_grace` 算，该值本身已被 `shutdown_bounds::GRACE_MAX_MS`（30s）封顶，`deadlines.modules` 就是 `drain + 这份宽限 + CONFIRM`。锁里的值不超过这份预算时用锁里的值（全局调得更长也不跟着加）；超过时截到预算，并打一行日志。不改 `shutdown_bounds` 的编译期断言（Hyphae 的 15s 在 30s 上限之内）。iDoris 锁没有 `stop_grace_ms`（2026-11-12 核对 `rust/crates/agent24-idoris/idoris.lock.json`），`agent24-idoris` 也不读该字段。
- 契约错误码：验签失败单独报 422 `hyphae_signature_rejected`，不再混进 `hyphae_hash_mismatch`（后者只表示字节与锁的 SHA-256 不符）。`NotNotarized` 和 spctl 超时（`NotarizationCheckFailed` 且为 `TimedOut`）提示「验证公证需要联网」；codesign 起不来或超时（`Io`）、spctl 起不来时 Gatekeeper 根本没跑，改提示「无法运行系统签名检查工具」，错误码同为 `hyphae_signature_rejected`（#1551 评审 L2）。安装接口只回固定文本，不带工具 stderr 和路径，完整错误只写 daemon 日志。
- 启动验签拒绝信号（Hyphae joint-ci 正向对照用）：agent24d 构建 comm 状态时若拒绝了找到的二进制，记下稳定原因而不是让槽位空着。`GET /api/v1/components` 主机视图里 Hyphae 为 `{state: "failed", reason: "signature_rejected"}`（AtRest codesign 要求/团队/Hardened Runtime 不过，或检查工具起不来）或 `reason: "hash_mismatch"`（sha256 与锁不符）；`GET /api/v1/comm/daemon` 同一原因放在 `data.rejection`（无拒绝时为 `null`）。`/api/v1/components` 的非主机视图只有 `{id, version, state, provides}`，`state: "failed"` 可见，`reason` 不可见；但 `GET /api/v1/comm/daemon` 的 `data.rejection` 对持 `comm.read`（Everyday）的调用方都可见。它只是固定码，不带路径和 stderr，所以可以接受。确实没装时仍是 `absent`。下一次不拒绝二进制的构建（启动成功、安装后热重建成功、或干脆没有二进制）清除该原因。构建失败但二进制没有被拒绝（指向的文件不存在、建目录 I/O 错误）不进 `data.rejection`：主机视图是 `{state: "failed", reason: "start_failed"}`（D5 把 `failed` 留给进程崩溃和启动失败，`absent` 只留给确实没装），非主机视图只看到 `failed`。这个标记和拒绝码一起清。
- `signing.notarized` 不能由锁关掉：macOS 资产的锁写 `notarized: false` 时，`signing_requirement` 直接以 `ManifestViolation` 拒绝（安装和启动都一样），不会降级成只验团队签名。
- **信任假设（r5，评审 M1 / L3）**：
  - (a) 签名资产与可复现构建的关系。CI 只证明 `reproducible.unsigned_sha256` 能从 `source.commit` 按配方复现。「锁里的签名资产 = 这份构建重新签名」这一点 CI 不证明，靠的是 Developer ID 签名（只有持有 HYAP444FN9 凭据的人能签）加 Apple 公证。后续要补的比对**不能**走「去签名后整文件比较」：Hyphae 2026-11-02 实测，`codesign --remove-signature` 的输出与未签名构建的 sha256 不相等。可行的路线是比较 code-page 哈希：取签名资产 CodeDirectory 里的各页哈希（不含签名本身所在的部分，如 `codesign -d --hash-sha256` 列出的页哈希），与对未签名构建按同样页大小算出的页哈希逐页比对，从而在 CI 里把「签名资产 = 这份复现构建签名而来」连起来。
  - (b) 威胁模型不含同 uid 攻击者。校验是按路径进行的（先打开文件算 sha256、再按同一路径跑 codesign 和 exec），校验与执行之间存在 path-vs-fd 的 TOCTOU 窗口；能以同一用户身份改写 `~/.agent24` 的进程本来就能直接篡改 agent24d 自身，不在 v1 防护范围内。现有缓解：已发布的 Hyphae 副本是 0500（只有属主可读可执行、任何人不可写，由 `VerifiedBinary` 写入后 chmod），下载缓存根是 0700 且属主必须是当前用户；每次启动都重算 sha256。这些只防误改和其它 uid，不防同 uid 的有意竞态。
