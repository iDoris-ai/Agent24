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
- **不改正文**：只允许遮挡个人信息、裁掉无关页。原件的 sha256 和处理后的 sha256 都要记录。
- **体积**：本仓库没有启用 Git LFS，所以单个文件 ≤ 1 MiB，S01 合计 ≤ 8 MiB。超出时只截取相关页。

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
covers: [C1, C5, C6]            # 对应 §2 的覆盖点
notes: "…"
```

## 4. 金标格式（`gold.json`）

金标回答 S01 的固定问题。字段集合沿用 T005 §9.2 第 1 题：截止日期、要求、否定 / 豁免、金额 / 编号、缺失与冲突。

```json
{
  "id": "s01-03-en-water-outage",
  "schema": "s01-gold-v1",
  "fields": [
    {
      "key": "deadline",
      "status": "present",
      "value": "2026-10-15 18:00",
      "normalized": "2026-10-15T18:00:00+01:00",
      "anchors": [{ "page": 1, "quote": "by 6pm on 15 October 2026" }],
      "severity": "critical"
    },
    {
      "key": "requirement",
      "status": "present",
      "value": "Store drinking water in advance",
      "anchors": [{ "page": 1, "quote": "…" }],
      "severity": "major"
    },
    {
      "key": "exemption",
      "status": "present",
      "value": "No need to register",
      "negation": true,
      "anchors": [{ "page": 2, "quote": "You do not need to register" }],
      "severity": "critical"
    },
    { "key": "fee", "status": "missing", "severity": "major" },
    {
      "key": "start_date",
      "status": "conflict",
      "values": ["2026-10-12", "2026-10-13"],
      "anchors": [{ "page": 1, "quote": "…12 October…" }, { "page": 2, "quote": "…13 October…" }],
      "severity": "critical"
    }
  ],
  "must_not_assert": ["a fee amount", "an online registration link"],
  "labeled_by": "claude-draft",
  "verified_by": null
}
```

规则：

- **`status` 三选一**：`present`、`missing`、`conflict`。缺失和冲突本身就是正确答案，不能用模型的推测填补。
- **锚点**：`quote` 必须逐字取自原件，空白规范化除外。`page` 从 1 开始。扫描件可以额外给 `rects`，格式与 ADR-DOC-02 §3 相同，第一版不强制。
- **`severity`**：
  - `critical`：日期、金额、否定词、编号。错一项就单独记为**严重错误**，不和普通准确率平均（T005 §9.2）。
  - `major`：要求、对象。
  - `minor`：措辞性的信息。
- **`must_not_assert`**：文档里没有、但模型容易编出来的内容。输出里出现其中任何一项，就记为严重错误。
- **数字原样**：`value` 保留原文写法，包括全半角、千位分隔符和前导零。`normalized` 只用来比对，不能替代 `value`。

## 5. 标注与核对流程

1. **收集**：按 §1、§2 选文件，写 `meta.yaml`，记录 sha256。
2. **起草金标**：由 Claude 起草，`labeled_by: claude-draft`。
3. **人工核对（必须）**：David 逐项对照原件核对。核对后 `verified_by` 填 `david`，改动直接写进 `gold.json`。**未核对的金标不进入验收统计。**
4. **入库**：每个样本目录都通过 PR 合入 `ab/documenting`。之后要修改金标，必须在 PR 里说明原因，旧版本由 git 追溯。
5. **使用**：#708 的验收 harness 读取 `gold.json`，分别统计：字段正确率、`missing` / `conflict` 判对率、锚点命中率、严重错误数（单独列出），以及 `must_not_assert` 违规数。

## 6. 不在本规范内

- S02 以后各场景的样本，会沿用本格式，但覆盖矩阵在各自切片里另写。
- DOC-1 第 2 片的模板样本（多语言模板的编辑、导出、重开），由第 2 片的规范补充。
- 获准云模式下的隐私负例样本，归 #735 的安全验收。
