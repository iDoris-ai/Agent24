# 文档 × 知识库（Documenting · Knowledge · WeKnora）验收测试用例

> 给 jason 在真机上做发布验收用。这份文档从**用户能看到、能操作的行为**出发，不要求读代码。
> 每条用例写明前置、操作和预期，结果栏打 ✅ / ❌；打 ❌ 的项附截图和终端输出，贴到 release PR 或对应 issue。

## 0. 说明

### 0.1 验收对象

| 项 | 内容 |
|---|---|
| 版本 | 下一版预览：`ab/documenting` 和 `ab/weknora` 都合入 main 之后的 release head，由 `apps/desktop/scripts/release-mac.sh` 出正式签名、公证的 DMG |
| release head | （发布时填） |
| DMG sha256 | （发布时填，与 Release 页的 SHA256SUMS 一致） |
| 文档线（Documenting） | 安装包自带 documents 模块（0.2.0），首次启动自动安装、异版本自动升级（T1-1：#1560、#1562、#1563）；正式流水线编译、签名模块（T1-3：#1564）；模块管理页停用、卸载、重新安装，卸载后启动不装回（T1-2：#1565）；运行中停用后文档页显示「已停用」（#1576） |
| 知识线（Knowledge） | 主对话检索 WeKnora 知识库并带来源（KB-R1、KB-R2：#1516、#1519、#1520、#1524）；桌面接管命令行起的 daemon，使知识库在这种情况下也生效（T1-0：#1559）；故障与撤权负例的脚本覆盖（T1-5：#1561） |
| WeKnora（fork） | `iDoris-ai/WeKnora` 的 `a24/develop`，提供 Agent24 专用的检索、读回接口 `/api/v1/agent24/knowledge/*` 和只对单个库有效的检索密钥 |

两条线以前分别在 `ab/documenting` 和 `ab/weknora` 上验证，**合在一起的包还没有人测过**。第 G 组的跨线用例就是为此加的，请务必做。

### 0.2 已经验证过什么（供参考，不代替本次验收）

| 范围 | 在哪个包上 | 结果 |
|---|---|---|
| 文档 + 知识库同一环境可用、问答带来源、英文问题、新对话隔离、开关关闭后重启仍关闭（下文 E 组的前身） | 候选包 r2（7f8c41e5，ad-hoc 签名），David 真机，11-02 | 全部通过 |
| 随包安装、升级、已是最新不重装、全新数据目录、装不上不挡启动（下文 A 组） | 候选包 t1（6a9e20f2，ad-hoc 签名），David 真机，11-04 | 全部通过 |
| 停用、卸载、装回（下文 B 组） | 同上 | 第一次测时，运行中停用后文档页显示 `module_stopping` 而不是「已停用」，已由 #1576 修复；修复后的真机重测和其余几项还在进行 |
| 接管命令行 daemon（下文 F 组） | T1-0 候选包（3281df06），David 真机 | 不用模型的反面用例已做，用知识库的几项还没做 |

以上都是 ad-hoc 签名的包。**签名、公证和模块验签（A-01、A-04）只能在 jason 用正式流水线出的包上验。**

### 0.3 判定约定

- ✅ 与预期一致；❌ 不一致，附截图；**K** 是已知局限（第 9 节），不算失败；**S** 是没做，要写明原因。
- 预期里引号中的文字是界面原文（中文界面）。英文界面对应的文字见各条后的括号。
- 「贴回」是指把终端输出原样贴进结果，不用自己判断对错。

## 1. 环境准备（一次，约 20 分钟）

### 1.1 机器和数据

- Apple Silicon 的 Mac，macOS 14 或更新。
- **先备份**，两处都要备份：数据目录 `~/.agent24`，以及 App 的 userData 目录（知识库设置、已保存的密钥、documents 卸载标记都在这里，不在 `~/.agent24`）。
  - userData 的目录名取自 App 包内 `package.json` 的 `name`，因为打包后那份 `package.json` 里没有 `productName`。所以目录是 `@agent24/desktop`，不是 `Agent24`。2026-11-04 在 ad-hoc 包的真机上核实过：卸载标记就落在这个目录里。
  - 先确认这个目录存在，下面各处用 `$UD` 指代它：

  ```sh
  UD="$HOME/Library/Application Support/@agent24/desktop"
  ls -d "$UD"
  ```

  - `ls` 报 `No such file` 时，用 `ls ~/Library/Application\ Support | grep -i agent24` 找出实际的目录，改掉 `UD`，并把这件事记进结果。然后备份：

  ```sh
  cp -R ~/.agent24 ~/agent24-backup-uat
  cp -R "$UD" ~/agent24-backup-uat-userdata
  ```

  测完想回到原状态，见第 13 节最后一段。
- 确认没有装 Agent24 服务（F-04 会临时装一次）。贴回：

  ```sh
  launchctl print gui/$(id -u)/ai.auraai.agent24 2>&1 | head -1; ls ~/Library/LaunchAgents/ai.auraai.agent24.plist 2>&1
  ```

  预期两行分别是 `Could not find service` 和 `No such file`。

### 1.2 样本

用仓库里的 S01 样本 `docs/documenting/samples/s01/`，共 7 份 PDF 和 1 张扫描件 JPG。仓库里每份都叫 `source.pdf` 或 `source.jpg`，导入后标题就是文件名，所以先按样本名拷一份出来（在仓库根目录执行）：

```sh
mkdir -p ~/agent24-uat/samples
for d in docs/documenting/samples/s01/*/; do n=$(basename "$d"); for f in "$d"source.pdf "$d"source.jpg; do [ -f "$f" ] && cp "$f" ~/agent24-uat/samples/"$n.${f##*.}"; done; done
ls ~/agent24-uat/samples
```

最后一行应列出 8 个文件。

下面用到的几份：

| 文件 | 内容 |
|---|---|
| `s01-01-zh-sh-fee-2025.pdf` | 上海证照工本费（中文） |
| `s01-02-en-epa-boil-water.pdf` | 美国 EPA 煮沸饮用水通知（英文，3 页） |
| `s01-03-zh-holiday-2026.pdf` | 2026 年节假日安排（中文） |
| `s01-06-zh-holiday-2026-scan.jpg` | 上一份的扫描件，没有文本层，只能靠 OCR |

### 1.3 模型

知识库问答要一个能调用工具的本地模型。我们回归时用的是 Ollama 上的 `qwen3.8:27b`（11434 端口）。没有本地模型时，第 A–C 组和第 F 组里不问问题的步骤照样能做。

下文「按 1.3 启动」一律指在终端 1 里这样启动。日志写进 `~/agent24-uat/app.log`，**每次启动都覆盖上一次的**，所以下面查日志看的都只是这一次启动。终端不要关，退出 App 用 ⌘Q：

```sh
mkdir -p ~/agent24-uat
DEFAULT_MODEL=qwen3.8:27b OMLX_URL=http://127.0.0.1:1 /Applications/Agent24.app/Contents/MacOS/Agent24 2>&1 | tee ~/agent24-uat/app.log
```

- 用 oMLX 的话，把两个变量换成你平时的设置。
- 再开一个终端（下文叫「终端 2」）跑命令。先把 App 自带的 `agent24` 命令放进 PATH，再定义查日志的 `dm` 和卸载标记的位置 `MK`：

  ```sh
  export PATH=/Applications/Agent24.app/Contents/Resources/backend:$PATH
  dm() { grep -E '\[documents-module\]|spawned pid|ready on port|taking over|refused shutdown|reusing external|not accepted by port' ~/agent24-uat/app.log; }
  UD="$HOME/Library/Application Support/@agent24/desktop"
  MK="$UD/documents-module.json"
  ```

  `UD` 用 1.1 里核实过的目录。

### 1.4 WeKnora 和样本知识库

- 一个 WeKnora fork（`a24/develop`）实例。官方镜像里没有 Agent24 用的接口，不能用。起法见 `docs/agent/KB-M5-ACCEPTANCE.md` 的「WeKnora 怎么起」：用 fork 的 `scripts/agent24/probe/build-local-app.sh` 构建镜像，app 容器设 `AGENT24_KNOWLEDGE_TAG_KEY`。
- **建数据用 standalone，Agent24 连 integration。** 同一个数据库上起两个 app：standalone app 用来建库、导入 S01、建检索密钥，Web 界面也在这里；integration app（`AGENT24_KNOWLEDGE_MODE=integration`）只给 Agent24 用。下文 D-02 填的接口地址是 **integration app** 的地址。App 只认 integration 模式的服务端，填 standalone 的地址，知识库会被判为不兼容，E、W、F、G 组都会失败。
- 里面要有一个导入了 S01 样本的知识库，再加一把**只对这个库有效**的检索密钥。W-01 还要用到第二个库的 ID（随便一个别的库）。
- 由 Knowledge 线准备演示栈，交给你四样东西：接口地址、知识库 ID、检索密钥，以及**访问方式**。**密钥不要写进仓库和 PR**。
- 访问方式：演示栈跑在 Knowledge 线的构建机上，只监听那台机器的本机端口。设置页只接受 https 或 `http://127.0.0.1`，所以要先把端口转发到你自己机器的 127.0.0.1。两种做法，验收前和 Knowledge 线约定用哪一种：
  - 用一把只能转发这个端口的 ssh key 连过去，`ssh -N -L <端口>:127.0.0.1:<端口> <构建机>`，终端保持不关；
  - 按 `docs/agent/KB-M5-ACCEPTANCE.md` 自己起一套 fork 栈，再导入 S01。
- 演示栈不是常驻服务（构建机重启后要手动拉起），验收前先和 Knowledge 线约好时间，确认它在线。
- 演示栈只开了关键词检索，问题里要带原文里的词，才问得到。

## 2. A 组：安装、首次启动、升级

| # | 场景 | 操作 | 预期 | 结果 |
|---|---|---|---|---|
| A-01 | 正式签名与公证 | 用浏览器下载 DMG（这样会带 quarantine，是真实用户的路径），先和 SHA256SUMS 比对，再拖进「应用程序」。双击打开一次，然后 ⌘Q。终端贴回：`spctl -a -vv /Applications/Agent24.app; xcrun stapler validate <下载的 DMG>` | 首次打开只有「从互联网下载」的常规确认，**没有**「无法验证开发者」或「已损坏」。`spctl` 输出 `accepted` 和 `source=Notarized Developer ID`，`stapler` 输出 `worked`。流水线只对 DMG 公证、钉票据，所以 `stapler` 验的是 DMG，不是 App | |
| A-02 | 从 v0.6.0-preview 升级（模块随 App 升级） | 前置：上一版装过手动的 documents 模块，并导入过文档。没装过就跳到 A-03，本条记 S。按 1.3 启动新 App，终端 2 贴回 `dm` 和 `grep '^version' ~/.agent24/packages/documents/domain-os.yml` | 日志有 `[documents-module] upgraded to documents 0.2.0: installed …/.agent24/packages/documents`；版本是 `version: "0.2.0"`。「能力中心 → 工具 → 模块管理」里 documents 是 `v0.2.0`、「已挂载」。「文档」页里以前导入的文档都在，点开有文字、有原页面图。升级前 documents 若是停用的，升级后仍是「已停用」（升级不改启用状态） | |
| A-03 | 首次安装（全新数据目录） | ⌘Q。终端 2 先贴回 `ls "$MK" 2>&1`，应是 `No such file`。卸载标记在 userData 里，不跟数据目录走：标记在的话，换了数据目录 App 也不会装，这条就判读不了。然后终端 1：`AGENT24_DATA_DIR=$HOME/agent24-uat/fresh /Applications/Agent24.app/Contents/MacOS/Agent24 2>&1 \| tee ~/agent24-uat/fresh.log`。终端 2：`grep documents-module ~/agent24-uat/fresh.log` | 日志有 `[documents-module] installed documents 0.2.0: installed …/agent24-uat/fresh/packages/documents`。「文档」页**不显示**「文档模块未安装」，能导入一份 PDF 并打开。做完 ⌘Q，`rm -rf ~/agent24-uat/fresh` | |
| A-04 | 模块程序带正式签名 | 终端 2 贴回：`for f in agent24-documents agent24-documents-pdfkit; do codesign -dv /Applications/Agent24.app/Contents/Resources/documents-module/bin/$f 2>&1 \| grep -E 'TeamIdentifier\|flags'; done; codesign -dv /Applications/Agent24.app 2>&1 \| grep TeamIdentifier` | 两个模块程序的 TeamIdentifier 与 App 相同，flags 里有 `runtime`。App 安装模块前会检查这两点，不满足就不装 | |
| A-05 | 已是最新时不重装 | 按 1.3 再启动一次，终端 2 贴回 `dm \| tail -3` | 这次**没有**新的 `upgraded` 或 `installed` 行，只有 `spawned pid`、`ready on port` | |
| A-06 | 模块装不上不挡启动 | ⌘Q。终端 1：`mkdir -p ~/agent24-uat/ro && chmod 500 ~/agent24-uat/ro && AGENT24_DATA_DIR=$HOME/agent24-uat/ro-data A24_OS_PACKAGES=$HOME/agent24-uat/ro/packages /Applications/Agent24.app/Contents/MacOS/Agent24 2>&1 \| tee ~/agent24-uat/ro.log`。⚠️ 这是一整行，粘贴时别被折成两行 | App 照常打开。日志有一行 `[documents-module] os install failed (rc=…)`，后面照常 `spawned pid`、`ready on port`。「文档」页显示「文档模块未安装」和「到「模块管理」页安装（Agent24 自带这个模块），装好后重启 daemon。」。做完 ⌘Q，`chmod 700 ~/agent24-uat/ro && rm -rf ~/agent24-uat/ro ~/agent24-uat/ro-data` | |

## 3. B 组：模块管理（停用、卸载、装回）

全程用默认数据目录，按 1.3 启动。`MK` 是 1.3 里定义的卸载标记位置。

| # | 场景 | 操作 | 预期 | 结果 |
|---|---|---|---|---|
| B-01 | 运行中停用 | 「模块管理」页 documents 一行点「停用」，确认。**不重启**，直接切到「文档」页。然后在菜单栏 Agent24 图标里选「重启 daemon」，再看一次「文档」页 | 确认框是「确认将领域模块「documents」由 启用 改为 停用？」。状态变成「已停用」。两次看「文档」页都只有一条「文档模块已停用。到「模块管理」页启用后即可使用。」（The documents module is turned off…），**没有** `module_stopping` 红字，也不是「未安装」 | |
| B-02 | 停用时不能卸载 | 保持停用，点「卸载」 | **不弹**确认框。页面出现红字「卸载 documents 失败：文档模块处于停用状态。请先启用再卸载（…）」 | |
| B-03 | 重新启用 | 点「启用」并确认；如果显示「需重启」，在菜单栏选「重启 daemon」 | 回到「已挂载」；「文档」页恢复正常 | |
| B-04 | 卸载（App 自己起的 daemon 正在跑） | 点「卸载」。看清确认框后点确认。终端 2 贴回：`ls ~/.agent24/packages; ls ~/.agent24/os/documents \| head -3; cat "$MK"; dm \| tail -2` | 确认框是「卸载文档模块（documents）？」，说明里写已导入的文档留在数据目录、启动时不再自动装回、daemon 会随之重启。几秒后 documents 一行变成「未安装。Agent24 自带这个模块，可在这里安装。」，带「安装」按钮。`packages` 里没有 `documents`，`os/documents` 还在；标记文件内容有 `"uninstalledByUser":true`；日志有一个新的 `spawned pid` | |
| B-05 | 卸载后重启不装回 | ⌘Q，按 1.3 再启动。终端 2 贴回 `dm \| tail -3` | 日志**没有** `installed`。「文档」页显示「文档模块未安装」和「到「模块管理」页安装（Agent24 自带这个模块），装好后重启 daemon。」，不再出现 `agent24 os install <目录>` 这种命令 | |
| B-06 | 装回 | 「模块管理」页点「安装」，确认。终端 2 贴回 `ls "$MK" 2>&1; ls ~/.agent24/packages` | 确认框是「安装 Agent24 自带的文档模块（documents）？」。几秒后回到「已挂载」、`v0.2.0`。「文档」页以前的文档都在、能打开。标记文件不在了，`packages` 里有 `documents` | |

## 4. C 组：文档导入与阅读

前置：documents 是「已挂载」。

| # | 场景 | 操作 | 预期 | 结果 |
|---|---|---|---|---|
| C-01 | 导入 PDF | 「文档」页点「导入文件（PDF、JPEG、PNG）」，选 `s01-02-en-epa-boil-water.pdf` | 最终状态是「已导入」，过程中可能闪过「上传 37%」这类上传进度（界面文字是 `import.uploading`：「上传 {percent}%」）和「导入中」（本机导入很快，看不到也算对）。列表里有这份文档，显示标题、格式和导入时间 | |
| C-02 | 读正文和原页面图 | 点开这份文档，翻到第 2、3 页，再在第 3 页点「下一页」 | 详情里有元信息（格式 · 大小 · 第 1 版 · 导入时间）和正文。正文上方是「原页面 · 第 1 页」和整页图。翻页时图和页码跟着变；在第 3 页「下一页」变灰；「上一页」能翻回 | |
| C-03 | 换一份文档从第 1 页开始 | 先导入 `s01-03-zh-holiday-2026.pdf`（E-03 也要用）。打开 EPA 那份翻到第 3 页，直接点列表里的 s01-03 | 从第 1 页开始，不残留上一份的图 | |
| C-04 | 扫描件靠 OCR 出字 | 导入 `s01-06-zh-holiday-2026-scan.jpg`，点开 | 导入成功，正文里有「春节」等文字（图片没有文本层，这些字只能来自 OCR）。刚导入时正文可能要等几秒才出来 | |
| C-05 | PNG | 把扫描件转成 PNG（`sips -s format png ~/agent24-uat/samples/s01-06-zh-holiday-2026-scan.jpg --out ~/agent24-uat/scan.png`）再导入 | 同 C-04 | |
| C-06 | 中文、带空格的文件名 | 把 `s01-01-zh-sh-fee-2025.pdf` 复制成 `工本费 通知.pdf` 再导入 | 导入成功，标题就是 `工本费 通知.pdf` | |
| C-07 | 重复导入同一个文件 | 再导入一次 C-01 的同一个文件 | 不会多出一份重复的文档（拿回的是原来的导入） | |
| C-08 | 取消、重试 | 导入一个较大的 PDF（几十 MB）。上传完、状态是「排队中」或「导入中」时，点「取消」（上传阶段没有这个按钮）；再对已取消的导入点「重试」 | 取消后状态是「已取消」；重试后重新导入并成功。本机导入很快，抢不到时机就记 S。脚本覆盖的是取消一个已失败的导入再重试（`doc1-r6-accept.sh` 的 C3、C4）；取消运行中的导入只有模块测试覆盖 | |
| C-09 | 关闭详情 | 点「关闭」 | 详情关闭，回到列表，页面上没有报错 | |

## 5. D 组：知识库设置

位置：「设置 → 连接 → 知识库（WeKnora）」。

| # | 场景 | 操作 | 预期 | 结果 |
|---|---|---|---|---|
| D-01 | 地址校验 | 地址分别填 `http://localhost:<端口>/api/v1` 和 `http://192.168.x.x:<端口>/api/v1`（任意局域网地址），其余填对，点「保存」 | 都保存不了，提示「请检查地址（https，或本机 http://127.0.0.1）、知识库 ID 和密钥。」。只接受 https，或回环 IP 字面量的 http | |
| D-02 | 正常保存 | 填 1.4 拿到的地址（`https://…/api/v1`，或 `http://127.0.0.1:<端口>/api/v1`）、知识库 ID、检索密钥，点「保存」，再勾上「在主对话中使用知识库」。这个勾选框一点就生效，不用再点「保存」 | 显示「已保存。」，状态行「已开启：主对话可以检索这些知识库。」 | |
| D-03 | 密钥不回显 | 离开设置页再回来 | 密钥框是空的，下面写「已保存密钥，留空则保持不变。」；不改密钥直接保存，之后问答照常可用 | |
| D-04 | 没填全不覆盖原设置 | 清空知识库 ID，开关保持勾上，点「保存」；然后离开设置页再回来 | 保存不了，提示「请检查地址（https，或本机 http://127.0.0.1）、知识库 ID 和密钥。」。回来后知识库 ID 还是原来的，状态行仍是「已开启：主对话可以检索这些知识库。」，原设置没被改掉 | |

## 6. E 组：主对话里的知识库问答（要模型和 WeKnora）

前置：D-02 已保存、开关开着，App 按 1.3 启动（App 自己起的 daemon）。每题开一个「新对话」。

**怎么判**：回答正文是模型写的，模型可能凭常识答对，也可能自己写出「来源」两个字。所以判断知识库**有没有用上**，只看回答**下方**那一行小字，那是 App 按这一轮实际读回了什么生成的。知识库开着且生效时，每条回答下方都有其中一行：
- 「经知识库检索回答（agent 模式）」：这一轮检索了，并读回了原文。
- 「⚠️ 这次回答没有检索知识库，不是依据知识库的内容。」
- 「⚠️ 这次检索了知识库，但没有读回原文，回答没有经过知识库核对。」

另外两种情况：
- 下方可能还多一行「⚠️ 以下来源本次没有读到，不能当作来源：…」，说明正文里列的这些来源这一轮没读过。
- 整轮失败时，回答本身就是「⚠️ 知识库回答失败：…」。

| # | 场景 | 操作 | 预期 | 结果 |
|---|---|---|---|---|
| E-01 | 中文问答带来源 | 问「上海居民身份证到期换发的工本费是多少？」 | 下方是「经知识库检索回答（agent 模式）」（Answered with Knowledge (agent mode)），没有「以下来源本次没有读到」。正文答 20 元/证，末尾「来源」列出 s01-01 或 s01-05（两份都写着「居民身份证到期换发20元/证」，引哪份都对） | |
| E-02 | 英文原题 | 问「Can the tap water be drunk during the boil water notice?」 | 下方是「经知识库检索回答」。正文说要先煮沸（煮沸 3 分钟），来源是 `s01-02-en-epa-boil-water.pdf` | |
| E-03 | 答案能在原文里核对 | 问「2026 年元旦放假是哪几天？」，再到「文档」页打开 C-03 导入的 `s01-03-zh-holiday-2026.pdf` 对照 | 下方是「经知识库检索回答」，来源是 s01-03，日期与原文一致 | |
| E-04 | 新对话不带上一段 | E-01 之后点「新对话」，问「我上一个问题问的是什么？」 | 回答说没有上一个问题，或者不知道；不会提到工本费（上一段的上下文没有带过来） | |
| E-05 | 标注跟着实际情况走 | 新对话里问一句和样本无关的话，比如「用一句话介绍你自己」 | 下方有上面三行里的一行，大概率是「⚠️ 这次回答没有检索知识库…」。模型若真去检索并读回了原文，显示「经知识库检索回答」也对。**不能**一行标注都没有 | |
| E-06 | 不再出现 500 | 新对话里再问一次 E-01 | 正常回答，没有 500「no user query」 | |
| E-07 | 关闭跨重启仍关闭 | 设置里取消勾选「在主对话中使用知识库」（一点就生效）；⌘Q，按 1.3 重新启动，再问 E-01 | 设置页仍是「已关闭：主对话不使用知识库。」。回答下方没有任何知识库标注行：既没有「经知识库检索回答」，也没有 ⚠️ 提示。做完勾回来 | |

## 7. W 组：WeKnora 侧的权限（要模型和 WeKnora）

| # | 场景 | 操作 | 预期 | 结果 |
|---|---|---|---|---|
| W-01 | 密钥只对它的库有效 | 设置里把知识库 ID 换成另一个库的 ID，密钥不变，保存。新对话问 E-01 | 下方**不是**「经知识库检索回答」，而是一条 ⚠️ 提示；或者整轮是「⚠️ 知识库回答失败：…」。正文可能凭常识答出 20 元，这不算失败，判定只看下方这一行。做完把 ID 改回来 | |
| W-02 | 密钥失效 | 在设置里把检索密钥改错一个字符并保存，新对话问 E-01 | 同 W-01，App 不会报错退出。做完换回有效密钥 | |
| W-03 | 知识库服务停了 | 停掉 WeKnora（或断开端口转发），新对话问 E-01；然后恢复，再开新对话问一次 | 停掉时同 W-01；恢复后下方是「经知识库检索回答」 | |

W 组的细节（断线、超时、取消、内容变化、提示注入、撤权、关闭后在途调用作废）由 `kb-m5-accept.py` 的 N4–N10、E7 覆盖（第 8 节），这里只看用户能察觉的结果。

## 8. F 组：App 和命令行 daemon 共存（T1-0）

| # | 场景 | 操作 | 预期 | 结果 |
|---|---|---|---|---|
| F-01 | 接管命令行起的 daemon，知识库生效 | ⌘Q。终端 2：`agent24 daemon start; agent24 daemon status`，记下 pid。按 1.3 启动 App，终端 2 贴回 `dm \| tail -3; agent24 daemon status; ps -p <旧 pid> \|\| echo old-gone`。然后新对话问 E-01 | 日志有 `taking over CLI daemon (pid <旧 pid>, port …) so Knowledge can work`，随后 `spawned pid`、`ready on port`；status 显示新的 pid；打印 `old-gone`。回答下方是「经知识库检索回答」，**不出现**「知识库已开启但当前未生效」 | |
| F-02 | App 开着时命令行停不了它 | 终端 2：`agent24 daemon stop; echo rc=$?` | 报 `host authority unavailable`，rc 不为 0，daemon 仍在跑。这是预期 | |
| F-03 | 退出 App，daemon 跟着退 | ⌘Q，等 5 秒。终端 2：`agent24 daemon status; ls ~/.agent24/daemon.json 2>&1` | `not running`、`No such file` | |
| F-04 | 装了服务的用户不被接管 | 终端 2：`agent24 service install; sleep 3; agent24 daemon status`。按 1.3 启动 App，终端 2 贴回 `dm`；鼠标悬停菜单栏图标；新对话问 E-01 | 菜单栏提示里有「外部 daemon · pid …」，并说明 `agent24 service install` 起的 daemon 上知识库不生效、要先 `agent24 service uninstall`。这次启动的日志有 `reusing external daemon`，**没有** `taking over`。回答是普通回答，下方有「⚠️ 知识库已开启但当前未生效，这次回答没有使用知识库。如果 daemon 是 `agent24 service install` 装的，请运行 `agent24 service uninstall` 后重新打开 Agent24…」 | |
| F-05 | 卸掉服务再打开就接管 | ⌘Q。终端 2：`agent24 service uninstall; ls ~/Library/LaunchAgents/ai.auraai.agent24.plist 2>&1`。按 1.3 启动，问 E-01 | plist 已不在。日志是 `taking over …`（服务卸掉时 daemon 还在跑）或直接 `spawned pid`。回答下方是「经知识库检索回答」，没有「未生效」 | |

## 9. G 组：跨线联合（两条线第一次在同一个包里）

| # | 场景 | 操作 | 预期 | 结果 |
|---|---|---|---|---|
| G-01 | 命令行 daemon 在跑时第一次启动新 App：模块安装和接管同时发生 | 前置：B-04 卸掉 documents 后清掉标记（`rm "$MK"`），或者用 A-02 那种还没升级的数据。⌘Q。终端 2：`agent24 daemon start`。按 1.3 启动 App，贴回 `dm` 的最后 5 行 | 日志里既有 `[documents-module] installed …`（或 `upgraded to …`），也有 `taking over CLI daemon …`，最后是 `spawned pid`、`ready on port`。「模块管理」里 documents「已挂载」，「文档」页可用；问 E-01，回答下方是「经知识库检索回答」 | |
| G-02 | 装了服务的用户卸载模块 | ⌘Q。终端 2：`agent24 service install`。按 1.3 启动 App（会复用外部 daemon，同 F-04）。「模块管理」点 documents 的「卸载」并确认，终端 2 贴回 `ls ~/.agent24/packages; cat "$MK"` | 卸载照常执行，确认框同 B-04。App 不会去停外部 daemon，所以 documents 一行显示「已卸载，重启 daemon 后生效」，鼠标悬停提示「daemon 是在 Agent24 之外启动的，请重启它：…」。`packages` 里没有 `documents`，标记文件已写 | |
| G-02b | 收尾：卸掉服务、装回模块 | ⌘Q。终端 2：`agent24 service uninstall`。按 1.3 启动，「模块管理」点「安装」并确认 | 启动后 documents 是「未安装…」（标记生效，不自动装回）；点「安装」后回到「已挂载」 | |
| G-03 | 文档模块停用，不影响知识库 | 停用 documents（B-01），新对话问 E-01；然后重新启用 | 回答下方照常是「经知识库检索回答」 | |
| G-04 | 同一会话里来回用 | 在「文档」页导入一份 PDF 并打开，切到助手问 E-02，再回「文档」页翻页 | 全程没有报错弹窗；两边各自正常 | |

## 10. 自动化用例（发布负责人在 release head 上跑，jason 可选）

这些用例不需要界面，脚本按用户的操作顺序在全新数据目录里起真实 daemon，逐条打 PASS / FAIL / KNOWN。命令和用例表见各自的文档。

| 脚本 | 覆盖 | 用例文档 | 最近一次结果 |
|---|---|---|---|
| `scripts/doc1-r6-accept.sh --bin <目录> --old <上一版的目录> --next` | 文档 CLI 与桌面共用的导入、读取、取消、重试、查找、渲染、抽取，以及升级和退回 | `docs/documenting/acceptance/R6-RELEASE-USECASES.md` | 7f8c41e5：60 PASS、1 KNOWN（T1，#705）、0 FAIL（没带 `--old`） |
| `python3 -I scripts/kb-r1-accept.py --old <目录> --new <目录>` | 桌面托管 daemon 的鉴权、knowledge binding 的激活与停用、非宿主调用被拒 | `docs/agent/KB-R1-ACCEPTANCE.md` | 7f8c41e5：21 PASS、4 FAIL，4 条都是已知项（W2r、W3r、W5r 的反面对照已过时，W4b 来自 #1497） |
| `python3 -I scripts/kb-m5-accept.py --bin <目录> --weknora <地址>` | 样本库端到端：检索、读回、引用、同源例外，以及断线、超时、取消、内容变化、提示注入、在途关闭、撤权、关闭跨重启 | `docs/agent/KB-M5-ACCEPTANCE.md` | #1561 head 757ecc9e：18/18 PASS（stub 模型） |

参数里的「目录」是一个装着 `agent24`、`agent24d`、`agent24-documents` 三个程序的目录（kb-m5 只要 `agent24d`）。用包内的二进制时，三个程序在 App 里分在两处，要先拷到一个目录：

```sh
mkdir -p ~/agent24-uat/bin
cp /Applications/Agent24.app/Contents/Resources/backend/agent24 /Applications/Agent24.app/Contents/Resources/backend/agent24d ~/agent24-uat/bin/
cp /Applications/Agent24.app/Contents/Resources/documents-module/bin/agent24-documents ~/agent24-uat/bin/
```

**要求**：
- 发布时用 release head 和包内的二进制重跑这三个脚本。
- 报告第一行写 head 和二进制的 sha256。
- `doc1-r6-accept.sh` 要带 `--old`，指向上一次发布的二进制。

## 11. 已知局限（不算失败）

- 文档工具在对话里不可用：`domain-os.yml` 还没声明 `tools:`，agent 看不到文档工具（#705）。所以助手不能直接读「文档」页里的文件；知识库问答读的是 WeKnora 里的库。
- 回答内容可能超出来源。「经知识库检索回答」只说明读回了原文，不保证每句话都忠于原文（#1519 不核对忠实度）。只用于样本数据，开发者预览。
- 引用是纯文本，不能点开跳到原文。
- 命令行 `agent24 daemon stop` 停不了 App 起的或接管后的 daemon（F-02）；退出 App 时 daemon 跟着退出（F-03）。
- 用 `agent24 service install` 装了服务的用户，知识库不生效，要 `service uninstall` 后重开 App（F-04）。
- 卸载、安装 documents 时，App 会先停自己起的 daemon，跑完再拉起，中间几秒连不上。
- 升级 documents，或者在模块管理页重新安装，会撤销它的授权。重装同一个包、模块文件没变时不撤销。
- 详情页：中间某页渲染失败会挡住后面的页（正常文档碰不到）；没有缩放、标注、选区。
- 换地址或换知识库后，桌面会开新会话；daemon 重启后，旧会话第一次用知识库时会换成新会话，上下文不带过去。用过知识库的会话不能再调 `http_fetch` 这类外发工具。

## 12. 不在本次验收范围

- 能力中心两栏、改名（ADR-039 §6，后续再做）。
- 知识库的 CLI 入口（宿主私有路由，CLI 调用回 403，按设计如此）。
- Design（Open Design）组件。
- 自动更新。

## 13. 结果回填

把下面这段贴到 release PR 或对应 issue：

```
验收人：jason    日期：        release head：        DMG sha256：
A 组：A-01 _ A-02 _ A-03 _ A-04 _ A-05 _ A-06 _
B 组：B-01 _ B-02 _ B-03 _ B-04 _ B-05 _ B-06 _
C 组：C-01 _ C-02 _ C-03 _ C-04 _ C-05 _ C-06 _ C-07 _ C-08 _ C-09 _
D 组：D-01 _ D-02 _ D-03 _ D-04 _
E 组：E-01 _ E-02 _ E-03 _ E-04 _ E-05 _ E-06 _ E-07 _
W 组：W-01 _ W-02 _ W-03 _
F 组：F-01 _ F-02 _ F-03 _ F-04 _ F-05 _
G 组：G-01 _ G-02 _ G-02b _ G-03 _ G-04 _
❌ 项与截图：
S 项与原因：
```

**回到测前状态**：先 ⌘Q 退出 App，确认 `agent24 daemon status` 是 `not running`，然后把 1.1 的两份备份拷回原处（`UD` 用 1.1 里核实过的目录）：

```sh
rm -rf ~/.agent24 && cp -R ~/agent24-backup-uat ~/.agent24
rm -rf "$UD" && cp -R ~/agent24-backup-uat-userdata "$UD"
```
