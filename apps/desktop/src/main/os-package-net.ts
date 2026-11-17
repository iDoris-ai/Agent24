// HTTPS for a registered release. The first URL is the github.com download
// URL; a redirect may only land on GitHub's release CDN.

import fs from 'node:fs'
import https from 'node:https'
import { isAllowedDownloadRedirect } from '../shared/os-package-registry'

const MAX_REDIRECTS = 5

export class DownloadRejected extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'DownloadRejected'
  }
}

export function requestHttps(url: string, redirectOk: (next: string) => boolean, timeoutMs = 60_000): Promise<Buffer> {
  return readHttps(url, redirectOk, timeoutMs, null)
}

function readHttps(
  url: string,
  redirectOk: (next: string) => boolean,
  timeoutMs: number,
  onData: ((chunk: Buffer, received: number, total: number | null) => void) | null,
  redirects = 0,
): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    const req = https.get(url, {
      headers: { 'User-Agent': 'Agent24', Accept: 'application/vnd.github+json' },
      timeout: timeoutMs,
    }, (res) => {
      const status = res.statusCode ?? 0
      const location = res.headers.location
      if (status >= 300 && status < 400 && location) {
        res.resume()
        if (redirects >= MAX_REDIRECTS) {
          reject(new DownloadRejected('too many redirects'))
          return
        }
        const next = new URL(location, url).href
        if (!redirectOk(next)) {
          reject(new DownloadRejected('redirect is not a GitHub release URL'))
          return
        }
        resolve(readHttps(next, redirectOk, timeoutMs, onData, redirects + 1))
        return
      }
      if (status !== 200) {
        res.resume()
        reject(new DownloadRejected(`download status ${status}`))
        return
      }
      const total = contentLength(res.headers['content-length'])
      const chunks: Buffer[] = []
      let received = 0
      res.on('data', (chunk: Buffer) => {
        received += chunk.length
        if (onData) onData(chunk, received, total)
        else chunks.push(chunk)
      })
      res.on('end', () => resolve(onData ? Buffer.alloc(0) : Buffer.concat(chunks)))
      res.on('error', reject)
    })
    req.on('timeout', () => req.destroy(new DownloadRejected('download timed out')))
    req.on('error', reject)
  })
}

function contentLength(value: string | string[] | undefined): number | null {
  const raw = Array.isArray(value) ? value[0] : value
  if (!raw) return null
  const n = Number(raw)
  return Number.isFinite(n) && n >= 0 ? n : null
}

/** Writes `url` to `dest`. `acceptInitial` checks the URL before any byte is read. */
export function downloadToFile(
  url: string,
  dest: string,
  acceptInitial: (url: string) => boolean,
  onProgress: (received: number, total: number | null) => void,
  maxBytes: number,
  redirects = 0,
): Promise<void> {
  if (!acceptInitial(url)) return Promise.reject(new DownloadRejected('download URL is not in the registry'))
  return new Promise((resolve, reject) => {
    let settled = false
    const fail = (error: unknown): void => {
      if (settled) return
      settled = true
      reject(error instanceof Error ? error : new DownloadRejected(String(error)))
    }
    const ok = (): void => {
      if (settled) return
      settled = true
      resolve()
    }
    const req = https.get(url, {
      headers: { 'User-Agent': 'Agent24', Accept: 'application/octet-stream' },
      timeout: 60_000,
    }, (res) => {
      const status = res.statusCode ?? 0
      if (status >= 300 && status < 400 && res.headers.location) {
        res.resume()
        if (redirects >= MAX_REDIRECTS) {
          fail(new DownloadRejected('too many redirects'))
          return
        }
        const next = new URL(res.headers.location, url).href
        if (!isAllowedDownloadRedirect(next)) {
          fail(new DownloadRejected('redirect is not a GitHub release URL'))
          return
        }
        downloadToFile(next, dest, () => true, onProgress, maxBytes, redirects + 1).then(ok, fail)
        return
      }
      if (status !== 200) {
        res.resume()
        fail(new DownloadRejected(`download status ${status}`))
        return
      }
      const total = contentLength(res.headers['content-length'])
      if (total !== null && total > maxBytes) {
        res.resume()
        fail(new DownloadRejected('download is too large'))
        return
      }
      const out = fs.createWriteStream(dest, { flags: 'wx', mode: 0o600 })
      let received = 0
      res.on('data', (chunk: Buffer) => {
        received += chunk.length
        if (received > maxBytes) {
          res.destroy()
          out.destroy()
          fail(new DownloadRejected('download is too large'))
          return
        }
        onProgress(received, total)
      })
      res.pipe(out)
      out.on('finish', ok)
      out.on('error', fail)
      res.on('error', fail)
    })
    req.on('timeout', () => req.destroy(new DownloadRejected('download timed out')))
    req.on('error', fail)
  })
}
