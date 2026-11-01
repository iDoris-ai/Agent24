import assert from 'node:assert/strict'
import crypto from 'node:crypto'
import { spawn } from 'node:child_process'
import fs from 'node:fs'
import http from 'node:http'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { fileURLToPath } from 'node:url'

const script = fileURLToPath(new URL('./check-open-design-release-asset.sh', import.meta.url))

function sha256(bytes) {
  return crypto.createHash('sha256').update(bytes).digest('hex')
}

function writeManifest(dir, url, { body = Buffer.alloc(0), size, digest } = {}) {
  const bytes = Buffer.isBuffer(body) ? body : Buffer.from(body)
  const file = path.join(dir, 'open-design-component.json')
  fs.writeFileSync(file, `${JSON.stringify({
    url,
    sha256: digest ?? sha256(bytes),
    size: size ?? bytes.length,
    version: '0.22.2',
    odSha: 'b'.repeat(40),
    platform: 'darwin',
    arch: 'arm64',
  })}\n`)
  return file
}

/** Async spawn: `spawnSync` would freeze this process's HTTP server, and curl would wait forever. */
function run(manifest, env = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn('bash', [script, manifest], { env: { ...process.env, ...env } })
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

test('check-open-design-release-asset downloads the body and verifies sha256 and size', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-asset-'))
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-asset-tmp-'))
  const body = Buffer.from('hello-asset')
  const server = http.createServer((req, res) => {
    res.writeHead(req.method === 'GET' ? 200 : 405, {
      'Content-Length': String(body.length),
      'Content-Type': 'application/octet-stream',
    })
    res.end(req.method === 'GET' ? body : undefined)
  })
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
  try {
    const { port } = server.address()
    const manifest = writeManifest(dir, `http://127.0.0.1:${port}/open-design.tar.gz`, { body })
    const result = await run(manifest, { TMPDIR: tmp })
    assert.equal(result.status, 0, result.stderr)
    assert.match(result.stdout, new RegExp(`downloaded size ${body.length} matches manifest, sha256 ${sha256(body)} verified`))
    assert.equal(fs.readdirSync(tmp).length, 0)
  } finally {
    server.close()
    fs.rmSync(dir, { recursive: true, force: true })
    fs.rmSync(tmp, { recursive: true, force: true })
  }
})

test('check-open-design-release-asset fails when the downloaded size disagrees with the manifest', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-asset-bad-'))
  const body = Buffer.from('nope')
  const server = http.createServer((_req, res) => {
    res.writeHead(200, { 'Content-Length': String(body.length) })
    res.end(body)
  })
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
  try {
    const { port } = server.address()
    const manifest = writeManifest(dir, `http://127.0.0.1:${port}/open-design.tar.gz`, { body, size: 99 })
    const result = await run(manifest)
    assert.notEqual(result.status, 0)
    assert.match(result.stderr, /downloaded size 4 does not match manifest size 99/)
  } finally {
    server.close()
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

test('check-open-design-release-asset fails when the downloaded sha256 disagrees with the manifest', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-asset-sha-'))
  const body = Buffer.from('hello-asset')
  const server = http.createServer((_req, res) => {
    res.end(body)
  })
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
  try {
    const { port } = server.address()
    const manifest = writeManifest(dir, `http://127.0.0.1:${port}/open-design.tar.gz`, { body, digest: 'a'.repeat(64) })
    const result = await run(manifest)
    assert.notEqual(result.status, 0)
    assert.match(result.stderr, /downloaded sha256 .* does not match manifest sha256 a{64}/)
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
    const manifest = writeManifest(dir, `http://127.0.0.1:${port}/missing.tar.gz`, { body: 'hello-asset' })
    const result = await run(manifest)
    assert.notEqual(result.status, 0)
    assert.match(result.stderr, /not reachable/)
  } finally {
    server.close()
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

test('check-open-design-release-asset rejects non-loopback http before downloading', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-asset-http-'))
  try {
    for (const url of ['http://example.com/open-design.tar.gz', 'http://127.0.0.1.evil.com/open-design.tar.gz']) {
      const manifest = writeManifest(dir, url, { body: 'hello-asset' })
      const result = await run(manifest)
      assert.notEqual(result.status, 0, url)
      assert.match(result.stderr, /must be https/, url)
    }
  } finally {
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

test('check-open-design-release-asset checks the body after a redirect and ignores the hop Content-Length', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-asset-redir-'))
  const body = Buffer.from('redirect-asset')
  const server = net.createServer((socket) => {
    let buf = ''
    socket.on('data', (chunk) => {
      buf += chunk
      if (!buf.includes('\r\n\r\n')) return
      const { port } = server.address()
      if (buf.includes(' /final ')) {
        socket.end(`HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n${body.toString('utf8')}`)
        return
      }
      socket.end(`HTTP/1.1 302 Found\r\nContent-Length: 0\r\nLocation: http://127.0.0.1:${port}/final\r\nConnection: close\r\n\r\n`)
    })
  })
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
  try {
    const { port } = server.address()
    const manifest = writeManifest(dir, `http://127.0.0.1:${port}/start`, { body })
    const result = await run(manifest)
    assert.equal(result.status, 0, `${result.stderr}\n${result.stdout}`)
    assert.match(result.stdout, new RegExp(`sha256 ${sha256(body)} verified`))
    assert.doesNotMatch(result.stderr, /Content-Length 0/)
  } finally {
    server.close()
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

test('check-open-design-release-asset still verifies size when Content-Length is absent', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-asset-nolen-'))
  const body = Buffer.from('no-length')
  const server = net.createServer((socket) => {
    socket.end(`HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n${body.toString('utf8')}`)
  })
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
  try {
    const { port } = server.address()
    const manifest = writeManifest(dir, `http://127.0.0.1:${port}/open-design.tar.gz`, { body })
    const result = await run(manifest)
    assert.equal(result.status, 0, `${result.stderr}\n${result.stdout}`)
    assert.match(result.stdout, /sha256 [0-9a-f]{64} verified/)
    assert.match(result.stdout, new RegExp(`downloaded size ${body.length} matches`))
  } finally {
    server.close()
    fs.rmSync(dir, { recursive: true, force: true })
  }
})
