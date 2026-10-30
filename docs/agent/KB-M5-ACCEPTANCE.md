# Knowledge M5：样本库端到端验收用例（#1001 退出验收 1）

- **对象**：在主对话里检索样本知识库，按证据作答，并给出来源。这是 11-11 版本的主线（M1–M5）。
- **怎么跑**：先在本机起一个 WeKnora，见下面「WeKnora 怎么起」；然后在本 checkout 里运行：

  ```
  python3 -I scripts/kb-m5-accept.py --bin <含 agent24d 的目录> --weknora http://127.0.0.1:<app 端口> [--work <目录>] \
      [--model-url <Ollama 地址> --model <模型名>]
  ```

  - `--bin` 指向 `cargo build -p agent24d` 的产物目录。
  - 不带 `--model-url` 时，模型是脚本内置的 stub（只监听本机 loopback），结果可重复。带上之后，用指定的 Ollama 模型作答（例如开发期的 Modal 代理），中间经过一个记录器，用来取得模型看到的工具结果。
  - 脚本逐条打出 PASS 或 FAIL，并在工作目录里写 `report.md`。只要有一条 FAIL，就以非 0 退出。
- **按桌面的做法走**：
  - daemon 的拉起方式与 `apps/desktop/src/main/backend-manager.ts` 相同：`agent24d serve --port 0 --auth-mode capabilities --host-bootstrap-stdio`，从 ready 行取 `product_host_token`，stdin 一直开着。
  - 脚本替代桌面 main 做这几件事（`apps/desktop/src/main/knowledge-settings.ts`、`knowledge-host.ts`）：
    1. 用端点 body 激活绑定；
    2. 把 retrieve key 作为 `api_key` 类型的 C 推进去；
    3. 新建 session；
    4. 新建指名该绑定的 run（`chat-knowledge.ts`、`backend-proxy.ts` 的 `withKnowledgeBinding`）。
- **不碰用户数据、不留进程**：
  - `--work` 不能在 `~/.agent24` 里，并且必须不存在或为空。
  - daemon 用工作目录下自己的 `AGENT24_DATA_DIR`、`A24_OS_PACKAGES` 和 `HOME`；调用者 shell 里以 `A24_`、`AGENT24`、`OMLX_`、`OLLAMA_` 开头的变量和 `DEFAULT_MODEL` 都不传给它。
  - ready 行最多等 30 秒，按总时限读，不会卡在半行上。
  - 伪 Ollama、模型 stub 和 daemon 从创建起都在 `try/finally` 里，出错也会停掉、释放端口。daemon 靠关闭 stdin 来停，15 秒内不退出就杀掉。
- **WeKnora 侧的数据**：每次运行都通过 WeKnora API 新建以下数据，不碰已有数据：
  - 一个新用户；
  - 一个只开关键词检索的知识库；
  - 一份合成的样本文档（蘑菇镇，纯属虚构）；
  - 一把只对这个知识库有效的 retrieve key。

  v0.8.x 即使只做关键词检索也要 embedding，所以脚本在 `127.0.0.1:11434` 起一个伪 Ollama，返回按文本哈希得出的固定向量。这个端口必须空闲。

## 用例
| 编号 | 检查 | 判定 |
|---|---|---|
| E0 | 按 main 的做法激活绑定并推入 C | 两次 PUT 都返回 204 |
| E1 | 带绑定的主对话 run 完成 | run 状态为 `completed` |
| E2 | 检索 | 这一轮的第一个工具结果是 `knowledge.search`，有证据，每条都来自本次新建的知识库，并有一条的 snippet 含完整的样本事实（「每周三晚上八点闭馆」和「林小满」） |
| E3 | 先读回，再引用 | 这一轮里有 `knowledge_read` 的结果，给出带 `content_tag` 的 `use: citation` 证据。回答在「来源」之前的正文（去掉 Markdown）里，同一行有「周三」「闭馆」和晚上八点（晚上八点、晚上8点、20:00、20点；八点半、20:30 不算），并有「林小满」。「来源」之后列出的每个 `knowledge_id#chunk_id` 都是读回的 citation，至少一个。stub 的回答正文就是读回的那段文字，不是写死的答案 |
| E4 | 同一会话的下一轮照样能检索 | 第二轮 run 完成。只看这一轮（最后一条用户消息之后）的工具结果，其中有含样本事实的证据 |
| E5 | 回源 | 在新的一轮里，按引用（`knowledge_id`、`chunk_id`、`content_tag`）调 `knowledge_read`。读回的同一个 `chunk_id` 块必须是 `use: citation`，`content_tag` 非空且与命中一致，这一块的正文含样本事实 |
| E6 | 同一个 run 里检索第二次 | 两次检索都拿到含样本事实的证据（Knowledge 同源例外，ADR-K1-02 §2.1.1 第 6 条） |
| N1 | 不带绑定的 run | run 完成，模型收到的工具列表里既没有 `knowledge_search`，也没有 `knowledge_read` |
| N2 | Off 之后 | DELETE 绑定后，指名该绑定的新 run 在创建时被拒：409 `knowledge_binding_inactive` |
| N3 | 检索成功之后调 `http_fetch` | 先确认这一轮的检索拿到了证据，随后的 `http_fetch` 被出口策略拒绝（`denied by policy`）：同源例外只放行 Knowledge 自己 |

真实模型模式只跑 E0–E3。

## WeKnora 怎么起
- 用 WeKnora fork `a24/develop` 本地构建的 app 镜像，构建脚本是 `scripts/agent24/probe/build-local-app.sh`。官方镜像里没有 `/api/v1/agent24/knowledge/*`。
- app 容器要设置 `AGENT24_KNOWLEDGE_TAG_KEY`，值为 standard base64、至少 32 字节。没有这个变量时，search 和 read 都关闭，返回 `forbidden`/`operation_disabled`。
- 设置方法：在栈目录里另写一个 compose 覆盖文件，把 app 的 image 换成本地构建的 tag，并加上这个变量，然后执行 `docker compose -f docker-compose.yml -f <覆盖文件> up -d --no-build app`。
- 不要用独立模式之外的 `AGENT24_KNOWLEDGE_MODE`：集成模式下注册、建库等接口都返回 404，脚本没法建数据。

## 结果
见 PR 正文中贴出的 `report.md`。
