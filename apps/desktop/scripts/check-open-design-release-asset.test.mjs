import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import fs from 'node:fs'
import http from 'node:http'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { fileURLToPath } from 'node:url'

const script = fileURLToPath(new URL('./check-open-design-release-asset.sh', import.meta.url))

function writeManifest(dir, url, size) {
  const file = path.join(dir, 'open-design-component.json')
  fs.writeFileSync(file, `${JSON.stringify({
    url,
    sha256: 'a'.repeat(64),
    size,
    version: '0.22.2',
    odSha: 'b'.repeat(40),
    platform: 'darwin',
    arch: 'arm64',
  })}\n`)
  return file
}

/** Async spawn: `spawnSync` would freeze this process's HTTP server, and curl would wait forever. */
function run(manifest) {
  return new Promise((resolve, reject) => {
    const child = spawn('bash', [script, manifest])
    let stdout = ''
    let stderr = ''
    child.stdout.setEncoding('utf8')
    child.stderr.setEncoding('utf8')
    child.stdout.on('data', (chunk) => { stdout += chunk })
    child.stderr.on('data', (chunk) => { stderr += chunk })
    child.on('error', reject)
    child.on('close', (status) => resolve({ status, stdout, stderr }))
  })
}

test('check-open-design-release-asset accepts a reachable URL whose Content-Length matches', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-asset-'))
  const server = http.createServer((req, res) => {
    const body = Buffer.from('hello-asset')
    res.writeHead(req.method === 'HEAD' || req.method === 'GET' ? 200 : 405, {
      'Content-Length': String(body.length),
      'Content-Type': 'application/octet-stream',
    })
    res.end(req.method === 'HEAD' ? undefined : body)
  })
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
  try {
    const { port } = server.address()
    const manifest = writeManifest(dir, `http://127.0.0.1:${port}/open-design.tar.gz`, 11)
    const result = await run(manifest)
    assert.equal(result.status, 0, result.stderr)
    assert.match(result.stdout, /Content-Length 11 matches/)
  } finally {
    server.close()
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

test('check-open-design-release-asset fails when Content-Length disagrees with the manifest', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-asset-bad-'))
  const server = http.createServer((_req, res) => {
    res.writeHead(200, { 'Content-Length': '4' })
    res.end('nope')
  })
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
  try {
    const { port } = server.address()
    const manifest = writeManifest(dir, `http://127.0.0.1:${port}/open-design.tar.gz`, 99)
    const result = await run(manifest)
    assert.notEqual(result.status, 0)
    assert.match(result.stderr, /Content-Length 4 does not match manifest size 99/)
  } finally {
    server.close()
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

test('check-open-design-release-asset fails when the URL is not reachable', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-asset-down-'))
  const server = http.createServer((_req, res) => {
    res.writeHead(404)
    res.end()
  })
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
  try {
    const { port } = server.address()
    const manifest = writeManifest(dir, `http://127.0.0.1:${port}/missing.tar.gz`, 11)
    const result = await run(manifest)
    assert.notEqual(result.status, 0)
    assert.match(result.stderr, /not reachable/)
  } finally {
    server.close()
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

test('check-open-design-release-asset skips the size check when Content-Length is absent', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-asset-nolen-'))
  const server = net.createServer((socket) => {
    socket.end('HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n')
  })
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
  try {
    const { port } = server.address()
    const manifest = writeManifest(dir, `http://127.0.0.1:${port}/open-design.tar.gz`, 99)
    const result = await run(manifest)
    assert.equal(result.status, 0, `${result.stderr}\n${result.stdout}`)
    assert.match(result.stdout, /no Content-Length/)
  } finally {
    server.close()
    fs.rmSync(dir, { recursive: true, force: true })
  }
})
