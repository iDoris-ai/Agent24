# Documenting 第六次发布（r6）完整用例验收

- **依据**：AGENTS.md 里 release PR 的门槛（#1293）：用全新的数据目录起真实的 daemon，通过 CLI 按用户的操作顺序，走完本次发布每个对外功能的主流程和关键异常流程。
- **对象**：ab/documenting 合进 main 的这次发布（v0.6.0-preview），即 r5（#1068）之后合进 ab/documenting 的 34 个 PR。哪些对外、哪些不跑用例，见下面「范围」一节。
- **怎么跑**：在发布 head 的 checkout 里运行下面的命令。脚本从这个 checkout 取 `domain-os.yml`、pdfkit 引擎源码和 S01 样本来组装 documents 包。

  ```
  scripts/doc1-r6-accept.sh --bin <二进制目录> [--work <目录>]
  ```

  - 二进制目录里要有 `agent24`、`agent24d`、`agent24-documents`（`cargo build -p agent24-cli -p agent24d -p agent24-documents`）。
  - 脚本逐条打出 PASS、KNOWN 或 FAIL，并在工作目录写 `report.md`。有一条 FAIL 就以非 0 退出；KNOWN 不算失败。
- **按用户的操作走**：
  - 包按模块 README 的方式组装：`domain-os.yml`、模块二进制，再加上 `engines/pdfkit/build.sh` 编出的读取引擎。
  - 之后依次运行 `agent24 os install`、`agent24 daemon start`、`agent24 documents …`。
  - 桌面端走的 `--host-stdin` 入口也按桌面端的传法跑（U6、O11）。#1293 规定，CLI 与桌面端共用这部分业务逻辑，CLI 用例通过即视为桌面端这部分通过。
- **只有 T1、T2 直接调 HTTP**：
  - T1 读 `GET /api/v1/tools`，这是 agent 能看到的工具列表，CLI 没有对应入口；
  - T2 从外部访问模块内部的工具路由。
- **只有 C1 动数据目录**：把 `blobs/tmp` 暂时设成不可写，造出一个存储失败的导入，然后马上改回去。这个失败是真实的，用户遇到磁盘或权限问题时就会这样失败；而运行中的导入只有约 0.1 秒，靠抢时机取消没法复现。
- **不碰用户数据**：daemon 和 CLI 都用工作目录下的 `AGENT24_DATA_DIR`，调用者 shell 里的 `A24_*`、`AGENT24_*` 全部清掉。Z1 检查 `~/.agent24` 没有变化；脚本发现自己的 daemon 在跑时，会在创建任何东西之前直接拒绝运行。`--work` 不能在 `~/.agent24` 下面，必须不存在或为空。
- **不留进程**：脚本无论怎样退出，都会 `agent24 daemon stop` 停掉自己起的 daemon。停不掉时，按状态文件里的 pid 强杀。
- **不需要模型**：抽取要用模型，但它只能由 agent 工具触发，不在本次发布的对外范围内（见 T1）。
- **依赖**：macOS（pdfkit 引擎）、swiftc、jq、curl、perl（给 CLI 限时、计时）、python3（解析路径）。

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
| #1313、#1321、#1345 | CLI 与桌面端共用的导入实现，`agent24 documents import` | 是 | P1–P3、U1–U7、N3、N5 |
| #1334 | 其余操作的 CLI：capabilities、list、get、text、job、cancel、retry、request | 是 | O1–O14、N1、N2、N4 |
| #1325、#1343 | 桌面端导入和文档操作改为调用上面的 CLI | 是，与 CLI 共用 | U6、O11（桌面端的传法） |
| #1186 | 导入的取消与重试 | 是 | C1–C5（主流程），O7、O8（已结束的作业） |
| #1192、#1218 | 文档详情与文字预览；关闭详情后焦点回到页面标题 | 详情和文字的数据是对外的；焦点属于纯界面 | O5、O6；焦点不跑用例 |
| #1090、#1097、#1098、#1099、#1104、#1109 | 给 agent 用的文档工具：list、get、job_get、read_range、find、extraction_get、extract | 本次不对外：`domain-os.yml` 没有声明 `tools:`，agent 看不到这些工具 | T1（KNOWN，#705）、T2 |
| #1126、#1194、#1201、#1206、#1213、#1234、#1240 | 文档工具按 authorized_resources 校验，以及授权结束后停掉在途的调用、作业和构建 | 本次不对外：只有工具调用才会走到 | 同上，不另跑用例 |
| #1092、#1096 | 存储重开与调用方脱钩 | 用户看不到（内部可靠性） | 不跑用例 |
| #906、#1080、#1082、#1293 | 设计与流程文档 | 否 | 不跑用例 |
| #1106、#1212、#1214、#1217 | 只改测试 | 否 | 不跑用例 |
| #1066、#1356，以及 DOC-1-47 | 吸收 main | 内核部分随 kernel 发布另验（`docs/agent/K1-R2-ACCEPTANCE.md`） | 不另跑；模块就绪变慢的影响见上一节 |

## 用例

| ID | 做什么 | 期望 |
|---|---|---|
| P1 | `agent24 os install <包>` | 退出 0，说 installed |
| P2 | `agent24 daemon start`（全新数据目录） | 退出 0，说 daemon started |
| P3 | 反复 `agent24 documents capabilities`，直到成功 | 30 秒内成功；记下用了多少毫秒 |
| U1 | `documents import <PDF>` | 退出 0，打出 `import job job_… (状态)` |
| U2 | 再导入同一个文件 | 同一个 job |
| U3 | `import --json`，文件超过一个分块（768 KiB） | 至少 2 行 progress，最后一行是 `ok: true` 的 result |
| U4 | 导入不存在的文件 | 退出 1，`error: file_unreadable: the file could not be read`，输出里不带路径 |
| U5 | `import --json` 一个文本文件 | 退出 1，422 `unsupported_format`，`retryable: false` |
| U6 | `import --host-stdin --json`（桌面端的传法） | 退出 0，与 U1 同一个 job |
| U7 | `--host-stdin` 带错的 token | 退出 1，`error: unauthorized: …` |
| O1 | `documents capabilities` | 存储 `ready`，pdfkit 解析引擎 `ready` |
| O2 | `documents job <U1 的 job>`，等它结束 | 一行 `import job … (succeeded)`；U3 的 job 也成功 |
| O3 | `documents list --json` | 两个文档，最早导入的排在最后 |
| O4 | `list --limit 1`，再用 `--cursor` 翻下一页 | 每页一个，第二页是另一个文档 |
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
| C1 | 把 `blobs/tmp` 设成不可写，导入两个文件 | 两个作业都是 `failed`，`storage_unavailable`，attempt 1；之后把目录改回可写 |
| C2 | `documents retry` 第一个作业 | 退出 0，第 2 次尝试成功，`error` 清空 |
| C3 | `documents cancel` 第二个作业（失败的） | 退出 0，变成 `cancelled` |
| C4 | `documents retry` 这个取消了的作业 | 退出 0，重新跑并成功 |
| C5 | `documents list` | 4 个文档（重试的两个也在） |
| T1 | `GET /api/v1/tools` | 期望里面有 documents 工具；现在没有，记 KNOWN（#705：`domain-os.yml` 还没有声明 `tools:`）。补上声明后这条会变成 PASS |
| T2 | 从外部 `POST /api/v1/documents/_a24/tools/find` | 404 `not_found`：模块内部的工具路由不对外 |
| N1 | `agent24 daemon stop`，再 `documents list` | 退出 1，说没有在跑的 daemon，并提示 `agent24 daemon start`；不会顺手起一个 |
| N2 | 没有 daemon 时 `job --json` | 退出 1，只有一行 result，`backend_not_ready` |
| N3 | 没有 daemon 时 `import --json` | 同上 |
| N4 | 没有 daemon 时 `list --limit 0 --json` | `invalid_request`，不是 `backend_not_ready`（请求本身不对，先说这个） |
| N5 | `import --host-stdin --json`，stdin 不是 JSON | 退出 1，一行 result，`invalid_request` |
| Z1 | 比较 `~/.agent24` 运行前后 | 没有变化 |

## 反面对照

拿上一次发布的二进制（main）跑同一个脚本，确认用例真会失败。main 的 `agent24` 没有 `documents` 子命令，所以除了不依赖它的 P1、P2、T2、Z1，其余用例都应该是 FAIL。结果写在发布 PR 里。
