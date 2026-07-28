import { describe, it, expect } from 'vitest'
import { Bridge, parseDecision, type SessionStore } from './bridge.js'
import type { Agent24Client, RunResult } from './agent24.js'
import type { Sender } from './ilink/sender.js'
import { parseAllowedUids } from './config.js'
import { splitText } from './ilink/sender.js'
import { messageText, MessageItemType, MessageType, MessageState, type WeixinMessage } from './ilink/types.js'

describe('parseDecision', () => {
  it('maps affirmatives to approve', () => {
    for (const t of ['y', 'YES', ' 批准 ', '同意', 'ok', '通过']) {
      expect(parseDecision(t)).toBe('approve')
    }
  })
  it('maps negatives to deny', () => {
    for (const t of ['n', 'No', '拒绝', '不', '取消']) {
      expect(parseDecision(t)).toBe('deny')
    }
  })
  it('returns null for anything else (treated as a new message)', () => {
    expect(parseDecision('帮我查一下天气')).toBeNull()
    expect(parseDecision('yesterday')).toBeNull()
  })
})

describe('splitText', () => {
  it('keeps short text in one chunk', () => {
    expect(splitText('hi', 1800)).toEqual(['hi'])
  })
  it('splits over the limit', () => {
    const chunks = splitText('x'.repeat(4000), 1800)
    expect(chunks.length).toBe(3)
    expect(chunks.join('')).toBe('x'.repeat(4000))
  })
})

describe('messageText', () => {
  function msg(items: WeixinMessage['item_list']): WeixinMessage {
    return {
      message_id: 1,
      from_user_id: 'u',
      to_user_id: 'bot',
      client_id: 'c',
      create_time_ms: 0,
      message_type: MessageType.USER,
      message_state: MessageState.FINISH,
      context_token: 'ctx',
      item_list: items,
    }
  }
  it('concatenates text items and ignores non-text', () => {
    const m = msg([
      { type: MessageItemType.TEXT, text_item: { text: 'hello' } },
      { type: MessageItemType.IMAGE },
      { type: MessageItemType.TEXT, text_item: { text: 'world' } },
    ])
    expect(messageText(m)).toBe('hello\nworld')
  })
  it('is empty for a message with no text', () => {
    expect(messageText(msg([{ type: MessageItemType.IMAGE }]))).toBe('')
  })
})

describe('parseAllowedUids', () => {
  it('splits on commas and whitespace, dropping blanks', () => {
    expect([...parseAllowedUids('a, b ,,c\n d')]).toEqual(['a', 'b', 'c', 'd'])
  })
  it('is empty for undefined / blank (fail-closed)', () => {
    expect(parseAllowedUids(undefined).size).toBe(0)
    expect(parseAllowedUids('   ').size).toBe(0)
  })
})

describe('Bridge authorization + serialization', () => {
  function textMsg(from: string, text: string): WeixinMessage {
    return {
      message_id: 1,
      from_user_id: from,
      to_user_id: 'bot',
      client_id: 'c',
      create_time_ms: 0,
      message_type: MessageType.USER,
      message_state: MessageState.FINISH,
      context_token: 'ctx',
      item_list: [{ type: MessageItemType.TEXT, text_item: { text } }],
    }
  }

  interface Fakes {
    bridge: Bridge
    calls: { createSession: number; runToCompletion: number; sends: string[] }
  }

  function makeBridge(allowed: string[], onRun?: () => Promise<RunResult>): Fakes {
    const calls = { createSession: 0, runToCompletion: 0, sends: [] as string[] }
    const agent = {
      async createSession(): Promise<string> {
        calls.createSession++
        return `session-${calls.createSession}`
      },
      async runToCompletion(): Promise<RunResult> {
        calls.runToCompletion++
        return onRun ? onRun() : { status: 'completed', text: 'ok', runId: 'r1' }
      },
    } as unknown as Agent24Client
    const sender = {
      async send(_user: string, _ctx: string, text: string): Promise<void> {
        calls.sends.push(text)
      },
    } as unknown as Sender
    const store: SessionStore = { load: () => new Map(), save: () => {} }
    const bridge = new Bridge(agent, sender, store, new Set(allowed))
    return { bridge, calls }
  }

  it('drops messages from unauthorized users without running or replying', async () => {
    const { bridge, calls } = makeBridge(['alice'])
    await bridge.handle(textMsg('mallory', 'rm -rf ~'))
    expect(calls.runToCompletion).toBe(0)
    expect(calls.createSession).toBe(0)
    expect(calls.sends).toEqual([]) // stranger is never answered
  })

  it('runs for an authorized user', async () => {
    const { bridge, calls } = makeBridge(['alice'])
    await bridge.handle(textMsg('alice', '查一下天气'))
    expect(calls.runToCompletion).toBe(1)
    expect(calls.createSession).toBe(1)
    expect(calls.sends).toContain('ok')
  })

  it('serializes concurrent messages from one user (single session, no TOCTOU)', async () => {
    let release!: () => void
    const gate = new Promise<void>((r) => (release = r))
    let firstRunStarted = false
    const { bridge, calls } = makeBridge(['alice'], async () => {
      // Hold the first run open so the second message would race sessionFor if
      // handling weren't serialized.
      if (!firstRunStarted) {
        firstRunStarted = true
        await gate
      }
      return { status: 'completed', text: 'ok', runId: 'r' }
    })
    const p1 = bridge.handle(textMsg('alice', 'one'))
    const p2 = bridge.handle(textMsg('alice', 'two'))
    release()
    await Promise.all([p1, p2])
    expect(calls.runToCompletion).toBe(2)
    expect(calls.createSession).toBe(1) // reused, created exactly once
  })
})
