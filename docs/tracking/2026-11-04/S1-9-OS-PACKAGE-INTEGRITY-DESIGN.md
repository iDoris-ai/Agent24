# S1-9：OS 包文件级完整性设计

状态：设计稿，2026-11-05；本片不改代码。核对基线：`ce85cdffc1193195f9aebb93a8cbbf620ccfe526`。

**适用边界：** 包里自带的清单只能发现字节损坏、部分更新，以及挂载之后被人改过。如果攻击者能写包目录，他可以把文件和 `files.json` 一起改，单靠清单拦不住。这种情况靠两样东西：一是 Agent24 自己持有的可信锁（ComponentLock），锁里记着整包归档的 sha256；二是 §5 的签名。详见 §2「信任边界」。

## 1. 决策与现状依据

采用 jason 在本任务确认的 Q15 和 11-05 方案 A，并按 jason 11-05 的拍板对可执行文件要求签名（§5）：OS 包与组件共用完整性机制；先发布生成工具，再由 Sin90/Cos72 携清单发新版，最后 Agent24 强制拒绝。不设只记录模式、用户旁路或旧包宽限期。

计划版本差异：当前 §4.2 只列到 S1-8，Q15 仍写“先只记录不拒绝”（`docs/tracking/2026-11-04/ARCH-OPTIMIZATION-PLAN.md:221`、`:280`）。后续已接受的 D10 明确要求直接拒绝（`docs/adr/ADR-038-component-contract-v1.md:333`）；本片依本任务补充的七项要求落实设计，不能照旧推荐实现。

以下是现状，后文均为拟实施决策：

| 现状 | 出处 |
|---|---|
| `scan` 将成功和拒绝分开；包目录及 manifest 符号链接拒绝；读 YAML 原始字节后计算摘要 | `rust/crates/agent24-os-packages/src/discovery.rs:64`、`:136`、`:183`、`:214` |
| `recheck` 重读 manifest、比较 mount 时摘要；它明确不检查二进制、脚本和资产 | `rust/crates/agent24-os-packages/src/discovery.rs:261`、`:272`、`:286` |
| 摘要算法是 `sha256:` + YAML 原始字节 SHA-256；握手比较模块名及摘要 | `rust/crates/agent24-os-proto/src/manifest.rs:56`、`rust/crates/agent24-os-proto/src/initialize.rs:330` |
| daemon 将摘要存入 ModuleSpec，通过 PackageCheck 闭包调用 recheck；supervisor 在重启检查失败时进入 PackageChanged | `rust/apps/agent24d/src/domain.rs:2070`、`:2083`、`rust/crates/agent24-os-proto/src/supervisor.rs:696` |
| 指定的 domain.rs:2605、:2761 是测试中扫描 fixture 的位置，不能作为生产接线点 | `rust/apps/agent24d/src/domain.rs:2605`、`:2761` |
| D2 的 Asset 使用 `files: Vec<String>`、归档 `sha256`、`executables`；逐文件摘要实际是内部 `Manifest = BTreeMap<String, FileDigest>`，值为 `{size, sha256}` | `rust/crates/agent24-components/src/lock.rs:65`、`rust/crates/agent24-components/src/install.rs:194` |
| D2 从验证过的归档重建 expected，检查实际文件集合、大小、摘要及权限；不是信任安装目录里自报的摘要 | `rust/crates/agent24-components/src/install.rs:148`、`:156`、`:797` |
| D2 安装权限为目录 0700、可执行文件 0500、其余 0400 | `rust/crates/agent24-components/src/install.rs:605`、`:812`、`:835` |
| domain manifest 拒绝未知字段，不能直接给旧宿主塞入新字段 | `rust/crates/agent24-domain/src/lib.rs:450` |
| OS 拒绝已有 Failed + refused / RefusedFor 的映射；D5 已定义 failed，签名/摘要拒绝已有稳定 reason 先例 | `rust/apps/agent24d/src/component_routes.rs:608`、`:621`、`docs/adr/ADR-038-component-contract-v1.md:211`、`:483` |

## 2. 清单格式与摘要绑定

包根目录保持 `domain-os.yml` 为唯一业务 manifest，增加 `files.json`，直接序列化 D2 已有的 `Manifest` 映射，**不把 D2 的 files 路径数组改成对象数组**。将现有 `FileDigest`、安全路径检查和树校验提取为 components 的共享 API，OS 包使用同一实现；不得复制一套校验器。此处公开现有内部结构，不宣称它已经是稳定的外部 API。

`files.json` 示例（hash 为示意，真实文件必须是 64 位小写 hex）：

```json
{
  "bin/sin90": {"size": 123456, "sha256": "<64位小写hex>"},
  "config/default.json": {"size": 128, "sha256": "<64位小写hex>"}
}
```

`domain-os.yml` 新增一个可选、严格校验的 `integrity` 段，引用固定文件名，并沿用 D2 的字段语义：

```yaml
integrity:
  manifest: files.json
  sha256: <files.json原始字节的64位小写hex>
  files: [bin/sin90, config/default.json]
  executables: [bin/sin90]
```

规则：`files` 集合必须恰好等于 JSON 的键集合；executables 必填且无重复、是 files 的子集；包的 spawn.command 必须是其中一个包内相对路径，拒绝绝对路径、PATH 查找及外部解释器绕过。辅助可执行文件也须列入。逐平台分别生成清单，不把多平台文件混在同一个包里。

`domain-os.yml` 和 `files.json` 是仅有的两个控制文件，排除在 payload 的 files 列表之外；不允许其他排除项。这避免自哈希循环：先散列最终 payload → 写 files.json → 把其摘要写入 YAML → 对最终 YAML 算原有 manifest_digest。验证时先用同一次读取的 YAML 解析声明及算摘要，再验证 files.json 原始字节摘要，再校验 payload。两个控制文件本身也必须是普通文件，权限与数据文件一致。

握手算法和字段均不变；清单通过 YAML 中的摘要间接被 manifest_digest 覆盖。只改 JSON 即使其语义相同也失败；同时改 JSON 和 YAML 则冻结的 mount 摘要不匹配。模块必须从最终发布的 YAML 读取摘要，不能把生成前的摘要编译进二进制，以免循环依赖。

JSON 重复键、非法摘要、负 size、重复路径、未知字段均拒绝；路径遵循共享 D2 检查并拒绝绝对路径、空段、`..`、反斜线、控制字符及非 UTF-8 文件名。生成器按 UTF-8 路径字节排序，固定 JSON 缩进与末尾换行；验证器按原始字节验证，不先规范化 JSON。

信任边界：包自带清单只能发现字节损坏及 mount 后变化，攻击者同时替换完整包和摘要不能靠自哈希识别。Agent24 下载的包必须另有受信任的 ComponentLock，其归档 sha256 覆盖最终 YAML、JSON 及全部 payload；不能使用包内自带的锁作为信任根。手工复制包必须明确标为本地来源，不得展示为已验证发布者。v1 不承诺防同 uid 的校验到执行竞态，沿用 D2 威胁模型（`docs/adr/ADR-038-component-contract-v1.md:487`）。

## 3. 扫描、首次启动与重启

强制上线后，scan 只有在以下全过时才返回 Discovered：现有 manifest 语义校验、必需 integrity、两个控制文件、完整 payload 集合/大小/sha256、权限、D2a 签名要求。失败保留拒绝记录，不创建该包的 provider、路由或工具注册；单包失败不隐藏其他包。

recheck 重做同一全量校验，再比较冻结的 manifest_digest；不使用 mtime、大小或之前成功结果来跳过哈希。结果应保存机器可读原因，避免从错误字符串推断 reason。大文件流式散列，枚举/读取失败也拒绝；清单限制建议 1 MiB、10,000 项，payload 大小使用 D2 安装限制，超限清晰报错。

| 文件变化 | scan 与 recheck 的一致处理 |
|---|---|
| 多出文件（含隐藏文件）、缺文件、大小或 sha256 不符 | 拒绝；不允许 `.DS_Store` 特例 |
| 目录 | 只准 files 中路径必需的父目录；额外空目录也拒绝 |
| 根、控制文件、payload 或中间目录为符号链接 | 不跟随，拒绝；设备、FIFO、socket 同样拒绝 |
| 硬链接 | 生成/安装阶段拒绝多链接普通文件，避免包外别名；验证阶段 Unix 检查 nlink=1 |
| Unix 权限 | 沿用 D2 的目录 0700、executables 0500、其他文件 0400，拒绝任何偏差及 setuid/setgid；检查器不自动修复 |
| 非 Unix 权限 | 不假装存在 POSIX mode；执行入口须在 executables，类型/摘要照常验证，平台权限约束沿用共享实现 |

运行数据、日志、缓存写到包外 data_dir，不能边运行边修改发布树。生成工具在 staging 中规范化权限，安装器保留上述权限；禁止为了消除错误在运行时重新生成清单。

首次 spawn 之前也必须 recheck，封闭 scan → spawn 的时间间隔；现有 `domain.rs:2083` 闭包继续负责重启检查，但不能以“已有重启检查”为由遗漏首次启动。后续任何自动重启、手动重试和升级后启动都走相同检查。运行中不增加后台轮询；重启前发现变更即停止重启链，不执行篡改后的文件。仍有同 uid TOCTOU 窗口，不能描述为执行字节的原子证明。

## 4. 拒绝状态与桌面

保持现有 `GET /api/v1/components`，不新增路由；符合 ADR-045 只约束新路由及模块入口归 components 的规则（ADR-045 D1 适用范围、N9 模块类资源）。拟扩展 Refused/RefusalReason 与 supervisor 错误承载，使扫描拒绝和重启拒绝保留同一种类型化原因：

| 情形 | state / reason | 桌面主机视图提示 |
|---|---|---|
| 缺 integrity 或 files.json（旧包） | failed / integrity_manifest_missing | 包版本过旧，升级 Sin90/Cos72 后重试 |
| payload 或清单摘要不符、冻结摘要变化 | failed / hash_mismatch | 文件校验失败，请重装可信版本 |
| 非法清单、集合/类型/权限不符 | failed / integrity_manifest_invalid | 安装内容不完整或格式不合法，请重装 |
| 签名、Team ID、Hardened Runtime 或公证拒绝 | failed / signature_rejected | 发布签名验证失败，请安装可信版本 |
| I/O 无法完成校验 | failed / integrity_check_failed | 无法校验包，请检查访问权限后重试 |

不是 absent（磁盘有包），也不是 incompatible（后者留给契约版本失配）。校验失败不得仅打印日志而丢失组件行。manifest 损坏到无法取得可信 name 时，用宿主生成的目录身份展示拒绝项，不能让伪造 name 覆盖正常组件；不得把损坏 manifest 的 provides 当作可用能力。

桌面经现有组件列表读取 state/reason，按固定码显示上述提示；详情只给主机 audience，非主机保持 `{id, version, state, provides}` 脱敏，不输出绝对路径、stderr 或敏感内容。现有接口及 reason 类型检查见 `apps/desktop/src/renderer/components/componentApi.test.ts:41`、`:58`；本片不宣称桌面已有这些新码的提示。同步更新 OpenAPI/API 类型中必要的枚举或描述及桌面测试。旧 `/os` 接口保持兼容，读同一拒绝记录；不另造错误通道。

重装成功后必须重新扫描并全量验证才清除原因。对外操作错误按 D6 `{code,message,retryable}` 返回（契约出处：`docs/adr/ADR-038-component-contract-v1.md:279`），使用 `os_package.<reason>`、`retryable:false`，避免自动重试掩盖篡改；手动修复后可重试。

## 5. D2a 签名（jason 2026-11-05 已拍板：适用）

适用：Sin90/Cos72 是独立发布的可执行 OS 包，不应因进入 packages 目录而减弱组件校验。macOS 使用共享 SignatureVerifier，逐个验证 executables，要求受宿主发布策略/可信锁指定的 Team ID、Developer ID、Hardened Runtime、公证；不能让包任意自选信任团队。纯数据仅校验哈希；Linux/Windows v1 沿用 D2a 的摘要和可复现构建规则，不新造签名体系（`docs/adr/ADR-038-component-contract-v1.md:167`）。

签名必须在生成清单之前完成，因为签名改变字节；外部 CI 顺序为构建 → 签名/公证及所有会改字节的处理 → 生成清单 → 校验 → 打包 → 计算最终归档 sha256 → 发布并更新可信锁。安装与启动采用 D2a 各自模式，不另定公证离线容错策略。由于 macOS 签名只保护被签的代码，配置及清单的发布来源仍靠外部可信归档摘要，而不是签名替代文件哈希。

## 6. 生成工具与 A 的上线顺序

推荐 Agent24 CLI 子命令（未来接口，本片未实现）：

```sh
agent24 package-integrity generate --root ./dist/sin90 --executable bin/sin90
agent24 package-integrity verify --root ./dist/sin90
```

`--executable` 可重复；generate 只操作明确给出的发布 staging，读取已有 domain-os.yml，保留业务字段，拒绝符号链接/特殊文件，规范化权限、扫描全部 payload、生成 files.json 并最后原子替换 YAML；不接收任意 exclude 参数。每个文件使用临时文件加 rename，整个目录须由 release CI 独占；失败退出不能拿半成品发布。重复生成字节一致。verify 只读、始终严格，成功退出 0、失败非零并输出固定原因及相对文件名，不执行包代码。发布工具随 Agent24 版本提供，Sin90/Cos72 CI 固定工具版本与下载摘要，不用浮动 latest。

具体步骤：

1. **Agent24 PR-1** 发布工具、共享校验 API 和可选 integrity 解析支持。旧包暂沿用已有行为；出现 integrity 的包立即完整校验并拒绝坏包，不设“验出错仍放行”。这是新包生产的先决兼容支持，不是观测期。旧宿主因 deny_unknown_fields 无法读新字段，先发布支持该字段的 Agent24，并在外仓 release notes 声明最低宿主版本。
2. **Sin90/Cos72 外仓发布**：在各支持平台 CI 使用固定版本工具；发带清单、符合签名和权限规则的新版本；记录 tag、平台资产摘要及对应最低 Agent24 版本。必须实际下载并解包验过，不能只以 CI 配置已合并作为完成证据。其他被扫描的 OS 包也需清点；同仓随本体包在最终接线 PR 更新其打包产物，保留 D1 的免独立发布锁边界。
3. **Agent24 PR-3** 在上述资产可获取后打开必需清单拒绝，同步更新发布 pin/可信锁及 UI。缺清单旧包直接拒绝、提示升级；不自动给旧包补清单，不提供 env/feature 绕过。

“打开拒绝”不是改摘要算法，也不是在 `domain.rs:2605` 或 `:2761` 的测试处加开关。现有生产入口是 `discovery.rs:218` 解析成功至 `:219` 构造 Discovered 之间，PR-1 在这里增加可选验证；最终把这里的 `verify_if_present(...)` **替换为** `verify_required(...)`，其 None 分支返回 `IntegrityManifestMissing`。函数名是本设计建议，不是声称现有代码已有此行。`recheck` 的 `read_package` 调用（`:286`）自动继承强制规则；首次 spawn 及重启接线仍需行为测试验证。上线后删除可选入口，不留下可切换的容错模式。

## 7. 实现拆片与逐项验收

建议三个逻辑 PR；PR-2 是外仓发布集成单元，Sin90、Cos72 各自提交对应 PR，不能假装一个 GitHub PR 可以跨仓。

| PR | 内容与验收测试（拟新增） | 故意改坏的正对照 |
|---|---|---|
| PR-1：共享模型、工具及兼容读取 | T1：共享 D2 类型序列化往返，JSON 键与 files 一致、摘要链包含 JSON；T2：两次 generate 字节一致，verify 接受有效包；T3：参数化覆盖增/删/改文件、链接、权限、非法路径、重复 JSON 键及上限；T4：旧包可读，新声明但损坏的包拒绝 | 去掉 JSON 摘要验证使 T1 红；引入不稳定顺序使 T2 红；逐个移除集合、摘要、链接或权限检查使对应 T3 红；把损坏新包当作旧包放行使 T4 红 |
| PR-2：外仓 CI 和新版本 | T5：各平台从真实 release 下载解包后 verify 成功；最终 YAML 可用于原协议握手；macOS 全 executables 验签；配置、脚本和资源全在清单 | 发布 fixture 中漏文件、签名后再改二进制、漏辅助 exe，各对应验收失败；把握手改为生成前摘要，握手测试红 |
| PR-3：强制接线及 UI | T6：新鲜 daemon 缺清单旧包 failed/指定 reason，其余有效包正常；T7：scan 后首次 spawn 前及自动重启前分别改文件，执行标记始终未创建、没有该包可用路由/工具；T8：表中每种原因在 host/非 host/桌面各有 fixture，重装清原因；T9：改清单并改 YAML 后 recheck 仍拒绝，macOS 错团队/缺公证拒绝 | 恢复 optional 让 T6 红；旁路首次及重启检查分别让 T7 红；丢 reason/泄漏 detail/删升级提示使 T8 红；取消冻结摘要比较或签名校验使对应 T9 红 |

每条测试先在未实现行为上验证失败，再恢复正确实现验证通过；故意改坏校验器是测试敏感性的正对照，不能只拿“坏输入被拒绝”冒充已做 mutation 对照。实现 PR 记录命令、退出码和具体失败测试；必须测试实际 spawn/路由/桌面结果，不能只断言 helper 返回 Err。

相关 Rust 测试使用 `ulimit -n 4096` 后在 rust/ 执行 `cargo test -p agent24-components`、`cargo test -p agent24-os-packages`、`cargo test -p agent24-os-proto`、`cargo test -p agent24d`，CLI/manifest 修改另跑对应 crate；有桌面修改则 pnpm 门禁。提交前仍按 AGENTS.md 完整门禁；安全/协议实现 PR 等 APPROVE，不在此设计片执行合并。

本设计片的验证仅检查七项决策覆盖、来源可定位及 PR 文案完整性；不运行无代码变化的 Rust/TS 构建，不声称 T1–T9 已实现或已通过。文档检查及删除关键条款后的失败对照记录在配套 PR body。
