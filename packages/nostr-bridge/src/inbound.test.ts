import { describe, it, expect } from 'vitest'
import { InboundBridge, envelopeToPrompt, pollOnce } from './inbound.js'
import { SpeakerClient, type InboundMessage } from './speaker.js'
import { Agent24Client, type RunResult } from './agent24.js'
import { FakeSpeaker } from './testing/fake-speaker.js'
import { makeContent } from './protocol.js'

interface FakeAgentCalls {
  createSession: number
  prompts: string[]
}

function fakeAgent(onRun?: (prompt: string) => RunResult): { agent: Agent24Client; calls: FakeAgentCalls } {
  const calls: FakeAgentCalls = { createSession: 0, prompts: [] }
  const agent = {
    async createSession(): Promise<string> {
      calls.createSession++
      return `sess-${calls.createSession}`
    },
    async runToCompletion(prompt: string): Promise<RunResult> {
      calls.prompts.push(prompt)
      return onRun ? onRun(prompt) : { status: 'completed', text: `echo:${prompt}`, runId: 'r1' }
    },
  } as unknown as Agent24Client
  return { agent, calls }
}

function bridge(fake: FakeSpeaker, allowed: string[], onRun?: (p: string) => RunResult) {
  const { agent, calls } = fakeAgent(onRun)
  const b = new InboundBridge(agent, new SpeakerClient(fake.runner), 'me', new Set(allowed))
  return { b, calls }
}

function inbound(from: string, content: string, eventId = 'e1'): InboundMessage {
  return { from, content, event_id: eventId }
}

describe('envelopeToPrompt', () => {
  it('tags an F4 envelope with its intent and pulls a readable body', () => {
    const env = JSON.stringify(makeContent({ intent: 'ask', payload: { q: '能接纺织单吗' } }))
    const { prompt, envelope } = envelopeToPrompt(env)
    expect(prompt).toContain('intent:ask')
    expect(prompt).toContain('能接纺织单吗')
    expect(envelope?.intent).toBe('ask')
  })
  it('passes raw non-envelope content through as the prompt', () => {
    const { prompt, envelope } = envelopeToPrompt('just some text')
    expect(prompt).toBe('just some text')
    expect(envelope).toBeUndefined()
  })
})

describe('InboundBridge (gated run + reply, fail-closed allowlist)', () => {
  it('drops messages from an unauthorized npub — no run, no reply', async () => {
    const fake = new FakeSpeaker()
    const { b, calls } = bridge(fake, ['npub1alice'])
    await b.handle(inbound('npub1mallory', 'do something dangerous'))
    expect(calls.prompts).toHaveLength(0)
    expect(calls.createSession).toBe(0)
    expect(fake.calls.some((c) => c.args[0] === 'agent' && c.args[1] === 'msg')).toBe(false)
  })

  it('authorized: runs the peer message in a per-npub session and answers back', async () => {
    const fake = new FakeSpeaker()
    const env = JSON.stringify(makeContent({ intent: 'ask', threadId: 't-1', payload: { q: 'ping' } }))
    const { b, calls } = bridge(fake, ['npub1alice'])
    await b.handle({ from: 'npub1alice', content: env, event_id: 'e9' })

    expect(calls.createSession).toBe(1)
    expect(calls.prompts[0]).toContain('ping')
    // replied as an 'answer', threaded off the inbound envelope, back to sender
    const msg = fake.calls.find((c) => c.args[0] === 'agent' && c.args[1] === 'msg')!
    expect(msg.args).toEqual(expect.arrayContaining(['--to', 'npub1alice']))
    const reply = fake.lastContent()!
    expect(reply.intent).toBe('answer')
    expect(reply.thread_id).toBe('t-1')
    expect(reply.reply_to).toBe('e9')
    expect((reply.payload as { text: string }).text).toContain('echo:')
  })

  it('dedups by event_id — the same message runs at most once', async () => {
    const fake = new FakeSpeaker()
    const { b, calls } = bridge(fake, ['npub1alice'])
    const m = inbound('npub1alice', 'hi', 'dup-1')
    await b.handle(m)
    await b.handle(m)
    expect(calls.prompts).toHaveLength(1)
  })

  it('awaiting_approval: tells the peer it needs the owner to approve', async () => {
    const fake = new FakeSpeaker()
    const { b } = bridge(fake, ['npub1alice'], () => ({ status: 'awaiting_approval', runId: 'r' }))
    await b.handle(inbound('npub1alice', 'rm -rf something'))
    const reply = fake.lastContent()!
    expect(reply.intent).toBe('answer')
    expect((reply.payload as { text: string }).text).toContain('批准')
  })

  it('pollOnce feeds every inbox message through the bridge', async () => {
    const fake = new FakeSpeaker()
    fake.inboxMessages = [
      { from: 'npub1alice', content: 'one', event_id: 'a' },
      { from: 'npub1mallory', content: 'two', event_id: 'b' }, // dropped
    ]
    const { b, calls } = bridge(fake, ['npub1alice'])
    await pollOnce(new SpeakerClient(fake.runner), b)
    expect(calls.prompts).toEqual(['one'])
  })
})
