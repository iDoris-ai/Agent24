import { describe, it, expect } from 'vitest'
import { Bridge, parseDecision, type SessionStore } from './bridge.js'
import type { Agent24Client, PendingApproval, RunResult } from './agent24.js'
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

  interface Behavior {
    runToCompletion?: () => Promise<RunResult>
    awaitRun?: () => Promise<RunResult>
    pendingApprovals?: () => Promise<PendingApproval[]>
    decide?: (id: string, decision: string) => Promise<boolean>
  }
  interface Fakes {
    bridge: Bridge
    calls: {
      createSession: number
      runToCompletion: number
      decide: { id: string; decision: string }[]
      sends: string[]
    }
  }

  function makeBridge(allowed: string[], behavior: Behavior = {}): Fakes {
    const calls = {
      createSession: 0,
      runToCompletion: 0,
      decide: [] as { id: string; decision: string }[],
      sends: [] as string[],
    }
    const agent = {
      async createSession(): Promise<string> {
        calls.createSession++
        return `session-${calls.createSession}`
      },
      async runToCompletion(): Promise<RunResult> {
        calls.runToCompletion++
        return behavior.runToCompletion
          ? behavior.runToCompletion()
          : { status: 'completed', text: 'ok', runId: 'r1' }
      },
      async awaitRun(): Promise<RunResult> {
        return behavior.awaitRun ? behavior.awaitRun() : { status: 'completed', text: 'done', runId: 'r1' }
      },
      async pendingApprovals(): Promise<PendingApproval[]> {
        return behavior.pendingApprovals ? behavior.pendingApprovals() : []
      },
      async decide(id: string, decision: string): Promise<boolean> {
        calls.decide.push({ id, decision })
        return behavior.decide ? behavior.decide(id, decision) : true
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
    const { bridge, calls } = makeBridge(['alice'], {
      runToCompletion: async () => {
        // Hold the first run open so the second message would race sessionFor if
        // handling weren't serialized.
        if (!firstRunStarted) {
          firstRunStarted = true
          await gate
        }
        return { status: 'completed', text: 'ok', runId: 'r' }
      },
    })
    const p1 = bridge.handle(textMsg('alice', 'one'))
    const p2 = bridge.handle(textMsg('alice', 'two'))
    release()
    await Promise.all([p1, p2])
    expect(calls.runToCompletion).toBe(2)
    expect(calls.createSession).toBe(1) // reused, created exactly once
  })

  it('replies with an error instead of going silent when a daemon call throws', async () => {
    const { bridge, calls } = makeBridge(['alice'], {
      runToCompletion: async () => {
        throw new Error('daemon down')
      },
    })
    await bridge.handle(textMsg('alice', '做点事'))
    // The user is never left without a reply; the error surfaces.
    expect(calls.sends.some((s) => s.includes('处理出错') && s.includes('daemon down'))).toBe(true)
  })

  it('queues parked approvals and resolves them FIFO', async () => {
    const approval: PendingApproval = {
      id: 'ap1',
      run_id: 'r1',
      summary: '删除 ~/notes.txt',
      available_decisions: ['approve', 'deny'],
    }
    const { bridge, calls } = makeBridge(['alice'], {
      runToCompletion: async () => ({ status: 'awaiting_approval', runId: 'r1' }),
      pendingApprovals: async () => [approval],
      awaitRun: async () => ({ status: 'completed', text: '已删除', runId: 'r1' }),
    })

    // First message parks on approval — the summary is surfaced to the user.
    await bridge.handle(textMsg('alice', '删掉笔记'))
    expect(calls.sends.some((s) => s.includes('需要你批准') && s.includes('删除 ~/notes.txt'))).toBe(true)

    // A non-decision reply nudges (with count + summary), without deciding.
    await bridge.handle(textMsg('alice', '在吗'))
    expect(calls.sends.some((s) => s.includes('1 条待批准') && s.includes('删除 ~/notes.txt'))).toBe(true)
    expect(calls.decide).toHaveLength(0)

    // "y" resolves the oldest approval and delivers the resumed run's outcome.
    await bridge.handle(textMsg('alice', 'y'))
    expect(calls.decide).toEqual([{ id: 'ap1', decision: 'approve' }])
    expect(calls.sends.some((s) => s.includes('已批准'))).toBe(true)
    expect(calls.sends.some((s) => s.includes('已删除'))).toBe(true)
  })
})
