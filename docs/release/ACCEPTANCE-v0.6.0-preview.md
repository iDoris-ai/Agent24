# v0.6.0-preview.20261028.1 完整用例验收清单

- **日期**：2026-10-27 起草，同日在发布候选上实跑。**状态**：已执行（结果见下），真机项由 jason 在发布后按 `MANUAL-TESTS-jason-v0.6.0-preview.md` 完成。
- **依据**：#1293（`DOC-1-40`，AGENTS.md「release PR 的合并门槛」由 jason 真机验收改为完整用例验收，**合并前需 jason 同意，见 #1159**）。
- **范围**：五条线合并后的 `v0.6.0-preview.20261028.1`。合入顺序：kernel → memory-p1 → documenting → weknora → idoris（iDoris 最后；截止 2026-10-28 12:00 Asia/Bangkok，赶不上则整条 iDoris 线移出本版，配对改下一个 preview，见 #1370）：
  - kernel：**已合并 main（`03bf1e17`）**。release PR #1280（`release/kernel-k1-r2-20261026`，迁移 store 0019–0026 + memory 0018）；
  - memory-p1：**已合并 main（`c98745b0`，在 #1280 之后）**。release PR #1341（`ab/memory-p1`，迁移 memory 0019–0020 + store 0027–0029，**待 #1280 先合，再吸收 main**）；
  - documenting：**已合并 main（r6，#1386，`8bba633e`）**。`ab/documenting`（#1293 的 base；store 无迁移，documents 模块自己的库 0007–0008）；
  - weknora：**已合并 main（KB-R1，#1396，`3659317d`）**。`ab/weknora`（KB-C1 / KB-HOST-01）；
  - 另：P-13（#1381，`bb3fb6d8`）已合并 main（store 0030）。
  - idoris（第五线，#1370）：release PR #1405 已合并，合并提交 `956e9009`（`ab/idoris`；Agent24 经 iDoris 网关调用本地模型，与 iDoris v0.3.1 联合发布）。ID-8 真机发现的提示问题由 #1420 修复。
- **2026-10-27 已实跑**：在发布候选 `21f484c6`（= #1405 head = 五线合并后的代码）上跑了第 6 节一键脚本，脚本原始分类：PASS 39、FAIL 0、KNOWN 1（M-14s，缺口 G-M1）、SKIP 18。18 条 SKIP 包括：4 条 BLOCKED（K-02 对应 G-K1，M-16 对应 G-M5，D-11/D-13 对应 G-D1/G-D2，处置见第 7 节缺口表）；9 条真机项；4 条是没加 `--idoris` 的 ID-1/2/3/5（iDoris 部分由 ID-8 真机覆盖）；1 条 X-07（脚本里标为手工）。X-07 另外手工执行并通过，所以执行记录合计 PASS 40。本节之后对 X-04、K-00、D-03 断言的收紧（见 PR #1421 第二轮）只做了反例核验，没有重跑完整脚本。原样脚本的 FAIL 全部是脚本自身问题，已修进第 6 节：D 系列取 `.data.job_id`、bash 3.2 下的 `case`、D-03 改导第二个 PDF、D-06 跳过页头、ID-4 的 `idk_` 桩 key 和 `/health` 字段、X-01/05 的 `seq`、X-04 的 memory 降级、M-09～11 内存盘挂载点、ID-2 先读后写。X-07 手工跑通过（documents 6→8，store 14→30，memory 17→20），**必须用隔离 HOME**：旧 RC 的 agent24d 不认 `AGENT24_DATA_DIR`。ID-8 另外在真机上用 CLI 跑通（见 #1370 / release PR）。

## 0. 规则对照（#1293）

| #1293 的要求 | 本清单怎么落实 |
|---|---|
| 完整用例 = 按 use case 模拟用户真实操作，不是单测，不是 e2e | 每个用例写前置、步骤（CLI 或 HTTP 命令，桌面端写点哪里）、期望、判定，不引用单元测试当证据 |
| 发布负责人用全新数据目录起真实 daemon，通过 CLI 走主流程和关键异常流程 | 全部用例用 `AGENT24_DATA_DIR` 指向工作目录下的新目录；CLI 够不着的入口才直接调 daemon 的 HTTP（理由逐条写明） |
| CLI 与桌面端共用业务逻辑，CLI 用例通过即视为桌面端这部分通过；纯界面展示由人工另行确认 | 「真机」标记的用例只含纯界面展示或 CLI 够不到的桌面独有流程；凡共用代码的部分（文档导入、托管 daemon）在 CLI 侧跑 |
| CLI 入口和共用代码补齐之前，缺口部分仍在桌面端补用例并在 release PR 正文写明 | 见第 7 节「缺口」，需要写进 release PR 正文 |
| release PR 正文列出跑过的用例、命令和结果；只有用户看不到的改动和纯界面展示可以不跑用例并逐项写明；其他对外功能缺用例就不合并 | 第 8 节给出正文模板；第 6 节「一键执行」产出 `report.md`；每条线末尾有「不跑用例的改动（逐项）」 |

## 1. 判定约定与执行方式标记

- **执行方式**：
  - `脚本`：第 6 节的一键脚本会跑，也可以照步骤手工跑；
  - `CLI`：只用 CLI 或 curl 能跑，但没有放进一键脚本（原因写在用例里）；
  - `真机`：需要桌面端 GUI，或需要签名、公证后的安装包；
  - `阻塞`：现在没有办法跑（缺入口、缺示例包），属于缺口，不能算通过。
- **判定**：每个用例给「断言」。能脚本化的写成可复制的 shell 判断；写「人看」的是纯界面展示。
- **PASS / FAIL / KNOWN / SKIP**：KNOWN 指登记过的已知局限（要有 issue 号或本文第 7 节编号），不算失败；SKIP 是没跑（缺参数、缺平台），**release PR 正文必须逐条交代 SKIP**。
- **隔离**：每个 daemon 用工作目录下自己的 `AGENT24_DATA_DIR` 和 `A24_OS_PACKAGES`；不碰 `~/.agent24`（脚本末尾 Z1 检查）。开始前 `agent24 daemon stop` 停掉自己的 daemon。
- **模型**：不依赖真实模型。memory 和 chat 类用例用脚本内置的 mock 模型（一个 20 行的 python HTTP 服务，`OMLX_URL` 指向它，永远回 `pong`，并把每个请求体记到 `mock.log`，用来看「召回的内容有没有进模型请求」）。documents 的冷却用例反过来，故意指向一个不通的端口。
- **直接读写库的地方**（没有对外入口，与 #1294 同样的理由）：只有三类：读 `_sqlx_migrations`；memory 用例里把 `mem_owner_quota` 的限额调到当前用量（产品里没有改限额的入口，设计的 D8 / c6 还没做，见缺口 G-M2）；读 `mem_owner_usage`、`mem_write_rejections`、`run_message_origins` 看结果。

## 2. 公共准备

```bash
cd <五线合并后的 release head checkout>           # 本文简称 $REPO
(cd rust && cargo build -p agent24d -p agent24-cli -p agent24-documents)
NEW=$REPO/rust/target/debug                       # 发布 head 的二进制
# main 的二进制（给 kernel 的 #1294 脚本当 --old）：在 main 的 checkout 里同样编一份，记为 $MAIN
# v0.5.1-preview.20261016.2 的二进制（给升级用例当 --preview）：
git worktree add ../a24-preview v0.5.1-preview.20261016.2 && (cd ../a24-preview/rust && cargo build -p agent24d -p agent24-cli)
PREVIEW=$PWD/../a24-preview/rust/target/debug
```

- 备选：preview 二进制也可以从 `Agent24-0.5.1-preview.20261016.2-arm64.dmg` 里取（`Agent24.app/Contents/Resources/backend/` 下，路径以实际为准）；从 DMG 取更接近用户手里的东西，但 CLI 子命令可能不全，升级用例只用到 `agent24d serve`，够用。
- documents 模块包（D 线用）：

```bash
mkdir -p /tmp/documents-pkg/bin
cp rust/apps/agent24-documents/domain-os.yml /tmp/documents-pkg/
cp "$NEW/agent24-documents" /tmp/documents-pkg/bin/
rust/apps/agent24-documents/engines/pdfkit/build.sh /tmp/documents-pkg/bin    # 仅 macOS，PDF 的读/渲染引擎
```

- iDoris 线（I 组）用的真实 iDoris v0.3.1 归档（#1370 回填的 sha256 以那里为准，**先校验再解**）：

```bash
IDORIS_TAR=idoris-v0.3.1-aarch64-apple-darwin.tar.gz
gh release download v0.3.1 -R iDoris-ai/iDoris -p "$IDORIS_TAR" -D $WORK/idoris-dl
echo "<#1370 回填的 sha256>  $WORK/idoris-dl/$IDORIS_TAR" | shasum -a 256 -c      # 必须 OK，否则停
mkdir -p $WORK/idoris && tar -xzf $WORK/idoris-dl/$IDORIS_TAR -C $WORK/idoris     # 一个 idoris 二进制 + 4 份配置；不含推理后端和模型权重
IDORIS=$WORK/idoris/idoris                                                       # 以解出的实际路径为准
```

- 手工跑单个用例时的小助手（一键脚本里有等价实现）：

```bash
export AGENT24_DATA_DIR=$WORK/m A24_OS_PACKAGES=$WORK/m.pk OMLX_URL=http://127.0.0.1:$MPORT DEFAULT_MODEL=mock
"$NEW/agent24d" serve > $WORK/m.log 2>&1 &            # 或 agent24 daemon start
PORT=$(jq -r .port $AGENT24_DATA_DIR/daemon.json); TOKEN=$(jq -r .token $AGENT24_DATA_DIR/daemon.json)
a() { curl -s -m 20 -w '\n[%{http_code}]\n' -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' "$@"; }
```

## 3. 用例总表

| 线 | 用例数 | 其中 脚本 | CLI（未入脚本） | 真机 | 阻塞 |
|---|---|---|---|---|---|
| kernel | 28（引用 #1294 的 25 个 + 新增 3） | 26 | 0 | 1（K-03） | 1（K-02） |
| memory-p1 | 16 | 13（M-12 需 `--slow`，M-09～M-11 需 macOS 且 `--disk`） | 0 | 2（M-14、M-15） | 1（M-16） |
| documenting | 13 | 10（D-09 需 `--slow`） | 0 | 1（D-12） | 2（D-11、D-13） |
| weknora | 4 | 2 | 0 | 2（W-03、W-04） | 0 |
| idoris（#1370） | 7（ID-7 已移出用户路径） | 6（ID-1～ID-6，其中 ID-1/2/3/5 需 `--idoris`，ID-3 的 Settings 部分和 ID-5 的额度比对仍手工） | 0 | 1（ID-8） | 0 |
| 跨线升级 | 8 | 5 | 1（X-07） | 2（X-06、X-08） | 0 |
| 合计 | 76 | 62 | 1 | 9 | 4 |

真机清单：K-03、M-14、M-15、D-12、W-03、W-04、ID-8、X-06、X-08。阻塞清单：K-02、M-16、D-11、D-13。

## 4. 各线用例

### 4.1 kernel（#1280）

**K-00 引用 #1294 的 25 个用例**（`docs/agent/K1-R2-ACCEPTANCE.md`，`scripts/k1-r2-accept.sh`，**不重写**，下表只做对应）。脚本取 `d50c319c`（#1314 合并点，已按 #1294 评审收紧）这一版。**10-27 实跑校正**：① `--main` 要用 kernel 合并前的 main 二进制（`982725e2`，即 `03bf1e17` 的第一父提交），不是现在的 main，否则会出 8 个假 FAIL；② 借用脚本第 181 行 U3 的迁移期望值已过时，应改为 store `19..30` / memory `20`，改后结果是 25 passed、1 known（G1）、0 failed。该脚本的特点是：它只用 HTTP、`os install`、MCP、ACP，**不依赖 `agent24 runs/schedules`**（那些是 #1311/#1324，不在 #1280 里）；`ab/kernel` 现在的版本（K-77 之后）用到了发布 head 没有的 CLI，**不要用**。

| 主题 | #1294 的用例 | 说明 |
|---|---|---|
| 模块工具授予 | G1–G6、P1 | G1 在 #1280 head 上是 KNOWN（#1290，修复 #1317 进下一次发布）；G3/G4 验开关和 90 天上限；G5/G6 验 `os install`、停用时撤销授予 |
| 来源分类 | S1、S2、V3 | 0026 回填；self-wake 来源 unknown；会话 run 扣下 |
| peer 视图 | V1–V6 | HTTP peer 视图、MCP `agent24_list_runs`、ACP `session/prompt` |
| 迁移 0018–0026 升级路径 | U1–U4、D1、D2、Z1 | 从 main 的库升到发布 head；回退被拒且库不变；隔离 |
| 无 HOME | H1–H3 | `os install/uninstall` 无 HOME 时完成并说明 |
| 按引用停（协议 4） | **没有** | 见 K-02 |

- 前置：`--old` 是 main 的二进制目录，`--new` 是发布 head 的。把脚本放进 `$REPO/scripts/`（脚本靠自己的位置找 `examples/node-module`），跑完删掉，**不要提交**：`git show d50c319c9454ffa79f8ccd5de2b4b8955fab4a87:scripts/k1-r2-accept.sh > scripts/k1-r2-accept.sh && chmod +x scripts/k1-r2-accept.sh`。
- 步骤：`scripts/k1-r2-accept.sh --old "$MAIN" --new "$NEW" --work $WORK/k1`
- 期望 / 判定：退出码 0；`$WORK/k1/report.md` 末行形如 `25 passed, 1 known limitation(s), 0 failed`（G4 有两条检查，所以检查数比用例数多一；以实际末行为准，KNOWN 至多 G1 一条）。
- 对应 PR：#1294（K-71）、#1314（K-74）；被验对象 #1280。执行方式：脚本。

**K-01 授予导出**
- 前置：新数据目录，`NEW` 起 daemon。
- 步骤：`a GET /api/v1/module-consents/export`
- 期望：200，JSON 体；没有授予时为空集合，不是 5xx。
- 判定：`[ "$CODE" = 200 ] && jq -e . r.json >/dev/null`
- 对应：`consent_routes` 的 `/export`（#1160/#1172/#1176）。执行方式：脚本。

**K-02 协议 4 模块的授予、确认卡片、按引用停**（阻塞）
- 要验的：声明协议 4 的、带工具的模块：`GET .../tools/{tool}/card` 取卡片 → `POST .../tools/{tool}` 授予（开关 `A24_MODULE_TOOL_GRANTS=1`）→ 调用成功 → 撤销或到期 → 在途调用被停、模块收到带引用的结束通知（发件箱、补发、只发给本次运行：#1227、#1231、#1236）→ 重新启用后旧授予判 Stale（启用纪元，#1155）。
- **阻塞原因**：仓库里没有声明协议 4 的示例模块（`examples/node-module` 握手 `protocol_versions: {min:1,max:1}`，且协议 1 的模块调授予 POST 只会得到 409 `protocol_too_old`）。#1294 自己也把这几项列为「不在这里验」，理由是只有 CI 测试（`consent_gate.rs`、`module_tool_runtime.rs`）覆盖。
- 按 #1293「对外功能缺用例就不合并」，这一块要么补一个协议 4 的示例模块并加用例，要么由 David 明确判它属于「用户看不到」并写进 release PR 正文。见缺口 G-K1。
- 对应 PR：#1121、#1125、#1202、#1141–#1144、#1155、#1157、#1160、#1172、#1176、#1182、#1190、#1191、#1222、#1227、#1231、#1236。执行方式：阻塞。

**K-03 授予确认卡片的界面显示**（真机）
- 说明：授予 POST 默认关闭、桌面和 CLI 都没有授予入口（#1294 已写明），所以界面只有在 `A24_MODULE_TOOL_GRANTS=1` 且有协议 4 模块时才出现；与 K-02 同样被阻塞，这里只列出人工确认项：卡片逐工具展示「可读、可改、可外发、宿主风险」四项（ADR-K1-03）。
- 判定：人看。对应 PR：#1160、#1172、#1176。

### 4.2 memory-p1（#1341）

共同前置（M-01～M-08、M-12）：用 `NEW` 起一个新数据目录的 daemon，`OMLX_URL` 指向 mock 模型。会话用 `POST /api/v1/sessions '{}'` 建，`session_id` 取返回的 `.id`。「记住」只认以「记住 / 记一下 / 记好 / 记着」开头的用户话（`retain.rs: explicit_remember`）。

**M-01 记住**
- 步骤：
  ```bash
  S1=$(a POST /api/v1/sessions '{}' | jq -r .id)       # 示意；一键脚本里是 newsess
  a POST /api/v1/chat '{"messages":[{"role":"user","content":"记住我对花生过敏"}],"session_id":"'$S1'"}'
  a GET '/api/v1/memory/assertions?limit=50'
  ```
- 期望：chat 回 200，`memory_receipt` 为 `saved`，没有 `memory_error_code`；列表里恰有 1 条 `text` 含「花生」，`status` 为 `active`。
- 判定：`jq -r .memory_receipt r.json` 等于 `saved`；`jq '[.assertions[]|select(.text|contains("花生"))]|length'` 等于 1。
- PR：#1285（回执三态）、M1-T10/T14（已在 main）。执行方式：脚本。

**M-02 召回**
- 步骤：另建会话 S2，`chat` 发「我对什么过敏？」。
- 期望：回执字段不存在（不是显式记住）；发给模型的请求里带着「花生」。
- 判定：`jq -e 'has("memory_receipt")|not' r.json`；`tail -n 1 mock.log | grep -q 花生`。
- PR：M1-T12。执行方式：脚本。

**M-03 撤回**
- 步骤：`a DELETE /api/v1/memory/assertions/$AID` 两次；再 `DELETE .../not-an-id`；然后新会话 S3 再问「我对什么过敏？」。
- 期望：两次 204；未知 id 404；列表里不再有这条；S3 的模型请求里**没有**「花生」（正对照是 M-02，同一机制下有）。
- 判定：状态码；`tail -n 1 mock.log | grep -q 花生` 退出码为 1。
- PR：M1-T10。执行方式：脚本。

**M-04 配额满之后的 chat 回执**（not_saved、failed、partial）
- 前置：先写好一条断言 B（`记住我喜欢茶`），再把默认限额调到个人分区当前用量：
  ```sql
  -- memory.db，个人分区的行数用 mem_os_partitions 精确关联，不按字符串前缀
  UPDATE mem_owner_quota SET max_rows = (SELECT row_count FROM mem_owner_usage u
     WHERE EXISTS (SELECT 1 FROM mem_os_partitions p WHERE p.owner_key = u.owner AND p.space_kind = 'personal'))
   WHERE owner = '*';
  ```
  记下原值，用例结束恢复。
- 步骤与期望（每步前新建一个会话；响应体里都不能出现 `usr:local`、`quota exceeded for owner`、`已记住`）：
  1. 发「你好」：`memory_receipt` = `not_saved`，`memory_error_code` = `memory_quota_rows`；
  2. 发「记住我喜欢咖啡」：`failed` + `memory_quota_rows`；
  3. 把限额设为「当前用量 + 2」再发「记住我喜欢咖啡」：`partial` + `memory_quota_rows`（对话两行存下了，断言没存）；列表里没有「咖啡」。
- 判定：`jq -r '[.memory_receipt,.memory_error_code]|join("/")' r.json`，分别等于 `not_saved/memory_quota_rows`、`failed/memory_quota_rows`、`partial/memory_quota_rows`；`grep -qE 'usr:local|quota exceeded for owner|已记住' r.json` 退出码为 1。
- 注意：第 3 步的「+2」来自设计 c2b 的口径（一轮 2 行、断言审计第 3 行）。口径变了这一步会误判，先查 `mem_owner_usage` 前后差值再下结论。
- PR：#1284（c2a 类型化错误码）、#1285（c2b 回执）。执行方式：脚本。

**M-05 配额满之后撤回仍可用，且豁免单独计数**
- 前置：M-04 的满额状态，断言 B 仍有效。
- 步骤：读 `mem_owner_usage` 个人分区的 `row_count` 与 `exempt_row_count`；`a DELETE /api/v1/memory/assertions/$BID`；再读一次。
- 期望：204；B 不再出现在列表里；`row_count` 不变，`exempt_row_count` 加 1。
- 判定：状态码；`[ "$RC1" = "$RC0" ] && [ "$EX1" = "$((EX0+1))" ]`。
- PR：#1275（c1，memory 迁移 0019）。执行方式：脚本。

**M-06 满额后内容仍被拒（撤回不释放空间）**
- 步骤：M-05 之后立刻再发「记住我喜欢咖啡」。
- 期望：仍是 `failed` + `memory_quota_rows`（撤回豁免只保证撤回可用，不放宽内容额度）。
- 判定：同 M-04 第 2 步。PR：#1275。执行方式：脚本。

**M-07 恢复额度后回到正常**
- 步骤：恢复 `max_rows` 原值，新会话发「记住我喜欢咖啡」。
- 期望：`saved`，无 `memory_error_code`。
- PR：#1285。执行方式：脚本。

**M-08 HostSystem 来源（记忆暂停通知）**
- 步骤：
  ```bash
  a PUT /api/v1/memory/settings '{"enabled":false}'
  a POST /api/v1/runs '{"prompt":"记住 X","session_id":"'$S8'"}'      # 等到 completed
  sqlite3 $AGENT24_DATA_DIR/agent24.db "SELECT count(*) FROM run_message_origins o JOIN run_messages m ON m.run_id=o.run_id AND m.seq=o.seq WHERE o.run_id='$RUN' AND o.kind='HostSystem'"
  a PUT /api/v1/memory/settings '{"enabled":true}'
  ```
- 期望：run 状态 `completed`（暂停时「记住」不再让整次失败）；那条暂停通知的来源是 `HostSystem`，恰 1 行。正对照：未暂停的 run 没有 `HostSystem` 行、有 `UserPrompt` 行。
- 判定：`jq -r .status`=completed；计数 = 1；对照计数 = 0。
- PR：#1251（R05-5 4a，store 迁移 0029）、#1248（HostSystem 来源契约）。执行方式：脚本。

**M-09 磁盘门：内容写入被拒**（macOS，`--disk`）
- 前置：建一个 256 MiB 的内存盘，把数据目录放上去（可用空间约 250 MiB，低于内容写入门槛 `max(512 MiB, 2×WAL)`）：
  ```bash
  DEV=$(hdiutil attach -nomount ram://524288 | awk '{print $1}'); diskutil erasevolume HFS+ a24acc $DEV
  ```
  数据先在普通盘上种好（两条有效断言 A、B），停 daemon，`cp -R` 到 `/Volumes/a24acc/data`，再起 daemon。
- 步骤：发「记住我喜欢咖啡」；再发「你好」。
- 期望：前者 `failed` + `memory_disk_low`，后者 `not_saved` + `memory_disk_low`。
- 判定：同 M-04 的 jq。PR：#1296（c4）。执行方式：脚本（macOS）；Linux 需要 root 建 tmpfs，脚本跳过并标 SKIP。

**M-10 磁盘门：撤回的 16 MiB 下限**（macOS，`--disk`）
- 步骤：内存盘上（剩余约 250 MiB）撤回 A；再 `dd` 填到剩余约 8 MiB，等 6 秒（门的采样缓存 5 秒），撤回 B；删掉填充文件，等 6 秒，再撤回 B。
- 期望：第一次 204；第二次 507，`error.code` 为 `memory_disk_low`，响应体没有分区键；第三次 204。
- 判定：`jq -r .error.code r.json` = `memory_disk_low`；`grep -q 'usr:local' r.json` 退出码 1。
- 备注：真正写满（`memory_disk_full`）没法稳定复现，只有单元测试覆盖（`memory_routes.rs`：`retract_on_a_full_disk_is_507_memory_disk_full`）；见缺口 G-M4。
- PR：#1296、#1284。执行方式：脚本（macOS）。

**M-11 写入拒绝留痕**
- 步骤：M-10 的 507 之后等 3 秒（异步队列落表），查 `mem_write_rejections`。
- 期望：至少 1 行 `code=memory_disk_low`、`rejected_kind=assertion.retracted`、`owner_kind=personal`；表里没有正文。
- 判定：`sqlite3 memory.db "SELECT count(*) FROM mem_write_rejections WHERE code='memory_disk_low' AND rejected_kind='assertion.retracted' AND owner_kind='personal'"` ≥ 1。
- 范围：c3 只接了「模块 remember」和「个人撤回失败」两处；chat 写入失败的会话缺口表（`mem_session_gaps`）接线是 c3b，**不在本次**（#1341 已声明）。PR：#1302（memory 迁移 0020）。执行方式：脚本（依赖 M-10）。

**M-12 WAL checkpoint**（`--slow`，约 2.5 分钟）
- 步骤：写入后（`memory.db-wal` 非空，作正对照），不再有任何记忆写入，每 5 秒看一次，最多等 150 秒（默认空闲 60 秒后触发，轮询间隔 5 秒）。
- 期望：`memory.db-wal` 变为 0 字节或消失；记忆列表里的内容一条不少。
- 判定：先 `[ -s memory.db-wal ]`（对照必须成立，否则记 SKIP「对照不成立」，不算通过）；之后 `[ ! -s memory.db-wal ]`；`GET /api/v1/memory/assertions` 数量不变。
- PR：#1283（c5）。执行方式：脚本。

**M-13 契约与协议文件（静态）**
- 步骤：对 `protocol/openapi.yaml`、`protocol/events.schema.json` grep。
- 期望：`memory_receipt` 枚举含 `not_saved`、`partial`；`memory_error_code` 枚举含 `memory_quota_rows`、`memory_quota_bytes`、`memory_disk_low`、`memory_disk_full`；事件 schema 含 `memory.write_failed`。
- 判定：`grep -q` 全部成功。
- 注意：openapi 里还写着「`memory_disk_low` is reserved (nothing emits it yet)」，而 c4 已经会发它，见缺口 G-M3。PR：#1284、#1285、#1296。执行方式：脚本。

**M-14 桌面端 chat 回执的显示**（真机）
- 步骤：桌面端 Chat 页，用 M-04 / M-07 / 暂停的办法依次造出各种回执（桌面端用的数据目录要带上同样的 mock 模型或真实模型）。
- 期望（人看）：`saved` 显示「已记住，可在「记忆」页查看或撤回」；暂停显示「记忆已暂停，这条没有被记住」；`failed` 显示「记住失败，请稍后重试」；`partial` 显示「对话已保存，但这条记忆没记住」；**`not_saved` 应显示「本轮未保存到记忆」（设计 D2 W1）**。
- 已知问题：`apps/desktop/src/renderer/pages/Chat.tsx` 的 `MemoryReceipt` 类型和 `Chat.i18n.ts` 没有 `not_saved`，普通轮次写入失败时界面什么都不显示，见缺口 G-M1。脚本里 M-14 的静态检查对此记 KNOWN。
- PR：#1285。执行方式：真机。

**M-15 记忆页的撤回显示**（真机）
- 步骤：桌面端「记忆」页，列出、点撤回；配额满（M-04 的办法）时再点一次。
- 期望（人看）：撤回成功后条目消失、不再被召回；满额时撤回照样成功。这一步的业务逻辑已由 M-03、M-05 在 HTTP 层验过，这里只看显示。
- PR：M1-T10、#1275。执行方式：真机。

**M-16 run 路径的失败可见性**（阻塞）
- 设计要求：run 路径写入失败时 `run.completed` 带 `memory_persisted:false`，桌面和 TUI 必须消费并显示（设计 P1-03 验收项）。
- 现状：后端已发（`routes.rs`），桌面和 TUI 都没有消费 `memory_persisted`（grep 无结果），P1-03 未做。
- 处理：写进缺口 G-M5；#1341 正文已把「用量 API 与记忆页 UI（P1-03）」列在范围外，release PR 正文要同样写明。执行方式：阻塞。

**不跑用例的改动（逐项）**
- R05 来源契约的内部部分：消息与来源同一事务（#1168、#1181、#1195）、发送副本按来源 kind 整形（#1250）、来源无法证明时不发送（#1197）、非 assistant 回合不执行工具（#1205）——用户看不到，由测试覆盖；用户可见的那一点（暂停通知的来源）见 M-08。
- H02 重申审计修复（#1152–#1154、#1263）：内部审计，测试覆盖。
- c0 容量实测（#1273）：测量报告，无行为。
- 设计、ADR、PLAN 文档、测试补充（#1245–#1247、#1253、#1261、#1262、#1268 等）。
- store 迁移 0027、0028（来源表及其 kind 扩展）：升级路径由 X 组验证。

### 4.3 documenting

documents 模块**不随安装包分发、不默认安装**（`rust/apps/agent24-documents/README.md`，ADR-DOC-01 D6），用例前置都是「手工装包」。PDF 读/渲染引擎只有 macOS（pdfkit），Linux 上引擎相关操作报 `engine_unavailable`。

共同前置：`cli() { env AGENT24_DATA_DIR=$WORK/d A24_OS_PACKAGES=$WORK/d.pk "$NEW/agent24" "$@"; }`；`cli os install /tmp/documents-pkg`；起 daemon（**用不通的模型地址**：`OMLX_URL=http://127.0.0.1:9 OLLAMA_URL=http://127.0.0.1:9`，抽取的冷却用例要用）；样本文件 `docs/documenting/samples/s01/s01-07-mixed-building-notice/source.pdf`（1 页）。

**D-01 安装与能力探测**
- 步骤：`cli os install /tmp/documents-pkg`；起 daemon；`cli os list`；`a GET /api/v1/documents/capabilities`；`cli documents capabilities --json`。
- 期望：`os list` 里 `documents` 为 `[mounted]`；两种方式的能力查询都成功，并报告存储 ready（macOS 上 parse、render 引擎 ready）。
- 判定：`cli os list | grep -E 'documents.*mounted'`；capabilities 返回 200，`.ok == true`。
- PR：#1293 所在线 DOC-1-01 起各片；ADR-DOC-01。执行方式：脚本。

**D-02 导入**
- 步骤：
  ```bash
  cli documents import "$PDF" --json               # 末行 {"type":"result","ok":true,"status":202,"data":{job...}}
  cli documents job $JOB --json                    # 轮询到 succeeded
  # 桌面端走的路径（共用同一份代码）：把 base/token/path 从 stdin 给进去
  jq -nc --arg base http://127.0.0.1:$PORT --arg token "$TOKEN" --arg path "$PDF" '{base:$base,token:$token,path:$path}' | cli documents import --host-stdin --json
  ```
- 期望：导入返回 202 的 job；轮询到 `succeeded`，job 带 `document_id`、`revision` 为 1；`--host-stdin` 的路径同样成功（再得到一个新文档：同一份字节导入两次是两个文档，ADR-DOC-02 §3）。
- 判定：`jq -c 'select(.type=="result")' | tail -1 | jq -e '.ok and .status==202'`；job 终态 `.data.status=="succeeded"`。
- 说明：共用代码在 `agent24-documents-client`，桌面端的文件选择框、进度条是界面，归 D-12。
- PR：#1313、#1321、#1325、#1345。执行方式：脚本。

**D-03 列出（含分页）**
- 步骤：`cli documents list --json`；`cli documents list --limit 1 --json`；用 `next_cursor` 再取一页。
- 期望：列表含 D-02 的文档，新的在前；`--limit 1` 返回 1 条且 `next_cursor` 非空；下一页不是同一条。
- 判定：`.data.documents|map(.document_id)|index($DOC)` 非空；`(.data.documents|length)==1 and .data.next_cursor!=null`；两页 id 不同。
- PR：#1334（`documents list/get/text/job/cancel/retry`）。执行方式：脚本。

**D-04 查看单个文档**
- 步骤：`cli documents get $DOC --json`
- 期望：`head_revision` 为 1，`head.size` 等于文件大小，`head.content_sha256` 去掉 `sha256:` 前缀后等于原文件的 sha256（存下的就是那份原件）。
- 判定：`shasum -a 256 "$PDF"` 与 `.data.head.content_sha256` 比较。
- PR：#1334；ADR-DOC-02 §3。执行方式：脚本。

**D-05 读文本**
- 步骤：`cli documents text $DOC 1 --json`（返回 503 `engine_unavailable`、`retryable` 为真时，等几秒重试，最多 30 秒）。
- 期望：成功，返回按块分页的文本，有 `parse_status`。
- 判定：`.ok == true` 且 `.data` 非空。
- PR：#1334、DOC-1-06～30 各片。执行方式：脚本。

**D-06 查（find）**
- 入口说明：`find` 在 CLI 和桌面端都**没有**入口（`ops.rs` 的白名单里没有；桌面端也没用），只有 REST 和 agent 工具；所以这里直接调 daemon 的 HTTP。
- 步骤：从样本的 `source.pagetext.txt` 取开头 4 个非空白字符作关键词：
  ```bash
  a POST /api/v1/documents/documents/$DOC/find '{"revision":1,"query":"<关键词>"}'
  a POST /api/v1/documents/documents/$DOC/find '{"revision":1,"query":"zzzz-不存在-qq"}'
  ```
- 期望：前者 200 且 `matches` 非空；后者 200 且 `matches` 为空（反例）。
- 判定：`jq '.matches|length'` 分别 ≥1 和 0。
- PR：#1098（DOC-1-30，片 C）。执行方式：脚本。

**D-07 关键异常流程**
- 步骤与期望：
  1. `cli documents request '{"op":"nope"}' --json` → 退出码 1，`.status==400`，`.error.code=="invalid_request"`；
  2. `cli documents get doc_01ZZZZZZZZZZZZZZZZZZZZZZZZ --json` → 退出码 1，`.error.code=="not_found"`；
  3. 导入一个 `.txt` 文件 → 退出码非 0，`.error.code` 非空（具体码以实际为准：`unsupported_format` 或 `invalid_request`，脚本打印出来，不硬断言码名）；
  4. 没有 daemon（用另一个空数据目录）→ 退出码 1，`.status==503`，`.error.code=="backend_not_ready"`。
- 判定：同上。PR：#1321、#1334、#1345。执行方式：脚本。

**D-08 取消与重试**
- 步骤：对 D-02 的文档发一个抽取（没有本地模型，job 会卡在等待）：
  ```bash
  a POST /api/v1/documents/documents/$DOC/extractions '{"revision":1,"schema":{"fields":[{"key":"k1","description":"任意字段","type":"text"}]}}'
  cli documents cancel $JOB --json            # 轮询到 cancelled
  cli documents retry $JOB --json             # attempt 变 2，回到 queued/running
  cli documents cancel $JOB --json            # 收尾
  ```
- 期望：提交返回 202 job（`job_id` 或 `id`）；取消后终态 `cancelled`；重试后 `attempt` 加 1；同一请求再发不新建 job。
- 判定：状态轮询；`.data.attempt` 前后差 1。PR：#1186（取消与重试）、#1334。执行方式：脚本。

**D-09 冷却：没有本地模型时抽取的等待与失败**（`--slow`，约 4 分钟）
- 前置：D-08 同一个 daemon（模型地址不通）；用另一个 `key`（k2）避免命中 D-08 已取消的 job。
- 步骤：提交抽取；30 秒后看状态；每 10 秒看一次，最多 240 秒。
- 期望：30 秒时 job 还在 `running`（在等路由器冷却的上限，60 秒一轮，累计不超过 3 分钟，ADR-DOC-02 §3.2）；最终 `failed`，`error.code` 为 `engine_unavailable`。
- 判定：`.data.status` 序列先 `running` 后 `failed`；`.data.error.code=="engine_unavailable"`。反例：一开始就 `failed` 说明没有等。
- PR：#970、#969（ME4-S2a 第 6 片）。执行方式：脚本。

**D-10 停用与重新启用**（授权撤销的可操作替代）
- 步骤：`cli os disable documents`；`a GET /api/v1/documents/capabilities`；`cli os enable documents`；重启 daemon；`cli documents list --json`。
- 期望：停用后能力查询不再是 200；重新启用并重启后，文档仍在列表里（数据没丢）。
- 判定：状态码 ≠ 200；`list` 含 `$DOC`。
- 说明：这只验模块停用，**不是**按引用撤销授权，后者见 D-11。PR：#1234、#1240（模块一侧）。执行方式：脚本。

**D-11 授权撤销时在途调用、作业、构建按引用停**（阻塞）
- 要验的：agent 经授予调用 documents 工具起了抽取，撤销授予后，在途调用被停、作业与构建按引用归属被停、失效引用在入口被拒（`/_a24/authorization/revoked`，#1213、#1206、#1234）。
- 阻塞原因（缺口 G-D1、G-D2）：
  1. `ab/documenting` 的 `domain-os.yml` 里没有 `tools:` 声明（设计 DOC-1-06b 的片 E 未做），模块的七个工具「已登记、不通告、调不到」；
  2. 设计明确「K1-6a 与 K1-6b 都完成并验收前，文档工具不授予」；合并后还要让 documents 的 `min_daemon_protocol` 提到 4 才能拿授予（现为 2，协议 1–3 的模块拿不到授予，#1280 的规则）。
- 处理：这一块用户现在够不着，可以按 #1293 的「用户看不到」逐项写进 release PR 正文，并写明谁在什么时候补；不能当作已验。执行方式：阻塞。

**D-12 桌面端文档页的界面**（真机）
- 步骤：桌面端文档页：选文件导入（进度显示）、导入中点「取消」、失败后点「重试」、点开文档看详情和文字预览、关闭详情。
- 期望（人看）：进度、取消、重试、详情、预览都正常；关闭详情后焦点回到页面标题（#1218）。业务逻辑（导入、取消、重试、读）已由 D-02～D-08 经同一份代码验过，这里只看显示和交互。
- PR：#1186、#1192、#1218、#1325、#1343。执行方式：真机。

**D-13 文档工具给 agent 用**（阻塞）
- 对话入口（agent 页里 list / get / read_range / find / extract / job_get / extraction_get）：同 D-11 的两条阻塞原因；设计文档自己也写了「不属于本轮片 1 的验收」。执行方式：阻塞，缺口 G-D1。

**不跑用例的改动（逐项）**
- 模型调用基础设施：按进展计时的流式接收（#956–#962）、`temperature` 与协议版本 1→2（#971–#977）、Ollama 的 `disable_thinking` 与两遍法（#1042–#1047）、挂断归类（#1021）：用户看不到，只有抽取用到，抽取要真模型，由 `packages/documents-accept` 的 S01 harness 与已入库的证据（`docs/documenting/evidence/`）覆盖。
- 文档模块工具的内部实现（#1097、#1098、#1099、#1104、#1109、#1126）：同 D-11，调不到。
- 测试、文档、设计修订（#1019、#1096、#1106、#1214、#1217 等）。
- documents 库迁移 0007、0008（`job_acceptances`、`job_revocations`，#1201、#1234）：升级路径见 X-07。

### 4.4 weknora（KB-C1 / KB-HOST-01）

先说结论：这条线在本次发布里**没有用户能直接用到的功能**。`agent24-knowledge` 的 Provider（搜索、读取、证据状态、签发、身份变化、日志与 seal）是库，`agent24d` 里没有任何地方调用它（只有 `knowledge_store.rs` 引用类型，`knowledge.db` 没有打开）；Knowledge 工具没有接入（`knowledge_host.rs` 里 `KnowledgeRefusal` 标着「Consumed by the Knowledge tools (PR-E)」且 `dead_code`）；CLI 没有 knowledge 子命令。用户可见的只有桌面端拉起 daemon 的方式变了，以及一组桌面私有的 host 接口。

**W-01 host 接口对非桌面调用方关闭**
- 前置：普通 daemon（legacy 单 token 模式，`agent24 daemon start` 或 `agent24d serve`）。
- 步骤：
  ```bash
  a PUT    '/api/v1/host/knowledge/bindings/acc-b1?seq=1'
  a DELETE '/api/v1/host/knowledge/bindings/acc-b1?seq=2'
  a POST /api/v1/runs '{"prompt":"x","knowledge_binding_id":"acc-b1"}'
  ```
- 期望：三者都是 403（legacy 模式没有 ProductHost 权限，`host_capability` 返回 `None`；`error.code` 预期为 `host_authority_required`，脚本打印实际值）；第三条同时证明 `RunCreate.knowledge_binding_id` 字段被识别（不是 400）。
- 判定：三个状态码都等于 403。
- PR：#1196、#1207、#1219。执行方式：脚本。

**W-02 没有绑定的 run 看不到 Knowledge 工具**
- 步骤：`a GET /api/v1/tools`
- 期望：200，工具列表里没有名字含 `knowledge` 的项。
- 判定：`grep -c knowledge r.json` 等于 0。
- 说明：因为 Knowledge 工具还没接入，这条现在是回归检查，接入后要改成「带 binding 的 run 才有」。PR：#1224。执行方式：脚本。

**W-03 桌面端以 capabilities 模式拉起 daemon**（真机）
- 步骤：全新数据目录启动桌面端；看 Chat 页能发消息、Runs 页能列出、托盘「停止」只停桌面自己拉起的 daemon（#602 的行为不变）；退出后 `daemon.json` 清理。
- 期望（人看）：全部可用。CLI 连不上桌面拉起的 daemon 是预期（`agent24 documents` 的 `NO_DAEMON` 提示里写明了）。
- 说明：这是 #1177、#1184 改的桌面与 daemon 之间的凭据与端点分离，属于「托管 daemon」，CLI 侧没有对应入口，按 #1293 这是桌面端补用例的缺口，要在 release PR 正文写明。
- PR：#1177（A1）、#1184（A2）。执行方式：真机。

**W-04 host 通道在真实桌面上的往返**（真机）
- 步骤：桌面端 Knowledge 登录态存在时（safeStorage 加密文件），激活 binding、推送凭据、停用；daemon 日志与响应里不得出现凭据。
- 说明：需要一个真实的 Knowledge 服务账号，且 Provider 没接入，现在没有用户入口去触发这条通道；起草时无法写出可执行步骤，建议在 release PR 正文把它归为「用户看不到」，由 `knowledge-host.test.ts`、`knowledge_host/tests.rs` 覆盖。如果 David 判不能这么归，需要补一个测试用的桌面 host 脚本。见缺口 G-W1。
- PR：#1198、#1215。执行方式：真机（前提不具备时记 SKIP）。

**不跑用例的改动（逐项）**
- `agent24-knowledge` crate 的全部内容（契约 wire 类型 #1110–#1124、HttpTransport #1161、证据包裹 #1232、续读与上限 #1264/#1269、Provider 状态机 KB-C1-04 第 1–6 片 #1221–#1340、持久化账本与重放 #1260–#1305）：未接入 daemon，用户碰不到。
- `knowledge.db` 的日志与 seal 编码（#1327、#1336，`knowledge_migrations/0001_log.sql`）：daemon 不打开它；等接入时再加升级用例。
- schema、夹具、契约测试（#1054–#1065、`packages/contract-tests/src/knowledge-v1.test.ts`）、契约与设计文档（#1044、#1113–#1115、#1171–#1174、#1255）。

### 4.5 idoris（第五线，#1370）

先说结论：这条线是 Agent24 经 iDoris 网关（`idoris-local`）调本地模型。配置 `IDORIS_URL`（loopback）和 `IDORIS_API_KEY`（iDoris 签发的只含 `local_only` 的数据 key，与 Admin token、`OMLX_API_KEY`、daemon token 分开，不进日志、不进 renderer）之后，Agent24 **独占** `idoris-local`：不保留 oMLX/Ollama 直连回退，只发约定的 role 与 `X-iDoris-Privacy` / `X-iDoris-Complexity` / `X-iDoris-Request-Id` 三个头，不流式，云端补位默认关。接口唯一的权威是 iDoris 仓库《iDoris × Agent24 分工边界与接口规范》修订 1.2 的 §3。

**本期明确不做（release notes 不得宣称）**：共同 launcher 与 Admin token FD 交接（R9 OD-1）、推荐 → 同意 → 自动安装模型（OD-2，Download 按钮不能冒充「已安装」）、fast/daily 角色重标（OD-3）、`idoris-any` 远程 provider 与第二把 key、流式、动态额度 HTTP API、Admin 写接口。

**已知限制（Release Notes 必写）**：iDoris 网关当前**忽略 `temperature`**。iDoris 生命周期路径的请求字段是白名单（`model, messages, stream, max_tokens, tools, response_format, tool_choice, parallel_tool_calls, chat_template_kwargs, disable_thinking`），带 `temperature` 会被 400 `unsupported_parameter` 拒绝，所以 #1365 改为：经 iDoris 网关的请求不发 `temperature`，其它 provider 照 main 透传。iDoris 以后支持采样参数时再另行商定。

共同前置（ID-1、ID-2、ID-3、ID-5）：真实 iDoris v0.3.1 归档已按第 2 节校验 sha256 并解出到 `$IDORIS_HOME`（= 解包目录，内有 `idoris` 与 `config/`）；后端是本地确定性 HTTP 假后端（OpenAI 兼容，`/v1/models` 返回 `fixture-small-chat`、`fixture-chat`，每个 `/v1/chat/completions` 请求体记一行到日志，永远回固定内容；不是 oMLX、不是 Ollama）。数据面 `127.0.0.1:8740`，只读 Admin `127.0.0.1:8741/admin/`。用户自己启动 iDoris（共同 launcher 本期不做）。下面的样例来自 iDoris 侧在 #1370 的回帖（issuecomment-6436424430）。

```bash
# 假后端（loopback:18000），日志 $WORK/fb.log；脚本里有等价实现
# ---- 数据面 key（只含 local_only）----
umask 077
export IDORIS_DEPLOY_MODE=personal
export IDORIS_DB_PATH="$WORK/idoris-state.sqlite3"        # 签 key 和 serve 必须用同一个库
cd "$IDORIS_HOME"
./idoris key issue --spec-stdin > $WORK/key-min.json <<'JSON'
{"owner":"agent24","allowed_privacy":["local_only"],"allowed_roles":["fast","daily","deep","decide"]}
JSON
# 输出里有 key_id 和一次性明文 secret；库里只存哈希。字段只能是 owner, tenant_id?, allowed_privacy, allowed_roles, expires_at_ms?, admin_scopes（必须为空）；未知字段会被拒
KEY=$(jq -r .secret $WORK/key-min.json)
# 具体模型 override 需要拥有全部角色（含 auto）的 key：
./idoris key issue --spec-stdin > $WORK/key-full.json <<'JSON'
{"owner":"agent24-full","allowed_privacy":["local_only"],"allowed_roles":["fast","daily","deep","vision","embed","rerank","decide","auto"]}
JSON
KEYFULL=$(jq -r .secret $WORK/key-full.json)

# ---- role 卡：复制 config/components/omlx.yaml，只改 endpoint 并加 extensions ----
#   endpoint: "http://127.0.0.1:18000"
#   extensions:
#     _degradation: "Removing these bindings disables role aliases on this backend."
#     idoris_role_models:
#       fast: fixture-small-chat
#       daily: fixture-chat
#       deep: fixture-chat
#       decide: fixture-chat
# 值必须是假后端 /v1/models 返回的真实 ID，不能有首尾空白，不能是另一个 idoris/ 别名；_degradation 必填，写错启动直接失败
./idoris serve &      # 另一个终端也行
curl -s http://127.0.0.1:8740/health | jq .
ic() { curl -s -m 20 -o $WORK/ir.json -w '%{http_code}' -H "Authorization: Bearer $1" -H 'content-type: application/json' -H 'X-iDoris-Privacy: local_only' \
  -d "{\"model\":\"$2\",\"messages\":[{\"role\":\"user\",\"content\":\"ping\"}]}" http://127.0.0.1:8740/v1/chat/completions; }
```

**ID-1 bootstrap**
- 前置：上面的 key、role 卡、假后端和 iDoris 都已就位，`IDORIS_ALLOCATED_BUDGET_BYTES` 不设。
- 步骤：
  ```bash
  curl -s http://127.0.0.1:8740/health | jq -r '[.service,.contract_version,.version]|@tsv'     # idoris  1.0.1  0.3.1
  ic "$KEY" idoris/fast;  ic "" idoris/fast;  ic wrong-key idoris/fast;  ic "$KEY" idoris/vision
  # Agent24 侧的好路径：
  start_daemon IDORIS_URL=http://127.0.0.1:8740 IDORIS_API_KEY="$KEY"        # 手工：第 2 节小助手
  a POST /api/v1/chat '{"messages":[{"role":"user","content":"ping"}]}'
  # Agent24 侧的负对照：不设 IDORIS_API_KEY / IDORIS_API_KEY=wrong 各起一次 daemon，再 chat
  ```
- 期望：`/health` 身份为 `idoris` / `1.0.1`；`idoris/fast` 带好 key 200；无 key、错 key 为 401/403；**用最小 key 请求 `idoris/vision` 为 403**（越权 role）；三种失败都不回落到 oMLX/Ollama（oMLX mock 行数不增）；Agent24 好路径 200，负对照提示指向 key 问题。`service`、`contract_version` 不符时，Agent24 启动即明确报错，不猜测兼容。
- 判定：状态码依次为 200、401|403、401|403、403；Agent24 好路径 `$CODE = 200`。
- PR/issue：#1370（ID-1）、#982、#1364。执行方式：脚本（`--idoris`；Agent24 侧提示文案「人看」）。

**ID-2 local_only 零外呼**
- 前置：同 ID-1。
- 步骤（负例构造方法见 iDoris 回帖）：
  ```bash
  # 1) 删掉 role 卡里 daily 那一行，重启 iDoris；没有绑定的 role 不进选路
  N0=$(wc -l < $WORK/fb.log); ic "$KEY" idoris/daily; N1=$(wc -l < $WORK/fb.log)
  # 2) 停掉假后端（本地没有模型）再请求 idoris/fast
  # 3) IDORIS_REMOTE_KILL_SWITCH=1 重启 iDoris：3a 已绑定的 daily 仍 200/loopback（熔断只切远程，正对照）；3b 熔断 + 停掉假后端，再请求 → 503
  ```
- 期望：第 1、2、3b 步 fail-closed，503，`reason_code` 一律是 `local_only_unavailable`（iDoris v0.3.1 不区分原因，见 #1370 issuecomment-6443049445），响应**不带** `X-iDoris-Served-Locality` 头；不退到别的 role，也不自动选第一个模型；**假后端（直连后端）请求计数不变**（`N1 = N0`）。第 3a 步：`IDORIS_REMOTE_KILL_SWITCH=1` 只切远程，本地已绑定的 daily 照常 200、locality=`loopback`（设计行为，不是缺陷）。Agent24 侧提示「本地没有可用模型」。402 是终态（不重试、不换 provider），由 iDoris 侧用例覆盖（v0.3.1 tag 上 18 个用例，断言不碰上游、不扣账；纯本地链路无法复现，见 #1370），Agent24 侧只确认 402 不触发重试。
- 判定：`ic` 返回 503；`[ "$N1" = "$N0" ]`。
- PR/issue：#1370（ID-2）。执行方式：脚本（`--idoris`，只覆盖第 1 步；2、3 步手工）。

**ID-3 角色到真实模型**
- 前置：同 ID-1（`fast` 绑 `fixture-small-chat`，`daily`/`deep`/`decide` 绑 `fixture-chat`）。
- 步骤：
  ```bash
  ic "$KEY" idoris/fast;   jq -r .model $WORK/ir.json; tail -n1 $WORK/fb.log | jq -r .model     # fixture-small-chat
  ic "$KEY" idoris/deep;   jq -r .model $WORK/ir.json; tail -n1 $WORK/fb.log | jq -r .model     # fixture-chat
  ic "$KEYFULL" fixture-small-chat;  tail -n1 $WORK/fb.log | jq -r .model    # 具体模型 override，需全角色 key
  ic "$KEY" fixture-small-chat                                               # 最小 key 请求具体 ID：应被拒（负例）
  # Settings 改选另一个模型或 role 后再发 chat（桌面/CLI 手工）：上游 model 随之变化
  ```
- 期望：假后端收到的上游请求 `model` 等于该 role 的绑定；`response.model` 是真实模型 ID；响应带 `X-iDoris-Record-Id` 与 `X-iDoris-Served-Locality: loopback`；具体 ID 覆盖仅在全角色 key 下成功，最小 key 被拒；Settings 的改动**真的进入 chat 请求**（不只是改高亮），界面显示真实 `response.model`、Record-Id 和 locality（界面部分「人看」，并入 ID-8）。
- 判定：每步 `ir.json` 的 `.model` 与 `fb.log` 最后一行的 `.model` 一致且等于上表；最小 key 请求具体 ID 不是 200。
- PR/issue：#1370（ID-3）。执行方式：脚本（`--idoris`）；Settings 与界面部分手工。

**ID-4 locality 负对照**
- 前置：一个 iDoris 的**桩网关**（脚本内置，不是真 iDoris）：`/health` 回 `{"service":"idoris","contract_version":"1.0.1"}`，`/v1/chat/completions` 回固定内容 `idoris-pong`，`X-iDoris-Served-Locality` 头按模式文件取值；daemon 用 `IDORIS_URL=<桩>`、`IDORIS_API_KEY=x` 起。
- 步骤：模式依次为 `loopback`（正对照）、缺头、`lan`、`remote`，每种发一次 chat。
- 期望：`loopback` 返回 200 且带 `idoris-pong`（正对照，证明桩与链路本身是通的）；缺头、`lan`、`remote` 一律按错误处理，**结果不交给调用者**（响应里没有 `idoris-pong`，状态非 200）。
- 判定：见脚本 ID-4；`idoris-local` 只接受 `loopback`。
- PR/issue：#1370（ID-4）；`idoris.rs` 的 locality 绊线。执行方式：脚本。

**ID-5 额度**
- 前置：同 ID-1；`IDORIS_ALLOCATED_BUDGET_BYTES` 在 `idoris serve` 的环境里设置，整数字节，内部按 1e9 换算成十进制 GB；Agent24 侧用 `agent24 hw idoris-env --core-reserved-bytes <N>` 算出要交给 iDoris 的值（手动启动的写法见 `docs/agent/IDORIS-MANUAL-BUDGET.md`）。
- 步骤：
  ```bash
  agent24 hw idoris-env --core-reserved-bytes 1073741824     # 打印 IDORIS_ALLOCATED_BUDGET_BYTES=<字节>；非整数/负数/小数 Agent24 侧应拒
  # a) 不设（独立模式）   b) IDORIS_ALLOCATED_BUDGET_BYTES=0（真零）   c) =2000000000（小额度）
  # d) 不合法：=-1、=1.5、=99999999999999999999999 → iDoris 必须拒绝启动
  ```
- 期望：a) 推荐不受额度限制；b) 推荐里没有任何需要内存的候选，任何加载都被拒；c) 推荐只含放得下的模型，强制指定超额的模型被拒绝生效；d) 三种不合法值进程都退出（非 0），不监听 8740。推荐结果和**实际加载准入**都与设定一致。
- 判定：d) 脚本化（起进程、2 秒后 `kill -0` 失败）；a/b/c 逐种比对推荐列表与加载结果（手工，推荐/准入的观察入口见 Admin 状态卡，iDoris#579 的 `TempChoice` / `BlockedChoice.degraded_alternative` 本期不经 HTTP 暴露，不在此验）。
- PR/issue：#982、#1370（ID-5）。执行方式：脚本（仅 d）+ 手工（a/b/c）。

**ID-6 零回归**
- 前置：新数据目录，**不**设 `IDORIS_URL`；oMLX 指向 mock。
- 步骤：`chat` 发一句话；看 mock 的请求日志。
- 期望：200，内容 `pong`；请求打到 oMLX 的 mock（`mock.log` 增 1 行）；没有任何对 iDoris 的访问；原有 `agent24 daemon start/stop`、Settings 页行为不变。
- 判定：`$CODE = 200`；`grep -c pong r.json` ≥ 1；`mock.log` 行数增加。
- PR/issue：#1370（ID-6）。执行方式：脚本。

**ID-8 真机**
- 机器：jason 笔记本（M1 Max）。iDoris 侧已用 v0.3.1 候选二进制（main 21b3f74 + #581 release 构建）在此机预演通过（`idoris/daily` → `Qwen3-0.6B-4bit`，200，locality=loopback，越权 `idoris/vision` 403）。本期验收仍要用正式 v0.3.1 归档再跑一遍：tag `v0.3.1` = `8e3a216`，资产 `idoris-v0.3.1-aarch64-apple-darwin.tar.gz`，sha256 `1125301fc32f9e44d4d06e5127137ba14196a32f9274b7416a345e690b2ffea1`（解包前 `shasum -a 256` 比对）。iDoris main 已到 `05fa85c`（含 #583），**不要用**，本期锁 v0.3.1。
- 前置（本机与样例不同的三处，照此执行）：
  1. **隔离 HOME**：不复用本机现有 iDoris/Agent24 状态。每个要跑的终端先 `export HOME=$(mktemp -d)`（或用全新 macOS 用户），再起 iDoris 和 Agent24；数据库、`~/.agent24` 都落在这个临时 HOME 里。
  2. **oMLX 端口是 8088（不是卡片默认的 8000），且开了认证**。启动 iDoris 前在**当前 shell**设 `IDORIS_OMLX_API_KEY`，值取本机 oMLX 的 API key：oMLX.app 的 Settings 里的 API key，或配置文件 `~/.omlx/settings.json` 的 `auth.api_key`。**注意先于换 HOME 之前读出**（或直接用真实 HOME 的绝对路径读）：
     `export IDORIS_OMLX_API_KEY="$(python3 -c 'import json,os;print(json.load(open("/Users/jason/.omlx/settings.json"))["auth"]["api_key"])')"`
     只放当前 shell 环境，**不写进文档、脚本、日志、shell 历史里的明文**，不落盘。
  3. **角色卡保持默认 `mode: on_demand`，不要改成 `resident`**（resident 对开了认证的 oMLX 不带 key，结果 401，见 iDoris#582，计划 v0.3.2 修）。卡片在打包的 `config/components/omlx.yaml` 基础上只改两处：`endpoint: "http://127.0.0.1:8088"`；追加 `extensions.idoris_role_models.daily: Qwen3-0.6B-4bit`（连同必填的 `_degradation`）。卡片放进 `IDORIS_COMPONENTS_DIR` 指向的目录。
  - 其余：oMLX 0.6.4 已在运行；已缓存小模型 `Qwen3-0.6B-4bit`（用户按安装指南准备，不是 Agent24 下载）；`IDORIS_ALLOCATED_BUDGET_BYTES=8000000000`（或用 `agent24 hw idoris-env` 算出的额度）；数据面默认 8740、Admin 8741，需空闲（有冲突可用 `IDORIS_PORT=18740 IDORIS_ADMIN_PORT=18741`，Agent24 的 `IDORIS_URL` 随之改）；断网（飞行模式）。
- 步骤：按上面前置起 iDoris；签发 key；桌面端或 CLI 配 `IDORIS_URL` 与该 key；Settings 里选 `idoris/daily`，发一次 chat；看界面。
- 期望（人看）：离线下得到一次真实的成功回复（**fixture 不能代替这一项**）；界面显示真实 `response.model`（`Qwen3-0.6B-4bit`）、Record-Id 和 locality=`loopback`；401/403 提示指向 key，503 提示「本地没有可用模型」；日志里不出现上游 oMLX key。
- PR/issue：#1370（ID-8）。执行方式：真机。

**不跑用例的改动（逐项）**
- **ID-7 temperature（原联合验收 ID-7，已移出用户路径）**：设了 `temperature` 也不经 iDoris 网关发出，调用仍成功。证明方式：#1365 的单元测试（网关路径请求体无 `temperature`、其它 provider 仍带），或手工抓上游请求体（桩网关日志里没有 `"temperature"` 键）。没有用户可达入口（`temperature` 只在 daemon 的模块推理回调里可设），按 #1293 不列用户路径用例；改在 Release Notes「已知限制」写明 iDoris 网关当前忽略 temperature。
- key 和 `IDORIS_URL` 不进日志、不进 renderer：由 `model_callback.rs`、`service.rs` 的单测覆盖（`capture_env()` 不含 `IDORIS_API_KEY`），用户看不到。
- 推荐器库层的两个用户选项（`TempChoice`、`BlockedChoice.degraded_alternative`、`headroom_breach_option`）：iDoris v0.3.1 不经 HTTP 暴露，Agent24 的展示和选择下一期做。
- 共同 launcher / Admin token FD 交接、`idoris-any`、流式：本期不做，不在 release notes 里宣称。

## 5. 跨线升级

目标：用 `v0.5.1-preview.20261016.2` 的数据目录升到新版，两个库按顺序迁移成功，数据不丢。两个库指 `agent24.db`（store）和 `memory.db`（memory）；documents 模块有自己的库，见 X-07。预期迁移范围（以合并后的代码为准，不符先查合并）：

| 库 | preview 的最高号 | 新版应有 | 新增 |
|---|---|---|---|
| store（`agent24.db`） | 0017 | 0001–0030，连续 | 0018（main）、0019–0026（kernel）、0027–0029（memory-p1）、0030（P-13 #1381） |
| memory（`memory.db`） | 0017 | 0001–0020，连续 | 0018（kernel `assertion_source_ref`）、0019–0020（memory-p1） |

**X-01 升级后两个库按顺序迁移成功**
- 前置：用 `$PREVIEW/agent24d serve` 在新目录 `$WORK/x` 种数据：建会话 SID、`记住我对花生过敏`、再发一个普通 chat、建一个 run；记下断言 id 与文本、run id；记下两个库的迁移号（应为 17 / 17）；停 daemon，备份整个目录。
- 步骤：`$NEW/agent24d serve` 打开同一个目录。
- 期望：正常 ready，`/api/v1/health` 200；两个库的 `_sqlx_migrations` 都是连续序列、全部 `success=1`，分别到 29 和 20。
- 判定：
  ```bash
  sqlite3 agent24.db "SELECT group_concat(version) FROM (SELECT version FROM _sqlx_migrations WHERE success=1 ORDER BY version)"   # = 1,2,...,30
  sqlite3 memory.db  "SELECT group_concat(version) FROM (SELECT version FROM _sqlx_migrations WHERE success=1 ORDER BY version)"   # = 1,2,...,20
  ```
  与 `seq 1 30 | paste -sd, -`、`seq 1 20 | paste -sd, -` 比较（macOS 的 `seq -s,` 末尾会多一个逗号）。
- PR：#1280、#1341、#1298（改号）。执行方式：脚本。

**X-02 升级后数据不丢，新功能可用**
- 步骤：X-01 之后：`GET /api/v1/memory/assertions`；`GET /api/v1/sessions/$SID`；`GET /api/v1/runs/$RUN`；新会话发「记住我喜欢茶」；`GET /api/v1/memory/settings`。
- 期望：旧断言还在且 id、文本不变；旧会话、旧 run 都在（run 仍是 `completed`）；新的「记住」回 `saved`；设置读取 200。
- 判定：id 与文本逐字相等；状态码；`memory_receipt=="saved"`。
- 执行方式：脚本。

**X-03 回不去：旧版打开升级后的库被拒，且库不变**
- 步骤：对 X-01 升级后的目录（先复制一份），用 `$PREVIEW/agent24d serve` 打开；前后各算一次 `sqlite3 agent24.db .dump | shasum`。
- 期望：启动失败，日志含 `previously applied but is missing`，库的 dump 哈希不变。
- 判定：daemon 起不来（退出码 ≠ 0）；日志 grep；哈希相等。
- 说明：用户不能降级，release notes 要写明；与 #1294 的 D1 同一性质。执行方式：脚本。

**X-04 开发机跑过旧编号的迁移：store 拒绝启动，memory 降级关闭**
- 背景：memory-p1 在 #1298 改了编号（memory 0018→0019、store 0021–0023→0027–0029），kernel 占用了 memory 0018 和 store 0019–0026。跑过 `ab/memory-p1` 旧开发版的机器，库里 memory 的 18 号和 store 的 21–23 号是**另一份迁移**。
- 模拟：对 X-01 升级后的目录复制两份，分别执行
  ```sql
  -- memory.db：假装 18 号是旧编号的 retract_quota_exempt
  UPDATE _sqlx_migrations SET description='retract_quota_exempt', checksum=zeroblob(48) WHERE version=18;
  -- agent24.db：假装 22 号是旧编号的某条迁移
  UPDATE _sqlx_migrations SET description='run_message_origin_kinds', checksum=zeroblob(48) WHERE version=22;
  ```
- 步骤：`$NEW/agent24d serve` 分别打开两份。
- 期望：**agent24.db（store）**：启动失败（退出码 ≠ 0），日志里点名迁移号（形如 `migration 22 was previously applied but has been modified`，措辞以 sqlx 为准）。**memory.db**：daemon 照常启动，但关闭记忆并打一条 WARN 点名迁移号（`server.rs` 的有意降级，10-27 实跑确认）。两份库的 dump 哈希都不变，没有半截迁移。
- 已知问题（10-27 实跑）：memory 这一路用户只能看到记忆不可用，界面上没有明确报错。已写进 Release Notes 的「升级注意」；后续在 `/health` 暴露 degraded（REV-02）。
- 判定：日志 `grep -E 'migration (18|22)'`；哈希相等。
- 疑问：代码里没有为这种情况写专门的报错或重置指引（缺口 G-X1）。是否算「清楚的重置步骤」取决于 release notes 写不写下面 X-05 的步骤；这条用例的第二半（步骤是否清楚）靠 X-05 的执行结果判定。执行方式：脚本。

**X-05 重置步骤可用**
- 重置步骤（要原样写进 release notes 和 CHANGELOG 的「迁移说明」）：
  1. `agent24 daemon stop`（桌面端用户先退出应用）；
  2. 开发机：把整个数据目录挪开备份，不要删：`mv ~/.agent24 ~/.agent24.dev-backup-$(date +%Y%m%d)`；只想保留一个库时，成对挪开 `agent24.db`、`agent24.db-wal`、`agent24.db-shm`，或 `memory.db`、`memory.db-wal`、`memory.db-shm`，**不要只挪主文件**；
  3. 重新启动，daemon 会用新编号重建；
  4. 想留着旧数据继续用旧版：用 `AGENT24_DATA_DIR=<另一个目录>` 让新旧版互不干扰（`docs/Deployment/README.md`）。
- 步骤：对 X-04 失败的目录执行 1–3。
- 期望：重新启动成功，两个库迁移到 30 / 20，健康检查 200，备份目录里的原库字节未变。
- 判定：`/api/v1/health` 200；迁移序列；备份目录里 `agent24.db` 的哈希等于挪开前。
- 注意：「David 决定的不做兼容、清库」只写在 K1-06b 矩阵 §6 第 4 条（针对 ab/kernel 中间版本）；memory-p1 旧编号这一类没有同等文字，需要 David 确认同样口径。执行方式：脚本。

**X-06 签名安装包覆盖安装**（真机）
- 前置：一台装着 `Agent24-0.5.1-preview.20261016.2-arm64.dmg`（签名、公证）且有真实数据（记忆、会话）的 Mac。
- 步骤：先备份 `~/.agent24`；下载新版 dmg，`spctl -a -vv /Applications/Agent24.app`、`xcrun stapler validate /Applications/Agent24.app`；覆盖安装并启动。
- 期望（人看加命令）：签名与公证校验通过；应用能启动；「记忆」页旧记忆都在；Chat 页能发消息；文档页可见（若文档模块未随包，则不显示或提示未安装，见 G-D3）。
- 执行方式：真机（签名）。

**X-07 documents 库升级**（CLI，未入一键脚本）
- 背景：documents 库由 0001–0006 变为 0001–0008。保持数据的唯一来源是 `documenting-r3-rc1` 等 RC 包（正式发布包不带 documents）。
- 步骤：用 `documenting-r3-rc1` 的包装 documents、导入一个 PDF、停 daemon；换成新版二进制和新的 documents 包再启动。
  ```bash
  gh release download documenting-r3-rc1 -R iDoris-ai/Agent24 -D $WORK/rc1   # 取包，具体资产名以实际为准
  ```
- 期望：daemon 就绪；`cli documents list` 里旧文档还在；`$DATA/os/documents/` 下的库 `_sqlx_migrations` 到 8。
- 判定：`list` 含旧文档 id；`sqlite3 <库> "SELECT max(version) FROM _sqlx_migrations"` = 8。
- 未入脚本的原因：需要 RC 包的具体资产名，起草时没有核实。执行方式：CLI。

**X-08 桌面端覆盖安装后的文档与记忆联合检查**（真机）
- 步骤：X-06 之后，桌面端导入一个 PDF（文档模块已装的前提），在 Chat 页「记住…」，再到「记忆」页撤回。
- 期望（人看）：全流程可用，无报错弹窗。
- 执行方式：真机。

## 6. 一键执行

把下面这个代码块存成文件后运行。它跑上面所有标「脚本」的用例，产出 `$WORK/report.md`，有 FAIL 就以非 0 退出，SKIP 单独计数并列出。

```bash
# 取出脚本（放在仓库外，由它自己 cd 到仓库根）
awk '/^```bash accept-v0.6.0.sh$/{f=1;next} /^```$/{f=0} f' docs/release/ACCEPTANCE-v0.6.0-preview.md > /tmp/accept-v0.6.0.sh && chmod +x /tmp/accept-v0.6.0.sh

# 一次完整执行（macOS；慢用例和内存盘用例各自开关）
git show d50c319c9454ffa79f8ccd5de2b4b8955fab4a87:scripts/k1-r2-accept.sh > scripts/k1-r2-accept.sh && chmod +x scripts/k1-r2-accept.sh
# U3 的迁移期望在借来的脚本里已过时（kernel 之后又进了 memory-p1 与 P-13）：改成本版的 store 19..30 / memory 20，并确认替换生效
sed -i '' 's|"19,20,21,22,23,24,25,26/18"|"19,20,21,22,23,24,25,26,27,28,29,30/20"|' scripts/k1-r2-accept.sh
grep -q '"19,20,21,22,23,24,25,26,27,28,29,30/20"' scripts/k1-r2-accept.sh || { echo "U3 patch failed" >&2; exit 1; }
# --main 用 kernel 合并前的 main 二进制：git worktree add /tmp/main-pre-k1 982725e2 后构建（982725e2 = 03bf1e17^1）
/tmp/accept-v0.6.0.sh --new "$NEW" --preview "$PREVIEW" --main "$MAIN" \
    --k1-script scripts/k1-r2-accept.sh --documents-pkg /tmp/documents-pkg \
    --work "$HOME/v060-accept" --slow --disk
rm scripts/k1-r2-accept.sh        # 脚本是借来的，不提交
```

- 只跑某几组：`--only K,M`（组名 K、M、D、W、I、X；I 是 iDoris 线，脚本跑 ID-4、ID-6，ID-1/2/3/5 另需 `--idoris DIR`（解包后的 iDoris v0.3.1 目录，内含 `idoris` 与 `config/`），缺则记 SKIP）。缺参数的组里对应用例记 SKIP。
- 依赖：curl、jq、sqlite3、python3、shasum；`--disk` 需要 macOS 的 `hdiutil`、`diskutil`。

```bash accept-v0.6.0.sh
#!/usr/bin/env bash
# v0.6.0-preview.20261028.1 acceptance: docs/release/ACCEPTANCE-v0.6.0-preview.md
#   accept-v0.6.0.sh --new BIN [--preview BIN] [--main BIN] [--k1-script PATH]
#                    [--documents-pkg DIR] [--pdf FILE] [--work DIR] [--idoris DIR]
#                    [--only K,M,D,W,I,X] [--slow] [--disk]
# BIN = a directory holding agent24d and agent24.
set -uo pipefail
export LC_ALL=C
cd "$(git rev-parse --show-toplevel)" || exit 2
REPO=$(pwd)

IDORIS_HOME="" NEW="" PREVIEW="" MAIN="" K1="" DOCPKG="" PDF="" WORK="" ONLY="K M D W I X" SLOW=0 DISK=0
while [ $# -gt 0 ]; do
  case "$1" in
    --new) NEW=$2; shift 2 ;;
    --preview) PREVIEW=$2; shift 2 ;;
    --main) MAIN=$2; shift 2 ;;
    --k1-script) K1=$2; shift 2 ;;
    --documents-pkg) DOCPKG=$2; shift 2 ;;
    --pdf) PDF=$2; shift 2 ;;
    --work) WORK=$2; shift 2 ;;
    --idoris) IDORIS_HOME=$2; shift 2 ;;
    --only) ONLY=${2//,/ }; shift 2 ;;
    --slow) SLOW=1; shift ;;
    --disk) DISK=1; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$NEW" ] || { sed -n 2,6p "$0" >&2; exit 2; }
for t in curl jq sqlite3 python3 shasum; do
  command -v "$t" > /dev/null || { echo "missing tool: $t" >&2; exit 2; }
done
for b in "$NEW/agent24d" "$NEW/agent24"; do [ -x "$b" ] || { echo "missing binary: $b" >&2; exit 2; }; done
PDF=${PDF:-$REPO/docs/documenting/samples/s01/s01-07-mixed-building-notice/source.pdf}

resolve() { python3 -I -c 'import os, sys; print(os.path.realpath(sys.argv[1]))' "$1"; }
REAL=$(resolve "${HOME:-/nonexistent}/.agent24")
WORK=$(resolve "${WORK:-${TMPDIR:-/tmp}/v060-accept.$$}")
case "$WORK/" in "$REAL"/*) echo "--work must not be inside $REAL" >&2; exit 2 ;; esac
[ -z "$(ls -A "$WORK" 2> /dev/null)" ] || { echo "--work must be new or empty: $WORK" >&2; exit 2; }
mkdir -p "$WORK" || exit 2
[ -s "$REAL/daemon.json" ] && kill -0 "$(jq -r .pid "$REAL/daemon.json")" 2> /dev/null \
  && { echo "a daemon is running on $REAL: stop it first (agent24 daemon stop)" >&2; exit 2; }
for v in $(env | grep -oE '^(A24|AGENT24|OMLX|OLLAMA|IDORIS)_[A-Z0-9_]+|^DEFAULT_MODEL'); do unset "$v"; done
touch "$WORK/.start"
[ -e "$REAL" ] && REAL_BEFORE=present || REAL_BEFORE=absent

FAILS=0 SKIPS=0
: > "$WORK/results"
# check ID "what" ok|fail|skip "evidence" ["known limitation"]
check() {
  local mark
  case $3 in
    ok) mark=PASS ;;
    skip) mark=SKIP; SKIPS=$((SKIPS + 1)) ;;
    *) if [ -n "${5:-}" ]; then mark=KNOWN; else mark=FAIL; FAILS=$((FAILS + 1)); fi ;;
  esac
  local ev; ev=$(printf '%s' "$4" | tr '|\n' '/ ' | cut -c1-240)
  printf '| %s | %s | %s | %s%s |\n' "$1" "$2" "$mark" "$ev" "${5:+ -- known: $5}" >> "$WORK/results"
  echo "$mark $1 -- $2 -- $ev"
}
is() { [ "$1" = "$2" ] && echo ok || echo fail; }
want() { case " $ONLY " in *" $1 "*) return 0 ;; esac; return 1; }

# ---- mock model: always answers "pong", logs every request body ----
cat > "$WORK/mock.py" <<'PY'
import json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
LOG = sys.argv[2]
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def _send(self, obj):
        b = json.dumps(obj).encode()
        self.send_response(200); self.send_header('content-type', 'application/json')
        self.send_header('content-length', str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        self._send({"object": "list", "data": [{"id": "mock", "object": "model"}]})
    def do_POST(self):
        n = int(self.headers.get('content-length') or 0)
        body = self.rfile.read(n).decode()
        with open(LOG, 'a') as f: f.write(body.replace("\n", " ") + "\n")
        self._send({"model": "mock", "choices": [{"index": 0, "message": {"role": "assistant", "content": "pong"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}})
ThreadingHTTPServer(('127.0.0.1', int(sys.argv[1])), H).serve_forever()
PY
MPORT=$(python3 -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
python3 -I "$WORK/mock.py" "$MPORT" "$WORK/mock.log" &
MOCK=$!
: > "$WORK/mock.log"
for _ in $(seq 1 20); do curl -s -m 1 "http://127.0.0.1:$MPORT/v1/models" > /dev/null && break; sleep 0.3; done
MODEL_ENV=(OMLX_URL="http://127.0.0.1:$MPORT" DEFAULT_MODEL=mock OMLX_API_KEY=x)
NOMODEL_ENV=(OMLX_URL=http://127.0.0.1:9 OLLAMA_URL=http://127.0.0.1:9)

PID="" PORT="" TOKEN="" DATA="" BIN="" CODE=""
RAMDEV=""
# start_daemon BIN_DIR DATA_DIR [VAR=value ...]: 0 once ready, 1 if it exited. Later VAR=value win.
start_daemon() {
  BIN=$1; DATA=$2; shift 2
  mkdir -p "$DATA"; rm -f "$DATA/daemon.json"
  env AGENT24_DATA_DIR="$DATA" A24_OS_PACKAGES="$DATA.pk" "${MODEL_ENV[@]}" "$@" \
    "$BIN/agent24d" serve > "$DATA.serve.log" 2>&1 &
  PID=$!
  for _ in $(seq 1 80); do
    if [ -s "$DATA/daemon.json" ]; then
      PORT=$(jq -r .port "$DATA/daemon.json"); TOKEN=$(jq -r .token "$DATA/daemon.json"); return 0
    fi
    kill -0 "$PID" 2> /dev/null || { wait "$PID"; PID=""; return 1; }
    sleep 0.5
  done
  stop_daemon; return 1
}
stop_daemon() {
  [ -n "$PID" ] || return 0
  kill -TERM "$PID" 2> /dev/null
  for _ in $(seq 1 20); do kill -0 "$PID" 2> /dev/null || break; sleep 0.5; done
  kill -0 "$PID" 2> /dev/null && kill -KILL "$PID" 2> /dev/null
  wait "$PID" 2> /dev/null; PID=""
}
detach_disk() { [ -n "$RAMDEV" ] && hdiutil detach "$RAMDEV" -force > /dev/null 2>&1; RAMDEV=""; }
trap 'stop_daemon; kill "$MOCK" 2> /dev/null; detach_disk' EXIT

# api METHOD PATH [BODY]: body in $WORK/r.json, status in $CODE
api() {
  rm -f "$WORK/r.json"
  CODE=$(curl -s -m 30 -o "$WORK/r.json" -w '%{http_code}' -X "$1" \
    -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
    ${3:+-d "$3"} "http://127.0.0.1:$PORT$2")
  [ -f "$WORK/r.json" ] || : > "$WORK/r.json"
}
jr() { jq -r "$1" "$WORK/r.json" 2> /dev/null; }
newsess() { api POST /api/v1/sessions '{}'; jr .id; }
chat() { api POST /api/v1/chat "$(jq -nc --arg s "$1" --arg p "$2" '{messages: [{role: "user", content: $p}], session_id: $s}')"; }
receipt() { echo "$(jr '.memory_receipt // "none"')/$(jr '.memory_error_code // "none"')"; }
wait_run() {
  for _ in $(seq 1 60); do
    api GET "/api/v1/runs/$1"
    case $(jr .status) in completed | failed | cancelled) return 0 ;; esac
    sleep 0.5
  done
  return 1
}
mdb() { sqlite3 -cmd '.timeout 5000' "$DATA/memory.db" "$1"; }
sdb() { sqlite3 -cmd '.timeout 5000' "$DATA/agent24.db" "$1"; }
vers() { sqlite3 "$1" "SELECT group_concat(version) FROM (SELECT version FROM _sqlx_migrations WHERE success = 1 ORDER BY version)"; }
dump_hash() { sqlite3 "$1" .dump | shasum | cut -d' ' -f1; }
cli() { env AGENT24_DATA_DIR="$DATA" A24_OS_PACKAGES="$DATA.pk" "${CLI_BIN:-$NEW}/agent24" "$@"; }
PERSONAL_ROWS="SELECT row_count FROM mem_owner_usage u WHERE EXISTS (SELECT 1 FROM mem_os_partitions p WHERE p.owner_key = u.owner AND p.space_kind = 'personal')"
PERSONAL_EXEMPT="SELECT exempt_row_count FROM mem_owner_usage u WHERE EXISTS (SELECT 1 FROM mem_os_partitions p WHERE p.owner_key = u.owner AND p.space_kind = 'personal')"
CLEAN='usr:local|quota exceeded for owner|已记住'

# ============================== K: kernel ==============================
run_K() {
  if [ -z "$K1" ] || [ -z "$MAIN" ]; then
    check K-00 "#1294 acceptance (25 cases)" skip "needs --k1-script and --main"
  elif [ "$(cd "$(dirname "$K1")/.." && pwd)" != "$REPO" ]; then
    check K-00 "#1294 acceptance (25 cases)" fail "--k1-script must live in $REPO/scripts/ (it finds examples/node-module from its own location)"
  else
    "$K1" --old "$MAIN" --new "$NEW" --work "$WORK/k1" > "$WORK/k1.out" 2>&1; local rc=$?
    local summary; summary=$(tail -n 1 "$WORK/k1/report.md" 2> /dev/null)
    local known; known=$(grep -c '| KNOWN |' "$WORK/k1/report.md" 2> /dev/null)
    check K-00 "#1294 acceptance (exit 0, at most 1 KNOWN = G1)" "$([ $rc = 0 ] && [ "${known:-9}" -le 1 ] && echo ok || echo fail)" "rc=$rc; $summary; $WORK/k1/report.md"
  fi
  start_daemon "$NEW" "$WORK/k01" || { check K-01 "module-consents export" fail "$WORK/k01.serve.log"; return; }
  api GET /api/v1/module-consents/export
  check K-01 "module-consents export is 200 JSON" "$( [ "$CODE" = 200 ] && jq -e . "$WORK/r.json" > /dev/null && echo ok || echo fail)" "status $CODE"
  stop_daemon
  check K-02 "protocol-4 grant, card and stop-by-reference" skip "BLOCKED: no protocol-4 example module (gap G-K1)"
  check K-03 "grant confirmation card UI" skip "real machine; blocked with K-02"
}

# ============================== M: memory-p1 ==============================
run_M() {
  start_daemon "$NEW" "$WORK/m" || { check M-00 "daemon starts with the mock model" fail "$WORK/m.serve.log"; return; }
  local S AID BID
  # M-01 remember
  S=$(newsess); chat "$S" "记住我对花生过敏"; local c1=$CODE r1; r1=$(receipt)
  api GET '/api/v1/memory/assertions?limit=50'
  local n; n=$(jq '[.assertions[] | select(.text | contains("花生"))] | length' "$WORK/r.json" 2> /dev/null)
  AID=$(jr '[.assertions[] | select(.text | contains("花生"))][0].id // empty')
  check M-01 "remember: receipt saved, one active assertion" "$(is "$c1/$r1/$n" "200/saved/none/1")" "chat $c1 receipt $r1; assertions $n"
  # M-02 recall
  S=$(newsess); chat "$S" "我对什么过敏？"
  check M-02 "recall: no receipt, the model request carries the fact" "$( [ "$(jr 'has("memory_receipt")')" = false ] && tail -n 1 "$WORK/mock.log" | grep -q 花生 && echo ok || echo fail)" "$(receipt)"
  # M-03 retract
  api DELETE "/api/v1/memory/assertions/$AID"; local d1=$CODE
  api DELETE "/api/v1/memory/assertions/$AID"; local d2=$CODE
  api DELETE /api/v1/memory/assertions/not-an-id; local d3=$CODE
  S=$(newsess); chat "$S" "我对什么过敏？"
  local gone; tail -n 1 "$WORK/mock.log" | grep -q 花生 && gone=still_recalled || gone=gone
  check M-03 "retract: 204, 204 again, 404 unknown, no longer recalled" "$(is "$d1/$d2/$d3/$gone" "204/204/404/gone")" "$d1/$d2/$d3/$gone"

  # M-04..M-07 quota
  S=$(newsess); chat "$S" "记住我喜欢茶"; api GET '/api/v1/memory/assertions?limit=50'
  BID=$(jr '[.assertions[] | select(.text | contains("喜欢茶"))][0].id // empty')
  local pre; pre=$(mdb "SELECT count(*) FROM mem_os_partitions WHERE space_kind = 'personal'")
  local ORIG; ORIG=$(mdb "SELECT max_rows FROM mem_owner_quota WHERE owner = '*'")
  if [ -z "$BID" ] || [ "${pre:-0}" -lt 1 ] || [ -z "$ORIG" ]; then
    check M-04 "quota-full receipts" fail "precondition: assertion '$BID', personal partitions '$pre', default quota '$ORIG'"
  else
    local S5 S6
    S5=$(newsess); S6=$(newsess)
    mdb "UPDATE mem_owner_quota SET max_rows = ($PERSONAL_ROWS) WHERE owner = '*'"
    chat "$S5" "你好"; local a1; a1=$(receipt); local clean1; grep -qE "$CLEAN" "$WORK/r.json" && clean1=leak || clean1=clean
    chat "$S5" "记住我喜欢咖啡"; local a2; a2=$(receipt); local clean2; grep -qE "$CLEAN" "$WORK/r.json" && clean2=leak || clean2=clean
    check M-04a "full quota, ordinary turn: not_saved + memory_quota_rows, body clean" "$(is "$a1/$clean1" "not_saved/memory_quota_rows/clean")" "$a1 $clean1"
    check M-04b "full quota, explicit remember: failed + memory_quota_rows, body clean" "$(is "$a2/$clean2" "failed/memory_quota_rows/clean")" "$a2 $clean2"
    # M-05 retract at full quota, exempt counted apart
    local rc0 ex0 rc1 ex1
    rc0=$(mdb "$PERSONAL_ROWS"); ex0=$(mdb "$PERSONAL_EXEMPT")
    api DELETE "/api/v1/memory/assertions/$BID"; local d4=$CODE
    rc1=$(mdb "$PERSONAL_ROWS"); ex1=$(mdb "$PERSONAL_EXEMPT")
    api GET '/api/v1/memory/assertions?limit=50'; local still; still=$(jq --arg id "$BID" '[.assertions[] | select(.id == $id)] | length' "$WORK/r.json")
    check M-05 "full quota: retract 204, gone from the list, exempt counted apart" "$(is "$d4/$still/$rc1/$ex1" "204/0/$rc0/$((ex0 + 1))")" "status $d4; listed $still; rows $rc0->$rc1; exempt $ex0->$ex1"
    # M-06 content still refused
    chat "$S5" "记住我喜欢咖啡"
    check M-06 "full quota: content still refused after the retract" "$(is "$(receipt)" "failed/memory_quota_rows")" "$(receipt)"
    # partial: the turn's two rows fit, the assertion's audit row does not
    mdb "UPDATE mem_owner_quota SET max_rows = ($PERSONAL_ROWS) + 2 WHERE owner = '*'"
    chat "$S6" "记住我喜欢咖啡"; local a3; a3=$(receipt)
    api GET '/api/v1/memory/assertions?limit=50'; local coffee; coffee=$(jq '[.assertions[] | select(.text | contains("咖啡"))] | length' "$WORK/r.json")
    check M-04c "turn fits but the assertion does not: partial + memory_quota_rows, no assertion" "$(is "$a3/$coffee" "partial/memory_quota_rows/0")" "$a3; coffee assertions $coffee (row accounting: design c2b)"
    # M-07 restore
    mdb "UPDATE mem_owner_quota SET max_rows = $ORIG WHERE owner = '*'"
    S=$(newsess); chat "$S" "记住我喜欢咖啡"
    check M-07 "quota restored: saved again" "$(is "$(receipt)" "saved/none")" "$(receipt)"
  fi

  # M-08 HostSystem
  api PUT /api/v1/memory/settings '{"enabled":false}'; local p1=$CODE
  S=$(newsess); api POST /api/v1/runs "$(jq -nc --arg s "$S" '{prompt: "记住 X", session_id: $s}')"; local RUN; RUN=$(jr .id)
  wait_run "$RUN"; local st; st=$(jr .status)
  local hs; hs=$(sdb "SELECT count(*) FROM run_message_origins o JOIN run_messages m ON m.run_id = o.run_id AND m.seq = o.seq WHERE o.run_id = '$RUN' AND o.kind = 'HostSystem'")
  api PUT /api/v1/memory/settings '{"enabled":true}'
  S=$(newsess); api POST /api/v1/runs "$(jq -nc --arg s "$S" '{prompt: "你好", session_id: $s}')"; local RUN2; RUN2=$(jr .id)
  wait_run "$RUN2"
  local hs2 up2; hs2=$(sdb "SELECT count(*) FROM run_message_origins WHERE run_id = '$RUN2' AND kind = 'HostSystem'")
  up2=$(sdb "SELECT count(*) FROM run_message_origins WHERE run_id = '$RUN2' AND kind = 'UserPrompt'")
  check M-08 "paused memory: run completes, the notice is HostSystem (control: unpaused run has none)" "$( [ "$p1" = 200 ] && [ "$st" = completed ] && [ "$hs" = 1 ] && [ "$hs2" = 0 ] && [ "${up2:-0}" -ge 1 ] && echo ok || echo fail)" "pause $p1; run $st; HostSystem $hs; control HostSystem $hs2 UserPrompt $up2"

  # M-13 static contract
  local s=ok f
  for f in memory_quota_rows memory_quota_bytes memory_disk_low memory_disk_full not_saved partial; do grep -q "$f" protocol/openapi.yaml || s="fail(openapi:$f)"; done
  grep -q 'memory.write_failed' protocol/events.schema.json || s="fail(events)"
  check M-13 "protocol files carry the receipt states and error codes" "$( [ "$s" = ok ] && echo ok || echo fail)" "$s"
  # M-14 static part of the desktop display check (the display itself is a real-machine step)
  if grep -q 'not_saved' apps/desktop/src/renderer/pages/Chat.tsx && grep -q 'memory.not_saved' apps/desktop/src/renderer/pages/Chat.i18n.ts; then
    check M-14s "desktop Chat handles not_saved" ok "present"
  else
    check M-14s "desktop Chat handles not_saved" fail "Chat.tsx / Chat.i18n.ts have no not_saved" "G-M1: ordinary-turn write failure shows nothing on the desktop"
  fi

  # M-12 WAL checkpoint (slow)
  if [ "$SLOW" = 1 ]; then
    if [ -s "$DATA/memory.db-wal" ]; then
      local t=0 gone=no n0 n1
      api GET '/api/v1/memory/assertions?limit=50'; n0=$(jr '.assertions | length')
      while [ $t -lt 150 ]; do
        [ -s "$DATA/memory.db-wal" ] || { gone=yes; break; }
        sleep 5; t=$((t + 5))
      done
      api GET '/api/v1/memory/assertions?limit=50'; n1=$(jr '.assertions | length')
      check M-12 "idle WAL checkpoint empties memory.db-wal, data unchanged" "$(is "$gone/$n1" "yes/$n0")" "wal emptied after ${t}s: $gone; assertions $n0 -> $n1"
    else
      check M-12 "idle WAL checkpoint" skip "control failed: memory.db-wal was already empty, nothing to prove"
    fi
  else
    check M-12 "idle WAL checkpoint" skip "needs --slow (about 2.5 minutes)"
  fi
  stop_daemon
  check M-14 "desktop receipt texts" skip "real machine"
  check M-15 "desktop memory page retract" skip "real machine"
  check M-16 "run path memory_persisted shown on desktop/TUI" skip "BLOCKED: not implemented (gap G-M5)"
}

# disk gate: needs a small volume; macOS only
run_Mdisk() {
  if [ "$DISK" != 1 ]; then check M-09 "disk gate" skip "needs --disk"; check M-10 "retract floor" skip "needs --disk"; check M-11 "rejection trace" skip "needs --disk"; return; fi
  if ! command -v hdiutil > /dev/null; then check M-09 "disk gate" skip "no hdiutil (macOS only)"; check M-10 "retract floor" skip "no hdiutil"; check M-11 "rejection trace" skip "no hdiutil"; return; fi
  # seed two assertions on the normal disk
  start_daemon "$NEW" "$WORK/seed" || { check M-09 "disk gate" fail "$WORK/seed.serve.log"; return; }
  local S A B
  S=$(newsess); chat "$S" "记住我喜欢茶"; S=$(newsess); chat "$S" "记住我喜欢咖啡"
  api GET '/api/v1/memory/assertions?limit=50'
  A=$(jr '[.assertions[] | select(.text | contains("茶"))][0].id // empty'); B=$(jr '[.assertions[] | select(.text | contains("咖啡"))][0].id // empty')
  stop_daemon
  RAMDEV=$(hdiutil attach -nomount ram://524288 | awk '{print $1}')
  local RAMVOL="a24acc$$"
  if [ -z "$RAMDEV" ] || ! diskutil erasevolume HFS+ "$RAMVOL" "$RAMDEV" > /dev/null 2>&1; then
    detach_disk; check M-09 "disk gate" skip "could not create the RAM disk"; return
  fi
  # the real mount point of OUR device (a same-named volume would make it "/Volumes/<name> 1")
  RAMMNT=$(diskutil info "$RAMDEV" 2> /dev/null | sed -n 's/^ *Mount Point: *//p')
  if [ -z "$RAMMNT" ] || [ ! -d "$RAMMNT" ] || [ "$(df -k "$RAMMNT" | awk 'NR==2{print $1}' | sed 's/s[0-9]*$//')" != "$RAMDEV" ]; then
    detach_disk; check M-09 "disk gate" fail "cannot resolve the mount point of $RAMDEV (got '$RAMMNT')"; return
  fi
  cp -R "$WORK/seed" "$RAMMNT/data" && cp -R "$WORK/seed.pk" "$RAMMNT/data.pk" 2> /dev/null
  start_daemon "$NEW" "$RAMMNT/data" || { check M-09 "disk gate" fail "$RAMMNT/data.serve.log"; detach_disk; return; }
  chat "$(newsess)" "记住我喜欢乌龙茶"; local m1; m1=$(receipt)
  chat "$(newsess)" "你好"; local m2; m2=$(receipt)
  check M-09 "low free space: explicit remember failed, ordinary turn not_saved, both memory_disk_low" "$(is "$m1/$m2" "failed/memory_disk_low/not_saved/memory_disk_low")" "$m1 $m2"
  sleep 6
  api DELETE "/api/v1/memory/assertions/$A"; local e1=$CODE
  local free; free=$(df -k "$RAMMNT" | awk 'NR==2{print int($4 / 1024)}')
  dd if=/dev/zero of="$RAMMNT/fill" bs=1m count=$((free - 8)) > /dev/null 2>&1
  sleep 6
  api DELETE "/api/v1/memory/assertions/$B"; local e2=$CODE e2c; e2c=$(jr '.error.code'); local leak; grep -q 'usr:local' "$WORK/r.json" && leak=leak || leak=clean
  rm -f "$RAMMNT/fill"; sleep 6
  api DELETE "/api/v1/memory/assertions/$B"; local e3=$CODE
  check M-10 "retract floor: 204 with room, 507 memory_disk_low under 16 MiB, 204 after freeing" "$(is "$e1/$e2/$e2c/$leak/$e3" "204/507/memory_disk_low/clean/204")" "$e1/$e2/$e2c/$leak/$e3 (free ${free} MiB before filling)"
  sleep 3
  local rej; rej=$(sqlite3 "$RAMMNT/data/memory.db" "SELECT count(*) FROM mem_write_rejections WHERE code = 'memory_disk_low' AND rejected_kind = 'assertion.retracted' AND owner_kind = 'personal'")
  check M-11 "the refused retract left a trace row" "$( [ "${rej:-0}" -ge 1 ] && echo ok || echo fail)" "rows $rej"
  stop_daemon; detach_disk
}

# ============================== D: documenting ==============================
doc_status() { cli documents job "$1" --json 2> /dev/null | jq -r 'select(.type == "result") | .data.status // "?"' | tail -n 1; }
doc_wait() { # job want secs
  local t=0 s
  while [ $t -lt "$3" ]; do
    s=$(doc_status "$1"); [ "$s" = "$2" ] && return 0
    case $s in failed | cancelled | interrupted) [ "$2" != "$s" ] && return 1 ;; esac
    sleep 2; t=$((t + 2))
  done
  return 1
}
run_D() {
  if [ -z "$DOCPKG" ] || [ ! -f "$PDF" ]; then check D-01 "documents module" skip "needs --documents-pkg and a PDF ($PDF)"; return; fi
  DATA="$WORK/d"; mkdir -p "$DATA"
  cli os install "$DOCPKG" > "$WORK/d-install.out" 2>&1
  start_daemon "$NEW" "$WORK/d" "${NOMODEL_ENV[@]}" || { check D-01 "documents module mounts" fail "$WORK/d.serve.log"; return; }
  sleep 3
  cli os list > "$WORK/d-list.out" 2>&1
  api GET /api/v1/documents/capabilities; local cap=$CODE
  cli documents capabilities --json > "$WORK/d-cap.out" 2>&1
  check D-01 "documents is mounted, capabilities answer via REST and CLI" "$( (grep -Eq 'documents.*mounted' "$WORK/d-list.out" && [ "$cap" = 200 ] && jq -e 'select(.type == "result") | .ok' "$WORK/d-cap.out" > /dev/null) && echo ok || echo fail)" "$(tr '\n' ' ' < "$WORK/d-list.out" | cut -c1-120); capabilities $cap"
  # D-02 import, CLI and the desktop's --host-stdin path
  cli documents import "$PDF" --json > "$WORK/d2.out" 2> "$WORK/d2.err"; local rc=$?
  local res; res=$(jq -c 'select(.type == "result")' "$WORK/d2.out" | tail -n 1)
  local JOB; JOB=$(printf '%s' "$res" | jq -r '.data.job_id // empty')
  local acc; acc=$(printf '%s' "$res" | jq -r '.status')
  doc_wait "$JOB" succeeded 120; local waited=$?
  local DOC; DOC=$(cli documents job "$JOB" --json 2> /dev/null | jq -r 'select(.type == "result") | .data.document_id // empty' | tail -n 1)
  jq -nc --arg base "http://127.0.0.1:$PORT" --arg token "$TOKEN" --arg path "$(dirname "$(dirname "$PDF")")/s01-02-en-epa-boil-water/source.pdf" '{base: $base, token: $token, path: $path}' \
    | cli documents import --host-stdin --json > "$WORK/d2b.out" 2> /dev/null; local rc2=$?
  local JOB2; JOB2=$(jq -c 'select(.type == "result")' "$WORK/d2b.out" | tail -n 1 | jq -r '.data.job_id // empty')
  doc_wait "$JOB2" succeeded 120; local waited2=$?
  check D-02 "import via CLI and via the desktop's --host-stdin path, both succeed" "$( [ $rc = 0 ] && [ $rc2 = 0 ] && [ "$acc" = 202 ] && [ $waited = 0 ] && [ $waited2 = 0 ] && [ -n "$DOC" ] && echo ok || echo fail)" "rc $rc/$rc2; accepted $acc; jobs $JOB $JOB2; doc $DOC"
  # D-03 list and paging
  cli documents list --json | jq -c 'select(.type == "result")' > "$WORK/d3.json"
  local has; has=$(jq --arg d "$DOC" '[.data.documents[] | select(.document_id == $d)] | length' "$WORK/d3.json")
  cli documents list --limit 1 --json | jq -c 'select(.type == "result")' > "$WORK/d3a.json"
  local cur; cur=$(jq -r '.data.next_cursor // empty' "$WORK/d3a.json"); local first; first=$(jq -r '.data.documents[0].document_id' "$WORK/d3a.json")
  local second="" n2=""; if [ -n "$cur" ]; then cli documents list --limit 1 --cursor "$cur" --json | jq -c 'select(.type == "result")' > "$WORK/d3b.json"; n2=$(jq '.data.documents | length' "$WORK/d3b.json" 2> /dev/null); second=$(jq -r '.data.documents[0].document_id // empty' "$WORK/d3b.json" 2> /dev/null); fi
  check D-03 "list holds the document; --limit 1 pages to a different one" "$( [ "$has" = 1 ] && [ "$(jq '.data.documents | length' "$WORK/d3a.json")" = 1 ] && [ -n "$cur" ] && [ "$n2" = 1 ] && [[ "$second" =~ ^[A-Za-z0-9_-]+$ ]] && [[ "$first" =~ ^[A-Za-z0-9_-]+$ ]] && [ "$first" != "$second" ] && echo ok || echo fail)" "listed $has; page1 $first; page2 ($n2) $second"
  # D-04 get
  cli documents get "$DOC" --json | jq -c 'select(.type == "result")' > "$WORK/d4.json"
  local sha; sha=$(shasum -a 256 "$PDF" | cut -d' ' -f1)
  check D-04 "get: revision 1, size and sha256 are the file's" "$( [ "$(jq -r '.data.head_revision' "$WORK/d4.json")" = 1 ] && [ "$(jq -r '.data.head.content_sha256' "$WORK/d4.json" | sed 's/^sha256://')" = "$sha" ] && [ "$(jq -r '.data.head.size' "$WORK/d4.json")" = "$(wc -c < "$PDF" | tr -d ' ')" ] && echo ok || echo fail)" "$(jq -c '.data.head' "$WORK/d4.json" | cut -c1-160)"
  # D-05 text (503 engine_unavailable is retryable)
  local ok5=fail
  for _ in $(seq 1 15); do
    cli documents text "$DOC" 1 --json | jq -c 'select(.type == "result")' > "$WORK/d5.json"
    [ "$(jq -r '.ok' "$WORK/d5.json")" = true ] && { ok5=ok; break; }
    sleep 2
  done
  check D-05 "read the text of revision 1" "$ok5" "$(jq -c '{ok, status, code: .error.code}' "$WORK/d5.json")"
  # D-06 find, with a negative control
  local q; q=$(python3 -I - "$(dirname "$PDF")/source.pagetext.txt" <<'PY'
import sys
for line in open(sys.argv[1], encoding='utf-8'):
    s = ''.join(line.split())
    if s and not s.startswith('==='):
        print(s[:4]); break
PY
)
  if [ -n "$q" ]; then
    api POST "/api/v1/documents/documents/$DOC/find" "$(jq -nc --arg q "$q" '{revision: 1, query: $q}')"; local f1=$CODE m1; m1=$(jr '.matches | length')
    api POST "/api/v1/documents/documents/$DOC/find" '{"revision":1,"query":"zzzz-不存在-qq"}'; local f2=$CODE m2; m2=$(jr '.matches | length')
    check D-06 "find: a word from the page matches, a made-up word does not" "$( [ "$f1" = 200 ] && [ "$f2" = 200 ] && [ "${m1:-0}" -ge 1 ] && [ "$m2" = 0 ] && echo ok || echo fail)" "'$q' $f1 matches $m1; control $f2 matches $m2"
  else
    check D-06 "find" skip "no source.pagetext.txt next to the PDF"
  fi
  # D-07 error flows
  cli documents request '{"op":"nope"}' --json > "$WORK/d7a.out" 2>&1; local x1=$?
  cli documents get doc_01ZZZZZZZZZZZZZZZZZZZZZZZZ --json > "$WORK/d7b.out" 2>&1; local x2=$?
  printf 'not a document' > "$WORK/notes.txt"; cli documents import "$WORK/notes.txt" --json > "$WORK/d7c.out" 2>&1; local x3=$?
  AGENT24_DATA_DIR="$WORK/d-empty" "$NEW/agent24" documents list --json > "$WORK/d7d.out" 2>&1; local x4=$?
  local c1 c2 c3 c4
  c1=$(jq -r 'select(.type == "result") | "\(.status)/\(.error.code)"' "$WORK/d7a.out" | tail -n 1)
  c2=$(jq -r 'select(.type == "result") | .error.code' "$WORK/d7b.out" | tail -n 1)
  c3=$(jq -r 'select(.type == "result") | .error.code // "none"' "$WORK/d7c.out" | tail -n 1)
  c4=$(jq -r 'select(.type == "result") | "\(.status)/\(.error.code)"' "$WORK/d7d.out" | tail -n 1)
  check D-07 "errors: bad op 400, unknown id not_found, a .txt refused, no daemon 503" "$( [ $x1 = 1 ] && [ "$c1" = 400/invalid_request ] && [ $x2 = 1 ] && [ "$c2" = not_found ] && [ $x3 != 0 ] && [ "$c3" != none ] && [ $x4 = 1 ] && [ "$c4" = 503/backend_not_ready ] && echo ok || echo fail)" "$c1; $c2; .txt -> rc $x3 '$c3'; no daemon -> $c4"
  # D-08 cancel and retry on an extraction that waits for a model
  local body='{"revision":1,"schema":{"fields":[{"key":"k1","description":"任意字段","type":"text"}]}}'
  api POST "/api/v1/documents/documents/$DOC/extractions" "$body"; local sub=$CODE EJ; EJ=$(jr '.job_id // .id')
  sleep 3
  cli documents cancel "$EJ" --json > /dev/null 2>&1; doc_wait "$EJ" cancelled 90; local cx=$?
  cli documents retry "$EJ" --json > /dev/null 2>&1; sleep 2
  cli documents job "$EJ" --json | jq -c 'select(.type == "result")' > "$WORK/d8.json"
  local att; att=$(jq -r '.data.attempt' "$WORK/d8.json"); local st8; st8=$(jq -r '.data.status' "$WORK/d8.json")
  cli documents cancel "$EJ" --json > /dev/null 2>&1; doc_wait "$EJ" cancelled 90
  check D-08 "extraction job: cancel -> cancelled, retry -> attempt 2 and waiting again" "$( [ "$sub" = 202 ] && [ $cx = 0 ] && [ "$att" = 2 ] && [[ $st8 == queued || $st8 == running ]] && echo ok || echo fail)" "submit $sub; cancelled $cx; attempt $att; status after retry $st8"
  # D-09 cooldown (slow)
  if [ "$SLOW" = 1 ]; then
    api POST "/api/v1/documents/documents/$DOC/extractions" '{"revision":1,"schema":{"fields":[{"key":"k2","description":"另一个字段","type":"text"}]}}'; local CJ; CJ=$(jr '.job_id // .id')
    sleep 30; local early; early=$(doc_status "$CJ")
    local t=30 final=""
    while [ $t -lt 240 ]; do
      final=$(doc_status "$CJ"); [ "$final" = failed ] && break
      sleep 10; t=$((t + 10))
    done
    local ec; ec=$(cli documents job "$CJ" --json | jq -r 'select(.type == "result") | .data.error.code // "none"' | tail -n 1)
    check D-09 "no local model: the job waits (running at 30 s), then fails engine_unavailable" "$(is "$early/$final/$ec" "running/failed/engine_unavailable")" "30 s: $early; ${t} s: $final; $ec"
  else
    check D-09 "no local model: cooldown wait then engine_unavailable" skip "needs --slow (about 4 minutes)"
  fi
  # D-10 disable / enable
  cli os disable documents > "$WORK/d10a.out" 2>&1; local o1=$?
  sleep 2; api GET /api/v1/documents/capabilities; local after=$CODE
  cli os enable documents > "$WORK/d10b.out" 2>&1; local o2=$?
  stop_daemon
  start_daemon "$NEW" "$WORK/d" "${NOMODEL_ENV[@]}" || { check D-10 "disable/enable" fail "$WORK/d.serve.log"; return; }
  sleep 3
  cli documents list --json | jq -c 'select(.type == "result")' > "$WORK/d10.json"
  check D-10 "disable stops the module; after enable and restart the documents are still there" "$( [ $o1 = 0 ] && [ "$after" != 200 ] && [ $o2 = 0 ] && [ "$(jq --arg d "$DOC" '[.data.documents[] | select(.document_id == $d)] | length' "$WORK/d10.json")" = 1 ] && echo ok || echo fail)" "disable rc $o1; capabilities $after; enable rc $o2"
  stop_daemon
  check D-11 "authorization revoked: stop by reference" skip "BLOCKED: documents tools are not declared or grantable (gap G-D1/G-D2)"
  check D-12 "desktop documents page UI" skip "real machine"
  check D-13 "agent calls documents tools" skip "BLOCKED: same as D-11"
}

# ============================== W: weknora ==============================
run_W() {
  start_daemon "$NEW" "$WORK/w" || { check W-01 "knowledge host interface" fail "$WORK/w.serve.log"; return; }
  api PUT '/api/v1/host/knowledge/bindings/acc-b1?seq=1'; local w1=$CODE c1; c1=$(jr '.error.code')
  api DELETE '/api/v1/host/knowledge/bindings/acc-b1?seq=2'; local w2=$CODE
  api POST /api/v1/runs '{"prompt":"x","knowledge_binding_id":"acc-b1"}'; local w3=$CODE c3; c3=$(jr '.error.code')
  check W-01 "legacy-mode token cannot use the host interface or bind a run (403 x3)" "$(is "$w1/$w2/$w3" 403/403/403)" "$w1 '$c1' / $w2 / $w3 '$c3'"
  api GET /api/v1/tools
  check W-02 "no knowledge tool is advertised without a binding" "$( [ "$CODE" = 200 ] && [ "$(grep -c knowledge "$WORK/r.json")" = 0 ] && echo ok || echo fail)" "status $CODE"
  stop_daemon
  check W-03 "desktop launches its daemon in capabilities mode" skip "real machine"
  check W-04 "host channel round trip on a real desktop" skip "real machine; no user entry yet (gap G-W1)"
}

# ============================== I: idoris (ID-4, ID-6; others need a real iDoris, see section 4.5) ==============================
run_I() {
  # ID-6: no IDORIS_URL -> the old route is unchanged (oMLX mock answers).
  start_daemon "$NEW" "$WORK/i6" || { check ID-6 "daemon starts without IDORIS_URL" fail "$WORK/i6.serve.log"; return; }
  local before after S; before=$(wc -l < "$WORK/mock.log"); S=$(newsess)
  chat "$S" "ping"
  after=$(wc -l < "$WORK/mock.log")
  check ID-6 "no IDORIS_URL: chat 200 via the old route (oMLX mock hit)" "$( [ "$CODE" = 200 ] && grep -q pong "$WORK/r.json" && [ "$after" -gt "$before" ] && echo ok || echo fail)" "status $CODE mock.log $before->$after"
  stop_daemon

  # ID-4: stub iDoris gateway, locality header per mode file.
  cat > "$WORK/idoris-stub.py" <<'PY'
import json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
MODE_FILE, LOG = sys.argv[2], sys.argv[3]
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def _send(self, obj, extra=()):
        b = json.dumps(obj).encode()
        self.send_response(200); self.send_header('content-type', 'application/json')
        for k, v in extra: self.send_header(k, v)
        self.send_header('content-length', str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        self._send({"service": "idoris", "version": "0.3.1", "contract_version": "1.0.1", "status": "ok", "instance_id": "stub", "dev_no_key_enabled": False})
    def do_POST(self):
        n = int(self.headers.get('content-length') or 0)
        body = self.rfile.read(n).decode()
        with open(LOG, 'a') as f: f.write(body.replace("\n", " ") + "\n")
        mode = open(MODE_FILE).read().strip()
        extra = [("X-iDoris-Record-Id", "rec_stub_1")]
        if mode != "missing": extra.append(("X-iDoris-Served-Locality", mode))
        self._send({"model": "stub-real-model", "choices": [{"index": 0, "message": {"role": "assistant", "content": "idoris-pong"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}, extra)
ThreadingHTTPServer(('127.0.0.1', int(sys.argv[1])), H).serve_forever()
PY
  local IPORT; IPORT=$(python3 -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
  echo loopback > "$WORK/idoris-mode"; : > "$WORK/idoris-stub.log"
  python3 -I "$WORK/idoris-stub.py" "$IPORT" "$WORK/idoris-mode" "$WORK/idoris-stub.log" &
  local STUB=$!
  for _ in $(seq 1 20); do curl -s -m 1 "http://127.0.0.1:$IPORT/health" > /dev/null && break; sleep 0.3; done
  start_daemon "$NEW" "$WORK/i4" IDORIS_URL="http://127.0.0.1:$IPORT" IDORIS_API_KEY=idk_stub-key \
    || { check ID-4 "daemon starts with IDORIS_URL (stub)" fail "$WORK/i4.serve.log"; kill "$STUB" 2> /dev/null; return; }
  S=$(newsess)
  chat "$S" "ping"
  check ID-4a "locality=loopback is the positive control (200 + stub content)" "$( [ "$CODE" = 200 ] && grep -q idoris-pong "$WORK/r.json" && echo ok || echo fail)" "status $CODE"
  local m bad=""
  for m in missing lan remote; do
    echo "$m" > "$WORK/idoris-mode"
    chat "$S" "ping $m"
    if [ "$CODE" = 200 ] || grep -q idoris-pong "$WORK/r.json"; then bad="$bad $m(status $CODE)"; fi
  done
  check ID-4 "missing / lan / remote locality is an error and the result is not handed to the caller" "$( [ -z "$bad" ] && echo ok || echo fail)" "leaked:${bad:- none}"
  stop_daemon; kill "$STUB" 2> /dev/null
  run_I_real
  check ID-8 "offline success on a real machine" skip "real machine (jason laptop M1 Max): isolated HOME, oMLX 127.0.0.1:8088 with IDORIS_OMLX_API_KEY from ~/.omlx/settings.json auth.api_key (shell env only), card stays on_demand (not resident, iDoris#582); see the ID-8 section"
}

# ID-1/2/3/5 against the real iDoris archive + a deterministic HTTP fake backend (samples from #1370 / iDoris side).
run_I_real() {
  if [ -z "$IDORIS_HOME" ] || [ ! -x "$IDORIS_HOME/idoris" ]; then
    local w; for w in ID-1 ID-2 ID-3 ID-5; do check $w "real iDoris cases" skip "needs --idoris DIR (unpacked iDoris v0.3.1, checksum verified)"; done; return
  fi
  local IR="$WORK/idoris-run" FBP=18000 IP=8740
  curl -s -m 1 "http://127.0.0.1:$IP/health" > /dev/null && { check ID-1 "port $IP is free" fail "something already listens on $IP"; return; }
  rm -rf "$IR"; cp -R "$IDORIS_HOME" "$IR"
  cat > "$WORK/fb.py" <<'PY'
import json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
LOG = sys.argv[2]
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def _send(self, obj):
        b = json.dumps(obj).encode()
        self.send_response(200); self.send_header('content-type', 'application/json')
        self.send_header('content-length', str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        self._send({"object": "list", "data": [{"id": "fixture-small-chat", "object": "model"}, {"id": "fixture-chat", "object": "model"}]})
    def do_POST(self):
        n = int(self.headers.get('content-length') or 0)
        body = self.rfile.read(n).decode()
        with open(LOG, 'a') as f: f.write(body.replace("\n", " ") + "\n")
        m = json.loads(body).get("model", "fixture-chat")
        self._send({"id": "fb-1", "object": "chat.completion", "model": m, "choices": [{"index": 0, "message": {"role": "assistant", "content": "fixture-ok"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}})
ThreadingHTTPServer(('127.0.0.1', int(sys.argv[1])), H).serve_forever()
PY
  : > "$WORK/fb.log"
  python3 -I "$WORK/fb.py" "$FBP" "$WORK/fb.log" & local FB=$!
  # role card: copy omlx.yaml, change endpoint, append extensions (adjust if the card already has an extensions block: gap G-I6)
  local card="$IR/config/components/omlx.yaml"
  python3 -I - "$card" "$FBP" "daily" <<'PY'
import re, sys
p, port, with_daily = sys.argv[1], sys.argv[2], sys.argv[3] == "daily"
t = open(p).read()
t = re.sub(r'(?m)^endpoint:.*$', 'endpoint: "http://127.0.0.1:%s"' % port, t)
t = re.sub(r'(?ms)^extensions:.*?(?=^\S|\Z)', '', t).rstrip("\n") + "\n"
t += 'extensions:\n  _degradation: "Removing these bindings disables role aliases on this backend."\n  idoris_role_models:\n    fast: fixture-small-chat\n'
if with_daily: t += '    daily: fixture-chat\n'
t += '    deep: fixture-chat\n    decide: fixture-chat\n'
open(p, 'w').write(t)
PY
  export IDORIS_DEPLOY_MODE=personal IDORIS_DB_PATH="$WORK/idoris-state.sqlite3"
  local KEY KEYFULL ic_code IPID=""
  (cd "$IR" && printf '%s' '{"owner":"agent24","allowed_privacy":["local_only"],"allowed_roles":["fast","daily","deep","decide"]}' | ./idoris key issue --spec-stdin > "$WORK/key-min.json" 2> "$WORK/key-min.err")
  (cd "$IR" && printf '%s' '{"owner":"agent24-full","allowed_privacy":["local_only"],"allowed_roles":["fast","daily","deep","vision","embed","rerank","decide","auto"]}' | ./idoris key issue --spec-stdin > "$WORK/key-full.json" 2> "$WORK/key-full.err")
  KEY=$(jq -r .secret "$WORK/key-min.json" 2> /dev/null); KEYFULL=$(jq -r .secret "$WORK/key-full.json" 2> /dev/null)
  if [ -z "$KEY" ] || [ "$KEY" = null ]; then check ID-1 "issue a local_only key" fail "$WORK/key-min.err"; kill "$FB" 2> /dev/null; return; fi
  iserve() { (cd "$IR" && env "$@" ./idoris serve > "$WORK/idoris.log" 2>&1) & IPID=$!
    for _ in $(seq 1 40); do curl -s -m 1 "http://127.0.0.1:$IP/health" > /dev/null && return 0; kill -0 "$IPID" 2> /dev/null || return 1; sleep 0.5; done; return 1; }
  istop() { [ -n "$IPID" ] && kill "$IPID" 2> /dev/null; wait "$IPID" 2> /dev/null; IPID=""; sleep 0.5; }
  ic() { curl -s -m 20 -o "$WORK/ir.json" -w '%{http_code}' ${1:+-H "Authorization: Bearer $1"} -H 'content-type: application/json' -H 'X-iDoris-Privacy: local_only' \
    -d "{\"model\":\"$2\",\"messages\":[{\"role\":\"user\",\"content\":\"ping\"}]}" "http://127.0.0.1:$IP/v1/chat/completions"; }
  lastm() { tail -n 1 "$WORK/fb.log" | jq -r .model 2> /dev/null; }
  if ! iserve IDORIS_X=1; then check ID-1 "iDoris serves" fail "$WORK/idoris.log"; kill "$FB" 2> /dev/null; return; fi

  # ID-1
  local id; id=$(curl -s "http://127.0.0.1:$IP/health" | jq -r '[.service,.contract_version]|join("/")')
  local c1 c2 c3 c4; c1=$(ic "$KEY" idoris/fast); c2=$(ic "" idoris/fast); c3=$(ic wrong-key idoris/fast); c4=$(ic "$KEY" idoris/vision)
  local ok1=fail; case "$id/$c1/$c4" in idoris/1.0.1/200/403) case "$c2$c3" in 40[13]40[13]) ok1=ok ;; esac ;; esac
  check ID-1 "health identity; good key 200; no/wrong key 401|403; out-of-scope role vision 403" "$ok1" "$id good=$c1 nokey=$c2 wrong=$c3 vision=$c4"

  # ID-3
  local m1 m2 m3 c5; ic "$KEY" idoris/fast > /dev/null; m1="$(jq -r .model "$WORK/ir.json")/$(lastm)"
  ic "$KEY" idoris/deep > /dev/null; m2="$(jq -r .model "$WORK/ir.json")/$(lastm)"
  ic "$KEYFULL" fixture-small-chat > /dev/null; m3=$(lastm); c5=$(ic "$KEY" fixture-small-chat)
  check ID-3 "role -> real model upstream; concrete override only with an all-roles key" "$( [ "$m1" = fixture-small-chat/fixture-small-chat ] && [ "$m2" = fixture-chat/fixture-chat ] && [ "$m3" = fixture-small-chat ] && [ "$c5" != 200 ] && echo ok || echo fail)" "fast=$m1 deep=$m2 override=$m3 minkey-override=$c5"
  istop

  # ID-2: drop the daily binding, restart, daily must fail closed with zero backend calls
  python3 -I - "$card" <<'PY'
import sys
p = sys.argv[1]
lines = open(p).readlines()
open(p, 'w').write(''.join(l for l in lines if l.strip() != 'daily: fixture-chat'))
PY
  if iserve IDORIS_X=1; then
    local n0 n1 c6; n0=$(wc -l < "$WORK/fb.log"); c6=$(ic "$KEY" idoris/daily); n1=$(wc -l < "$WORK/fb.log")
    check ID-2 "unbound role fails closed (503) and the backend saw 0 requests" "$( [ "$c6" = 503 ] && [ "$n1" = "$n0" ] && echo ok || echo fail)" "status $c6 backend lines $n0->$n1 code=$(jq -r '.error.code // .code // "?"' "$WORK/ir.json" 2> /dev/null)"
  else
    check ID-2 "iDoris restarts after removing the daily binding" fail "$WORK/idoris.log"
  fi
  istop

  # ID-5 (d): invalid budgets must refuse to start
  local v bad5=""
  for v in -1 1.5 99999999999999999999999; do
    (cd "$IR" && env IDORIS_ALLOCATED_BUDGET_BYTES="$v" ./idoris serve > "$WORK/idoris-b.log" 2>&1) & IPID=$!
    sleep 2
    if kill -0 "$IPID" 2> /dev/null || curl -s -m 1 "http://127.0.0.1:$IP/health" > /dev/null; then bad5="$bad5 $v"; fi
    istop
  done
  check ID-5 "invalid IDORIS_ALLOCATED_BUDGET_BYTES (-1, 1.5, overflow) refuses to start; a/b/c recommendations are manual" "$( [ -z "$bad5" ] && echo ok || echo fail)" "started with:${bad5:- none}"
  kill "$FB" 2> /dev/null
}

# ============================== X: cross-line upgrade ==============================
run_X() {
  if [ -z "$PREVIEW" ] || [ ! -x "$PREVIEW/agent24d" ]; then check X-01 "upgrade from v0.5.1-preview.20261016.2" skip "needs --preview"; return; fi
  start_daemon "$PREVIEW" "$WORK/x" || { check X-01 "preview daemon starts" fail "$WORK/x.serve.log"; return; }
  local S AID TEXT RUN pv
  S=$(newsess); chat "$S" "记住我对花生过敏"; chat "$S" "你好"
  api GET '/api/v1/memory/assertions?limit=50'; AID=$(jr '.assertions[0].id'); TEXT=$(jr '.assertions[0].text')
  api POST /api/v1/runs '{"prompt":"升级前的 run"}'; RUN=$(jr .id); wait_run "$RUN"
  stop_daemon
  pv="$(vers "$WORK/x/agent24.db") | $(vers "$WORK/x/memory.db")"
  cp -R "$WORK/x" "$WORK/x-backup"; rm -f "$WORK/x-backup/daemon.json"
  start_daemon "$NEW" "$WORK/x" || { check X-01 "new daemon opens the preview data" fail "$WORK/x.serve.log"; return; }
  local sv mvr; sv=$(vers "$WORK/x/agent24.db"); mvr=$(vers "$WORK/x/memory.db")
  api GET /api/v1/health; local h=$CODE
  check X-01 "both libraries migrate in order: store 1..30, memory 1..20" "$( [ "$h" = 200 ] && [ "$sv" = "$(seq 1 30 | paste -sd, -)" ] && [ "$mvr" = "$(seq 1 20 | paste -sd, -)" ] && echo ok || echo fail)" "before $pv; after store ${sv##*,} memory ${mvr##*,}; health $h"
  api GET '/api/v1/memory/assertions?limit=50'
  local same; same=$(jq --arg id "$AID" --arg t "$TEXT" '[.assertions[] | select(.id == $id and .text == $t)] | length' "$WORK/r.json")
  api GET "/api/v1/sessions/$S"; local sc=$CODE
  api GET "/api/v1/runs/$RUN"; local rc=$CODE rs; rs=$(jr .status)
  S=$(newsess); chat "$S" "记住我喜欢茶"; local nr; nr=$(receipt)
  api GET /api/v1/memory/settings; local ms=$CODE
  check X-02 "old assertion, session and run survive; a new remember is saved" "$(is "$same/$sc/$rc/$rs/$nr/$ms" "1/200/200/completed/saved/none/200")" "assertion $same; session $sc; run $rc $rs; new $nr; settings $ms"
  stop_daemon
  # X-03 no way back
  cp -R "$WORK/x" "$WORK/x-down"; rm -f "$WORK/x-down/daemon.json"
  local before after; before=$(dump_hash "$WORK/x-down/agent24.db")
  if start_daemon "$PREVIEW" "$WORK/x-down"; then local opened=yes; stop_daemon; else local opened=no; fi
  after=$(dump_hash "$WORK/x-down/agent24.db")
  check X-03 "the preview refuses the upgraded library and leaves it unchanged" "$( [ $opened = no ] && grep -q 'previously applied but is missing' "$WORK/x-down.serve.log" && [ "$before" = "$after" ] && echo ok || echo fail)" "opened $opened; $(grep -m1 -o 'migration [0-9]* was previously[^"]*' "$WORK/x-down.serve.log" | cut -c1-90); dump unchanged: $( [ "$before" = "$after" ] && echo yes || echo no)"
  # X-04 old numbering on a dev machine
  local which line
  for which in "memory.db:18" "agent24.db:22"; do
    local file=${which%%:*} ver=${which##*:}
    local d="$WORK/x-old-$ver"
    cp -R "$WORK/x" "$d"; rm -f "$d/daemon.json"
    sqlite3 "$d/$file" "UPDATE _sqlx_migrations SET description = 'old_numbering', checksum = zeroblob(48) WHERE version = $ver"
    before=$(dump_hash "$d/$file")
    local hc="" mc="" mcode=""
    if start_daemon "$NEW" "$d"; then
      opened=yes
      api GET /api/v1/health; hc=$CODE
      api GET /api/v1/memory/assertions; mc=$CODE; mcode=$(jr '.error.code // empty')
      stop_daemon
    else opened=no; fi
    after=$(dump_hash "$d/$file")
    line=$(grep -m1 -E "migration $ver" "$d.serve.log" | cut -c1-140)
    if [ $file = memory.db ]; then
      # memory: deliberate degrade (server.rs) -- daemon serves, memory is off, WARN names the migration
      check "X-04/$ver" "old numbering in $file: daemon starts with memory off (health 200, memory 503 memory_unavailable), WARN names migration $ver, library unchanged" "$( [ $opened = yes ] && [ "$hc" = 200 ] && [ "$mc" = 503 ] && [ "$mcode" = memory_unavailable ] && [ -n "$line" ] && [ "$before" = "$after" ] && echo ok || echo fail)" "opened $opened; health $hc; memory $mc $mcode; '$line'; unchanged: $( [ "$before" = "$after" ] && echo yes || echo no)"
    else
      check "X-04/$ver" "old numbering in $file: start is refused, the error names migration $ver, library unchanged" "$( [ $opened = no ] && [ -n "$line" ] && [ "$before" = "$after" ] && echo ok || echo fail)" "opened $opened; '$line'; unchanged: $( [ "$before" = "$after" ] && echo yes || echo no)"
    fi
  done
  # X-05 the documented reset
  local d="$WORK/x-old-18" bk="$WORK/x-old-18.backup"
  local keep; keep=$(dump_hash "$d/memory.db")
  mv "$d" "$bk"
  if start_daemon "$NEW" "$d"; then
    api GET /api/v1/health; local rh=$CODE
    local rstore rmem; rstore=$(vers "$d/agent24.db"); rmem=$(vers "$d/memory.db")
    stop_daemon
    check X-05 "reset (move the data directory aside, restart): healthy, 30/20, backup untouched" "$( [ "$rh" = 200 ] && [ "$rstore" = "$(seq 1 30 | paste -sd, -)" ] && [ "$rmem" = "$(seq 1 20 | paste -sd, -)" ] && [ "$(dump_hash "$bk/memory.db")" = "$keep" ] && echo ok || echo fail)" "health $rh; store ${rstore##*,}; memory ${rmem##*,}"
  else
    check X-05 "reset steps" fail "$d.serve.log"
  fi
  check X-06 "signed DMG installed over the preview" skip "real machine (signed)"
  check X-07 "documents library 0001-0006 -> 0008" skip "manual: needs the documenting-r3-rc1 package"
  check X-08 "desktop: documents + memory after the over-install" skip "real machine"
}

want K && run_K
want M && { run_M; run_Mdisk; }
want D && run_D
want W && run_W
want I && run_I
want X && run_X

# Z1: ~/.agent24 untouched
if [ "$REAL_BEFORE" = absent ]; then [ -e "$REAL" ] && z=fail || z=ok
else [ -z "$(find "$REAL" -newer "$WORK/.start" -print -quit 2> /dev/null)" ] && z=ok || z=fail; fi
check Z-01 "the user's ~/.agent24 was not touched" "$z" "$REAL $REAL_BEFORE before"

{
  echo "# v0.6.0-preview.20261028.1 acceptance -- $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo
  echo "- new: \`$("$NEW/agent24d" --version 2> /dev/null)\` ($NEW); head $(git rev-parse --short HEAD)"
  echo "- machine: $(uname -sm); work dir $WORK"
  echo
  echo "| ID | check | result | evidence |"
  echo "|---|---|---|---|"
  cat "$WORK/results"
  echo
  echo "$(grep -c '| PASS |' "$WORK/results") passed, $(grep -c '| KNOWN |' "$WORK/results") known, $SKIPS skipped, $FAILS failed."
} > "$WORK/report.md"
echo "report: $WORK/report.md"
[ "$FAILS" = 0 ]
```

## 7. 缺口与疑问（起草时看到的，需要 David 或各线负责人回答）

| 编号 | 线 | 现象 | 为什么要紧 | 建议 |
|---|---|---|---|---|
| G-K1 | kernel | 仓库里没有声明协议 4 的示例模块；授予 POST、确认卡片、按引用停、结束通知、启用纪元都只有 CI 测试（#1294 自己列为「不在这里验」） | 按 #1293「对外功能缺用例就不合并」，这块是 #1280 的头号对外功能 | 补一个协议 4 的示例模块（node-module 的协议 4 版）并加用例 K-02；或 David 明确判为「用户看不到」并在 release PR 正文逐项写明 |
| G-K2 | kernel | #1280 正文写 head 为 27acdd72，现在是 2983d164（合了 main）；#1294 的验收报告也是在 27acdd72 上跑的。另外 `ab/kernel` 比 release 分支多 57 个提交，其中有 store 迁移 `0027_session_tool_result_origins`（#1309） | 若发布改为从新的 ab/kernel 重切（`K1-R2-ACCEPTANCE.md` 的 `ab/kernel` 版本写的就是「按 #1293 从新的 ab/kernel head 重切」），store 0027 会与 memory-p1 的 `0027_run_message_origins` **撞号**，X-01 的「连续 1..29」也要变 | 先定 kernel 的发布范围；若重切，memory-p1 要再改号一次（#1298 的做法），本清单的迁移号和 K-00 的脚本版本（改用 K-77 之后的版本）都要跟着改 |
| G-M1 | memory | 桌面 `Chat.tsx` 的 `MemoryReceipt` 类型和 `Chat.i18n.ts` 没有 `not_saved`；后端对普通轮次写入失败发 `not_saved`，界面什么都不显示（设计 D2 W1 要求显示「本轮未保存到记忆」） | 配额满或磁盘低时，用户的普通对话悄悄没存，违反「失败必须显式显示」 | 补 `not_saved` 类型、三种语言的文案和显示；M-14s 现在记 KNOWN |
| G-M2 | memory | 没有改配额的产品入口（设计 D8 / c6 依赖 CORE-2 在场证明，不在本次）；用例要直接改 `mem_owner_quota` | 只能直接改库来造「满额」 | 可接受（与 #1294 造旧授予同类），但 release PR 正文要写明 |
| G-M3 | memory | `protocol/openapi.yaml` 的 `memory_error_code` 描述仍写「`memory_disk_low` is reserved (nothing emits it yet)」，c4（#1296）已经会发 | 协议文档与行为不符 | 改文案，重新 `pnpm gen:api` |
| G-M4 | memory | `memory_disk_full`（真的写满）没有可稳定复现的用例，只有单元测试 | 类型化错误码里这一个靠不到 CLI 验 | 记为「可接受不跑」并在正文写明，或用 macOS 内存盘填满再试（不稳定，没放进脚本） |
| G-M5 | memory | run 路径的 `memory_persisted:false` 和 `memory.write_failed` 桌面、TUI 都没消费（P1-03 未做） | 配额满时 run 路径照常 `completed`，用户看不到记忆没写进去 | #1341 已声明 P1-03 在范围外；release PR 正文要写明这一条是已知缺口 |
| G-D1 | documenting | `domain-os.yml` 没有 `tools:` 声明（设计 DOC-1-06b 片 E 未做），七个文档工具已实现、已登记但调不到；#1126（按 `authorized_resources` 校验）、#1213/#1234（按引用停）都只能靠测试 | agent 用文档的对话入口这次不可用；撤销授权的停止行为没有用户可达的验证 | 按设计是预期（「K1-6a 与 K1-6b 验收前文档工具不授予」）；release PR 正文写明「文档对话入口不在本次」 |
| G-D2 | documenting | `domain-os.yml` 的 `min_daemon_protocol: 2`，而 kernel 规定只有协议 4 的模块才能拿授予（`protocol_too_old`） | 将来补上 `tools:` 时必须同时把协议提到 4，否则永远拿不到授予 | 补 `tools:` 的 PR 里一并改，并加一条用例 |
| G-D3 | documenting | documents 模块不随安装包分发、不默认安装，PDF 引擎只有 macOS | 「导入文档」对绝大多数用户这次不是开箱可用的功能；桌面文档页在没装模块时显示什么没有核实 | 发布说明写明手工安装；X-06 / D-12 时看一眼没装模块时的页面 |
| G-D4 | documenting | `find` 在 CLI 和桌面都没有入口（`ops.rs` 白名单没有），D-06 只能直接调 HTTP | #1293 要求 CLI 与桌面共用逻辑；这里两边都没有，属于只有 agent 工具和 REST | 可接受，但在 release PR 正文写明 |
| G-W1 | weknora | Provider 全部没接入 daemon；`knowledge.db` 不会被打开；Knowledge 工具没接入；host 通道没有用户入口 | 这条线的用户可见面几乎为零，W-04 无法写出可执行的用例 | 建议 David 判为「用户看不到」并逐项写进正文；接入 daemon 的那次发布再补用例（含 `knowledge.db` 的升级用例） |
| G-X1 | 跨线 | 代码里没有为「旧编号迁移」写专门的报错或重置指引，用户看到的只是 sqlx 的 `migration N was previously applied but has been modified / is missing`；`docs/Deployment/README.md` 只讲了用 `AGENT24_DATA_DIR` 隔离 | 需求是「明确报错且有清楚的重置步骤」 | X-05 的四步要写进 release notes 和 CHANGELOG；更好的做法是 daemon 在这类错误后追加一行指向该步骤的提示。另需要 David 确认：memory-p1 旧编号的开发库也按 K1-06b 矩阵 §6 第 4 条「不做兼容、清库」处理 |
| G-X2 | 跨线 | 起草时四线还没有合并，迁移号（store 29、memory 20）、文件名、`1..29` 连续性都来自各分支的 `ls-tree`，没在合并结果上验证 | 如果 #1341 吸收 main 或 #1280 重切时有改号，X-01 的期望值要改 | 合并后第一件事：跑 X-01 看连续性 |
| G-I1 | idoris | `ab/idoris` 还没有第二次吸收 main：#1365 先吸收一次，要等 #1358/#1364/#1368/#1366 进 main 后再吸收一次；`ab/idoris` → main 的 release PR 还没开 | 截止 2026-10-28 12:00（Asia/Bangkok）前要拿到 PR-Daemon APPROVE 并可合并；iDoris 合入顺序最后 | 前四线合入后立即吸收 main、开 release PR；赶不上则整条 I 组、第 8 节 idoris 一行和 checklist 的 iDoris 项一并移出本版 |
| G-I2 | idoris | 真实 iDoris v0.3.1 归档的 tag 指向提交、sha256、字节数在 #1370 里仍是「待回填」 | ID-1/2/3/5/8 要先校验 sha256 再用；Release Notes 要引用该 tar 的 sha256 | iDoris 打 tag 后回填 #1370；本文第 2 节的校验命令和 release notes 都按回填值写，**不要用占位值跑** |
| G-I3 | idoris | 已解决（改写）：`temperature` 只在模块推理回调里可设、没有用户入口，且 iDoris 生命周期路径的请求字段是白名单（`temperature` 会 400 `unsupported_parameter`）。结论：#1365 改为 iDoris 网关路径不发 `temperature`，其它 provider 照 main；ID-7 移出用户路径，改单测/抓上游请求证明，Release Notes 写「已知限制：iDoris 网关当前忽略 temperature」 | 剩余：**#1365 的修复（含单元测试）要先进 `ab/idoris`**，否则带 temperature 的模块调用在 iDoris 路径上会直接失败 | iDoris 侧在 #1365 单独提交修复；本表在它合入后关闭 |
| G-I4 | idoris | 已解决：key spec、`idoris_role_models` 角色卡、负例构造方法、额度写法已由 iDoris 侧在 #1370 回帖（issuecomment-6436424430）给出，已写进 4.5 节和脚本。剩余：脚本里改卡的做法假定 `config/components/omlx.yaml` 存在 `endpoint:` 行，且 serve 命令、配置目录是否从当前目录读取是依据回帖推断的 | 若与真实 v0.3.1 归档布局不符，ID-1/2/3/5 会先红 | 第一次执行以归档实际布局为准修正脚本（G-I6 同） |
| G-I5 | idoris | ID-8 需要隔离 HOME、已缓存的小模型和本地推理后端；iDoris 归档不含推理后端与权重 | fixture 不能代替这一项；没有真机就不能算已验 | 已指定真机：jason 笔记本 M1 Max，iDoris 侧已预演通过（v0.3.1 候选二进制，oMLX 8088 + 认证，on_demand 卡片）；正式 v0.3.1 归档仍需在此机跑一遍，做不了的记 SKIP 并在 release PR 正文交代 |
| G-I6 | idoris | 一键脚本的 ID-4 用桩网关；ID-1/2/3/5 的 `run_I_real` 假定 iDoris 默认数据面端口 8740、`./idoris serve` 在归档目录里运行、卡里没有别的 `extensions:` 块（脚本会整块替换）；ID-5 的 a/b/c 与 ID-3 的 Settings 部分仍手工；ID-2 的停后端与 `IDORIS_REMOTE_KILL_SWITCH=1` 两步也手工。ID-4 的桩网关模拟的是 `/health` 与 `/v1/chat/completions` 的最小形状（`service`、`contract_version`、两个头），没有对着真实 iDoris 响应核对；daemon 端对 chat 路由到 `idoris-local` 的触发条件（是否需要 role/模型设置）起草时没有跑 | 桩与真实形状有出入时 ID-4a 的正对照会先红，应先修桩 | 第一次执行以 ID-4a 的结果判断桩是否可用；真实形状以 ID-3 抓到的响应为准 |

## 8. release PR 正文模板

```
## 完整用例验收（#1293 规则）
- 环境：<机器、系统>，二进制 <head>，数据目录 <WORK>，不碰 ~/.agent24（Z-01）
- 命令：scripts 见 docs/release/ACCEPTANCE-v0.6.0-preview.md 第 6 节；完整报告 <report.md 贴在评论>
- 结果：N 项 PASS，M 项 KNOWN（逐条写编号和原因），K 项 SKIP（逐条写原因）
- 真机项：K-03、M-14、M-15、D-12、W-03、W-04、ID-8、X-06、X-08：<谁在哪台机器上做的、结果>
- 阻塞项（不能算已验）：K-02、M-16、D-11、D-13：<逐条写：为什么用户现在够不着，或为什么判为用户看不到，David 的决定>
- idoris（#1370）：使用真实 iDoris v0.3.1 归档，sha256 <回填值，已校验>；ID-1…ID-6、ID-8 逐条结果 <PASS/KNOWN/SKIP>；**已知限制：iDoris 网关当前忽略 `temperature`**（经 iDoris 网关的请求不发 temperature，其它 provider 照旧；证明用 #1365 单测，不列用户路径用例）；本期不做项（launcher/Admin FD 交接、自动安装、`idoris-any`、流式）不得宣称
- 不跑用例的改动：见清单各线「不跑用例的改动（逐项）」
- 桌面端需补用例的缺口（CLI 入口未补齐）：W-03（托管 daemon）、D-12 里的界面部分
```
