#!/usr/bin/env node
'use strict';

/**
 * Agent24 out-of-process domain OS — minimal Node.js reference module.
 *
 * This file was written ONLY against docs/specs/WIRE-OOP-MODULE.md — it does
 * NOT import, require, or read anything from this repository's Rust protocol
 * crates (agent24-os-proto / agent24-os-sdk). That is the point of it: it is
 * the "does the document alone let a third party in another language build a
 * working module" proof for ME4-5.4.1 / T14.
 *
 * Node.js standard library only. No npm dependencies.
 *
 * What it does, end to end:
 *   1. Reads the A24_* environment variables the kernel launches it with.
 *   2. Takes over fd 3 (the pre-bound, listening Unix-domain-socket the
 *      kernel handed it) as an HTTP server for its own namespace's routes.
 *   3. Connects to the callback Unix-domain-socket named by
 *      A24_CALLBACK_SOCK and completes the `initialize` handshake.
 *   4. Emits one `task.transitioned` event right after the handshake.
 *   5. On the first HTTP request that reaches it through the kernel's proxy,
 *      calls `_a24/memory/private/remember` and then
 *      `_a24/memory/private/recall`, and serves a small JSON status page
 *      showing what it just did — this is what the black-box test
 *      (rust/apps/agent24d/tests/me4_node_module_blackbox.rs) reads back.
 *   6. Exits promptly when it reads EOF on the callback socket (SPEC-ME3
 *      §3's D1: "the callback connection is this generation's lifeline").
 */

const net = require('node:net');
const http = require('node:http');
const fs = require('node:fs');
const path = require('node:path');

// ---------------------------------------------------------------------------
// 1. Environment contract (WIRE-OOP-MODULE.md §2 "Launch environment")
// ---------------------------------------------------------------------------

function requiredEnv(name) {
  const value = process.env[name];
  if (!value) {
    process.stderr.write(`node-ref: missing required env var ${name}\n`);
    process.exit(1);
  }
  return value;
}

const LISTEN_FD = Number(requiredEnv('A24_LISTEN_FD'));
const CALLBACK_SOCK = requiredEnv('A24_CALLBACK_SOCK');
const HANDSHAKE_TOKEN = requiredEnv('A24_HANDSHAKE_TOKEN');
const DATA_DIR = requiredEnv('A24_DATA_DIR');

const MODULE_NAME = 'node-ref';
const MANIFEST_PATH = path.join(__dirname, 'domain-os.yml');

function manifestDigest() {
  const crypto = require('node:crypto');
  const bytes = fs.readFileSync(MANIFEST_PATH);
  return `sha256:${crypto.createHash('sha256').update(bytes).digest('hex')}`;
}

function probePath(name) {
  return path.join(DATA_DIR, name);
}

/** Write-to-temp + rename: atomic on the same filesystem (POSIX rename(2)),
 * so a concurrent reader (the black-box test) never observes a truncated or
 * partially written file — same technique the Rust ME-3f blackbox module
 * uses (rust/apps/agent24d/tests/me3f_blackbox.rs `dump_atomic`). */
function writeProbeAtomic(name, value) {
  fs.mkdirSync(DATA_DIR, { recursive: true });
  const target = probePath(name);
  const tmp = `${target}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(value));
  fs.renameSync(tmp, target);
}

// ---------------------------------------------------------------------------
// 2. NDJSON framing + a tiny JSON-RPC client over the callback socket
//    (WIRE-OOP-MODULE.md §4 "Frame format & RPC")
// ---------------------------------------------------------------------------

class CallbackChannel {
  constructor(socket) {
    this.socket = socket;
    this.buffer = '';
    this.nextId = 1;
    this.pending = new Map(); // id -> {resolve, reject}
    this.closed = false;

    socket.on('data', (chunk) => this._onData(chunk));
    socket.on('close', () => this._onClose());
    socket.on('error', () => this._onClose());
  }

  _onData(chunk) {
    this.buffer += chunk.toString('utf8');
    let idx;
    // NDJSON: one JSON value per line, newline-terminated.
    while ((idx = this.buffer.indexOf('\n')) !== -1) {
      const line = this.buffer.slice(0, idx);
      this.buffer = this.buffer.slice(idx + 1);
      if (line.length === 0) continue;
      this._onLine(line);
    }
  }

  _onLine(line) {
    let msg;
    try {
      msg = JSON.parse(line);
    } catch (e) {
      process.stderr.write(`node-ref: unparseable line from kernel: ${e}\n`);
      return;
    }
    const id = msg.id;
    const waiter = id === null || id === undefined ? null : this.pending.get(id);
    if (!waiter) {
      // Not a response we're waiting on (e.g. a kernel-initiated request
      // such as a fired scheduler delivery arrives on the HTTP side, not
      // here — so in this minimal module there should be none, but a
      // stray/duplicate response must not crash the process).
      return;
    }
    this.pending.delete(id);
    if (Object.prototype.hasOwnProperty.call(msg, 'error')) {
      waiter.reject(new CallbackError(msg.error));
    } else {
      waiter.resolve(msg.result);
    }
  }

  _onClose() {
    if (this.closed) return;
    this.closed = true;
    for (const waiter of this.pending.values()) {
      waiter.reject(new Error('callback socket closed'));
    }
    this.pending.clear();
    // SPEC-ME3 §3 "允许的连接数" (D1): the callback connection is this
    // generation's lifeline. Reading EOF on it means the kernel is done
    // with this run — the module MUST exit, not attempt to reconnect (the
    // same generation is never allowed a second connection, and a
    // reconnecting module would risk racing a brand-new daemon instance
    // over the same data directory). Exit code 0: this is expected,
    // graceful shutdown, not a crash.
    process.stderr.write('node-ref: callback socket closed by kernel, exiting\n');
    process.exit(0);
  }

  /** Send one JSON-RPC request, return a Promise of its `result`. */
  call(method, params) {
    if (this.closed) return Promise.reject(new Error('callback socket closed'));
    const id = String(this.nextId++);
    const frame = JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n';
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.socket.write(frame);
    });
  }
}

class CallbackError extends Error {
  constructor(errorObject) {
    super(errorObject.message || 'callback error');
    this.code = errorObject.code;
    this.kind = errorObject.data && errorObject.data.kind;
    this.data = errorObject.data;
  }
}

// ---------------------------------------------------------------------------
// 3. `initialize` handshake (WIRE-OOP-MODULE.md §3)
// ---------------------------------------------------------------------------

function connectAndHandshake() {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection(CALLBACK_SOCK, () => {
      const channel = new CallbackChannel(socket);
      // The handshake IS the first call on the channel: it is sent the same
      // way as any other RPC (CallbackChannel.call takes care of framing
      // and id bookkeeping), the kernel just requires it to be the first
      // line on the wire.
      channel
        .call('initialize', {
          protocol_versions: { min: 1, max: 1 },
          module: MODULE_NAME,
          manifest_digest: manifestDigest(),
          auth_token: HANDSHAKE_TOKEN,
          capabilities: ['events', 'memory'],
        })
        .then((result) => resolve({ channel, result }))
        .catch(reject);
    });
    socket.on('error', reject);
  });
}

// ---------------------------------------------------------------------------
// 4. HTTP server on the inherited fd 3 (WIRE-OOP-MODULE.md §2)
// ---------------------------------------------------------------------------

function startHttpServer(channel) {
  const server = http.createServer((req, res) => {
    handleRequest(channel, req, res).catch((err) => {
      process.stderr.write(`node-ref: request handler failed: ${err}\n`);
      if (!res.headersSent) {
        res.writeHead(500, { 'content-type': 'application/json' });
      }
      res.end(JSON.stringify({ error: String(err && err.message ? err.message : err) }));
    });
  });
  // Node's net/http servers accept an already-bound, already-listening fd
  // via `listen({ fd })` — no `fd.open`/`socket()` call needed, the kernel
  // did that part; this just starts accepting connections on it.
  server.listen({ fd: LISTEN_FD }, () => {
    process.stderr.write('node-ref: serving on inherited fd 3\n');
  });
  server.on('error', (err) => {
    process.stderr.write(`node-ref: http server error: ${err}\n`);
  });
  return server;
}

async function handleRequest(channel, req, res) {
  // The path the kernel forwards is the FULL path including the
  // `/api/v1/<ns>` prefix (WIRE-OOP-MODULE.md §6) — this module's own
  // router must match against that, not a stripped path.
  const url = new URL(req.url, 'http://node-ref.invalid');

  if (url.pathname === '/api/v1/node-ref/hello') {
    const remembered = await channel.call('_a24/memory/private/remember', {
      kind: 'node-ref-note',
      body: { text: 'hello from the node reference module', at: new Date().toISOString() },
    });
    const recalled = await channel.call('_a24/memory/private/recall', {
      query: 'node-ref-note',
      page_size: 10,
    });
    await channel.call('_a24/events/emit', {
      kind: 'task.transitioned',
      payload: { probe: 'node-ref', path: url.pathname },
    });
    writeProbeAtomic('callback_probe.json', {
      remembered,
      recalled,
    });
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ ok: true, remembered, recalled }));
    return;
  }

  res.writeHead(404, { 'content-type': 'application/json' });
  res.end(JSON.stringify({ error: 'not_found', path: url.pathname }));
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

async function main() {
  const { channel, result } = await connectAndHandshake();
  process.stderr.write(`node-ref: handshake ok, protocol_version=${result.protocol_version}, offer=${JSON.stringify(result.offer)}\n`);

  // One real callback round trip right after the handshake, independent of
  // any HTTP request — proves the module can use its connection-authority
  // credential for background work, not only inside a proxied request
  // (SPEC-ME3 §3 "后台任务只能写自己的私有分区").
  const emitResult = await channel.call('_a24/events/emit', {
    kind: 'task.transitioned',
    payload: { probe: 'node-ref', phase: 'startup' },
  });
  writeProbeAtomic('startup_probe.json', { offer: result.offer, emit: emitResult });

  startHttpServer(channel);
}

main().catch((err) => {
  try {
    fs.mkdirSync(DATA_DIR, { recursive: true });
    fs.writeFileSync(path.join(DATA_DIR, 'error.txt'), String(err && err.stack ? err.stack : err));
  } catch {
    // best effort
  }
  process.stderr.write(`node-ref: fatal: ${err && err.stack ? err.stack : err}\n`);
  process.exit(1);
});
