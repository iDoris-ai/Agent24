# HW-TIERS — 能力 × 硬件分档：探测、声明、自动匹配

> 状态：**设计定稿候选（jason 10-14 已拍板 §10 五条）**｜2026-10-14｜分支 `design/hardware-tiers`
> 关联：`docs/agent/PLAN-DECIDE.md` §1.1（决策模型 T0–T3，本设计把它推广到全平台）、
> `iDoris/docs/07`（`HardwareAwareModelRecommender`）、`iDoris/docs/13` §3.1（修正后的可用内存公式）/ §6.1（RAM 矩阵）、
> `docs/design/INTEGRATION-AGENTEAR-IDORIS.md`（语音运行时归 AgentEar）。
> **本文所有体积 / 内存数字都是草案**：取自 iDoris 13（HF 字节数核验）或模型卡，定稿前必须按 §8 HW-5 在本机实测回填，不抄模型卡。

---

## 0. 一句话

**对外：一张公开的「能力 × 硬件」表，让用户知道每项能力要什么硬件；对内：用户不配任何东西——每个能力模块声明一条从好到差的「能力梯」，启动时由一个纯函数按本机预算自动挑选能跑的那一级，并且把挑了哪一级、为什么、差什么才能升级，原样告诉用户。**

## 1. 目标与原则

| # | 原则 | 含义 |
|---|---|---|
| P1 | **知情透明** | 能力不是「没有」，而是「你的硬件没到」。每项能力在用户机器上的状态必须是四种之一，并附原因：本地完整 / 本地降级 / 走云端 / 不可用。 |
| P2 | **零配置** | 首次运行自动探测、自动挑选、一次确认即安装推荐组合。设置页只提供「降档 / 关闭」，不要求用户懂模型。 |
| P3 | **降级自动，升级要确认** | 内存不够时自动降一级（不打断）；换机或加内存后只**推荐**升级，下载需要用户点头（沿用 PLAN-DECIDE §1.1「不自动升档下载」）。 |
| P4 | **不偷偷上云** | 降级链里的「云端」一级只在用户已启用云端且请求不是 `Privacy::LocalOnly` 时可选；否则落到「不可用」并说明。沿用 `agent24-models::router` 已有的隐私保证。 |
| P5 | **新能力必须先声明** | 任何新能力模块（内置或第三方 domain OS）合入前必须带硬件声明；没有声明的模块在 CI 里红。 |
| P6 | **档位给人看，预算给机器算** | 1–6 档是展示用的档位；真正的选择按「可用字节预算」做 admission。这样 18GB、36GB、48GB 这种非整档机器不会被硬塞进某一档而浪费或超载。 |

## 2. 现状盘点（已有什么、缺什么）

| 已有 | 位置 | 问题 / 缺口 |
|---|---|---|
| `HardwareProbe` / `HardwareProfile` / `SystemProbe`（sysinfo） | `rust/crates/agent24-decide/src/hw.rs` | ① 代码写在决策模型的 crate 里，只有决策服务会调用；daemon 启动时并不探测，其他模块也拿不到结果——要改成平台层、开机自动跑；② 磁盘只看了 cwd 所在的一块盘，要改为枚举全部内置盘和外接盘；③ `on_battery` 永远 `None`；④ 没取芯片型号、GPU 核数、GPU wired 上限、独显显存。②–④ 都有现成系统接口，见 §3.2，HW-1 直接补齐。 |
| `TierPolicy` T0–T3 + `DownloadConsent` | `agent24-decide/src/tier.rs` | 阈值只有 8/16/32 三条线，和 jason 10-07 拍板的 **8/16/24/32/64 五档**不一致；只看总内存，不扣系统保留和 App 自身开销。思路（纯函数、只能降不能升、consent 闸）是对的，直接沿用。 |
| `decide-models.catalog.json`（pin revision+sha256） | `agent24-decide` | 目录机制是对的，但只装决策模型。要推广成全平台的模型目录。 |
| `DomainOsManifest.requires_models: Vec<String>` | `agent24-domain/src/lib.rs` | 只是名字列表，没有内存 / 加速器 / 磁盘需求，也没有降级梯。 |
| `ModelRouter` 的 `Tier::{Local,Remote,Lora}` | `agent24-models/src/router.rs` | 这是**路由位置**，与硬件档位正交，不动。注意命名冲突：新概念不要再叫 `Tier`。 |
| 可用内存公式、RAM 矩阵、各档默认模型 | `iDoris/docs/13` §3.1、§6.1 | 理论账算得很细，是本设计的数字来源；但只覆盖 LLM，没覆盖语音、嵌入、沙箱、文档等 Agent24 自己的能力。 |
| `idoris-router/src/host_facts.rs` + `idoris-recommender` | iDoris 仓库 | iDoris 自己也有一个探测（只取总内存 + CPU 型号）和完整的 LLM 推荐器（footprint 公式、常驻 / 临时准入）。推荐器只依赖传入的 `HostFacts`、不碰 OS——正好可以吃 Agent24 的探测结果。分工见 §3.4。 |

**结论**：不新起炉灶。把 decide 里的探测和档位策略**提升为平台层**，扩展字段、修 bug、换成五档，再在其上加「能力梯声明 + 解析器」。

## 3. 硬件探测

### 3.1 什么时候测

| 时机 | 做什么 |
|---|---|
| 首次运行（安装后第一次启动 daemon） | 全量探测 → 写 `~/.agent24/hardware.json` → 触发首次推荐（§6.3）。 |
| 每次启动 | 轻量重测（几十毫秒）。机器指纹（芯片 + 总内存 + 独显）变了 → 视为换机，重新推荐。 |
| 运行中 | 订阅内存压力（macOS `DISPATCH_SOURCE_TYPE_MEMORYPRESSURE`，或周期读可用内存）和电源变化，供 §6.4 运行时降级用。 |
| 用户点「重新检测」 | 全量探测。 |

### 3.2 测什么（`HardwareProfile` v2）

| 字段 | macOS 来源 | Linux / Windows 来源 | 用途 |
|---|---|---|---|
| `total_mem_bytes` / `avail_mem_bytes` | sysinfo | sysinfo | 预算主变量 |
| `chip` | `sysctl machdep.cpu.brand_string`（如 `Apple M4`） | `/proc/cpuinfo` / WMI | 展示；速度档参考 |
| `unified_memory` | Apple Silicon = true | false | 决定按 RAM 还是按显存算模型预算 |
| `gpu_cores` | `system_profiler SPDisplaysDataType -json` | — | 展示；速度估计 |
| `gpu_wired_limit_bytes` | `sysctl iogpu.wired_limit_mb`（0 = 系统默认） | — | 用户调过上限时纠正预算 |
| `discrete_gpus[]`（型号、显存） | — | `nvidia-smi --query-gpu=name,memory.total --format=csv`；AMD/Intel 先记录不使用 | 独显预算；**有 CUDA 但显存 < 6GB 视同无加速器** |
| `accelerator` | Metal（aarch64） | Cuda / None | 沿用现有枚举 |
| `disks[]` | sysinfo `Disks`（全部挂载卷）+ `diskutil info -plist` 补内置 / 外接、SSD / HDD | sysinfo `Disks` + `/sys/block/*/removable` | **枚举本机全部磁盘**：内置盘和外接盘各列总量、余量、是否可移除、SSD/HDD、文件系统；过滤 APFS 快照、只读系统卷、网络卷（单独标出） |
| `models_dir_disk` | 模型目录所在的那块盘（最长挂载点前缀匹配） | 同左 | 安装判断用这块盘；它放不下而外接盘放得下时，建议把模型目录迁到外接盘（并提示：外接盘拔掉时这些能力不可用，解析器会按「模型缺失」自动降级） |
| `on_battery` | `pmset -g batt` 或 IOKit `IOPSCopyPowerSourcesInfo` | `/sys/class/power_supply` | 电池时上限降一级（沿用 T1 上限规则） |
| `os` / `os_version` | `sw_vers` | — | MLX 需要 macOS 14+；不满足时 oMLX 类梯级不可选 |
| `runtimes` | oMLX / Ollama / ORT 是否可用、版本 | 同左 | 梯级的运行时前置条件 |

外部命令全部带超时（≤ 2s）、失败不致命：拿不到的字段是 `None`，解析器对 `None` 一律按**保守**处理（例外：`on_battery = None` 不封顶，沿用现有语义，避免台式机被误降）。

### 3.3 预算（真正参与计算的数）

```
reserve(RAM)        = clamp(RAM × 0.30, 3GB, 16GB)            # 给系统，iDoris 13 §3.1
app_overhead        = 实测值（daemon + 桌面端，草案 1.0GB）      # iDoris 公式里没有，Agent24 必须扣
usable(RAM)         = min(RAM × 0.66, RAM − reserve) − app_overhead     # 默认 conservative
若用户调过 iogpu.wired_limit_mb：usable = min(usable, wired_limit − app_overhead)
独显机器：gpu_budget = VRAM × 0.90；cpu_budget 按上式另算（CPU 跑的小模型吃这一份）
```

| RAM | 档位 | usable（草案，扣 1GB App） | 建议 |
|---:|:---|---:|:---|
| < 8GB | **1 档** | — | 只用云端；本地只跑语音输入和规则 |
| 8GB | **2 档** | ~4.0GB | 系统本身已吃掉大半内存，**首次运行强烈建议开启云端**；本地 LLM 只作可选项，默认不装 |
| 16GB / 18GB | **3 档** | ~9.6 / 10.9GB | 本地日常可用，重活走云端 |
| 24GB | **4 档** | ~14.8GB | 本地主力 9B |
| 32GB / 36GB / 48GB | **5 档** | ~20.1 / 22.8 / 30.7GB | 深度模型需驱逐主力 |
| ≥ 64GB | **6 档** | ≥ 41.2GB | 主力与深度模型共存 |

对用户只显示「1 档 … 6 档」；代码里是 `HwLevel::L1..L6`，不叫 `Tier`，避免和路由的 `Tier`、决策模型的 `T0–T3` 混淆；`T0–T3` 改由档位推导（§7）。

### 3.4 谁来探测：Agent24 和 iDoris 怎么分工

两边都探测会出两个问题：重复代码；更要命的是**两个进程各自以为整台机器的可用内存都归自己**，Agent24 按 14GB 装了语音 + 决策 + 嵌入，iDoris 也按 14GB 装了 9B，合起来就爆了。所以原则是：**一台机器只有一个探测者、一本内存账**。

| 职责 | 归属 | 说明 |
|---|---|---|
| 探测硬件（`HardwareProfile`） | **Agent24**（`agent24-hw`） | Agent24 是每台用户机器上一定存在、开机即起的宿主，且语音、沙箱、磁盘等非 LLM 能力只有它关心。crate 不依赖 Agent24 其他部分，iDoris 可直接依赖。 |
| 整机内存账 / 预算分配 | **Agent24** | 先给 core 能力（语音、决策、嵌入）记账，再把剩下的划一块 `llm_budget` 给 iDoris。 |
| 在 `llm_budget` 内挑 LLM（常驻主力、按需深度、量化级别） | **iDoris**（`idoris-recommender`） | 它已经做好，Agent24 不重写。Agent24 的 `llm.*` 能力梯只是「问 iDoris」；iDoris 未安装时才用内置的静态梯（§4 表里那几行）。 |
| 能力 × 硬件公开表、用户可见的状态和原因 | **Agent24** | iDoris 的推荐结果以 `/capabilities` 的形式回来，Agent24 合并后统一展示。 |
| iDoris 单独部署在另一台机器（如网关跑在 Mac mini） | iDoris 自探 | 那是另一台机器，自然各探各的；它用同一个 `agent24-hw` crate，不再维护自己的 `host_facts.rs`。 |

接口：Agent24 → iDoris 传 `HostFacts`（由 `HardwareProfile` 投影出来）+ `llm_budget_bytes`；iDoris 返回 `Recommendation`。iDoris 09 号文档里「归属由 Agent24 定」那一条，就按这张表定。

**调试开关**：`A24_HW_BUDGET_CAP_GB=8` 让 64GB 笔记本按 8GB 的预算跑——这是在一台机器上验收五档体验的唯一现实办法（见 [[feedback_no_mac_mini_without_permission]]，不依赖别的机器）。

## 4. 公开的「能力 × 硬件」表（草案）

图例：✅ 本地完整｜◐ 本地降级（括号里是降级方式）｜☁ 需云端（用户已配 key / 订阅）｜✕ 不可用。
云端补位**默认关闭**：所有 ☁ 格只有在用户开启云端后才成立，否则即 ✕。1 档整列只有语音输入和规则是本地的。

| 能力 | 主要用到 | 2 档 8GB | 3 档 16GB | 4 档 24GB | 5 档 32GB | 6 档 64GB+ |
|---|---|:-:|:-:|:-:|:-:|:-:|
| 语音输入（ASR） | SenseVoiceSmall q8（~0.25GB）/ whisper-large-v3-turbo（~1.6GB） | ✅ SenseVoice | ✅ | ✅ +泰语 whisper | ✅ | ✅ +Qwen3-ASR |
| 语音输出（TTS） | Kokoro-82M（<0.5GB）/ CosyVoice3-0.5B（~1.6GB） | ✅ Kokoro | ✅ | ✅ +CosyVoice | ✅ | ✅ |
| 基础事务（发邮件、日程、格式转换、按清单执行） | MiniCPM5-2B Q4（~3.3GB @32K）| ☁ 推荐；◐ 可选本地 2B（默认不装） | ✅ | ✅ | ✅ | ✅ |
| 意图 / 决策模型（记住、召回门控…） | 方案 B：e5-small → base → large-instruct → Qwen3-Embedding-4B → 8B | ✅ e5-small | ✅ e5-base | ✅ e5-large | ✅ Q3E-4B | ✅ Q3E-8B |
| 记忆检索（向量召回） | bge-m3（~1.1GB fp16）| ◐ 关键词 + 小嵌入 | ✅ | ✅ | ✅ | ✅ |
| 日常对话 / 主力模型 | Qwen3.5-4B Q4（~6.2GB）/ Qwen3.5-9B Q4（~9.1GB）| ☁ / ◐ 2B 简答 | ◐ 4B | ✅ 9B | ✅ 9B | ✅ 9B Q8 |
| 文档摘要 / 改写 / 长文转换 | 主力模型 + 32K 上下文 | ☁ / ✕ | ◐ 4B 分段摘要（map-reduce，慢、质量降） | ✅ 9B | ✅ 9B 更长上下文 | ✅ |
| 文档文字提取（PDF / 网页转 Markdown） | 原生 PDFKit / 解析器，不用模型 | ✅ | ✅ | ✅ | ✅ | ✅ |
| 看图 / OCR | Qwen2.5-VL-3B（~2GB，按需）| ☁ | ◐ 按需加载、需先卸主力 | ✅ 按需 | ✅ | ✅ |
| 代码沙箱（BoxLite VM） | 微 VM 2–4GB | ✕ | ◐ 单个、小内存 | ✅ | ✅ | ✅ |
| 深度推理 / 本地 coding agent | Qwen3.8-27B Q4（~21.8GB）/ Qwen3.6-35B-A3B（~24.5GB）| ☁ | ☁ | ☁ | ◐ 27B 需驱逐主力 | ✅ 35B-A3B 与主力共存 |
| Open Design（可选组件，按需下载） | 见 OD 组件清单 | ☁ | ◐ | ✅ | ✅ | ✅ |
| 本地个人模型微调（LoRA） | 训练峰值内存 | ✕ | ✕ | ✕ | ◐ 小模型 | ✅ |

2 档（8GB）：系统和桌面端已占去大半内存，可用约 4.0GB。账面上「MiniCPM5-2B 3.3 + 语音输入 0.25 + 决策 0.1 ≈ 3.65GB」刚好塞得下，但没有任何余量，一开浏览器就会被挤。所以 2 档的默认方案是**语音输入 + 小决策模型本地跑，其余走云端**；首次运行页对 2 档用户单独强调「建议开启云端」。本地 2B 留作可选，HW-5 实测通过后才在 2 档推荐。

> 这张表**不手写**：由 §5 的能力目录生成（`agent24 hw table --markdown`），README / 官网 / 设置页三处同源，CI 校验生成结果与仓库里的副本一致。手写表一定会和真实行为漂移。

## 5. 能力声明：「能力梯」

### 5.1 概念

- **Capability（能力）**：用户可感知的一件事，如 `doc.summarize`、`voice.asr`。
- **Rung（梯级）**：实现这个能力的一种方式，带硬件需求和质量标签。一个能力有一条从好到差排好序的梯。
- **最后一级**永远是 `cloud`（受 P4 约束）或 `unavailable`（带给用户看的原因）。
- 模型本身只在**模型目录**里登记一次（沿用 decide catalog 的 pin revision + sha256 机制，推广为全平台 `models.catalog.json`）；梯级只引用模型 id，资源数字以目录里的实测值为准。

### 5.2 声明格式

内置能力写在 `capabilities/*.yml`；第三方 domain OS 在 `domain-os.yml` 里加同样结构的 `capabilities:` 段，取代今天只有名字的 `requires_models`（保留旧字段做兼容，解析时当作「无硬件信息、按最保守处理」并在模块管理页标黄）。

```yaml
# capabilities/doc-summarize.yml
id: doc.summarize
title: 文档摘要 / 改写
user_facing: true              # 进公开表
priority: on_demand            # core | resident | on_demand
rungs:
  - id: local-9b
    quality: full
    needs:
      models: [qwen3.5-9b-q4]          # 引用模型目录
      accelerator: [metal, cuda]
      min_usable_bytes: 10.5GB         # 草案；HW-5 实测后回填
      context_tokens: 32768
  - id: local-4b-chunked
    quality: degraded
    degraded_note: 分段摘要，长文质量下降、耗时约 3 倍
    needs:
      models: [qwen3.5-4b-q4]
      min_usable_bytes: 7GB
  - id: cloud
    quality: full
    needs: { cloud: true }             # 用户开启云端且请求非 LocalOnly
  - id: unavailable
    reason: 需要约 16GB 内存在本地运行，或在设置中开启云端模型
```

要点：
- `needs` 只写**需求**，不写「适用于哪一档」。档位列是解析器算出来的，能力作者不需要知道 1–6 档是什么——这样 18GB、36GB 机器也自然得到正确结果。
- 资源数字有两个来源，按优先级：本机实测缓存 > 模型目录实测值 > 声明值。声明值只是兜底。
- `priority: core` 的能力（如意图判断、语音输入）在预算里先占位，不会被别的能力驱逐。

### 5.3 准入规则（CI）

1. 每个 `user_facing` 能力至少一个本地梯级 + 一个终止梯级（`cloud` / `unavailable`）。
2. 梯级里引用的每个模型都在模型目录里、且已 pin。
3. 能力目录生成的公开表与仓库副本一致。
4. 新加的模型在 HW-5 的实测表里有一行，否则只能标 `measured: false`，设置页显示「估算值」。

## 6. 自动匹配

### 6.1 解析器（纯函数）

```
resolve(hw: HardwareProfile, budget: Budget, caps: [Capability], catalog: ModelCatalog,
        prefs: UserPrefs /* 降档、关闭、是否允许云端 */, installed: InstalledSet)
  -> Plan { per_capability: [(cap_id, rung_id, status, reasons[], upgrade_hint)],
            resident_set, total_download_bytes }
```

和 `TierPolicy::decide` 一样：无 I/O、无时钟，用 `FakeHardwareProbe` 在 8/16/18/24/32/36/48/64/96GB、有无加速器、电池、磁盘不足等边界上做表驱动测试。

算法（够用即可，不做最优化求解）：
1. 过滤：去掉加速器、OS、运行时、磁盘不满足的梯级。
2. 先给 `core` 能力挑「能放下的最高梯级」，扣预算；再给 `resident`；`on_demand` 不扣常驻预算，只检查「单独加载时能否放下」，放不下但驱逐非 core 后能放下的标 `requires_eviction`（即表里的「需驱逐主力」）。
3. 用户偏好只能**往下**压（降档 / 关闭），不能把解析器判为放不下的梯级强行打开（同 `TierPolicy` 的「覆盖只能降档」）。
4. 每个结果带两段人话：`reasons`（为什么是这一级）和 `upgrade_hint`（「再多约 6GB 可用内存可本地运行 9B」/「开启云端可用完整版」）。

### 6.2 运行时（调用那一刻）

解析结果是**计划**，不是保证。调用能力时再做一次轻量准入：
- 读当下可用内存；不够 → 先按 LRU 卸载非 core 的按需模型 → 仍不够则本次**临时降一级**执行。
- 降级要在响应元数据里带上 `served_rung` 和原因，聊天界面显示一个小标记（如「本次用了轻量模型：内存紧张」）。这是 P1 的运行时版本，和 `Served.tier` 的思路一致。
- 电池 / 高温：只影响按需大模型的加载，不打断已在跑的任务。

### 6.3 首次运行的体验

```
安装完成 → 第一次启动
  → 探测（1–2 秒）
  → 一页「你的电脑能做什么」：
       本机：Apple M2 · 16GB（3 档）· 可用约 9.6GB · 内置盘剩 180GB，外接盘 1 块剩 820GB
       ✅ 语音输入、基础事务、意图判断、记忆检索
       ◐ 文档摘要（分段，较慢）  ◐ 看图（需临时卸载主力）
       ☁ 深度推理（开启云端后可用）
       [查看完整对照表]
  → [一键安装推荐组合（约 6.6GB，示意）]   [稍后]   [只用云端]
```

只有一个决定要用户做：同不同意下载（沿用 `DownloadConsent`）。不问选哪个模型。云端补位默认不勾选；1、2 档用户这一页把「开启云端」放在首位并说明原因。

### 6.4 硬件变了

- 指纹变化或持续内存压力 → 重新 resolve。
- 结果**降级**：直接生效，通知一次。
- 结果**可升级**：设置页 + 一次性提示「检测到 64GB 内存，可本地运行深度推理（需下载约 24GB）」，用户确认才下载。

## 7. 与现有代码的关系

| 现有 | 处理方式 |
|---|---|
| `agent24-decide::hw` | 迁到新 crate `agent24-hw`（探测 + 预算 + 解析器），daemon 启动时调用，decide 和其他模块都读它的结果；字段扩展、枚举全部磁盘、补电池。`FakeHardwareProbe` 保留。 |
| `agent24-decide::tier`（T0–T3） | T0–T3 是决策服务自己的四级（T0 只用规则 / T1 CPU 小编码器 / T2 编码器 + 0.6B 风险模型 / T3 再加 4B 深度层），今天它自己看总内存定级（<8GB→T0，8–16→T1，16–32→T2，≥32→T3，无加速器或电池→最多 T1）。改为不再自己看硬件，而是读平台档位：1 档或未同意下载 → T0；2 档 → T1；3、4 档 → T2；5、6 档 → T3。阈值和今天一模一样，所以决策结果不变，只是全平台只剩一套阈值。决策模型五档选型（方案 B）后续写成 `decide.*` 能力梯，T0–T3 最终只留作日志字段 `hw_tier`。 |
| `decide-models.catalog.json` | 推广为 `models.catalog.json`，结构不变（pin + 实测 `measured[]`），新增 `role`（asr/tts/embed/llm/vision）。 |
| `DomainOsManifest.requires_models` | 新增 `capabilities:` 段；旧字段兼容一个版本后废弃。 |
| `ModelRouter`（Local/Remote） | 不动。解析器决定「本地用哪个模型」；路由器仍负责「本地还是云端、隐私是否允许」。`cloud` 梯级就是交给路由器的 Remote。 |
| AgentEar（语音运行时） | 运行时仍归 AgentEar；AgentEar 用同一 schema 向 Agent24 声明 `voice.*` 能力梯，Agent24 只做预算和展示，不托管它的模型。 |
| iDoris `idoris-recommender` / `host_facts.rs` | 按 §3.4：LLM 选型交给 `idoris-recommender`，Agent24 只给它 `HostFacts` + `llm_budget`；iDoris 的 `host_facts.rs` 改用 `agent24-hw`（iDoris 侧任务，另开 issue）。 |
| 新增 API / CLI | `GET /api/v1/hardware`（探测结果 + 预算）、`GET /api/v1/capabilities/plan`（解析结果）、`agent24 hw probe|plan|table`。 |

## 8. 落地拆分（按少拆原则，6 片）

| 片 | 内容 | 验收 |
|---|---|---|
| HW-0 | 本文定稿（jason 回答 §10） | 拍板记录 |
| HW-1 | `agent24-hw` crate：探测 v2（含全部磁盘）+ 预算 + `A24_HW_BUDGET_CAP_GB`；daemon 启动时探测并落盘；`agent24 hw probe`；decide 改读平台档位 | 本机输出与 `system_profiler` / `diskutil list` 对得上；接一块外接盘能列出；decide 现有档位测试全部不改仍通过 |
| HW-2 | 能力梯 schema + 模型目录推广 + 解析器 + CI 准入 + 公开表生成 | 9 种内存 × 加速器 / 电池 / 磁盘边界的表驱动测试；生成的表与 §4 一致 |
| HW-3 | `/api/v1/hardware`、`/capabilities/plan`；对接 `idoris-recommender`（§3.4）；桌面端首次运行页 + 设置页「本机能力」 | 本机截图；`BUDGET_CAP=8/16/24/32` 四张截图 |
| HW-4 | 运行时准入 + 临时降级 + `served_rung` 透出 | 人为压内存时聊天显示降级标记，任务不失败 |
| HW-5 | 实测回填：在 64GB 笔记本上用预算上限模拟各档，测每个梯级的常驻 / 峰值内存、P95 | 实测表进 `docs/research/`，目录里 `measured: true`，§4 草案数字替换为实测 |

HW-1、HW-2 可并行；HW-5 可与 HW-3 并行。

## 9. 不做什么

- 不做模型组合的最优化求解（贪心 + 优先级够用）。
- 不做 GPU 跑分；速度只按芯片代际给粗估，标「估算」。
- 不自动改 `iogpu.wired_limit_mb`（要 sudo、有不稳风险）。
- 不替用户开云端。
- 首版不支持 AMD / Intel 独显推理（只探测、只展示）。

## 10. 已拍板（jason 2026-10-14）

1. **云端补位默认关闭**。
2. **8GB 及以下**：支持，但首次运行明确建议开启云端——系统本身就吃掉大半内存，本地只适合语音输入、规则和小决策模型（§3.3、§4）。
3. **档位命名**：只用数字「1 档 … 6 档」（1 = <8GB，2 = 8GB，3 = 16GB，4 = 24GB，5 = 32–48GB，6 = 64GB+），不用「基础 / 标准」这类词。
4. **decide 的 T0–T3**：按建议在 HW-1 改为读平台档位（映射见 §7，阈值不变、行为不变）。
5. **平台范围**：探测先做全（含 NVIDIA 显存）；解析器首版只对 Apple Silicon（Metal）给本地 LLM 梯级，CUDA 下 LLM 用保守估算。
6. **谁来探测**：按 §3.4，Agent24 探测并管整机内存账，iDoris 在分到的预算内挑 LLM。
