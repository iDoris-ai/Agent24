# node-ref — Agent24 out-of-process module reference implementation (Node.js)

This is a minimal, working Agent24 domain-OS module written **only** against
[`docs/specs/WIRE-OOP-MODULE.md`](../../docs/specs/WIRE-OOP-MODULE.md). It does not
`require`/`import` anything from this repository's Rust protocol crates
(`agent24-os-proto`, `agent24-os-sdk`) — that is the point of it: it is proof that
the wire document alone is enough for a third party, in a different language, to
build a working module.

Node.js standard library only. Zero npm dependencies (see `package.json`).

## What it does

1. Reads the four `A24_*` environment variables the kernel launches it with.
2. Takes over inherited fd 3 as an HTTP server for its own namespace
   (`/api/v1/node-ref/*`).
3. Connects to the kernel's callback Unix-domain-socket and completes the
   `initialize` handshake, requesting the `events` and `memory` capabilities.
4. Right after the handshake, emits one `_a24/events/emit` call — proving a
   background callback (not tied to any HTTP request) works.
5. On `GET /api/v1/node-ref/hello` (reached through the kernel's proxy), calls
   `_a24/memory/private/remember`, then `_a24/memory/private/recall`, then
   `_a24/events/emit` again, and returns a small JSON summary. It also writes
   an atomic probe file (`callback_probe.json`) into its data directory so an
   external test can inspect exactly what it did without race conditions.
6. Exits when it reads EOF on the callback socket (the kernel closes it when
   this generation ends — see WIRE-OOP-MODULE.md §7).

## Running it standalone (without a kernel)

You can't — this module is not runnable stand-alone: it requires the four
`A24_*` environment variables and an already-listening fd 3, both of which only
`agent24d` (or a hand-rolled test harness, see
`rust/apps/agent24d/tests/me4_node_module_blackbox.rs`) can provide.

## Installing it under a real `agent24d`

```bash
mkdir -p ~/.agent24/packages/node-ref
cp -r examples/node-module/* ~/.agent24/packages/node-ref/
# restart the daemon — packages are discovered from disk at startup
agent24 os list
# → node-ref   mounted
curl -H "authorization: Bearer <daemon token>" http://127.0.0.1:<port>/api/v1/node-ref/hello
```

(`agent24 os install <dir>` performs the same copy with the correct
permissions — see SPEC-ME3-OUT-OF-PROCESS.md §1's package-tree ownership
requirements.)

## Files

- `domain-os.yml` — the manifest. `impl_kind: out_of_process_provider`,
  `spawn: {command: node, args: ["index.js"]}`.
- `index.js` — the whole module: NDJSON framing, the `initialize` handshake, a
  tiny JSON-RPC client over the callback socket, and an HTTP handler on fd 3.
- `package.json` — no dependencies, `"private": true` (never published).
