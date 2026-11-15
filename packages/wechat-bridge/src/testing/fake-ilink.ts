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

type OutboundWaiter = {
  match: (text: string, toUserId: string) => boolean
  resolve: (rec: OutboundRecord) => void
  reject: (err: Error) => void
  timer: ReturnType<typeof setTimeout>
}

interface StoredUpdate {
  seq: number
  msg: WeixinMessage
}

interface PendingPoll {
  after: number
  res: http.ServerResponse
}

export class FakeILink {
  private readonly server: http.Server
  private readonly history: StoredUpdate[] = []
  private readonly pending: PendingPoll[] = []
  private readonly outboundWaiters: OutboundWaiter[] = []
  private nextSeq = 1
  private nextMessageId = 1
  /**
   * Seq captured on the first empty-cursor poll. Older history is not
   * replayed. Later polls that still send `''` use this same floor, so a
   * hold can redeliver. A committed numeric cursor is the only way to skip
   * messages that arrived after that floor.
   */
  private emptyFloor: number | null = null
  /** Every outbound message the bot sent, in order (ack + real replies). */
  readonly outbound: OutboundRecord[] = []
  /** `get_updates_buf` sent on each getupdates, in order. */
  readonly polls: string[] = []
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
    const buf = this.latestBuf()
    while (this.pending.length) {
      const waiter = this.pending.shift()!
      this.respondUpdates(waiter.res, [], buf)
    }
    for (const w of this.outboundWaiters.splice(0)) {
      clearTimeout(w.timer)
      w.reject(new Error('FakeILink closed before outbound arrived'))
    }
    await new Promise<void>((resolve, reject) =>
      this.server.close((err) => (err ? reject(err) : resolve())),
    )
  }

  /** Complete parked long-polls so a stopped Monitor can leave the loop. */
  releasePolls(): void {
    const buf = this.latestBuf()
    while (this.pending.length) {
      const waiter = this.pending.shift()!
      this.respondUpdates(waiter.res, [], buf)
    }
  }

  /** Wait until `n` getupdates requests have been seen. */
  async waitUntilPolled(n: number, timeoutMs = 2000): Promise<void> {
    const start = Date.now()
    while (this.polls.length < n) {
      if (Date.now() - start > timeoutMs) {
        throw new Error(`timed out waiting for ${n} polls, saw ${this.polls.length}`)
      }
      await new Promise((resolve) => setTimeout(resolve, 10))
    }
  }

  /** Simulate a WeChat user sending a text message to the bot. */
  pushUserText(fromUserId: string, text: string): void {
    this.enqueue(fromUserId, text)
    this.wake()
  }

  /** Queue several messages and wake once, so one poll sees the whole batch. */
  pushBatch(items: Array<{ from: string; text: string }>): void {
    for (const item of items) this.enqueue(item.from, item.text)
    this.wake()
  }

  private enqueue(fromUserId: string, text: string): void {
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
    const seq = this.nextSeq++
    this.history.push({ seq, msg })
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
      const raw = (body as { get_updates_buf?: unknown }).get_updates_buf
      const cursor = typeof raw === 'string' ? raw : ''
      this.polls.push(cursor)
      // Empty does not replay from seq 0. The floor freezes on the first
      // empty poll. A restart that does not load the persisted cursor sends
      // `''` again and is handed that same floor — the resume test stays
      // red unless it passes the committed buf.
      const after = cursor === '' ? (this.emptyFloor ??= this.latestSeq()) : Number(cursor) || 0
      const msgs = this.since(after)
      if (msgs.length) this.respondUpdates(res, msgs, this.latestBuf())
      else this.pending.push({ after, res })
      return
    }
    if (url.startsWith('/ilink/bot/sendmessage')) {
      this.recordOutbound(body as SendMessageBody)
      send(res, { ret: 0 })
      return
    }
    send(res, { ret: 0 })
  }

  private since(after: number): WeixinMessage[] {
    return this.history.filter((item) => item.seq > after).map((item) => item.msg)
  }

  private latestSeq(): number {
    const last = this.history[this.history.length - 1]
    return last ? last.seq : 0
  }

  private latestBuf(): string {
    const seq = this.latestSeq()
    return seq ? String(seq) : ''
  }

  /** Deliver queued messages to polls that are still behind the history. */
  private wake(): void {
    const buf = this.latestBuf()
    const stay: PendingPoll[] = []
    for (const waiter of this.pending) {
      const msgs = this.since(waiter.after)
      if (msgs.length) this.respondUpdates(waiter.res, msgs, buf)
      else stay.push(waiter)
    }
    this.pending.splice(0, this.pending.length, ...stay)
  }

  private respondUpdates(res: http.ServerResponse, msgs: WeixinMessage[], buf: string): void {
    send(res, { ret: 0, msgs, get_updates_buf: buf })
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
