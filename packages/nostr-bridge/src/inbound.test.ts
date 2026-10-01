import { describe, it, expect, vi } from 'vitest'
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

  it('a transient failure does NOT consume the event — a later poll retries it', async () => {
    const fake = new FakeSpeaker()
    let attempts = 0
    const { b, calls } = bridge(fake, ['npub1alice'], () => {
      attempts++
      if (attempts === 1) throw new Error('daemon down') // transient
      return { status: 'completed', text: 'ok', runId: 'r' }
    })
    const m = inbound('npub1alice', 'hi', 'ev-transient')
    await b.handle(m) // attempt 1 throws → event un-committed
    await b.handle(m) // retried → succeeds
    expect(attempts).toBe(2)
    expect(calls.prompts).toHaveLength(2)
  })

  it('cancelled: tells the peer the request was cancelled (not "still processing")', async () => {
    const fake = new FakeSpeaker()
    const { b } = bridge(fake, ['npub1alice'], () => ({ status: 'cancelled', runId: 'r' }))
    await b.handle(inbound('npub1alice', 'hi'))
    const reply = fake.lastContent()!
    expect((reply.payload as { text: string }).text).toContain('取消')
  })

  it('awaiting_approval: tells the peer it needs the owner to approve', async () => {
    const fake = new FakeSpeaker()
    const { b } = bridge(fake, ['npub1alice'], () => ({ status: 'awaiting_approval', runId: 'r' }))
    await b.handle(inbound('npub1alice', 'rm -rf something'))
    const reply = fake.lastContent()!
    expect(reply.intent).toBe('answer')
    expect((reply.payload as { text: string }).text).toContain('批准')
  })

  it('pollOnce maps StoredMessage rows and feeds authorized ones through', async () => {
    const fake = new FakeSpeaker()
    // real agent-speaker StoredMessage shape (sender_npub / plaintext / id /
    // is_incoming), verified in 联调
    fake.inboxRows = [
      { sender_npub: 'npub1alice', plaintext: 'one', id: 'a', is_incoming: true },
      { sender_npub: 'npub1mallory', plaintext: 'two', id: 'b', is_incoming: true }, // dropped
      { sender_npub: 'npub1alice', plaintext: 'mine', id: 'c', is_incoming: false }, // outbound, skipped
    ]
    const { b, calls } = bridge(fake, ['npub1alice'])
    await pollOnce(new SpeakerClient(fake.runner), b, undefined, { dispatchEnabled: true })
    expect(calls.prompts).toEqual(['one'])
  })

  it('history inbox: full sender_npub matches the allowlist; garbage id synthesizes a stable dedup key', async () => {
    const fake = new FakeSpeaker()
    // real `history inbox --json` StoredMessage: full sender_npub (so the
    // fail-closed allowlist can actually match) + a non-hex garbage id
    // (agent-speaker bug) → the bridge synthesizes a stable key from
    // sender+created_at+content, and re-polling the same window dedups.
    fake.inboxRows = [
      { sender_npub: 'npub1alice', plaintext: 'hi', id: 'garbage', created_at: 1785318666, is_incoming: true },
    ]
    const { b, calls } = bridge(fake, ['npub1alice'])
    const speaker = new SpeakerClient(fake.runner)
    await pollOnce(speaker, b, undefined, { dispatchEnabled: true })
    await pollOnce(speaker, b, undefined, { dispatchEnabled: true }) // same last-N window again — must NOT re-run
    expect(calls.prompts).toEqual(['hi'])
  })

  it('agent-speaker error envelope surfaces as a thrown error', async () => {
    const fake = new FakeSpeaker()
    fake.nextError = 'keystore is locked'
    await expect(new SpeakerClient(fake.runner).inbox()).rejects.toThrow(/keystore is locked/)
  })

  it('inbox() targets its own identity via `history inbox --as` (no identity-use hijack)', async () => {
    // agent-speaker#30 gave history inbox --as; the bridge reads ITS identity's
    // inbox directly instead of hijacking the keystore default. With no identity
    // set, --as is omitted (reads the default).
    const fake = new FakeSpeaker()
    await new SpeakerClient(fake.runner, { identity: 'f4-me' }).inbox()
    const call = fake.calls.find((c) => c.args[0] === 'history' && c.args[1] === 'inbox')!
    expect(call.args).toEqual(expect.arrayContaining(['--as', 'f4-me']))
    // the bridge must NOT mutate the global default identity
    expect(fake.calls.some((c) => c.args[0] === 'identity' && c.args[1] === 'use')).toBe(false)

    const anon = new FakeSpeaker()
    await new SpeakerClient(anon.runner).inbox()
    expect(anon.calls[0]?.args).not.toContain('--as')
  })

  it('uses the real hex event_id as the dedup key (agent-speaker#30), not a synthesized one', async () => {
    const fake = new FakeSpeaker()
    const hexId = 'c2abfcb7a9fb305dc17cc213d10e2f8bd81d411658ffa027b303d1d62d8f6a7e'
    fake.inboxRows = [
      { sender_npub: 'npub1alice', plaintext: 'hi', id: hexId, created_at: 1785318666, is_incoming: true },
    ]
    const [msg] = await new SpeakerClient(fake.runner).inbox()
    expect(msg?.event_id).toBe(hexId) // real id, not the sha1 synth fallback
  })

  // COMM-5a: F4b is frozen by default — inbound dispatch into a gated run is
  // held unless A24_NOSTR_F4B_INBOUND=1. pollOnce must still poll and still
  // feed `liveness.observe()` (FU-32's canary depends on it), it just must not
  // call through to `runToCompletion` for a whitelisted sender's message.
  describe('COMM-5a: F4b inbound dispatch freeze', () => {
    it('flag unset (default): liveness still observes the poll, but the whitelisted message does NOT trigger a run', async () => {
      const fake = new FakeSpeaker()
      fake.inboxRows = [{ sender_npub: 'npub1alice', plaintext: 'hi', id: 'e1', is_incoming: true }]
      const { b, calls } = bridge(fake, ['npub1alice'])
      const observeSpy = vi.fn((rows: InboundMessage[]) => rows)
      const liveness = { observe: observeSpy, ready: true }
      // dispatchEnabled: false is what main.ts passes when A24_NOSTR_F4B_INBOUND
      // is unset — this is the production default (COMM-5a).
      await pollOnce(new SpeakerClient(fake.runner), b, liveness, { dispatchEnabled: false })
      expect(observeSpy).toHaveBeenCalledTimes(1) // canary liveness probe still confirmed
      expect(calls.prompts).toHaveLength(0) // but no run was dispatched
    })

    it('A24_NOSTR_F4B_INBOUND=1 (legacy): the same instrumentation now counts exactly one run', async () => {
      const fake = new FakeSpeaker()
      fake.inboxRows = [{ sender_npub: 'npub1alice', plaintext: 'hi', id: 'e1', is_incoming: true }]
      const { b, calls } = bridge(fake, ['npub1alice'])
      const observeSpy = vi.fn((rows: InboundMessage[]) => rows)
      const liveness = { observe: observeSpy, ready: true }
      await pollOnce(new SpeakerClient(fake.runner), b, liveness, { dispatchEnabled: true })
      expect(observeSpy).toHaveBeenCalledTimes(1)
      expect(calls.prompts).toHaveLength(1)
    })

    // R2's design concern on #613: `dispatchEnabled` used to default to `true`
    // inside `pollOnce`'s own signature, so "frozen by default" held only
    // because `main.ts` happened to thread the flag through — a caller that
    // omitted the 4th argument entirely (no options object at all) silently
    // got the pre-freeze dispatch-every-message behavior, and nothing asserted
    // the production default. This is the structural guarantee: no options
    // (not even an empty `{}`) must still freeze dispatch.
    it('structural guarantee: calling pollOnce with NO options object at all still freezes dispatch', async () => {
      const fake = new FakeSpeaker()
      fake.inboxRows = [{ sender_npub: 'npub1alice', plaintext: 'hi', id: 'e1', is_incoming: true }]
      const { b, calls } = bridge(fake, ['npub1alice'])
      await pollOnce(new SpeakerClient(fake.runner), b) // no liveness, no options
      expect(calls.prompts).toHaveLength(0)
    })

    // R2 finding #1 on #613: while frozen, a message from a sender NOT on the
    // allowlist was never going to dispatch anyway — that's `handle()`'s own
    // fail-closed rejection, not an "F4b frozen" skip, and it must not log as
    // one.
    it('while frozen, an unauthorized sender gets the unauthorized warning, not the frozen-skip log', async () => {
      const fake = new FakeSpeaker()
      fake.inboxRows = [{ sender_npub: 'npub1mallory', plaintext: 'hi', id: 'e1', is_incoming: true }]
      const { b, calls } = bridge(fake, ['npub1alice']) // mallory is NOT allowlisted
      const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {})
      const logSpy = vi.spyOn(console, 'log').mockImplementation(() => {})
      try {
        await pollOnce(new SpeakerClient(fake.runner), b, undefined, { dispatchEnabled: false })
        expect(calls.prompts).toHaveLength(0)
        expect(warnSpy).toHaveBeenCalledWith(expect.stringContaining('忽略未授权'))
        expect(logSpy).not.toHaveBeenCalledWith(expect.stringContaining('F4b 入站执行已冻结'))
      } finally {
        warnSpy.mockRestore()
        logSpy.mockRestore()
      }
    })

    // Item 3: unfreezing must not replay the backlog that piled up while
    // frozen as if it were fresh traffic.
    it('unfreezing (dispatchSinceSec set): a message older than the unfreeze moment is skipped, a newer one dispatches', async () => {
      const fake = new FakeSpeaker()
      const unfrozenAt = 1_700_000_000
      fake.inboxRows = [
        { sender_npub: 'npub1alice', plaintext: 'old-backlog', id: 'old1', created_at: unfrozenAt - 3600, is_incoming: true },
        { sender_npub: 'npub1alice', plaintext: 'fresh', id: 'new1', created_at: unfrozenAt + 5, is_incoming: true },
      ]
      const { b, calls } = bridge(fake, ['npub1alice'])
      await pollOnce(new SpeakerClient(fake.runner), b, undefined, {
        dispatchEnabled: true,
        dispatchSinceSec: unfrozenAt,
      })
      expect(calls.prompts).toEqual(['fresh'])
    })

    it('unfreezing without dispatchSinceSec dispatches everything as before (no guard requested)', async () => {
      const fake = new FakeSpeaker()
      fake.inboxRows = [
        { sender_npub: 'npub1alice', plaintext: 'old-backlog', id: 'old1', created_at: 1, is_incoming: true },
      ]
      const { b, calls } = bridge(fake, ['npub1alice'])
      await pollOnce(new SpeakerClient(fake.runner), b, undefined, { dispatchEnabled: true })
      expect(calls.prompts).toEqual(['old-backlog'])
    })
  })
})
