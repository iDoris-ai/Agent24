# COMM-5b 零运行回归门禁

权威判据为 `docs/design/COMM-HYPHAE.md` §7/§8。结构门禁和 T2 随
`cargo test --workspace` 执行；T3 需要真实 Hyphae 和 Go，必须额外显式执行。
原有联调提交已带入旧测试，本任务补强其检测能力与执行入口，不改生产行为。

## 结构门禁与 T2

在 `rust/` 执行：

```sh
ulimit -n 4096
export CARGO_TARGET_DIR=$HOME/Dev/auraai/Agent24/rust/target
cargo test -p agent24-comm --test comm_dependency_allowlist --test comm_zero_run -- --nocapture
```

依赖门禁遍历 Cargo metadata 的全部 normal 边（包括传递边与各平台依赖），
用同一检查器证明 agent24d 会被 `agent24-agent` 拒绝。HTTP feature 检查用
`cargo tree -p agent24-comm -e normal --target all --format '{p}|{f}'`，避免
workspace feature 合并的误报，也能捕捉隐式开启的 hyper/client。
合成图包含传递禁止依赖、dev/build 排除、循环，以及 client/client-legacy 变异。
状态门禁解析 Rust AST，固定 CommState 与 Backend 全部字段的类型，新增执行句柄、
别名、回调或嵌套 Backend 字段都会失败。

测试内的 `synthetic_transitive_mutations_reject_every_forbidden_package_and_feature`
逐项注入禁止依赖及 HTTP client feature，并断言同一检查器给出准确的拒绝项；
`synthetic_state_mutations_reject_execution_handles_aliases_and_callbacks`
对 CommState 和 Backend 分别注入六种执行句柄/回调类型，断言结构匹配失败。
实际 agent24d 的 normal 图必须命中 `agent24-agent`，否则正对照失败。

本机另用 Cargo 输出代理向真实 metadata 增加 reqwest normal 边，或向真实
feature tree 增加 `hyper-util v0.1|client,http1`，运行相同已编译依赖门禁。
两次均 exit 101，分别报告以下错误；恢复真实 Cargo 后 4 项结构测试全部通过：

```text
COMM normal dependency boundary violated: {"reqwest"}
COMM normal dependency boundary violated: {"hyper-util/client"}
```

这些是测试检查器的变异反证，未修改生产依赖或行为。

T2 在独立测试子进程设置 OLLAMA_URL、OPENAI_BASE_URL、A24_BASE_URL、OMLX_URL，
均指向同一个本地 Python HTTP 桩，不修改测试宿主的环境。
六类消息在 history/outbox 原样返回，假 daemon 每 100ms 写日志；实际启动、
inbox pull、20 次各读接口、停止均完成后，桩的业务请求总数必须为零。
模型、模块和未知健康路径的请求正对照证明计数器能捕捉全部请求；只有测试自己
读取计数的 `/counts` 免计数。缺少 python3 直接失败。

## T3：真实进程、正对照与六类真实对端消息

准备一个独立且无 tracked 修改的 Hyphae checkout，HEAD 必须等于
`rust/crates/agent24-comm/hyphae.lock.json` 的 source_sha；不要修改日常 checkout。
按 lock 的 Go 版本和 recipe 构建当前平台二进制。支持 lock 记录的
macOS arm64 与 Linux x64：recipe 的 GOOS/GOARCH 分别取 darwin/arm64 或 linux/amd64。

```sh
# 在 lock-pinned Hyphae checkout；下例为 macOS arm64
GOTOOLCHAIN=go1.26.4 CGO_ENABLED=0 GOOS=darwin GOARCH=arm64 \
  go build -trimpath -buildvcs=false -ldflags=-buildid= -o hyphae ./cmd/hyphae
# 回到 Agent24 的 rust/；路径须为实际绝对路径
HYPHAE_SOURCE_DIR=/absolute/path/to/locked-hyphae \
A24_HYPHAE_BIN=/absolute/path/to/locked-hyphae/hyphae \
  cargo test --locked -p agent24d --test comm_blackbox -- --ignored --nocapture
```

T3 校验源码 SHA、query/response fixture hash 和二进制 hash，构建本地 relay，
启动真实 agent24d、受管 Hyphae 和独立 peer。所有 HOME、relay 存储和状态均隔离。
正对照通过 sessions/runs 真正执行一条 run 并调用模型及 MCP 模块：runs Δ=1，
模型与模块计数均增加且 run 必须 completed。随后以实际发布的 event id 和
原样正文确认六类消息全部入站，读取各接口 20 次后 runs Δ=0、模型/模块请求 Δ=0，
停止与关机也不得增加计数。计数桩只排除没有任何 HTTP 字节的 TCP 连接和测试自身的
`/__ready`、`/counts` 请求，未知 HTTP 路径仍计数；普通 workspace 内的
`counter_counts_http_requests_but_not_empty_tcp_connections` 为此提供独立回归。
修复前该测试 exit 101（空连接错误计为 `(1, 0)`），修复仅为测试桩跳过空 header。
成功时打印 `COMM-5b T3 PASS` 和实测计数。

`.github/workflows/hyphae-lock-verify.yml` 对 main/ab/comm 的 Rust 变动运行此显式命令，
使用 lock-built Linux x64 二进制；workspace 测试中的 ignored 不等于 T3 通过。
缺环境变量、缺 Python/Go、源码/hash 不符、正对照为零、消息未收齐或命令超时均失败。

## 本机 T3 实测（2026-10-09）

macOS arm64，Hyphae `671c584f9e9eb807a15968e2aa42fd7507e178b8`，
Go `go1.26.4`，二进制 SHA256 为
`d1171421e91ae62c40374bd00049cd51dd6ac1135b6cd7b31908968b9158df60`。
在 Agent24 仓库根目录执行（先设置上文的 ulimit 与共享 target）：

```sh
HYPHAE_SOURCE_DIR=/tmp/comm5b-hyphae-ctx245b71b \
A24_HYPHAE_BIN=/tmp/comm5b-hyphae-ctx245b71b/hyphae \
  cargo test --locked --manifest-path rust/Cargo.toml -p agent24d --test comm_blackbox -- --include-ignored --nocapture
```

结果：2 passed / 0 failed / 0 ignored（包含桩回归与显式 T3），exit 0。
实际输出：

```text
COMM-5b T3 PASS: positive runs delta=1, models=2, modules=1; six real peer events; passive runs delta=0, model/module delta=0
```

本机结果证明 darwin-arm64 场景；linux-x64 使用上文 CI 显式入口运行，
不能由本机结果或 workspace 内的 ignored 状态代替。
