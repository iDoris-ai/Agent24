import { EventEmitter } from 'node:events'
import https from 'node:https'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { DownloadRejected, MAX_CONTROL_RESPONSE_BYTES, requestHttps } from './os-package-net'

afterEach(() => {
  vi.restoreAllMocks()
})

function reply(chunks: Buffer[], headers: Record<string, string> = {}, status = 200): void {
  vi.spyOn(https, 'get').mockImplementation(((url: string | URL, options: unknown, callback?: (res: EventEmitter) => void) => {
    const cb = typeof options === 'function' ? options : callback
    const res = new EventEmitter()
    let destroyed = false
    Object.assign(res, {
      statusCode: status,
      headers,
      resume() {},
      destroy() {
        destroyed = true
        return res
      },
    })
    const req = new EventEmitter()
    Object.assign(req, {
      destroy(error?: Error) {
        if (error) req.emit('error', error)
        return req
      },
    })
    queueMicrotask(() => {
      cb?.(res)
      for (const chunk of chunks) {
        if (destroyed) return
        res.emit('data', chunk)
      }
      if (!destroyed) res.emit('end')
    })
    void url
    return req
  }) as typeof https.get)
}

describe('requestHttps', () => {
  it('stops release JSON and SHA256SUMS at 64 KiB', () => {
    expect(MAX_CONTROL_RESPONSE_BYTES).toBe(64 * 1024)
  })

  it('rejects a response whose content-length is over 64 KiB', async () => {
    reply([], { 'content-length': String(MAX_CONTROL_RESPONSE_BYTES + 1) })
    await expect(requestHttps('https://api.github.com/repos/iDoris-ai/Sin90/releases/latest', () => false))
      .rejects.toBeInstanceOf(DownloadRejected)
  })

  it('rejects a chunked body once it passes 64 KiB', async () => {
    reply([Buffer.alloc(MAX_CONTROL_RESPONSE_BYTES), Buffer.alloc(1)])
    await expect(requestHttps('https://github.com/iDoris-ai/Sin90/releases/download/v0.5.2/SHA256SUMS', () => false))
      .rejects.toThrow(/response is too large/)
  })

  it('returns a body of exactly 64 KiB', async () => {
    const body = Buffer.alloc(MAX_CONTROL_RESPONSE_BYTES, 0x61)
    reply([body], { 'content-length': String(body.length) })
    await expect(requestHttps('https://example.com/SHA256SUMS', () => false)).resolves.toEqual(body)
  })
})
