// End-to-end harness: the real bridge stack against FakeILink and FakeDaemon.
// A finished user message with text is posted as an inbound event. `started`
// is answered with 「已记录」. A sender the daemon ignores is posted and not answered.

import { describe, it, expect, afterEach } from 'vitest'
import { Bridge } from './bridge.js'
import { Monitor } from './ilink/monitor.js'
import { Sender } from './ilink/sender.js'
import { ILinkClient } from './ilink/client.js'
import { Agent24Client } from './agent24.js'
import { CONFIG } from './config.js'
import { FakeILink } from './testing/fake-ilink.js'
import { FakeDaemon } from './testing/fake-daemon.js'

const teardowns: Array<() => Promise<void>> = []
afterEach(async () => {
  for (const t of teardowns.splice(0).reverse()) await t()
})

async function harness() {
  const ilink = await new FakeILink().listen()
  const daemon = await new FakeDaemon().listen()
  const client = new ILinkClient('fake-token', ilink.baseUrl)
  const sender = new Sender(client)
  const agent = new Agent24Client({ base: daemon.baseUrl, token: 'test-bearer' })
  const bridge = new Bridge(agent, sender)
  const monitor = new Monitor(client, (msg) => bridge.handle(msg))
  const finished = monitor.start()
  teardowns.push(async () => {
    monitor.stop()
    await ilink.close()
    await daemon.close()
    await finished
  })
  return { ilink, daemon, finished }
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
    ilink.pushUserText('alice', '过期')
    await expect(done).resolves.toBe('auth')
    expect(ilink.polls).toEqual([''])
    expect(ilink.outbound).toEqual([])
    expect(daemon.events).toEqual([])
  })
})
