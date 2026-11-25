# KB-LITE-01 精简版服务端制品实测记录（2026-11-26）

> Loop 8 第 3 项。本轮只做服务端托管档、固定 SHA 制品和实测，App 不接线（David 11-26 确认，`knowledge-materials/plan/loop-8-draft.md`「已定的范围」第 1 条）。依据：`docs/knowledge/PLAN-KNOWLEDGE-TASKS.md` WS4（KB-LITE-01a/01b）、D6、§6；WeKnora `docs/agent24/CAPABILITY-MATRIX.md` #15；`CONTRACT-READONLY-v1.md` R1.4。
> 代码基线：WeKnora fork `iDoris-ai/WeKnora` `a24/develop`：托管档 #177（合入 46a882c6，PR-Daemon APPROVE）、制品入口 #178（合入 308cc352）、M5 修复 #179（a24/kb-lite-01c-m1cpu）。实测制品源码 **70c8d1d2f8cc**（a24/develop 46a882c6 加 #179），两台机器用的是同一个签名制品。
> **建议结论：按需下载**；本 Loop **不追加** KB-LITE-02 接线（理由见 §5）。David 在 §7 确认后才算收口。

## 1. 摘要

- **制品**：darwin/arm64 无头 Lite，归档 **78.5 MiB**（82,272,517 字节），解压后 **263 MB**，其中程序 248 MB、jieba 词典 13 MB，许可证、config、迁移合计约 1 MB。不带前端。
- **启动快**：进程就绪（握手第一行）mini 0.31–0.32 秒，MacBook 0.30–0.34 秒；全新状态下第一次启动（含建库迁移，也是这个二进制第一次执行）mini 1.98 秒，MacBook 4.21 秒。
- **Lite 本身占的内存不大**：空闲 RSS mini 348–358 MB，MacBook 184 MB（第一轮 100 MB）；导入 20 份 md/txt 的峰值 mini 366 MB、MacBook 193 MB。MacBook 上测到的最高值 352 MB 出现在冷启动阶段。embedding 模型的内存不算在这里，算在模型服务那边。
- **embedding 经 iDoris 网关打通（mini）**：Lite 的内置 embedding 指向本机 iDoris 网关的 `/v1/embeddings`（后端是 bge-m3），带上 iDoris virtual key 后，20 份全部 completed，纯向量检索有命中。需要两项配置：`SSRF_WHITELIST=127.0.0.1`，以及一把 virtual key。MacBook 上 embedding 直连 oMLX（bge-m3-mlx-fp16），同样 20/20 completed。
- **托管档的四条负例在两台机器的真实制品上都成立**：只监听 127.0.0.1；`/auth/register` 返回 403；三次重启密钥不变；日志里查不到 setup token 和 JWT。
- **MacBook 平衡档的关键发现与 Lite 无关**：平衡档下 oMLX 直接拒绝加载日常模型（`507: Model 'Qwen3-8B-4bit' (4.51GB) does not fit under the dynamic memory ceiling (3.38GB)`），预热请求和 3 次对话全部是 507。测试窗口内 oMLX 日志里 Memory pressure 和 Aborted 都是 0 行，系统空闲内存从 42% 降到 35%。所以「平衡档下 Lite 和本地模型同住不 abort」这一条**测不出来**：没有 abort，是因为日常模型根本没加载上，不是因为两者能同住。
- **实测中修了一个阻塞问题**：第一版制品在 M5 上一启动就 SIGSEGV（go-m1cpu v0.1.6 在包 init 时调 cgo），由 #179 修复，见 §4。

## 2. 方法

| 项 | 做法 |
|---|---|
| 机器 | mini：Mac-mini-Nicolas，Apple M4、16 GB，不跑 oMLX，embedding 由本机 ollama 的 bge-m3 提供。MacBook：David 的机器，Apple M5 Pro、24 GiB、macOS 27.0，iDoris/oMLX 正在运行，测试时切到**平衡档**，测完恢复激进档。MacBook 那次由监工照 `kb-lite-eval/README-mac.md` 执行，编码会话没有连他的机器。 |
| 制品 | 用 #178 的 `scripts/agent24/build-lite-headless.sh 70c8d1d2… --sign-key <一次性 key>` 构建；两台机器都用 `verify-lite-headless.sh` 验签，输出 `OK 70c8d1d2… darwin/arm64`；换一把无关的公钥，验签报 `ERROR: signature`。 |
| 拉起 | `kb-lite-eval/run_lite_eval.py`：一次性工作目录，`ENV_FILE` 指向临时 env（SQLite、本地存储、内存队列、`GIN_MODE=release`、`LOG_LEVEL=info`、`SSRF_WHITELIST=127.0.0.1`），命令 `WeKnora-lite --managed --state-dir <work>/state --port 0 --handshake-fd 3`，握手经 pipe 读取。同一个 state-dir 连续冷启动 3 次，第一次含建库迁移。 |
| 计时 | 进程就绪 = exec 到读到握手第一行；模型就绪 = 读到第二行（embedding 实际调用一次）；health = `/health` 返回 200。 |
| 内存 | `ps -o rss=` 每 0.25 秒采样一次；空闲 = 就绪后静置 20 秒时的值；导入峰值 = 从导入开始到全部解析完成后 2 秒内的最大值。 |
| embedding | mini：iDoris 网关（`idoris` 路由二进制，iDoris main 792ed90，release 构建）起在一次性端口，组件卡只有一个本机 embedding（ollama bge-m3，resident），key 库放在一次性目录，用 `idoris key issue --spec-stdin` 签一把 local_only 的 virtual key；Lite 的 `builtin_models.yaml` 写 `base_url: http://127.0.0.1:<网关端口>/v1`、`api_key: ${LITE_EVAL_EMBED_KEY}`，维度 1024。MacBook：直连 oMLX 的 `/v1`（带 oMLX API key），模型 bge-m3-mlx-fp16，用的是和 App 同一个模型服务，这样同住时的内存才真实。 |
| 导入 | 用握手里的 setup token 调 `POST /api/v1/auth/auto-setup` 拿 JWT，建一个用内置 embedding 的库，逐个上传 20 份文档（15 份 md、5 份 txt，共 463 KB，取自本仓库 `docs/knowledge` 和 `docs/documenting`，清单见 `kb-lite-eval/docs-list.txt`），轮询到全部 completed 或 failed，再做一次只走向量的 hybrid-search。 |
| 对话负载（MacBook） | 导入开始时，在另一个终端向 oMLX 发 3 次 Qwen3-8B-4bit 对话，测前先预热一次；测试窗口内统计 oMLX 日志里的 `Memory pressure` 和 `Aborted` 行数。 |

凭据（setup token、JWT、virtual key、oMLX key）只放在脚本内存和 600 权限的一次性文件里，不写进输出。测试实例只用系统分配的临时端口和一次性目录，测完已删除；演示栈 18280、18282、18283 没有动。

## 3. 结果

### 3.1 制品

| 项 | mini | David MacBook |
|---|---|---|
| 归档（tar.gz） | 82,272,517 字节（78.5 MiB） | 同一文件，sha256 一致，验签 OK |
| 解压后 | 263 MB（程序 248 MB、dict 13 MB、其余约 1 MB） | 268,960 KB |
| 按 SHA 构建（mini） | 约 42 秒 | — |

### 3.2 启动与内存

| 项 | mini（M4，3 次） | David MacBook（M5 Pro，平衡档，3 次） |
|---|---|---|
| 进程就绪（秒） | 1.98 / 0.31 / 0.32 | 4.21 / 0.30 / 0.34 |
| 模型就绪行（秒） | 2.03 / 0.34 / 0.35，embedding `ready` | 6.05 / 0.48 / 0.53，embedding `ready` |
| 空闲 RSS（MB） | 357.9 / 348.3 / 348.3 | 99.8 / 184.0 / 184.4 |
| 导入 20 份耗时 | 53.5 秒，20/20 completed | 9.2 秒，20/20 completed |
| 导入峰值 RSS（MB） | 366.3 | 192.7（导入前冷启动阶段测到的最高值是 351.7） |
| 纯向量检索 | 200，6 条命中 | 200，6 条命中 |
| 同时段日常模型对话 | 不适用 | 预热和 3 次全部 507（模型 4.51 GB，超过平衡档动态上限 3.38 GB），每次约 1 秒 |
| oMLX Memory pressure / Aborted | 不适用 | 0 / 0 |
| 系统空闲内存 | — | 42% → 35% |

原始输出：`kb-lite-eval/result-mini.jsonl`、`kb-lite-eval/result-mac.jsonl`。MacBook 的导入快很多，是因为 embedding 直连同机 oMLX 的 MLX 版 bge-m3；mini 上多绕了一层网关，后端是 ollama。两边的耗时不能直接比较。macOS 会压缩不活跃进程的内存，所以 RSS 读数随时间波动；MacBook 的空闲值低于 mini，不代表 Lite 在 M5 上更省内存。

### 3.3 负例（真实制品，两台机器）

| 负例 | 结果 |
|---|---|
| 默认不监听外网 | 三次握手的 origin 都是 127.0.0.1；从本机非回环 IPv4 连同一端口被拒 |
| 注册入口关闭 | `/auth/register` 返回 403，`1002 Public registration is disabled` |
| 握手 token 不出现在日志 | 工作目录下全部日志和 stdout/stderr 文件里，setup token 和 JWT 都是 0 处（MacBook 上监工另外 grep 了一遍，结果相同） |
| 重启后密钥不变 | 三次冷启动，`managed-secrets.json` 内容一致 |

### 3.4 embedding 经 iDoris 网关

- **能跑通**。配置是：Lite 的 `builtin_models.yaml` 写一条 `type: Embedding`、`source: remote`、`provider: generic`、`base_url: http://127.0.0.1:<网关端口>/v1`、`api_key: <iDoris virtual key>`、`dimension: 1024`；Lite 进程环境加 `SSRF_WHITELIST=127.0.0.1`。
- 中途碰到两处拒绝，都是预期的安全行为，不是 bug：① WeKnora 的 SSRF 校验拒绝 127.0.0.1 的模型地址（`hostname 127.0.0.1 is restricted`），需要加白名单；② 网关对空 key 或无效 key 回 401 `VIRTUAL_KEY_UNAUTHORIZED`。
- 接线时要定：virtual key 由谁签、给哪些角色。本次为了省事给了全部角色，只允许 local_only。Agent24 托管的 iDoris 已有 embed 角色（`rust/crates/agent24-idoris/src/idoris_host.rs:1056`），宿主签一把只含 embed 的 key 交给 Lite 就行，Lite 请求里的模型名要和网关能路由的名字一致。MacBook 那次没走网关：App 的 virtual key 在钥匙串里，不在 David 的机器上另签 key。

## 4. 发现的问题（已在 PR 里修）

| 问题 | 怎么发现的 | 处理 |
|---|---|---|
| 按 `-trimpath` 构建的制品一启动就 panic（gojieba 找不到词典） | #178 第一次真构建 | 制品自带 `dict/`；程序在没设 `JIEBA_DICT_DIR` 时去 `<程序目录>/dict` 找；构建脚本在无关目录跑一次 `--version` 自证 |
| 宿主没传 fd 3 时，同号 fd 可能是本进程打开的其他文件，握手会把 token 写进去 | mini 第一次拉起时报 `write agent24-handshake: device not configured` | 握手 fd 只接受 pipe/socket，否则拒绝启动；加了测试和反面对照（#177） |
| OIDC 首次登录建号不检查注册模式 | Codex 评审 #177 | 托管档设 `OIDC_AUTH_ENABLE=false` |
| **M5 上一启动就 SIGSEGV**：`go-m1cpu@v0.1.6 init.0 → _Cfunc_initialize`，这时还没进 main | MacBook 第一轮实测 | 依赖链是 milvus SDK → gopsutil/v3/cpu → go-m1cpu。#179 把它升到 v0.2.4（改为首次调用时才初始化，并支持 M5），构建脚本遇到链入 v0.1.x 的制品就拒绝出包；第二轮 MacBook 实测通过 |

## 5. 结论

**建议：按需下载。本 Loop 不追加 KB-LITE-02 接线。**

1. **不随包**。归档 78.5 MiB、解压 263 MB，随包会让 App 安装包明显变大，而用户目前看不到精简版（本轮不接线）。已定的方向也一致：fork 制品默认不发布（PLAN D6）、Lite 走按需下载（KB-LITE-03a）、文档引擎一律按需下载不进安装包（`ADR-DOC-01-placement-and-integration.md:158`）。实测数据没有压倒性地支持随包：启动快、Lite 本身内存不大，但这两点对按需下载同样成立。
2. **按需下载在技术上可行**。按精确 SHA 构建、签名 manifest 绑定 SHA/架构/许可证、验签脚本都已就绪（#178），两台机器验签都通过；第一次启动 2–4 秒，之后 0.3 秒左右，下载后首次使用不会有明显等待。
3. **接线的两个条件，一个满足、一个测不出**：
   - ②「embedding 经 iDoris 打通」：**满足**（mini，§3.4）。
   - ①「平衡档下 Lite 和本地模型同住时不触发 oMLX 内存 abort」：**测不出来**。在 David 当前的内存占用下，平衡档的动态上限是 3.38 GB，比日常模型 Qwen3-8B-4bit（4.51 GB）还小，所以不管 Lite 在不在，日常对话都会以 507 拒绝加载。Lite 本身空闲 100–360 MB、导入峰值 190–370 MB，还会再占掉一部分余量。这一条要改写成「平衡档下日常模型以 507 拒载（与 Lite 无关）」，同住的结论只能在下面两种情况之一出现后再测：激进档下实测，或者平衡档的上限能放下日常模型。
   因此本 Loop 不追加接线，接线放到 Loop 9，前提是 §6 的闸门和上面条件①的重测。
4. **为什么不选「等条件」**：要等的是接线条件，不是交付形态。制品形态（按 SHA 构建、签名、按需下载）这轮的证据已经够用；把结论写成「等条件」，会让 KB-LITE-03a（按需下载）连方向都定不下来。

## 6. 接线前的闸门（PR-Daemon 在 #177 给出，KB-LITE-02 开工前要满足）

- **G1** 宿主必须显式传入握手 fd 并设 close-on-exec，不能只靠 EOF 判断。
- **G2** 一个 state dir 只起一个实例；密钥文件改为原子写入并 fsync（FU-241）。
- **G3** 宿主把 `DB_PATH` 和本地存储都放进 state dir；不要随包带含密钥的 `.env.lite`（Lite 会自动加载它）。
- **G4** setup_token 在整个进程生命周期内都是管理员凭据，考虑改成单次使用或轮换（FU-242）。
- **G5** 回环端口加 Host/Origin 白名单，防 DNS rebinding（FU-242）。
- **G6** 核对 integration 模式与托管档同时开启的组合：integration 白名单里没有 auto-setup。
- 另加：平衡档能放下日常模型，或在激进档下重测同住（§5 第 3 条）。

## 7. David 确认

（待填：是否同意「按需下载」，以及本 Loop 不追加接线）

## 8. 留下的事（FU，已登记在 `docs/agent/followups.md`）

- FU-241：托管档密钥文件改为原子写入（先校验环境变量，再写临时文件、fsync、rename）；握手 fd 设 close-on-exec；修正 `OpenHandshake` 拒绝路径的注释（#177 C2、N1–N4）。
- FU-242：setup_token 改成单次使用或可轮换；回环端口加 Host/Origin 白名单（#177 N6、G4、G5）。
- FU-243：模型就绪只在启动时探测一次，chat 只看状态不实际调用；接线时宿主改用 `/agent24/knowledge/ready` 持续探测。
- FU-244：`SSRF_WHITELIST` 只能按主机放行，放行 127.0.0.1 等于放行本机所有端口；本机单用户实例下可以接受，多用户或远端部署前要改成按端口放行。
- FU-245：制品的生产签名密钥由谁保管、`AGENT24_LITE_SIGNING_KEY` 何时配置；归档不能逐字节复现；只验证了 darwin/arm64；Lite 只内置 Simple 解析，PDF/DOCX 的解析能力还没测。
