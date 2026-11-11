#!/usr/bin/env node
'use strict';

/**
 * Agent24 attached domain OS — minimal Node.js reference module.
 *
 * This file was written ONLY against docs/specs/WIRE-OOP-MODULE.md and
 * docs/design/A3-ATTACHED-MODULE.md — it does NOT import, require, or read
 * anything from this repository's Rust protocol crates (agent24-os-proto /
 * agent24-os-sdk).
 *
 * Node.js standard library only. No npm dependencies.
 *
 * What it does, end to end:
 *   1. Reads socket path and auth token from environment variables or CLI arguments.
 *   2. Connects to the attached Unix-domain-socket and completes the `initialize`
 *      handshake requesting the `events` capability.
 *   3. Emits one `task.transitioned` event right after the handshake.
 *   4. Writes `startup_probe.json` snapshot atomically to its data directory.
 *   5. Keeps running and answers reverse commands if invoked.
 *   6. Exits promptly and gracefully when it reads EOF on the attached socket.
 */

const net = require('node:net');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { StringDecoder } = require('node:string_decoder');

// ---------------------------------------------------------------------------
// 1. Environment & arguments contract
// ---------------------------------------------------------------------------

function resolveArgOrEnv(argIdx, envName) {
  if (process.argv[argIdx]) return process.argv[argIdx];
  if (envName && process.env[envName]) return process.env[envName];
  return undefined;
}

const CALLBACK_SOCK = resolveArgOrEnv(2, 'A24_ATTACH_SOCK');
if (!CALLBACK_SOCK) {
  process.stderr.write('node-ref: missing required argument or env var (A24_ATTACH_SOCK)\n');
  process.exit(1);
}

const HANDSHAKE_TOKEN = resolveArgOrEnv(3, 'A24_ATTACH_TOKEN');
if (!HANDSHAKE_TOKEN) {
  process.stderr.write('node-ref: missing required argument or env var (A24_ATTACH_TOKEN)\n');
  process.exit(1);
}

// 探针目录必须显式给（参数或 A24_DATA_DIR）；没给就不落盘（不写 ~/.agent24），连接失败也不写 error.txt 到默认目录
const DATA_DIR = resolveArgOrEnv(4, 'A24_DATA_DIR') || null;

const MODULE_NAME = 'node-ref';
const MANIFEST_PATH = path.join(__dirname, 'domain-os.yml');

function manifestDigest() {
  const bytes = fs.readFileSync(MANIFEST_PATH);
  return `sha256:${crypto.createHash('sha256').update(bytes).digest('hex')}`;
}

function probePath(name) {
  return DATA_DIR ? path.join(DATA_DIR, name) : null;
}

/** Write-to-temp + rename: atomic on the same filesystem (POSIX rename(2)),
 * so a concurrent reader (the black-box test) never observes a truncated or
 * partially written file. */
function writeProbeAtomic(name, value) {
  if (!DATA_DIR) return;
  fs.mkdirSync(DATA_DIR, { recursive: true });
  const target = probePath(name);
  const tmp = `${target}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(value));
  fs.renameSync(tmp, target);
}

// ---------------------------------------------------------------------------
// 2. NDJSON framing + a tiny JSON-RPC client over the attached socket
//    (A3-ATTACHED-MODULE.md §4 "Frame format & RPC")
// ---------------------------------------------------------------------------

class CallbackChannel {
  constructor(socket) {
    this.socket = socket;
    this.buffer = '';
    // A multi-byte UTF-8 character can be split across two socket chunks;
    // the decoder holds the partial bytes until the rest arrive.
    this.decoder = new StringDecoder('utf8');
    this.nextId = 1;
    this.pending = new Map(); // id -> {resolve, reject}
    this.closed = false;

    socket.on('data', (chunk) => this._onData(chunk));
    socket.on('close', () => this._onClose());
    socket.on('error', () => this._onClose());
  }

  _onData(chunk) {
    this.buffer += this.decoder.write(chunk);
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
      // Check if this is a kernel-originated request (has method and string id)
      if (msg.method && id !== null && id !== undefined) {
        this._handleKernelRequest(msg);
      }
      return;
    }
    this.pending.delete(id);
    if (Object.prototype.hasOwnProperty.call(msg, 'error')) {
      waiter.reject(new CallbackError(msg.error));
    } else {
      waiter.resolve(msg.result);
    }
  }

  _handleKernelRequest(msg) {
    if (msg.method === '_a24/command/invoke') {
      const { name, body } = msg.params || {};
      if (name !== 'echo') {
        const frame =
          JSON.stringify({
            jsonrpc: '2.0',
            id: msg.id,
            error: { code: -32602, message: `Unknown command: ${name}` },
          }) + '\n';
        this.socket.write(frame);
        return;
      }
      if (!body || typeof body !== 'object' || typeof body.message !== 'string') {
        const frame =
          JSON.stringify({
            jsonrpc: '2.0',
            id: msg.id,
            error: { code: -32602, message: 'Invalid params: body.message (string) is required' },
          }) + '\n';
        this.socket.write(frame);
        return;
      }
      const frame =
        JSON.stringify({
          jsonrpc: '2.0',
          id: msg.id,
          result: { echo: body.message },
        }) + '\n';
      this.socket.write(frame);
      return;
    }
    const frame =
      JSON.stringify({
        jsonrpc: '2.0',
        id: msg.id,
        error: { code: -32601, message: 'Method not found' },
      }) + '\n';
    this.socket.write(frame);
  }

  _onClose() {
    if (this.closed) return;
    this.closed = true;
    for (const waiter of this.pending.values()) {
      waiter.reject(new Error('attached socket closed'));
    }
    this.pending.clear();
    process.stderr.write('node-ref: attached socket closed by kernel, exiting\n');
    process.exit(0);
  }

  /** Send one JSON-RPC request, return a Promise of its `result`. */
  call(method, params) {
    if (this.closed) return Promise.reject(new Error('attached socket closed'));
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
    const kindPrefix =
      errorObject && errorObject.data && errorObject.data.kind
        ? `[${errorObject.data.kind}] `
        : '';
    super(`${kindPrefix}${(errorObject && errorObject.message) || 'callback error'}`);
    this.code = errorObject && errorObject.code;
    this.kind = errorObject && errorObject.data && errorObject.data.kind;
    this.data = errorObject && errorObject.data;
  }
}

// ---------------------------------------------------------------------------
// 3. `initialize` handshake (A3-ATTACHED-MODULE.md §4.3)
// ---------------------------------------------------------------------------

function connectAndHandshake() {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection(CALLBACK_SOCK, () => {
      const channel = new CallbackChannel(socket);
      channel
        .call('initialize', {
          protocol_versions: { min: 1, max: 1 },
          module: MODULE_NAME,
          manifest_digest: manifestDigest(),
          auth_token: HANDSHAKE_TOKEN,
          capabilities: ['events'],
        })
        .then((result) => resolve({ channel, result }))
        .catch(reject);
    });
    socket.on('error', reject);
  });
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

async function main() {
  const { channel, result } = await connectAndHandshake();
  process.stderr.write(
    `node-ref: handshake ok, protocol_version=${result.protocol_version}, offer=${JSON.stringify(result.offer)}\n`
  );

  // Emit one real callback round trip right after the handshake
  const emitResult = await channel.call('_a24/events/emit', {
    kind: 'task.transitioned',
    payload: { probe: 'node-ref', phase: 'startup' },
  });
  writeProbeAtomic('startup_probe.json', { offer: result.offer, emit: emitResult });
}

main().catch((err) => {
  if (DATA_DIR) {
    try {
      fs.mkdirSync(DATA_DIR, { recursive: true });
      fs.writeFileSync(
        path.join(DATA_DIR, 'error.txt'),
        String(err && err.stack ? err.stack : err)
      );
    } catch {
      // best effort
    }
  }
  process.stderr.write(`node-ref: fatal: ${err && err.stack ? err.stack : err}\n`);
  process.exit(1);
});
