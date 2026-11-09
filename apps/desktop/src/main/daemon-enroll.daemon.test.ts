// M5 B2: enrols with a real agent24d in service mode. Runs only when
// AGENT24D_ENROL_BIN points at a debug build with the test-enrollment feature
// (`cargo build -p agent24d --features test-enrollment`): enrolment accepts
// only Developer ID signed clients otherwise, and neither CI nor the build
// machine has one. The daemon side is covered by agent24d's own tests.

import { describe, it, expect, afterEach } from 'vitest'
import { spawn, type ChildProcess } from 'node:child_process'
import fs from 'node:fs'
import http from 'node:http'
import path from 'node:path'
import { BackendManager, daemonConnection } from './backend-manager'
import { enrol } from './daemon-enroll'

const bin = process.env['AGENT24D_ENROL_BIN']

function get(port: number, route: string, headers: Record<string, string>): Promise<number> {
  return new Promise((resolve, reject) => {
    const req = http.get({ host: '127.0.0.1', port, path: route, headers, timeout: 10_000 }, (res) => {
      res.resume()
      resolve(res.statusCode ?? 0)
    })
    req.on('error', reject)
  })
}

describe.skipIf(!bin || process.platform === 'win32')('a real service-mode agent24d (M5 B2)', () => {
  const saved = { home: process.env['HOME'], dataDir: process.env['AGENT24_DATA_DIR'], backend: process.env['AGENT24_BACKEND'] }
  let manager: BackendManager | null = null
  let daemon: ChildProcess | null = null
  let dir = ''

  afterEach(async () => {
    manager?.stop()
    manager = null
    if (daemon && daemon.exitCode === null) {
      const exited = new Promise((resolve) => daemon!.once('exit', resolve))
      daemon.kill('SIGTERM')
      await exited
    }
    daemon = null
    for (const [key, value] of [['HOME', saved.home], ['AGENT24_DATA_DIR', saved.dataDir], ['AGENT24_BACKEND', saved.backend]] as const) {
      if (value === undefined) delete process.env[key]
      else process.env[key] = value
    }
    if (dir) fs.rmSync(dir, { recursive: true, force: true })
  })

  /** Starts the daemon as the LaunchAgent does (plist ProgramArguments) and returns its daemon.json. */
  async function serviceDaemon(presence: 'allow' | null): Promise<Record<string, unknown>> {
    // Short: the enrolment socket must fit macOS's 103-byte limit.
    dir = fs.mkdtempSync(path.join(process.platform === 'darwin' ? '/tmp' : (process.env['TMPDIR'] ?? '/tmp'), 'a24e-'))
    process.env['HOME'] = dir
    process.env['AGENT24_DATA_DIR'] = path.join(dir, 'data')
    delete process.env['AGENT24_BACKEND']
    const env: NodeJS.ProcessEnv = { ...process.env }
    if (presence) env['AGENT24_TEST_PRESENCE'] = presence
    else delete env['AGENT24_TEST_PRESENCE']
    daemon = spawn(bin!, ['serve', '--port', '0', '--auth-mode', 'capabilities', '--enroll-endpoint'], { env, stdio: 'ignore' })
    const recordPath = path.join(dir, 'data', 'daemon.json')
    await expect.poll(() => fs.existsSync(recordPath), { timeout: 30_000, interval: 100 }).toBe(true)
    return JSON.parse(fs.readFileSync(recordPath, 'utf8')) as Record<string, unknown>
  }

  const neverSpawn = (): ChildProcess => { throw new Error('spawned over the service daemon') }

  it('the app enrols as first-party shell, does not spawn, and its token works only until renewed', async () => {
    const record = await serviceDaemon('allow')
    expect(record['auth_mode']).toBe('capabilities')
    expect(typeof record['enroll_endpoint']).toBe('string')
    manager = new BackendManager({ authMode: 'capabilities', spawnProcess: neverSpawn })
    await manager.start()
    expect(manager.isExternalDaemon()).toBe(true)
    expect(manager.status()).toBe('running')
    const conn = daemonConnection()!
    expect(conn.port).toBe(record['port'])
    expect(await get(conn.port, '/api/v1/models', conn.authHeaders)).toBe(200)
    expect(await get(conn.port, '/api/v1/models', {})).toBe(401)
    const token = conn.authHeaders['Authorization']!.slice('Bearer '.length)
    expect(fs.readFileSync(path.join(dir, 'data', 'daemon.json'), 'utf8')).not.toContain(token)

    // Renew on a connection of its own: the new token works, the old is void.
    const session = await enrol({ socketPath: record['enroll_endpoint'] as string, generation: record['generation'] as string, httpPort: record['port'] as number })
    const before = session.token
    const after = await session.renew()
    expect(after).not.toBe(before)
    expect(await get(conn.port, '/api/v1/models', { Authorization: `Bearer ${after}` })).toBe(200)
    expect(await get(conn.port, '/api/v1/models', { Authorization: `Bearer ${before}` })).toBe(401)
    session.close()
  }, 60_000)

  it('an unsigned build the daemon rejects is reported, not spawned over', async () => {
    await serviceDaemon(null)
    manager = new BackendManager({ authMode: 'capabilities', spawnProcess: neverSpawn })
    await manager.start()
    expect(manager.status()).toBe('error')
    expect(manager.statusDetail()).toMatchObject({ kind: 'enrol_refused', code: 'identity_rejected' })
    expect(daemonConnection()).toBeNull()
  }, 60_000)
})
