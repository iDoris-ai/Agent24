# 08 评审抓到的真实缺陷（工程实践素材）

> 出处：各 PR 的评审意见（`gh api repos/iDoris-ai/Agent24/pulls/<N>/reviews`）与作者回复（`gh pr view <N> --comments`）。评审方为 PR-Daemon（GitHub 账号 clestons），多轮管线：R1a/R1b DeepSeek、R2 Opus 独立读 diff、R3 Codex 交叉验证、R4 Opus 终裁（轮次说明见各评审末尾）。
> 共同点：这些缺陷都发生在**零调用点**的 D0 契约代码里，今天没有产品影响；评审仍把其中三条判为阻塞，理由是「本 PR 交付的就是契约」，D1 会直接以这些不变量为前提接线（#688 第 1 轮评审原话）。

## 总表

| # | PR | 严重度 | 一句话 | 锁住它的测试 |
|---|---|---|---|---|
| 1 | #688 | Medium（阻塞） | 后端谎报自身类型，就能把模拟概率标成「已校准」 | `decide_normalizes_calibrated_by_the_backends_real_kind_not_its_self_report` |
| 2 | #688 | Medium（阻塞） | 阈值反序列化绕过校验；写反的阈值让「交人」区间整段变成「执行」 | `inverted_order_json_is_rejected_not_silently_accepted` 等 + `compile_fail` doctest |
| 3 | #688 | 文档（阻塞） | ADR 与模块文档描述的「携带上一层结论」路径是死代码 | `unavailable_layer_carries_the_last_floor_forward` 等 |
| 4 | #689 | Medium（阻塞） | 模型目录「pin」只查非空，`revision: "main"` 也算 pin | `branch_tag_or_short_sha_revision_is_rejected`、`non_64_hex_sha256_is_rejected` |
| 5 | #689 | CI 抓到 | 下载同意变量没进 LaunchAgent 透传清单，daemon 永远报 T0 | `passthrough_list_matches_what_the_daemon_actually_reads` |
| 6 | #718 | 方法论（非阻塞，强烈建议） | 训练集 / 评测集近义模板泄漏，逐字检查查不出 | `tests/test_overlap.py`（#722） |
| 7 | #717 | 非阻塞 forward-note | CLI 首次获得迁移权，新旧版本错位会让旧 daemon 起不来 | 未加测试（判定为既有风险类别） |

---

## 1. `calibrated` 不变量其实要靠后端自觉（#688 M1）

**问题**：设计约束是「LLM 模拟的结果永远不能报 `calibrated = true`」。`Answer::new(.., backend: BackendKind, calibrated)` 里确实会在 `backend == LlmSimulation` 时强制改成 false——但 `backend` 是**自由参数**，由后端在自己的 `evaluate()` 里传；`DecisionService` 从不拿它和后端的 `kind()` 核对。文档（`types.rs`、ADR-033 第 6 条）却写着「不是靠调用方自觉遵守的约定」。

**怎么发现的**：评审用一个独立 crate 以 path 依赖本 PR head 实跑反例：后端 `kind()` 返回 `LlmSimulation`，`evaluate()` 里用 `BackendKind::Encoder` 构造答案，走完 `decide()` 输出 `backend=LlmSimulation calibrated=true`。

**为什么危险**：D1 会把阈值三段建立在「calibrated 概率」上。一个 LLM 模拟后端（出 bug 或被恶意实现）就能让它编出来的 0.97 当成校准概率去触发「执行」。

**怎么修**：真正的强制点挪到 service——`evaluate()` 返回后，立刻用调用前取得的 `backend.kind()` 对每个答案重新盖章（`Answer::normalized_for`，`pub(crate)`），`Decided` 和 `NoConclusion` 的 floor 都过一遍。`Answer::new` 的覆盖保留为纵深防御，文档老实写明它单独不够。

**怎么锁住**：新测试完全复刻评审反例。作者和评审都做了**变异测试**：把 `let answers = normalize(answers, kind);` 改回 `let answers = answers;`，新测试立刻变红，还原后 `git diff` 为空。

## 2. 阈值可以绕过校验反序列化出来，而且错误方向是放宽（#688 M2）

**问题**：`ThresholdBands::new` 会校验范围和次序，字段私有、没有 setter——作者据此在自检里说「不存在构造时校验过、执行时已经漂移的窗口」。但结构体同时 `#[derive(Deserialize)]`，serde 派生实现直接填私有字段，**完全不经过 `new()`**。字段私有挡得住 struct 字面量和 setter，挡不住 `Deserialize`。

**怎么发现的**：评审实跑：

```
deser invalid bands: Ok(ThresholdBands { execute_at: 5.0, escalate_below: -3.0 })
new(0.2,0.9) = Err(Inverted { escalate_below: 0.9, execute_at: 0.2 })
inverted deser p=0.25 -> Execute
inverted deser p=0.5 -> Execute
inverted deser p=0.85 -> Execute
```

**为什么危险**：配置里两个字段写反是很常见的错误。`new()` 会拒，反序列化却照单全收；又因为 `action_for` 先判断 `p >= execute_at`，本该反问或交人的区间**整段变成直接执行**——与 `lib.rs`「只能更严，不能更宽」的硬约束正好相反。R3 Codex 当时判为「当前产品影响 Low」，R4 凭「放宽方向」这条新证据定为 Medium。

**怎么修**：`#[serde(try_from = "RawThresholdBands")]`，`TryFrom` 里直接调 `ThresholdBands::new`——反序列化成为同一条校验路径，而不是平行通路。

**怎么锁住**：新增 `inverted_order_json_is_rejected_not_silently_accepted`（复现原反例）、`out_of_range_json_is_rejected`、`nan_json_is_rejected`（测试注释写明：标准 JSON 没有 NaN 字面量，这条在 tokenizer 层就失败，不假装是同一层校验挡的）、`valid_json_round_trips_through_deserialize`。变异测试：去掉 `try_from` 属性，前两条立刻变红。顺带采纳 L5 建议：原 `there_is_no_default_impl` 测试的注释自己承认测不了「没有 Default」，改用 `compile_fail` doctest 在编译期强制。

## 3. 文档描述了一条不存在的代码路径（#688 M3）

**问题**：ADR-033 第 5 条和 `service.rs` 模块文档说 `Unavailable` 时「带上上一个真正跑过的层的结论」，并说测试 `unavailable_layer_carries_forward_the_rule_layers_conclusion` 锁定了这个行为。实际上 `floor_answers` 只在 `NoConclusion` 分支被赋值为空，`Decided` 一律直接返回——它**永远为空**，那条测试也没断言 `answers`。

**为什么列为阻塞**：ADR 是以后的 PR 自检会当作既定事实引用的文本。错误的 ADR 条目会被后续 PR 当真（评审原话：与 #687「引用一份还没定稿的东西当成已经确定」是同一个教训）。

**怎么修**：二选一（删描述 or 接通路径）。作者按 PLAN-DECIDE §0「不可用时带上已有结论」的要求选择**接通**：`BackendOutcome::NoConclusion` 从空变体改为 `NoConclusion { floor: Vec<Answer> }`，service 携带最近一次非空 floor。ADR-033 第 5/6/7 条按实际保证方式重写，并各加一段「2026-10-07 评审修正」小字说明原文哪里不对——**不悄悄改写历史**。

**怎么锁住**：删掉名不副实的旧测试，换成 `unavailable_layer_carries_the_last_floor_forward`（断言 `question_id`、`p` 原样带上）和 `an_empty_floor_carries_forward_as_empty_not_as_a_fabricated_answer`（空 floor 不被编造成非空）。

同一 PR 的附带发现：作者在 PR body 里说新 crate「拆开后中间态编译不过」所以不拆；评审按真实依赖图指出 `threshold.rs`、`points.rs` 不依赖 crate 内任何东西，这个理由不成立。评审**不要求拆**，只要求把理由改成真实的那一个（「契约各部分需要一起评审」）。

## 4. 「pin」只检查非空（#689）

**问题**：`ModelCatalog::load_str` 的「pin required, no latest」校验只做 `.filter(|s| !s.is_empty())`。`revision: "main"`（会漂移的分支名）和 `sha256: "deadbeef"` 都会被当成「已 pin」。PR 自己的测试数据就实锤了：`one_bad_entry_fails_the_whole_load_not_just_that_entry` 里的「好」条目用的正是 `"main"` + `"deadbeef"`。

**为什么危险**：D0-5 / D1 填真实条目时，这是唯一的校验闸门。按需下载组件的安全性建立在「下载的就是评测过的那个版本」上；分支名 pin 等于没 pin。

**怎么修**：`is_lower_hex(s, len)`——`revision` 必须 40 位小写 hex，`sha256` 必须 64 位小写 hex；新增 `InvalidRevision` / `InvalidSha256` 错误；fixture 全部换成真实形状的值。

**怎么锁住**：两条参数化负向测试，分别覆盖 `"main"`、`"v1.0"`、7 位短 SHA、大写 hex、41 位、含 `g`；以及 `"deadbeef"`、63 位、65 位、大写、非 hex。变异测试：把 `is_lower_hex` 短路成恒 `true`，两条测试按预期转红。

（Codex 独立确认；R2/R4 两轮 Opus 复核一致。）

## 5. 下载同意变量没传给 LaunchAgent（#689，CI 发现）

**问题**：D0-3 引入 `A24_DECIDE_DOWNLOAD_CONSENT`，但 CLI 安装 LaunchAgent 时的 `PASSTHROUGH_VARS` 清单没有它。

**为什么危险**：LaunchAgent 启动的 daemon 永远看不到这个变量，`effective_tier` 会一直报 T0——用户同意了也没用，而且是静默的。

**怎么修 / 锁住**：清单从 16 项扩到 17 项。既有测试 `passthrough_list_matches_what_the_daemon_actually_reads` 正是抓到它的那道 CI；删掉新增项它立刻变红，报错原文：`A24_DECIDE_DOWNLOAD_CONSENT is read by the daemon but is not in PASSTHROUGH_VARS; a LaunchAgent-started daemon would never see it`。这是「让测试从代码里反推配置清单」的好例子。

## 6. 训练 / 评测近义泄漏（#718 → #722）

**问题**：SetFit 训练集与评测集之间只做了逐字精确匹配。评审用 `difflib.SequenceMatcher` 发现「北京明天天气 / 上海明天天气」（0.94）这类换模板词的近义句，≥0.7 的命中占每份评测集 2–6%。

**为什么危险**：会让少样本模型的分数虚高，而 SetFit 恰好是横评的领先者，D1 选型会直接引用这个数。

**怎么修**：#722 新增 `overlap.py`（字符 3-gram Jaccard + SequenceMatcher，任一 ≥0.7 命中），改写 17 条训练样本，评测集不动；SetFit 训练前自动检查并把命中写进报告，不静默改数据。

**怎么锁住**：`tests/test_overlap.py`，包括钉住 #718 那对具体样本的 `test_seq_ratio_matches_known_pair_from_pr718_review` 和「当前数据 0 命中」回归测试。评审额外做了**反向验证**：换回旧训练集重跑，检测器真的报出那几对。

**结果**：去重后 SetFit 准确率 0.762/0.870/0.469 → 0.762/0.850/0.469，领先成立（详见 `04-evaluation-design.md` §5、`05-results.md` §4）。

## 7. CLI 迁移权带来的版本错位（#717，非阻塞）

**问题**：`agent24 decide export/delete` 直接 `Store::open`，而 `Store::open` 无条件跑迁移，sqlx 默认 `ignore_missing = false`（评审读了 vendored `sqlx-core-0.8.6` 的 `validate_applied_migrations` 确认）。新版 CLI 先迁移到 0014，旧版 daemon 再启动会 `VersionMissing` 起不来。

**为什么没阻塞**：R3 Codex 判 Blocking Medium；R4 做了反证——①CLI 默认拉起的是同次构建的 `agent24d`；②main 上已有 13 个迁移，同样的失败类在任意两个 daemon 版本之间**早已存在**，本 PR 只是多开了一条触发路径。加上零调用点、空表、恢复只需重新编译对齐，下调为 forward-note。评审也补充：对「LaunchAgent 常驻 daemon + CLI 分别重建」的场景，反证①不完全成立。

同一评审的其余 forward-note（outcome 无幂等键、时间戳裸文本比较、SQLite 参数上限、CHECK 测试空转）见 `07-data-flywheel-and-personal-model.md` §6。

## 8. 其他值得一提的非阻塞发现

- **「a few syscalls」差了三个数量级**（#689 Low）：`SystemProbe` 用 `System::new_all()`，在 sysinfo 0.39.6 里会读**每个进程**的 cmd/environ/exe 等。R4 本机实测（719 进程）：全量冷 20.7ms、热 4.7–6.9ms；磁盘列表冷 36.5ms；而只刷新内存+CPU 只要 23–35µs。读其他进程的 environ 没有任何必要。建议改窄 refresh。
- **测错了盘**（#689 Low）：`free_disk_for_cwd` 测的是 daemon 当前目录所在盘，launchd 下通常是 `/`，不是模型下载目录；2GB 的 T0 门槛可能测错盘。
- **静默空数组**（#689 Low）：`tiers` / `points` 用 `#[serde(default)]`，字段名打错会变成空数组、条目永远选不中；重复 id 不拒绝。
- **鉴权「恰好兜住」**（#689 Low）：新路由落到默认的 host-only 分支是对的（fail-closed），但没有显式测试；R1b 报的 3 条「无鉴权」是误报——它只看到 diff hunk，没看到同函数里未改动的 `.layer(auth)`。
- **引用了一份还不存在的 ADR**（#686 非阻塞）：PLAN-DECIDE §2.3 原写「与记忆清除权 ADR 同一套语义」，而该 ADR 排在 P1、全仓不存在。现行文本已改为前瞻措辞（见 PLAN-DECIDE §2.3「尚未成文」）。

## 9. 可以提炼的工程实践

1. **契约 PR 的评审标准是「声明 = 代码」**：三条阻塞都不是功能 bug，而是文档 / ADR 声称的不变量在代码上不成立。
2. **用探针实跑，不用「读起来合理」**：M1、M2 都是评审另建 path 依赖 crate 跑出反例；#687 的规则基线是评审独立移植规则、30/30 自测后对 101 条重跑。
3. **修复必须配变异测试**：每个修复都验证了「去掉修复那一行，新测试会变红」。
4. **小 PR 才评得动**：`00-timeline-and-decisions.md` 教训 3——三个真实缺陷（校准标记、阈值方向、版本 pin）都在小 PR 里抓到。
5. **严重度看方向**：同一个绕过，「越界」和「放宽」的严重度不同；`ThresholdBands` 因放宽方向被定为 Medium。
6. **不悄悄改写历史**：ADR 修正保留原文 + 修正说明。
7. **多模型交叉评审会产生误报**，需要终裁：#689 的 R1b 鉴权误报、#717 的 `busy_timeout` 误报都被后续轮次用代码证据驳回。
