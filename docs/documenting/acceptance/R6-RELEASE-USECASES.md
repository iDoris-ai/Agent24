# Documenting 第六次发布（r6）完整用例验收

- **依据**：AGENTS.md 里 release PR 的门槛（#1293）：用全新的数据目录起真实的 daemon，通过 CLI 按用户的操作顺序，走完本次发布每个对外功能的主流程和关键异常流程。
- **对象**：ab/documenting 合进 main 的这次发布（v0.6.0-preview），即 r5（#1068）之后合进 ab/documenting 的 34 个 PR。哪些对外、哪些不跑用例，见下面「范围」一节。
- **怎么跑**：在发布 head 的 checkout 里运行下面的命令。脚本从这个 checkout 取 `domain-os.yml`、pdfkit 引擎源码和 S01 样本来组装 documents 包。

  ```
  scripts/doc1-r6-accept.sh --bin <二进制目录> [--old <main 的二进制目录>] [--work <目录>]
  ```

  - 二进制目录里要有 `agent24`、`agent24d`、`agent24-documents`（`cargo build -p agent24-cli -p agent24d -p agent24-documents`）。
  - `--old` 指向用 main（r5）编出的同样三个二进制。给了它，才跑升级用例 M1–M7（见「从 r5 升级」一节），这时还需要 sqlite3。发布验收要带上它。
  - 脚本逐条打出 PASS、KNOWN 或 FAIL，并在工作目录写 `report.md`。有一条 FAIL 就以非 0 退出；KNOWN 不算失败。
- **按用户的操作走**：
  - 包按模块 README 的方式组装：`domain-os.yml`、模块二进制，再加上 `engines/pdfkit/build.sh` 编出的读取引擎。
  - 之后依次运行 `agent24 os install`、`agent24 daemon start`、`agent24 documents …`。
  - 桌面端走的 `--host-stdin` 入口也按桌面端的传法跑（U6、O11）。#1293 规定，CLI 与桌面端共用这部分业务逻辑，CLI 用例通过即视为桌面端这部分通过。
- **只有 T1、T2 直接调 HTTP**：
  - T1 读 `GET /api/v1/tools`，这是 agent 能看到的工具列表，CLI 没有对应入口；
  - T2 从外部访问模块内部的工具路由。
- **只有 C1 动数据目录**：把 `blobs/tmp` 暂时设成不可写，造出一个存储失败的导入，然后马上改回去。这个失败是真实的，用户遇到磁盘或权限问题时就会这样失败；而运行中的导入只有约 0.1 秒，靠抢时机取消没法复现。
- **不碰用户数据**：daemon 和 CLI 都用工作目录下的 `AGENT24_DATA_DIR`，调用者 shell 里的 `A24_*`、`AGENT24_*` 以及 `AGENT24D_BIN` 这类变量全部清掉，不然 CLI 会起 `AGENT24D_BIN` 指的那个 daemon，而不是 `--bin` 里的。Z1 比较 `~/.agent24` 下每一项的路径、大小和修改时间，运行前后必须完全一样，有读不了的项也算失败；脚本发现自己的 daemon 在跑时，会在创建任何东西之前直接拒绝运行。`--work` 不能在 `~/.agent24` 下面，必须不存在或为空。
- **不留进程**：脚本无论怎样退出，都会对三个数据目录（主用例的、E 组的 `bare/data`、升级用例的 `up/data`）各做一次 `agent24 daemon stop`。停不掉时，按状态文件里的 pid 强杀。
- **不需要模型**：抽取要用模型，但它只能由 agent 工具触发，不在本次发布的对外范围内（见 T1）。
- **依赖**：macOS（pdfkit 引擎）、swiftc、jq、curl、perl（给 CLI 限时、计时）、python3（解析路径）、sips（U9 把样本转成 PNG，macOS 自带）；带 `--old` 时还要 sqlite3。

## 模块就绪要等一下

`agent24 daemon start` 返回时，documents 模块还没完成握手。这段时间里，`agent24 os list` 已经显示 `[mounted]`，但请求会得到 `module_not_ready`，提示是等一下再试。导入客户端对这个码连试 3 次，中间不停顿，熬不过这段时间。

本机 debug 构建实测，从 `daemon start` 返回到 `documents capabilities` 首次成功：

| 构建 | 结果（3 次） |
|---|---|
| r5 之后、合 main 之前（8f17863c） | 68、230、234 ms |
| 合 main 之后（c727acbd） | 1347、1315、1310 ms |

多出来的约 1.1 秒，来自内核 K1-6a-G 在启动模块前对整个包算制品身份（`agent24-os-proto` 的 `artifact::identity`）。这次的包里有一个 36 MB 的 debug 模块二进制，哈希就在这一步算。

所以 P3 先等模块回答（最多 30 秒）并记下用了多久，后面的用例都在模块就绪之后跑。r5 时期的用例脚本只等 daemon 在跑，在合 main 之后的二进制上会稳定失败 13 条（都是 `module_not_ready` 及其连带失败），在合 main 之前的二进制上只是偶尔失败。

## 范围

| PR | 内容 | 对外？ | 用例 |
|---|---|---|---|
| #1313、#1321、#1345 | CLI 与桌面端共用的导入实现，`agent24 documents import` | 是 | P1–P3、U1–U10、N3、N5 |
| #1334 | 其余操作的 CLI：capabilities、list、get、text、job、cancel、retry、request | 是 | O1–O15、N1、N2、N4、E1–E3 |
| #1325、#1343 | 桌面端导入和文档操作改为调用上面的 CLI | 是，与 CLI 共用 | U6、O11（桌面端的传法） |
| #1186 | 导入的取消与重试 | 是 | C1–C5（主流程），O7、O8（已结束的作业） |
| #1192、#1218 | 文档详情与文字预览；关闭详情后焦点回到页面标题 | 详情和文字的数据是对外的；焦点属于纯界面 | O5、O6、U8–U10、E3；焦点不跑用例 |
| #1090、#1097、#1098、#1099、#1104、#1109 | 给 agent 用的文档工具：list、get、job_get、read_range、find、extraction_get、extract | 本次不对外：`domain-os.yml` 没有声明 `tools:`，agent 看不到这些工具 | T1（KNOWN，#705）、T2 |
| #1126、#1194、#1201、#1206、#1213、#1234、#1240 | 文档工具按 authorized_resources 校验，以及授权结束后停掉在途的调用、作业和构建 | 本次不对外：只有工具调用才会走到 | 同上，不另跑用例 |
| #1092、#1096 | 存储重开与调用方脱钩 | 用户看不到（内部可靠性） | 不跑用例 |
| #906、#1080、#1082、#1293 | 设计与流程文档 | 否 | 不跑用例 |
| #1106、#1212、#1214、#1217 | 只改测试 | 否 | 不跑用例 |
| #1066、#1356，以及 DOC-1-47 | 吸收 main | 内核部分随 kernel 发布另验（用例在 ab/kernel 上的 `docs/agent/K1-R2-ACCEPTANCE.md`，#1335，本分支还没有） | 不另跑；模块就绪变慢的影响见上一节 |

## 用例

| ID | 做什么 | 期望 |
|---|---|---|
| P1 | `agent24 os install <包>` | 退出 0，说 installed |
| P2 | `agent24 daemon start`（全新数据目录） | 退出 0，说 daemon started |
| P3 | 反复 `agent24 documents capabilities`，直到成功 | 从 `daemon start` 返回算起 30 秒内成功（按实测的毫秒数判定） |
| U1 | `documents import <PDF>` | 退出 0，打出 `import job job_… (状态)` |
| U2 | 再导入同一个文件 | 与 U1 同一个 job（U1 没拿到 job 时这条也失败） |
| U3 | `import --json`，文件超过一个分块（768 KiB） | 有一行 progress 在 786432 字节处（第二块开始发送），一共至少 3 行；最后一行是 `ok: true` 的 result |
| U4 | 导入不存在的文件 | 退出 1，`error: file_unreadable: the file could not be read`，输出里不带路径 |
| U5 | `import --json` 一个文本文件 | 退出 1，422 `unsupported_format`，`retryable: false` |
| U6 | `import --host-stdin --json`（桌面端的传法） | 退出 0，与 U1 同一个 job（U1 没拿到 job 时这条也失败） |
| U7 | `--host-stdin` 带错的 token | 退出 1，`error: unauthorized: …` |
| O1 | `documents capabilities` | 存储 `ready`，pdfkit 解析引擎 `ready` |
| O2 | `documents job <U1 的 job>`，等它结束 | 一行 `import job … (succeeded)`；U3 的 job 也成功 |
| O3 | `documents list --json` | 两个文档，U1 的排在最后（U3 之前先等 U1 的作业结束，所以它一定更早） |
| O4 | `list --limit 1`，再用 `--cursor` 翻下一页 | 两页各一个文档：第一页不是 U1 的文档，第二页是 |
| O5 | `documents get <doc>` | 缩进的 JSON，`document_id` 对得上，`head_revision` 为 1 |
| O6 | `documents text <doc> 1 --json` | 文字块里有样本 PDF 自己的字（「boil」） |
| O7 | `documents cancel` 一个已成功的作业 | 退出 0，作业不变（`succeeded`，attempt 1） |
| O8 | `documents retry` 一个已成功的作业 | 退出 1，`error: invalid_request: a succeeded job cannot be retried`，作业仍是 `succeeded` |
| O9 | `documents get ../../runs` | 在发出之前就拒绝：`invalid_request: not an allowed documents operation` |
| O10 | `list --limit 0 --json` | 退出 1，一行 result，`invalid_request` |
| O11 | `request --host-stdin --json` 发 `get`（桌面端的传法） | 退出 0，同一个文档 |
| O12 | `request '{"op":"proxy",…}'` | 退出 1，`invalid_request: not an allowed documents operation` |
| O13 | `request --host-stdin` 带错的 token | 退出 1，`error: unauthorized: …` |
| O14 | `documents job` 一个不存在的 id | 退出 1，`error: not_found: no such job` |
| O15 | `documents get` 一个不存在的文档 | 退出 1，`error: not_found: no such document` |
| C1 | 把 `blobs/tmp` 设成不可写，导入两个文件 | 两个作业都是 `failed`，都是 `storage_unavailable`，attempt 1；之后把目录改回可写（运行被打断时，退出前也会改回） |
| C2 | `documents retry` 第一个作业 | 退出 0，第 2 次尝试成功，`error` 清空 |
| C3 | `documents cancel` 第二个作业（失败的） | 退出 0，变成 `cancelled` |
| C4 | `documents retry` 这个取消了的作业 | 退出 0，重新跑并成功 |
| C5 | `documents list` | 4 个文档，重试成功的两个作业各自的文档都在其中 |
| U8 | 导入一张 JPG 扫描件（S01 的 2026 年节假日通知），读第 1 页文字 | 作业成功，`media_type` 为 `image/jpeg`；文字里有「春节」。图片没有文本层，这两个字只能来自 OCR。首次读取时文字层还在生成，会先得到 `engine_unavailable`，脚本对这个码重试，60 秒后不再发起新的读取（按实际经过的时间算，不按次数：每次读取在服务端最多要等 8 秒，最后一次读取也可能再多等这么久）；判定看的是文字，不是用时。其他错误码立即判失败 |
| U9 | 同一张扫描件转成 PNG（`sips`）再导入 | 同上，`media_type` 为 `image/png` |
| U10 | 把 U1 的 PDF 复制一份，改名为 `煮沸 通知.pdf`（中文加空格）再导入。文件名是导入幂等键的一部分，所以这是一次新的导入，不会拿回 U1 的作业 | 作业成功，文档标题就是 `煮沸 通知.pdf` |
| T1 | `GET /api/v1/tools` | 期望里面有 documents 工具。现在列表正常返回（200，非空）但其中没有，记 KNOWN（#705：`domain-os.yml` 还没有声明 `tools:`）；补上声明后这条会变成 PASS。列表本身出错（非 200、没有列表）记 FAIL |
| T2 | 从外部 `POST /api/v1/documents/_a24/tools/find` | 404 `not_found`：模块内部的工具路由不对外 |
| N1 | `agent24 daemon stop`，再 `documents list` | 退出 1，说没有在跑的 daemon，并提示 `agent24 daemon start`；不会顺手起一个 |
| N2 | 没有 daemon 时 `job --json` | 退出 1，只有一行 result，`backend_not_ready` |
| N3 | 没有 daemon 时 `import --json` | 同上 |
| N4 | 没有 daemon 时 `list --limit 0 --json` | `invalid_request`，不是 `backend_not_ready`（请求本身不对，先说这个） |
| N5 | `import --host-stdin --json`，stdin 不是 JSON | 退出 1，一行 result，`invalid_request` |
| E1 | 另一个全新数据目录：包里只放 `domain-os.yml` 和模块二进制，不放读取引擎（漏了 `engines/pdfkit/build.sh` 这一步），安装、启动 | 都退出 0，模块就绪；`capabilities` 里存储 `ready`，`apple-pdfkit` 的 parse、render 都是 `absent` |
| E2 | 在这个模块上导入 PDF | 作业照样成功：导入不需要引擎 |
| E3 | `documents text <doc> 1 --json` | 退出 1，503 `engine_unavailable`，`engine: apple-pdfkit`，`retryable: true`，提示 `no read engine on this platform` |
| Z1 | 比较 `~/.agent24` 运行前后每一项的路径、大小、修改时间 | 完全一样 |

## 发布后追加的用例（DOC-1-55）

O15、U8–U10、E1–E3 是 r6 合进 main（#1386）之后补的。合并前用探针在发布二进制上手动试过，这里把它们写成有精确判定的用例，在合并后的 main 上正式跑。

还有四个缺口，CLI 上造不出确定的场景，不写成用例，记在这里：

| 缺口 | 为什么造不出 | 现在靠什么 |
|---|---|---|
| 取消正在运行的导入 | 运行中的导入只有约 0.1 秒，靠抢时机取消没法稳定复现（见上文 C1 的说明） | C3、C4 覆盖取消失败的作业再重试；运行中取消由模块测试 `imports/tests.rs` 的 `an_import_cancelled_before_or_after_its_bytes_are_stored_commits_nothing` 覆盖（用闸门把作业停在运行中再取消） |
| 被打断的作业再跑一次（daemon 在作业运行中退出） | 同上，作业只跑约 0.1 秒，杀 daemon 的时机抢不准 | C2 覆盖重试成功；被打断的作业由 `imports/handler_test.rs` 的 `an_interrupted_import_runs_again_and_a_cancelled_one_stays_cancelled` 覆盖（再导入时第 2 次尝试成功） |
| 上传中断后续传 | 本机分块上传每块只要几毫秒，在两块之间掐断 CLI 抢不准时机 | 重新导入同一个文件会续传：客户端用同一个幂等键拿回原来的上传，从服务端已收到的位置接着传。由客户端测试 `import/tests.rs` 的 `an_upload_cut_off_midway_resumes`、`a_lost_import_is_continued_by_importing_again` 覆盖 |
| 文字分页（`next_cursor`） | CLI 的 `text` 没有 `--limit`；实测 3000 行的 PDF 第 1 页是 49 个块，一页就返回完，造不出第二页 | O4 覆盖 `list` 的分页；`text` 的分页由 `read/text/tests.rs` 的 `cursors_page_through_every_block_once` 覆盖 |

## 从 r5 升级（M1–M7，带 `--old` 时跑）

r6 给 documents 库加了迁移 0007、0008（`job_acceptances`、`job_revocations`，都是新表，不改已有的表）。这几条用另一个全新的数据目录，先让 r5 建库，再按用户的步骤升级，最后试一次退回 r5。

- **r5 那边怎么驱动**：r5 的 CLI 没有 `documents`，所以 r5 的安装和启动用它自己的 `os install`、`daemon start`；导入和读取用本次的 CLI，它只是 REST 客户端，调的是 r5 本来就提供的接口（r5 的桌面端也这样调）。
- **r5 的包**：`domain-os.yml` 和 pdfkit 引擎从 r5 到 r6 没有变（`git diff origin/main` 为空），只换模块二进制。
- **升级步骤**：`agent24 os install` 不覆盖已装的同名包，会提示先卸载；`agent24 os uninstall` 只删 `packages/documents` 下的包文件，不动 `os/documents` 下的数据。所以用户的升级步骤是：停 daemon → `os uninstall documents` → `os install <r6 的包>` → `daemon start`。release 说明要写明这一步。
- **只有 M1、M4 直接读库**：迁移版本没有对外接口，用 sqlite3 读 `_sqlx_migrations`；M6 比较库的 `.dump` 哈希。

| ID | 做什么 | 期望 |
|---|---|---|
| M1 | 用 r5 安装、启动，导入一个 PDF，读它的文字 | 都成功；库的迁移在 6 |
| M2 | 停掉 r5，`os uninstall documents`，`os install` r6 的包，`daemon start` | 都退出 0，模块 30 秒内回答 |
| M3 | 读 r5 导入的作业、文档和文字 | 作业 `succeeded`，`head_revision` 1，文字块数与 r5 读到的相同 |
| M4 | 读迁移记录 | 最高到 8，每一步都成功（和 M1 的 6 合起来，证明确实从 6 迁到了 8） |
| M5 | 在升级后的库上再导入一个 | 成功，列表里两个文档 |
| M6 | 停掉 r6，装回 r5 的包再启动，`documents list` | 回退前库在 `8/1`；r5 不认这个库，`documents list` 退出 1，返回 `storage_unavailable`（提示语是 corrupt）；库的 `.dump` 前后一样，r5 什么都没写（读不出库时这条失败，不会把两次读失败当成「没变」） |
| M7 | 再装回 r6 | 两个文档都在 |

M1 把 r5 的库版本钉在 6。r6 合进 main 之后，main 的 documents 库就在 8 了；以后的发布要用 `--old` 指向上一次发布（r6）的二进制，并把 M1 和 M4 的版本号改成那一版的。

M6 退回的是整套二进制，r5 的内核要先能起来，这条才测得到 documents 库。`--bin` 的内核要是带了 r5 没有的 store 迁移，r5 的内核会先拒绝打开自己的库（`migration N was previously applied but is missing`），M6 失败在内核这一步；证据里会带上这条错误。r6 发布时的 main（8bba633e）没有新的 store 迁移，但之后的 main 有（#1381 的 0030），所以跑本脚本时 `--bin` 要用发布那一次的提交编，不要用之后的 main。

M6 是退回旧版的已知行为，不是缺陷：r5 打不开 r6 迁移过的库，只是提示语写的是 corrupt，容易让人以为库坏了。数据没有动，装回 r6 就恢复（M7）。release 说明里写明：升级后不要退回 r5。

## 反面对照

- 拿上一次发布的二进制（main）当 `--bin` 跑同一个脚本，确认用例真会失败。main 的 `agent24` 没有 `documents` 子命令，所以除了不依赖它的 P1、P2、T2、Z1 和 T1（列表正常返回、其中没有 documents 工具，仍是 KNOWN），其余用例都应该是 FAIL。
- 升级用例另做一次：`--old` 也指向 r6 的二进制。这时 M1（库已经在 8）和 M6（不会被拒）应该是 FAIL。

结果写在发布 PR 里。
