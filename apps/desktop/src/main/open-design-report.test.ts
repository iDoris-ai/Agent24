import http from 'node:http'
import type { AddressInfo } from 'node:net'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { OpenDesignComponentState } from './open-design-component'
import type { SidecarState } from './sidecar-contract'
import {
  OPEN_DESIGN_HEARTBEAT_MS,
  OPEN_DESIGN_REPORT_PATH,
  OpenDesignStatusReporter,
  mapOpenDesignReport,
  postOpenDesignReport,
  type OpenDesignReport,
  type OpenDesignReportInput,
} from './open-design-report'

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
    const sent: OpenDesignReport[] = []
    const logs: string[] = []
    const send = vi.fn(async (_connection: unknown, reportBody: OpenDesignReport) => {
      sent.push(reportBody)
      return 204
    })
    const reporter = new OpenDesignStatusReporter({
      snapshot: () => body,
      connection: () => connection,
      send,
      log: (message) => { logs.push(message) },
    })
    return {
      reporter,
      sent,
      logs,
      send,
      setBody: (next: OpenDesignReport) => { body = next },
      setConnection: (next: typeof connection) => { connection = next },
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
    send.mockResolvedValueOnce(403)
    expect(() => reporter.notify()).not.toThrow()
    await flush()
    expect(logs.some((line) => line.includes('403'))).toBe(true)
    setConnection(null)
    reporter.notify()
    await flush()
    expect(logs.some((line) => line.includes('not up'))).toBe(true)
    setConnection({ port: 9, authHeaders: { Authorization: 'Bearer host' } })
    send.mockResolvedValueOnce(404)
    await vi.advanceTimersByTimeAsync(OPEN_DESIGN_HEARTBEAT_MS)
    await flush()
    expect(logs.some((line) => line.includes('404'))).toBe(true)
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
})
