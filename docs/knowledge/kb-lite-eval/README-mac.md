# KB-LITE-01 David Mac 平衡档实测步骤（交监工执行）

> 制品：WeKnora-lite-headless 0.9.0，源码 70c8d1d2f8cc（= a24/develop 46a882c6 + #179 M5 修复），darwin/arm64，已签名。
> mini 上同一制品的结果见 Agent24 `docs/knowledge/KB-LITE-EVAL-2026-11.md`。本步骤只起一次性进程、一次性目录，不改 App、不碰 ~/.agent24、不写 iDoris 的 key 库。
> 测完删掉 `~/lite-eval`。全程约 10 分钟。

## 0. 前提（监工核对）
- oMLX 切到**平衡档**（备份 `~/.agent24-k4/omlx-settings.balanced.bak.json`，切档由监工做），测完按原规矩切回激进档。
- Agent24 App 正常运行、daily 模型（Qwen3-8B-4bit）已加载过一次；记下当前 `memory_pressure` 和 oMLX 日志行数：
  `memory_pressure | tail -1; wc -l ~/.omlx/logs/server.log`

## 1. 把包拉到 MacBook（从 MacBook 侧发起；mini 连不过去）
包在 mini：`/Volumes/UltraDisk/Dev2/caches/dev-pkg/lite-eval-mac/`（79 MB）。
```
mkdir -p ~/lite-eval && scp -r <mini 的 ssh 目标>:/Volumes/UltraDisk/Dev2/caches/dev-pkg/lite-eval-mac/ ~/lite-eval/bundle
cd ~/lite-eval/bundle
bash verify-lite-headless.sh WeKnora-lite-headless_0.9.0_70c8d1d2f8cc_darwin_arm64.manifest.json \
  WeKnora-lite-headless_0.9.0_70c8d1d2f8cc_darwin_arm64.tar.gz allowed_signers 70c8d1d2f8cca1b2d8b043a22131a2c8fd362808
# 期望最后一行：OK 70c8d1d2f8cca1b2d8b043a22131a2c8fd362808 darwin/arm64
ls -l *.tar.gz                      # 记：归档体积
mkdir ../pkg && tar -xzf *.tar.gz -C ../pkg && du -sh ../pkg/*    # 记：解压后体积
xattr -dr com.apple.quarantine ../pkg   # scp 来的一般没有隔离属性，有就去掉
```

## 2. embedding 端点
- 首选：oMLX 的 OpenAI 兼容端点（与 App 同一个模型服务，内存同住才真实）：`--embed-base http://127.0.0.1:8000/v1 --embed-model bge-m3-mlx-fp16`。
  先确认：`curl -s 127.0.0.1:8000/v1/models | head -c 400`。oMLX 要 API key 时，把 key 写进 `~/lite-eval/embed.key`（chmod 600），加 `--embed-key-file ~/lite-eval/embed.key`；不要打印 key。
- 经 iDoris 网关的路径已在 mini 上打通（需要 iDoris virtual key，App 的 key 在钥匙串里，不在 David Mac 上另签）。

## 3. 跑（同时开第二个终端制造对话负载）
终端 A：
```
cd ~/lite-eval/bundle
python3 -I run_lite_eval.py --pkg ~/lite-eval/pkg/WeKnora-lite-headless_0.9.0_70c8d1d2f8cc_darwin_arm64 \
  --work ~/lite-eval/run --docs ~/lite-eval/bundle/docs \
  --embed-base http://127.0.0.1:8000/v1 --embed-model bge-m3-mlx-fp16 --runs 3 --idle-seconds 20 \
  | tee ~/lite-eval/result-mac.jsonl
```
终端 B：在终端 A 打出 `"step": "kb_create"` 之后马上跑（导入期间让 daily 模型同时推理 3 次；模型名以 `curl 127.0.0.1:8000/v1/models` 为准）：
```
for i in 1 2 3; do curl -s -m 120 127.0.0.1:8000/v1/chat/completions -H 'content-type: application/json' \
  -d '{"model":"Qwen3-8B-4bit","messages":[{"role":"user","content":"用三句话介绍上海 /no_think"}],"max_tokens":200}' \
  -o /dev/null -w "chat $i %{http_code} %{time_total}s\n"; done
```

## 4. 收集（贴回给编码会话）
```
cat ~/lite-eval/result-mac.jsonl
memory_pressure | tail -1
tail -n +<第 0 步记下的行数> ~/.omlx/logs/server.log | grep -E -c 'Memory pressure|Aborted'
tail -n +<第 0 步记下的行数> ~/.omlx/logs/server.log | grep -E 'Memory pressure|Aborted' | tail -5
pgrep -fl WeKnora-lite   # 应为空（脚本结束时已停掉）
```
判据：`import` 一行 `status_counts` 全为 completed、`search_status` 200；oMLX 日志在测试窗口内**没有 Aborted**；终端 B 三次都是 200。

## 5. 清理
`rm -rf ~/lite-eval`，oMLX 切回激进档。
