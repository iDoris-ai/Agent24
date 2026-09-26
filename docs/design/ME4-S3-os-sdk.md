# ME4-S3 —— `agent24-os-sdk` 设计（ME4-5.1.1：从两个调用方提取）

> **状态：v4（冻结），2026-09-26。** v1 经 Tier-2 对抗评审（全新上下文 Opus 子代理；Codex 额度耗尽期间，记 ME4-CODEX-DEBT-8）结论 **CHANGES**：5 High / 10 Medium / 11 Low，v2 逐条处置（§11.1）。v2 复审（同为 Tier-2 Opus）结论仍为 **CHANGES**：2 High / 5 Medium / 5 Low，v3 逐条处置（§11.2）。v3 复审（同为 Tier-2 Opus）结论 **CHANGES**：1 High / 3 Medium / 7 Low，v4 逐条处置（§11.3）。除 L6（删 `notify`）、M5（删 `retry_class`）两条按提取原则做了内容取舍外，全部在 §8 已拍板决策的框架内修。**本文自 v4 起冻结**：按用户 2026-09-26 明确指示，v4 是冻结前最后一轮修改，不再送复审；Codex 补审仍记 ME4-CODEX-DEBT-8，待额度恢复后补一轮，不阻塞冻结。冻结后即可按 §7 开 5.1.2 系列 PR、开始写 SDK 代码。**同日追加**：用户拍板 v0.1.0 定位为「原型版本」，收敛判据与切法范围，见 §0「原型定位」与 §7（v4 改写）。
> 文件名按 PLAN-ME4 §三 ME4-5.1.1 的原文取 `ME4-S3-os-sdk.md`（S3 = PLAN §二 的「S3 `agent24-os-sdk`」；`S5` 是 T14 wire 文档那一节，不用这个编号，免得撞名）。
>
> **盘点基线**：
> - Agent24 `origin/main` @ `309d525`（v0–v2 盘点时）；v3 已 rebase 到 `bb3505a`（含 #512 ME4-4.3.1、#513 台账回填），本文引用的 agentd/proto 行号在这两个提交之间未变（#512 只加测试与探针）。
> - Sin90 `origin/main` @ `135ddb7`（`T5.5.1 M5 real-mount acceptance`，即 ME4-M4b 门的最后一个 task）。下文 `Sin90 path:line` 一律指这个提交。
> - Cos72：`MushroomDAO/Cos72`（本机 `~/Dev/mycelium/Cos72`）目前只有 LICENSE/README/CLAUDE.md，**没有代码**；Cos72 的需求只能从 PLAN §二 S4、ME4-5.3.x、`docs/agent/roadmap.md` M4/M5、`docs/decision.md` ADR-004/029 推。Agent24 的旧分支 `feat/me4-cos72-skeleton` 是进程内骨架（PLAN-OOP §73：「3f 之后重做成进程外样例；现在不合」），对进程外 SDK 没有参考价值，不采用。
>
> 文中每一段 Rust 签名都来自 scratch 工作区 `scratchpad/sdk-sketch/`（`fd/` = `agent24-os-fd` 实物、`proto/` = proto 模块侧桩、`sdk/` = SDK 草图 + example + 测试），已在 `1.98.0` 上 `cargo check / clippy -D warnings / test / fmt --check` 全过；`agent24-os-fd` 不是桩，它的校验逻辑在本机 macOS 上用真实 fd 跑过，Linux 分支用 `--target x86_64-unknown-linux-gnu` 做过 clippy（编译，不运行）。评审提出的绕过实验在 `scratchpad/sdk-bypass/`，已用 v3 名单重跑。命令与输出见附录 A。正文里的片段是**节选**（函数体以 `{ … }` 省略），承重原文见附录 B。
>
> **v3 发现的工具链事实**：本机 `rustup run 1.98.0 cargo clippy` 实际调用的是 PATH 里 Homebrew 的 `cargo-clippy`（**clippy 0.1.95**，rustc 仍是 1.98.0）——v0–v2 附录里所有 clippy 读数都是 0.1.95 的。v3 用 `scratchpad/c98`（把 `~/.rustup/toolchains/1.98.0-*/bin` 放到 PATH 最前，`cargo clippy -V` = `clippy 0.1.98`）**重跑了全部 clippy 判据与绕过实验**，附录 A 的读数全部换成 0.1.98 的。对照重跑结果：两个版本对本文所有判据与绕过实验的读数**逐条相同**；v2 附录 A.3 里「v1 名单只报 3 条」是 b9/b10 加进实验之前的旧读数，v3 更正为 5 条（A.3）。**v4 更正**：本机 `cargo +1.98.0 clippy` 实测同样拿到 Homebrew 的 clippy 0.1.95——`+1.98.0` 只挑 `cargo` 本体走哪条工具链，`cargo-clippy` 子命令仍按 PATH 查找，Homebrew 装的排在 rustup 代理之前；v3 「判据命令里的 `cargo +1.98.0` 走 rustup 代理、不受影响」的说法是错的。**本地复现判据一律改用 `scratchpad/c98` 的方法**（把目标工具链的 `bin` 目录塞进 PATH 最前）**并先跑 `c98 clippy -V` 断言等于 `clippy 0.1.98`**，再执行判据命令；不再信任裸 `cargo +1.98.0 clippy` 或 `rustup run … cargo clippy`。CI 不受影响：CI runner 没有 Homebrew，`rustup toolchain install` 装的 stable 就是 PATH 上唯一的 `cargo-clippy`。

## 版本改动记录

| 版本 | 日期 | 改动 |
|---|---|---|
| v0 | 2026-09-26 | 初稿：盘点 + 边界 + API 草图（已编译）+ 迁移 + 判据 + 切法 + 开放问题 |
| v1 | 2026-09-26 | jason 拍板 Q1–Q11（全部按推荐；Q3 选 (a) advise+轮询，gate 回调回推留 followup；Q4/Q1 选 (a) 五种客户端全进 v0.1.0）：§8 由「待用户拍板的开放问题」改写为「已拍板决策」，正文里 §1.2 盘点表、§2.1/2.4/2.6/2.7/2.8、§4.3、§5.2、§7 切法依赖、附录处的相关「待定/见 §8 Qx/若…则…」措辞改为确定表述；三个 followup（gate 回调回推、内核 remember 幂等键、proto kernel/module feature 拆分）登记进 `docs/agent/followups.md` FU-84~FU-86；送评审 |
| v2 | 2026-09-26 | 第 1 轮 Tier-2 对抗评审（CHANGES）逐条修订，处置表 §11。要点：**H1** fd 接管收进 `agent24-os-fd::take_inherited_listener()`（自读 env、进程级一次、fstat/AF_UNIX/SOCK_STREAM/监听校验、CLOEXEC+非阻塞；`ModuleEnv` 不再持 fd 号；token 留在 environ 的风险写明，§4.3）；**H2** 模块侧用宽松镜像 `InitializeReply` 解析握手响应（J-S19）；**H3** `remember_once` 签名/语义/单写者约束写死（§3.5），Sin90 5.2.1 改用它（J-S18）；**H4** proto `test-util` 提供与 Sin90 `test_support.rs` 同签名 API，TS.1.0 增录入站金样，迁移验收加「测试模块 diff 0 行」（§5）；**H5** clippy 名单扩到 24 项并逐项正对照、proto 不导出宏与 socket 别名（J-S1/J-S1b，绕过实验全部被抓）；**M1–M10** J-S2 改 `cargo metadata`、J-S3 改解析 Cargo.toml、`ModuleEnv::from_vars`/`with_env`、`manifest_digest` 挪进 proto、删 `retry_class`（FU-87）、J-S7 落到各回调模块单测、J-S10(b) 用真实投递 body、PLAN 三处同步、advise 孤儿审批（§2.10）、按 SZ-1 口径重估并拆成 17 个 PR（§7）；**L1–L11** 全修（L6 删 `notify`）。发现两处平台事实：macOS 不实现 `SO_ACCEPTCONN`、rustix 0.38.44 的 `getsockname` 在 macOS 上对未命名 AF_UNIX 地址 panic（§4.3） |
| v3 | 2026-09-26 | rebase 到 `bb3505a`（#512、#513 已合并；本分支登记的 Codex 债改号 ME4-CODEX-DEBT-8）。v2 复审（Tier-2 Opus，CHANGES：2H/5M/5L）逐条修订，处置表 §11.2。要点：**H-1** §5.2 第 3 条改为「测试模块与已提交的期望补丁逐字相等」，期望补丁只允许 `clients/model.rs` 测试模块那一行 `use crate::ai::UnavailableCause;` → `use agent24_os_sdk::UnavailableCause;`，脚本与 4 个正对照实测（A.8）；**H-2** 更正 v2 的绕过分析（实测是外部宏 b3 **被** clippy 抓、跨 crate 别名 b2 **没**被抓），名单加 `str::parse`、`core::str::FromStr::from_str` 到 26 项，J-S1b 改为「proto `module*` 与 os-fd 不许有任何 `pub type`、`pub use` 只能转出 crate 内部项」并改用 Python 脚本（缺工具时失败而非静默通过），`FatalHook` 因此从类型别名改为 newtype，`take_listener` 返回具名的 `InheritedListener`；写明名单是绊线不是沙箱、两条已知残余不防（§2.2）；**M-1** macOS 弱校验的已知缺口写进 §4.3/源码并用 `cfg(apple)` 测试钉住，`EINVAL` 映射为 `NotListening`；**M-2** a3-fd 加 `macos-latest` CI job；**M-3** 生产路径在 `connect()` 里接管监听器，`with_env` 跳过；**M-4** 串行开 PR、base 恒为 main、依赖改 DAG、b2b+b2d 合并（SZ-1 实测 209）、c1 拆 c1a/c1b，仍 17 片；**M-5** 4.3.1 回填 DONE，M4b 门按事实改 `IN_PROGRESS`；**L1–L5** 全修。另发现本机 `rustup run 1.98.0 cargo clippy` 实际是 clippy 0.1.95，全部 clippy 读数已用真 0.1.98 重跑（文首） |
| v4 | 2026-09-26 | **冻结版，不再复审**（用户明确指示）。v3 复审（Tier-2 Opus，CHANGES：1H/3M/7L）逐条修订，处置表 §11.3。要点：**H-A** §5.2 第 3 条的 `check-test-modules.sh` 改为 `tests_of` 只认「`#[cfg(test)]` 紧跟 `mod tests {`」的锚点、每文件恰好一个否则 `exit 3`（旧的「从第一个 `#[cfg(test)]` 开始截到文件尾」在真实迁移会撞上 reconciler.rs 非测试部分新增的 `#[cfg(test)] use …RECALL_PRECHECK_MAX_PAGES;` 这一诱饵行），`scratchpad/h1mig/` 三种情形实测：真实形状迁移 rc=0、测试模块被改一处 rc=1、锚点缺失 rc=3；写明该脚本是 TS.1.1 那个 PR 的一次性检查，不进常驻 CI（L-f）；**M-A** §2.2 已知残余补两条（b16：proto 内核侧公开函数直接返回 socket 类型、内核侧两跳别名——J-S1b 只管 proto `module*`/os-fd 与「点名」别名，管不到这两类），登记 FU-88；**M-B** 更正「`cargo +1.98.0` 不受 Homebrew clippy 覆盖影响」的错误说法：本机实测同样中招，本地复现一律走 `scratchpad/c98` 并先断言 `clippy -V` = 0.1.98，CI 不受影响（文首、§9 第 11 条）；**M-C** a3-fd 依赖加 a3-skel，推荐串行顺序改为 a3-skel → a3-fd（§7、tasks.md、PLAN 三处同步）；**L-a** b2b 若 SZ-1 实测超 300 兜底拆回独立 b2d；**L-b** §11.2 M-5 行与版本记录按 `e39a2af` 核对更新为 M2/M3/M4b 门均已回填 DONE；**L-c** J-S1b/J-S3 判据文字写明依赖 `cargo fmt --check` 兜底逐行正则；**L-e** 写明 a2-core 落定最终 `call(.., CallOptions)` 签名（`CallOptions` 在 a2-core 定义，取消行为在 a2-cancel 实现）；**L-d** serve 路由挂载可拆成可 oneshot 的方法，留给实现 a3-module 时决定，本轮不定案；**L-g** 其余次要措辞问题回应写进 PR body，不改文档。**同日追加（用户拍板）**：v0.1.0 收敛为「原型版本」——Sin90、Cos72 都还没有规模，本轮只提取两个调用方确实要用的最小面；§0 新增「原型定位」，§6 判据分核心/原型阶段可选两档，§7 切法从 17 片压缩为 4 片（proto 模块侧 / SDK crate + 客户端 + fired / Sin90 迁移 / Cos72 占位），单片允许超 300 行、PR body 回应 SZ-1，tasks.md 与 PLAN §三同步压缩 |

---

## 0. 这份文档解决什么、不解决什么

**解决**：
1. 在 Sin90 手写 adapter（`src/adapter_agent24/`，9593 行，含测试）里逐项区分「任何进程外模块都要写的」和「Sin90 特有的」，并推演 Cos72 需要哪几项（§1）。
2. 定 SDK 边界：crate 名与依赖、transport/握手放哪、五种类型化客户端、fired 注册点、错误映射、超时/重试归属、版本与兼容、非 Rust 模块如何对齐 wire（§2）。
3. 公共 API 草图，已编译（§3），以及 proto / `agent24-os-fd` 为此要补的公开 API（§4）。
4. Sin90 迁到 SDK 的步骤和「零行为变化」的验收办法（§5，ME4-5.2.1 / Sin90 TS.1.0 + TS.1.1）。
5. 判据与 PR 切法（§6、§7；v4 起对应 ME4-5.1.2a/b 两个大片，§7.1 保留 v3 时期拆出的 17 个细粒度组件参考）。
6. 记录 jason 2026-09-26 已拍板的 Q1–Q11 决策及理由（§8），以及三轮评审的逐条处置（§11.1 第 1 轮 → v2，§11.2 复审 → v3，§11.3 三审 → v4 冻结）。

**不解决**：
- 内核回调面本身（调度、推理、记忆、审批的内核实现已由 ME4-S1/S2 与 ME-3e 冻结；SDK 按 PLAN「不带来任何新能力」）。
- T14 wire 文档正文（ME4-5.4.1，规范 S5）；本文只规定 SDK 与 wire 文档的对齐方式（§2.9）。
- Cos72 的业务设计（ME4-5.3.1 起在 Cos72 仓库做）；本文只把 Cos72 对 SDK 的需求推到够用，并按 Q3 的拍板把一处与内核闭集冲突的计划措辞改为 advise（§8 Q3），把 advise 的孤儿审批约束写给 Cos72（§2.10）。

**原型定位（v0.1.0，v4 追加，用户 2026-09-26 拍板）**：Sin90、Cos72 都还没有规模（Cos72 甚至还没有代码），本轮 SDK 是**原型**——只提取两个调用方（Sin90、Python 黑盒/T14）确实要用的最小面，不为「将来可能有的第三方 SDK 作者恶意或无意绕过」预先建防御纵深。因此本文把判据分两档（§6 开头的分级说明）：
- **核心判据**（v0.1.0 冻结、Sin90 迁移、开 PR 都需要）：能编译（`cargo check`/`cargo test`）；Sin90 迁移后真实挂载黑盒全绿（J-S16）；五个客户端对内核真实 handler 的对等测试（J-S7）；SDK 依赖只经 `agent24-os-proto`（J-S2 的依赖集合断言本身，不含它的正对照脚本）。
- **原型阶段可选**（记录在案、冻结后按需补，不阻塞本轮）：clippy 26 项名单的逐项正对照三步机械断言（J-S1 的 `positive-control` 部分）；J-S1b（Python 脚本 fail-closed、两跳别名等扩展规则）；J-S3 的正对照脚本；J-S20 的 `macos-latest` CI job；§5.2 第 3 条的期望补丁逐字脚本（H-A 新增的 `check-test-modules.sh`）。这些判据本身不删——已经在 scratch 上写出并实测过，成本沉没——只是不作为本轮「必须做」的硬性 gate，留给规模变大、第三方 SDK 作者出现之后再决定是否升级为常驻 CI。
- 相应地，§7 的切法从 v3 的 17 个 PR 压缩为 4 个大片（proto 模块侧 transport/握手/fd、SDK crate + 五个客户端 + fired、Sin90 迁移、Cos72 占位），单片允许超过 PR-Daemon 的 SZ-1 默认上限 300 行，超限时在 PR body 里回应即可，不强制再拆。

---

## 1. 盘点：Sin90 adapter 逐项归类，外加 Cos72 的需求推演

### 1.1 归类规则

- **进 SDK**：两个调用方都要，或 Cos72 明确要（用户规则）。Sin90 已有实现、且实现里不含 Sin90 领域类型的，按原语义搬。**新语义不进**：Sin90 没有、Cos72 也还没有代码去用的「新分类 / 新助手」不在 v0.1.0 发明（v2 据此删 `retry_class`，§2.6）。
- **进 proto**：凡是碰 socket 字节、帧、握手线格式的（PLAN S3-1：「SDK 不做任何 socket 字节 I/O 与帧解析……若 proto 缺少 SDK 需要的公开 API，在 proto 里补」）。
- **进 `agent24-os-fd`**：把继承来的 fd 号变成 socket 的那一步（唯一的 `unsafe`，§8 Q2）。
- **留在 Sin90**：带 Sin90 领域语义、Sin90 自己的存储、或 Sin90 自己的分层约定的。

「Cos72 要不要」一列的依据：PLAN S4（「每个动作发事件；任务完成摘要写 `_a24/memory/private/remember`；发积分经内核审批」）、ME4-5.3.4（「与 Sin90 同时挂载时互相读不到对方的记忆与 schedules」）、D6（「用到事件 + 记忆 + 审批」）。

### 1.2 盘点表

| # | 项 | Sin90 位置 | 归属 | Cos72 要不要 | 说明 |
|---|---|---|---|---|---|
| 1 | spawn 环境变量读取 | `mod.rs:144-165`（`SpawnEnv::from_env`） | **proto**（三个）+ **os-fd**（`A24_LISTEN_FD`） | 要 | 常量 proto 早有：`launch.rs:30-37`（`ENV_LISTEN_FD` 等）。Sin90 手抄了字符串。v2：`ModuleEnv` 只持 `A24_DATA_DIR`/`A24_CALLBACK_SOCK`/`A24_HANDSHAKE_TOKEN`，**不持 fd 号**；`A24_LISTEN_FD` 只由 `agent24-os-fd` 自己读（H1，§4.3） |
| 2 | manifest 摘要 `sha256:<hex>` | `mod.rs:169-173` | **proto** | 要 | 内核侧今天在 `agent24-os-packages/src/discovery.rs:46-57`（`pub fn manifest_digest`），挪进 `agent24_os_proto::manifest`，os-packages 改为转出；新增依赖边 `agent24-os-packages → agent24-os-proto`（§4.4，M4） |
| 3 | `initialize` 请求/响应、协议范围、握手后缓冲区不许有残字节 | `mod.rs:66-74`（范围 1..=1000）、`mod.rs:326-402` | **proto** | 要 | proto 已有 `InitializeParams`/`InitializeResult`/`Offer`（`initialize.rs:59-160`），但只有**内核侧**的 `accept`。Sin90 手拼 `json!` 且自己 `serde_json::from_slice` 解析响应（`mod.rs:365-386`）——PLAN-OOP T13 判据原话「SDK 里出现 `serde_json::from_slice` 直接解析协议帧 = 缝切错了」。**响应解析用宽松镜像 `module::InitializeReply`，不用带 `deny_unknown_fields` 的 `InitializeResult`（`initialize.rs:148-150`）**（H2，§4.1） |
| 4 | `INITIALIZE_CAPABILITIES` 手写常量 + 与 manifest 对齐的测试 | `mod.rs:84`、`mod.rs:640-663` | **SDK（改为从 manifest 派生）** | 要 | 手抄一份再用测试钉住，是「两份真相」。SDK 从 manifest 文本里读 `name`/`route_namespace`/`kernel_capabilities`，这个常量与测试整个消失（§2.3） |
| 5 | 帧读写（1 MiB 上限、`\n` 分隔、EOF 半帧 = Closed） | `frame.rs:34-97` | **proto（已有，删 Sin90 这份）** | 要 | proto 有 `frame::MAX_FRAME_BYTES`（`frame.rs:84`）与 `rpc::read_frame_async`（`rpc.rs:886`）。Sin90 这份是重复实现 |
| 6 | 多路复用 transport：单写任务、读任务按 id 分发、64 在途上限、drop 即发 `$/cancelRequest`、写超时 10s、响应兜底 35s、连接死 → `FatalHook` 恰好一次、NotSent/ConnectionLost 二分 | `transport.rs:63-631`（非测试 539 行，其中注释 178）；测试 `transport.rs:686-1378` 共 16 条 | **proto** | 要 | 这是 PLAN S3-2 点名的「T3.2.0 多路复用 transport」。它**持有 socket**，按 S3-1 结构判据只能进 proto。常量与 proto 同值：64 = `rpc::MAX_IN_FLIGHT_PER_CONNECTION`（`rpc.rs:73`），35s > `rpc::CALL_TIMEOUT` 30s（`rpc.rs:82`），10s = `rpc::WRITE_TIMEOUT`（`rpc.rs:93`），`$/cancelRequest` = `rpc::CANCEL_METHOD`（`rpc.rs:101`）。Sin90 注释自己承认「只是数值巧合，没有强制对称」（`transport.rs:26-29`）——搬进 proto 后改成直接引用 proto 常量 |
| 7 | `RpcErrorInfo`（code / data.kind / message / raw） | `transport.rs:119-153` | **proto** | 要 | 解析 JSON-RPC error 对象属于协议层。`raw` → `data`（只留 `data`），入站金样钉住映射不变（§5.1） |
| 8 | `KernelClients`：持连接 + 握手时固定的 `Offer`，`provides(prefix)` | `mod.rs:210-322` | **SDK（`Module`）** | 要 | 无重连、`Offer` 一代不变，这两条语义原样保留 |
| 9 | 闭集错误 `ClientError` + `is_permanent`/`is_retryable` + 两路映射（传输错误、内核 `kind`） | `clients/error.rs:73-397` | **SDK** | 要 | 唯一的 Sin90 耦合：`Unavailable.cause` 用的是 `crate::ai::UnavailableCause`（`error.rs:217-229`、`378-397`）。SDK 自带同名四值闭集，Sin90 在自己的 `ModelFailure` 映射里转换 |
| 10 | 「omit, don't null」可选字段约定 | `clients/mod.rs:74-78` | **SDK** | 要 | 内核 params 全是 `deny_unknown_fields` + `#[serde(default)]`；发 `null` 与缺省语义不同的地方（如 scheduler `enabled`）会出错 |
| 11 | `EventSink`：同步 `emit`、256 容量队列、4 个固定 worker、32 子配额、5s 等位、满即丢并按 2 的幂打日志 | `mod.rs:86-122`、`mod.rs:425-503` | **SDK** | 要（每个动作发事件） | 这套数字是两轮评审（N-M1/N-M2/M5）磨出来的，任何模块自己再写一遍大概率回到「每次 emit spawn 一个任务」的旧缺陷。SDK 同时给「可等待的 `emit`」与「即发即弃的 sink」 |
| 12 | `SchedulerClient`（upsert/delete/list + 类型） | `clients/scheduler.rs:35-243` | **SDK** | 按 §8 Q4 拍板：进（是否申请由 5.3.1 定） | Cos72 的 S4 闭环不用调度；但 ME4-5.3.4 要验证「互相读不到对方的 schedules」，Cos72 若不申请 `scheduler` 就只能从内核 REST 侧验。第二个真实调用方是 Agent24 黑盒 Python 模块（`me4_scheduler_blackbox.rs:75-240`）与 T14 Node 模块 |
| 13 | `MemoryClient`（remember/recall/recent） | `clients/memory.rs:30-149` | **SDK** | 要（摘要进记忆） | |
| 14 | `remember` 不幂等 → 先 `recall` 翻页找 dedup 标记再写 | `reconciler.rs:255-386`（`RECALL_PRECHECK_MAX_PAGES = 10`） | **SDK（`remember_once`，按 §8 Q5 拍板）** | 要（摘要写入同样会遇到「超时后重试写出两条」） | 算法依赖内核 `recall` 的实际语义（子串匹配、每次最多扫 2000 行、`cursor` 表示「窗口扫完了」而非「还有匹配」，`reconciler.rs:100-120`），很容易写错；本轮原样搬进 SDK（标记键名沿用 `body.dedup_key`，§3.5），**Sin90 在 5.2.1 改用 SDK 版**（H3，§5.1）；内核加幂等键留作 FU-85 |
| 15 | `ApprovalClient`（gate/advise/status，`ApprovalToken` 脱敏） | `clients/approval.rs:38-209` | **SDK** | 要 | Sin90 **没有业务调用方**，只有 test-hooks 路由（`kernel_roundtrip.rs:227-254`）。Cos72 是它第一个真实用户。注意 gate 的动作闭集今天只有 `schedule_callback`（`agent24-protocol/src/types.rs:421-431, 575-590`），按 §8 Q3 拍板：Cos72 用 advise；孤儿审批约束见 §2.10 |
| 16 | 从被代理请求头取 `X-A24-Request-Id` / `X-A24-Approval-Token` | `kernel_roundtrip.rs:227-254`；Python 黑盒 `me4_scheduler_blackbox.rs:201-221` | **SDK（`RequestContext` 提取器）** | 要（审批必须带这两样） | 头名 proto 已有：`proxy.rs:87`、`proxy.rs:96` |
| 17 | `ModelClient` 的 wire 部分（`_a24/model/complete` 参数/结果、125s 响应期限） | `clients/model.rs:44-156` | **SDK（wire 形状）** | 不要 | 按 §8 Q4 拍板进 SDK（Cos72 不需要，但目前只有 Sin90 用，T14 的 Rust 对照需要它）。Sin90 自己的 `Role` 没有 `Assistant`、`usage` 用 `u32`，内核是 `System/User/Assistant` 与 `u64`（`agentd model_callback.rs:91-96, 189-193`）。SDK 按内核 wire 定义；Sin90 在自己的 `model.rs` 里做 `u64 → u32`（越界按今天的「响应形状错」处理，入站金样钉住，§5.1） |
| 18 | `ModelPort`/`ModelCaller` 适配、`ClientError → ModelFailure` 映射、`SemaphoredModelCaller` 每模块并发 2 | `clients/model.rs:159-215`、`mod.rs:567-579` | **留 Sin90** | — | Sin90 AI 引擎梯的端口，领域语义 |
| 19 | `wire_kernel_clients`：没给能力时降级为 `NullEventSink`、能力前缀表 | `mod.rs:175-204`、`mod.rs:544-581` | **留 Sin90**（降级策略）；「按 `Offer` 返回 `Option`」的机制进 SDK | 各自写 | 各模块的「缺能力怎么降级」不同 |
| 20 | `listener_from_fd`（`unsafe { from_raw_fd }`） | `mod.rs:600-604` | **`agent24-os-fd`**（按 §8 Q2 拍板：独立小 crate 持有唯一 `unsafe`）；proto 包一层给 tokio | 要 | 工作区 `unsafe_code = "forbid"`（`rust/Cargo.toml:33`）。v2 把入口写死为 `take_inherited_listener()`（§4.3） |
| 21 | fired 保留路由：`X-A24-Fire-Id` 必填、body `deny_unknown_fields`、定宽 ISO-8601 校验、任何结果都回 2xx | `http/mod.rs:190`、`http/mod.rs:320-338`、`http/mod.rs:1347-1359`、`http/mod.rs:1376-1478` | 解析/校验进 **SDK**（`FiredDelivery` 提取器 + `with_fired` 注册点）；「按 `fire_id` 去重写库、镜像事件」**留 Sin90** | 仅当 Cos72 申请 scheduler | Sin90 的分层（`lib.rs:3`：`core ← store ← http ← adapter_agent24`，`http` 不许依赖 Agent24 类型）决定了 Sin90 在 5.2.1 **不**改用 SDK 提取器（§5.2）。内核侧 `FiredBody` 是 `agentd scheduler_deliver.rs:178` 的私有借用类型，挪进 proto |
| 22 | 主流程：读 env → 握手 → 建客户端 → 开 store → nest 到 `/api/v1/sin90` → `axum::serve` | `main.rs:118-262` | 骨架进 **SDK**（`Module::builder(..).connect()` / `serve(router)`）；中间 Sin90 的 store、actor keys、reconciler、test-hooks 合并留 Sin90 | 要 | `route_namespace` 从 manifest 读（Sin90 手写了 `"/api/v1/sin90"`，`main.rs:259`）。manifest 文本用 Sin90 已有的 `sin90::ai::MANIFEST_YAML`（`ai/ports.rs:122-124`，按 `remote-allowed-manifest` feature 二选一） |
| 23 | 连接死 → `exit(70)` | `main.rs:138-145` | **SDK 默认值**，可覆盖 | 要 | §2.7；测试入口强制非退出钩子（§3.2） |
| 24 | outbox 泵、全量对账、退避 1s×2 上限 5min、other-bucket 20 次耗尽、按错误分类决定「继续 / 停本批 / 整个泵停」 | `reconciler.rs:136-1180` | **全部留 Sin90**（含分类表） | 各自写（Cos72 用 `is_permanent`/`is_retryable` + 穷举 match 自己分） | v1 曾把分类抽成 `ClientError::retry_class`；它与 Sin90 实际行为不一致（`Cancelled`）、Sin90 5.2.1 不用、Cos72 还没有 outbox 代码——两个调用方都不用的新语义，按 §1.1 删除；等 Cos72 outbox 落地后按两个真实调用方再提取（FU-87，§2.6） |
| 25 | Actor keys（人 / 自动化两把钥匙） | `http/actor.rs` | **留 Sin90** | 各自 | 领域的「AI 不直接写」门 |
| 26 | test-hooks 调试路由、假内核测试夹具 | `kernel_roundtrip.rs`、`reconciler_debug.rs`、`clients/test_support.rs` | 调试路由**留 Sin90**；假内核夹具进 **proto**（`test-util` feature，SDK 转出） | 要夹具 | SDK 自己的测试也不能开 socket（clippy 判据对 `--all-targets` 生效），只能用 proto 给的内存假内核。**函数名与签名与 Sin90 `test_support.rs` 逐一相同**，Sin90 保留一个一行 shim 即可让测试模块里的夹具调用零改动（H4，§4.5；`clients/model.rs` 测试模块另有一行 `use` 必须改，见 §5.2 第 3 条） |

**Cos72 需要的集合**（推演结论）：1–11、13、14、15、16、20、22、23。调度（12、21）按 §8 Q4 拍板进 SDK，Cos72 是否申请由 5.3.1 定，隔离验法见 ME4-5.3.4；推理（17）Cos72 不要；24 的错误分类 Cos72 在自己的 outbox 里写（FU-87 之后再看能否提取）。

### 1.3 这份盘点揭出的三件计划里没写到的事

1. **proto 今天没有任何模块侧 API。** PLAN S3-1（v1 时）写「proto 提供两个入口 `Client::connect_from_env()` 和 `listener_from_env()`」——这两个入口**不存在**（`rg connect_from_env|listener_from_env rust/` 为空）。proto 22,713 行全是内核侧：`accept`（握手判定）、`rpc::serve`（服务模块的调用）、launch/supervise/proxy。所以 5.1.2a 的大头是「把 Sin90 的 transport 搬进 proto 当模块侧客户端」，不是写 SDK。v2 已把 PLAN S3-1 的入口名改成本文的实际签名（M8）。
2. **把 fd 号变成 socket 需要 `unsafe`，而工作区禁了。** 只有 `FromRawFd::from_raw_fd` / `BorrowedFd::borrow_raw` 能做，都是 `unsafe`；`rust/Cargo.toml:33` 是 `unsafe_code = "forbid"`（forbid 不能被 `#[allow]` 局部放开）。**已拍板**：新建独立小 crate `agent24-os-fd`（§8 Q2），入口 `take_inherited_listener()`（§4.3）。
3. **PLAN S4 写「发积分经内核审批（`_a24/approval/gate`）」，但 gate 今天只接受 `schedule_callback` 一个动作**，其余一律 `forbidden`（`types.rs:421-431`）。**已拍板**：Cos72 用 `advise` + 轮询 `status`（§8 Q3）；PLAN S4 措辞已改（M8）；「审批裁决回调」注册点放下一轮（FU-84）。

---

## 2. SDK 边界

### 2.1 crate 名、位置、依赖

- 名字 `agent24-os-sdk`，位置 `rust/crates/agent24-os-sdk`（PLAN S3-1）。工作区成员，继承 `[lints] workspace = true`（因此 `unsafe_code = "forbid"` 自动生效）。
- **普通依赖恰为**：`agent24-os-proto`、`axum 0.8`（`default-features = false, features = ["json","tokio","http1"]`，与 proto 同主版本）、`serde`、`serde_json`、`thiserror`、`tokio`（`sync`,`time`,`rt`,`macros`）、`tracing`——这个集合本身就是判据（J-S2 按集合相等断言，顺带挡住 `socket2`/`nix`/`libc` 之类能绕开 clippy 名单的依赖）。
- 新 crate `rust/crates/agent24-os-fd`：只依赖 `rustix 1.1`（`fs`,`net`），整个工作区唯一不继承 `[lints] workspace = true` 的成员（自己写 `unsafe_code = "deny"` + 其余 lint 原样重述），唯一一处 `#[allow(unsafe_code)]`（§4.3，J-S3）。用 rustix 1.x 而不是 proto 的 0.38：scratch 实测 **rustix 0.38.44 的 `getsockname` 在 macOS 上对未命名 AF_UNIX 地址（socketpair 端）直接 panic**（`backend/libc/net/read_sockaddr.rs:320` 的 `unwrap`），1.1.4 正常；1.1.4 已在 Agent24 `Cargo.lock` 里（其它依赖带进来的）。
- **依赖方向**（全部无环）：`agent24-os-sdk → agent24-os-proto → agent24-os-fd`；`agent24-os-proto → agent24-domain`（已有）；**新增** `agent24-os-packages → agent24-os-proto`（`manifest_digest` 挪进 proto 后 os-packages 转出，§4.4）。os-packages 今天只被 agentd 与 agent24-cli 依赖，二者本来就依赖 proto，所以这条边不给任何下游新增编译负担。
- SDK 需要 manifest 的三个字段。proto 本来就依赖 `agent24-domain`（`agent24-os-proto/Cargo.toml:10`），由 proto 的 `manifest` 模块给出宽松的三字段解析（§4.4），SDK 不直接依赖 `agent24-domain`。
- **依赖重量的代价**：proto 今天连带 `command-fds`、`close_fds = "=0.3.2"`（精确钉版）、`rustix`、`hyper`/`hyper-util`、`agent24-domain`（→ `serde_yaml`、`agent24-protocol` → `schemars`）。Sin90/Cos72 以 git 依赖引入 SDK 时这些都要编译。**已拍板**：本轮不拆 proto `kernel`/`module` feature（§8 Q8，FU-86）。

### 2.2 结构性判据：SDK 从不持有 socket、从不自己把字节解析成 JSON（PLAN S3-1，已在 scratch 验证）

`rust/crates/agent24-os-sdk/clippy.toml`（26 项；scratch 原文）：

```toml
disallowed-types = [
  { path = "tokio::net::UnixStream", reason = "S3-1: the SDK never owns a socket; use agent24_os_proto::module" },
  { path = "tokio::net::UnixListener", reason = "S3-1: use agent24_os_proto::module::take_listener" },
  { path = "tokio::net::UnixSocket", reason = "S3-1" },
  { path = "tokio::net::UnixDatagram", reason = "S3-1" },
  { path = "tokio::net::TcpStream", reason = "S3-1" },
  { path = "tokio::net::TcpListener", reason = "S3-1" },
  { path = "tokio::net::TcpSocket", reason = "S3-1" },
  { path = "tokio::net::UdpSocket", reason = "S3-1" },
  { path = "std::os::unix::net::UnixStream", reason = "S3-1" },
  { path = "std::os::unix::net::UnixListener", reason = "S3-1" },
  { path = "std::os::unix::net::UnixDatagram", reason = "S3-1" },
  { path = "std::net::TcpStream", reason = "S3-1" },
  { path = "std::net::TcpListener", reason = "S3-1" },
  { path = "std::net::UdpSocket", reason = "S3-1" },
  { path = "std::os::fd::OwnedFd", reason = "S3-1: fd adoption lives in agent24-os-fd" },
]
disallowed-methods = [
  { path = "std::os::fd::FromRawFd::from_raw_fd", reason = "S3-1: fd adoption lives in agent24-os-fd" },
  { path = "serde_json::from_slice", reason = "PLAN T13: parsing protocol frames in the SDK means the seam is cut wrong" },
  { path = "serde_json::from_str", reason = "PLAN T13" },
  { path = "serde_json::from_reader", reason = "PLAN T13" },
  { path = "serde_json::Deserializer::from_slice", reason = "PLAN T13" },
  { path = "serde_json::Deserializer::from_str", reason = "PLAN T13" },
  { path = "serde_json::Deserializer::from_reader", reason = "PLAN T13" },
  { path = "serde_json::Deserializer::new", reason = "PLAN T13 (bypass: Deserializer::new(SliceRead::new(b)))" },
  { path = "axum::Json::from_bytes", reason = "PLAN T13: use the Json extractor, which parses inside axum" },
  { path = "str::parse", reason = "PLAN T13 (bypass: s.parse::<serde_json::Value>()); the SDK parses no text at all" },
  { path = "core::str::FromStr::from_str", reason = "PLAN T13 (bypass: <Value as FromStr>::from_str)" },
]
```

clippy 的路径名单不支持通配（`std::net::*` 写不出来），所以逐项列；`std::net` 里能建连接/监听的三个类型都在。SDK 合法需要的 JSON 操作只有两种：`serde_json::from_value`（对 proto 已解析好的 `Value` 取类型，`clients::Core`）与 axum 的 `Json` 提取器（fired body，解析发生在 axum crate 内部，不受本 crate 的 clippy 约束）；二者都不在名单里。`str::parse` / `FromStr::from_str` 整个禁掉（v3，复审点名的 b11/b12）：SDK 今天没有任何文本解析（草图全量 clippy 绿），将来真要解析数字就用不经 `FromStr` 的 `u64::from_str_radix` 之类，名单不为它开例外。

**名单是绊线，不是沙箱（v3）。** clippy 的 disallowed 名单只对本 crate 源码里**写出来的路径**生效，它挡的是「无意中/顺手」在 SDK 里自己连 socket、自己解析字节，不是一个对抗恶意 SDK 作者的隔离边界——真正的边界是评审 + 这些机械判据的组合。已知残余，**记录、不防**：
- **b13** 手工拼一个 `axum::http::Request`（`Body::from(bytes)` + `content-type: application/json`）喂给 `Json::<Value>::from_request`——解析发生在 axum 里，与 fired 提取器走的是同一条合法路径，名单无法区分「解析内核投来的 HTTP 请求」和「把任意字节包成请求再解析」。
- **b14** `std::fs::File::open("/dev/fd/3")`——经文件系统路径拿到继承来的 fd（Linux 上 `/proc/self/fd/3` 同理）。`File` 与 `open` 都是正当 API，禁不得。os-fd 已给 fd 3 设 CLOEXEC 并由 `Module` 独占，这条只能靠评审。
- **b16**（v4，M-A）proto **内核侧**公开函数直接把 socket 类型写在返回值里而不是走别名（例如 `endpoint.rs:563` 的 `accept_one` 若签名是 `pub fn accept_one(&self) -> io::Result<UnixStream>`）——J-S1b 的规则②（不许 `pub type`）只管 `module*`/os-fd 两个范围，规则④（点名 socket 类型的 `pub type`/`pub use` 全 proto 禁）只管**别名/转出**，两者都不管一个函数签名里直接写出的返回类型；SDK 侧要用到这种函数才会在源码里写出 `tokio::net::UnixStream`，届时会被 clippy 名单本身抓住（这是「正当路径」而非绕过），所以只在 SDK 从不调用内核侧 API 的前提下才是残余——记录以防将来 SDK 误调用了内核态函数。
- **b16b**（v4，M-A）**内核侧**（非 `module*`/os-fd）的两跳别名，且第一跳不是 `pub`：`type Inner = tokio::net::UnixStream;`（私有，规则④不查非 `pub` 项）后 `pub type Sock2 = Inner;`（第二跳只点名本地标识符 `Inner`，规则④按文本匹配「点名 socket/fd 类型」，匹配不到 `Inner` 这个名字）——J-S1b 是文本/正则扫描，不做类型解析，两跳里只要有一跳既不在 `module*`/os-fd 范围、又不直接点名具体类型，就能溜过去。两条都**记录、不防**，理由与 b13/b14 相同：本轮内核侧代码由内部团队写、经评审即可发现，为了堵死它们要么把规则②的「不许任何 `pub type`」扩到全 proto（内核侧确有正当理由要 `pub type` 别名非 socket 的东西）、要么上真的类型解析（本轮工作量不值）；已登记 **FU-88**（proto 内核侧 API 按 kernel/module 可见性收紧，与 FU-86 的 feature 拆分一起做）。
- 四者都要求 SDK 或内核侧作者**有意或无意**写出这几种少见形状，评审一眼可见；为它们引入更重的机制（例如禁 `axum::Json::from_request`、禁 `std::fs`、给 J-S1b 上完整类型解析）会连带禁掉合法用法或工作量骤增，本轮不值。

**绕过实测：v1 / v2 / v3 三份名单**（`scratchpad/sdk-bypass/sdk/src/bypass.rs`，每一行是一种绕过写法；v3 用真 clippy 0.1.98 重跑，读数见附录 A.3）。**v2 表把 b2 与 b3 的结论标反了**（复审 H-2）：实测是**外部宏 b3 被抓**（clippy 对宏展开出的路径照样报，报错位置指向调用处 `bypass.rs:15`），**跨 crate 别名 b2 没被抓**（`bypass.rs:13` 零报错）——v2 把 `:15` 那条错误当成了 b2 的。

| # | 绕过写法 | v1 | v2 | v3 | 由谁挡 |
|---|---|---|---|---|---|
| b1 | `tokio::net::UnixSocket::new_stream()?.connect(p)`（从不写出 `UnixStream`）+ `serde_json::from_str` | 漏 | 抓 | 抓 | 名单 |
| b2 | proto 导出的**一跳**别名 `agent24_os_proto::Sock::connect`（`pub type Sock = tokio::net::UnixStream`） | **漏** | **漏** | **漏** | **J-S1b**（proto `module*`/os-fd 不许 `pub type`；全 proto 不许 `pub type/use` 点名 socket 类型） |
| b3 | proto 导出的 `#[macro_export]` 宏展开出 `::tokio::net::UnixStream::connect` | 抓 | 抓 | 抓 | 名单（J-S1b 另外禁 `#[macro_export]`，双保险） |
| b4 | `use tokio::net::UnixStream as Renamed` | 抓 | 抓 | 抓 | 名单 |
| b5 | `serde_json::Deserializer::from_slice` / `serde_json::from_reader` | 漏 | 抓 | 抓 | 名单 |
| b6 | `std::net::TcpStream::connect` | 漏 | 抓 | 抓 | 名单 |
| b7 | `serde_json::Deserializer::new(serde_json::de::SliceRead::new(b))` | 漏 | 抓 | 抓 | 名单 |
| b8 | `axum::Json::<Value>::from_bytes(b)` | 漏 | 抓 | 抓 | 名单 |
| b9 | SDK 内部 `type Local = std::os::unix::net::UnixStream` | 抓 | 抓 | 抓 | 名单（本 crate 内的别名 clippy 看得穿） |
| b10 | `std::os::unix::net::UnixListener::from(OwnedFd)` | 半抓（只报 `UnixListener`） | 抓 | 抓 | 名单 |
| b11 | `s.parse::<serde_json::Value>()` | 漏 | 漏 | 抓 | 名单（`str::parse`） |
| b12 | `<serde_json::Value as FromStr>::from_str(s)` | 漏 | 漏 | 抓 | 名单（`core::str::FromStr::from_str`） |
| b13 | 手工拼 `Request` 喂 `axum::Json::<Value>::from_request` | 漏 | 漏 | 漏 | **不防**（已知残余，见上） |
| b14 | `std::fs::File::open("/dev/fd/3")` | 漏 | 漏 | 漏 | **不防**（已知残余，见上） |
| b15 | proto 导出的**两跳**别名（`type Inner = UnixStream; pub type Sock2 = Inner;`） | 漏 | 漏 | 漏 | **J-S1b**（`pub type` 一律禁，不看右边写的是什么） |

v3 名单下 15 条 `use of a disallowed`（附录 A.3），覆盖 b1、b3–b12；b2/b15 由 J-S1b 在 proto/os-fd 侧从源头禁止（正对照里一跳、两跳、外部 `pub use`、`#[macro_export]` 四种都被脚本命中，A.4）；b13/b14 记录不防。别名为什么要在 proto 侧禁：clippy 的 `disallowed_types` 按「源码里写出的路径」解析，跨 crate 的 `pub type` 在调用处已经是另一个路径（`agent24_os_proto::Sock`），名单里写原类型对它无效；而在 proto 里把 `Sock` 也加进 SDK 名单只能挡已知名字，挡不住下一个别名——所以规则定成结构性的「proto 模块侧不许有 `pub type`」，`FatalHook` 也因此从 `pub type FatalHook = Arc<dyn Fn()…>` 改成 newtype（§3.2、附录 B.1）。其余可能的通道——新增能开 socket 的依赖（`socket2`、`nix`、`libc`）——由 J-S2 的依赖集合相等挡住；`unsafe` 由 `forbid` 挡住。

其它实测（附录 A）：
- `ModuleBuilder::connect` 里 `take_listener()` 拿到的是 proto 的具名结构 `InheritedListener`（存进 `Module`），`serve` 里 `axum::serve(listener.into_tokio(), app)` —— `UnixListener` 只出现在 proto 的签名里，SDK 源码不写它，**clippy 不报**（PLAN 的假设成立；v3 因 M-3 把接管挪进 `connect()`，SDK 要把监听器存进结构体字段，而字段类型必须写出来，所以由 proto 给一个不是别名的包装类型）。
- 正对照 `--features positive-control`（`src/positive_control.rs`：名单每一项恰好一行）→ clippy 退出 101，**恰好 26 条 `use of a disallowed`，去重后也是 26 种，等于 `clippy.toml` 的条目数**（v3，真 clippy 0.1.98）。CI 用三步写死：`! cargo clippy … --features positive-control`、错误条数 == 条目数、去重种类数 == 条目数（任何一项名单写错路径都会让种类数少 1）。

### 2.3 transport + 握手：全在 proto，SDK 只调

proto 新增 `agent24_os_proto::module`（§4），承担 §1.2 的第 1、2、3、5、6、7 项；第 20 项由 `agent24-os-fd` 承担、proto 包一层。SDK 的 `ModuleBuilder::connect()` 只做三件事（v3 在最前面加了第 0 步）：
0. **生产路径先接管监听器**（v3，M-3）：`take_listener()` → fd 3 校验、设 `FD_CLOEXEC`，得到的 `InheritedListener` 存进 `Module`，`serve` 时再交给 axum。放在最前面是为了让 CLOEXEC 在模块有机会起子进程之前就设上（v2 放在 `serve` 里，`connect()` 与 `serve()` 之间模块自己起的子进程会继承 fd 3），同时让坏 fd 在启动时就报错而不是等到 `serve`。`with_env` 测试路径**跳过**这一步（§3.2）；
1. 取 `ModuleEnv`：生产路径 `ModuleEnv::from_env()`；测试路径是 `with_env(env, hook)` 注入的（§3.2）；
2. 从 manifest 文本读出 `name` / `route_namespace` / `kernel_capabilities`，组 `Hello`（**不再手抄能力名**，§1.2 第 4 项），协议范围 `1..=1`（§8 Q7）；
3. `Connection::connect_from_env(&env, &hello, on_fatal)`——proto 内部用宽松的 `InitializeReply` 解析响应、校验 id、校验 `protocol_version ∈ [min, max]`、校验缓冲区无残字节。

「无重连、`Offer` 一代不变」（Sin90 `mod.rs:22-30`、`transport.rs:11-22`；内核侧 `endpoint.rs:11-23`「One connection per generation, by structure」）原样成为 SDK 的公开契约，写在 `Module` 的文档里。

### 2.4 五种类型化客户端（逐一确认）

PLAN S3-2 与 ME4-5.1.2b 原文列的五种是 **Events / Memory / Approval / Scheduler / Model**。逐一对到内核方法与 Sin90 现有实现：

| 客户端 | `Offer` 前缀 | 方法（内核定义位置） | Sin90 现有 | 第二个调用方 |
|---|---|---|---|---|
| `EventsClient` | `_a24/events/` | `emit{kind, payload, request_id?}`（`agentd events_emit.rs:251-256`） | `KernelEventSink`（`mod.rs:425-503`），不发 `request_id` | Cos72（S4）、Python 黑盒 |
| `MemoryClient` | `_a24/memory/private/` | `remember/recall/recent`（`memory_callback.rs:36-67`） | `clients/memory.rs` + `reconciler.rs` 的去重预查 | Cos72（S4）、Python 黑盒 |
| `ApprovalClient` | `_a24/approval/` | `gate/advise/status`（`approval_callback.rs:34-64`） | `clients/approval.rs`（无业务调用方） | Cos72（S4） |
| `SchedulerClient` | `_a24/scheduler/` | `upsert/delete/list`（`scheduler_callback.rs:75-143`） | `clients/scheduler.rs` | Python 黑盒、T14 Node；Cos72 是否申请由 5.3.1 定（§8 Q4 已拍板进 SDK） |
| `ModelClient` | `_a24/model/` | `complete`（`model_callback.rs:68-116, 181-193`） | `clients/model.rs:44-156` | 暂无（§8 Q4 已拍板进 SDK，目前只有 Sin90 用） |

共同规则（从 Sin90 原样继承）：
- 构造函数 `new(&Arc<Connection>) -> Option<Self>`：`Offer` 不覆盖本前缀就返回 `None`，**没有「照样调、调了必败」的路径**（Sin90 `clients/mod.rs:5-11`，architecture.md 不可破边界 #7）。
- 可选字段缺省即不发（「omit, don't null」）。
- **响应类型一律不加** `deny_unknown_fields`（内核加字段不能打坏没重编的模块）——握手响应也不例外（v1 漏了这一处，H2，§4.1）；请求形状必须与内核 `deny_unknown_fields` 的 params 逐字段一致，由 agentd 各回调模块内的 wire 对等测试钉住（J-S7）。
- 每个客户端把它会发的完整方法名写成 `pub const`（如 `memory::REMEMBER = "_a24/memory/private/remember"`）并汇总成 `clients::METHODS`；`Core::call` 只收 `&'static str` 方法名，不拼接——J-S15 的抽取因此不会漏（v1 自审第 6 条）。
- `request_id` 参数类型是 `Option<&RequestId>`，`RequestId` 在正式 API 里**只能**从被代理请求的头里提取——模块不可能「编」一个 id 去绑别人的请求生命周期；`test-util` 下有 `RequestId::for_test`（J-S7 的全 `Some` 调用与 Sin90 test-hooks 用）。

### 2.5 fired 注册点

- 常量 `FIRED_PATH = "/_a24/scheduler/fired"`（相对模块自己的 `route_namespace`；内核 POST 的完整路径是 `/api/v1/<ns>/_a24/scheduler/fired`，ME4-S1 §5.3）。
- 提取器 `FiredDelivery { fire_id, schedule_key, body: FiredBody, ctx: RequestContext }`：`X-A24-Fire-Id` 缺失、body 有未知字段、`scheduled_for`/`fired_at` 不是定宽 ISO-8601 → `FiredRejection`（默认渲染为 400）。非 2xx 会被内核算作一次失败尝试并重投（ME4-S1 §5.3），这正是想要的。
- 注册点 `with_fired(router, handler)`：把 handler 挂到 `FIRED_PATH`。它的文档注释写明内核契约（L11，数值出自 `agent24-scheduler/src/deliveries.rs`）：**整次尝试 10 s 内完成**（`DELIVERY_TIMEOUT`，`:92`）；非 2xx 或超时算一次失败，间隔 5 s、15 s 重投（`RETRY_BACKOFF`，`:63`），**至多 3 次实际发出的尝试**（`MAX_SENT_ATTEMPTS`，`:57`）后该次 fire 记 `failed`；响应体上限 64 KiB（`MAX_FIRED_RESPONSE_BYTES`，`:84`）。所以 handler 应「先按 `fire_id` 去重、尽快回 2xx、慢活放到回复之后」。
- 模块想用自己的错误体形状：用 `Result<FiredDelivery, FiredRejection>` 作提取器自己渲染（scratch 测试 `caller_can_render_rejection_in_its_own_shape` 已验）。
- 语义提醒写进文档注释（不是 SDK 能强制的）：**先按 `fire_id` 去重，再做副作用或提交审批**；`ctx` 里的 request id / 审批 token 只在本次投递内有效（ME4-S1 v2 M10）。
- `FiredBody` 从 agentd 的私有借用类型（`scheduler_deliver.rs:176-183`）挪到 `agent24_os_proto::kernel_call::FiredBody`（拥有型，`Serialize + Deserialize + deny_unknown_fields`），内核序列化、SDK 反序列化用**同一个类型**；判据 J-S10(b) 直接拿内核真实发出的字节喂 SDK 提取器（M7）。

### 2.6 错误类型映射

- `ClientError` 闭集按 Sin90 `clients/error.rs` 原样搬：18 个内核 `kind` 中除握手专用的 `auth_failed`/`manifest_mismatch` 和本来就落 `Other` 的 `invalid_lease`/`unknown_capability`/`version_mismatch` 外各有专属变体；`-32602` → `InvalidParams`；`timeout` + `data.retryable == false` → `RequestNotInFlight`；`unavailable` 必须同时有合法 `cause` 与布尔 `retryable`，否则落 `Other`（Sin90 L4 的「不猜」原则）。
- 两处合并保留（Sin90 L-6）：本端 64 在途满 与 内核 `busy` → `Busy`；本端帧超限 与 内核 `payload_too_large` → `PayloadTooLarge`。
- `UnavailableCause` 改为 SDK 自有的四值闭集（去掉对 Sin90 `crate::ai` 的耦合）。
- `is_permanent()` / `is_retryable()` 原样保留（Sin90 在用：`reconciler.rs:692-728` 的分类 match 里用到）。
- **v2 删除 v1 新增的 `ClientError::retry_class()` / `RetryClass`**（M5，按 §1.1 提取原则取舍）。理由：① 它不是 Sin90 的现有语义，而是对 Sin90 分类表的一次再设计——`Cancelled` 在 Sin90 落「其它 → 退避 + other-bucket 计数」（`reconciler.rs:745-752` 的兜底分支），在 `retry_class` 里却是 `OutcomeUnknown`，两者对同一错误给出不同处置；② Sin90 5.2.1 为零行为变化不会用它；③ Cos72 没有 outbox 代码，第二个调用方不存在。两个调用方都不用的分类表一旦发布就成了 0.x 的兼容负担。Cos72 在自己的 outbox 里用 `is_permanent`/`is_retryable` 加一个**无通配的穷举 match**（§8 Q6 不加 `non_exhaustive` 正是为此：新增变体时编译失败提醒它补分类）。等 Cos72 outbox 落地后，按两个真实调用方的实际分类表再决定是否提取、`Cancelled` 该归哪类（FU-87）。
- **已拍板**：`ClientError` 不加 `#[non_exhaustive]`（§8 Q6）；草图按此写。

### 2.7 超时、重试、并发：谁负责

| 机制 | 归属 | 数值 / 规则 | 依据 |
|---|---|---|---|
| 写超时（含 flush） | proto `Connection` | 10s = `rpc::WRITE_TIMEOUT` | Sin90 `transport.rs:87` |
| 单次调用响应兜底 | proto `Connection`，按调用可覆盖（`CallOptions.response_timeout`） | 默认 35s = `module::DEFAULT_RESPONSE_TIMEOUT`（> `rpc::CALL_TIMEOUT` 30s，让内核的具体错误先到）；编译期断言 `35 > 30`（scratch 已写） | Sin90 `transport.rs:89-94` |
| 推理调用响应期限 | SDK `ModelClient` | `clients::MODEL_RESPONSE_TIMEOUT` = 125s（> 内核 `MODEL_CALL_TIMEOUT` 120s，`model_callback.rs:35`） | ME4-S2 J10a；Sin90 `model.rs:55` |
| 在途上限 | proto `Connection` | 64，第 65 个立即 `Busy`（默认不排队）；`CallOptions.slot_wait` 可选有界等待 | Sin90 `transport.rs:24-34` |
| 取消 | proto `Connection` | 丢弃调用 future 即 best-effort 发 `$/cancelRequest`（`params: {id}`，无顶层 id）；迟到响应静默丢弃 | Sin90 `transport.rs:50-61`。PLAN 说的「call/notify/cancel」里的 cancel 就是这个 drop 语义，不另开显式 `cancel` 方法；`notify` 无调用方，v2 删除（L6） |
| 事件队列（容量 / worker / 子配额 / 等位） | SDK `EventSink`，`EventSinkConfig` 可调 | 默认 256 / 4 / 32 / 5s（与 Sin90 相同） | Sin90 `mod.rs:86-122` |
| **重试** | **调用方** | SDK 与 proto 永不自动重试。`NotSent` 表示没上线；`ConnectionLost`/`Timeout`/`Cancelled` 表示结果未知 | Sin90 `transport.rs:36-48`、`error.rs:262-279` |
| 退避曲线、outbox、耗尽阈值、错误分类表 | **调用方** | SDK 只给 `is_permanent()` / `is_retryable()`（§2.6） | M5 |
| 幂等写记忆 | SDK `remember_once` 提供预查；**并发控制归调用方** | 非原子，同一 `dedup_key` 只允许一个写者（§3.5） | H3 |
| 推理的每模块并发上限 | **调用方**（内核另有自己的 2 并发） | Sin90 `SemaphoredModelCaller` | ME4-S2 §5 |
| 连接死后怎么办 | SDK 提供 `FatalHook` 注入；**默认** `warn!` + `std::process::exit(70)` | 与 Sin90 `main.rs:138-145` 相同；supervisor 起新一代。测试入口 `with_env` 必须同时给钩子（§3.2） | 已拍板：默认可接受，可用 `on_connection_lost` 覆盖（§8 Q10） |

### 2.8 版本与兼容

- **SDK 自身**：SemVer 从 `0.1.0` 起（PLAN S3-3）。发布方式：Agent24 仓库打 tag `agent24-os-sdk-v0.1.0`，外部仓库 `agent24-os-sdk = { git = "https://github.com/iDoris-ai/Agent24", tag = "agent24-os-sdk-v0.1.0" }`。0.x 期间 minor 升级即可破坏兼容；新增错误 `kind` 属于破坏性变化（§8 Q6）。
- **CHANGELOG**：按 §8 Q9，每个 SDK tag 在 `rust/crates/agent24-os-sdk/CHANGELOG.md` 有独立小节，写明「最低内核版本」；`v0.1.0` 这一节随 c2（打 tag 的那个 PR）写入（L10）。
- **工具链**（L5 更正 v1 的错误陈述）：Agent24 CI 装的是 **stable**（`.github/workflows/ci.yml:61`、`:195`：`rustup toolchain install stable`），不是 1.98.0；`1.98.0` 是本仓库判据命令与本地验证钉的版本（`cargo +1.98.0 …`）。Sin90 CI 同样用 stable（`dtolnay/rust-toolchain@stable`，`.github/workflows/ci.yml:24-25`）。edition 2024 + agentd 已用 let-chains（`model_callback.rs:126-127`、`133-134`）要求 Rust ≥ 1.88。SDK `Cargo.toml` 写 `rust-version = "1.88"`，并在 a3-skel 的 CI 步骤里加一条 `rustup toolchain install 1.88 --profile minimal && cargo +1.88 check -p agent24-os-sdk`，防止 SDK 无意中抬高 MSRV 打坏钉旧工具链的下游（v1 把「要不要这条作业」留给评审，v2 定为要：一次 `check` 的成本很低）。
- **wire 协议版本**：SDK 在 `initialize` 里声明的范围。Sin90 今天是 `1..=1000`（`mod.rs:73-74`），内核 `negotiate` 取 `min(module.max, kernel.max)`（`version.rs:175-189`），内核是 v1（`agent24-domain lib.rs:397`）→ 今天两种写法结果都是 1。但 `1..=1000` 意味着将来 v2 内核会和只懂 v1 的 SDK「协商成功」地讲 v2。**已拍板**：SDK 只声明它实际实现的范围 `1..=1`（§8 Q7）；scratch 常量已改为 `PROTOCOL_MAX = 1`（L1），J-S11 断言握手里是 `{min:1,max:1}`。proto 另外校验内核回的 `protocol_version` 落在声明范围内，否则 `ConnectError::Protocol`。
- **内核加字段**：响应类型宽松解析（含握手响应，§4.1），不破坏旧模块。**内核加必填参数**：属于 wire 破坏，SPEC 与 SDK 同 PR 改，SDK minor 升。**内核加错误 kind**：落 `Other` 直到 SDK 升级（deny by default），不 panic。
- **manifest 加字段**：SDK 只读三个字段、忽略其余（`ManifestFacts` 不带 `deny_unknown_fields`，L4）。manifest 的合法性由内核在安装时校验，SDK 不做第二遍校验。

### 2.9 非 Rust 模块（T14 Node.js）如何对齐 wire

- **唯一真相是 wire，不是 SDK。** T14 的判据是「只拿 `docs/specs/WIRE-OOP-MODULE.md` 的子代理写得出 Node 模块」（PLAN S5）。所以 SDK 的每一条行为约定都必须能在 wire 文档里找到出处，反过来 SDK 不许有 wire 文档之外的「隐藏协议」。
- 为此本设计要求：SDK 源码里所有协议常量（方法名、头名、路径、环境变量名、超时关系）要么引用 proto 常量，要么在 wire 文档里有同名条目。ME4-5.4.1 的 PR 附一张「SDK 常量 → wire 文档章节」对照表（判据 J-S15），子代理卡住的点补文档不补代码。
- Rust 与 Node 在三处容易跑偏，wire 文档必须写明（本文先列出，供 5.4.1 用）：① 握手后缓冲区不许有残字节（Sin90 `mod.rs:388-400`）；② 调用 id 是字符串、响应不保证顺序、按 id 匹配；③ 可选字段缺省不发、不要发 `null`。外加 v2 新增两条：④ 握手响应与所有方法响应都可能多出字段，解析端必须忽略未知字段；⑤ fired 的 10 s / 3 次尝试契约（§2.5）。
- Python 黑盒模块（`me4_scheduler_blackbox.rs:75-240`）是 PLAN 点名的第二个提取来源。它证明了「不借 SDK、只按 wire 也能做出 `initialize` → upsert → fired → 绑定 request_id 的 remember」，同时暴露了一个 SDK 必须替开发者挡住的坑：它的 `rpc()` 是串行的一问一答，读到的下一行默认就是自己的响应——一旦并发就错。这是 transport 必须按 id 分发的实证。

### 2.10 advise 与孤儿审批（M9，给 Cos72 的约束）

事实（`approval_callback.rs`）：`advise` 在提交时只写库、不等人（`:458-475`，judgement 9）；**同一 `{request_id, approval_token}` 重复提交在 wire 上幂等，返回同一个 `approval_id`**（`:478-487`，judgement 16a）；`request_id`/`approval_token` 只在所属被代理请求存活期间有效；审批有过期（`module_approvals.rs:67`、`:124`：`pending`/`timed_out`，「already resolved or has expired」）。模块只有 `status(approval_id)`，**没有「按内容列出我的审批」的方法**——不知道 id 的审批，模块永远查不到。

由此给 Cos72（ME4-5.3.x）的写法约束，写进 `ApprovalClient::advise` 的文档注释（scratch 已写）与 Cos72 的设计：
1. **advise 必须在 submit 的 HTTP handler 内同步完成**（token 随请求结束而失效，不能挪到 outbox 里异步发）。
2. **在同一请求内重试**：`Timeout` 用同一对 `{request_id, approval_token}` 重试，拿到的是同一个 `approval_id`，不会产生第二条审批；`ConnectionLost`/`NotSent` 意味着这一代结束（exit 70），不重试，请求失败。
3. **先落本地再 advise**：handler 先在 Cos72 库里写一条 `pending_award`（`award_id` 唯一，与任务提交同一事务），advise 的 `payload` 带 `award_id`；advise 成功后把 `approval_id` 写回该行。
4. **孤儿的来源与补偿**：advise 已在内核落库、但 Cos72 没记下 `approval_id`（写回失败、进程在回复前死掉、请求被客户端提前取消）——内核里就多了一条 Cos72 永远不会去 `status` 的审批。补偿办法：
   - 入账只认 Cos72 自己记下的 `approval_id`，且积分账本以 `award_id` 唯一约束入账——孤儿即使被人批准也**不会入账，更不会重复入账**（错误方向是「少发」而不是「多发」）；
   - 用户重新提交（新请求 = 新 token）时，Cos72 看到 `approval_id IS NULL` 的 `pending_award`，发一次新的 advise 并记下新 id；旧孤儿留在内核里直到过期（`timed_out`）；
   - 孤儿的 `payload` 里有 `award_id`，人工审批界面上能看出它与新审批是同一笔；被批准的孤儿无效这一点写进 Cos72 的用户文档。
   - 根治（模块能收到裁决回推、或能按 payload 查询）属于 FU-84 的范围，本轮不做。

---

## 3. 公共 API 草图（scratch 已编译，节选）

> 全部来自 `scratchpad/sdk-sketch/sdk/`。proto 部分是桩（只有签名有效），`agent24-os-fd` 是实物，见 §4 与附录 B。

### 3.1 crate 根导出

```rust
pub use agent24_os_proto::kernel_call::{FireTrigger, FiredBody};
pub use agent24_os_proto::module::{FatalHook, SPAWN_ENV_VARS};   // FatalHook: newtype since v3
pub use clients::{
    ApprovalClient, EventSink, EventSinkConfig, EventsClient, MemoryClient, ModelClient,
    RememberOnce, SchedulerClient,
};
pub use context::{ApprovalToken, RequestContext, RequestId};
pub use error::{ClientError, UnavailableCause};
pub use fired::{FIRED_PATH, FiredDelivery, FiredRejection, with_fired};
pub use module::{Module, ModuleBuilder, SdkError};

/// proto's in-memory fake kernel, forwarded (L8).
#[cfg(feature = "test-util")]
pub use agent24_os_proto::module::testing;
```

SDK 的 `[features]`：`test-util = ["agent24-os-proto/test-util"]`（转发，L8——模块的 dev 依赖只写 SDK 一个），外加两个只给判据用的正对照 feature `positive-control`（J-S1）与 `positive-control-unsafe`（J-S3）。

### 3.2 `Module`（全文见 scratch `sdk/src/module.rs`）

```rust
/// Protocol range this SDK version implements — exactly what it speaks (§8 Q7).
pub const PROTOCOL_MIN: u32 = 1;
pub const PROTOCOL_MAX: u32 = 1;

pub struct ModuleBuilder { /* manifest_yaml: Cow<'static, str>, on_fatal: Option<FatalHook>, env: Option<ModuleEnv> */ }

impl ModuleBuilder {
    /// Default: log + `std::process::exit(70)` (Sin90's behaviour).
    pub fn on_connection_lost(mut self, hook: FatalHook) -> Self { … }
    /// Tests only: connect with `env` instead of the process environment.
    /// The fatal hook is a REQUIRED argument, so a test can never fall back
    /// to the default `exit(70)` (M3). Does NOT take the inherited listener;
    /// `serve` then returns `SdkError::NoListener` (v3 M-3).
    #[cfg(feature = "test-util")]
    pub fn with_env(mut self, env: ModuleEnv, hook: FatalHook) -> Self { … }
    /// Production path: FIRST take over the inherited listener (fd 3,
    /// `FD_CLOEXEC`) and keep it in the `Module`, then read env, parse
    /// manifest (name / namespace / kernel_capabilities come from it — never
    /// hand-copied), dial + `initialize`. Once per process.
    pub async fn connect(self) -> Result<Module, SdkError> { … }
}

impl Module {
    /// `manifest_yaml` must be the exact bytes the kernel digests
    /// (typically `include_str!("../domain-os.yml")`; Sin90 passes its
    /// feature-selected `sin90::ai::MANIFEST_YAML`). `Cow` so a test can
    /// build a manifest at runtime.
    pub fn builder(manifest_yaml: impl Into<Cow<'static, str>>) -> ModuleBuilder { … }
    pub fn data_dir(&self) -> &Path { … }
    pub fn offer(&self) -> &Offer { … }
    pub fn is_alive(&self) -> bool { … }
    pub fn events(&self) -> Option<EventsClient> { … }
    pub fn memory(&self) -> Option<MemoryClient> { … }
    pub fn approval(&self) -> Option<ApprovalClient> { … }
    pub fn scheduler(&self) -> Option<SchedulerClient> { … }
    pub fn model(&self) -> Option<ModelClient> { … }
    /// Nest `router` under the manifest's `route_namespace` and serve it on
    /// the kernel-bound listener taken in `connect()`. Returns when the
    /// server stops.
    pub async fn serve(self, router: axum::Router) -> Result<(), SdkError> { … }
}

pub struct Module { /* env, facts, conn: Arc<Connection>, listener: Option<InheritedListener> */ }

pub enum SdkError { Env(EnvError), Manifest(String), Connect(ConnectError), Listen(ListenError),
                    NoListener /* built with `with_env` */, Io(std::io::Error) }
```

- `manifest_yaml: impl Into<Cow<'static, str>>`（L3）：生产代码传 `&'static str`（Sin90 的 `MANIFEST_YAML` 本来就是 `include_str!` 的结果，`ai/ports.rs:122-124`；它 `main.rs:45` 的 `const MANIFEST: &[u8]` 迁移后不再需要），测试可以传运行时拼的 `String`（J-S11 的「换一份能力不同的 manifest」不用 `String::leak`）。
- 测试入口的非退出保证是**签名上的**：注入环境的唯一途径 `with_env` 必须同时给钩子，`test-util` 提供 `testing::recording_hook()`（计数、不退出）与 `testing::noop_hook()`。没有做成「开了 `test-util` 就换默认钩子」：cargo resolver 2 在 `cargo test` 时会把 dev 依赖的 feature 合并进为集成测试构建的二进制，Sin90 真实挂载黑盒跑的正是这个二进制，默认钩子一变，退出码 70 这条 §5.2 验收就被悄悄改了。
- **监听器在 `connect()` 里接管**（v3，M-3）：生产路径（没调 `with_env`）的第一步是 `agent24_os_proto::module::take_listener()`（内部调 `agent24_os_fd::take_inherited_listener()`），得到的 `InheritedListener` 存进 `Module`，`serve` 只是取出来交给 axum。`with_env` 路径不碰 fd 3、不消耗那一次机会，`serve` 返回 `SdkError::NoListener`——测试直接对 router 用 `tower::ServiceExt::oneshot`，不经 `serve`。
- **进程级一次性的后果**（v3，M-3）：接管在一个进程里只能成功一次，而 `cargo test` 把同一个测试二进制里的所有测试放在同一个进程里跑。所以**每个测试二进制里至多一个测试可以走到 `take_inherited_listener`/`take_listener`/不带 `with_env` 的 `connect()`**：os-fd 里是 `only_one_attempt_per_process`（其余 os-fd 测试只调私有的 `validate`）；proto、SDK、Sin90、Cos72 的单测一律经 `with_env`/`fake_kernel`，不调 `take_*`；真实挂载黑盒是独立子进程，不受影响。这条写进 `take_listener` 与 `ModuleBuilder::connect` 的文档注释。

### 3.3 请求上下文

```rust
/// `X-A24-Request-Id` of the proxied request being handled.
pub struct RequestId(String);              // no public constructor in the normal API
impl RequestId {
    pub fn as_str(&self) -> &str { … }
    /// Tests only (M6's all-`Some` wire-parity calls, Sin90's test hooks).
    #[cfg(feature = "test-util")]
    pub fn for_test(id: &str) -> Self { … }
}

/// `X-A24-Approval-Token`: secret, redacted in Debug.
pub struct ApprovalToken(String);          // no public constructor

/// Kernel-injected per-request context. Extractor never fails.
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    pub request_id: Option<RequestId>,
    pub approval_token: Option<ApprovalToken>,
}
impl RequestContext { pub fn from_headers(h: &axum::http::HeaderMap) -> Self { … } }
impl<S: Send + Sync> FromRequestParts<S> for RequestContext { type Rejection = std::convert::Infallible; … }
```

### 3.4 错误

```rust
pub enum UnavailableCause { NoProvider, RequestRejected, BackendConfig, ResponseTooLarge }

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ClientError {
    Forbidden(String), RateLimited(String), Busy(String), QuotaExceeded(String),
    InvalidParams(String), Timeout(String), RequestNotInFlight(String), NotReady(String),
    Draining(String), Revoked(String), TokenInvalid(String), PayloadTooLarge(String),
    NotFound(String), ConnectionLost, NotSent(String),
    Unavailable { retryable: bool, cause: UnavailableCause },
    Cancelled, Other(String),
}

impl ClientError {
    pub fn is_permanent(&self) -> bool { … }   // Sin90 原样
    pub fn is_retryable(&self) -> bool { … }   // Sin90 原样
}
```

（上面为可读性把变体压成一行；scratch 里每个变体带 `#[error(..)]`。v1 的 `RetryClass` / `retry_class()` 已删，§2.6。）

### 3.5 五个客户端

```rust
// module `events`: pub const EMIT: &str = "_a24/events/emit"; pub const METHODS: [&str; 1]
// (each client module has its full method-name consts + METHODS the same way)
impl EventsClient {
    pub fn new(conn: &Arc<Connection>) -> Option<Self> { … }
    pub async fn emit(&self, kind: &str, payload: Map<String, Value>,
                      request_id: Option<&RequestId>) -> Result<(), ClientError> { … }
    pub fn spawn_sink(&self, cfg: EventSinkConfig) -> EventSink { … }
}
pub struct EventSinkConfig { pub queue_capacity: usize, pub workers: usize,
                             pub sub_quota: usize, pub slot_wait: Duration }  // Default = 256/4/32/5s
impl EventSink { pub fn emit(&self, kind: &str, payload: Map<String, Value>) { … }
                 pub fn dropped(&self) -> u64 { … } }

// module `memory`
pub const DEDUP_KEY_FIELD: &str = "dedup_key";
pub const RECALL_PRECHECK_MAX_PAGES: usize = 10;
pub const RECALL_PRECHECK_PAGE_SIZE: usize = 50;

pub enum RememberOnce {
    /// No marker found in the scanned history; `remember` was called.
    Created { id: String, at: String },
    /// A memory whose `body.dedup_key` equals the marker exists; nothing sent.
    Found { id: String },
    /// Scanned RECALL_PRECHECK_MAX_PAGES pages and `cursor` was still set:
    /// genuinely unknown. NOT "absent" — try again later. Nothing sent.
    Inconclusive,
}

impl MemoryClient {
    pub fn new(conn: &Arc<Connection>) -> Option<Self> { … }
    /// NOT idempotent on the wire: every success mints a new id.
    pub async fn remember(&self, kind: &str, body: Map<String, Value>,
                          request_id: Option<&RequestId>) -> Result<Remembered, ClientError> { … }
    /// Write `body` at most once per `dedup_key` (see below).
    pub async fn remember_once(&self, kind: &str, dedup_key: &str, body: Map<String, Value>,
                               request_id: Option<&RequestId>) -> Result<RememberOnce, ClientError> { … }
    pub async fn recall(&self, query: &str, page_size: usize, cursor: Option<&str>,
                        request_id: Option<&RequestId>) -> Result<RecallPage, ClientError> { … }
    pub async fn recent(&self, page_size: usize, cursor: Option<&str>,
                        request_id: Option<&RequestId>) -> Result<RecallPage, ClientError> { … }
}

pub struct ApprovalSubmit<'a> {
    pub action: &'a str, pub target: Option<&'a str>, pub payload: Value,
    pub request_id: &'a RequestId, pub approval_token: &'a ApprovalToken,
}
impl ApprovalClient {
    pub fn new(conn: &Arc<Connection>) -> Option<Self> { … }
    /// Kernel-executed closed set only (today: `schedule_callback`).
    pub async fn gate(&self, s: &ApprovalSubmit<'_>) -> Result<ApprovalAnswer, ClientError> { … }
    /// Module-domain action. Call INSIDE the proxied request; same
    /// {request_id, token} resubmit is idempotent at the wire (§2.10).
    pub async fn advise(&self, s: &ApprovalSubmit<'_>) -> Result<ApprovalAnswer, ClientError> { … }
    pub async fn status(&self, approval_id: &str) -> Result<ApprovalAnswer, ClientError> { … }
}

pub struct UpsertRequest<'a> { pub key: &'a str, pub spec: &'a ScheduleSpec,
                               pub enabled: bool, pub label: Option<&'a str> }
impl SchedulerClient {
    pub fn new(conn: &Arc<Connection>) -> Option<Self> { … }
    pub async fn upsert(&self, r: &UpsertRequest<'_>,
                        request_id: Option<&RequestId>) -> Result<UpsertResult, ClientError> { … }
    pub async fn delete(&self, key: &str,
                        request_id: Option<&RequestId>) -> Result<DeleteResult, ClientError> { … }
    pub async fn list(&self, request_id: Option<&RequestId>) -> Result<ListResult, ClientError> { … }
}

pub const MODEL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(125);
pub struct CompleteRequest { pub messages: Vec<ModelMessage>, pub response_format: Option<JsonSchemaFormat>,
                             pub max_tokens: Option<u32>, pub complexity: Option<Complexity> }
impl ModelClient {
    pub fn new(conn: &Arc<Connection>) -> Option<Self> { … }
    pub async fn complete(&self, req: &CompleteRequest,
                          request_id: Option<&RequestId>) -> Result<CompleteResult, ClientError> { … }
}

/// Every `_a24/*` method the SDK can send, per client (J-S15's source).
pub const METHODS: [&[&str]; 5] = [&events::METHODS, &memory::METHODS, &approval::METHODS,
                                   &scheduler::METHODS, &model::METHODS];
```

**`remember_once` 的语义（H3，写死）**：
- **标记**：把 `dedup_key` 写进 `body["dedup_key"]`（`DEDUP_KEY_FIELD`）——与 Sin90 今天写的键完全相同（`reconciler.rs:365`），所以迁移前后写下的记忆互相认得。`body` 里已有同名键且值不同 → 本地返回 `ClientError::InvalidParams`，一次调用都不发；值相同 → 照常。
- **预查**：`recall(query = dedup_key, page_size = 50, cursor)` 顺 `cursor` 最多翻 10 页；只有 `item.body.dedup_key` **逐字相等**才算找到（`recall` 是子串匹配，`"r:1"` 会命中 `"r:10"`，J-S18 用这个近似项做负样本）；**不比较 `kind`**（与 Sin90 相同，调用方应让 `dedup_key` 自带命名空间，如 `review:<id>`）。常量与 Sin90 相同（`reconciler.rs:255`、`:260`）。
- **结果**：找到 → `Ok(Found { id })`（不发 `remember`，返回已存在那条的 id，对应 Sin90 L5「同一 `result_ref`」）；`cursor` 用尽仍未找到 → 发 `remember`，`Ok(Created { id, at })`；10 页后 `cursor` 仍非空 → `Ok(Inconclusive)`（不发 `remember`）。任何 `recall`/`remember` 错误原样返回。
- **`Inconclusive` 的映射**：SDK 把它做成 `Ok` 的一个变体而不是 `ClientError::Other`，理由是「还不知道」不是内核报的错，塞进 `Other` 会和内核真错误混在一起、无法区分；调用方必须显式处理它。**Sin90 为零行为变化**在自己的 `remember_review_summary` 里把 `Inconclusive` 映射回今天的 `Err(ClientError::Other(<原文案>))`（`reconciler.rs:352-361`），于是它的 outbox 分类 match（Other → 退避 + other-bucket 计数）一行不改。
- **非原子，只允许单写者**：预查与写入之间没有锁，两个并发调用同一 `dedup_key` 都可能看到「没有」然后各写一条。调用方负责「同一 `dedup_key` 同时只有一个调用在飞」。Sin90：只有一个 outbox 泵任务，天然单写者。**Cos72：任务完成摘要不得从 HTTP handler 里直接调 `remember_once`**（两个并发的 submit 请求、或客户端重试，会并发打同一个键）；handler 只在自己库里入一条 outbox 行（`dedup_key` 唯一约束），由单一泵任务调 `remember_once`。模块一代只有一个进程（内核 `endpoint.rs:11-23` 一代一连接 + `drain.rs:856` 不叠跑两个 supervisor），所以进程内串行化就够。
- **残余风险**：一次 `remember` 在内核已提交、但模块侧看到 `Timeout`，随后重试的预查若恰好早于内核提交可见，仍可能写出第二条。与 Sin90 今天完全相同；FU-85（内核幂等键）根治，届时 `remember_once` 改走内核幂等键、签名不变。
- **v0.1.0 的调用方**：Sin90 在 5.2.1 改用 SDK 版（删自己的 `memory_recall_finds_dedup_key`/`RecallCheck`/两个常量，约 90 行），这与 §5.1「能搬进 SDK 的都删掉」一致，也让 `remember_once` 在发布时就有一个真实调用方和 Sin90 现成的一整套 reconciler 测试做验收（§5.2 第 2、3 条）；Cos72 是第二个。

数据类型（`ScheduleSpec`/`ScheduleState`/`LastFire(s)`/`UpsertOutcome`/`DeleteOutcome`/`Remembered`/`Recollection`/`RecallPage`/`ApprovalAnswer`/`ApprovalKind`/`ApprovalDecision`/`ModelMessage`/`ModelRole{System,User,Assistant}`/`Complexity`/`JsonSchemaFormat`/`ServedTier`/`Usage{u64,u64}`/`CompleteResult`）字段与 Sin90 现有类型一一对应，只改了三处：`ModuleSpec → ScheduleSpec`、`ModuleScheduleState → ScheduleState`（去掉「Module」前缀，SDK 里一切都是模块的；Sin90 在 shim 里 `use … as` 保留旧名，§5.1）；`ModelRole` 补 `Assistant`、`Usage` 用 `u64`（按内核 wire）。

与 Sin90 现签名的差别（迁移时要改的调用点）：`request_id: Option<&str>` → `Option<&RequestId>`（Sin90 业务代码全部传 `None`，调用点文本不变）；`SchedulerClient::upsert` 五个位置参数 → `UpsertRequest` 结构体；`ApprovalClient::gate/advise` 五个参数 → `ApprovalSubmit`。

### 3.6 fired（全文见附录 B.3）

```rust
pub const FIRED_PATH: &str = "/_a24/scheduler/fired";

pub struct FiredDelivery {
    pub fire_id: String,
    pub schedule_key: Option<String>,
    pub body: FiredBody,          // agent24_os_proto::kernel_call::FiredBody
    pub ctx: RequestContext,
}
pub enum FiredRejection { MissingFireId, BadBody(String), BadTimestamp }
impl IntoResponse for FiredRejection { … }                 // 400 {"error":{"code":"invalid_request",...}}
impl<S: Send + Sync> FromRequest<S> for FiredDelivery { type Rejection = FiredRejection; … }

/// … Kernel contract: finish within 10 s (DELIVERY_TIMEOUT); non-2xx/timeout is a
/// failed attempt, retried after 5 s then 15 s, 3 sent attempts at most
/// (MAX_SENT_ATTEMPTS) before the fire is `failed`; response body ≤ 64 KiB.
pub fn with_fired<S, H, T>(router: Router<S>, handler: H) -> Router<S>
where S: Clone + Send + Sync + 'static, H: Handler<T, S>, T: 'static { … }
```

### 3.7 `examples/minimal.rs`（scratch 里编译通过）

```rust
const MANIFEST: &str =
    "name: minimal\nroute_namespace: /api/v1/minimal\nkernel_capabilities: [events, scheduler]\n";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let module = Module::builder(MANIFEST).connect().await?;
    let sink = module.events().map(|e| e.spawn_sink(Default::default()));
    let fired_sink = sink.clone();

    let app = Router::new().route(
        "/hello",
        get(move |ctx: RequestContext| async move {
            if let Some(s) = &sink {
                s.emit("hello.seen", Map::new());
            }
            axum::Json(json!({ "bound": ctx.request_id.is_some() }))
        }),
    );
    let app = with_fired(app, move |d: FiredDelivery| async move {
        // dedupe by d.fire_id in your own store first, then act
        if let Some(s) = &fired_sink {
            let mut m = Map::new();
            m.insert("key".into(), json!(d.body.key));
            s.emit("minimal.fired", m);
        }
        axum::Json(json!({ "status": "recorded" }))
    });
    module.serve(app).await?;
    Ok(())
}
```

（真实 example 的 manifest 走 `include_str!("minimal/domain-os.yml")`，并补 `impl_kind`/`spawn` 等安装必需字段；上面为了 scratch 自包含而内联。）

### 3.8 `test-util`（SDK 转出 proto 的 `module::testing`）

见 §4.5。SDK 自己的单测、J-S5/J-S6/J-S11、Sin90 与 Cos72 的单测都只经它拿连接；它在 proto 里用 `UnixStream::pair()` / 临时目录里的 `UnixListener`，不受 SDK 的 clippy 约束。

---

## 4. proto 与 `agent24-os-fd` 要补的公开 API（5.1.2a 的主体）

全部放在新模块 `agent24_os_proto::module`（模块侧）与新 crate `agent24-os-fd`，不碰既有内核侧代码路径（`manifest_digest` 的搬家与 `FiredBody` 的挪动除外，二者都是「一处算法 / 一个类型」的收拢）。签名见附录 B（scratch，已编译）。

### 4.1 环境与握手

- `ModuleEnv`：字段私有，只有 `data_dir` / `callback_sock` / `handshake_token` 三个（**不持 fd 号**，H1），`handshake_token` 在 `Debug` 里脱敏。
  - `ModuleEnv::from_env() -> Result<ModuleEnv, EnvError>`：读 `launch::ENV_DATA_DIR/ENV_CALLBACK_SOCK/ENV_HANDSHAKE_TOKEN`。
  - `ModuleEnv::from_vars(get: impl FnMut(&str) -> Option<OsString>) -> Result<ModuleEnv, EnvError>`（M3）：同一套解析，查表函数由调用方给；`from_env` 就是 `from_vars(|k| std::env::var_os(k))`。测试与假内核用它造环境，不碰进程环境变量（edition 2024 里 `set_var` 是 `unsafe`，测试本来也写不了）。
  - `pub const SPAWN_ENV_VARS: [&str; 4]`：四个 spawn 变量名，给「自己还要起子进程」的模块做 `Command::env_remove` 用（§4.3 token 一段）。
- `Hello<'a> { module, manifest_bytes, capabilities, protocol_min, protocol_max }`。
- `Connection::connect_from_env(&ModuleEnv, &Hello, FatalHook) -> Result<Connection, ConnectError>`：拨 `A24_CALLBACK_SOCK`，发 `InitializeRequest`（用既有 `InitializeParams` 序列化，不再手拼 `json!`），读响应用 `rpc::read_frame_async`，**解析用 `module::InitializeReply`**，校验 id 相同、`protocol_version ∈ [protocol_min, protocol_max]`、缓冲区无残字节，然后 spawn mux 任务。
- **握手响应的宽松解析（H2）**：proto 既有的 `InitializeResult` 带 `#[serde(deny_unknown_fields)]`（`initialize.rs:148-150`），违背 §2.4「响应类型不加 `deny_unknown_fields`」。评审给的两个选项里取「模块侧用宽松镜像类型」，不取「SPEC 明文冻结 + 对等测试」：
  - `pub struct InitializeReply { pub protocol_version: u32, pub offer: Offer }`，**不带** `deny_unknown_fields`（`Offer` 本身也不带，`initialize.rs:119-120`）。内核侧 `InitializeResult` 保持原样不动（它是内核写出的类型，严格无害）。
  - 理由：冻结意味着内核以后给握手响应加任何字段（例如能力版本、限额提示）都是 wire 破坏、所有模块必须同步升级——握手是每个模块启动的第一步，这里最不该脆；而宽松镜像只多一个两字段的类型和一条测试。
  - 判据 J-S19（proto 单测，scratch 已跑通）：带多余字段（顶层 `future` 与 `offer.x`）的响应，`InitializeResult` 解析失败而 `InitializeReply` 成功且值正确；任一 `InitializeResult` 序列化后用 `InitializeReply` 解析得到相同的两个字段。
  - 与 Sin90 的差异：Sin90 今天连 `protocol_version`、`offer` 缺失都接受（`mod.rs:379-386` 用 `unwrap_or_default`）；SDK 要求二者存在（内核的 `InitializeResult` 两个字段都是必填，真内核不会缺）。这是有意的收紧，列进 §5.2 第 2 条的「有意差异」清单。
- `manifest_digest` 从 `agent24-os-packages/src/discovery.rs:46-57` 挪进 `agent24_os_proto::manifest`（§4.4）。

### 4.2 多路复用连接

- `Connection::{offer, is_alive, call}`；`CallOptions { response_timeout, slot_wait }`，默认 `DEFAULT_RESPONSE_TIMEOUT`（35s）/ 不等位；编译期断言 `DEFAULT_RESPONSE_TIMEOUT > rpc::CALL_TIMEOUT`。**签名在写 mux 核心时就落定为最终形态（v4，L-e）**：`call(&self, method: &'static str, params: Value, opts: CallOptions) -> impl Future<Output = Result<Value, CallError>>`——`CallOptions` 类型本身随 mux 核心一起定义；取消是「丢弃这个 future」这一行为本身（`CallGuard::drop` 发 `$/cancelRequest`），随后补的取消/超时能力（响应兜底超时、写超时、`slot_wait`）都不需要再改 `call` 的签名或新增参数，只是往 `CallOptions`/内部状态机加字段与逻辑。
- `CallError { NotSent, ConnectionLost, Busy, FrameTooLarge, Timeout, IdCollision, Rpc(RpcErrorInfo) }`，与 Sin90 `TransportError` 一一对应；`RpcErrorInfo { code, kind, message, data }`（Sin90 的 `raw` 改成只留 `data`，SDK 需要的 `retryable`/`cause` 都在 `data` 里；映射不变由 §5.1 入站金样钉住）。
- 实现 = Sin90 `transport.rs:63-631` 的移植（单写任务、读任务、pending 表、`declare_dead` 恰好一次、`CallGuard` drop 发取消），常量改为引用 `rpc::*`；Sin90 的 16 条 transport 测试一并移植为 proto 测试（判据 J-S8）。按 SZ-1 口径拆 a2-core / a2-cancel 两个 PR（§7）。
- **v2 删除 `notify`**（L6）：今天没有任何模块→内核的通知方法，`$/cancelRequest` 由 `Connection` 内部在 drop 时发；按 §1.1「无调用方不进」删除。PLAN S3-1 的「call/notify/cancel」措辞同步改为「类型化的 `call`（丢弃即取消）」（M8）。

### 4.3 监听 fd：`agent24-os-fd`（H1，签名写死）

```rust
// crate agent24-os-fd — exports NO safe function that accepts a raw fd.
pub const ENV_LISTEN_FD: &str = "A24_LISTEN_FD";   // == proto launch::ENV_LISTEN_FD
pub const LISTEN_FD: RawFd = 3;                    // == proto launch::LISTEN_FD (compile-time assert in proto)

pub enum InheritError { AlreadyTaken, Missing, NotANumber, WrongFd(RawFd),
                        NotASocket, NotUnix, NotStream, NotListening, Io(std::io::Error) }

/// Take over the kernel-bound listening socket, exactly once per process.
pub fn take_inherited_listener() -> Result<std::os::unix::net::UnixListener, InheritError>;

// crate agent24-os-proto, module `module`
pub enum ListenError { Inherit(agent24_os_fd::InheritError), Io(std::io::Error) }
/// Named struct, not an alias (J-S1b): the SDK stores it in `Module`.
pub struct InheritedListener(tokio::net::UnixListener);
impl InheritedListener { pub fn into_tokio(self) -> tokio::net::UnixListener; }
pub fn take_listener() -> Result<InheritedListener, ListenError>;   // os-fd + UnixListener::from_std
```

`take_inherited_listener()` 的步骤（scratch 实现，附录 B.2）：
1. **进程级一次**：`static TAKEN: AtomicBool`，`swap(true)` 已为 true → `AlreadyTaken`。任何一次尝试（成功或失败）都消耗掉这一次机会——失败后不许再试，避免第二次调用在别人已经占用 fd 3 之后去接管。
2. **自读 env**：只认 `A24_LISTEN_FD`，解析为整数且**必须等于 3**（内核 `launch.rs:42` 永远传 fd 3；`agent24-os-fd` 不能依赖 proto，所以自己重述常量，proto 里 `const _: () = assert!(agent24_os_fd::LISTEN_FD == launch::LISTEN_FD)`，env 名由 proto 单测断言相等）。调用方无法传入 fd 号——`ModuleEnv` 不持 fd，os-fd 不导出任何接受 `RawFd` 的安全函数（`adopt(fd)` 是私有的）。
3. **先借用校验、再接管**：`BorrowedFd::borrow_raw(3)` 上做 `fstat` → `S_ISSOCK`；`getsockname` → `AF_UNIX`；`SO_TYPE` → `SOCK_STREAM`；是否在监听，两个平台强度不同（v3，M-1）：
   - **Linux（及一切非 Apple 目标）**：`SO_ACCEPTCONN`，直接判定「调用过 `listen()`」。只 `socket()`/`bind()` 未 `listen()` 的流 socket → `NotListening`（测试 `bound_but_not_listening_is_rejected`，只在非 Apple 目标编译运行）。
   - **macOS：弱校验，已接受**。macOS 不实现 `SO_ACCEPTCONN`（系统头声明了常量，`getsockopt` 返回 `ENOPROTOOPT`，`scratchpad/h1probe/` 实测；rustix 在 apple 上直接不提供 `socket_acceptconn`），改用 `getpeername` 做间接判定「没有对端」：`ENOTCONN` → 视为监听；`Ok`（有对端：已连接流、存活的 socketpair 端）→ `NotListening`；`EINVAL`（对端已关闭的流 / socketpair 端，h1probe 实测）→ `NotListening`；其它错误 → `Io`。**已知缺口**：`socket()` 后未 `listen()`（无论是否 `bind()`）的流 socket 也回 `ENOTCONN`，**在 macOS 上会通过校验**（h1probe 实测）。接受的理由：内核传来的 fd 3 永远是 `listen()` 过的（`launch.rs:456` 起），出现这种 fd 意味着有人伪造了 spawn 环境；后果只是 `accept` 报错、模块退出，不会越权，也不会关掉别人的 fd（校验失败本来就不接管，这里是「接管了一个不能 accept 的 socket」）。缺口由 `cfg(target_vendor = "apple")` 的测试 `apple_gap_bound_but_not_listening_passes` **钉住**（断言它通过）：将来若 macOS/rustix 行为变化或有人改了判定，这条测试会变红，提醒同步本节。os-fd 源码 `is_listening` 的 apple 分支注释写明同样的内容（附录 B.2）。
   全部通过才 `OwnedFd::from_raw_fd(3)` 接管；任一失败直接返回错误，**不接管也不关闭**这个 fd（它可能是别人的）。
4. **接管后**：`fcntl(F_SETFD, FD_CLOEXEC)`（内核蹦床为了让 fd 3 跨 exec 存活而没设 CLOEXEC；接管后必须设上，否则模块起的子进程会继承监听 socket）；`set_nonblocking(true)`（tokio 需要）。
5. 唯一的 `unsafe` 是私有函数 `adopt` 里的 `borrow_raw` 与 `from_raw_fd` 两个块，函数上一处 `#[allow(unsafe_code)]`；crate 级 `unsafe_code = "deny"`。J-S3 断言全工作区 `#[allow(unsafe_code)]` 恰好 1 处且在 os-fd。
6. 校验逻辑的单测（判据 J-S20；scratch 在 macOS 上用真实 fd 跑过，7 条全过；Linux 分支用 `--target x86_64-unknown-linux-gnu` 过了 clippy `-D warnings`，本机不能运行）：监听中的 Unix socket 通过；socketpair 端 → `NotListening`；对端已关闭的 socketpair 端 → `NotListening`（macOS 走 `EINVAL` 映射，Linux 走 `SO_ACCEPTCONN = 0`）；`/dev/null` → `NotASocket`；TCP 监听 → `NotUnix`；只 `bind` 未 `listen` → Linux `NotListening` / macOS **通过**（缺口钉子，两条测试按 `cfg` 二选一编译）；第二次调用 → `AlreadyTaken`。测试只对测试自己持有的 fd 调私有的 `validate`，从不碰 fd 3 与进程环境；`only_one_attempt_per_process` 是这个测试二进制里**唯一**调 `take_inherited_listener` 的测试（进程级一次，§3.2）。
7. **两个平台都要在 CI 上跑**（v3，M-2）：Agent24 CI 今天只有 `ubuntu-latest`（`.github/workflows/ci.yml`），它编译并运行 Linux 分支（`SO_ACCEPTCONN` 与 `bound_but_not_listening_is_rejected`）；a3-fd 同一个 PR 在 `ci.yml` 里**新增一个 `macos-latest` job，只跑 `cargo test -p agent24-os-fd`**（装 stable、不跑全工作区，成本约一分钟），运行 apple 分支与缺口钉子。两个 job 缺一个，J-S20 就只验了一半。

**handshake token 留在 environ（H1 的第二问）——接受风险，理由与边界**：
- 不在 os-fd 里 `remove_var`：edition 2024 里 `std::env::remove_var` 是 `unsafe`，其 SAFETY 前提是「没有其它线程同时读写环境」；SDK 的 `connect()` 在 `#[tokio::main]` 的多线程运行时里执行，一个库无法证明这一点。把它塞进 os-fd 等于给唯一的 unsafe crate 加一个写不出真话的 SAFETY 注释。
- 风险为什么小：token 只用于**一次**握手——内核每次 spawn 都新铸一个（`launch.rs:256-277`，FU-44），并且回调端点在接受第一条连接后立即关闭监听、删除路径（`endpoint.rs:11-23`、`:547-551`「Exactly one connection is served」）。所以握手完成之后，任何读到这个 token 的子进程都已无处可连。窗口只剩「spawn 到握手之间」模块自己起的子进程，以及同 uid 读 `/proc/<pid>/environ`（同 uid 本来就能 ptrace，不构成新的越权）。
- 给模块的缓解：`SPAWN_ENV_VARS` 列出四个变量名，SDK 文档要求「要起子进程的模块对子进程 `env_remove` 这四个」；监听 fd 已被设为 CLOEXEC，子进程继承不到；即使子进程读到 `A24_LISTEN_FD=3` 再调 `take_inherited_listener`，它的 fd 3 要么不存在要么是别的东西，会在校验处失败。

### 4.4 manifest 与 `FiredBody`

- `manifest::ManifestFacts { name, route_namespace, kernel_capabilities }` + `facts_from_yaml(&str) -> Result<ManifestFacts, String>`：**宽松**——不带 `deny_unknown_fields`，只读三个字段、忽略其余（L4）；`kernel_capabilities` 缺省为空。不是校验（内核在安装时校验），只是取数。实现用 proto 已有的 `agent24-domain` → `serde_yaml`。
- `manifest::manifest_digest(bytes: &[u8]) -> String`：**从 `agent24-os-packages/src/discovery.rs:46-57` 原样挪入**（M4）；os-packages 改为 `pub use agent24_os_proto::manifest::manifest_digest;`，它自己的调用点（`discovery.rs:219`）与测试（`:324-334`、`:656`）不改；os-packages 新增对 proto 的普通依赖（方向见 §2.1，无环：proto 不依赖 os-packages）。proto 已依赖 `sha2 0.10`（审批 token 哈希用），不新增依赖。模块侧握手与内核侧 discovery 从此是同一个函数。
- `kernel_call::FiredBody`（拥有型）+ `FireTrigger`，agentd `scheduler_deliver.rs` 改用它（§2.5）。v3 单独成片 c1a（从 main 开、不依赖任何 SDK 片），fired 提取器在 c1b；不放 a1。

### 4.5 `module::testing`（`test-util` feature，H4）

Sin90 的 `clients/test_support.rs`（`135ddb7`）导出的 API 与签名，proto 逐一照抄（返回的连接类型从 `Arc<KernelClients>` 换成 `Arc<Connection>`，两者都被各客户端的 `new(&…)` 接受）：

| Sin90 `test_support.rs` | proto `module::testing` |
|---|---|
| `pub(crate) struct FakePeer` | `pub struct FakePeer`（内部持 socket 两半，私有） |
| `async fn fake_kernel(offer: Vec<String>) -> (Arc<KernelClients>, FakePeer)` | `pub async fn fake_kernel(offer: Vec<String>) -> (Arc<Connection>, FakePeer)` |
| `async fn read_request(peer: &mut FakePeer) -> Value` | 同签名 |
| `async fn respond(peer: &mut FakePeer, req: &Value, result: Value)` | 同签名 |
| `async fn respond_error(peer: &mut FakePeer, req: &Value, code: i64, kind: &str, message: &str)`（`kind` 为空 = 无 `data`） | 同签名、同语义 |
| `async fn respond_error_with_data(peer: &mut FakePeer, req: &Value, code: i64, message: &str, data: Value)` | 同签名 |
| `fn noop_hook() -> FatalHook`（私有） | `pub fn noop_hook() -> FatalHook` |
| — | `pub fn recording_hook() -> (FatalHook, Arc<AtomicUsize>)`（M3） |
| — | `pub struct FakeEndpoint`：`bind(dir: &Path) -> (ModuleEnv, FakeEndpoint)`（用 `ModuleEnv::from_vars` 造指向临时 socket 的环境）+ `accept_initialize(self, result: Value) -> (Value /* initialize params */, FakePeer)`，J-S11/J-S12 用 |

于是 Sin90 迁移时 `clients/test_support.rs` 变成一行 shim（`#[cfg(test)] pub(crate) use agent24_os_sdk::testing::{fake_kernel, read_request, respond, respond_error, respond_error_with_data, FakePeer};`），各测试模块里的 `use crate::adapter_agent24::clients::test_support::{…}` 一行不改。

---

## 5. Sin90 迁移到 SDK（ME4-5.2.1 / Sin90 TS.1.0 + TS.1.1）

### 5.1 步骤

**前置 PR（Sin90 TS.1.0，迁移前合）：线协议金样，出站 + 入站两个方向。** 在迁移前的 Sin90 上，用现有的假内核夹具（`clients/test_support.rs`）录 golden 文件；比较时忽略 JSON-RPC `id` 值与对象键序。

*出站金样*（Sin90 发出去的 `(method, params)` 序列）：
- reconciler：upsert（cron+tz）、delete、`list`、`memory.remember` 前的 recall 翻页 + remember（含 `body.dedup_key`）；
- model：classify/summarize/propose 各一次 `complete`；
- events：一个 Routine 变更触发的 `emit`；
- 握手：`initialize` 的 params（去掉 `auth_token`）。

*入站金样*（H4 新增：内核回什么 → Sin90 落成什么状态）：
- **每个错误 kind 回放**：18 个 `ErrorKind` 各一条，外加 `-32602`、`timeout`+`retryable:false`、`unavailable` 的 4 个 cause × `retryable` 真/假、缺 `cause`、缺 `retryable`、`retryable` 非布尔、cause 不在闭集——逐条喂给 reconciler 的 `apply_one`，对三种 outbox 行（`scheduler.upsert`/`scheduler.delete`/`memory.remember`）各录一行 `(row.status, row.attempts, row.other_bucket_attempts, 退避档位, 泵动作 ∈ {继续, 停本批, 停泵})`；同一批错误喂给 `ModelPort::complete`，录 `ModelFailure` 变体。这钉住了 `RpcErrorInfo.raw → data` 之后 `unavailable` 的 `cause`/`retryable` 仍从同一处取到。
- **Usage 宽度**：`usage.prompt_tokens`/`completion_tokens` 取 `u32` 范围内的值 → `ModelReply` 相同；取 `u32::MAX + 1` → 迁移前是「响应形状错」落 `ModelFailure::Other`，迁移后 SDK 按 `u64` 解析成功、Sin90 自己 `u32::try_from` 失败也落 `ModelFailure::Other`——金样只比 `ModelFailure` 变体，二者相同。
- **宽松解析**：`initialize` 响应与每个方法的成功响应各加一个未知字段（顶层与嵌套各一处）→ 迁移前后都成功且解析值相同。
- **recall 预查**：Found（第 3 页）/ NotFound / Inconclusive（10 页仍有 cursor）三种脚本 → 录出站调用序列与 outbox 行结果（Inconclusive 行的 `last_error` 文案一并录，迁移后逐字相同）。

**迁移 PR（TS.1.1）**，按文件：

| Sin90 文件 | 动作 |
|---|---|
| `Cargo.toml` | 加 `agent24-os-sdk = { git = "...Agent24", tag = "agent24-os-sdk-v0.1.0" }`；dev 依赖加同一个 SDK 的 `features = ["test-util"]`（不直接依赖 proto，L8）。`sha2`/`hex` 若只剩 `manifest_digest` 在用则删 |
| `clippy.toml`（新增） | Unix socket / fd 子集（§5.2 第 6 条） |
| `adapter_agent24/frame.rs` | **删** |
| `adapter_agent24/transport.rs` | **删**（测试已移入 proto，J-S8） |
| `adapter_agent24/clients/{error,memory,approval,mod}.rs` | 改为转出 SDK：`pub use agent24_os_sdk::…`（`clients` 模块保留为转出层，减少调用点改动） |
| `adapter_agent24/clients/scheduler.rs` | 转出 `ScheduleSpec as ModuleSpec`、`ScheduleState as ModuleScheduleState` 等旧名；另保留一个 newtype `SchedulerClient(agent24_os_sdk::SchedulerClient)`，其 `upsert(key, &ModuleSpec, enabled, label, request_id)` **保持 Sin90 今天的五个位置参数**、内部组 `UpsertRequest`——`reconciler.rs` 测试模块直接这样调它（`fake_kernel_rejects_an_uppercase_key`，`reconciler.rs:1437-1450`），不保留就做不到测试模块零改动。`request_id` 参数类型改为 `Option<&RequestId>`，测试里的字面量 `None` 两种类型都接受，文本不变（约 50 行） |
| `adapter_agent24/clients/test_support.rs` | **一行 shim**：`#[cfg(test)] pub(crate) use agent24_os_sdk::testing::{fake_kernel, read_request, respond, respond_error, respond_error_with_data, FakePeer};`（§4.5） |
| `adapter_agent24/clients/model.rs` | 保留一个 newtype `ModelClient(agent24_os_sdk::ModelClient)`：`new(&Arc<Connection>)` + 固有方法 `complete(&ModelRequest) -> Result<ModelReply, ClientError>`（`ModelRequest → CompleteRequest`、`CompleteResult → ModelReply`（含 `u64 → u32`）在这里写），`ModelPort`/`ModelCaller` 实现与 `ClientError → ModelFailure` 映射原样；`UnavailableCause` 在这里从 SDK 的转成 `crate::ai` 的。newtype 让该文件测试模块里的 `ModelClient::new(&clients)` / `client.complete(&req)` 一字不改；测试引用的 `MODEL_CALL_TIMEOUT`（`model.rs:36`）保留为 `pub(crate) const MODEL_CALL_TIMEOUT: Duration = agent24_os_sdk::clients::MODEL_RESPONSE_TIMEOUT;`。**测试模块唯一要改的一行**（v3，H-1）：`use crate::ai::UnavailableCause;`（`model.rs:223`）→ `use agent24_os_sdk::UnavailableCause;`——该测试模块用它构造的是 `ClientError::Unavailable { cause, .. }`（`:471`、`:502`），而迁移后 `ClientError` 是 SDK 的类型、`cause` 是 SDK 的 `UnavailableCause`，不改这一行就类型不匹配；这一行写进期望补丁（§5.2 第 3 条） |
| `adapter_agent24/mod.rs` | 删 `SpawnEnv`/`manifest_digest`/`connect_and_initialize`/`KernelClients`/`listener_from_fd`/`INITIALIZE_CAPABILITIES` 及其测试；留 `wire_kernel_clients`（改吃 `&Module`），`KernelEventSink` 改为包一层 SDK `EventSink` 实现 Sin90 的 `http::EventSink` trait |
| `adapter_agent24/reconciler.rs` | 非测试部分：改类型名与三处调用签名（`upsert` 用 `UpsertRequest`、`request_id` 传 `None`）；**删 `memory_recall_finds_dedup_key`/`RecallCheck` 与 `RECALL_PRECHECK_MAX_PAGES`/`RECALL_PRECHECK_PAGE_SIZE` 两个常量的定义**；`remember_review_summary` 改调 `memory.remember_once("review.summary", &desired.dedup_key, body, None)`，`Found{id}`/`Created{id,..}` → `Ok(id)`，`Inconclusive` → 原文案的 `Err(ClientError::Other(..))`（H3）；**分类 match 原样保留**。测试模块经 `use super::*` 仍要用 `RECALL_PRECHECK_MAX_PAGES`（`reconciler.rs:3678`，唯一一处；`PAGE_SIZE` 测试不用），所以非测试部分补一行 **`#[cfg(test)] use agent24_os_sdk::clients::RECALL_PRECHECK_MAX_PAGES;`**——必须带 `#[cfg(test)]`：删掉预查后非测试代码不再用这个常量，不带的话非测试构建报 `unused_imports`，CI 的 `clippy -D warnings` 红（v3，L3）。测试模块（`#[cfg(test)] mod tests` 起到文件尾）与迁移前**逐字相同**（期望补丁里 reconciler 部分为空，§5.2 第 3 条）；其中两处文档注释仍以 intra-doc 链接提到已删的 `memory_recall_finds_dedup_key`/`RecallCheck::Inconclusive`（`:3574`、`:3654` 附近），`cfg(test)` 代码不进 rustdoc，不产生告警，保留不改 |
| `adapter_agent24/kernel_roundtrip.rs` | 头读取改用 `RequestContext`（test-hooks 路由）；它构造 `RequestId` 的地方（test-hooks 专用）用 `RequestId::for_test`——test-hooks 是 Sin90 的 feature，Sin90 在 `test-hooks = ["agent24-os-sdk/test-util"]` 里转发 |
| `main.rs` | `SpawnEnv + KernelClients::handshake + listener_from_fd + nest("/api/v1/sin90")` 换成 `Module::builder(sin90::ai::MANIFEST_YAML).on_connection_lost(同一个 exit(70) 钩子).connect()` + `module.serve(router)`；`const MANIFEST: &[u8]` 删除；test-hooks 的两个路由照旧 merge |
| `http/mod.rs` 的 fired handler | **不动**（`http` 不许依赖 Agent24 类型，`lib.rs:3`）。可选：把 `"x-a24-fire-id"` 字面量换成 SDK 转出的 proto 常量——但这会让 `http` 依赖 SDK，违反 Sin90 自己的分层，**不做** |

### 5.2 零行为变化的验收办法

1. **真实挂载黑盒不变全绿**：`AGENT24_CHECKOUT=../Agent24 cargo test --test agent24_mount_blackbox -- --ignored --test-threads=1`，先 `-- --list --ignored` 断言恰好 5 条（`sin90_mounts_under_a_real_agent24_daemon`、`kernel_clients_roundtrip`、`routine_m3_real_mount_acceptance`、`t441_finalized_review_summary_is_recallable_from_kernel_memory`、`t551_ai_v1_m5_real_mount_acceptance`，`tests/agent24_mount_blackbox.rs:908/1175/1419/1861/2158`），跑之前 `../Agent24` ff 到含 SDK tag 的 main。
2. **线协议金样逐条相等**：TS.1.0 录的出站与入站 golden 在迁移后重跑逐条相等（迁移后用 SDK `test-util` 假内核）。**有意差异清单**（PR body 单列，不算回归）：① `initialize.params.protocol_versions.max` 1000 → 1（§8 Q7）；② 握手响应缺 `protocol_version` 或 `offer` 时由「接受」变为 `ConnectError::Protocol`（§4.1，真内核不会发这种响应）。变异：在迁移后的 Sin90 里把 `upsert` 的 `label` 改成发 `null` → 出站 golden 红；把 SDK 映射里 `"quota_exceeded"` 分支删掉 → 入站 golden 的对应行红。
3. **测试模块只改一行，且与已提交的期望补丁逐字相等**（H4；v3 按复审 H-1 修正）：v2 写的「两个测试模块 diff 0 行」做不到——`clients/model.rs` 的测试模块 `use crate::ai::UnavailableCause;`（`model.rs:223`）并用它构造 `ClientError::Unavailable { cause, .. }`，迁移后 `cause` 是 SDK 的类型，这一行必须改（§5.1 model 行）；v2 的脚本在一次正确的迁移上也会退出 1（A.8 实测）。v3 的判据：TS.1.1 同一个 PR 提交 `scripts/ts11-test-modules.expected.patch`，内容恰为
   ```diff
   --- a/src/adapter_agent24/clients/model.rs (tests)
   +++ b/src/adapter_agent24/clients/model.rs (tests)
   @@ -4,7 +4,7 @@
        use crate::adapter_agent24::clients::test_support::{
            fake_kernel, read_request, respond, respond_error_with_data,
        };
   -    use crate::ai::UnavailableCause;
   +    use agent24_os_sdk::UnavailableCause;
        use serde_json::{json, Map};
    
        fn sample_request() -> ModelRequest {
   ```
   （`reconciler.rs` 部分为空），由 `scripts/check-test-modules.sh <迁移前 base>` 机械检查两件事：① 期望补丁里的 `+`/`-` 行**恰好**是上面那一对（补丁本身不许夹带别的改动）；② 两个文件的「测试模块」区段、迁移前 vs 迁移后的 `diff -u`（固定 label）与期望补丁**逐字节相等**。**`tests_of` 的锚点（v4，H-A）**：不是「从第一个 `#[cfg(test)]` 行开始截到文件尾」（v2/v3 的写法），而是只认「`#[cfg(test)]` 这一行紧接着下一行恰为 `mod tests {`」这个锚点，且要求每个文件**恰好命中一次**，否则整体失败（`exit 3`，不静默通过）：
   ```sh
   tests_of() { awk 'p{print;next} prev=="#[cfg(test)]" && $0=="mod tests {" {n++; p=1; print prev; print} {prev=$0} END{if(n!=1) exit 3}'; }
   ```
   改锚点的原因：迁移会往 `reconciler.rs` 的**非测试部分**新增一行 `#[cfg(test)]` 门控的 `use`（§5.1 reconciler 行：`#[cfg(test)] use agent24_os_sdk::clients::RECALL_PRECHECK_MAX_PAGES;`）——这是一个合法的、单独的 `#[cfg(test)]` 行，但它后面不是 `mod tests {`。v2/v3 的「见第一个 `#[cfg(test)]` 就开始截」在这种真实形状下会截到这一诱饵行，把本该属于非测试部分的一整段错当成「测试模块」去比对，产生假阳性差异。v4 的锚点只在「`#[cfg(test)]` + 下一行恰为 `mod tests {`」时才算数，天然跳过这个诱饵。脚本全文见 A.8，已在 `scratchpad/h1mig/`（Sin90 `135ddb7` 的这两个文件）实测三种情形：① 真实形状迁移（`model.rs` 测试模块改那一行 `use`，`reconciler.rs` 非测试部分按上面加两行诱饵、`rustfmt` 之后）→ **rc=0**；② 在真实迁移基础上再改动测试模块一处（如 `reconciler.rs` 测试模块里再改一行）→ 与期望补丁不符 → **rc=1**；③ 把某个文件的 `#[cfg(test)]` 改成 `#[cfg(all(test))]`（锚点不再逐字匹配）→ **rc=3**。另有 4 条正对照全红（同 v3：reconciler 测试模块改一处、model 测试模块改一处断言、期望补丁多夹带一行、`#[cfg(test)]` 标记被改掉，最后一条现在归为 rc=3 的锚点缺失，其余归 rc=1 的内容不符）。两个测试模块覆盖了 recall 预查翻页（`reconciler.rs:3133` 起）、stateful 假内核调度（`:1239` 起）、model 的 125s 超时与 `unavailable` 映射（`model.rs:347-540`），在只动一行 `use` 的前提下全绿，比新写的等价测试更能说明「行为没变」。**这个脚本只是 TS.1.1 那一个 PR 的一次性验收检查（v4，L-f）**：它假设了 Sin90 迁移前后恰好是这两个文件、恰好是这一行 `use` 的改动，不是一个通用的「测试模块没变」框架，迁移这个 PR 合并之后即可删除，不进 Sin90 的常驻 CI。
4. **常量同值**：一条 Sin90 单测断言 `EventSinkConfig::default()` 等于 `256/4/32/5s`、`MODEL_RESPONSE_TIMEOUT == 125s`、`agent24_os_sdk::clients::{RECALL_PRECHECK_MAX_PAGES, RECALL_PRECHECK_PAGE_SIZE, DEDUP_KEY_FIELD} == (10, 50, "dedup_key")`；连接死时退出码仍是 70（`main.rs` 的钩子原样传入）。
5. **HTTP 面不变**：Sin90 `cargo test` 全绿（包含 `http/tests.rs` 的 fired 400 用例，因为 fired handler 没动）。
6. **结构判据**：Sin90 仓库根新增 `clippy.toml`，内容是 SDK 名单里**Unix socket / fd 的子集**（`tokio::net::{UnixStream,UnixListener,UnixSocket,UnixDatagram}`、`std::os::unix::net::{UnixStream,UnixListener,UnixDatagram}`、`std::os::fd::OwnedFd`、`FromRawFd::from_raw_fd`），CI 现有的三条 `cargo clippy --all-targets -- -D warnings` 就是判据。**不照搬 SDK 的全表**（M7 要求「同一份」，这里有理由地收窄）：TCP 与 JSON 两部分在 Sin90 里有合法用途——standalone 模式本来就 `tokio::net::TcpListener::bind(("127.0.0.1", port))`（`main.rs:109`），黑盒测试用 `std::net::TcpStream`（`tests/agent24_mount_blackbox.rs:23,172,625-675`），领域代码到处 `serde_json::from_str` 解析库里的 JSON 列与模型输出（`store/repo.rs`、`core/types.rs`、`ai/*` 约 30 处）；它们与「经 SDK 连 Agent24」无关。**正对照已实测**（附录 A.6）：把这份子集放进迁移前的 Sin90 `135ddb7` 树跑 `cargo clippy --all-targets -- -D warnings` → 退出 101，28 条 `use of a disallowed`，**全部落在迁移会删/改掉的三个文件**（`adapter_agent24/mod.rs` 14、`transport.rs` 10、`clients/test_support.rs` 4），树里其它文件 0 条——所以迁移后变绿即证明 Sin90 不再自己碰 socket/fd。
7. **全局前置**：`cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`，外加 `--features test-hooks` 与 `--features remote-allowed-manifest` 两个变体（Sin90 CI 已有）。
8. **净删代码**：`git diff --stat origin/main -- src/adapter_agent24` 删除行 > 新增行（TS.1.1 原验收）。按 §1.2 粗估：删 `frame.rs`（160）、`transport.rs`（1378）、`clients/` 下 error/memory/scheduler/approval/test_support 的实现与测试（约 2100，测试随代码进了 SDK/proto）、`mod.rs` 约 700、`model.rs` 的 wire 部分约 250、`reconciler.rs` 的预查约 90，新增约 250 行（shim、`SchedulerClient`/`ModelClient` 两个 newtype、类型转换）。Sin90 冷编译时间变化记入 PR body（FU-86）。

---

## 6. 判据清单（J-S*）

规则同 PLAN §一 第 5 条：`cargo test <过滤>` 一律先 `-- --list` 断言匹配数 > 0；每条带正对照或变异。「scratch」列标明该判据的命令/测试已在 scratch 上实测过的部分（附录 A）；没标的是只能在真实实现上跑的（依赖真 proto mux 或 agentd）。

**判据分级（v4，原型定位追加，用户 2026-09-26 拍板）**：下表判据全部保留、结论不变，按「v0.1.0 这一轮是不是必须（gate）」分两档，不改变判据本身的内容，只影响它是否作为本轮强制 CI 关口：
- **核心判据**（v0.1.0 冻结、Sin90 迁移、开 PR 都必须过）：能编译（`cargo check`/`cargo test`）；J-S4/J-S5/J-S6（错误映射、前缀门控、缺省不发）；**J-S7**（五个客户端对内核真实 handler 的对等测试）；J-S8/J-S9（transport 语义、超时关系）；J-S11/J-S12/J-S19（握手正确）；J-S13（挂载冒烟）；**J-S16**（Sin90 迁移后真实挂载黑盒全绿）；J-S18（`remember_once` 预查算法）；J-S2 的依赖集合断言本身（SDK 只经 `agent24-os-proto`，不含它的正对照脚本）。
- **原型阶段可选**（已在 scratch 上写出并验证过，记录在案，不阻塞本轮，冻结后按需升级为常驻 CI）：J-S1 的 `positive-control` 逐项正对照三步机械断言；**J-S1b**（proto/os-fd 后门检查 Python 脚本）；J-S3 的②③④机械正对照部分（①`lints.workspace`断言仍是核心的一部分，因为它保证只有 os-fd 能 unsafe）；**J-S20** 里的 `macos-latest` 专用 CI job（os-fd 本身编译、Linux 分支跑绿仍是核心，macOS 单独出 job 可选）；J-S10(b)（内核真实字节回放）、J-S14 的发布机械脚本、J-S15（wire 文档抽取数断言）；J-S17（Cos72 专属，Cos72 还没有代码，随 5.3.x 落地时再决定要不要）。
- 下表「PR」列沿用 v3 时期的细粒度切法编号（a1/a2-core/…/c2/5.2.1/5.3.x/5.4.1），只用来标注该判据出自设计推演的哪一处；v4 起 §7 的实际开发切法已压缩为 4 个大片，细粒度编号 → 新切片的对应关系见 §7 开头的映射表，不在这里逐行重复替换。

| # | 判据 | PR | 怎么验（机械） | 正对照 / 变异 | scratch |
|---|---|---|---|---|---|
| J-S1 | SDK 不持有 socket/fd、不自己把字节解析成 JSON | a3-skel | `cargo +1.98.0 clippy -p agent24-os-sdk --all-targets -- -D warnings` 绿，外加 `cargo +1.98.0 clippy -p agent24-os-sdk --lib --features test-util -- -D warnings` 绿（v3，L1：`--all-targets` 已经经 dev 依赖打开了 `test-util`，第二次只需要确认**库本身**在开 `test-util` 时也干净——`testing` 转出、`with_env`、`RequestId::for_test` 都在库里） | `--features positive-control`（`positive_control.rs` 名单每项一行）必须失败，且：错误条数 == 去重种类数 == `grep -c '{ path = ' clippy.toml`（今天 26）。CI 三步：`! cargo clippy … --features positive-control > log`、`grep -c 'use of a disallowed' log`、`grep -o "use of a disallowed [a-z]* \`[^\`]*\`" log \| sort -u \| wc -l`。名单是绊线不是沙箱，已知残余 b13/b14 不防（§2.2） | ✅ 26/26/26，退出 101（真 clippy 0.1.98）；绕过实验 b1、b3–b12 全抓，b2/b15 归 J-S1b，b13/b14 记录不防（A.3） |
| J-S1b | proto / os-fd 不给 SDK 留后门（v3 按 H-2 重写：clippy 看不穿跨 crate 别名） | a3-fd（proto `module*` 与 os-fd 在这一片之后才都存在） | `python3 rust/scripts/check-proto-exports.py rust/crates/agent24-os-proto rust/crates/agent24-os-fd` 退出 0。规则（只看去掉 `//` 注释后的代码）：① proto 与 os-fd 全部源码无 `#[macro_export]`；② proto 的 `src/module*`（`module.rs` 与 `module/` 目录）与 os-fd **不许有任何 `pub type`**；③ 同范围内 `pub use` 只能以 `crate::`/`self::`/`super::` 开头（只转出 crate 内部项；把私有 `use` 再 `pub use` 出去是编译错误 E0364，不需要脚本管）；④ proto 全部源码（含内核侧）不许有点名 socket/fd 类型的 `pub type`/`pub use`。用 Python 而不是 `rg`：v3 实测 `rg` 不在 `sh` 的 PATH 里时，v2 的 `! rg …` 写法会**静默通过**；脚本目录不存在时直接失败。**依赖 `cargo fmt --check` 兜底（v4，L-c）**：脚本按行做正则/字符串匹配，隐含假设 `pub type`、`#[macro_export]`、`pub use` 各自单独占一行（rustfmt 的标准输出）；CI 里本判据必须排在 `cargo fmt --check` 之后（或至少同一批一起跑、fmt 不过就整体红），否则可以把这些声明故意拆成多行绕开逐行正则——J-S1b 不是重新实现 rustfmt，两者是搭配关系 | 在 scratch proto `module.rs` 加两跳别名（`type Inner = tokio::net::UnixStream; pub type Sock2 = Inner;`）→ 红；加 `pub use tokio::net::UnixStream as S;` → 红；在 `lib.rs` 加 `#[macro_export]` 宏 → 红；在内核侧加 `pub type K = tokio::net::UnixStream;` → 红；在 os-fd 加 `pub type L = UnixListener;` → 红；目录不存在 → 红。负对照：`pub use crate::initialize::Offer as ReOffer;` → 绿 | ✅ scratch（proto `module.rs` + os-fd）与真实 proto 今天均绿；6 条正对照全红、1 条负对照绿（A.4） |
| J-S2 | SDK 普通依赖恰为允许集合 | a3-skel | `cargo metadata --format-version 1 --no-deps \| jq -r '.packages[] \| select(.name=="agent24-os-sdk") \| .dependencies[] \| select(.kind==null) \| .name' \| sort \| tr '\n' ' '` 等于 `agent24-os-proto axum serde serde_json thiserror tokio tracing `（v1 的 `cargo tree … \| grep '^agent24-'` 会把 SDK 自己那一行也打出来，恒为两行，M1） | 在 SDK `Cargo.toml` 临时加 `agent24-domain` → 输出多一项 → 红 | ✅ 正/负都跑过（A.5） |
| J-S3 | 全工作区只有 os-fd 能有 `unsafe`，且只有一处 | a3-skel（脚本 + SDK 正对照；a3-fd 加入 os-fd 后同一脚本自动要求恰 1 处） | `python3 rust/scripts/check-lints.py`（在 `rust/` 下跑）：① 对 `cargo metadata --no-deps` 的每个工作区成员用 `tomllib` 读 `Cargo.toml`，断言 `lints.workspace == true`，唯一例外 `agent24-os-fd` 必须**不**继承且 `lints.rust.unsafe_code == "deny"`；② **os-fd 重述的 lint 表去掉 `rust.unsafe_code` 后与 `[workspace.lints]` 去掉同一项后完全相等**（v3，L2：工作区以后加一条 lint，os-fd 忘了抄就红）；③ 扫所有成员的 `.rs`（去掉 `//` 注释），正则 `\b(allow\|expect)\s*\([^)]*\bunsafe_code\b`——覆盖 `#[allow(unsafe_code)]`、`#![allow(unsafe_code)]`、多项 `#[allow(dead_code, unsafe_code)]`、`#[expect(unsafe_code)]`、`cfg_attr(…, allow(unsafe_code))`（v3，L2）——命中恰 1 处且在 os-fd（os-fd 尚未加入时恰 0 处）。**依赖 `cargo fmt --check` 兜底（v4，L-c）**：③的正则同样假设属性宏与其参数不会被人为拆成异常的多行/多个 `#[]` 块来打断匹配；CI 里同样要求 `cargo fmt --check` 先跑或与本判据同批失败，J-S3 的逐行正则不是完整的 Rust 属性解析器 | ① 删掉 proto 的 `[lints] workspace = true` → 红；② os-fd 的 `unwrap_used` 改 `warn` → 红，os-fd 多加一条 `dbg_macro` → 红；③ 在 proto 加 `#![allow(unsafe_code)]` / `#[allow(dead_code, unsafe_code)]` / `#[expect(unsafe_code)]` / `#[cfg_attr(unix, allow(unsafe_code))]` → 各自红；④ `cargo check -p agent24-os-sdk --features positive-control-unsafe`（内含 `unsafe {}`）必须编译失败（`usage of an unsafe block`） | ✅ scratch 绿（1 处）、**真实 Agent24 工作区**（`bb3505a`）绿（0 处）；上列 7 条正对照全红；unsafe 正对照编译失败（A.5） |
| J-S4 | 错误映射覆盖内核闭集 | b1a | 测试遍历 `agent24_os_proto::rpc::ErrorKind::ALL`（18 个），每个 kind 映射到文档表里写死的变体；`-32602`、`timeout+retryable:false`、`unavailable` 缺 cause/缺 retryable/非布尔 retryable 各一例 | 删掉 `"quota_exceeded"` 分支 → 该例红；proto 新增第 19 个 kind 而 SDK 表没更新 → 遍历测试红 | |
| J-S5 | 前缀门控 | b2a–b3b | 5 个客户端 × {`Offer` 含前缀 → `Some`，不含 → `None`} 共 10 例，用 `testing::fake_kernel` | 把某个 `new` 改成无条件 `Some` → 对应例红 | |
| J-S6 | 缺省不发 | b2a–b3b | 每个带可选字段的方法，`None` 时假内核收到的 params **没有**该键 | 把 `set_opt` 改成写 `Value::Null` → 红 | |
| J-S7 | 与内核 wire 对等（M6：落到各回调模块自己的单测） | b2a–b3b（每个客户端随自己的 PR） | agentd dev 依赖 SDK（`features = ["test-util"]`）。在 `events_emit.rs`、`memory_callback.rs`、`approval_callback.rs`、`scheduler_callback.rs`、`model_callback.rs` 各自的 `#[cfg(test)] mod tests` 里加 `sdk_wire_parity_*`：SDK 客户端跑在 `testing::fake_kernel` 上，假内核把收到的 params **原样**交给该模块现有的 handler 夹具 `h.call(params)`（`events_emit.rs:518-528` 的 `handler(..)`、`memory_callback.rs:348`/`:657` 的 `RememberHandler`/`RecallHandler`、`approval_callback.rs:372`/`:448` 的 `submit_handler`、`scheduler_callback.rs:1256`/`:1286` 的 `fx.upsert`/`fx.delete`、`model_callback.rs:1402` 的 `handler(..)`），再把 handler 的返回值作为响应回给 SDK，由 SDK 自己的结果类型解析。每个方法两次调用：**可选字段全填 `Some`**（`request_id` 用 `RequestId::for_test`，夹具能 admit 该 id 的就 admit——`approval_callback.rs:482` 的 `generation_with_good_params_admitted` 即此——使调用成功；不能的，断言错误不是 `-32602`，即 params 已被 `deny_unknown_fields` 类型接受）；**可选字段全 `None`** → 必须成功且 SDK 解析出结果 | 把 SDK `UpsertRequest` 的 `label` 键改名 → `fx.upsert` 返回 `-32602` → 红；把 SDK `Remembered` 的 `id` 改名 → 结果解析失败 → 红 | |
| J-S8 | transport 语义 | a2-core、a2-cancel | Sin90 `transport.rs:686-1378` 的 16 条测试移植到 proto（第 65 个在途 `Busy`、drop 发**逐字节**正确的 cancel 帧、正常完成不发 cancel、迟到响应被丢且连接继续可用、断连 → 在途 `ConnectionLost` 且钩子恰好一次、关闭后调用 `NotSent`、超大帧在写任务前被拒、恰好 1 MiB 可过、响应超时 → `Timeout`+cancel、写超时 → 钩子一次、非 JSON 帧/超大入帧触发钩子、`slot_wait` 成功与超时）；前 8 条随 a2-core，后 8 条随 a2-cancel | 各测试原有的变异说明随迁移保留；新增：把 `CallGuard::drop` 里的 cancel 删掉 → cancel 帧测试红 | |
| J-S9 | 超时关系 | a2-cancel（前半）、b3b（后半） | proto 编译期断言 `DEFAULT_RESPONSE_TIMEOUT > rpc::CALL_TIMEOUT`；agentd 测试断言 `agent24_os_sdk::clients::MODEL_RESPONSE_TIMEOUT > model_callback::MODEL_CALL_TIMEOUT` | 把 35 改成 30 → 编译失败；把 125 改成 120 → agentd 测试红 | ✅ 前半的 `const _: () = assert!(…)` 已写在 scratch proto 并编译 |
| J-S10 | fired 提取器 | c1b（(b) 用到的 `FiredBody` 在 c1a 挪进 proto） | (a) SDK 测试：合法请求 200；缺 `X-A24-Fire-Id` 400；body 多一个字段 400；时间戳非定宽 400；`Result<FiredDelivery, FiredRejection>` 可自定义形状。(b) **内核真实发出的字节**被 SDK 接受（M7）：`scheduler_deliver.rs` 的 `deliver_on_posts_the_declared_namespace_path_and_the_fired_body_shape`（`:262-388`）在已有断言之后，把 mock 上游收到的 `head`（`x-a24-fire-id`/`x-a24-schedule-key` 头）与 `body` 字节原样组成 `axum::http::Request`，喂给 `FiredDelivery::from_request`，断言 `Ok` 且 `fire_id`/`schedule_key`/`body.key`/`body.trigger` 等于本次投递的值 | (a) 去掉 `deny_unknown_fields` → 多字段例红；(b) 把内核 `fmt_iso` 改成带毫秒、或把 `FiredBody` 某字段改名 → 该测试红（v1 自审第 5 条的耦合由它钉住） | ✅ (a) 4 条；(b) 的 SDK 半边：按 agentd 私有 `FiredBody<'a>` 逐字段复刻的序列化结果被提取器接受（`kernel_shaped_fired_body_is_accepted`） |
| J-S11 | 握手内容来自 manifest，协议范围是 `{1,1}` | **a3-module**（M3：v1 放在 a1，但它测的是 `Module::connect`，a3 才有） | `testing::FakeEndpoint::bind(tmp)` 得到 `(env, ep)`；`Module::builder(manifest).with_env(env, recording_hook().0).connect()` 与 `ep.accept_initialize(reply)` 并发；断言捕获的 params：`module == manifest.name`、`capabilities == manifest.kernel_capabilities`、`protocol_versions == {"min":1,"max":1}`（L1）、`manifest_digest == manifest_digest(manifest 字节)` | 在 SDK 里硬编码 `capabilities` → 用一份能力不同的 manifest（运行时拼的 `String`，§3.2）跑 → 红；把 `PROTOCOL_MAX` 改回 1000 → 红 | ✅ 形状编译通过（`js11_shape`，scratch 的 proto 是桩，不运行） |
| J-S12 | 握手后残字节 | a1 | proto 内部测试：本地 `UnixListener` 充当内核，在 `initialize` 响应后同一次写入里多塞一行 → `connect_from_env` 返回 `ConnectError::Protocol` | 删掉该检查 → 红 | |
| J-S13 | example 挂载冒烟 | c2 | `rust/apps/agent24d/tests/me4_sdk_minimal_blackbox.rs`：构建 `examples/minimal`、`os install`、`os list` 为 `mounted`、经代理 GET `/api/v1/minimal/hello` 得 200 且 `bound: true`、事件流看到 `hello.seen`、一次 `run_now` 后看到 `minimal.fired`；`--list` 断言 1 条 | 把 example 的 `route_namespace` 改错 → 代理 404 → 红 | |
| J-S14 | 发布 | c2 合并**之后**的发布步骤（v3，L5：tag 只能打在已合并的 main 提交上，不是 c2 PR 里能跑的判据） | c2 PR 内：`CHANGELOG.md` 有 `0.1.0` 小节且含「最低内核版本」一行（L10）。c2 合并后：在 main 的合并提交上打 `agent24-os-sdk-v0.1.0` 并推送，`git ls-remote --tags origin agent24-os-sdk-v0.1.0` 非空且指向该提交；探针 `4c SDK` 为 ●；tasks.md 的 c2 行在这两步做完后才标 DONE | — | |
| J-S15 | SDK ↔ wire 文档对齐（ME4-5.4.1 执行） | 5.4.1 | 抽取：`rg -o --no-filename '"(/?_a24/[a-z_/]+\|x-a24-[a-z-]+\|\$/[A-Za-z]+\|A24_[A-Z_]+)"' rust/crates/agent24-os-sdk/src rust/crates/agent24-os-proto/src/module* \| sort -u`；**先断言抽取数 ≥ 26**（M7 的正对照：11 个方法名 + 5 个前缀 + `/_a24/scheduler/fired` + `$/cancelRequest` + 4 个头 + 4 个环境变量），再逐个在 `WIRE-OOP-MODULE.md` 里 `grep -F` 命中 | 抽取数：把某个方法名 const 改成 `concat!(…)` 拼接 → 数量降到 25 → 红；对齐：在 SDK 里新增一个未入文档的方法名 → 红 | ✅ scratch 抽出 26；`concat!` 变异降为 25（A.7） |
| J-S16 | Sin90 零行为变化 | 5.2.1 | §5.2 的 1–8 条 | 见 §5.2 | ✅ 第 6 条的正对照（迁移前 28 处全在三个待删/改文件，A.6） |
| J-S17 | Cos72 只经 SDK | 5.3.x | Cos72 仓库放与 Sin90 相同的 Unix socket / fd 子集 `clippy.toml`（§5.2 第 6 条；Cos72 没有 standalone TCP 需求的话可以直接用 SDK 全表，由 5.3.1 定），CI `clippy -D warnings` | 在 Cos72 写 `UnixStream::connect` → 红 | |
| J-S18 | `remember_once` 预查算法（H3） | b2b（v3 与 memory 客户端合并） | SDK 单测，预查循环对一个脚本化的 `recall` 闭包跑（`precheck` 是私有纯函数，不需要连接）：(a) 第 3 页出现精确标记 → `Found`，恰好调用 3 次；(b) 翻完无精确标记（每页都有 `"r:10"` 这种子串近似项）→ `NotFound`；(c) 50 页都有 cursor → `Inconclusive`，恰好 10 次；边界：第 10 页命中 → `Found`，第 11 页命中 → 仍 `Inconclusive`。另加 fake_kernel 上的整合用例：`Created` 时 `remember` 的 body 含 `dedup_key`；body 自带不同 `dedup_key` → `InvalidParams` 且假内核零调用 | 把精确相等改成 `contains` → (a)(b)(c) 全红；`RECALL_PRECHECK_MAX_PAGES` 改 11 → (c) 红 | ✅ 3 条全过；两个变异分别 3 红、1 红（A.2） |
| J-S19 | 握手响应宽松解析（H2） | a1 | proto 单测 `initialize_reply_is_lenient_where_the_kernel_type_is_strict`：多余字段让 `InitializeResult` 失败、`InitializeReply` 成功且值正确；`InitializeResult` 往返到 `InitializeReply` 值相同 | 给 `InitializeReply` 加 `deny_unknown_fields` → 红 | ✅（A.1） |
| J-S20 | fd 接管校验（H1；v3 按 M-1/M-2 补全） | a3-fd | os-fd 单测（真实 fd）：监听 Unix socket 通过；socketpair 端 `NotListening`；对端已关闭的 socketpair 端 `NotListening`（macOS 经 `EINVAL` 映射）；`/dev/null` `NotASocket`；TCP 监听 `NotUnix`；只 `bind` 未 `listen`：非 Apple 目标 `bound_but_not_listening_is_rejected`（`NotListening`）/ Apple 目标 `apple_gap_bound_but_not_listening_passes`（**断言通过**，钉住 §4.3 已知缺口）；第二次 `take_inherited_listener()` → `AlreadyTaken`。CI：`ubuntu-latest`（现有 job，编译并运行 Linux 分支）+ a3-fd 新增的 `macos-latest` job（只跑 `cargo test -p agent24-os-fd`，运行 apple 分支） | 把 `is_listening` 改成恒 `true` → socketpair 例红；去掉 apple 分支的 `INVAL` 映射 → 对端已关闭例红（scratch 实测）；去掉 `TAKEN` 检查 → 第二次调用例红 | ✅ macOS 上 7 条全过；`INVAL` 变异红；Linux 分支 `--target x86_64-unknown-linux-gnu` clippy `-D warnings --all-targets` 绿（A.1、A.10） |

---

## 7. 切法（先改 PLAN §三 与 `tasks.md` 台账再开工）

v1 按「非注释代码行」估算，与 PR-Daemon 的 SZ-1 口径不符（M10）。SZ-1 的实际口径（`PR-Daemon/scripts/pre_pr_check.py` `check_size`）：`git diff --numstat` 的**新增 + 删除**，含注释、空行、`Cargo.toml`、`clippy.toml`、CI yaml、CHANGELOG、文档；**不含**锁文件/生成物、测试文件（`tests/` 等）与 Rust 源文件内的 `#[cfg(test)] mod` 区段。注意 `test-util` feature 下的 `module::testing` 不是 `#[cfg(test)]`，**要计入**。上限按默认档 300（devloop 档是 200，另有弹性区间）。

估算方法：scratch 各文件非测试行数（含注释）× 注释系数。Sin90 被移植代码的实测注释密度：`transport.rs` 非测试 539 行里注释 178、`error.rs` 398 行里注释 250、`reconciler.rs` 1181 行里注释 590——移植时保留 Sin90 的设计说明（评审轮次磨出来的「为什么」），所以按 1.5 左右的系数估。

**v4 收敛为原型（同日追加，用户 2026-09-26 拍板）**：Sin90、Cos72 都还没有规模（Cos72 甚至还没有代码），本轮 SDK 只是原型——不需要为了单个 PR 严格 ≤ 300 行，把一批内部高度耦合的组件（proto mux 的读写任务与取消逻辑、SDK 的五个客户端与公共提取器）硬切成 17 次独立评审。**§7 从 v3 的 17 片压缩为 4 个大片**，单片允许超过 PR-Daemon SZ-1 默认上限 300 行，超限时在 PR body 里回应即可（引用下表「内容」列说明为什么按这个粒度、为什么不再拆），不强制再拆分：

| 新编号 | 内容（= v3 细粒度片的并集，见 7.1） | 仓库 | 判据（核心，见 §6 分级） | 依赖 |
|---|---|---|---|---|
| **①proto 模块侧** | transport + 握手 + fd：`ModuleEnv`/`Hello`/`InitializeReply`/`connect_from_env`、`manifest::{ManifestFacts,facts_from_yaml}`、`manifest_digest` 挪入 proto、`Connection` mux（单写任务、读任务按 id 分发、64 在途、`declare_dead`、`CallGuard` drop 取消、响应/写超时、`slot_wait`）、`module::testing`（`test-util`）、新 crate `agent24-os-fd`（`take_inherited_listener` + 两平台校验）+ proto `take_listener`/`InheritedListener` | Agent24 | J-S8、J-S9 前半、J-S12、J-S19、J-S11 的握手前置部分 | 5.1.1 冻结 |
| **②SDK crate** | `agent24-os-sdk` 骨架 + 五个客户端 + fired：`clippy.toml`、`Module`/`ModuleBuilder`/`SdkError`/`serve`/`with_env`、`ClientError`/`UnavailableCause`、`RequestContext`/`RequestId`/`ApprovalToken`、五个客户端（Events/Memory+`remember_once`/Approval/Scheduler/Model）、`FiredBody` 挪进 proto + fired 提取器 + `with_fired`、`examples/minimal` + 挂载冒烟 + 探针 `4c` + `CHANGELOG.md`；合并后打 tag `agent24-os-sdk-v0.1.0` | Agent24 | J-S1（主体）、J-S2、J-S3①、J-S4/J-S5/J-S6/**J-S7**、J-S9 后半、J-S10、J-S11、J-S13、J-S14、J-S18 | ①（proto `module`/os-fd 需先存在） |
| **③Sin90 迁移** | Sin90 `TS.1.0`（线协议金样）+ `TS.1.1`（迁移，含 §5.2 第 3 条的期望补丁与检查脚本） | Sin90 | **J-S16**（真实挂载黑盒全绿） | ②（及其 tag） |
| **④Cos72 占位** | 接入时再补：`.pilot.yml`、骨架、mytask、审批发积分、真实挂载黑盒——本轮只占位，不展开子任务，落地时按 PLAN §三 既有的 ME4-5.3.1–5.3.4 拆法走 | Cos72 | J-S17（Cos72 有代码后再定） | ②（及其 tag） |

说明（v4）：
- **单片允许超过 SZ-1 默认上限 300 行**：②尤其会超（等价于 v3 的 8 个子片总和）。开 PR 时 PR-Daemon 的 `pre-pr-check.sh` 仍要跑，只是它的「超限」告警在本轮不是阻塞项——PR body 里引用 7.1 的组件清单回应「为什么这个粒度」即可，评审知情放行。
- **①②之间仍是硬依赖**：②的 SDK 依赖只经 `agent24-os-proto`（J-S2），proto 的 `module`/os-fd 必须先能编译；一次只开一个 PR、base 恒为 `main`，①合并后再开②（M-4 的「不叠、不改 base」原则不变）。
- **片内实现顺序**：①内部先做 proto mux 核心（读写任务、按 id 分发）再做取消/超时，最后做 os-fd；②内部先做 crate 骨架与 `clippy.toml`/`check-lints.py`，再做五个客户端（Events/Memory/Approval/Scheduler/Model 顺序不重要，可并行），fired 与 `examples/minimal` 放最后。M-C 的发现（os-fd 的 CI 用到 crate 骨架里的 `check-lints.py`）在压缩后是①②两片之间的一个协调点：如果 os-fd 的 CI 步骤想引用 `check-lints.py`，等②的骨架部分先写出来即可；J-S1b/J-S3 的机械脚本本来就是「原型阶段可选」（§6），实在协调不上可以先跳过，冻结后再补。
- ③依赖②（及其 tag）：Sin90 迁移要用 `agent24-os-sdk-v0.1.0` 这个 tag。
- ④是占位：Cos72 仓库今天没有代码，本轮不展开。

### 7.1 细粒度参考（v3 遗留的 17 片设计推演，不再是独立 PR 边界）

保留原因：下面每一行仍然是有效的组件级设计决策（哪个类型、哪个函数、依赖哪条判据），只是 v4 不再要求逐片单独开 PR 评审；实现①②时按这张表分头去做，验收时以 §6 的判据清单（尤其核心档）为准，不必凑够 17 次提交。

| v3 编号 | 内容 | 判据 | 归入 v4 片 |
|---|---|---|---|
| a1 | proto `module`：`ModuleEnv`（`from_env`/`from_vars`，不持 fd）、`SPAWN_ENV_VARS`、`Hello`、`FatalHook`（newtype）、`InitializeReply`、`connect_from_env` 的握手部分、`manifest::{ManifestFacts, facts_from_yaml}`、`manifest_digest` 从 os-packages 挪入 | J-S12、J-S19 | ① |
| a2-core | proto `Connection` mux 核心：`RpcErrorInfo`/`CallError`、单写任务、读任务按 id 分发、pending 表、`declare_dead` 恰好一次、NotSent/ConnectionLost、64 在途 | J-S8 前 8 条 | ① |
| a2-cancel | `CallGuard` drop 发 cancel、响应兜底超时、写超时、`slot_wait`、`DEFAULT_RESPONSE_TIMEOUT` 编译期断言 | J-S8 后 8 条、J-S9 前半 | ① |
| a2-kit | `module::testing`（`test-util`）：与 Sin90 `test_support.rs` 同签名的 5 个函数 + `FakePeer` + `noop_hook`/`recording_hook` + `FakeEndpoint` | 自测 | ① |
| a3-fd | 新 crate `agent24-os-fd`（`take_inherited_listener`、校验、两平台分支）+ proto `take_listener`/`InheritedListener`/`ListenError` + `scripts/check-proto-exports.py`（J-S1b，可选）+ `macos-latest` CI job（可选） | J-S1b（可选）、J-S20（macOS job 可选） | ① |
| a3-skel | SDK crate 骨架：`Cargo.toml`、`clippy.toml`（26 项）、`positive_control.rs`/`positive_control_unsafe.rs`、`lib.rs`、`scripts/check-lints.py`（J-S3，可选部分） | J-S1、J-S2、J-S3 | ② |
| a3-module | `Module`/`ModuleBuilder`/`SdkError`/`serve`/`with_env`，`connect()` 里接管监听器；不含客户端访问器 | J-S11 | ② |
| b1a | `ClientError`、`UnavailableCause`、两路映射、`is_permanent`/`is_retryable` | J-S4 | ② |
| b1b | `RequestContext`/`RequestId`（含 `for_test`）/`ApprovalToken`、客户端公共 `Core`、`set_opt`、空的 `clients::METHODS` 骨架 | 提取器单测 | ② |
| b2a | Events（含 sink）+ `Module::events()` + agentd `events_emit.rs` 对等测试 | J-S5/J-S6/**J-S7**（events） | ② |
| b2b | Memory（`remember`/`recall`/`recent`）**+ `remember_once`** + `Module::memory()` + agentd `memory_callback.rs` 对等测试（scratch 等价物 SZ-1 实测 209，A.9；**兜底（v4，L-a）**：若实现时实测超 300，拆回独立的内部提交单元，不必再开单独 PR） | J-S5/J-S6/**J-S7**（memory）、J-S18 | ② |
| b2c | Approval（含 advise 的 §2.10 文档）+ `Module::approval()` + agentd `approval_callback.rs` 对等测试 | J-S5/J-S6/**J-S7**（approval） | ② |
| b3a | Scheduler + `Module::scheduler()` + agentd `scheduler_callback.rs` 对等测试 | J-S5/J-S6/**J-S7**（scheduler） | ② |
| b3b | Model + `Module::model()` + agentd `model_callback.rs` 对等测试 | J-S5/J-S6/**J-S7**（model）、J-S9 后半 | ② |
| c1a | `FiredBody`/`FireTrigger` 挪进 `agent24_os_proto::kernel_call`，agentd `scheduler_deliver.rs` 改用 | agentd 现有测试不变全绿 | ② |
| c1b | fired 提取器与 `with_fired` 注册点 | J-S10 | ② |
| c2 | `examples/minimal` + 挂载冒烟 + 探针 `4c` + `CHANGELOG.md`；合并后打 tag（J-S14） | J-S13、J-S14 | ② |

`clients::METHODS` 与 `Module` 的客户端访问器随实现顺序逐步扩；J-S15（wire 文档抽取数 ≥ 26）属于 5.4.1，不是本轮判据；FU-86（proto feature 拆分）不在①之前做，若将来做是独立重构。

---

## 8. 已拍板决策（原「待用户拍板的开放问题」）

> 2026-09-26，jason 全部拍板，均按本文当时的推荐选项裁决。以下按裁决结果记录：决策 / 拍板人 / 日期 / 理由。原选项列表保留供溯源。v2 的评审修订全部在这些决策框架内进行；个别决策下补了「v2 落实」一行，说明评审问题是怎样在该决策之内解决的，**不改变决策本身**。

**Q1（范围）——「只有两个调用方都要的才进 SDK」与 PLAN「五种客户端」怎么取舍？**
- **决策**：随 Q4 一并裁决——SDK v0.1.0 五种客户端（Events/Memory/Approval/Scheduler/Model）全进。
- 拍板人：jason。日期：2026-09-26。
- 理由：与 PLAN「五种客户端」原文一致；Cos72、Python 黑盒、T14 Node 已构成多个真实/计划中的调用方，Q1 本身不单独取舍，见 Q4。

**Q2（架构/安全）——`listener_from_env` 的 `unsafe` 放哪？** 工作区 `unsafe_code = "forbid"`，一个 fd 号变 socket 只能 `unsafe { from_raw_fd }`。原选项：(a) 新建极小 crate `agent24-os-fd`；(b) proto 的 lint 降为 `deny` 局部 `#[allow]`；(c) 改内核 launch 契约为监听 socket 路径；(d) 依赖第三方 `listenfd`。
- **决策**：方案 (a)。新建独立小 crate `agent24-os-fd`（~20 行，该 crate 单独 `unsafe_code = "deny"` + 一处带 SAFETY 说明的 `#[allow]`），proto 依赖它；proto 与 SDK 继续 `forbid`。
- 拍板人：jason。日期：2026-09-26。
- 理由：unsafe 被隔离在一个一眼能审完的 crate 里，不给 proto 这个 22k 行的内核协议 crate 开口子；与 ME3-SUP D3「用小 crate 替代自己写 unsafe」的精神一致（只是这次小 crate 是自己的）。
- v2 落实（H1）：入口定为 `take_inherited_listener()`（自读 env、进程级一次、四项校验、CLOEXEC + 非阻塞，§4.3）；校验与平台分支使它从 ~20 行长到约 110 行（含 SAFETY 说明与两平台分支），仍是一眼能审完的单文件；`#[allow(unsafe_code)]` 仍只有一处（函数级，内含 `borrow_raw` 与 `from_raw_fd` 两个块）。

**Q3（产品/架构）——Cos72「发积分经内核审批」用 gate 还是 advise？** PLAN S4 写 `_a24/approval/gate`，但 gate 只接受内核能执行的闭集（今天只有 `schedule_callback`），发积分是 Cos72 自己库里的动作，内核执行不了。原选项：(a) advise + 轮询 `status`；(b) 扩 gate 闭集，新增「内核批准后回调模块」的可执行动作。
- **决策**：方案 (a)。Cos72 用 **advise** + 模块轮询 `status(approval_id)`，批准后 Cos72 自己入账；PLAN S4 措辞改成 advise。SDK **不**提供轮询助手（如 `wait_decided`），Cos72 在自己的 outbox/泵里轮询，避免 SDK 替调用方决定轮询节奏。方案 (b)「gate 扩展为批准后回调模块」放到下一轮，登记为 followup（FU-84）。
- 拍板人：jason。日期：2026-09-26。
- 理由：积分账本的真相本来就只在 Cos72 库里，内核无论如何都执行不了它；扩 gate 闭集是新的内核能力 + 新的保留路由 + SPEC 改动，要走单独一轮设计评审，不放在本轮。
- v2 落实（M9、M8）：advise 的「请求内同步完成、请求内重试、先落本地再提交、孤儿补偿」写成 Cos72 的约束（§2.10）；PLAN S4（`PLAN-ME4-OS-CAPABILITIES.md:182`）已改为 advise。

**Q4（范围，含 Q1）——Scheduler / Model / fired 进不进 SDK v0.1.0？** 按「两个真实调用方」规则：Events/Memory/Approval 两边都要；Scheduler 与 fired 的第二个调用方是 Python 黑盒与未来的 Node 模块，Cos72 只有在 ME4-5.3.4 想从模块侧验「读不到 Sin90 的 schedules」时才需要；Model 只有 Sin90。原选项：(a) 五个客户端 + fired 全进；(b) Model 留 Sin90；(c) 只进三件。
- **决策**：方案 (a)。SDK v0.1.0 五种客户端（Events/Memory/Approval/Scheduler/Model）+ fired 全部进。Cos72 是否申请 `scheduler` 由 5.3.1 定；若不申请，5.3.4 的 schedules 隔离从内核 REST 侧验。
- 拍板人：jason。日期：2026-09-26。
- 理由：Scheduler/Model 的 wire 已被 ME4-S1/S2 冻结，SDK 这一层只是机械翻译，形状出错的风险低；T14 的 Rust 对照需要它们。

**Q5（架构）——`remember` 的幂等问题谁解？** Sin90 用「写前先 recall 翻最多 10 页找标记」绕开（`reconciler.rs:255-386`），Cos72 的摘要写入会遇到同一个问题。原选项：(a) SDK 提供 `MemoryClient::remember_once`，翻页算法原样搬进来；(b) 内核给 `remember` 加可选幂等键；(c) 各模块自己写。
- **决策**：本轮方案 (a)。SDK 提供 `MemoryClient::remember_once(kind, body, dedup_marker, ...)`，把 Sin90 的翻页算法原样搬进来（含「翻不完就报错而不是当作不存在」）。方案 (b)「内核加幂等键」登记为下一轮 followup（FU-85）；落地后 `remember_once` 改走内核幂等键，SDK 侧签名不变。
- 拍板人：jason。日期：2026-09-26。
- 理由：Cos72 的摘要写入会遇到与 Sin90 同样的「超时后重试写出两条」问题，本轮先用已验证过的翻页算法堵住；内核幂等键是新的 wire 字段 + 存储唯一索引 + SPEC 改动，是计划外的内核 task。
- v2 落实（H3）：签名定为 `remember_once(kind, dedup_key, body, request_id) -> Result<RememberOnce, ClientError>`；「翻不完」表达为 `RememberOnce::Inconclusive`（不当作不存在、不发 `remember`），Sin90 在自己一侧映射回今天的 `Err(Other)`（§3.5）；Sin90 5.2.1 改用 SDK 版，使它发布即有调用方。

**Q6（兼容策略）——`ClientError` 要不要 `#[non_exhaustive]`？** 原选项：(a) 不加；(b) 加。
- **决策**：方案 (a)。不加 `#[non_exhaustive]`。
- 拍板人：jason。日期：2026-09-26。
- 理由：新增 kind 属于 SDK 0.x minor 升级（0.x 下 minor 本就可破坏兼容）；下游可以写无通配的穷举 match，漏处理新变体时**编译失败**——Sin90 `model.rs:166-188` 正是故意这样写的，加 `#[non_exhaustive]` 会让下游必须写 `_ =>`，失去编译期提醒。

**Q7（兼容策略）——SDK 在 `initialize` 声明的协议范围：`1..=1000`（Sin90 现状）还是 `1..=1`？** 内核取 `min(模块上限, 内核上限)`：声明 1000 时，将来的 v2 内核会跟只懂 v1 的模块「成功」协商成 v2。今天内核是 v1，两种写法行为相同。
- **决策**：`1..=1`。SDK 只声明它实际实现的范围，实现时常量写 1，不沿用 Sin90 现状的 1000。
- 拍板人：jason。日期：2026-09-26。
- 理由：避免将来 v2 内核与只懂 v1 的 SDK「协商成功」讲 v2；新协议版本随 SDK 升级一起放开。Sin90 迁移后这是握手 params 里唯一有意的变化（§5.2 第 2 条单列）。
- v2 落实（L1）：scratch 常量已改为 `PROTOCOL_MAX = 1`，J-S11 断言 `{min:1,max:1}`。

**Q8（工程成本）——proto 要不要拆 `kernel` / `module` feature？** 不拆：Sin90/Cos72 编译时带上 `command-fds`、`close_fds`（精确钉版）、`rustix`、`hyper`、`agent24-domain`（`serde_yaml`、`schemars`）。拆：proto 22k 行要按 feature 划 `#[cfg]`，本轮工作量明显变大。
- **决策**：本轮不拆。在 5.2.1 的 PR 里记录 Sin90 冷编译时间变化；登记为下一轮 followup（FU-86），若 Cos72 或第三方反馈成本不可接受再拆。
- 拍板人：jason。日期：2026-09-26。
- 理由：22k 行的 crate 按 feature 划 `#[cfg]` 本轮工作量明显变大，且当前没有实测数据证明成本不可接受。

**Q9（治理）——`agent24-os-sdk` 的 tag 由谁打、`0.1.x` 补丁怎么发？** PLAN 只写了 `agent24-os-sdk-v0.1.0`。
- **决策**：SDK 与 Agent24（daemon）版本分开打 tag，只在 SDK 或 proto `module` 有改动时打；每个 SDK tag 在 `CHANGELOG.md` 的独立小节列出对应的内核最低版本；v0.5.0 发布清单（ME4-6.0.1）把这个 tag 列进去。
- 拍板人：jason。日期：2026-09-26。
- 理由：SDK 与内核的演进节奏不同，绑在一起打 tag 会造成不必要的版本号跳动，也不利于下游按需锁定 SDK 版本。
- v2 落实（L10）：`CHANGELOG.md` 的 `0.1.0` 小节放进 c2（打 tag 的 PR），J-S14 检查「最低内核版本」一行存在。

**Q10（架构）——库默认 `exit(70)` 可以吗？** 连接断了这一代就结束（无重连），Sin90 的做法是立刻退出让 supervisor 重启。原选项：(a) 默认同 Sin90，可覆盖；(b) 默认只记日志、由 `main` 决定。
- **决策**：方案 (a)。SDK 默认 `warn!` + `exit(70)`（Sin90 现状），可用 `on_connection_lost` 覆盖。
- 拍板人：jason。日期：2026-09-26。
- 理由：零配置就是正确行为；方案 (b) 要 serve 同时盯连接存活、在途 HTTP 请求会被中断、退出码也会变，代价更大，覆盖口留给测试足够。
- v2 落实（M3）：测试入口 `with_env(env, hook)` 把钩子做成必填参数，测试不可能落到默认 `exit(70)`；默认值本身不变（§3.2）。

**Q11（架构）——wire DTO 放两份（内核私有 + SDK）还是挪进 proto 共用？** 内核的 params 类型都在 agentd 里且大多私有（`memory_callback.rs:36-67` 等）。原选项：(a) SDK 自带一份 + agentd 对等测试；(b) 挪进 proto 共用。
- **决策**：方案 (a)。SDK 自带一份 wire 类型（请求严格、响应宽松），agentd 加对等测试钉住（J-S7）；唯一例外是 `FiredBody`，挪进 `agent24_os_proto::kernel_call`，内核与 SDK 共用一份。
- 拍板人：jason。日期：2026-09-26。
- 理由：内核 params 类型已被 S1/S2/ME-3e 冻结，把它们全挪进 proto 改动面大、要再过一轮评审；`FiredBody` 本来就只是一个借用序列化结构，挪动成本极小，且方向单一（内核→模块），两边共用一份最省事。
- v2 落实（M6、H2）：J-S7 落到各回调模块自己的单测、用现有 handler 夹具跑真实 handler（§6）；握手响应同样按「响应宽松」处理（`InitializeReply`，§4.1）。

---

## 9. 自审（送复审前自己先挑的毛病）

1. **本文的 proto `Connection` 只是桩。** 移植 Sin90 transport 时最容易丢的是 `declare_dead` 与 `closed` 标志在同一把锁下插入 pending 的竞态防护（Sin90 `transport.rs:179-218`）。J-S8 移植了 Sin90 的并发测试，但评审应专门看 a2-core 的这一段。
2. ~~`RequestId` 无公开构造函数时测试怎么造~~ → v2 定为 `test-util` 下的 `RequestId::for_test`（§3.3）。
3. **`EventSink` 的丢弃计数只在溢出时加**；Sin90 的「按 2 的幂打日志」没搬进草图（草图只计数）。b2a 实现时补上，属机械移植。
4. **`Module::serve` 返回即意味着 HTTP 服务停了**，但连接可能仍活着；反之连接死了由 `FatalHook` 负责退出。两者不联动是 Sin90 现状，保留。
5. ~~fired 时间戳校验与内核 `fmt_iso` 的耦合~~ → 由 J-S10(b) 用内核真实发出的字节钉住（§6）。
6. ~~J-S15 漏掉拼接出来的方法名~~ → 各客户端写完整方法名 const、`Core::call` 只收 `&'static str`，J-S15 先断言抽取数 ≥ 26（§6）。
7. **（v2 新，v3 定稿）macOS 的监听判定是间接的、弱的，已接受。** `getpeername == ENOTCONN` 也会把「只 `socket()`/`bind()`、未 `listen()` 的流 socket」判为监听中（h1probe 实测）；对端已关闭的流回 `EINVAL`，映射为 `NotListening`。内核传来的 fd 3 永远是 `listen()` 过的（`launch.rs:456` 起），出现这种 fd 意味着有人伪造环境，后果只是 `accept` 报错、模块退出，不会越权；Linux 上 `SO_ACCEPTCONN` 是直接判定。缺口写进 §4.3 第 3 步与 os-fd 源码注释，由 `cfg(apple)` 测试钉住，macOS CI job 运行它（§4.3 第 7 条）。
8. **（v2 新）rustix 版本分叉。** os-fd 用 rustix 1.1、proto 仍是 0.38（`agent24-os-proto/Cargo.toml` 的 `rustix = { version = "0.38", features = ["process"] }`）。1.1.4 已在锁文件里，不新增下载；proto 升 1.x 不在本轮。0.38.44 的 macOS panic 只在 `getsockname` 解码未命名 AF_UNIX 地址时出现，proto 今天不调它。
9. **（v2 新）`remember_once` 超时后的残余重复。** 见 §3.5「残余风险」；与 Sin90 今天一致，FU-85 根治。
10. **（v2 新，v3 改写，v4 收敛）PR 的合并方式。** v2 写的是「stacked、从底往上依次合、每合一个就把下一个 rebase 到 main」；v3 改为**根本不叠**：一次只开一个 PR、base 恒为 `main`、上一片合并后下一片才开，17 片按 DAG 排队（§7 说明第 2 条，v3 时点）。**v4（同日追加，用户拍板原型收敛）把 17 片压缩为 4 个大片**（①proto 模块侧 ②SDK crate+客户端+fired ③Sin90 迁移 ④Cos72 占位），「不叠、base 恒为 main、上一片合并后下一片才开」这条原则不变，只是片数从 17 降到 4、单片允许超过 SZ-1 默认上限（§7）。代价从「总工期更长（无并行评审）」变成「单次评审的 diff 更大」；换来的仍是永远碰不到「合并删分支关掉上层 PR」「改 base 作废 approve」，且省下 17 次评审排队的等待成本——这本来就是原型阶段愿意接受的取舍。
11. **（v3 新）本机 clippy 版本错位。** `rustup run 1.98.0 cargo clippy` 在本机调用的是 Homebrew 的 `cargo-clippy`（0.1.95），因为 `rustup run` 只替换它自己启动的那一个二进制、`cargo` 再去 PATH 里找 `cargo-clippy`。v3 用 `scratchpad/c98` 重跑了全部 clippy 读数（文首说明）；判据命令里写的是 `cargo +1.98.0 …`（rustup 代理会把整条工具链放到 PATH 前面），不受影响，但本地复现判据时不要用 `rustup run`。

---

## 10. followups

**已登记（jason 2026-09-26 拍板后登记进 `docs/agent/followups.md`）：**

- **FU-84** 审批裁决回推（§8 Q3 (b)：gate 扩展为「内核批准后回调模块」的可执行动作）。v2 补充：它也是 §2.10 孤儿审批的根治办法。
- **FU-85** 内核 `remember` 幂等键（§8 Q5 (b)）。v2 补充：它同时消除 §3.5 的「超时后残余重复」。
- **FU-86** proto `kernel`/`module` feature 拆分（§8 Q8）。v3 补充（L4）：os-fd 用 rustix **1.x**，而 Sin90 锁文件今天只有 rustix 0.38.44（proto 与其它依赖带进来的）——Sin90 迁移后锁文件会**新增 rustix 1.x**，两个大版本并存编译；5.2.1 的冷编译时间记录要把这一项单列。拆 feature 之后模块侧仍需要 os-fd，所以拆分的判据改为「不再出现 proto 自己的 rustix 0.38」而不是「不再出现 rustix」（followups.md 同步）。

**v2 新登记：**

- **FU-87** `retry_class`（错误 → 重试处置的分类表）不进 SDK v0.1.0（§2.6，M5）：等 Cos72 的 outbox 落地后，按 Sin90 与 Cos72 两个真实调用方的实际分类表决定是否提取进 SDK，届时一并决定 `Cancelled` 归「结果未知」还是「普通退避」（Sin90 今天是后者，`reconciler.rs:745-752`）。
- **ME4-CODEX-DEBT-8** 本文 v1 的第 1 轮评审与 v2 的复审都是 Tier-2（Opus 子代理）；Codex 额度 2026-09-29 19:28 恢复后对最终版补一轮 Codex。（rebase 到 `bb3505a` 后改号：origin/main 上 -7 已被 #512 占用。）

---

## 11. 评审处置

### 11.1 第 1 轮评审（v1，Tier-2 Opus，结论 CHANGES）逐条处置 → v2

| 编号 | 问题（摘要） | 处置 | 落点 |
|---|---|---|---|
| H1 | `listener_from_env` 不健全：fd 号外传、可多次接管、不校验、不设 CLOEXEC；token 留在 environ 未处理 | **修**。`agent24-os-fd::take_inherited_listener()` 唯一入口：自读 `A24_LISTEN_FD` 且必须为 3、`AtomicBool` 进程级一次、借用校验 `S_ISSOCK`/`AF_UNIX`/`SOCK_STREAM`/监听（Linux `SO_ACCEPTCONN`，macOS 因不实现该选项改用 `getpeername == ENOTCONN`）、通过后才接管、设 `FD_CLOEXEC` 与非阻塞；`ModuleEnv` 不持 fd；os-fd 不导出接受裸 fd 的安全函数；签名写死。token：**接受风险并说明**（一次性 + 端点接受首连即关闭；`remove_var` 在多线程运行时里的 SAFETY 不可证），给 `SPAWN_ENV_VARS` 供子进程 `env_remove` | §4.3、§4.1、附录 B.2、J-S20 |
| H2 | 握手响应用带 `deny_unknown_fields` 的 `InitializeResult` 解析 | **修**（选宽松镜像，不选冻结，理由见 §4.1）。`module::InitializeReply`；「宽松解析」列入 TS.1.0 入站金样 | §4.1、§5.1、J-S19 |
| H3 | `remember_once` 无签名与语义；v0.1.0 无调用方 | **修**。签名 + `RememberOnce{Created,Found,Inconclusive}`；标记键 `body.dedup_key` 与 Sin90 相同；`Inconclusive` 为 `Ok` 变体、Sin90 映射回 `Err(Other)`；写明非原子/单写者与 Cos72 的 outbox 串行化办法；**Sin90 5.2.1 改用 SDK 版**；新增 J-S18；tasks.md/PLAN 增 b2d 行 | §3.5、§5.1、§7、J-S18 |
| H4 | Sin90 迁移验收网不足 | **修**。proto `test-util` 与 Sin90 `test_support.rs` 同签名（表见 §4.5）+ Sin90 一行 shim + `SchedulerClient`/`ModelClient` newtype 保旧签名；验收加「reconciler/model 测试模块 diff 0 行」（带正对照的脚本）；TS.1.0 增入站金样（每个错误 kind 回放、outbox 行状态与 attempts、`unavailable` 的 raw→data、Usage u32→u64、宽松解析、recall 预查三态） | §4.5、§5.1、§5.2 第 2、3 条 |
| H5 | clippy 名单可绕过 | **修**。名单扩到 24 项（含评审点名的全部 + 实验中发现的 `Deserializer::new`、`axum::Json::from_bytes`、`OwnedFd`），正对照逐项一行、条数与种类数双重断言；proto/os-fd 不导出宏与 socket 别名（J-S1b，机械 `rg`）；`sdk-bypass/` 用新名单重跑：10 种绕过除「外部宏」外全部被 clippy 抓住，外部宏由 J-S1b 在源头禁止 | §2.2、J-S1、J-S1b、附录 A.3/A.4 |
| M1 | J-S2 命令恒输出两行 | **修**。改 `cargo metadata` + jq 断言依赖集合相等；正/负实测 | J-S2、A.5 |
| M2 | J-S3 只断言 `lints.workspace` 字段、无正对照 | **修**。`tomllib` 解析每个成员的 `Cargo.toml`、os-fd 为唯一例外且必须 `deny`；`positive-control-unsafe` 必须编译失败；`#[allow(unsafe_code)]` 全局恰 1 处；已在真实 Agent24 工作区跑绿 | J-S3、A.5 |
| M3 | 切法可测性 | **修**。J-S11 移到 a3-module；`ModuleEnv::from_vars`；`ModuleBuilder::with_env(env, hook)` 钩子必填；`testing::recording_hook` | §3.2、§4.1、§7 |
| M4 | `manifest_digest` 实际位置 | **修**。在 `agent24-os-packages/src/discovery.rs:46-57`，挪进 `agent24_os_proto::manifest`，os-packages 转出；新增依赖边 os-packages → proto，写明无环与下游无新增负担 | §2.1、§4.4 |
| M5 | `retry_class` 是新语义 | **不保留（删除）**，按提取原则的内容取舍：非 Sin90 现有语义、与 Sin90 的 `Cancelled` 处置冲突、两个调用方都不用；Cos72 用 `is_permanent`/`is_retryable` + 穷举 match；FU-87 | §2.6、§1.2 #24 |
| M6 | J-S7 可行性 | **修**。落到 5 个回调模块各自的 `#[cfg(test)]`，SDK 客户端 → `fake_kernel` → 现有 `h.call(params)` 夹具（逐一给出行号）→ 结果回 SDK 用 SDK 类型解析；可选字段全 `Some` 一次、全 `None` 一次；随各客户端 PR 落地 | J-S7、§7 |
| M7 | J-S10(b) 用 rg 计数不够；J-S15 无正对照；Sin90 的 clippy.toml | **修**。J-S10(b) 用 `scheduler_deliver.rs` 测试里内核真实发出的 head/body 喂 SDK 提取器；J-S15 先断言抽取数 ≥ 26（`concat!` 变异降为 25）；Sin90 放 clippy.toml——**有理由地收窄为 Unix socket / fd 子集**（Sin90 standalone 的 TCP 监听与领域 JSON 解析是合法用途），并在迁移前的 Sin90 树上实测正对照（28 处全在待删/改的三个文件） | J-S10、J-S15、§5.2 第 6 条、A.6 |
| M8 | 与 PLAN 不同步 | **修**。`PLAN-ME4-OS-CAPABILITIES.md` S3-1（`:166-172`）改为本文签名与名单出处；S4（`:182`）改为 advise；§三 5.1.2 列表换成 §7 的 17 行；本文 §0 第 6 条改为「已拍板决策」 | PLAN、§0 |
| M9 | advise 孤儿审批 | **修**。advise 必须在 submit handler 内同步完成、`Timeout` 同 token 请求内重试（wire 幂等，`approval_callback.rs:478-487`）；先落 `pending_award` 再提交；孤儿不入账（`award_id` 唯一 + 只认记下的 `approval_id`）、靠内核过期清理；根治归 FU-84 | §2.10、`ApprovalClient::advise` 文档 |
| M10 | 行数口径 | **修**。按 SZ-1 实际口径（加删都算、含注释与配置、不含测试）重估；a2 预拆 a2-core/a2-cancel，另拆出 a2-kit、a3-fd/a3-skel/a3-module、b1a/b1b、b2a–b2d、b3a/b3b，共 17 个；tasks.md 补 `remember_once`（b2d）与 Sin90 TS.1.0（ME4-5.2.0）行 | §7、tasks.md |
| L1 | sketch `PROTOCOL_MAX = 1000` | **修**，改 1；J-S11 断言 `{1,1}` | §3.2、J-S11 |
| L2 | 措辞 | **修**：§0 第 6 条；删去 v1 里「由评审定」「若评审认为…可删」「倾向前者，未写进草图」等悬置措辞，逐一改为确定决定（MSRV 作业、`notify`、`RequestId::for_test`） | §0、§2.8、§4.2、§9 |
| L3 | MANIFEST 类型 | **修**。Sin90 实际常量是 `sin90::ai::MANIFEST_YAML: &str`；`Module::builder` 收 `impl Into<Cow<'static, str>>` | §3.2、§5.1 |
| L4 | `ManifestFacts` 应宽松 | **修**。不带 `deny_unknown_fields`、只取三字段 | §4.4、§2.8 |
| L5 | CI 工具链事实 | **修**。Agent24 CI 装 stable（`ci.yml:61,195`），1.98.0 是判据命令钉的版本；MSRV 1.88 check 作业定为要 | §2.8 |
| L6 | `notify` 无调用方 | **删**（按提取原则的内容取舍）；PLAN S3-1 同步 | §4.2 |
| L7 | 注释编号错（positive-control 标成 J-S3） | **修**。`positive_control.rs`/`Cargo.toml` 注释改为 J-S1，新增的 unsafe 正对照标 J-S3 | scratch |
| L8 | SDK 转发 `test-util` | **修**。`test-util = ["agent24-os-proto/test-util"]`，`agent24_os_sdk::testing` 转出；Sin90 dev 依赖只写 SDK | §3.1、§5.1 |
| L9 | tasks.md 与门状态不一致 | **修**（只改 M4b 门 / 5.1.1 两行；4.2.3b 与 S1/S2 各行留给 PR #513）。M4b 门回填为 DONE（Sin90 `135ddb7` T5.5.1），5.1.1 状态写 v2 待复审 | tasks.md |
| L10 | CHANGELOG 最低内核版本 | **修**，放 c2，J-S14 检查 | §2.8、§7 |
| L11 | `with_fired` 文档写 10s / 3 次 | **修**，并补 5s/15s 退避与 64 KiB 响应上限，数值带出处 | §2.5、§3.6 |

### 11.2 v2 复审（Tier-2 Opus，结论 CHANGES：2H/5M/5L）逐条处置 → v3

| 编号 | 问题（摘要） | 处置 | 落点 |
|---|---|---|---|
| H-1 | 「`clients/model.rs` 测试模块 diff 0 行」做不到：测试 `use crate::ai::UnavailableCause` 并用它构造 `ClientError::Unavailable`，迁移后 cause 是 SDK 类型 | **修，按 (a)**。§5.2 第 3 条改为「与已提交的期望补丁逐字相等」，期望补丁只允许 `model.rs:223` 那一行改成 `use agent24_os_sdk::UnavailableCause;`（reconciler 部分为空）；脚本同时断言补丁本身只含这一对 `+/-` 行、找不到 `#[cfg(test)]` 即失败。实测：v2 脚本在正确迁移上 rc=1（问题复现）；v3 脚本绿；4 条正对照全红 | §5.1 model 行、§5.2 第 3 条、A.8 |
| H-2 | H5 绕过分析标反：实测外部宏 b3 被抓、跨 crate 别名 b2 没被抓；`FromStr` 两条路未禁 | **修**。§2.2 表重做（b1–b15，v1/v2/v3 三列 + 「由谁挡」），附录 A.3 用真 clippy 0.1.98 重跑并按行号标注；名单加 `str::parse`、`core::str::FromStr::from_str` 到 26 项，正对照 26/26/26；J-S1b 改为「proto `module*` 与 os-fd 不许任何 `pub type`、`pub use` 只能转出 crate 内部项」+ 全 proto 无 `#[macro_export]` 与点名 socket 类型的别名，改用 Python（`rg` 缺失时 v2 写法会静默通过，实测），正对照补两跳别名，6 红 1 绿；`FatalHook` 由类型别名改 newtype、`take_listener` 返回具名 `InheritedListener`（否则 J-S1b 自己就过不了）；§2.2 写明名单是绊线不是沙箱，b13（`Json::from_request` 喂人工请求）、b14（`File::open("/dev/fd/N")`）列为已知残余、不防；PLAN S3-1 的那句「外部宏展开 clippy 不 lint」改正 | §2.2、§3.1、§4.3、J-S1、J-S1b、A.3、A.4、B.1、PLAN S3-1 |
| M-1 | macOS 弱校验 | **接受并写明**。Linux `SO_ACCEPTCONN`；macOS `getpeername`：`ENOTCONN` → 监听、`Ok`/`EINVAL` → `NotListening`、其它 → `Io`；「`socket()`/`bind` 未 `listen` 也会通过」写进 §4.3 第 3 步与 os-fd 源码注释；新增 `cfg(apple)` 测试 `apple_gap_bound_but_not_listening_passes`（断言通过，钉住缺口）与非 Apple 的 `bound_but_not_listening_is_rejected`（断言 `NotListening`）；新增对端已关闭例覆盖 `EINVAL` 映射（去掉映射 → 红，实测）；h1probe 读数进 A.10 | §4.3、§9 第 7 条、J-S20、A.10、B.2 |
| M-2 | J-S20 两个平台只跑了一个 | **修**。a3-fd 片的工作内容加一个 `macos-latest` CI job，只跑 `cargo test -p agent24-os-fd`；Linux 分支在现有 `ubuntu-latest` job 里编译并运行；本机另用 `--target x86_64-unknown-linux-gnu` 过了 clippy `-D warnings --all-targets` | §4.3 第 7 条、§7 a3-fd 行、J-S20、A.10 |
| M-3 | 监听器接管时机与进程级一次性 | **修**。生产路径在 `connect()` 第一步接管（CLOEXEC 在模块能起子进程之前设上、坏 fd 启动即报错），存进 `Module`；`with_env` 路径跳过，`serve` 返回 `SdkError::NoListener`；写明「每个测试二进制至多一个测试调 `take_*`」，并写进 `take_listener` 文档注释与 os-fd 那条测试的注释 | §2.3 第 0 步、§3.2、B.1、scratch `module.rs` |
| M-4 | 合并流程与依赖链 | **修**。一次只开一个 PR、base 恒为 `main`、不叠不改 base；依赖改 DAG：a3-fd 只依赖 a1，a3-skel 从 main 开，c1 拆出 c1a（`FiredBody` 挪进 proto + agentd 改用）从 main 开、c1b 为提取器；b2b+b2d 合并（SZ-1 实测 209）；写明 `clients::METHODS` 与 `Module` 访问器随各客户端 PR 逐步扩；J-S3 脚本随 a3-skel、J-S1b 脚本随 a3-fd（均实估 ≤ 300）。仍为 17 片 | §7、§9 第 10 条、PLAN §三、tasks.md |
| M-5 | M4b 门状态 | **修，按事实判断（v3 时的判断）；v4 按 `e39a2af` 更新为已回填**。v3 判断时：rebase 后 origin/main 的 4.3.1 行是 `PR_OPEN #512`，而 #512 已于 2026-09-26 合并（`bb3505a`）→ 4.3.1 改 `DONE`；M4b 门的前置「4.3.1、M3 门」里 M3 门（及其前置 M2 门）当时台账上仍是 `BACKLOG`（Sin90 `origin/main` 实际已含 T3.1.1–T3.5.1、T4.1.1–T4.4.1，只是没人回填），所以 v3 把 M4b 门标 `IN_PROGRESS`，注明「待 M2/M3 门回填 DONE 后本行改 DONE」。**`e39a2af`（`docs(adr): ADR-032 iDoris 部分核对修订`所在分支之前的一次提交，`docs(agent): 回填 ME4-M2/M3/M4b 门 DONE（按 Sin90 origin/main 135ddb7 核对）+ 4.3.1 进度`）已经按这条 v3 的注明把 M2 门、M3 门、M4b 门三行全部回填 `DONE`**（`docs/agent/tasks.md` 现状：M2 门 `DONE`、M3 门 `DONE`、M4b 门 `DONE`）。v4 不再重复这一步，只核对 tasks.md 现状与本文一致 | tasks.md（已由 `e39a2af` 完成，v4 只核对） |
| L1 | 第二次 clippy 应只看库 | **修**。`cargo clippy -p agent24-os-sdk --lib --features test-util -- -D warnings` | J-S1、A.1 |
| L2 | J-S3 正则漏 `#![allow]`/多项 allow/`expect`；os-fd 重述的 lint 可能漂移 | **修**。正则 `\b(allow\|expect)\s*\([^)]*\bunsafe_code\b`（去注释后匹配，也覆盖 `cfg_attr`）；断言 os-fd lint 表去掉 `unsafe_code` 后与 `[workspace.lints]` 相等；7 条正对照全红，真实工作区绿 | J-S3、A.5 |
| L3 | §5.1 reconciler 行措辞 | **修**。写清删掉哪两个常量、测试模块只用到 `RECALL_PRECHECK_MAX_PAGES`（`:3678`）、补的那行必须是 `#[cfg(test)] use …`（否则非测试构建 `unused_imports`、`-D warnings` 红）、测试模块逐字不变（期望补丁 reconciler 部分为空）、测试模块里两处指向已删项的 intra-doc 链接不进 rustdoc | §5.1 reconciler 行 |
| L4 | FU-86 漏了 rustix 1.x | **修**。Sin90 锁文件今天只有 rustix 0.38.44，迁移后新增 rustix 1.x（os-fd），冷编译记录单列；FU-86 的判据改为「不再出现 proto 自己的 rustix 0.38」 | §10、followups.md FU-86 |
| L5 | J-S14 的 tag 不是 c2 PR 内能验的 | **修**。J-S14 拆成「c2 PR 内：CHANGELOG」与「c2 合并后：在 main 合并提交上打 tag、`ls-remote` 非空、探针 ●，然后 c2 行才标 DONE」 | J-S14、§7 c2 行 |
| （自查） | 本机 `rustup run 1.98.0 cargo clippy` 实为 Homebrew clippy 0.1.95 | 全部 clippy 读数用真 0.1.98 重跑，读数与 0.1.95 逐条相同；判据命令 `cargo +1.98.0` 不受影响（**v4 更正，M-B：这一句是错的**，`cargo +1.98.0` 同样中招，见 §11.3 M-B） | 文首、§9 第 11 条、附录 A |

### 11.3 v3 复审（Tier-2 Opus，结论 CHANGES：1H/3M/7L）逐条处置 → v4（冻结）

| 编号 | 问题（摘要） | 处置 | 落点 |
|---|---|---|---|
| H-A | 「测试模块只改一行」的检查脚本 `tests_of()`（v2/v3：`sed -n '/^#\[cfg(test)\]$/,$p'`）从**第一个** `#[cfg(test)]` 行开始截到文件尾；而真实迁移会往 `reconciler.rs` 非测试部分新增一行 `#[cfg(test)] use agent24_os_sdk::clients::RECALL_PRECHECK_MAX_PAGES;`（§5.1），这行本身也匹配 `^#\[cfg(test)\]$`，脚本会把从这里到文件尾的一大段非测试代码误当成「测试模块」去比对期望补丁，在一次完全正确的迁移上产生假阳性差异 | **修**。`tests_of` 改为 awk 实现，只认「`#[cfg(test)]` 紧跟 `mod tests {`」这一锚点，且要求每个文件恰好命中一次，否则 `exit 3`（不静默通过）；`scratchpad/h1mig/` 实测三种情形：真实形状迁移（非测试部分加两行诱饵、rustfmt 后）rc=0，测试模块再被动一处 rc=1，锚点缺失（`#[cfg(test)]` 改写、不再逐字匹配）rc=3；写明该脚本是 TS.1.1 那个 PR 的一次性检查，合并后即可删，不进常驻 CI（L-f） | §5.2 第 3 条、A.8 |
| M-A | §2.2 的已知残余清单（b13/b14）没覆盖：① proto 内核侧公开函数直接把 socket 类型写在函数签名的返回值里（不经别名）；② 内核侧（非 `module*`/os-fd）两跳别名里第一跳非 `pub`、第二跳只点名本地标识符——J-S1b 是文本/正则扫描，两种都扫不到 | **接受并写明**。新增 b16（内核侧函数直接返回 socket 类型）、b16b（内核侧两跳别名，第二跳不点名具体类型）为已知残余、记录不防；写明 J-S1b 的规则②（禁 `pub type`）只管 `module*`/os-fd，规则④（禁点名 socket 的 `pub type`/`pub use`）虽覆盖全 proto 但只按文本「点名」判断，二者组合起来仍有这两类残余；登记 **FU-88**（proto 内核侧 API 按 kernel/module 可见性收紧，与 FU-86 一起做） | §2.2、followups.md FU-88 |
| M-B | 文首与 §9 第 11 条写「判据命令里的 `cargo +1.98.0` 走 rustup 代理，不受 Homebrew clippy 覆盖问题影响」——实测是错的 | **修**。本机 `cargo +1.98.0 clippy` 实测同样拿到 Homebrew 的 clippy 0.1.95（`+1.98.0` 只挑 `cargo` 本体的工具链，`cargo-clippy` 子命令仍按 PATH 查找）；改为「本地复现一律用 `scratchpad/c98` 的方法，并先断言 `clippy -V` 为 `clippy 0.1.98` 再执行判据；CI 不受影响（CI runner 没有 Homebrew）」 | 文首、§9 第 11 条、§11.2（自查）行 |
| M-C | a3-fd 的 CI 用到 a3-skel 提供的 `scripts/check-lints.py`（J-S3 ③），但依赖列只写了 a1，没把 a3-skel 列为前置 | **修**。a3-fd 依赖加 a3-skel；推荐串行顺序里 a3-skel 挪到 a3-fd 之前（`… a2-kit → a3-skel → a3-fd → a3-module → …`）；§7、tasks.md、PLAN §三三处同步。**同日追加的原型收敛**（见下）把 §7 压成 4 片后，a3-fd/a3-skel 分别归入片①/片②，跨片顺序作为片内实现顺序的footnote 保留，不再是独立 PR 间的 DAG 依赖 | §7、tasks.md、PLAN-ME4-OS-CAPABILITIES.md |
| L-a | b2b（memory + `remember_once` 合并片）的 SZ-1 只有一次 scratch 实测（209），没写超预算时的退路 | **修**。写明兜底：开 PR 时若实测 SZ-1 超过 300，把 `remember_once` 拆回独立的 b2d PR（依赖 b2b） | §7 b2b 行、tasks.md |
| L-b | §11.2 的 M-5 行与 tasks.md 现状不一致：M-5 写的是 v3 时点「M2/M3 门还是 BACKLOG、M4b 门标 IN_PROGRESS」，但 `e39a2af`（`docs(agent): 回填 ME4-M2/M3/M4b 门 DONE`）已经把三行全部回填 DONE | **修**。§11.2 M-5 行补充「v4 按 `e39a2af` 更新」，注明 M2/M3/M4b 门现状均为 `DONE`，不再重复回填动作，只核对一致性 | §11.2、tasks.md |
| L-c | J-S1b（Python，`check-proto-exports.py`）与 J-S3（`check-lints.py`）都是逐行正则/字符串匹配，没写明它们隐含假设源码已经 `cargo fmt --check` 过 | **修**。两条判据的「怎么验」都补一句：CI 里必须让 `cargo fmt --check` 先跑或与本判据同批失败，否则可以把 `pub type`/`#[macro_export]`/属性宏故意拆成多行绕开逐行正则；两个脚本不是重新实现 rustfmt 或完整的 Rust 解析器 | J-S1b、J-S3 |
| L-d | serve 路由挂载（`Module::serve(router)`）目前设计为消费 `self` 的整块阻塞调用，评审问是否该拆出一个可 oneshot（单次请求测试用）的方法 | **不定案，留给实现**。本轮不改 `Module::serve` 的签名；实现 a3-module（压缩后为片②的一部分）时若发现测试或黑盒场景需要不经 `axum::serve` 单发一次请求，由实现者决定是否拆一个内部可 oneshot 的辅助方法，不影响 `serve` 对外签名 | （不改文档正文，留待实现） |
| L-e | mux 核心的 `call` 方法签名在拆成 a2-core/a2-cancel 两片时会不会跟着变，没写清楚 | **修**。写明 `call(&self, method, params, opts: CallOptions)` 的签名与 `CallOptions` 类型在写 mux 核心时就落定为最终形态；随后补的取消/超时能力都通过 `CallOptions` 内部字段与状态机实现，不再改 `call` 本身的签名 | §4.2 |
| L-f | H-A 新脚本会不会被误当成要长期维护的 CI 判据 | **修**。§5.2 第 3 条与 A.8 都写明：这是 TS.1.1 那一个 PR 的一次性验收检查，假设了固定的两个文件与固定的一行改动，迁移合并后即可删，不进 Sin90 常驻 CI | §5.2 第 3 条、A.8 |
| L-g | 复审里若干次要措辞/格式类问题（未点名到需要改设计内容的程度） | **回应但不改文档**：在 TS.1.1/各 PR 的 PR body 里按编号回应，不在本文里逐条改写 | （PR body） |

---

## 附录 A：scratch 与命令记录

位置：`scratchpad/sdk-sketch/`（工作区成员 `fd/` = `agent24-os-fd` 实物，`proto/` = proto 模块侧 API 桩（v3 起模块侧拆到 `proto/src/module.rs`，与真实 proto 将来的文件布局一致，J-S1b 的 `module*` 范围才有意义），`sdk/` = SDK 草图 + `examples/minimal.rs` + `tests/sketch.rs` + `clippy.toml`）；`scratchpad/sdk-bypass/` = 评审的绕过实验（v1 sketch + `bypass.rs` b1–b15 + proto 里的一跳/两跳别名与宏）；`scratchpad/sin90-135ddb7/` = `git archive 135ddb7` 的 Sin90 树（只加了一个 `clippy.toml`）；`scratchpad/h1mig/` = §5.2 第 3 条脚本的实验仓库；`scratchpad/sz1-b2b/` = b2b 合并片的 SZ-1 实测仓库；`scratchpad/h1probe/` = macOS socket 状态探针；`scratchpad/js3.py`、`js1b.py` = 将来的 `rust/scripts/check-lints.py`、`check-proto-exports.py`。工作区 lint 与 Agent24 相同：`unsafe_code = "forbid"`、`unwrap_used`/`expect_used = "deny"`，edition 2024。

**所有 clippy 读数用 `scratchpad/c98`（v3）**：`PATH=$HOME/.rustup/toolchains/1.98.0-aarch64-apple-darwin/bin:$PATH exec cargo "$@"`。`c98 clippy -V` → `clippy 0.1.98 (88d9e12ae1 2026-08-18)`；而 `rustup run 1.98.0 cargo clippy -V` → `clippy 0.1.95`（Homebrew）。下文 `c98` 即此。

### A.1 四件套（v3 重跑）

```
$ c98 check --offline --workspace --all-targets
    Finished `dev` profile [unoptimized + debuginfo] target(s)

$ c98 clippy --offline --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s)
$ c98 clippy --offline -p agent24-os-sdk --lib --features test-util -- -D warnings      # L1
    Finished `dev` profile [unoptimized + debuginfo] target(s)

$ c98 test --offline --workspace
test tests::a_listening_unix_socket_passes ... ok                      # agent24-os-fd (J-S20, real fds, macOS)
test tests::a_socketpair_end_is_not_listening ... ok
test tests::a_socketpair_end_with_peer_closed_is_not_listening ... ok   # v3: EINVAL -> NotListening
test tests::apple_gap_bound_but_not_listening_passes ... ok             # v3: pins the macOS gap
test tests::a_regular_file_is_not_a_socket ... ok
test tests::a_tcp_listener_is_not_unix ... ok
test tests::only_one_attempt_per_process ... ok
test result: ok. 7 passed; 0 failed
test tests::initialize_reply_is_lenient_where_the_kernel_type_is_strict ... ok   # proto (J-S19)
test tests::digest_format ... ok
test result: ok. 2 passed; 0 failed
test clients::memory::tests::found_on_a_later_page_stops_paging ... ok           # sdk lib (J-S18)
test clients::memory::tests::exhausted_without_exact_match_is_not_found ... ok
test clients::memory::tests::ten_pages_with_cursor_is_inconclusive ... ok
test result: ok. 3 passed; 0 failed
test permanence_is_sin90s ... ok                                                 # sdk tests/sketch.rs
test caller_can_render_rejection_in_its_own_shape ... ok
test kernel_shaped_fired_body_is_accepted ... ok
test fired_ok_and_rejections ... ok
test result: ok. 4 passed; 0 failed

$ c98 fmt --all --check      # 无输出，退出 0
```

os-fd 最初用 rustix 0.38（与 proto 同版）时，`a_socketpair_end_is_not_listening` 在 macOS 上 panic：`rustix-0.38.44/src/backend/libc/net/read_sockaddr.rs:320:26: called Result::unwrap() on an Err value: Os { code: 22, kind: InvalidInput }`；换 1.1 后通过（§2.1）。`bound_but_not_listening_is_rejected` 只在非 Apple 目标编译，本机不运行（A.10）。

### A.2 J-S18 变异

```
# 精确相等 → contains
test result: FAILED. 0 passed; 3 failed
# RECALL_PRECHECK_MAX_PAGES 10 → 11
test clients::memory::tests::ten_pages_with_cursor_is_inconclusive ... FAILED
test result: FAILED. 2 passed; 1 failed
# 复原
test result: ok. 3 passed; 0 failed
```

### A.3 J-S1 正对照与绕过实验（v3，真 clippy 0.1.98）

```
$ c98 clippy --offline -p agent24-os-sdk --features positive-control -- -D warnings > pc3.log; echo $?
101
$ grep -c "use of a disallowed" pc3.log                                            → 26
$ grep -o "use of a disallowed [a-z]* \`[^\`]*\`" pc3.log | sort -u | wc -l         → 26
$ grep -c '{ path = ' sdk/clippy.toml                                              → 26

# sdk-bypass，v1 名单（6 项）：5 条（v2 附录写的 3 条是 b9/b10 加入实验前的旧读数）
error: use of a disallowed type `tokio::net::UnixStream`          @ bypass.rs:15:25   (b3 外部宏——调用处)
error: use of a disallowed type `tokio::net::UnixStream`          @ bypass.rs:17:1    (b4)
error: use of a disallowed type `tokio::net::UnixStream`          @ bypass.rs:18:25   (b4)
error: use of a disallowed type `std::os::unix::net::UnixStream`  @ bypass.rs:32:14   (b9)
error: use of a disallowed type `std::os::unix::net::UnixListener` @ bypass.rs:35:44  (b10 半)

# sdk-bypass，v3 名单（26 项）：15 条
error: use of a disallowed type `tokio::net::UnixSocket`               @ bypass.rs:6:17    (b1)
error: use of a disallowed method `serde_json::from_str`               @ bypass.rs:9:33    (b1)
error: use of a disallowed type `tokio::net::UnixStream`               @ bypass.rs:15:25   (b3)
error: use of a disallowed type `tokio::net::UnixStream`               @ bypass.rs:17:1    (b4)
error: use of a disallowed type `tokio::net::UnixStream`               @ bypass.rs:18:25   (b4)
error: use of a disallowed method `serde_json::Deserializer::from_slice` @ bypass.rs:20:27  (b5)
error: use of a disallowed method `serde_json::from_reader`            @ bypass.rs:20:105  (b5)
error: use of a disallowed type `std::net::TcpStream`                  @ bypass.rs:22:19   (b6)
error: use of a disallowed method `serde_json::Deserializer::new`      @ bypass.rs:26:17   (b7)
error: use of a disallowed method `axum::Json::from_bytes`             @ bypass.rs:30:27   (b8)
error: use of a disallowed type `std::os::unix::net::UnixStream`       @ bypass.rs:32:14   (b9)
error: use of a disallowed type `std::os::fd::OwnedFd`                 @ bypass.rs:35:12   (b10)
error: use of a disallowed type `std::os::unix::net::UnixListener`     @ bypass.rs:35:44   (b10)
error: use of a disallowed method `str::parse`                         @ bypass.rs:38:50   (b11)
error: use of a disallowed method `core::str::FromStr::from_str`       @ bypass.rs:40:48   (b12)
# 零报错的行：:13 b2（一跳别名 agent24_os_proto::Sock）、:42-47 b13（Json::from_request）、
#             :49 b14（File::open("/dev/fd/3")）、:51 b15（两跳别名 agent24_os_proto::Sock2）
# 用 Homebrew clippy 0.1.95 重跑 v1、v3 两份名单：读数与上面逐条相同
```

### A.4 J-S1b（v3：Python 脚本，fail closed）

```
$ python3 js1b.py proto fd                                          → J-S1b OK (2 module-side files scanned)   rc=0
$ python3 js1b.py <Agent24 bb3505a>/rust/crates/agent24-os-proto fd → J-S1b OK                                  rc=0
# 正对照（每条单独加、跑、复原）：
module.rs 加 `type Inner = tokio::net::UnixStream; pub type Sock2 = Inner;`
  proto/src/module.rs:363: pub type Sock2 = Inner;  [no `pub type` in proto module* / os-fd]                rc=1
module.rs 加 `pub use tokio::net::UnixStream as S;`
  proto/src/module.rs:361: … [`pub use` … only forwards crate-internal items] + [whole proto: … socket …]   rc=1
lib.rs 加 `#[macro_export] macro_rules! sneaky { … }`
  proto/src/lib.rs:161: #[macro_export]  [no #[macro_export] in proto or os-fd]                             rc=1
lib.rs（内核侧）加 `pub type K = tokio::net::UnixStream;`
  proto/src/lib.rs:161: pub type K = …  [whole proto: no pub alias/re-export naming a socket or fd type]    rc=1
fd/src/lib.rs 加 `pub type L = UnixListener;`
  fd/src/lib.rs:227: pub type L = UnixListener;  [no `pub type` in proto module* / os-fd]                  rc=1
python3 js1b.py proto nonexist → "J-S1b: no such dir nonexist/src"                                          rc=1
# 负对照：module.rs 加 `pub use crate::initialize::Offer as ReOffer;`                                       rc=0
# v2 的 rg 写法为何换掉：rg 在本机是 zsh 的 shell 函数，`sh` 脚本里 `rg: command not found`，
# 而 `! rg …` / `rg … && hit=1` 两种写法都会因此把 6 条正对照全部判绿。
```

### A.5 J-S2 / J-S3

```
# J-S2（v2 起，未变）
$ cargo metadata --format-version 1 --no-deps --offline | jq -r '.packages[] | select(.name=="agent24-os-sdk") | .dependencies[] | select(.kind==null) | .name' | sort | tr '\n' ' '
agent24-os-proto axum serde serde_json thiserror tokio tracing                    → PASS
# 正对照：临时加 agent24-domain（path 依赖）
agent24-domain agent24-os-proto axum serde serde_json thiserror tokio tracing     → FAIL（预期）

# J-S3（v3 脚本）
$ python3 ../js3.py                                          → J-S3 lints OK (1 allow site(s))   rc=0
$ (cd <Agent24 bb3505a>/rust && python3 …/js3.py)            → J-S3 lints OK (0 allow site(s))   rc=0
# 正对照（每条单独做、跑、复原）
proto/src/lib.rs 首行加 `#![allow(unsafe_code)]`                                            rc=1
proto/src/lib.rs 加 `#[allow(dead_code, unsafe_code)] fn _x() {}`                           rc=1
proto/src/lib.rs 加 `#[expect(unsafe_code)] fn _x() {}`                                     rc=1
proto/src/lib.rs 加 `#[cfg_attr(unix, allow(unsafe_code))] fn _x() {}`                      rc=1
fd/Cargo.toml `unwrap_used = "warn"`
  agent24-os-fd: restated lints differ from [workspace.lints] (besides unsafe_code): … rc=1
fd/Cargo.toml [lints.clippy] 多加 `dbg_macro = "deny"`                                      rc=1
删掉 proto 的 `[lints] workspace = true`（v2 已有）                                          rc=1
$ c98 check --offline -p agent24-os-sdk --features positive-control-unsafe
error: usage of an `unsafe` block
```

### A.6 Sin90 Unix socket / fd 子集 clippy 的正对照（迁移前树）

```
$ cd sin90-135ddb7 && CARGO_TARGET_DIR=../sin90-target rustup run 1.98.0 cargo clippy --offline --all-targets -- -D warnings; echo $?
101
   1 use of a disallowed method `std::os::fd::FromRawFd::from_raw_fd`
   1 use of a disallowed type `std::os::unix::net::UnixListener`
   2 use of a disallowed type `std::os::unix::net::UnixStream`
   8 use of a disallowed type `tokio::net::UnixListener`
  16 use of a disallowed type `tokio::net::UnixStream`
# 按文件：
   4 src/adapter_agent24/clients/test_support.rs
  14 src/adapter_agent24/mod.rs
  10 src/adapter_agent24/transport.rs
```

### A.7 J-S15 抽取数

```
$ rg -o --no-filename '"(/?_a24/[a-z_/]+|x-a24-[a-z-]+|\$/[A-Za-z]+|A24_[A-Z_]+)"' sdk/src proto/src | sort -u | wc -l
26                                     → ≥ 26 PASS
# 变异：RECENT 改成 concat!("_a24/memory/private/", "recent")
25                                     → FAIL（预期）
# v3（proto 模块侧拆到 module.rs、真 clippy 无关）重跑：仍为 26
```

### A.8 §5.2 第 3 条：测试模块期望补丁（H-1 → v4 H-A，`scratchpad/h1mig/`）

实验仓库：Sin90 `135ddb7` 的 `reconciler.rs` 与 `clients/model.rs` 作为 `origin/main`（commit `9b5ae23`，标 `base`）；期望补丁与 v3 相同（11 行，正文 §5.2 第 3 条所示，只改 `model.rs` 测试模块那一行 `use`）。

**v4 脚本**（`tests_of` 改锚点，H-A）：

```bash
#!/usr/bin/env bash
# Sin90 TS.1.1 acceptance §5.2 item 3 (ME4-S3 v4, H-1): the test
# modules of reconciler.rs and clients/model.rs differ from the
# pre-migration base by EXACTLY the committed expected patch, and that
# patch changes nothing but the one allowed `use` line.
#
# tests_of anchors on "#[cfg(test)]" IMMEDIATELY followed by "mod tests {"
# (not just any "#[cfg(test)]" line) and requires EXACTLY ONE such anchor
# per file; anything else (0 or >1 matches) is a hard failure (exit 3),
# not a silent pass. This matters because the real migration adds a
# second, unrelated "#[cfg(test)] use agent24_os_sdk::clients::
# RECALL_PRECHECK_MAX_PAGES;" line to the NON-test part of reconciler.rs
# (§5.1) — a naive "start printing from the first #[cfg(test)] line"
# extractor would latch onto that decoy instead of the real test module.
#
# This script is a one-time check for the TS.1.1 PR only (L-f): it is not
# meant to become a permanent CI job.
set -euo pipefail
base=${1:-origin/main}
exp=scripts/ts11-test-modules.expected.patch
allowed_minus='-    use crate::ai::UnavailableCause;'
allowed_plus='+    use agent24_os_sdk::UnavailableCause;'
tests_of() { awk 'p{print;next} prev=="#[cfg(test)]" && $0=="mod tests {" {n++; p=1; print prev; print} {prev=$0} END{if(n!=1) exit 3}'; }
out=$(mktemp); trap 'rm -f "$out"' EXIT
for f in src/adapter_agent24/reconciler.rs src/adapter_agent24/clients/model.rs; do
  old=$(git show "$base:$f" | tests_of) || { echo "anchor (#[cfg(test)] / mod tests {) not found exactly once in $base:$f"; exit 3; }
  new=$(tests_of < "$f") || { echo "anchor (#[cfg(test)] / mod tests {) not found exactly once in $f"; exit 3; }
  [ -n "$old" ] && [ -n "$new" ] || { echo "anchor matched but captured empty body in $f"; exit 3; }
  diff -u --label "a/$f (tests)" --label "b/$f (tests)" <(printf '%s\n' "$old") <(printf '%s\n' "$new") >> "$out" || true
done
# 1. the expected patch itself only swaps the one allowed line
changed=$(grep -E '^[-+]' "$exp" | grep -vE '^(\+\+\+|---) ' || true)
[ "$changed" = "$allowed_minus"$'\n'"$allowed_plus" ] || { echo "expected patch changes more than the allowed use line:"; echo "$changed"; exit 1; }
# 2. the actual diff equals it byte for byte
cmp -s "$out" "$exp" || { echo "test modules differ from the expected patch:"; diff -u "$exp" "$out" || true; exit 1; }
echo "test modules: only the allowed use line changed"
```

**三种情形实测**（`scratchpad/h1mig/`，base = commit `9b5ae23`）：

```
# ① 真实形状迁移：model.rs 测试模块改那一行 use；
#   reconciler.rs 非测试部分把 `const RECALL_PRECHECK_MAX_PAGES: usize = 10;`
#   换成两行诱饵 `#[cfg(test)]` + `use agent24_os_sdk::clients::RECALL_PRECHECK_MAX_PAGES;`，
#   `rustfmt` 之后（本例里 rustfmt 未改变格式，说明诱饵行本身已是规范形状）：
$ bash scripts/check-test-modules.sh 9b5ae23
test modules: only the allowed use line changed
rc=0
# v2/v3 的旧脚本（从第一个 #[cfg(test)] 开始截到文件尾）在同一份迁移上会把
# reconciler.rs 的诱饵行当成测试模块起点，比对出假阳性差异（H-1 当年即由此发现）。
# v4 的锚点要求「#[cfg(test)] 后一行恰为 mod tests {」，诱饵行不满足，被正确跳过。

# ② 在①的基础上再改动测试模块一处（reconciler.rs 测试模块内某个 use 换掉）：
test modules differ from the expected patch:
--- scripts/ts11-test-modules.expected.patch
+++ /tmp/xxx
@@ 期望补丁只有 model.rs 那一对 +/-；实际多出 reconciler.rs 测试模块的一对 +/- @@
rc=1

# ③ 锚点缺失：把 reconciler.rs 的 `#[cfg(test)]` 改成 `#[cfg(all(test))]`
#   （不再逐字匹配 "#[cfg(test)]"，紧跟的 `mod tests {` 也就配不上锚点）：
anchor (#[cfg(test)] / mod tests {) not found exactly once in src/adapter_agent24/reconciler.rs
rc=3
```

```
# 另有 4 条 v3 已实测的正对照，v4 复核结论不变（前三条落 rc=1，第四条落 rc=3）：
reconciler 测试模块某行末尾加 ` // touched`                                          rc=1
model 测试模块 `UnavailableCause::NoProvider` → `RequestRejected`                     rc=1
期望补丁多夹带一行（`use serde_json::{json, Map, Value};`）
  expected patch changes more than the allowed use line: …                           rc=1
model.rs 的 `#[cfg(test)]` 改成 `#[cfg(all(test))]`
  anchor (#[cfg(test)] / mod tests {) not found exactly once in …                     rc=3
```

### A.9 b2b（memory + `remember_once` 合并片）的 SZ-1 实测（M-4，`scratchpad/sz1-b2b/`）

base 提交放 SDK 骨架（`module.rs`/`lib.rs`/`clients/mod.rs`），分支提交加上 scratch 的 `clients/memory.rs`（含 `remember_once` 与预查、3 条单测）、`Module::memory()` 访问器、`clients` 的 `mod`/`pub use`/METHODS 一行、`lib.rs` 的转出，然后跑 PR-Daemon 的检查器：

```
$ bash ~/Dev/tools/PR-Daemon/scripts/pre-pr-check.sh --base main --json-only | jq .size
{"lines": 209, "files": 4, "inline_test_lines": 65, "test_file_lines": 0, "band": "normal",
 "limits": {"max_lines": 300, …}}
```

agentd `memory_callback.rs` 的对等测试全在 `#[cfg(test)]` 里，不计入。209 + 移植 Sin90 预查注释的余量 < 300。

### A.10 os-fd 的平台分支（M-1/M-2）

```
# macOS socket 状态探针（scratchpad/h1probe，libc getsockopt + rustix getpeername）
listening                          getpeername=Err(ENOTCONN 57)  SO_ACCEPTCONN=err Protocol not available (42)
fresh socket() no bind no listen   getpeername=Err(ENOTCONN 57)  SO_ACCEPTCONN=err (42)   ← 缺口
bound, not listening               getpeername=Err(ENOTCONN 57)  SO_ACCEPTCONN=err (42)   ← 缺口
socketpair end (peer alive)        getpeername=Ok                SO_ACCEPTCONN=err (42)
socketpair end (peer closed)       getpeername=Err(EINVAL 22)    SO_ACCEPTCONN=err (42)
client stream (server side closed) getpeername=Err(EINVAL 22)    SO_ACCEPTCONN=err (42)
client stream after shutdown(Both) getpeername=Err(EINVAL 22)    SO_ACCEPTCONN=err (42)

# EINVAL 映射的变异：apple 分支删掉 `| Err(rustix::io::Errno::INVAL)`
test tests::a_socketpair_end_with_peer_closed_is_not_listening ... FAILED
test result: FAILED. 6 passed; 1 failed

# Linux 分支（编译 + clippy，本机不能运行）
$ rustup target add x86_64-unknown-linux-gnu --toolchain 1.98.0
$ c98 clippy --offline -p agent24-os-fd --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s)
```


### A.11 scratch 行数

scratch 各文件非测试行数（含注释 / 纯代码），供 §7 估算：os-fd `lib.rs` 124/80（v3）；proto 桩 `lib.rs` 118 + `module.rs` 364（v3 拆文件；`module.rs` 里 `testing` 约 95）；SDK `clients/memory.rs` 195/147、`module.rs` 181/140（v3：`connect()` 接管监听器）、`error.rs` 154/143、`clients/scheduler.rs` 138/117、`clients/events.rs` 129/111、`fired.rs` 115/89、`clients/model.rs` 115/95、`clients/approval.rs` 108/81、`clients/mod.rs` 82/67、`context.rs` 65/52、`positive_control.rs` 33/30（v3：26 项）、`lib.rs` 25/21。

## 附录 B：承重文本（签名与 scratch 逐字一致；B.1 函数体以 `{ … }` 省略、文档注释有删节，B.2、B.3 为 scratch 原文逐字摘出的完整函数）

### B.1 proto `module` 桩（5.1.2a 的目标 API）

```rust
pub mod module {   // v3: its own file `src/module.rs` (J-S1b scans `src/module*`)
    /// Newtype, not `pub type` (J-S1b forbids every `pub type` here, v3 H-2).
    #[derive(Clone)]
    pub struct FatalHook(Arc<dyn Fn() + Send + Sync>);
    impl FatalHook {
        pub fn new(f: impl Fn() + Send + Sync + 'static) -> Self { … }
        pub fn fire(&self) { … }
    }

    /// The spawn variables a module that starts children should
    /// `Command::env_remove` (the SDK never mutates `environ`, §4.3).
    pub const SPAWN_ENV_VARS: [&str; 4] = [
        launch::ENV_LISTEN_FD, launch::ENV_CALLBACK_SOCK, launch::ENV_HANDSHAKE_TOKEN, launch::ENV_DATA_DIR,
    ];

    // os-fd cannot depend on proto (proto depends on it), so it restates
    // the two values; this is where they are held equal.
    const _: () = assert!(agent24_os_fd::LISTEN_FD == launch::LISTEN_FD);

    pub enum EnvError { Missing(&'static str) }

    /// Three of the four spawn variables. The listen fd is deliberately NOT
    /// here: only [`take_listener`] reads `A24_LISTEN_FD` (H1).
    pub struct ModuleEnv { data_dir: PathBuf, callback_sock: PathBuf, handshake_token: String }
    impl ModuleEnv {
        pub fn from_env() -> Result<Self, EnvError> { … }
        pub fn from_vars(mut get: impl FnMut(&str) -> Option<OsString>) -> Result<Self, EnvError> { … }
        pub fn data_dir(&self) -> &Path { … }
    }

    pub struct Hello<'a> {
        pub module: &'a str,
        pub manifest_bytes: &'a [u8],
        pub capabilities: &'a [&'a str],
        pub protocol_min: u32,
        pub protocol_max: u32,
    }

    /// Module-side parse of the handshake reply: same two fields as
    /// `initialize::InitializeResult` but WITHOUT `deny_unknown_fields` (H2).
    #[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
    pub struct InitializeReply { pub protocol_version: u32, pub offer: Offer }

    pub enum ConnectError { Io(std::io::Error), Refused { code: i64, kind: Option<String>, message: String }, Protocol(String) }
    pub struct RpcErrorInfo { pub code: i64, pub kind: Option<String>, pub message: String, pub data: Option<Value> }
    pub enum CallError { NotSent, ConnectionLost, Busy, FrameTooLarge, Timeout, IdCollision, Rpc(RpcErrorInfo) }

    pub struct CallOptions { pub response_timeout: Duration, pub slot_wait: Option<Duration> }
    pub const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(35);
    const _: () = assert!(DEFAULT_RESPONSE_TIMEOUT.as_secs() > crate::rpc::CALL_TIMEOUT.as_secs());

    /// No `notify`: nothing module→kernel uses one (L6).
    pub struct Connection { /* offer + mux internals */ }
    impl Connection {
        /// Dial, send `initialize`, parse the reply as [`InitializeReply`],
        /// require `protocol_version` within `hello.protocol_min..=hello.protocol_max`
        /// and no bytes after the reply, then spawn the mux tasks.
        pub async fn connect_from_env(env: &ModuleEnv, hello: &Hello<'_>, on_fatal: FatalHook)
            -> Result<Self, ConnectError> { … }
        pub fn offer(&self) -> &Offer { … }
        pub fn is_alive(&self) -> bool { … }
        /// Dropping the returned future before it resolves sends `$/cancelRequest`.
        pub async fn call(&self, method: &str, params: Value, opts: CallOptions) -> Result<Value, CallError> { … }
    }

    pub enum ListenError { Inherit(agent24_os_fd::InheritError), Io(std::io::Error) }
    /// Named struct, not an alias: the SDK keeps it in `Module` (v3 M-3).
    pub struct InheritedListener(tokio::net::UnixListener);
    impl InheritedListener { pub fn into_tokio(self) -> tokio::net::UnixListener { … } }
    /// Once per PROCESS: at most one test per test binary may reach this.
    pub fn take_listener() -> Result<InheritedListener, ListenError> { … }

    #[cfg(feature = "test-util")]
    pub mod testing {
        pub struct FakePeer { /* private */ }
        pub fn noop_hook() -> FatalHook { … }
        pub fn recording_hook() -> (FatalHook, Arc<AtomicUsize>) { … }
        pub async fn fake_kernel(offer: Vec<String>) -> (Arc<Connection>, FakePeer) { … }
        pub async fn read_request(peer: &mut FakePeer) -> Value { … }
        pub async fn respond(peer: &mut FakePeer, req: &Value, result: Value) { … }
        pub async fn respond_error(peer: &mut FakePeer, req: &Value, code: i64, kind: &str, message: &str) { … }
        pub async fn respond_error_with_data(peer: &mut FakePeer, req: &Value, code: i64, message: &str, data: Value) { … }
        pub struct FakeEndpoint { /* private */ }
        impl FakeEndpoint {
            pub fn bind(dir: &Path) -> (ModuleEnv, Self) { … }
            pub async fn accept_initialize(self, result: Value) -> (Value, FakePeer) { … }
        }
    }
}

pub mod manifest {
    /// LENIENT on purpose (no `deny_unknown_fields`): not validation (L4).
    #[derive(Debug, Clone, Deserialize)]
    pub struct ManifestFacts { pub name: String, pub route_namespace: String,
                               #[serde(default)] pub kernel_capabilities: Vec<String> }
    pub fn facts_from_yaml(yaml: &str) -> Result<ManifestFacts, String> { … }
    /// MOVED from `agent24-os-packages/src/discovery.rs:46-57` (M4).
    pub fn manifest_digest(bytes: &[u8]) -> String { … }
}
```

（scratch 里 `{ … }` 处是占位实现；`Debug for ModuleEnv` 的脱敏实现从略。`manifest_digest` 在 scratch 里是真实实现并有 `sha256("abc")` 单测。）

### B.2 `agent24-os-fd`（完整，scratch 实物）

```rust
static TAKEN: AtomicBool = AtomicBool::new(false);

pub fn take_inherited_listener() -> Result<UnixListener, InheritError> {
    if TAKEN.swap(true, Ordering::SeqCst) {
        return Err(InheritError::AlreadyTaken);
    }
    let raw = std::env::var(ENV_LISTEN_FD).map_err(|_| InheritError::Missing)?;
    let fd: RawFd = raw.parse().map_err(|_| InheritError::NotANumber)?;
    if fd != LISTEN_FD {
        return Err(InheritError::WrongFd(fd));
    }
    let owned = adopt(fd)?;
    rustix::io::fcntl_setfd(&owned, rustix::io::FdFlags::CLOEXEC)
        .map_err(|e| InheritError::Io(e.into()))?;
    let listener = UnixListener::from(owned);
    listener.set_nonblocking(true).map_err(InheritError::Io)?;
    Ok(listener)
}

/// The single `unsafe` site of the workspace. Validates through a borrow
/// first and only then claims ownership, so a wrong fd is never closed.
#[allow(unsafe_code)]
fn adopt(fd: RawFd) -> Result<OwnedFd, InheritError> {
    // SAFETY: `fd` is 3 (checked by the caller), inherited from the kernel
    // across exec; nothing in this process opened or owns it, and the
    // `TAKEN` swap guarantees this function runs at most once, so no second
    // owner can exist. The borrow does not outlive this call.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    validate(borrowed)?;
    // SAFETY: as above; validation proved it is the listening socket.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn validate(fd: BorrowedFd<'_>) -> Result<(), InheritError> {
    let st = rustix::fs::fstat(fd).map_err(|_| InheritError::NotASocket)?;
    if rustix::fs::FileType::from_raw_mode(st.st_mode) != rustix::fs::FileType::Socket {
        return Err(InheritError::NotASocket);
    }
    let local = rustix::net::getsockname(fd).map_err(|e| InheritError::Io(e.into()))?;
    if local.address_family() != rustix::net::AddressFamily::UNIX {
        return Err(InheritError::NotUnix);
    }
    let ty = rustix::net::sockopt::socket_type(fd).map_err(|e| InheritError::Io(e.into()))?;
    if ty != rustix::net::SocketType::STREAM {
        return Err(InheritError::NotStream);
    }
    if !is_listening(fd)? {
        return Err(InheritError::NotListening);
    }
    Ok(())
}

/// Linux (and every non-Apple target): `SO_ACCEPTCONN` is a direct answer —
/// true exactly when `listen()` has been called. CI compiles and tests this
/// branch on `ubuntu-latest` (J-S20).
#[cfg(not(target_vendor = "apple"))]
fn is_listening(fd: BorrowedFd<'_>) -> Result<bool, InheritError> {
    rustix::net::sockopt::socket_acceptconn(fd).map_err(|e| InheritError::Io(e.into()))
}

/// Apple declares `SO_ACCEPTCONN` but does not implement it (`ENOPROTOOPT`;
/// rustix gates it off), so this is a WEAKER, indirect check: "has no peer".
/// `getpeername` answers `ENOTCONN` for a listening socket — and ALSO for a
/// stream socket that was only `socket()`ed or `bind()`ed and never
/// `listen()`ed. KNOWN GAP (accepted, ME4-S3 §4.3 step 3): on macOS such a
/// socket passes validation; the kernel only ever hands fd 3 after
/// `listen()` (`launch.rs`), so meeting one means a forged environment, and
/// the failure then surfaces at `accept` (module exits), not as privilege.
/// A connected stream or live socketpair end has a peer (`Ok`); one whose
/// peer is gone answers `EINVAL` — both are "not listening". The test
/// `apple_gap_bound_but_not_listening_passes` pins the gap so a change in
/// either direction is noticed.
#[cfg(target_vendor = "apple")]
fn is_listening(fd: BorrowedFd<'_>) -> Result<bool, InheritError> {
    match rustix::net::getpeername(fd) {
        Err(rustix::io::Errno::NOTCONN) => Ok(true),
        Ok(_) | Err(rustix::io::Errno::INVAL) => Ok(false),
        Err(e) => Err(InheritError::Io(e.into())),
    }
}
```

### B.3 SDK `fired.rs` 的提取器（注册点签名见 §3.6）

```rust
impl<S: Send + Sync> FromRequest<S> for FiredDelivery {
    type Rejection = FiredRejection;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let h = req.headers();
        let fire_id = h
            .get(FIRE_ID_HEADER)
            .and_then(|v| v.to_str().ok())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or(FiredRejection::MissingFireId)?;
        let schedule_key = h
            .get(SCHEDULE_KEY_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let ctx = RequestContext::from_headers(h);
        let Json(body) = Json::<FiredBody>::from_request(req, state)
            .await
            .map_err(|e| FiredRejection::BadBody(e.body_text()))?;
        if !is_fixed_iso8601(&body.scheduled_for) || !is_fixed_iso8601(&body.fired_at) {
            return Err(FiredRejection::BadTimestamp);
        }
        Ok(Self {
            fire_id,
            schedule_key,
            body,
            ctx,
        })
    }
}
```
