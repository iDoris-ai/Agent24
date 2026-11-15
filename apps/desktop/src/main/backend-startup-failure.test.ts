import { EventEmitter } from 'node:events'
import { PassThrough } from 'node:stream'
import type { ChildProcess } from 'node:child_process'
import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  BackendManager,
  getBackendEndpoint,
  getDaemonStartupFailure,
  parseDaemonFailureLine,
  stderrExitFailure,
  type ExternalDaemonSource,
} from './backend-manager'

const MIGRATION = 'migration 34 was previously applied but is missing in the resolved migrations'

describe('parseDaemonFailureLine', () => {
  it('reads the daemon failed line', () => {
    expect(parseDaemonFailureLine(JSON.stringify({ type: 'failed', code: 'start_failed', error: MIGRATION }))).toEqual({
      code: 'start_failed', error: MIGRATION,
    })
  })

  it('ignores ready lines, plain logs, and broken JSON', () => {
    expect(parseDaemonFailureLine('{"type":"ready","port":1}')).toBeNull()
    expect(parseDaemonFailureLine('booting')).toBeNull()
    expect(parseDaemonFailureLine('{')).toBeNull()
    expect(parseDaemonFailureLine('{"type":"failed"}')).toEqual({ code: 'start_failed', error: '' })
  })

  it('builds the stderr fallback from the last line', () => {
    expect(stderrExitFailure('database is locked', 1)).toEqual({ code: 'exit', error: 'database is locked' })
    expect(stderrExitFailure('   ', 2)).toEqual({ code: 'exit', error: 'exit 2' })
  })
})

describe('BackendManager records why the daemon did not start', () => {
  const managers: BackendManager[] = []
  afterEach(() => { for (const manager of managers) manager.stop() ; managers.length = 0 })

  function external(): ExternalDaemonSource {
    return {
      read: () => null,
      checkHealth: vi.fn().mockResolvedValue(false),
      fetchHealth: vi.fn().mockResolvedValue({ ok: false, protocolVersion: null }),
      serviceInstalled: () => false,
      acceptsToken: vi.fn().mockResolvedValue(false),
      requestShutdown: vi.fn().mockResolvedValue(null),
      isAlive: () => false,
    }
  }

  function childOf(pid: number): { child: ChildProcess; stdout: PassThrough; stderr: PassThrough } {
    const stdout = new PassThrough()
    const stderr = new PassThrough()
    const child = Object.assign(new EventEmitter(), { pid, stdout, stderr, kill: vi.fn().mockReturnValue(true) }) as unknown as ChildProcess
    return { child, stdout, stderr }
  }

  async function start(spawn: () => ChildProcess): Promise<BackendManager> {
    const manager = new BackendManager({ external: external(), spawnProcess: spawn, authMode: 'legacy' })
    managers.push(manager)
    await manager.start()
    return manager
  }

  it('keeps the failed line, and a later ready line clears it', async () => {
    const first = childOf(11)
    const second = childOf(12)
    const spawned = [first, second]
    let n = 0
    const manager = await start(() => spawned[n++]!.child)
    first.stdout.write(`${JSON.stringify({ type: 'failed', code: 'start_failed', error: MIGRATION })}\n`)
    await vi.waitFor(() => expect(getDaemonStartupFailure()).toEqual({ code: 'start_failed', error: MIGRATION }))
    first.child.emit('exit', 1)
    await vi.waitFor(() => expect(getBackendEndpoint()).toBeNull())
    expect(getDaemonStartupFailure()?.code).toBe('start_failed')

    manager.restart()
    second.stdout.write(`${JSON.stringify({ type: 'ready', port: 4242, token: 't' })}\n`)
    await vi.waitFor(() => expect(getBackendEndpoint()).toEqual({ port: 4242 }))
    expect(getDaemonStartupFailure()).toBeNull()
  })

  it('falls back to the last stderr line when the process exits non-zero with no failed line', async () => {
    const { child, stderr } = childOf(21)
    await start(() => child)
    stderr.write('noise\n')
    stderr.write('migration helper: disk full\n')
    await new Promise((r) => setImmediate(r))
    child.emit('exit', 1)
    await vi.waitFor(() => expect(getDaemonStartupFailure()).toEqual({ code: 'exit', error: 'migration helper: disk full' }))
  })

  it('a zero exit does not invent a failure from stderr', async () => {
    const { child, stderr } = childOf(22)
    await start(() => child)
    stderr.write('bye\n')
    await new Promise((r) => setImmediate(r))
    child.emit('exit', 0)
    await new Promise((r) => setImmediate(r))
    expect(getDaemonStartupFailure()).toBeNull()
  })
})
