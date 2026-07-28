// iLink HTTP client — authed POST + unauthed GET (QR login). Ported from
// heinu1 (bot/src/ilink/client.ts); the wire contract is ground truth.

import { randomBytes } from 'node:crypto'

function randomWechatUin(): string {
  const value = randomBytes(4).readUInt32BE(0)
  return Buffer.from(String(value), 'utf8').toString('base64')
}

function buildHeaders(token: string): Record<string, string> {
  return {
    'Content-Type': 'application/json',
    AuthorizationType: 'ilink_bot_token',
    Authorization: `Bearer ${token}`,
    'X-WECHAT-UIN': randomWechatUin(),
  }
}

function normalizeBase(base: string): string {
  return base.replace(/\/+$/, '')
}

async function parseResponse<T>(res: Response, label: string): Promise<T> {
  const text = await res.text()
  const payload: unknown = text ? JSON.parse(text) : {}
  const p = payload as { ret?: number; errmsg?: string }
  if (!res.ok) {
    throw new Error(`${p?.errmsg ?? `${label} HTTP ${res.status}`} (HTTP ${res.status})`)
  }
  if (typeof p?.ret === 'number' && p.ret !== 0) {
    throw new Error(`${label} ret=${p.ret}: ${p.errmsg ?? ''}`)
  }
  return payload as T
}

export class ILinkClient {
  constructor(
    private readonly token: string,
    private readonly baseUrl: string, // domain only, e.g. https://ilinkai.weixin.qq.com
  ) {}

  async post<T>(endpoint: string, body: unknown, timeoutMs = 15_000): Promise<T> {
    const url = new URL(endpoint, normalizeBase(this.baseUrl) + '/')
    const res = await fetch(url, {
      method: 'POST',
      headers: buildHeaders(this.token),
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(timeoutMs),
    })
    return parseResponse<T>(res, endpoint)
  }
}

/** Unauthenticated client used only during QR login (before there is a token). */
export class ILinkPreAuth {
  constructor(private readonly baseUrl: string) {}

  async get<T>(endpoint: string, extraHeaders: Record<string, string> = {}): Promise<T> {
    const url = new URL(endpoint, normalizeBase(this.baseUrl) + '/')
    const res = await fetch(url, { headers: extraHeaders })
    return parseResponse<T>(res, endpoint)
  }
}
