# node-ref — Agent24 attached module reference implementation (Node.js)

This is a minimal, working Agent24 domain-OS module written **only** against
[`docs/specs/WIRE-OOP-MODULE.md`](../../docs/specs/WIRE-OOP-MODULE.md) and
[`docs/design/A3-ATTACHED-MODULE.md`](../../docs/design/A3-ATTACHED-MODULE.md). It does not
`require`/`import` anything from this repository's Rust protocol crates
(`agent24-os-proto`, `agent24-os-sdk`) — that is the point of it: it is proof that
the wire document alone is enough for a third party, in a different language, to
build a working module.

Node.js standard library only. Zero npm dependencies (see `package.json`).

> **安装包策略与附着模块**：
> Agent24 安装的 OS 包（`out_of_process_provider`）必须是原生可执行程序；签名按 S1-9 §5 平台策略（Linux/Windows v1 无代码签名），完整性清单拒绝脚本与裸解释器命令。使用解释器（`command: node`）启动的模块不能作为安装包；开发与参考实现改走「附着模块」（A3 `attached_process`），以独立进程连接常驻套接字并完成线协议交互。

## What it does

1. Reads the attach socket path and auth token from environment variables (`A24_ATTACH_SOCK`, `A24_ATTACH_TOKEN`) or explicit CLI arguments (`node index.js <socket> <token> <data_dir>`). CLI arguments take precedence over environment variables.
2. Connects to the kernel's resident attach Unix-domain-socket (`~/.agent24/attach/agent24d.sock`) and completes the `initialize` handshake, requesting the `events` capability.
3. Right after the handshake, emits one `_a24/events/emit` call (`task.transitioned`) — proving a callback round trip works over the wire.
4. If a data directory is explicitly configured (via argument or `A24_DATA_DIR`), writes an atomic probe file (`startup_probe.json`) with the received handshake offer.
5. Keeps running to answer reverse commands (`echo`) if dispatched.
6. Exits when it reads EOF on the attached socket (the kernel closes it when this generation ends).

## Running it under a real `agent24d`

> **安全说明**：避免通过命令行参数传递 auth token，因为命令行参数会被系统进程表（`ps`/`/proc`）暴露给同机其他非特权进程。推荐将 token 从安全来源读入受限环境变量。

```bash
# 1. 注册附着模块（返回 JSON，含 socket_path 与 token，将 token 保存至仅所有者可读文件）
agent24 os attach add examples/node-module/domain-os.yml --json

# 2. 从安全凭据源读入环境变量并启动 Node 模块
export A24_ATTACH_SOCK="/path/to/socket"
export A24_ATTACH_TOKEN="$(cat token-file)"
export A24_DATA_DIR="$HOME/.agent24/os/node-ref"
node examples/node-module/index.js
```

## Files

- `domain-os.yml` — the manifest. `impl_kind: attached_process`, `kernel_capabilities: [events]`.
- `index.js` — the whole module: NDJSON framing, the `initialize` handshake, and a tiny JSON-RPC client over the attached socket.
- `package.json` — no dependencies, `"private": true` (never published).
