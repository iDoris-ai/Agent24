// M5 B3: the presence connection and the two-step elevation against a real
// service-mode agent24d. Runs only when AGENT24D_ENROL_BIN points at a debug
// build with the test-enrollment feature and AGENT24_TEST_UNSIGNED=1 is set:
// that switch (K-B3b) lets this unsigned client enrol and leaves the presence
// provider alone, so every challenge comes over the presence connection to
// `ask`. CI's desktop job has neither; the daemon side is covered in
// rust/apps/agent24d. To do once K-B3b is in: the three summaries
// (ApprovalDecision, ScheduleManage, Shutdown).

import { describe, it, expect, afterEach, vi } from 'vitest'
import { spawn, type ChildProcess } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import { BackendManager, daemonConnection, daemonElevation } from './backend-manager'
import { PresenceConnection, type PresenceDecision, type SocketChallenge } from './presence-connection'
import { sendElevating } from './elevated-request'

const bin = process.env['AGENT24D_ENROL_BIN']
const unsigned = process.env['AGENT24_TEST_UNSIGNED'] === '1'

describe.skipIf(!bin || !unsigned || process.platform === 'win32')('a real service-mode agent24d (M5 B3)', () => {
  const saved = { home: process.env['HOME'], dataDir: process.env['AGENT24_DATA_DIR'], backend: process.env['AGENT24_BACKEND'] }
  let manager: BackendManager | null = null
  let presence: PresenceConnection | null = null
  let daemon: ChildProcess | null = null
  let dir = ''

  afterEach(async () => {
    await presence?.stop()
    presence = null
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

  async function start(answer: PresenceDecision) {
    // Short: the enrolment sockets must fit macOS's 103-byte limit.
    dir = fs.mkdtempSync(path.join('/tmp', 'a24p-'))
    process.env['HOME'] = dir
    process.env['AGENT24_DATA_DIR'] = path.join(dir, 'data')
    delete process.env['AGENT24_BACKEND']
    const env: NodeJS.ProcessEnv = { ...process.env, AGENT24_TEST_UNSIGNED: '1', AGENT24_TEST_ENROLL_IDENTIFIER: 'ai.auraai.agent24' }
    delete env['AGENT24_TEST_PRESENCE']
    daemon = spawn(bin!, ['serve', '--port', '0', '--auth-mode', 'capabilities', '--enroll-endpoint'], { env, stdio: 'ignore' })
    const recordPath = path.join(dir, 'data', 'daemon.json')
    await expect.poll(() => fs.existsSync(recordPath), { timeout: 30_000, interval: 100 }).toBe(true)
    manager = new BackendManager({ authMode: 'capabilities', spawnProcess: () => { throw new Error('spawned') } })
    await manager.start()
    expect(manager.status()).toBe('running')
    const ask = vi.fn(async (_c: SocketChallenge, _s: AbortSignal) => answer)
    presence = new PresenceConnection({ target: () => manager!.presenceTarget(), ask })
    presence.start()
    await vi.waitFor(() => expect(presence!.ready).toBe(true), { timeout: 10_000 })
    return { ask }
  }

  const deps = { connection: daemonConnection, elevation: daemonElevation }
  const stop = { method: 'POST' as const, path: '/api/v1/idoris/stop' }

  it('allow: the challenge comes over the presence connection, and the elevated route goes through', async () => {
    const { ask } = await start('allow')
    const res = await sendElevating(stop, {}, deps)
    expect(res.status).not.toBe(403)
    expect(ask).toHaveBeenCalledTimes(1)
    expect(ask.mock.calls[0]![0]).toMatchObject(stop)
  }, 90_000)

  it('deny: 403 presence_denied, asked once, nothing retried', async () => {
    const { ask } = await start('deny')
    const res = await sendElevating(stop, {}, deps)
    expect(res.status).toBe(403)
    expect((res.data as { error: { code: string } }).error.code).toBe('presence_denied')
    expect(ask).toHaveBeenCalledTimes(1)
  }, 90_000)

  it('the presence connection stays up across the daemon\'s pings (pong within 10 s)', async () => {
    const { ask } = await start('deny')
    await new Promise((r) => setTimeout(r, 75_000))
    expect(presence!.ready).toBe(true)
    expect(ask).not.toHaveBeenCalled()
  }, 120_000)
})
