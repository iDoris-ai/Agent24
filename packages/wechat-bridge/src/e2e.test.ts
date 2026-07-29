// End-to-end harness test (H11): the REAL bridge stack — Monitor + Bridge +
// Sender + ILinkClient + Agent24Client — wired against the hermetic FakeILink and
// FakeDaemon. Nothing is mocked at the module boundary; both HTTP hops are real,
// so this is the regression net for the channel's inbound → run → reply and
// approval-over-WeChat round-trips that otherwise could only be tested by hand.

import { describe, it, expect, afterEach } from 'vitest'
import { Bridge, type SessionStore } from './bridge.js'
import { Monitor } from './ilink/monitor.js'
import { Sender } from './ilink/sender.js'
import { ILinkClient } from './ilink/client.js'
import { Agent24Client } from './agent24.js'
import { FakeILink } from './testing/fake-ilink.js'
import { FakeDaemon, type Planner } from './testing/fake-daemon.js'

const teardowns: Array<() => Promise<void>> = []
afterEach(async () => {
  for (const t of teardowns.splice(0).reverse()) await t()
})

async function harness(plan: Planner, allowed: string[] = ['alice']) {
  const ilink = await new FakeILink().listen()
  const daemon = await new FakeDaemon(plan).listen()
  const client = new ILinkClient('fake-token', ilink.baseUrl)
  const sender = new Sender(client)
  const agent = new Agent24Client({ base: daemon.baseUrl, token: '' })
  const store: SessionStore = { load: () => new Map(), save: () => {} }
  const bridge = new Bridge(agent, sender, store, new Set(allowed))
  const monitor = new Monitor(client, (msg) => void bridge.handle(msg).catch(() => {}))
  monitor.start()
  teardowns.push(async () => {
    monitor.stop()
    await ilink.close() // flushes the parked long-poll so the Monitor loop exits
    await daemon.close()
  })
  return { ilink, daemon }
}

describe('WeChat channel end-to-end (real stack, fake transport + daemon)', () => {
  it('inbound message → run → reply comes back over the channel', async () => {
    const { ilink, daemon } = await harness((prompt) => ({ kind: 'completed', text: `echo:${prompt}` }))

    ilink.pushUserText('alice', '你好')
    const reply = await ilink.waitOutbound((t) => t.includes('echo:你好'))

    expect(reply.toUserId).toBe('alice')
    expect(daemon.prompts).toContain('你好')
  })

  it('approval-over-WeChat: park → "y" resolves → resumed run reply', async () => {
    const { ilink, daemon } = await harness((prompt) =>
      prompt === '删笔记'
        ? { kind: 'approval', summary: '删除 ~/notes.txt', then: { kind: 'completed', text: '已删除' } }
        : { kind: 'completed', text: 'ok' },
    )

    ilink.pushUserText('alice', '删笔记')
    await ilink.waitOutbound((t) => t.includes('需要你批准') && t.includes('删除 ~/notes.txt'))

    ilink.pushUserText('alice', 'y')
    const done = await ilink.waitOutbound((t) => t.includes('已删除'))

    expect(done.toUserId).toBe('alice')
    expect(daemon.decisions).toEqual([{ approvalId: expect.any(String), type: 'approve' }])
  })

  it('unauthorized sender is dropped before any run (fail-closed, end-to-end)', async () => {
    const { ilink, daemon } = await harness((prompt) => ({ kind: 'completed', text: `echo:${prompt}` }))

    ilink.pushUserText('mallory', 'rm -rf ~') // not in the allowlist
    ilink.pushUserText('alice', 'ping') // authorized; ordered after mallory
    await ilink.waitOutbound((t) => t.includes('echo:ping'))

    // Only alice's prompt ever reached the daemon — mallory's never became a run.
    expect(daemon.prompts).toEqual(['ping'])
  })
})
