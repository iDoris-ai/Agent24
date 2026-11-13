// End-to-end harness: the real bridge stack against FakeILink and FakeDaemon.
// A finished user message with text is posted as an inbound event. `started`
// is answered with 「已记录」. A sender the daemon ignores is posted and not answered.

import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { describe, it, expect, afterEach } from 'vitest'
import { Bridge, RECORD_FAILED_REPLY } from './bridge.js'
import { Monitor } from './ilink/monitor.js'
import { Sender } from './ilink/sender.js'
import { ILinkClient } from './ilink/client.js'
import { Agent24Client } from './agent24.js'
import { CONFIG, loadBridgeCursor } from './config.js'
import { FakeILink } from './testing/fake-ilink.js'
import { FakeDaemon } from './testing/fake-daemon.js'

const teardowns: Array<() => Promise<void>> = []
afterEach(async () => {
  for (const t of teardowns.splice(0).reverse()) await t()
})

async function harness(cursor = '') {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'wechat-bridge-e2e-'))
  const dev = path.join(dir, 'dev')
  fs.mkdirSync(dev, { mode: 0o700 })
  fs.chmodSync(dev, 0o700)
  const previousData = process.env.AGENT24_DATA_DIR
  process.env.AGENT24_DATA_DIR = dir
  const ilink = await new FakeILink().listen()
  const daemon = await new FakeDaemon().listen()
  const client = new ILinkClient('fake-token', ilink.baseUrl)
  const sender = new Sender(client)
  const agent = new Agent24Client({ base: daemon.baseUrl, token: 'test-bearer' })
  const bridge = new Bridge(agent, sender)
  const monitor = new Monitor(client, (msg) => bridge.handle(msg), cursor)
  const finished = monitor.start()
  teardowns.push(async () => {
    monitor.stop()
    await ilink.close()
    await daemon.close()
    await finished
    if (previousData === undefined) delete process.env.AGENT24_DATA_DIR
    else process.env.AGENT24_DATA_DIR = previousData
    fs.rmSync(dir, { recursive: true, force: true })
  })
  await ilink.waitUntilPolled(1)
  return { ilink, daemon, finished, dir }
}

describe('WeChat channel end-to-end (real stack, fake transport + daemon)', () => {
  it('posts a finished user message and replies 已记录', async () => {
    const { ilink, daemon } = await harness()

    ilink.pushUserText('alice', '你好')
    const reply = await ilink.waitOutbound((text) => text === '已记录')

    expect(reply.toUserId).toBe('alice')
    expect(daemon.events.map((event) => event.text)).toEqual(['你好'])
    expect(daemon.events[0]?.id).toMatch(/^wechat:\d+$/)
    expect(daemon.events[0]?.at).toBe('1970-01-01T00:00:00Z')
  })

  it('treats y as text: one record, the same reply, no approval', async () => {
    const { ilink, daemon } = await harness()

    ilink.pushUserText('alice', 'y')
    const reply = await ilink.waitOutbound((text, to) => text === '已记录' && to === 'alice')

    expect(reply.text).toBe('已记录')
    expect(daemon.events.map((event) => event.text)).toEqual(['y'])
  })

  it('posts a stranger and does not reply when the daemon ignores them', async () => {
    const { ilink, daemon } = await harness()
    daemon.ignoredActors.add('mallory')

    ilink.pushUserText('mallory', 'rm -rf ~')
    const deadline = Date.now() + 2000
    while (daemon.events.length < 1) {
      if (Date.now() > deadline) throw new Error('stranger event was not posted')
      await new Promise((resolve) => setTimeout(resolve, 10))
    }
    await expect(ilink.waitOutbound(() => true, 200)).rejects.toThrow(/timed out/)

    expect(daemon.events.map((event) => event.actor)).toEqual(['mallory'])
    expect(ilink.outbound).toEqual([])
  })

  it('advances the cursor only after the server accepts the event', async () => {
    const { ilink, daemon } = await harness()
    ilink.pushUserText('alice', '第一条')
    await ilink.waitOutbound((text) => text === '已记录')
    ilink.pushUserText('alice', '第二条')
    const deadline = Date.now() + 2000
    while (daemon.events.length < 2) {
      if (Date.now() > deadline) {
        throw new Error(`events ${daemon.events.length} polls ${JSON.stringify(ilink.polls)}`)
      }
      await new Promise((resolve) => setTimeout(resolve, 10))
    }
    expect(ilink.polls[0]).toBe('')
    expect(ilink.polls[1]).not.toBe('')
    expect(daemon.events.map((event) => event.text)).toEqual(['第一条', '第二条'])
  })

  it('keeps the cursor when the inbound POST fails, then delivers the same message', async () => {
    const previous = CONFIG.RECONNECT_DELAY_MS
    CONFIG.RECONNECT_DELAY_MS = 30
    try {
      const { ilink, daemon } = await harness()
      daemon.failures.push(500)
      ilink.pushUserText('alice', '重投')
      const deadline = Date.now() + 3000
      while (daemon.attempts.length < 2) {
        if (Date.now() > deadline) {
          throw new Error(`attempts ${daemon.attempts.length} polls ${JSON.stringify(ilink.polls)}`)
        }
        await new Promise((resolve) => setTimeout(resolve, 10))
      }
      expect(ilink.polls[0]).toBe('')
      expect(ilink.polls[1]).toBe('')
      expect(daemon.attempts).toHaveLength(2)
      expect(daemon.attempts[0]?.id).toBe(daemon.attempts[1]?.id)
      expect(daemon.events).toHaveLength(1)
    } finally {
      CONFIG.RECONNECT_DELAY_MS = previous
    }
  })

  it('stops on 401 and does not poll with a newer cursor', async () => {
    const ilink = await new FakeILink().listen()
    const daemon = await new FakeDaemon().listen()
    teardowns.push(async () => {
      await ilink.close()
      await daemon.close()
    })
    daemon.failures.push(401)
    const client = new ILinkClient('fake-token', ilink.baseUrl)
    const bridge = new Bridge(
      new Agent24Client({ base: daemon.baseUrl, token: 'test-bearer' }),
      new Sender(client),
    )
    const monitor = new Monitor(client, (msg) => bridge.handle(msg))
    const done = monitor.start()
    await ilink.waitUntilPolled(1)
    ilink.pushUserText('alice', '过期')
    await expect(done).resolves.toBe('auth')
    expect(ilink.polls).toEqual([''])
    expect(ilink.outbound).toEqual([])
    expect(daemon.events).toEqual([])
  })

  it('advances on 400 and replies 记录失败 once', async () => {
    const { ilink, daemon } = await harness()
    daemon.failures.push(400)
    ilink.pushUserText('alice', '坏请求')
    await ilink.waitOutbound((text) => text === RECORD_FAILED_REPLY)
    ilink.pushUserText('alice', '下一条')
    await ilink.waitOutbound((text) => text === '已记录')
    const failed = ilink.outbound.filter((item) => item.text === RECORD_FAILED_REPLY)
    expect(failed).toHaveLength(1)
    expect(daemon.events.map((event) => event.text)).toEqual(['下一条'])
    expect(ilink.polls[0]).toBe('')
    expect(ilink.polls.some((cursor) => cursor !== '')).toBe(true)
  })

  it('holds the cursor on 5xx and does not post the rest of the batch', async () => {
    const previous = CONFIG.RECONNECT_DELAY_MS
    CONFIG.RECONNECT_DELAY_MS = 30
    try {
      const { ilink, daemon } = await harness()
      daemon.failures.push(500, 500, 500, 500)
      ilink.pushBatch([
        { from: 'alice', text: '先失败' },
        { from: 'alice', text: '不该投' },
      ])
      const deadline = Date.now() + 3000
      while (daemon.attempts.filter((attempt) => attempt.text === '先失败').length < 2) {
        if (Date.now() > deadline) {
          throw new Error(`attempts ${JSON.stringify(daemon.attempts)} polls ${JSON.stringify(ilink.polls)}`)
        }
        await new Promise((resolve) => setTimeout(resolve, 10))
      }
      expect(daemon.attempts.every((attempt) => attempt.text === '先失败')).toBe(true)
      expect(ilink.polls.every((cursor) => cursor === '')).toBe(true)
      expect(daemon.events).toEqual([])
    } finally {
      CONFIG.RECONNECT_DELAY_MS = previous
    }
  })

  it('resumes from the persisted cursor after 401, without redelivering the committed message', async () => {
    const { ilink, daemon, finished } = await harness()
    ilink.pushUserText('alice', '已提交')
    await ilink.waitOutbound((text) => text === '已记录')
    const cursorDeadline = Date.now() + 2000
    while (loadBridgeCursor() === '') {
      if (Date.now() > cursorDeadline) throw new Error('cursor was not saved')
      await new Promise((resolve) => setTimeout(resolve, 10))
    }
    const committed = loadBridgeCursor()
    daemon.failures.push(401)
    ilink.pushUserText('alice', '未提交')
    await expect(finished).resolves.toBe('auth')
    expect(loadBridgeCursor()).toBe(committed)
    expect(daemon.events.map((event) => event.text)).toEqual(['已提交'])

    const client = new ILinkClient('fake-token', ilink.baseUrl)
    const bridge = new Bridge(new Agent24Client({ base: daemon.baseUrl, token: 'new-bearer' }), new Sender(client))
    const restarted = new Monitor(client, (msg) => bridge.handle(msg), loadBridgeCursor())
    const restartedDone = restarted.start()
    teardowns.push(async () => {
      restarted.stop()
      const deadline = Date.now() + 2000
      while (Date.now() < deadline) {
        ilink.releasePolls()
        const settled = await Promise.race([
          restartedDone.then(() => true, () => true),
          new Promise<boolean>((resolve) => setTimeout(() => resolve(false), 20)),
        ])
        if (settled) return
      }
      throw new Error('restarted monitor did not stop')
    })
    const deadline = Date.now() + 3000
    while (!daemon.events.some((event) => event.text === '未提交')) {
      if (Date.now() > deadline) {
        throw new Error(
          `events ${JSON.stringify(daemon.events)} polls ${JSON.stringify(ilink.polls)} cursor ${committed}`,
        )
      }
      await new Promise((resolve) => setTimeout(resolve, 10))
    }
    expect(daemon.events.map((event) => event.text)).toEqual(['已提交', '未提交'])
    expect(daemon.attempts.filter((attempt) => attempt.text === '已提交')).toHaveLength(1)
    expect(ilink.polls.filter((cursor) => cursor === committed).length).toBeGreaterThan(0)
  })
})
