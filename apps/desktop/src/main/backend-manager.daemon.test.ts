// Starts the real agent24d through BackendManager's own spawn path. Runs only
// when AGENT24D_BIN points at a built binary (`cargo build -p agent24d`); CI's
// desktop job has none, and the daemon side is covered by
// rust/apps/agent24d/tests/capability_startup.rs.

import { describe, it, expect, afterEach } from 'vitest'
import fs from 'node:fs'
import http from 'node:http'
import os from 'node:os'
import path from 'node:path'
import { BackendManager, daemonConnection, getBackendEndpoint, type ExternalDaemonSource } from './backend-manager'

const bin = process.env['AGENT24D_BIN']

function get(port: number, route: string, headers: Record<string, string>): Promise<number> {
  return new Promise((resolve, reject) => {
    const req = http.get({ host: '127.0.0.1', port, path: route, headers, timeout: 10_000 }, (res) => {
      res.resume()
      resolve(res.statusCode ?? 0)
    })
    req.on('error', reject)
  })
}

describe.skipIf(!bin || process.platform === 'win32')('real agent24d in capability mode (KB-HOST-01 PR-A2)', () => {
  const saved = { home: process.env['HOME'], dataDir: process.env['AGENT24_DATA_DIR'], backend: process.env['AGENT24_BACKEND'] }
  let manager: BackendManager | null = null
  let dir = ''
  let daemonPid = 0

  afterEach(async () => {
    manager?.stop()
    manager = null
    // Remove the data dir only once the daemon has finished shutting down.
    if (daemonPid > 0) await expect
      .poll(() => {
        try {
          process.kill(daemonPid, 0)
          return true
        } catch {
          return false
        }
      }, { timeout: 15_000, interval: 50 })
      .toBe(false)
    daemonPid = 0
    for (const [key, value] of [['HOME', saved.home], ['AGENT24_DATA_DIR', saved.dataDir], ['AGENT24_BACKEND', saved.backend]] as const) {
      if (value === undefined) delete process.env[key]
      else process.env[key] = value
    }
    if (dir) fs.rmSync(dir, { recursive: true, force: true })
  })

  it('starts over the private pipes, hands main the host bearer, and keeps it out of daemon.json', async () => {
    // Short: the daemon's callback sockets must fit macOS's 103-byte limit.
    dir = fs.mkdtempSync(path.join(process.platform === 'darwin' ? '/tmp' : os.tmpdir(), 'a24h-'))
    process.env['HOME'] = dir
    process.env['AGENT24_DATA_DIR'] = path.join(dir, 'data')
    delete process.env['AGENT24_BACKEND']
    const external: ExternalDaemonSource = { read: () => null, checkHealth: async () => true }
    manager = new BackendManager({ external, authMode: 'capabilities' })
    await manager.start()

    await expect.poll(() => daemonConnection(), { timeout: 30_000, interval: 100 }).not.toBeNull()
    const conn = daemonConnection()!
    expect(getBackendEndpoint()).toEqual({ port: conn.port })
    expect(await get(conn.port, '/api/v1/models', conn.authHeaders)).toBe(200)
    expect(await get(conn.port, '/api/v1/models', {})).toBe(401)

    const record = JSON.parse(fs.readFileSync(path.join(dir, 'data', 'daemon.json'), 'utf8')) as Record<string, unknown>
    expect(record['auth_mode']).toBe('capabilities')
    daemonPid = record['pid'] as number
    expect(JSON.stringify(record)).not.toContain(conn.authHeaders['Authorization']!.slice('Bearer '.length))
  }, 60_000)
})
