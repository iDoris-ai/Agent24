// Hermetic fake of the WeChat iLink Bot web API — the FakeSlack pattern from
// OpenWorker (docs/reference-notes/openworker.md §4): the REAL adapter (Monitor
// + Sender + ILinkClient) runs against an in-process fake HTTP server instead of
// the live weixin.qq.com service, so the whole inbound → run → reply → approval
// round-trip is automatable with no network, no QR scan, and no real sleeps.
//
// Only the two endpoints the running bridge hits are faked:
//   POST /ilink/bot/getupdates   — long-poll: resolves as soon as a message is
//                                   queued (so tests never wait the real 45s)
//   POST /ilink/bot/sendmessage  — records outbound text for assertions

import http from 'node:http'
import type { AddressInfo } from 'node:net'
import { MessageItemType, MessageState, MessageType, type WeixinMessage } from '../ilink/types.js'

export interface OutboundRecord {
  toUserId: string
  text: string
}

type UpdatesResolver = (msgs: WeixinMessage[]) => void
type OutboundWaiter = {
  match: (text: string, toUserId: string) => boolean
  resolve: (rec: OutboundRecord) => void
  reject: (err: Error) => void
  timer: ReturnType<typeof setTimeout>
}

export class FakeILink {
  private readonly server: http.Server
  private readonly inbound: WeixinMessage[] = []
  private readonly updatesWaiters: UpdatesResolver[] = []
  private readonly outboundWaiters: OutboundWaiter[] = []
  private cursor = 0
  private nextMessageId = 1
  /** Every outbound message the bot sent, in order (ack + real replies). */
  readonly outbound: OutboundRecord[] = []
  port = 0

  constructor() {
    this.server = http.createServer((req, res) => void this.handle(req, res))
  }

  async listen(): Promise<this> {
    await new Promise<void>((resolve) => this.server.listen(0, '127.0.0.1', resolve))
    this.port = (this.server.address() as AddressInfo).port
    return this
  }

  get baseUrl(): string {
    return `http://127.0.0.1:${this.port}`
  }

  async close(): Promise<void> {
    // Flush any parked long-poll so in-flight fetches complete and the Monitor
    // loop can observe `running=false` and exit, instead of hanging to the 45s
    // client timeout.
    while (this.updatesWaiters.length) this.updatesWaiters.shift()!([])
    for (const w of this.outboundWaiters.splice(0)) {
      clearTimeout(w.timer)
      w.reject(new Error('FakeILink closed before outbound arrived'))
    }
    await new Promise<void>((resolve, reject) =>
      this.server.close((err) => (err ? reject(err) : resolve())),
    )
  }

  /** Simulate a WeChat user sending a text message to the bot. */
  pushUserText(fromUserId: string, text: string): void {
    const msg: WeixinMessage = {
      message_id: this.nextMessageId++,
      from_user_id: fromUserId,
      to_user_id: 'bot',
      client_id: `c${this.nextMessageId}`,
      create_time_ms: 0,
      message_type: MessageType.USER,
      message_state: MessageState.FINISH,
      context_token: `ctx-${fromUserId}`,
      item_list: [{ type: MessageItemType.TEXT, text_item: { text } }],
    }
    const waiter = this.updatesWaiters.shift()
    if (waiter) waiter([msg])
    else this.inbound.push(msg)
  }

  /** Resolve when the bot sends an outbound message whose text matches. */
  waitOutbound(
    match: (text: string, toUserId: string) => boolean,
    timeoutMs = 2000,
  ): Promise<OutboundRecord> {
    const existing = this.outbound.find((o) => match(o.text, o.toUserId))
    if (existing) return Promise.resolve(existing)
    return new Promise<OutboundRecord>((resolve, reject) => {
      const timer = setTimeout(() => {
        const i = this.outboundWaiters.findIndex((w) => w.resolve === resolve)
        if (i >= 0) this.outboundWaiters.splice(i, 1)
        reject(new Error(`timed out waiting for outbound after ${timeoutMs}ms`))
      }, timeoutMs)
      this.outboundWaiters.push({ match, resolve, reject, timer })
    })
  }

  private async handle(req: http.IncomingMessage, res: http.ServerResponse): Promise<void> {
    const body = await readJson(req)
    const url = req.url ?? ''
    if (url.startsWith('/ilink/bot/getupdates')) {
      if (this.inbound.length) {
        this.respondUpdates(res, this.inbound.splice(0))
      } else {
        this.updatesWaiters.push((msgs) => this.respondUpdates(res, msgs))
      }
      return
    }
    if (url.startsWith('/ilink/bot/sendmessage')) {
      this.recordOutbound(body as SendMessageBody)
      send(res, { ret: 0 })
      return
    }
    send(res, { ret: 0 })
  }

  private respondUpdates(res: http.ServerResponse, msgs: WeixinMessage[]): void {
    send(res, { ret: 0, msgs, get_updates_buf: String(++this.cursor) })
  }

  private recordOutbound(body: SendMessageBody): void {
    const msg = body?.msg
    const text = (msg?.item_list ?? [])
      .map((i) => i.text_item?.text ?? '')
      .join('')
    const rec: OutboundRecord = { toUserId: msg?.to_user_id ?? '', text }
    this.outbound.push(rec)
    for (let i = this.outboundWaiters.length - 1; i >= 0; i--) {
      const w = this.outboundWaiters[i]!
      if (w.match(rec.text, rec.toUserId)) {
        clearTimeout(w.timer)
        this.outboundWaiters.splice(i, 1)
        w.resolve(rec)
      }
    }
  }
}

interface SendMessageBody {
  msg?: {
    to_user_id?: string
    item_list?: { text_item?: { text?: string } }[]
  }
}

function readJson(req: http.IncomingMessage): Promise<unknown> {
  return new Promise((resolve) => {
    let raw = ''
    req.on('data', (c) => (raw += c))
    req.on('end', () => {
      try {
        resolve(raw ? JSON.parse(raw) : {})
      } catch {
        resolve({})
      }
    })
  })
}

function send(res: http.ServerResponse, payload: unknown): void {
  const s = JSON.stringify(payload)
  res.writeHead(200, { 'Content-Type': 'application/json' })
  res.end(s)
}
