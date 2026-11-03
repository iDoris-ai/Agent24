import { EventEmitter } from 'node:events'
import http from 'node:http'
import type { ChildProcess } from 'node:child_process'
import type { AddressInfo } from 'node:net'
import { PassThrough } from 'node:stream'
import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  BackendManager,
  daemonAuthMode,
  daemonConnection,
  onDaemonReady,
  type ExternalDaemonSource,
} from './backend-manager'
import type { OpenDesignComponentState } from './open-design-component'
import {
  OPEN_DESIGN_HEARTBEAT_MS,
  OPEN_DESIGN_REPORT_PATH,
  OpenDesignStatusReporter,
  mapOpenDesignReport,
  REPORT_REASON_MAX_BYTES,
  postOpenDesignReport,
  sanitizeReportReason,
  type OpenDesignReport,
  type OpenDesignReportInput,
} from './open-design-report'
import { CLIENT_PROTOCOL_VERSION } from './protocol-compatibility'
import type { SidecarState } from './sidecar-contract'

const VERSION = '0.26.1'

function report(input: OpenDesignReportInput): OpenDesignReport {
  return mapOpenDesignReport({ version: VERSION, ...input })
}

describe('mapOpenDesignReport', () => {
  it.each<[OpenDesignComponentState, OpenDesignReport]>([
    ['not-installed', { state: 'absent' }],
    ['downloading', { state: 'installing' }],
    ['verifying', { state: 'installing' }],
    ['paused', { state: 'installing' }],
    ['installed', { state: 'installed', version: VERSION }],
    ['failed', { state: 'failed', version: VERSION }],
  ])('maps install state %s', (installState, expected) => {
    expect(report({ installState })).toEqual(expected)
  })

  it('maps unavailable to absent and keeps a reason', () => {
    expect(report({ installState: 'unavailable', reason: 'platform mismatch' })).toEqual({
      state: 'absent', reason: 'platform mismatch',
    })
    expect(report({ installState: 'unavailable' })).toEqual({ state: 'absent', reason: 'unavailable' })
    expect(report({ installState: 'unavailable', reason: 'bad\u0000reason' })).toEqual({
      state: 'absent', reason: 'badreason',
    })
  })

  it('keeps a failed install error as the reason and drops a version the daemon would reject', () => {
    expect(report({ installState: 'failed', reason: 'sha256 mismatch' })).toEqual({
      state: 'failed', version: VERSION, reason: 'sha256 mismatch',
    })
    expect(mapOpenDesignReport({ installState: 'installed', version: 'has spaces' })).toEqual({ state: 'installed' })
  })

  it.each<[SidecarState, OpenDesignReport['state']]>([
    ['stopped', 'stopped'],
    ['starting', 'starting'],
    ['ready', 'ready'],
    ['healthy', 'ready'],
    ['degraded', 'degraded'],
    ['stopping', 'stopping'],
    ['failed', 'failed'],
  ])('maps sidecar %s to %s once the component is installed and the headless instance is up', (sidecar, state) => {
    expect(report({ installState: 'installed', sidecar })).toEqual({ state, version: VERSION })
  })

  it('stays on the install mapping when the headless instance is not running', () => {
    expect(report({ installState: 'installed', sidecar: null })).toEqual({ state: 'installed', version: VERSION })
    expect(report({ installState: 'not-installed', sidecar: 'ready' })).toEqual({ state: 'absent' })
    expect(report({ installState: 'downloading', sidecar: 'ready' })).toEqual({ state: 'installing' })
  })
})

describe('OpenDesignStatusReporter', () => {
  afterEach(() => { vi.useRealTimers() })

  function harness(initial: OpenDesignReport = { state: 'absent' }) {
    let body = initial
    let connection: { port: number; authHeaders: Record<string, string> } | null = {
      port: 9, authHeaders: { Authorization: 'Bearer host' },
    }
    let authMode: 'legacy' | 'capabilities' | null = 'capabilities'
    const sent: OpenDesignReport[] = []
    const logs: string[] = []
    const infos: string[] = []
    const send = vi.fn(async (_connection: unknown, reportBody: OpenDesignReport) => {
      sent.push(reportBody)
      return 204
    })
    const reporter = new OpenDesignStatusReporter({
      snapshot: () => body,
      connection: () => connection,
      authMode: () => authMode,
      send,
      log: (message) => { logs.push(message) },
      info: (message) => { infos.push(message) },
    })
    return {
      reporter,
      sent,
      logs,
      infos,
      send,
      setBody: (next: OpenDesignReport) => { body = next },
      setConnection: (next: typeof connection) => { connection = next },
      setAuthMode: (next: typeof authMode) => { authMode = next },
    }
  }

  async function flush(): Promise<void> {
    for (let i = 0; i < 8; i += 1) await Promise.resolve()
  }

  it('reports a state change immediately and repeats it every 30 seconds', async () => {
    vi.useFakeTimers()
    const { reporter, sent, setBody } = harness()
    reporter.start()
    await flush()
    expect(sent).toEqual([{ state: 'absent' }])
    setBody({ state: 'installing' })
    reporter.notify()
    await flush()
    expect(sent).toEqual([{ state: 'absent' }, { state: 'installing' }])
    await vi.advanceTimersByTimeAsync(OPEN_DESIGN_HEARTBEAT_MS - 1)
    expect(sent).toHaveLength(2)
    await vi.advanceTimersByTimeAsync(1)
    await flush()
    expect(sent).toEqual([{ state: 'absent' }, { state: 'installing' }, { state: 'installing' }])
    expect(OPEN_DESIGN_HEARTBEAT_MS).toBe(30_000)
  })

  it('does not throw when the daemon is down or refuses the report, and retries later', async () => {
    vi.useFakeTimers()
    const { reporter, send, logs, setConnection, setBody } = harness({ state: 'installed', version: VERSION })
    send.mockResolvedValueOnce(401)
    reporter.start()
    await flush()
    expect(logs.some((line) => line.includes('401'))).toBe(true)
    send.mockRejectedValueOnce(new Error('socket hang up'))
    setBody({ state: 'ready', version: VERSION })
    expect(() => reporter.notify()).not.toThrow()
    await flush()
    expect(logs.some((line) => line.includes('socket hang up'))).toBe(true)
    send.mockResolvedValueOnce(500)
    expect(() => reporter.notify()).not.toThrow()
    await flush()
    expect(logs.some((line) => line.includes('500'))).toBe(true)
    setConnection(null)
    reporter.notify()
    await flush()
    expect(logs.some((line) => line.includes('not up'))).toBe(true)
    setConnection({ port: 9, authHeaders: { Authorization: 'Bearer host' } })
    send.mockResolvedValueOnce(502)
    await vi.advanceTimersByTimeAsync(OPEN_DESIGN_HEARTBEAT_MS)
    await flush()
    expect(logs.some((line) => line.includes('502'))).toBe(true)
    send.mockResolvedValueOnce(204)
    await vi.advanceTimersByTimeAsync(OPEN_DESIGN_HEARTBEAT_MS)
    await flush()
    expect(send).toHaveBeenLastCalledWith(
      { port: 9, authHeaders: { Authorization: 'Bearer host' } },
      { state: 'ready', version: VERSION },
    )
    expect(send.mock.calls.length).toBeGreaterThanOrEqual(4)
  })

  it('clears the heartbeat when stopped', async () => {
    vi.useFakeTimers()
    const { reporter, sent, setBody } = harness()
    reporter.start()
    await flush()
    reporter.stop()
    setBody({ state: 'ready' })
    reporter.notify()
    await vi.advanceTimersByTimeAsync(OPEN_DESIGN_HEARTBEAT_MS * 3)
    await flush()
    expect(sent).toEqual([{ state: 'absent' }])
  })

  it('reportNow sends again when that payload was already accepted', async () => {
    const { reporter, sent } = harness()
    reporter.start()
    await flush()
    expect(sent).toEqual([{ state: 'absent' }])
    reporter.notify()
    await flush()
    expect(sent).toHaveLength(1)
    reporter.reportNow()
    await flush()
    expect(sent).toEqual([{ state: 'absent' }, { state: 'absent' }])
  })

  it('does not report when the daemon is not in capabilities mode, and logs that once', async () => {
    vi.useFakeTimers()
    const { reporter, send, infos, logs, setAuthMode } = harness()
    setAuthMode('legacy')
    reporter.start()
    await flush()
    expect(send).not.toHaveBeenCalled()
    expect(infos).toEqual(['open-design report skipped: daemon auth mode is legacy, not capabilities'])
    expect(logs).toEqual([])
    reporter.notify()
    await vi.advanceTimersByTimeAsync(OPEN_DESIGN_HEARTBEAT_MS * 2)
    await flush()
    reporter.reportNow()
    await flush()
    expect(send).not.toHaveBeenCalled()
    expect(infos).toHaveLength(1)
    setAuthMode('capabilities')
    reporter.reportNow()
    await flush()
    expect(send).toHaveBeenCalledTimes(1)
  })

  it('stops after HTTP 403 or 404 until the next reportNow, and warns once per suspension', async () => {
    vi.useFakeTimers()
    const { reporter, send, logs, setBody } = harness({ state: 'installed', version: VERSION })
    reporter.start()
    await flush()
    expect(send).toHaveBeenCalledTimes(1)

    setBody({ state: 'ready', version: VERSION })
    send.mockResolvedValueOnce(403)
    reporter.notify()
    await flush()
    expect(logs.filter((line) => line.includes('403'))).toHaveLength(1)

    reporter.notify()
    await vi.advanceTimersByTimeAsync(OPEN_DESIGN_HEARTBEAT_MS * 2)
    await flush()
    expect(send).toHaveBeenCalledTimes(2)
    expect(logs.filter((line) => line.includes('403'))).toHaveLength(1)

    send.mockResolvedValueOnce(204)
    reporter.reportNow()
    await flush()
    expect(send).toHaveBeenCalledTimes(3)

    setBody({ state: 'degraded', version: VERSION })
    send.mockResolvedValueOnce(404)
    reporter.notify()
    await flush()
    expect(logs.filter((line) => line.includes('404'))).toHaveLength(1)
    const calls = send.mock.calls.length
    await vi.advanceTimersByTimeAsync(OPEN_DESIGN_HEARTBEAT_MS)
    await flush()
    expect(send).toHaveBeenCalledTimes(calls)

    send.mockResolvedValueOnce(204)
    reporter.reportNow()
    await flush()
    expect(send).toHaveBeenCalledTimes(calls + 1)
  })
})

describe('Open Design report follows daemon ready', () => {
  function childAnnouncing(ready: Record<string, unknown>): ChildProcess {
    const stdout = new PassThrough()
    const child = new EventEmitter() as unknown as ChildProcess
    Object.assign(child, { pid: 7, stdout, stderr: null, kill: vi.fn().mockReturnValue(true) })
    setImmediate(() => stdout.write(`${JSON.stringify({ type: 'ready', ...ready })}\n`))
    return child
  }

  async function settle(): Promise<void> {
    for (let i = 0; i < 10; i += 1) await Promise.resolve()
  }

  function external(read: ExternalDaemonSource['read']): ExternalDaemonSource {
    return {
      read,
      checkHealth: async () => true,
      fetchHealth: async () => ({ ok: true, protocolVersion: CLIENT_PROTOCOL_VERSION }),
      // A LaunchAgent-managed daemon that accepts its recorded token, so the
      // legacy record below is reused (not taken over) as before T1-0.
      serviceManaged: () => true,
      acceptsToken: async () => true,
      requestShutdown: async () => 202,
      isAlive: () => true,
    }
  }

  it('reports once when the daemon is ready, again on the restarted connection, and dedupe does not swallow either', async () => {
    vi.useRealTimers()
    const sent: Array<{ port: number; body: OpenDesignReport }> = []
    const reporter = new OpenDesignStatusReporter({
      snapshot: () => ({ state: 'installed', version: VERSION }),
      connection: daemonConnection,
      authMode: daemonAuthMode,
      send: async (connection, body) => {
        sent.push({ port: connection.port, body })
        return 204
      },
    })
    const children: ChildProcess[] = []
    const manager = new BackendManager({
      external: external(() => null),
      authMode: 'capabilities',
      spawnProcess: () => {
        const child = childAnnouncing({
          port: 4500 + children.length,
          auth_mode: 'capabilities',
          product_host_token: `tok-${children.length}`,
        })
        children.push(child)
        return child
      },
    })
    const stopReady = onDaemonReady(() => reporter.reportNow())
    try {
      reporter.start()
      await manager.start()
      await vi.waitFor(() => expect(sent).toEqual([{ port: 4500, body: { state: 'installed', version: VERSION } }]))
      await settle()
      reporter.notify()
      await settle()
      expect(sent).toHaveLength(1)

      manager.restart()
      await vi.waitFor(() => expect(sent.map((item) => item.port)).toEqual([4500, 4501]))
      await settle()
      expect(sent[1]?.body).toEqual({ state: 'installed', version: VERSION })
      reporter.notify()
      await settle()
      expect(sent).toHaveLength(2)
    } finally {
      stopReady()
      reporter.stop()
      manager.stop()
    }
  })

  it('does not post while a reused legacy daemon is the connection, and says so once', async () => {
    vi.useRealTimers()
    const sent: number[] = []
    const infos: string[] = []
    const reporter = new OpenDesignStatusReporter({
      snapshot: () => ({ state: 'installed', version: VERSION }),
      connection: daemonConnection,
      authMode: daemonAuthMode,
      send: async () => {
        sent.push(1)
        return 204
      },
      info: (message) => { infos.push(message) },
    })
    const manager = new BackendManager({
      external: external(() => ({ port: 4600, token: 'legacy-token', pid: 9, authMode: 'legacy' })),
      authMode: 'capabilities',
      spawnProcess: vi.fn(),
    })
    const stopReady = onDaemonReady(() => reporter.reportNow())
    try {
      reporter.start()
      await manager.start()
      await settle()
      expect(daemonAuthMode()).toBe('legacy')
      expect(sent).toEqual([])
      expect(infos).toEqual(['open-design report skipped: daemon auth mode is legacy, not capabilities'])
      reporter.notify()
      await settle()
      expect(sent).toEqual([])
      expect(infos).toHaveLength(1)
    } finally {
      stopReady()
      reporter.stop()
      manager.stop()
    }
  })
})

describe('postOpenDesignReport', () => {
  it('posts the host credential to the open-design report route and returns the status', async () => {
    const seen: { method?: string; url?: string; authorization?: string; body?: string } = {}
    const server = http.createServer((req, res) => {
      const chunks: Buffer[] = []
      req.on('data', (chunk: Buffer) => chunks.push(chunk))
      req.on('end', () => {
        seen.method = req.method
        seen.url = req.url
        seen.authorization = req.headers.authorization
        seen.body = Buffer.concat(chunks).toString()
        res.writeHead(req.headers.authorization === 'Bearer host' ? 204 : 401)
        res.end()
      })
    })
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
    try {
      const port = (server.address() as AddressInfo).port
      const status = await postOpenDesignReport(
        { port, authHeaders: { Authorization: 'Bearer host' } },
        { state: 'ready', version: VERSION },
      )
      expect(status).toBe(204)
      expect(seen).toEqual({
        method: 'POST',
        url: OPEN_DESIGN_REPORT_PATH,
        authorization: 'Bearer host',
        body: JSON.stringify({ state: 'ready', version: VERSION }),
      })
    } finally {
      await new Promise<void>((resolve) => server.close(() => resolve()))
    }
  })

  it('gives up at the whole-request deadline even while the response keeps trickling', async () => {
    const server = http.createServer((_req, res) => {
      res.writeHead(204, { 'Transfer-Encoding': 'chunked' })
      // One byte well inside the idle timeout, forever: only a total deadline ends this.
      const drip = setInterval(() => res.write(' '), 20)
      res.on('close', () => clearInterval(drip))
    })
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
    try {
      const port = (server.address() as AddressInfo).port
      const started = Date.now()
      await expect(postOpenDesignReport(
        { port, authHeaders: { Authorization: 'Bearer host' } },
        { state: 'stopped', version: VERSION },
        200,
      )).rejects.toThrow()
      expect(Date.now() - started).toBeLessThan(2_000)
    } finally {
      server.closeAllConnections()
      await new Promise<void>((resolve) => server.close(() => resolve()))
    }
  })
})

describe('sanitizeReportReason', () => {
  it('caps the reason at 256 UTF-8 bytes, not characters, and never splits a code point', () => {
    const cjk = sanitizeReportReason('中'.repeat(100))!
    expect(Buffer.byteLength(cjk, 'utf8')).toBeLessThanOrEqual(REPORT_REASON_MAX_BYTES)
    expect(cjk).toBe('中'.repeat(85))
    const emoji = sanitizeReportReason('a' + '😀'.repeat(100))!
    expect(Buffer.byteLength(emoji, 'utf8')).toBeLessThanOrEqual(REPORT_REASON_MAX_BYTES)
    expect(emoji).toBe('a' + '😀'.repeat(63))
    expect(sanitizeReportReason('x'.repeat(300))).toBe('x'.repeat(256))
  })

  it('drops control characters and returns nothing for an empty result', () => {
    expect(sanitizeReportReason('bad\u0000\nline')).toBe('badline')
    expect(sanitizeReportReason('\u0007')).toBeUndefined()
    expect(sanitizeReportReason(undefined)).toBeUndefined()
  })
})
