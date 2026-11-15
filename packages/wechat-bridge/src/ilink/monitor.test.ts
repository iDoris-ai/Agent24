import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { InboundHttpError } from '../agent24.js'
import { Bridge, DEV_ROUTE_MISSING, HOLD_GIVE_UP, RECORD_FAILED_REPLY } from '../bridge.js'
import { CONFIG } from '../config.js'
import type { Agent24Client, InboundEvent } from '../agent24.js'
import type { Sender } from './sender.js'
import { Monitor } from './monitor.js'
import { MessageState, MessageType, type WeixinMessage } from './types.js'
import type { ILinkClient } from './client.js'

const cleanup: Array<() => void> = []
afterEach(() => {
  for (const fn of cleanup.splice(0).reverse()) fn()
})

function userMessage(id: number, text: string): WeixinMessage {
  return {
    message_id: id,
    from_user_id: 'alice',
    to_user_id: 'bot',
    client_id: 'c',
    create_time_ms: 1,
    message_type: MessageType.USER,
    message_state: MessageState.FINISH,
    context_token: 'ctx',
    item_list: [{ type: 1, text_item: { text } }],
  }
}

describe('monitor cursor and 404', () => {
  it('exits on 404 without a newer cursor or the failure reply', async () => {
    const polls: string[] = []
    const sends: string[] = []
    const errors: string[] = []
    const previous = console.error
    console.error = (...args: unknown[]) => {
      errors.push(args.map(String).join(' '))
    }
    cleanup.push(() => {
      console.error = previous
    })
    const client = {
      async post(_path: string, body: { get_updates_buf: string }) {
        polls.push(body.get_updates_buf)
        return { get_updates_buf: '9', msgs: [userMessage(1, '你好'), userMessage(2, '下一条')] }
      },
    } as unknown as ILinkClient
    const agent = {
      async recordInbound() {
        throw new InboundHttpError(404)
      },
    } as unknown as Agent24Client
    const sender = {
      async send(_user: string, _ctx: string, text: string) {
        sends.push(text)
      },
    } as unknown as Sender
    const bridge = new Bridge(agent, sender)
    const monitor = new Monitor(client, (msg) => bridge.handle(msg), '')
    await expect(monitor.start()).resolves.toBe('dev_tool')
    expect(polls).toEqual([''])
    expect(sends).toEqual([])
    expect(errors.join('\n')).toContain(DEV_ROUTE_MISSING)
  })

  it('holds the in-memory cursor when persisting it fails', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'bridge-cursor-hold-'))
    const dev = path.join(dir, 'dev')
    fs.mkdirSync(dev, { mode: 0o700 })
    fs.chmodSync(dev, 0o755)
    const previousData = process.env.AGENT24_DATA_DIR
    const previousDelay = CONFIG.RECONNECT_DELAY_MS
    process.env.AGENT24_DATA_DIR = dir
    CONFIG.RECONNECT_DELAY_MS = 15
    const polls: string[] = []
    const errors: string[] = []
    const previous = console.error
    console.error = (...args: unknown[]) => {
      errors.push(args.map(String).join(' '))
    }
    cleanup.push(() => {
      console.error = previous
      CONFIG.RECONNECT_DELAY_MS = previousDelay
      if (previousData === undefined) delete process.env.AGENT24_DATA_DIR
      else process.env.AGENT24_DATA_DIR = previousData
      fs.rmSync(dir, { recursive: true, force: true })
    })
    const client = {
      async post(_path: string, body: { get_updates_buf: string }) {
        polls.push(body.get_updates_buf)
        return { get_updates_buf: '42', msgs: [] }
      },
    } as unknown as ILinkClient
    const monitor = new Monitor(client, async () => 'advance', '')
    const finished = monitor.start()
    const deadline = Date.now() + 2000
    while (polls.length < 2) {
      if (Date.now() > deadline) throw new Error(`polls ${JSON.stringify(polls)}`)
      await new Promise((resolve) => setTimeout(resolve, 10))
    }
    monitor.stop()
    await finished
    expect(polls.slice(0, 2)).toEqual(['', ''])
    expect(errors.join('\n')).toContain('游标没有写入')
  })

  it('advances past a bad timestamp and posts the next message', async () => {
    const dir = privateDev()
    const polls: string[] = []
    const recorded: InboundEvent[] = []
    const sends: string[] = []
    const parked = park()
    const client = {
      async post(_path: string, body: { get_updates_buf: string }) {
        polls.push(body.get_updates_buf)
        // A real long-poll blocks. Returning immediately after the cursor
        // moves spins the loop and never lets the test observe it.
        if (body.get_updates_buf !== '') {
          await parked
          return { get_updates_buf: '9', msgs: [] }
        }
        const bad = userMessage(1, '坏时间')
        bad.create_time_ms = 1e20
        return { get_updates_buf: '9', msgs: [bad, userMessage(2, '正常')] }
      },
    } as unknown as ILinkClient
    const agent = {
      async recordInbound(event: InboundEvent) {
        recorded.push(event)
        return { id: event.id, outcome: 'started' }
      },
    } as unknown as Agent24Client
    const sender = {
      async send(_user: string, _ctx: string, text: string) {
        sends.push(text)
      },
    } as unknown as Sender
    const monitor = new Monitor(client, (msg) => new Bridge(agent, sender).handle(msg), '')
    cleanup.push(() => {
      monitor.stop()
      releasePark()
    })
    const finished = monitor.start()
    const deadline = Date.now() + 2000
    while (!polls.includes('9') || recorded.length < 1) {
      if (Date.now() > deadline) {
        throw new Error(`polls ${JSON.stringify(polls)} recorded ${JSON.stringify(recorded)}`)
      }
      await new Promise((resolve) => setTimeout(resolve, 10))
    }
    monitor.stop()
    releasePark()
    await finished
    expect(recorded.map((event) => event.text)).toEqual(['正常'])
    expect(sends).toEqual(['已记录'])
    fs.rmSync(dir, { recursive: true, force: true })
  })

  it.each([
    ['unreachable', new Error('connect ECONNREFUSED')],
    ['503', new InboundHttpError(503)],
  ])('keeps the cursor while the daemon stays %s past the ceiling', async (_label, fail) => {
    const dir = privateDev()
    const previousDelay = CONFIG.RECONNECT_DELAY_MS
    const previousCap = CONFIG.HOLD_BACKOFF_CAP_MS
    CONFIG.RECONNECT_DELAY_MS = 1
    CONFIG.HOLD_BACKOFF_CAP_MS = 1
    const polls: string[] = []
    const attempts: string[] = []
    cleanup.push(() => {
      CONFIG.RECONNECT_DELAY_MS = previousDelay
      CONFIG.HOLD_BACKOFF_CAP_MS = previousCap
    })
    const parked = park()
    const client = {
      async post(_path: string, body: { get_updates_buf: string }) {
        polls.push(body.get_updates_buf)
        if (body.get_updates_buf !== '') {
          await parked
          return { get_updates_buf: body.get_updates_buf, msgs: [] }
        }
        return { get_updates_buf: '9', msgs: [userMessage(7, '一直失败')] }
      },
    } as unknown as ILinkClient
    const agent = {
      async recordInbound(event: InboundEvent) {
        attempts.push(event.text ?? '')
        throw fail
      },
    } as unknown as Agent24Client
    const sender = { async send() {} } as unknown as Sender
    const bridge = new Bridge(agent, sender)
    const monitor = new Monitor(client, (msg) => bridge.handle(msg), '')
    cleanup.push(() => {
      monitor.stop()
      releasePark()
    })
    const finished = monitor.start()
    const deadline = Date.now() + 3000
    // Past the ceiling, one more attempt still holds. A give-up publishes cursor "9".
    while (attempts.length <= HOLD_GIVE_UP + 2 && !polls.includes('9')) {
      if (Date.now() > deadline) {
        throw new Error(`polls ${JSON.stringify(polls)} attempts ${attempts.length}`)
      }
      await new Promise((resolve) => setTimeout(resolve, 5))
    }
    monitor.stop()
    releasePark()
    await finished
    expect(attempts.length).toBeGreaterThan(HOLD_GIVE_UP)
    expect(polls.every((cursor) => cursor === '')).toBe(true)
    expect(polls).not.toContain('9')
    fs.rmSync(dir, { recursive: true, force: true })
  })

  it('advances after an unlisted 4xx is held past the ceiling', async () => {
    const dir = privateDev()
    const previousDelay = CONFIG.RECONNECT_DELAY_MS
    const previousCap = CONFIG.HOLD_BACKOFF_CAP_MS
    CONFIG.RECONNECT_DELAY_MS = 1
    CONFIG.HOLD_BACKOFF_CAP_MS = 1
    const polls: string[] = []
    const attempts: string[] = []
    cleanup.push(() => {
      CONFIG.RECONNECT_DELAY_MS = previousDelay
      CONFIG.HOLD_BACKOFF_CAP_MS = previousCap
    })
    const parked = park()
    const client = {
      async post(_path: string, body: { get_updates_buf: string }) {
        polls.push(body.get_updates_buf)
        if (body.get_updates_buf !== '') {
          await parked
          return { get_updates_buf: '9', msgs: [] }
        }
        return { get_updates_buf: '9', msgs: [userMessage(7, '一直失败')] }
      },
    } as unknown as ILinkClient
    const agent = {
      async recordInbound(event: InboundEvent) {
        attempts.push(event.text ?? '')
        throw new InboundHttpError(418)
      },
    } as unknown as Agent24Client
    const sender = { async send() {} } as unknown as Sender
    const bridge = new Bridge(agent, sender)
    const monitor = new Monitor(client, (msg) => bridge.handle(msg), '')
    cleanup.push(() => {
      monitor.stop()
      releasePark()
    })
    const finished = monitor.start()
    const deadline = Date.now() + 3000
    while (!polls.includes('9')) {
      if (Date.now() > deadline) {
        throw new Error(`polls ${JSON.stringify(polls)} attempts ${attempts.length}`)
      }
      await new Promise((resolve) => setTimeout(resolve, 5))
    }
    monitor.stop()
    releasePark()
    await finished
    expect(attempts.length).toBeGreaterThan(HOLD_GIVE_UP)
    expect(polls.filter((cursor) => cursor === '').length).toBeGreaterThan(HOLD_GIVE_UP)
    fs.rmSync(dir, { recursive: true, force: true })
  })

  it('stops posting a given-up message while a later one in the batch still holds', async () => {
    const dir = privateDev()
    const previousDelay = CONFIG.RECONNECT_DELAY_MS
    const previousCap = CONFIG.HOLD_BACKOFF_CAP_MS
    CONFIG.RECONNECT_DELAY_MS = 1
    CONFIG.HOLD_BACKOFF_CAP_MS = 1
    const posts: string[] = []
    const sends: string[] = []
    const polls: string[] = []
    cleanup.push(() => {
      CONFIG.RECONNECT_DELAY_MS = previousDelay
      CONFIG.HOLD_BACKOFF_CAP_MS = previousCap
    })
    const parked = park()
    const client = {
      async post(_path: string, body: { get_updates_buf: string }) {
        polls.push(body.get_updates_buf)
        if (body.get_updates_buf !== '') {
          await parked
          return { get_updates_buf: body.get_updates_buf, msgs: [] }
        }
        return {
          get_updates_buf: '9',
          msgs: [userMessage(2, 'B'), userMessage(3, 'C')],
        }
      },
    } as unknown as ILinkClient
    const agent = {
      async recordInbound(event: InboundEvent) {
        posts.push(event.id)
        if (event.id === 'wechat:2') throw new InboundHttpError(418)
        throw new InboundHttpError(503)
      },
    } as unknown as Agent24Client
    const sender = {
      async send(_user: string, _ctx: string, text: string) {
        sends.push(text)
      },
    } as unknown as Sender
    const bridge = new Bridge(agent, sender)
    const monitor = new Monitor(client, (msg) => bridge.handle(msg), '')
    cleanup.push(() => {
      monitor.stop()
      releasePark()
    })
    const previous = console.error
    console.error = () => {}
    cleanup.push(() => {
      console.error = previous
    })
    const finished = monitor.start()
    const deadline = Date.now() + 3000
    const count = (id: string) => posts.filter((posted) => posted === id).length
    while (!(count('wechat:2') === HOLD_GIVE_UP + 1 && count('wechat:3') >= 5)) {
      if (Date.now() > deadline) {
        throw new Error(`posts B=${count('wechat:2')} C=${count('wechat:3')} total=${posts.length}`)
      }
      await new Promise((resolve) => setTimeout(resolve, 5))
    }
    monitor.stop()
    releasePark()
    await finished
    expect(count('wechat:2')).toBe(HOLD_GIVE_UP + 1)
    expect(count('wechat:3')).toBeGreaterThanOrEqual(5)
    expect(polls).not.toContain('9')
    expect(sends).toEqual([RECORD_FAILED_REPLY, RECORD_FAILED_REPLY])
    fs.rmSync(dir, { recursive: true, force: true })
  })
})

let releasePark = (): void => {}

/** Resolves when the test calls {@link releasePark}. */
function park(): Promise<void> {
  return new Promise((resolve) => {
    releasePark = resolve
  })
}

function privateDev(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'bridge-cursor-ok-'))
  const dev = path.join(dir, 'dev')
  fs.mkdirSync(dev, { mode: 0o700 })
  fs.chmodSync(dev, 0o700)
  const previousData = process.env.AGENT24_DATA_DIR
  process.env.AGENT24_DATA_DIR = dir
  cleanup.push(() => {
    if (previousData === undefined) delete process.env.AGENT24_DATA_DIR
    else process.env.AGENT24_DATA_DIR = previousData
    fs.rmSync(dir, { recursive: true, force: true })
  })
  return dir
}
