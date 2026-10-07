# Documenting 样本与金标规范（S01 起）

> **Issue**：#706（DOC-1-07）· **适用**：DOC-1 第 1 片（S01，只读）的验收样本；之后各切片沿用同一格式扩展。
> **依据**：
> - README §11.8（金标评测）、§18.2（样本集）；
> - ADR-DOC-02 §3（溯源锚点）；
> - T005 [`user-scenarios-capabilities.md`](https://github.com/jhfnetboy/researcher/blob/main/topics/T005-documenting/subtopics/user-scenarios-capabilities.md) §3.1（S01 定义）、§6（中文与多语必测项）、§9.2（固定验收题）。
>
> **背景**：jason 确认没有现成样本（2026-10-07），T005 §9 的样本征集也尚未执行。因此样本由本规范约束，从**可合规入库的公开来源**收集。

## 1. 来源规则

| 优先级 | 来源 | 许可 / 依据 | 要求 |
|---|---|---|---|
| 1 | 中国国家机关公开发布的通知、公告、办事指南 | 著作权法第五条：具有行政性质的文件不适用该法 | 记录发布机关和原始链接 |
| 1 | 英国中央 / 地方政府公开文件 | Open Government Licence v3.0（以页面标注为准） | 确认页面标注了 OGL，否则不收 |
| 1 | 美国联邦政府作品 | 17 U.S.C. §105（公有领域） | 只收联邦文件，州和地方另行确认 |
| 2 | 其他公开文件（企业、学校、物业） | 需要明确的再分发许可 | 没有许可就**不入库**，只在本地评测时用 |
| 3 | **合成样本** | 本仓库原创 | 只在公开来源里找不到某个陷阱时使用。`kind` 标 `synthetic`，占比 ≤ 30%，写法要照真实通知的格式 |

- **不收**含个人信息的文件：个人姓名配联系方式、身份证号、账号、门牌号等。公开文件里如果出现**个人**电话、邮箱，用黑块遮挡后另存，并在 `meta.yaml` 记 `redactions`。机构的公开联系方式保留。
- **公职人员例外**（v2，David 2026-10-08）：政府公文中以**公职身份**公开的官员姓名，搭配其**机构**联系方式（办公电话、机构邮箱），可以保留，不算个人信息。要在 `meta.yaml` 的 `public_officials` 里逐项登记，例如 s01-02 的 EPA RTCR Manager。私人电话、个人邮箱仍须遮挡。
- **不改正文**：只允许遮挡个人信息、裁掉无关页。原件的 sha256 和处理后的 sha256 都要记录。
- **派生件**（v2）：允许由已入库的公开原件生成模拟扫描件。允许的处理只有栅格化、旋转、缩放和有损压缩，不得改动文字。
  - `meta.yaml` 必须记录 `derived_from`、`sha256_parent`（输入文件）和 `derivation`（逐步参数）。
  - 此时 `sha256_original` 指父件，`sha256_stored` 指生成物。
  - 派生件计入合成占比。
- **体积**：本仓库没有启用 Git LFS，所以单个文件 ≤ 1 MiB，S01 合计 ≤ 8 MiB。超出时只截取相关页。样本变大以后怎么存放，见 §7。

## 2. S01 覆盖矩阵

S01 是“个人收到说明或通知，需要知道该做什么”。最少 8 份样本，合起来必须覆盖下表每一行。一份样本可以覆盖多行。

| # | 覆盖点 | 依据 |
|---|---|---|
| C1 | 中文**文本层 PDF** | §9.2 第 1 题 |
| C2 | 中文**扫描件或照片**（倾斜、阴影、印章至少有一项） | §6 扫描件 |
| C3 | 英文文本层 PDF | 多语 |
| C4 | **中英混排**（专名、单位、缩写） | §6 中西文混排 |
| C5 | **多个日期**，有截止日也有生效日或例外日 | S01 风险：漏掉期限 |
| C6 | **否定或豁免**（“无需”“不得”“除……外”“no need”“do not”） | S01 风险：读错否定词 |
| C7 | 金额、单位、小数、千位分隔符，或带**前导零**的编号 | §6 数字不可改写 |
| C8 | **表格**（多行表头、合并单元格、空值至少有一项） | §6 表格 |
| C9 | **文档里本来没有答案**的字段，期望输出“缺失” | §9.2：无答案时明确缺失 |
| C10 | **前后矛盾**（两处日期或要求冲突），期望标出“冲突”，不得擅自取舍 | S01 完成标准 |

C1–C8 优先用真实公开样本；C9、C10 在真实样本里很少见，可以用合成样本。

## 3. 目录与文件

```text
docs/documenting/samples/
  README.md                 本规范
  s01/
    <id>/                   id = s01-<两位序号>-<lang>-<短名>，例如 s01-03-en-water-outage
      source.<pdf|png|jpg>  入库的那份文件（必要时已遮挡）
      meta.yaml             来源与处理记录
      gold.json             金标
```

`meta.yaml` 字段：

```yaml
id: s01-03-en-water-outage
title: "…"                      # 原标题
lang: zh | en | mixed           # 主要语言
kind: text_pdf | scanned_pdf | image | synthetic
pages: 2
source_url: "https://…"         # synthetic 填 null
publisher: "…"                  # 发布机关或机构
retrieved_at: 2026-10-08
license: "CN-Copyright-Art5 | OGL-UK-3.0 | US-17USC105 | synthetic | …"
sha256_original: "…"
sha256_stored: "…"              # 和原件相同时与上一项一致
redactions: []                  # 例如 [{page: 1, what: "personal phone"}]
public_officials: []            # v2：保留的公职人员姓名，例如 [{page: 2, name: "…", role: "…", contact: "institutional"}]
covers: [C1, C5, C6]            # 对应 §2 的覆盖点；每个覆盖点都必须能指到金标里的具体字段
reader: "…"                     # v2：预期读者，例如“在上海补办身份证的居民”
questions: [Q1, Q3, Q5]         # v2：本样本要回答的 S01 问题（见 §4.1）
extraction_notes:               # v2：文本层异常一律注明抽取器
  - { extractor: "PDFKit (macOS 26)", page: 1, finding: "标题重复两次" }
derived_from: null              # v2：派生件填父样本 id
sha256_parent: null
derivation: []                  # v2：例如 ["sips rasterize 72dpi p1", "rotate 2°", "resample 900px", "jpeg q45"]
notes: "…"
```

## 4. 金标格式（`gold.json`，schema `s01-gold-v2`）

v1 的问题来自 2026-10-08 的 Codex 审阅：适用对象表达不了、相对期限表达不了、冲突候选没有原文、缺失原因不分、锚点没有覆盖整句命题。v2 只增加字段，不改变 v1 已有字段的含义。

### 4.1 S01 问题集与读者

每份样本的 `meta.yaml` 写明 `reader`（预期读者）和 `questions`。字段只标注这位读者回答这些问题需要的信息，不追求覆盖全文的每一行：

| Q | 问题 |
|---|---|
| Q1 | 我要做什么（动作、方式、地点） |
| Q2 | 什么时候做 / 截止到什么时候（含相对期限、结束条件） |
| Q3 | 要花多少钱、有什么数量限制 |
| Q4 | 什么**不用**做、谁可以不做（否定、豁免、例外） |
| Q5 | 这条规定对**谁**适用、在什么条件下适用 |
| Q6 | 出了问题找谁 |

### 4.2 字段

```json
{
  "key": "late_registration_filing_deadline",
  "q": "Q2",
  "status": "present",
  "value": "3 months from the date on the letter or email",
  "time": { "kind": "relative", "trigger": "date on HMRC's letter or email", "duration": "P3M" },
  "audience": "people who register after 5 October 2026",
  "applies_if": ["registered after 5 October 2026"],
  "negation": false,
  "severity": "critical",
  "anchors": [
    { "page": 2, "quote": "If you register after 5 October 2026" },
    { "page": 2, "quote": "this will be 3 months from the date on the letter or email." }
  ]
}
```

- **`status`** 只有三种：`present`、`missing`、`conflict`。缺失和冲突本身就是正确答案，不能用推测填补。
- **`missing_reason`**（`status: missing` 时必填），四选一：
  - `blank_in_template`：模板留空；
  - `not_in_document`：全文都没有；
  - `referenced_but_absent`：文中提到、但内容不在文件里；
  - `outside_page_scope`：在本件未收录的页上。
- **`candidates`**（`status: conflict` 时必填）：每个候选 `{value, normalized, anchors}`。`value` 保留原文写法，例如 “2026年10月12日” 和 “10月13日” 分开记录。**不得**只给规范化值。
- **`time`**：
  - `kind` 取值：`date`、`datetime`、`interval`、`relative`、`open_ended`；
  - `inferred` 列出推断出来的部分，例如 `["year"]`，并在 `inferred_from` 写依据；
  - 原文没有的时区和时刻**不得补**，不能把读者本地时区注入原文没写时区的时间；
  - `relative` 用 `trigger + duration`（ISO 8601）表示；`open_ended` 用 `end_condition` 表示，例如 “until further notice”。
- **`audience` / `applies_if` / `exceptions`**：规定只对部分人或部分情形适用时**必填**，例如 EPA 的 24 小时要求只针对运营方。`exceptions` 填其他字段的 `key`。
- **锚点覆盖整句命题**：`anchors` 必须同时覆盖值和决定含义的部分，即条件、对象、否定词，以及表格的行标签和列。只锚一个数字不算。
- **`severity`**：
  - `critical`：日期、时间、期限、金额、数量限额、否定 / 豁免、编号。编号包括文号、单元号、设备编号、电话。错一项就单独记为严重错误。**缺失字段按目标信息的类别定级**，例如缺失的截止日也是 `critical`。
  - `major`：动作、对象、适用条件。
  - `minor`：措辞性的信息。
- **`must_not_assert`**：用对象 `{proposition, unless?}` 表达可判定的命题，不按字符串匹配。由 harness 的评审器（人工或评测模型）判定，`unless` 写可以接受的有条件说法。
- **数字原样**：`value` 保留原文写法，包括全半角、括号样式、千位分隔符和前导零。`normalized` 只用于比对。

### 4.3 锚点比对

`quote` 一律取原件**显示**的文字。比对分级记录，不是只有命中和不命中：

| 级别 | 规则 | 结论 |
|---|---|---|
| L1 | 两边都做 NFKC 规范化，并把空白（含换行）折叠成单个空格后，原文中能找到 | 命中 |
| L2 | 在 L1 的基础上，再删除**两侧都是 CJK 字符**的空白后，原文中能找到 | 命中，但记“文本层断字” |
| L3 | L1、L2 都不中，但人工核对显示页面后确认，或者有 `rects` 证据 | 显示正确、文本层异常，单列统计 |
| — | 都不满足 | 金标错误 |

- 文本层异常和**抽取器有关**：同一份 PDF 换一个抽取器，结果可能不同，比如出现标题重复或行间交错。这类异常记在 `extraction_notes` 里，必须注明抽取器和版本。
- 页面上印出来的文字也是原文，例如链接文字。引用整句时不能省略，或者只引其中不含这类文字的连续片段。
- 金标以 PDFKit（macOS）抽出的文本为 L1/L2 的基准，并把这份文本另存为 `source.pagetext.txt` 便于复核。

## 5. 标注与核对流程

1. **收集**：按 §1、§2 选文件，写 `meta.yaml`，记录 sha256。
2. **起草金标**：由 Claude 起草，`labeled_by: claude-draft`。
2.5. **独立审阅**（v2）：由 Codex 按本规范逐条审阅，确认 L1/L2 锚点命中、问题集都已覆盖、`must_not_assert` 可以判定。审阅意见处理完后，才进入第 3 步。
3. **人工核对（必须）**：David 逐项对照原件核对。核对后 `verified_by` 填 `david`，改动直接写进 `gold.json`。**未核对的金标不进入验收统计。**
4. **入库**：每个样本目录都通过 PR 合入 `ab/documenting`。之后要修改金标，必须在 PR 里说明原因，旧版本由 git 追溯。
5. **使用**：#708 的验收 harness 读取 `gold.json`，分别统计：字段正确率、`missing` / `conflict` 判对率、锚点命中率、严重错误数（单独列出），以及 `must_not_assert` 违规数。

## 6.5 S01 样本索引（v1，2026-10-07）

| id | 来源 | 类型 | 覆盖 |
|---|---|---|---|
| s01-01-zh-sh-fee-2025 | 上海市财政局、发改委 | text_pdf，33 页 | C1 C5 C6 C7 C8 C9 |
| s01-02-en-epa-boil-water | U.S. EPA Region 8 | text_pdf，3 页 | C3 C6 C7 C9 |
| s01-03-zh-holiday-2026 | 国务院办公厅（gov.cn 网页打印） | text_pdf，2 页 | C1 C5 C6 |
| s01-04-en-govuk-sa-deadlines | HMRC / GOV.UK（OGL） | text_pdf，4 页 | C3 C5 C6 |
| s01-05-zh-putuo-fee-2024 | 上海市普陀区 | text_pdf，10 页 | C7 C8 C9 |
| s01-06-zh-holiday-2026-scan | 由 s01-03 派生的模拟扫描 | image | C2 |
| s01-07-mixed-building-notice | 合成 | synthetic | C4 C6 C7 C9 C10 |

- **合成占比**：2/7（s01-06 是派生件，也计入合成），满足 ≤ 30%。合计 1.78 MB。
- **核对状态**：全部金标都是 `claude-draft`，核对前不计入验收统计。
- **v2 返工中**（2026-10-08）：Codex 审阅（§5 第 2.5 步）认定 v1 金标不能签核，主要问题有：
  - 10 个锚点和文本层对不上；
  - 缺失字段的严重度标错了；
  - 适用条件和对象没有标；
  - 覆盖点报多了；
  - 样本只有 7 份，少于 §2 要求的 8 份。

  因此全部金标改按 `s01-gold-v2` 重写，并补第 8 份真实样本。返工完成后，本表的覆盖列按字段级证据重新核算。

## 7. 样本变大后的存放（待 jason 决定）

S01 直接进 git 即可（见 §1 的体积上限）。出现**下列任一情况**时，在 #714 提出存放方案，请 jason 决定，在此之前不新增大文件：

- 全部样本合计超过 **20 MiB**；
- 出现单个超过 1 MiB、又不能只截取相关页的文件，例如第 2 片的模板、长扫描件、多页表格。

备选方案：

| 方案 | 做法 | 代价 |
|---|---|---|
| A Git LFS | 加 `.gitattributes`，样本走 LFS | 影响整个仓库：每个克隆和 PR-Daemon 的 worktree 都要装 `git-lfs`；所有 CI 的 checkout 都要加 `lfs: true`；按组织计费的 LFS 存储和流量；启用后再撤掉需要改写历史，与集成分支禁止 force-push 的规则冲突 |
| B 单独的样本仓库 | 例如 `iDoris-ai/documenting-samples`，本仓库只记录版本和 sha256 | 多维护一个仓库；验收 harness 要按 pin 拉取 |
| C Release 附件 | 样本打成压缩包挂在 GitHub Release 上，本仓库记录 URL 和 sha256 | 不进 git 历史；改样本就要发新版本 |
| D 只放本地 | 不入库，只放在约定的本地目录，仓库只存 `meta.yaml` 和 `gold.json` | 其他人和 CI 复现不了，只适合临时使用 |

不管选哪种，`meta.yaml` 和 `gold.json` 都留在本仓库，样本按 sha256 精确对应。

## 8. 不在本规范内

- S02 以后各场景的样本，会沿用本格式，但覆盖矩阵在各自切片里另写。
- DOC-1 第 2 片的模板样本（多语言模板的编辑、导出、重开），由第 2 片的规范补充。
- 获准云模式下的隐私负例样本，归 #735 的安全验收。
