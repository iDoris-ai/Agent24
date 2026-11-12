// End-to-end harness: the real bridge stack against FakeILink and FakeDaemon.
// A finished user message with text is posted as an inbound event. `started`
// is answered with 「已记录」. A sender the daemon ignores is posted and not answered.

import { describe, it, expect, afterEach } from 'vitest'
import { Bridge } from './bridge.js'
import { Monitor } from './ilink/monitor.js'
import { Sender } from './ilink/sender.js'
import { ILinkClient } from './ilink/client.js'
import { Agent24Client } from './agent24.js'
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
  const monitor = new Monitor(client, (msg) => void bridge.handle(msg).catch(() => {}))
  monitor.start()
  teardowns.push(async () => {
    monitor.stop()
    await ilink.close()
    await daemon.close()
  })
  return { ilink, daemon }
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
})
