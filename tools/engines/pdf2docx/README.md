# a24-pdf2docx：PDF → Word 转换引擎（Loop 12）

这是 documents 模块「转换为 Word」用的按需下载组件，**不进 App 安装包**。桌面端下载钉版的 tar.gz，校验 sha256 后解到
`<documents data>/engines/pdf2docx/<ENGINE_VERSION>/`。documents OS 在 SBX-1 沙箱里，为每次转换单独起一个进程运行它。

选型依据见裁决记录：`~/.claude/decisions/2026-11-28-pdf2word-component/`，以及 knowledge-materials/research/2026-11-28-pdf2word/。

## 组成
- `a24_pdf2docx.py`：命令行入口，格式为 `a24-pdf2docx convert <in.pdf> <out.docx>`。
  - stdout 只输出最后一行 JSON。
  - 退出码：
    - 0：成功，输出 `{"ok":true,"pages":N,"seconds":S}`。
    - 2：用法错误，输出 `{"ok":false,"error":"usage"}`。
    - 3：扫描件，整份没有文字层，输出 `{"ok":false,"error":"scanned_pdf","pages":N}`。
    - 4：其他失败，输出 `{"ok":false,"error":"convert_failed"}`。
  - 失败时不留下输出文件。
- `requirements.lock`：钉版依赖，带 hash。包括 pdf2docx 0.5.13（MIT）、PyMuPDF（AGPL-3.0，不修改）等。
- `patches/`：对 pdf2docx 的补丁。PyMuPDF 不打补丁。
- `build.sh`：构建 arm64 onedir 包，产出 tar.gz、sha256 和 manifest。
- `tests/`：回归测试，见下。

## 三个修复（David 11-28 定的第一片条件）
1. **行尾中文标点丢失**：pdf2docx 抽字时带了 `TEXT_MEDIABOX_CLIP`，会把挂在行尾的「，」「、」丢掉。补丁 `patches/0001-no-mediabox-clip.patch` 把它去掉。
2. **编号条款被识别成无边框表格**：转换时传 `parse_stream_table=False`，只识别有框线的表格。
   - 已知代价：真正无边框的表格会变成段落（FU）。
3. **单元格截断**：转换后把 `w:trHeight hRule="exact"` 和 `w:spacing lineRule="exact"` 改成 `atLeast`。
   - 已知代价：列宽本来就有偏差，有的表格页数会变多，例如普陀 10 页变 19 页（FU）。

另外做了字体映射：中文字体名按 `rust/apps/agent24-documents/src/office.rs` 的 FONT_PAIRS 换成 Noto Serif SC 或 Noto Sans SC，方正系列（`FZ*`）同样换掉。

## 许可
- PyMuPDF 是 AGPL-3.0。按 ADR-DOC-03 D2 的口径处理：
  - 单独下载；
  - 只走进程边界调用；
  - 组件里附 AGPL-3.0 全文（`licenses/PyMuPDF-COPYING`，取 PyMuPDF 同版本源码仓库，钉 sha256）、pdf2docx 的 MIT 许可，以及对应版本的源码地址（`licenses/SOURCE.txt`）；
  - 我们的补丁和入口源码随组件一起提供（`licenses/` 目录）。
- **公开 release 资产之前要先过正式许可审查**（ADR-DOC-03:66、196）。开发期的组件只装到验收机，不公开。

## 测试
```sh
# 对构建产物跑
A24_PDF2DOCX=<dist>/a24-pdf2docx/a24-pdf2docx python3 -m unittest discover -s tools/engines/pdf2docx/tests
```
期望值不用 PyMuPDF 抽：PDF 里的字符数是用仓内 PDFKit 解析器数出来的，避免同一个库自己证明自己。

样本：
- `tests/fixtures/contract-realistic.pdf`、`heilongjiang-decoration.pdf`：由仓内 DOCX fixture 经 LibreOffice 26.8 加 Noto SC 生成。
- `scan.pdf`：s01-06 的图片转成的 PDF。
- 普陀那份用 `docs/documenting/samples/s01/s01-05-zh-putuo-fee-2024/source.pdf`。
