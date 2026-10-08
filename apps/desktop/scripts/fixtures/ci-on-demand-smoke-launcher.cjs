#!/usr/bin/env node
'use strict'

// M2 (review finding, 2026-10-05): a test-only stand-in for the real
// fork's `app/prebundled/agent24-headless.cjs`. It exists because the
// real one cannot be used here yet: Agent24's own headless config now
// speaks protocol v2 (explicit resourceSafeBase — see creative-serve-
// web.ts and PR iDoris-ai/open-design-agent24#7), but the officially
// pinned Open Design commit (OPEN_DESIGN_SHA in release-desktop.yml)
// still only understands protocol v1 — the pin is deliberately NOT
// bumped yet ("pin 暂不改，等 fork PR 合并后由我统一升 pin"). Running the
// REAL pinned agent24-headless.cjs against a v2 config would just fail
// on a protocol mismatch, telling us nothing about Agent24's own code.
//
// This script mirrors the v2 contract's validation rules exactly (see
// apps/packaged/src/agent24-headless.ts's assertAgent24ResourceRoot in
// the fork) and, once satisfied, serves a real local HTTP /api/ready —
// so what this CI step actually proves is Agent24's own side of the
// integration end to end: the installer's real install path, the real
// spawn args/cwd/env (including ELECTRON_RUN_AS_NODE), the real stdout
// ready-line contract, and the real /api/ready poll — all under the
// real packaged Electron binary. It does NOT prove the fork's own
// protocol v2 implementation; that is the fork PR's own test suite's
// job (apps/packaged/tests/agent24-headless.test.ts, already green).
//
// Once the fork PR merges and OPEN_DESIGN_SHA is re-pinned to a commit
// that ships real v2 support, this fixture (and the CI step that wires
// it in in place of the real tarball's launcher) should be deleted and
// the ordinary production tarball's own agent24-headless.cjs used
// instead — at that point this file no longer carries its own weight.

const fs = require('node:fs')
const http = require('node:http')
const path = require('node:path')
const crypto = require('node:crypto')

function fail(message) {
  process.stderr.write(`ci-on-demand-smoke-launcher: ${message}\n`)
  process.exit(1)
}

const configIndex = process.argv.indexOf('--config')
if (configIndex === -1 || !process.argv[configIndex + 1]) fail('missing --config <path>')
const configPath = process.argv[configIndex + 1]

let config
try {
  config = JSON.parse(fs.readFileSync(configPath, 'utf8'))
} catch (error) {
  fail(`unreadable config at ${configPath}: ${error instanceof Error ? error.message : String(error)}`)
}

if (config.protocol !== 2) fail(`protocol mismatch: expected 2, got ${String(config.protocol)}`)
if (typeof config.resourceRoot !== 'string' || !path.isAbsolute(config.resourceRoot)) {
  fail('resourceRoot must be an absolute path')
}
if (typeof config.resourceSafeBase !== 'string' || !path.isAbsolute(config.resourceSafeBase)) {
  fail('resourceSafeBase must be an absolute path (this launcher only exercises the v2 explicit-base path)')
}

let realSafeBase
try {
  realSafeBase = fs.realpathSync(config.resourceSafeBase)
} catch (error) {
  fail(`resourceSafeBase does not exist: ${error instanceof Error ? error.message : String(error)}`)
}
if (realSafeBase !== config.resourceSafeBase) fail('resourceSafeBase must be its own realpath (no symlink indirection)')

const relativeToSafeBase = path.relative(realSafeBase, config.resourceRoot)
if (relativeToSafeBase.startsWith('..') || path.isAbsolute(relativeToSafeBase)) {
  fail('resourceRoot must be under resourceSafeBase')
}

if (typeof process.getuid === 'function') {
  const info = fs.statSync(realSafeBase)
  if (info.uid !== process.getuid()) fail('resourceSafeBase must be owned by the current user')
  if ((info.mode & 0o022) !== 0) fail('resourceSafeBase must not be group- or other-writable')
}

const webServer = http.createServer((req, res) => {
  if (req.url === '/api/ready') {
    res.writeHead(200, { 'content-type': 'application/json' })
    res.end(JSON.stringify({ ready: true }))
    return
  }
  res.writeHead(404)
  res.end()
})
const daemonServer = http.createServer((_req, res) => {
  res.writeHead(200)
  res.end()
})

webServer.listen(0, '127.0.0.1', () => {
  daemonServer.listen(0, '127.0.0.1', () => {
    const webPort = webServer.address().port
    const daemonPort = daemonServer.address().port
    process.stdout.write(`${JSON.stringify({
      type: 'ready',
      protocol: 2,
      instanceId: crypto.randomUUID(),
      pinVersion: config.pinVersion,
      webOrigin: `http://127.0.0.1:${webPort}`,
      daemonOrigin: `http://127.0.0.1:${daemonPort}`,
      ownership: { ownerPid: process.pid, kind: 'process-tree' },
    })}\n`)
  })
})

process.on('SIGTERM', () => process.exit(0))
process.on('SIGINT', () => process.exit(0))
