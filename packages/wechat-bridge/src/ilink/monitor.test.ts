import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { InboundHttpError } from '../agent24.js'
import { Bridge, DEV_ROUTE_MISSING } from '../bridge.js'
import { CONFIG } from '../config.js'
import type { Agent24Client } from '../agent24.js'
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
})
