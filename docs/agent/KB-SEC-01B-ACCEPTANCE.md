# KB-SEC-01b 全路径出口验收

- **对象**：#1089。Knowledge 证据进了 run 以后，daemon 的每条出口都守住 LocalOnly，远端档收到的请求数为 0。这是 `docs/knowledge/K1-DEPENDENCIES.md` §3 私有数据禁用门 G1(b) 的证据，T1、T2 两条是 G1(a) 要的 Agent24 → WeKnora 传输用例。
- **怎么跑**：在要验的那个提交的 checkout 里编 `cargo build -p agent24d`，再跑 `python3 -I scripts/kb-sec-01b-accept.py --bin <agent24d 所在目录> [--work <目录>]`。脚本逐条打出 PASS/FAIL，在工作目录写 `report.md`；有一条 FAIL 就以非 0 退出。大约 20 秒跑完。
- **只验 daemon，其余都是本机回环上的假件**，不用起演示栈，也不用真模型：
  - 模型：脚本里的假模型按提示词里的 `SEC:<步骤,…>` 依次发工具调用，最后把各个工具结果原样回给 run。它同时扮演两档：`OMLX_URL=http://127.0.0.1:<端口>` 是本地档；`OLLAMA_URL=http://[::ffff:127.0.0.1]:<端口>` 是远端档。后者用的是 v4 映射地址，路由器会把它标成 Remote，远端档收到的每个请求都记下来计数。
  - WeKnora：按契约 v1 和 `protocol/knowledge/fixtures/v1` 写的 integration 模式假服务，通过 main 的 L3 闸门（capabilities、ready）。`content_tag` 在客户端只做字符串比较，所以假服务不需要标签密钥。
  - MCP：一个 stdio MCP server，只有一个 `ping` 工具，每收到一次 `tools/call` 就记一行。
- **按 desktop main 的方式起 daemon**：capability 模式，`--host-bootstrap-stdio`；激活绑定并推入凭据；`AGENT24_DATA_DIR`、`HOME` 都在工作目录下；清掉继承的 `A24_`、`AGENT24`、`OMLX_`、`OLLAMA_`、`IDORIS_` 环境变量；`A24_SCHEDULER_TICK_SECS=1`。
- **#994 的口径**：LocalOnly 严格指本机回环（#994 David 10-25 定）。所以回环上的 WeKnora 算本机；非回环端点只能用 https，属于组织自有端点那一档，由 KB-SVC-POLICY-01（WeKnora#42）管。

## 用例

#1089 列的十条路径，每条至少对应一个用例。

| ID | 路径 | 怎么验 | 判据 |
|---|---|---|---|
| E0 | — | 激活绑定，推入凭据 | 两次都是 204 |
| O1 | 对照 | 没有 Knowledge 证据的三个 run，各走一次 http_fetch、MCP、shell_exec | 都放行：http_fetch 走到 DNS 才失败（目标是 `.invalid` 域名，不会真连出去），MCP server 收到 1 次调用，shell 建出了文件 |
| A1 | 回答 | 带绑定的 run 调 knowledge_search，再作答 | run 完成；结果里有样本事实，只出现在本地档收到的请求里 |
| H1 | HTTP | knowledge_search → http_fetch | http_fetch 得到 `denied by policy` |
| X1 | Exec | knowledge_search → shell_exec `touch` | 被拒，文件没建出来 |
| P1 | MCP | knowledge_search → MCP `ping` | 被拒，MCP server 没收到调用 |
| D1 | 委派 | knowledge_search → explore | 子 agent 的模型调用到了本地档 |
| C1 | 续轮 | 同一会话：第一轮 knowledge_search，第二轮只调 http_fetch | 第二轮的 http_fetch 被拒 |
| B1 | 后台 | knowledge_search → self_wake 1 秒后唤醒 | 被唤醒的 run 里 http_fetch 被拒 |
| R1 | 恢复 | knowledge_search → fs_write 停在审批上，`kill -9` 掉 daemon，重启后批准 | run 接着跑完，后面的 http_fetch 被拒 |
| S1 | 摘要 | 一个会话第一轮 knowledge_search，再跑 20 轮，第 21 轮触发压缩 | 摘要调用到了本地档，远端档一次没有 |
| M1 | 记忆 | 在带 knowledge_search 的 run 里 `remember that …`；换一个会话、不带绑定，用提示词召回这条事实，再调 http_fetch | 召回生效（提示里有 `[记忆数据`），http_fetch 被拒 |
| T1 | 传输 | 先 shell_exec（批准，输出算 Unknown），再 knowledge_search | knowledge_search 被拒，假 WeKnora 的 search 计数不变（发送前就拦下） |
| T2 | 传输 | 用 `http://192.0.2.10/api/v1` 激活另一个绑定 | 400 |
| L1 | LocalOnly | 另起一个 daemon，`OMLX_URL` 和 `OLLAMA_URL` 都指向远端档 | run 以 `no local provider available for a local-only task` 失败，`/api/v1/chat` 回 503 并带同一句话 |
| Z1 | 全程 | 两个 daemon 跑完后，统计远端档收到的请求 | 0 |

**说明**：
- 生产里所有模型调用都是 LocalOnly（`agent24-store/src/source_policy.rs` 的构造器写死了 `mode: LocalOnly`），后面还有一层 `deny_all` 出口门。所以在 Z1 的配置下，就算 LocalOnly 排除失效，远端计数也还是 0。能证明排除本身在起作用的是 L1：它的报错只会出现在「LocalOnly 且没有本地 provider」这个分支（同 `docs/design/K1-06b-egress-matrix.md` §2 末尾的说法）。
- 依赖检索的用例都先确认 knowledge_search 带回了样本事实。检索失败的结果同样会打上 Unknown 标签，不先确认这一点，后面的拒绝就证明不了是 Knowledge 证据起的作用（见反面对照第三组）。
- 工具出口只在 run 带着非 Ordinary 来源时才拦。Knowledge 的结果算 Unknown（`Tool::result_source` 的默认值）。knowledge_search 之后再调 Knowledge 照常放行，这是同源例外（`knowledge_tools.rs`）。
- M1 放在最后跑：召回按提示词做全文检索，它留下的 Unknown 事实里没有 `SEC`，前面用例的提示词召回不到它，所以不会影响这些用例。

## 反面对照

前两组在同一棵树上改一行、重编，第三组只改脚本副本，都照原样跑；结果见 PR 正文。

| 改动 | 预期 |
|---|---|
| `KnowledgeTool` 的 `result_source` 改为 Ordinary（假装 Knowledge 证据是普通资料） | H1、X1、P1、C1、B1、R1、M1 共 7 条变红；其余 9 条照旧通过，因为模型路径不看来源分类，一律 LocalOnly |
| 路由层 `tier_order` 给 LocalOnly 也排上 Remote | 只有 L1 变红，报错变成出口门拒绝；Z1 照旧是 0，靠的是 `deny_all` |
| 脚本副本里让假 WeKnora 的 search 一律回 500（daemon 不改） | A1、H1、X1、P1、D1、C1、B1、R1、S1、M1 共 10 条变红；E0、O1、T1、T2、L1、Z1 照旧通过 |

## 不在这里验的

- **子进程自己出网**：MCP server、模块进程、shell 子进程的网络不在沙箱里。X1、P1 只证明调用在执行前就被拒（同出口矩阵 §4）。
- **模块工具与模块模型回调**：要一个声明了工具、带授予的协议 4 模块，仓库里没有这样的示例包。由 `module_tool_runtime.rs`、`model_callback.rs` 的测试和 `me4_model_blackbox.rs` 覆盖。
- **获准云处理**：生产里没有任何路径会授予 CloudAuthorized，云授权的正例只能在单元层用测试 gate 做（出口矩阵 (c) 列）。
- **WeKnora 服务端自己的出口**（embedding、摘要、rerank）：属于 KB-SVC-POLICY-01（WeKnora#42）。
