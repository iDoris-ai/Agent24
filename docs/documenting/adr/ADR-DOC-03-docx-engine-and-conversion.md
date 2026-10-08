# ADR-DOC-03：DOCX 编辑引擎与转换 worker（README §17 #6 / #11）

> **状态**：**Proposed**。引擎方向已由 jason 于 2026-10-08 拍板：**只用 SuperDoc 的 headless SDK**（[#696 决策记录](https://github.com/iDoris-ai/Agent24/issues/696#issuecomment-6056284819)）。
> 改为 Accepted 还需要两项：David 评审通过；PR-Daemon APPROVE。
> **日期**：2026-10-08 · **起草**：Claude Code（应 jason 要求）· **负责人**：David Xu · **Issue**：#696
> **依赖**：
> - [ADR-DOC-01](ADR-DOC-01-placement-and-integration.md)：D2 进程外、D6 引擎按需下载、D8 隐私、D10 macOS 优先；
> - [ADR-DOC-02](ADR-DOC-02-operation-contract.md)：唯一提交点、change set、typed errors、`/capabilities`。
>
> **依据**：
> - [README](../README.md) §2.1、§17 #2 / #3 / #6 / #11；
> - researcher [`document-editing-engines.md` @ `33f30f1`](https://github.com/jhfnetboy/researcher/blob/33f30f18a4c3e5566e286579684ebdff54911cb4/topics/T005-documenting/subtopics/document-editing-engines.md)；
> - 2026-10-08 本机 PoC（§5）。

## 0. 范围与产品边界

**Documenting 不做功能完整的 Office。** 它只做三件事：接住用户的文件；在文件上做有限的编辑和插入；导出后能回到 Office 继续用。（jason 2026-10-08 确认）

本 ADR 规定三项：
- DOCX 的读写由哪个组件承担；
- PDF 导出和格式转换由哪个组件承担；
- 它们和 ADR-DOC-02 操作契约怎么对接。

**不在本 ADR 范围内**：PDF 原件的编辑（#694）、首发格式矩阵（#692）、内容模型（#691）。

## 1. 决策

### D1 DOCX 引擎：SuperDoc headless SDK

- **组件**：`@superdoc/sdk` 加上对应平台的原生 host `@superdoc/sdk-<platform>`。
  - 版本锁定为 **2.18.0**，pin + sha256。
  - 作为**按需下载组件**（ADR-DOC-01 D6），运行在独立进程。
- **调用方**：只有 `documents` OS。agent 和页面都调 Documenting 的操作（ADR-DOC-02），由 OS 再调 SDK。
  - agent 和页面不直接接触 SDK。
  - 不暴露 SuperDoc 自带的 MCP、CLI 或 agent 工具。
- **用途**：读结构、定位目标、修改与插入、修订（tracked）、批注、导出 DOCX。
  - 定位目标的手段：书签、内容控件、表格单元格、列表项、文本匹配。
  - 本 ADR 用到的操作见 §3。
- **升级**：换版本必须重跑 §5 的样本对照，并刷新锁定的 sha256。不追 `next` 预发布版。

### D2 许可边界：只用 headless

SuperDoc 是 AGPL-3.0（另有商业授权）。为了让 AGPL 义务只落在单独下载的组件上，不落在 Agent24 桌面端：

1. **Agent24 的任何产物都不包含 SuperDoc 代码。**
   - 范围包括 renderer、主进程、Rust 二进制和安装包。
   - 涉及的包有 `superdoc`、`@superdoc/react`、`@superdoc/docx-engine` 的编辑器 bundle，以及其他 `@superdoc/*` 包。
   - CI 增加依赖检查：`apps/` 和 `rust/` 的依赖清单里出现 `superdoc` 就失败。
2. **只通过进程边界调用 SDK**（IPC / stdio），不做静态或动态链接。
3. **不修改 SuperDoc 源码。** 确需修改时，先改上游，或者另立 ADR。
4. **下载组件附带完整许可文件**：原样附带 SuperDoc 的 LICENSE、NOTICE、THIRD_PARTY_NOTICES，并注明对应版本源码的获取地址。
5. **暂不购买商业授权。** 将来如果要做闭源版本，或要嵌入编辑器，就重新评估（§6）。

以上是工程上的判断，不是法律意见。发布前需要做一次正式的许可审查（§6）。

### D3 转换与 PDF worker：LibreOffice headless

- **组件**：LibreOffice 26.x，pin + sha256，按需下载，运行在独立进程。许可证为 MPL-2.0。
- **只承担三类工作**：
  1. DOCX 导出 PDF；
  2. ADR-DOC-02 `render` 操作需要的页面预览图；
  3. 导入时把 DOC / ODT / RTF 转成 DOCX。转出的文件**标为转换副本**，不当作原件的无损修改。
- **禁止 LibreOffice 写回权威 DOCX，也不允许它重新保存权威 DOCX。**
  - 原因：PoC 显示它会把整份文档重新写出（§5）。
  - 预览和 PDF 都只是派生产物。
- **调用方式**：用 `--convert-to`，或在 soffice 进程内以宏的方式运行。
  - 不用它自带的独立 Python 解释器：该程序在 macOS 26 上启动即被系统 SIGKILL（§5）。
- **隔离**：每个任务使用独立 profile，禁用宏的自动执行和外部链接，并设置超时和资源上限。

### D4 字体是产品要求

- worker 随附 Noto Serif SC 和 Noto Sans SC（OFL），常规和粗体各一份。
- 同时配置替换表：

  | 文档里的字体 | 替换为 |
  |---|---|
  | 宋体 / SimSun / NSimSun、仿宋 / FangSong / 仿宋_GB2312、楷体 / KaiTi | Noto Serif SC |
  | 黑体 / SimHei、微软雅黑 / Microsoft YaHei、等线 / DengXian | Noto Sans SC |

- 每次导出的报告里列出实际发生的字体替换。
- 不配置会出什么问题：macOS 上没有 SimSun / SimHei 时，LibreOffice 会选一款希伯来文字体替代，PDF 里的中文一片空白（§5）。

### D5 界面：DOC-1 不嵌编辑画布

- **文档页由三部分组成**：
  1. 页面预览（D3 生成的图片）；
  2. **结构化编辑面板**：列出可编辑的目标，即书签、内容控件、表格单元格、列表项和段落，由用户修改或插入内容；
  3. change set 的 diff，由用户逐条 review，然后 commit（ADR-DOC-02 Q5）。
- **不嵌入所见即所得的编辑器。** 这和 D2 直接相关：SuperDoc 的编辑器只能运行在 renderer 里。
- **#11 外部编辑应用：不采用。** 用户需要完整编辑时，导出后在 Office 里编辑，再回流（§6）。
- DOC-3 是否需要编辑画布，另行决策。候选方向有三个：购买商业授权；采用非 AGPL 的编辑器；或者继续用结构化面板。

### D6 隐私与网络

- **SDK 进程**：始终带 `SUPERDOC_TELEMETRY=0` 启动。
- **网络**：SDK 和 worker 在运行时都不需要网络。G8 验收在断网条件下进行（README §18）。
- **资料处理政策**：两者都只处理 OS 交给它们的本地文件，遵循 ADR-DOC-01 D8。

## 2. 版本与修订：不能混用的三套概念

| 概念 | 归属 | 说明 |
|---|---|---|
| Documenting revision | ADR-DOC-02 | 只有 `revision.commit` 产生，是唯一的权威 |
| SuperDoc session revision（`expectedRevision`） | SDK 会话内 | 只用于防止同一会话内的并发写，不对外暴露 |
| Word 修订标记（`w:ins` / `w:del`） | 文件内容 | 用户原有的修订属于文档内容，必须保留 |

## 3. 与 ADR-DOC-02 操作的对接

- **`change.propose`**：
  1. 在 SDK 会话里打开 base revision 的 blob；
  2. 以 **tracked** 模式执行 ops；
  3. 从 `trackChanges.list` 取出**本次新增**的修订 id，记入 change set，原有修订一律排除；
  4. 保存为 `review-preserving` 的提案文件（草稿，不算 revision）。
  - 允许的 op 白名单（DOC-1）：`replace`（文本目标，带 `expectedText`）、`contentControls.text.setValue`、`tables.setCellText`、`tables.insertRow`、`lists.insert`、`create.paragraph`、`create.image`。
  - 白名单之外的 op 返回 422 `invalid_request`，`details.reason` 为 `op_not_allowed`。如需专用错误码，在 ADR-DOC-02 §6 增补。
- **`change.review`**：只记录逐条决定（ADR-DOC-02 §5），这一步不调用 SDK。
- **`revision.commit`**：
  1. 打开提案文件；
  2. 对 change set 里的每条修订调用 `trackChanges.decide`，按 review 结果接受或拒绝；
  3. 以 **`review-preserving`** 模式保存，写入 blob，成为新的 revision。
  - **禁止用 `final` 模式提交。** PoC 显示 `final` 会把用户原有的 Word 修订一并接受掉（§5）。
- **`export`**：
  - DOCX：默认 `review-preserving`。
  - 清洁交付版：必须由用户显式选择。导出前列出全部修订、批注和隐藏内容，由用户确认后才导出（README §11）。
  - PDF：由 D3 的 worker 从选定的 revision 生成。
- **`/capabilities`**：`engines` 中登记两项，`superdoc-sdk`（kind `edit`、`export`）和 `libreoffice`（kind `render`、`export`、`convert`），各自注明版本。未下载时，对应操作返回 `engine_unavailable`。

## 4. 平台与体积

| 组件 | 平台 | 体积（实测，macOS arm64） |
|---|---|---|
| SuperDoc SDK 原生 host | darwin-arm64 / x64、linux-x64 / arm64、windows-x64 | 146 MB |
| LibreOffice 26.8 | macOS、Linux、Windows | 应用 804 MB（dmg 285 MB） |
| Noto CJK SC（4 个字重文件） | 通用 | 约 40 MB |

Windows 的时间点仍然跟随 ADR-DOC-01 D10。

## 5. 证据：本机 PoC（2026-10-08，macOS arm64）

**样本**：手写的中英文通知 DOCX，包含：
- 页眉页脚和 PAGE 域；
- 书签、纯文本内容控件；
- 批注、已有修订、脚注；
- 编号列表；
- 带 gridSpan 和 vMerge 合并的表格。

**操作**：两条链路执行相同的 6 个操作，直接模式和修订模式各跑一遍。

| 项 | SuperDoc SDK 2.18.0 | LibreOffice 26.8 UNO |
|---|---|---|
| 6 个操作（书签文本、内容控件、单元格、加行、插入列表项、插入图片） | 全部正确 | 全部正确 |
| 被替换文字的格式 | 保持 | **缺陷**：被替换的日期继承了前面标签的粗体 |
| 未改动的部分 | styles.xml 字节不变；页眉只多一个 `w14:paraId` | 整个包重新写出：文字按中英文边界被拆碎，多出空白页眉页脚文件，混入自带样式和字体 |
| 修订模式 | 标准 Word 修订标记，作者 Agent24；加行标记为 `trPr/ins` | 可用，但被拆成按字的碎片 |
| `final` 导出 | 接受**全部**修订，包括用户原有修订；批注保留 | — |
| 选择性提交（模拟 commit） | 按 id 接受 Agent24 的修订、拒绝其中一条后，被拒的单元格恢复原值；用户原有修订和批注都保留 | — |
| 耗时 | 打开 0.06 s，6 个操作 0.1–0.16 s | 启动、编辑、导出 DOCX 和 PDF 合计约 0.6 s |
| 网络 | 采样期间进程树无外部连接 | 不需要网络 |

**交叉验证**：SuperDoc 的输出交给 LibreOffice 打开并导出 PDF。中文、合并单元格、新增行和图片都正常。

**真实 Word 读取验证**（Word for Mac 16.113.4，未授权的只读模式，通过 AppleScript 读取）：
- 7 个文件（原始样本、SuperDoc 的 4 个版本、LibreOffice 的 2 个版本）都能正常打开，没有出现修复提示。
- SuperDoc 的输出：
  - Word 识别出书签、批注、脚注、表格、图片和内容控件里的文字；
  - 修订模式下读到 10 条修订：Agent24 8 条，王五 2 条；
  - 选择性提交后只剩王五的 2 条，被拒的单元格恢复为"待定"；
  - `final` 导出后 0 条，王五的修订也被接受了。
- LibreOffice 修订模式的输出：作者全部显示为 "Unknown Author"，我们的署名丢失。

**脚本与样本**：见 `docs/documenting/evidence/2026-10-08-engine-poc/`（另一个 PR）。

## 6. 待办与重新评估的条件

| 项 | 时机 | 说明 |
|---|---|---|
| Word 中的版式核对（从 Word 导出 PDF，与 LibreOffice 渲染逐页对比） | **暂缓**（jason 2026-10-08）；DOC-1 第 2 片验收前补做 | 读取层面已用真实 Word 验证（§5）。版式需要 Word 能保存或导出，当前的 Word 未授权，只能只读 |
| G6b Office 回流实测（在 Word 中修改后导回） | **暂缓**（jason 2026-10-08）；与下面的回流操作一起做 | 同样需要授权的 Word |
| 用 S03 / S08 真实 Word 模板复测 | DOC-1 第 2 片 | 本次样本是手写的 |
| #692 格式矩阵 | DOC-1 第 2 片之前 | README §17 规定 #3 先于 #6。本 ADR 提前定了引擎方向，若矩阵要求的能力 SuperDoc 不能通过，就重新评估 |
| 正式许可审查 | 首个对外发行之前 | 审查 D2 边界，以及下载组件的源码提供义务 |
| Office 回流（再导入为同一文档的新版本，并检测冲突） | DOC-3 | ADR-DOC-02 需要新增操作；不能后写覆盖先写 |
| 通用的按需组件安装器 | DOC-1 第 2 片 | ADR-DOC-01 D6 已登记为 Agent24 依赖 |
| SuperDoc API 变动 | 每次升级 | 版本锁定，升级时按 §5 复测 |

**出现以下任一情况，重新评估本 ADR**：
- 需要闭源分发，或需要在 renderer 里嵌入编辑器；
- SuperDoc 在 #692 承诺的能力上失败；
- SuperDoc 上游停止维护，或许可证发生变化。
