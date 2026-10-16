# 2026-10-08 引擎 PoC：SuperDoc headless SDK 与 LibreOffice UNO 对比

这是 [ADR-DOC-03](../../adr/ADR-DOC-03-docx-engine-and-conversion.md) §5 的证据，也是 #696 决策记录的依据。

**这里的脚本只是一次性的验证工具，不属于产品代码，也不进入任何构建。** 需要 npm 包的脚本在临时目录里安装依赖后再运行。仓库里没有、也不应该有 SuperDoc 依赖（ADR-DOC-03 D2）。

## 环境（实测）

| 项 | 值 |
|---|---|
| 系统 | macOS 26，arm64 |
| SuperDoc | `@superdoc/sdk` 2.18.0 和原生 host `@superdoc/sdk-darwin-arm64` 2.18.0（npm，2026-10-06 发布） |
| LibreOffice | 26.8.0.3，`LibreOffice_26.8.0_MacOS_aarch64.dmg`，sha256 `8858d8058da4f862f47559486814e65efc27294da67c5e4bb56b006b1ee59f89`（与官方 `.sha256` 一致） |
| 字体 | [notofonts/noto-cjk](https://github.com/notofonts/noto-cjk) 的 SubsetOTF：NotoSerifSC、NotoSansSC，常规和粗体各一份 |
| Node / Python | Node 26，Python 3.12（系统 python 只运行 `probe.py` 等纯标准库脚本） |

## 文件

| 文件 | 作用 |
|---|---|
| `make_fixture.py` | 生成手写的中英文通知 DOCX，含页眉页脚和 PAGE 域、书签、内容控件、批注、已有修订、脚注、编号列表、gridSpan 和 vMerge 合并的表格 |
| `probe.py` / `cmp.py` | 提取 DOCX 的结构指纹，并比较修改前后的差异 |
| `sd_edit.mjs` | 链路 A：用 SuperDoc SDK 执行 6 个操作，分 `direct` 和 `tracked` 两种模式 |
| `sd_commit.mjs` | 模拟 commit：按 id 接受 Agent24 的修订、拒绝其中一条，并以 `review-preserving` 保存 |
| `lo_macro.py` | 链路 B：同样的 6 个操作，在 soffice 进程内作为 Python 宏运行 |
| `setup_lo_profile.py` | 生成隔离的 LibreOffice profile：放入随附字体、CJK 字体替换表和宏 |
| `render.swift` | 用 PDFKit 把 PDF 渲染成 PNG，并输出文本层，用于人工核对版式 |

## 复现

```sh
P=docs/documenting/evidence/2026-10-08-engine-poc
W=$(mktemp -d)                                   # 临时工作目录，所有产物都放这里
python3 -I $P/make_fixture.py $W/notice.docx
magick -size 400x200 xc:'#2b6cb0' $W/logo.png

# 链路 A：SuperDoc
(cd $W && npm init -y >/dev/null && npm i --ignore-scripts @superdoc/sdk@2.18.0)
cp $P/sd_edit.mjs $P/sd_commit.mjs $W/
(cd $W && node sd_edit.mjs notice.docx $W/sd-tracked.docx tracked logo.png \
       && node sd_commit.mjs sd-tracked.docx $W/sd-committed.docx)

# 链路 B：LibreOffice（从 dmg 解出 LibreOffice.app，并下载字体到 $W/fonts）
SO=/path/to/LibreOffice.app/Contents/MacOS/soffice
python3 -I $P/setup_lo_profile.py $W/lo-profile $W/fonts $P/lo_macro.py
A_SRC=$W/notice.docx A_DOCX=$W/lo-tracked.docx A_PDF=$W/lo-tracked.pdf A_PNG=$W/logo.png \
A_LOG=$W/lo.log A_TRACKED=1 \
  $SO -env:UserInstallation=file://$W/lo-profile --headless --norestore \
  "vnd.sun.star.script:lo_macro.py\$run?language=Python&location=user"

# 对比结构，并把 SuperDoc 的输出交给 LibreOffice 导出 PDF
python3 -I $P/cmp.py $W/notice.docx $W/sd-tracked.docx $W/lo-tracked.docx
$SO -env:UserInstallation=file://$W/lo-profile --headless --convert-to pdf --outdir $W $W/sd-tracked.docx
swiftc -O $P/render.swift -o $W/render && $W/render $W/sd-tracked.pdf $W/sd text
```

## 结果

| 项 | SuperDoc SDK 2.18.0 | LibreOffice 26.8 UNO |
|---|---|---|
| 6 个操作（书签文本、内容控件、单元格、加行、插入列表项、插入图片） | 全部正确 | 全部正确 |
| 被替换文字的格式 | 保持 | **缺陷**：替换后的日期继承了前面标签的粗体 |
| 未改动的部分 | styles.xml 字节不变；页眉只多一个 `w14:paraId` | 整个包重新写出：文字按中英文边界被拆碎，多出空白页眉页脚文件，混入自带样式和字体 |
| 修订模式 | 标准 Word 修订标记，作者 Agent24；加行标记为 `trPr/ins` | 可用，但被拆成按字的碎片 |
| `final` 导出 | 接受**全部**修订，包括用户原有修订 | — |
| 选择性提交 | 只处理自己的修订，被拒的单元格恢复原值；用户的修订和批注都保留 | — |
| 耗时 | 打开 0.06 s，6 个操作 0.1–0.16 s，保存 0.01 s | 启动、编辑、导出 DOCX 和 PDF 合计约 0.6 s |
| 网络 | 采样期间进程树无外部连接（`lsof` 每 0.2 s 一次，共 15 次；不是抓包） | 不需要网络 |

### 其他发现

- **中文字体缺失。** 不配置字体时，LibreOffice 把 SimSun / SimHei 替换成希伯来文字体 FrankRuhlHofshi，PDF 里的中文一片空白，但文本层仍然存在。用 `setup_lo_profile.py` 配置后，PDF 内嵌了 NotoSerifSC 和 NotoSansSC，渲染正常。
- **PDF 中的修订。** LibreOffice 导出 PDF 时会把修订标记（删除线、下划线）一起画出来，批注默认不导出。要得到清洁的 PDF，必须先处理修订。
- **自带 Python 不可用。** `LibreOffice.app/Contents/Resources/python` 在 macOS 26 上启动即被 SIGKILL（退出码 137），在沙箱内外都一样。因此改为以宏的方式运行。
- **交叉验证。** SuperDoc 输出的 DOCX 交给 LibreOffice 打开并导出 PDF，中文、合并单元格、新增行和图片都正常。

## 没有覆盖的部分

- 没有在真实 Microsoft Word 中重新打开（测试机上没有 Word）。
- 样本是手写的，不是 Word 生成的真实模板（S03 / S08 待补）。
- 没有测试 SuperDoc 的浏览器编辑器：按 ADR-DOC-03 D2，DOC-1 不使用它。
