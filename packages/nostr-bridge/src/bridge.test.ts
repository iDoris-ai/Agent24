import { describe, it, expect } from 'vitest'
import { NostrBridge } from './bridge.js'
import { SpeakerClient } from './speaker.js'
import { FakeSpeaker } from './testing/fake-speaker.js'
import { loadAgent24Profile, toAgentSpeakerProfile } from './profile.js'
import { makeContent, PROTOCOL_VERSION } from './protocol.js'

const PROFILE_YAML = `
atomic:
  - id: post_xiaohongshu
    from: module:xiaohongshu
  - id: send_wechat
    from: module:wechat-bridge
capabilities:
  - name: "触达纺织业客户群"
    description: "在小红书/微信触达纺织行业目标客户"
    tags: [textile, marketing]
    backed_by: [post_xiaohongshu, send_wechat]
  - name: "内容分发"
    tags: [content]
publish:
  mode: tagged
  availability: "7x24"
`

function bridge(fake: FakeSpeaker) {
  return new NostrBridge(new SpeakerClient(fake.runner), 'alice')
}

describe('protocol.makeContent', () => {
  it('mints a thread id and stamps the version', () => {
    const c = makeContent({ intent: 'ask' })
    expect(c.version).toBe(PROTOCOL_VERSION)
    expect(c.intent).toBe('ask')
    expect(c.thread_id).toMatch(/[0-9a-f-]{36}/)
  })
  it('keeps a caller-supplied thread and turns ttl into an absolute expiry', () => {
    const c = makeContent({ intent: 'answer', threadId: 't1', replyTo: 'e1', ttlSeconds: 60 }, 1_000_000)
    expect(c.thread_id).toBe('t1')
    expect(c.reply_to).toBe('e1')
    expect(c.expires_at).toBe(Math.floor(1_000_000 / 1000) + 60)
  })
  it('scrubs internal atomic-tool wiring markers out of the outbound payload', () => {
    const c = makeContent({
      intent: 'inform',
      payload: {
        impl: 'handled by module:xiaohongshu',
        via: 'tool:http_fetch',
        ok: 'a normal string',
        nested: { deep: ['module:wechat-bridge'] },
      },
    })
    const json = JSON.stringify(c.payload)
    expect(json).not.toContain('module:')
    expect(json).not.toContain('tool:')
    expect(json).toContain('[redacted]')
    expect((c.payload as { ok: string }).ok).toBe('a normal string') // untouched
  })
})

describe('profile transform (business capabilities only)', () => {
  it('publishes business capabilities, never atomic tools', () => {
    const p = loadAgent24Profile(PROFILE_YAML)
    const asp = toAgentSpeakerProfile('alice-agent', p, 1234)
    expect(asp.name).toBe('alice-agent')
    expect(asp.updated_at).toBe(1234)
    expect(asp.capabilities?.map((c) => c.name)).toEqual(['触达纺织业客户群', '内容分发'])
    // atomic ids never leak into the published profile
    const json = JSON.stringify(asp)
    expect(json).not.toContain('post_xiaohongshu')
    expect(json).not.toContain('module:')
  })
  it('rejects a profile with no capabilities list', () => {
    expect(() => loadAgent24Profile('atomic: []')).toThrow(/capabilities/)
  })
})

describe('NostrBridge outbound (real bridge + SpeakerClient vs fake agent-speaker)', () => {
  it('register: transforms YAML → agent-speaker JSON and publishes it via --json-file', async () => {
    const fake = new FakeSpeaker()
    const p = loadAgent24Profile(PROFILE_YAML)
    const { result } = await bridge(fake).register('alice-agent', p, 2_000_000_000_000)

    const call = fake.calls.find((c) => c.args[0] === 'profile' && c.args[1] === 'publish')
    expect(call).toBeDefined()
    expect(call!.args).toEqual(expect.arrayContaining(['--as', '--json-file', '--json']))
    expect(call!.publishedProfile?.mode).toBe('structured') // capabilities ⇒ structured (联调)
    // the file the CLI actually received parsed back to the business profile
    expect(call!.publishedProfile?.name).toBe('alice-agent')
    expect(call!.publishedProfile?.capabilities?.[0]?.name).toBe('触达纺织业客户群')
    expect(JSON.stringify(call!.publishedProfile)).not.toContain('post_xiaohongshu')
    // consumes PR #29's structured publish result
    expect(result.published_to).toBe(1)
  })

  it('register: fails when the profile reached no relays', async () => {
    const fake = new FakeSpeaker()
    fake.publishResult = { name: 'x', published_to: 0, relay_count: 0, relays: [] }
    const p = loadAgent24Profile(PROFILE_YAML)
    await expect(bridge(fake).register('x', p)).rejects.toThrow(/no relays/)
  })

  it('say: sends a directed, encrypted message carrying the intent envelope', async () => {
    const fake = new FakeSpeaker()
    const res = await bridge(fake).say('npub1bob', { intent: 'ask', topic: 'textile-outreach', payload: { q: '能接单吗' } })

    const msg = fake.calls.find((c) => c.args[0] === 'agent' && c.args[1] === 'msg')!
    expect(msg.args).toEqual(
      expect.arrayContaining(['--from', 'alice', '--to', 'npub1bob', '--encrypt=true', '--json']),
    )
    const env = fake.lastContent()!
    expect(env.version).toBe(PROTOCOL_VERSION)
    expect(env.intent).toBe('ask')
    expect(env.topic).toBe('textile-outreach')
    expect((env.payload as { q: string }).q).toBe('能接单吗')
    expect(res.event_id).toBe('evt_fake')
  })

  it('say: fails loudly when the message reached no relays and was not queued', async () => {
    const fake = new FakeSpeaker()
    fake.sendResult = { published_to: 0, relay_count: 0, relays: [], queued_for_retry: false }
    await expect(bridge(fake).say('npub1bob', { intent: 'ask' })).rejects.toThrow(/no relays/)
  })

  it('say: accepts a queued-for-retry message (agent-speaker outbox will retry)', async () => {
    const fake = new FakeSpeaker()
    fake.sendResult = { published_to: 0, relay_count: 0, relays: [], queued_for_retry: true }
    const res = await bridge(fake).say('npub1bob', { intent: 'ask' })
    expect(res.queued_for_retry).toBe(true)
  })

  it('search: discovers agents by business capability', async () => {
    const fake = new FakeSpeaker()
    fake.discoverResult = [{ npub: 'npub1bob', profile: { name: 'bob', updated_at: 1 } }]
    const hits = await bridge(fake).search('触达纺织业客户群')

    const call = fake.calls.find((c) => c.args[0] === 'profile' && c.args[1] === 'discover')!
    expect(call.args).toEqual(expect.arrayContaining(['--capability', '触达纺织业客户群', '--json']))
    expect(hits).toHaveLength(1)
    expect(hits[0]!.npub).toBe('npub1bob')
  })
})
