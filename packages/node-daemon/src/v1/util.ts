// Shared v1 utilities: body reading with a hard size cap, and ULID generation.

import type http from 'node:http'
import crypto from 'node:crypto'

export const MAX_BODY_BYTES = 1024 * 1024 // 1 MiB — loopback is not a DoS boundary

export class PayloadTooLargeError extends Error {
  statusCode = 413
  constructor() {
    super(`Request body exceeds ${MAX_BODY_BYTES} bytes`)
  }
}

/** Read a JSON body, rejecting with PayloadTooLargeError beyond MAX_BODY_BYTES.
 * On overflow the remaining body is drained (discarded, bounded memory) rather
 * than destroying the socket — destroying would prevent the 413 response from
 * ever reaching the client. */
export function readBodyLimited(req: http.IncomingMessage): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = []
    let total = 0
    let exceeded = false
    req.on('data', (chunk: Buffer) => {
      if (exceeded) return // drain & discard
      total += chunk.length
      if (total > MAX_BODY_BYTES) {
        exceeded = true
        chunks.length = 0
        return
      }
      chunks.push(chunk)
    })
    req.on('end', () => {
      if (exceeded) { reject(new PayloadTooLargeError()); return }
      const raw = Buffer.concat(chunks).toString()
      if (!raw) { resolve({}); return }
      try { resolve(JSON.parse(raw)) } catch { resolve({}) }
    })
    req.on('error', reject)
  })
}

// ── ULID (Crockford base32, 48-bit time + 80-bit randomness) ─────────────────
const B32 = '0123456789ABCDEFGHJKMNPQRSTVWXYZ'

export function ulid(now = Date.now()): string {
  let time = ''
  let t = now
  for (let i = 0; i < 10; i++) {
    time = B32[t % 32] + time
    t = Math.floor(t / 32)
  }
  const rand = crypto.randomBytes(16)
  let out = ''
  for (let i = 0; i < 16; i++) out += B32[rand[i]! % 32]
  return time + out
}
