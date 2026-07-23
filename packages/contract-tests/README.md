# @agent24/contract-tests

协议契约测试——对任意 Agent24 daemon 实现跑同一套断言（今天：node daemon；B5 起：agent24d Rust daemon 双跑），经 `A24_BASE_URL` 参数化。

## 运行

可分套件运行：`pnpm test:current`（现状端点，仅 node daemon）/ `pnpm test:v1`（v1 契约，双后端）。

```bash
# 1. 构建并启动 daemon（不需要 Electron）
pnpm build                      # 仓库根目录（tsc → dist/backend/server.js）
node dist/backend/server.js &

# 2. 跑契约测试
pnpm --dir packages/contract-tests install
pnpm --dir packages/contract-tests test
```

> 注：`pnpm -F contract-tests` 的 workspace 过滤在 A4（pnpm workspace 重构）落地后可用；
> 在那之前用 `--dir`。CI 集成在 A6。

## 环境变量

| 变量 | 默认 | 说明 |
|---|---|---|
| `A24_BASE_URL` | `http://127.0.0.1:8765` | 被测 daemon 地址（agent24d 用动态端口时传入）。不用 `BASE_URL`——那是 Vite 内置变量，vitest 会注入 `'/'` 覆盖外部值 |
| `A24_TOKEN` | 空 | Bearer token（agent24d 需要；node mock 免） |
| `A24_EXPECT_LLM` | `auto` | `up`=强制断言 200 成功契约；`down`=强制断言 503 契约；`auto`=本地探测（**CI 下 auto 直接报错**，必须显式二选一，防止静默跳过造成假绿） |
| `A24_TARGET` | `node` | `node`=运行 current（pre-v1）套件；其他值（如 `rust`）跳过 current，仅 v1 套件生效——防止把 node-daemon 专属契约误跑到 agent24d 上 |

## 结构

- `src/current.test.ts` — 现状（pre-v1）端点契约：/health、/api/llm/*、/api/modules*；
  `/api/modules` 的 manifest 会用 `protocol/module.schema.json` 逐个校验
- `src/v1.test.ts` — v1 契约骨架（`it.todo`），按里程碑激活（A5 → M-A 组；C1..C5 → M-C 组）
- LLM 相关正例按运行时探测（oMLX:8088 / Ollama:11434）自动选择 200-成功 或 503-不可用 契约
