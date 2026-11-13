import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, it, expect } from 'vitest'
import { Bridge, DEV_ROUTE_MISSING, FAILURE_REPLY_LIMIT, HOLD_GIVE_UP, RECORD_FAILED_REPLY, RECORDED_REPLY, canonicalStamp, classifyInboundFailure, holdCountsTowardGiveUp, inboundStamp } from './bridge.js'
import { InboundHttpError } from './agent24.js'
import type { Agent24Client, InboundEvent, InboundRecord } from './agent24.js'
import type { Sender } from './ilink/sender.js'
import { splitText } from './ilink/sender.js'
import { messageText, MessageItemType, MessageType, MessageState, type WeixinMessage } from './ilink/types.js'

describe('production sources do not post a run', () => {
  it('has no /api/v1/runs under src, outside tests', () => {
    const root = fileURLToPath(new URL('.', import.meta.url))
    const files = productionSources(root)
    expect(files.length).toBeGreaterThan(0)
    const forbidden = '/api/v1/' + 'runs'
    for (const file of files) {
      expect(readFileSync(file, 'utf8'), file).not.toContain(forbidden)
    }
  })

  it('does not read a legacy bearer', () => {
    const root = fileURLToPath(new URL('.', import.meta.url))
    const banned = ['A24_TOKEN', 'requireDaemonBearer', 'daemonBearer']
    for (const file of productionSources(root)) {
      const text = readFileSync(file, 'utf8')
      for (const word of banned) expect(text, `${file} ${word}`).not.toContain(word)
    }
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

describe('canonicalStamp', () => {
  it('drops fractional seconds', () => {
    expect(canonicalStamp(Date.parse('2026-11-12T03:04:05.678Z'))).toBe('2026-11-12T03:04:05Z')
  })
})

describe('inboundStamp', () => {
  it('rejects a missing, non-finite, or out-of-range time', () => {
    expect(inboundStamp(undefined)).toBeNull()
    expect(inboundStamp(Number.NaN)).toBeNull()
    expect(inboundStamp(Number.POSITIVE_INFINITY)).toBeNull()
    expect(inboundStamp(1e20)).toBeNull()
    expect(inboundStamp(8.64e15 + 1)).toBeNull()
    expect(inboundStamp(-8.64e15 - 1)).toBeNull()
    expect(inboundStamp(0)).toBe('1970-01-01T00:00:00Z')
    expect(inboundStamp(8.64e15)).toBe(canonicalStamp(8.64e15))
  })
})

describe('Bridge transport', () => {
  function textMsg(
    from: string,
    text: string,
    patch: Partial<WeixinMessage> = {},
  ): WeixinMessage {
    return {
      message_id: 42,
      from_user_id: from,
      to_user_id: 'bot',
      client_id: 'c',
      create_time_ms: Date.parse('2026-11-12T03:04:05.678Z'),
      message_type: MessageType.USER,
      message_state: MessageState.FINISH,
      context_token: 'ctx',
      item_list: [{ type: MessageItemType.TEXT, text_item: { text } }],
      ...patch,
    }
  }

  interface Fakes {
    bridge: Bridge
    recorded: InboundEvent[]
    sends: string[]
  }

  function imageMsg(from: string): WeixinMessage {
    return textMsg(from, '', { item_list: [{ type: MessageItemType.IMAGE }] })
  }

  function makeBridge(fail?: Error, outcome: InboundRecord['outcome'] = 'started', reason?: string): Fakes {
    const recorded: InboundEvent[] = []
    const sends: string[] = []
    const agent = {
      async recordInbound(event: InboundEvent): Promise<InboundRecord> {
        if (fail) throw fail
        recorded.push(event)
        return { id: event.id, outcome, ...(reason ? { reason } : {}) }
      },
    } as unknown as Agent24Client
    const sender = {
      async send(_user: string, _ctx: string, text: string): Promise<void> {
        sends.push(text)
      },
    } as unknown as Sender
    return { bridge: new Bridge(agent, sender), recorded, sends }
  }

  function holdsOf(bridge: Bridge): Map<string, number> {
    return (bridge as unknown as { holds: Map<string, number> }).holds
  }

  it('records a finished user message and replies only 已记录', async () => {
    const { bridge, recorded, sends } = makeBridge()
    await expect(bridge.handle(textMsg('alice', '查一下天气'))).resolves.toBe('advance')
    expect(recorded).toEqual([
      {
        id: 'wechat:42',
        actor: 'alice',
        at: '2026-11-12T03:04:05Z',
        text: '查一下天气',
      },
    ])
    expect(sends).toEqual([RECORDED_REPLY])
  })

  it('records y as ordinary text and does not approve anything', async () => {
    const { bridge, recorded, sends } = makeBridge()
    await bridge.handle(textMsg('alice', 'y'))
    expect(recorded.map((event) => event.text)).toEqual(['y'])
    expect(sends).toEqual([RECORDED_REPLY])
  })

  it('records a stranger and does not reply when the daemon ignores them', async () => {
    const { bridge, recorded, sends } = makeBridge(undefined, 'ignored', 'not_allowlisted')
    await bridge.handle(textMsg('mallory', 'rm -rf ~'))
    expect(recorded.map((event) => event.actor)).toEqual(['mallory'])
    expect(sends).toEqual([])
  })

  it.each(['stale', 'rate_limited', 'no_rule', 'bot'])(
    'does not reply when the daemon ignores for %s',
    async (reason) => {
      const { bridge, recorded, sends } = makeBridge(undefined, 'ignored', reason)
      await bridge.handle(textMsg('alice', '再来一条'))
      expect(recorded).toHaveLength(1)
      expect(sends).toEqual([])
    },
  )

  it('does not reply to a duplicate, which may replay an off-list message', async () => {
    const { bridge, recorded, sends } = makeBridge(undefined, 'duplicate')
    await bridge.handle(textMsg('mallory', '查一下天气'))
    expect(recorded).toHaveLength(1)
    expect(sends).toEqual([])
  })

  it('does not reply when the daemon says the event is already linked', async () => {
    const { bridge, recorded, sends } = makeBridge(undefined, 'already_linked')
    await bridge.handle(textMsg('alice', '查一下天气'))
    expect(recorded).toHaveLength(1)
    expect(sends).toEqual([])
  })

  it('does not post or reply to a picture or blank text', async () => {
    const { bridge, recorded, sends } = makeBridge()
    await expect(bridge.handle(imageMsg('alice'))).resolves.toBe('advance')
    await bridge.handle(textMsg('alice', '   '))
    await bridge.handle(textMsg('alice', '', { item_list: [] }))
    expect(recorded).toEqual([])
    expect(sends).toEqual([])
  })

  it('does not record GENERATING or BOT messages', async () => {
    const { bridge, recorded, sends } = makeBridge()
    await bridge.handle(textMsg('alice', 'partial', { message_state: MessageState.GENERATING }))
    await bridge.handle(textMsg('alice', 'bot said', { message_type: MessageType.BOT }))
    await bridge.handle(
      textMsg('alice', 'both', {
        message_state: MessageState.GENERATING,
        message_type: MessageType.BOT,
      }),
    )
    expect(recorded).toEqual([])
    expect(sends).toEqual([])
  })

  it('replies with a fixed failure line and does not echo the error', async () => {
    const { bridge, sends } = makeBridge(new Error('daemon down'))
    await expect(bridge.handle(textMsg('alice', '做点事'))).resolves.toBe('hold')
    expect(sends).toEqual([RECORD_FAILED_REPLY])
    expect(sends.join('')).not.toContain('daemon down')
  })

  it('stops on 401 without the failure reply', async () => {
    const { bridge, sends } = makeBridge(new InboundHttpError(401))
    await expect(bridge.handle(textMsg('alice', '做点事'))).resolves.toBe('stop')
    expect(sends).toEqual([])
  })

  it('advances on 400 after one failure reply', async () => {
    const { bridge, sends } = makeBridge(new InboundHttpError(400))
    await expect(bridge.handle(textMsg('alice', '做点事'))).resolves.toBe('advance')
    expect(sends).toEqual([RECORD_FAILED_REPLY])
  })

  it('stops on 404 without advancing or the failure reply', async () => {
    const errors: unknown[] = []
    const previous = console.error
    console.error = (...args: unknown[]) => {
      errors.push(args.join(' '))
    }
    try {
      const { bridge, sends } = makeBridge(new InboundHttpError(404))
      await expect(bridge.handle(textMsg('alice', '做点事'))).resolves.toBe('dev_tool')
      expect(sends).toEqual([])
      expect(errors.join('\n')).toContain(DEV_ROUTE_MISSING)
      expect(classifyInboundFailure(new InboundHttpError(408))).toBe('hold')
      expect(classifyInboundFailure(new InboundHttpError(400))).toBe('advance')
      expect(classifyInboundFailure(new InboundHttpError(418))).toBe('hold')
    } finally {
      console.error = previous
    }
  })

  it('holds on 500, 429, and a timeout', async () => {
    for (const fail of [new InboundHttpError(500), new InboundHttpError(429)]) {
      const { bridge } = makeBridge(fail)
      await expect(bridge.handle(textMsg('alice', '做点事'))).resolves.toBe('hold')
    }
    const timeout = new Error('The operation was aborted due to timeout')
    timeout.name = 'TimeoutError'
    const { bridge } = makeBridge(timeout)
    await expect(bridge.handle(textMsg('alice', '做点事'))).resolves.toBe('hold')
  })

  it('drops the oldest failure reply past the cap', async () => {
    const { bridge, sends } = makeBridge(new InboundHttpError(400))
    const first = textMsg('alice', '做点事', { message_id: 1 })
    await bridge.handle(first)
    for (let id = 2; id <= FAILURE_REPLY_LIMIT; id++) {
      await bridge.handle(textMsg('alice', '做点事', { message_id: id }))
    }
    expect(sends).toHaveLength(FAILURE_REPLY_LIMIT)
    await bridge.handle(textMsg('alice', '做点事', { message_id: FAILURE_REPLY_LIMIT }))
    expect(sends).toHaveLength(FAILURE_REPLY_LIMIT)
    await bridge.handle(textMsg('alice', '做点事', { message_id: FAILURE_REPLY_LIMIT + 1 }))
    expect(sends).toHaveLength(FAILURE_REPLY_LIMIT + 1)
    await bridge.handle(first)
    expect(sends).toHaveLength(FAILURE_REPLY_LIMIT + 2)
  })

  it.each([Number.NaN, undefined, 1e20, 8.64e15 + 1])(
    'advances a bad create_time_ms (%s) without posting or the failure reply',
    async (create_time_ms) => {
      const errors: unknown[] = []
      const previous = console.error
      console.error = (...args: unknown[]) => {
        errors.push(args.join(' '))
      }
      try {
        const { bridge, recorded, sends } = makeBridge()
        await expect(
          bridge.handle(textMsg('alice', '坏时间', { create_time_ms: create_time_ms as number })),
        ).resolves.toBe('advance')
        expect(recorded).toEqual([])
        expect(sends).toEqual([])
        expect(errors.join('\n')).toContain('create_time_ms 不合法')
      } finally {
        console.error = previous
      }
    },
  )

  it.each([408, 418])(
    'advances after status %s passes the hold ceiling and does not post another failure reply',
    async (status) => {
      const errors: unknown[] = []
      const previous = console.error
      console.error = (...args: unknown[]) => {
        errors.push(args.join(' '))
      }
      try {
        const { bridge, sends } = makeBridge(new InboundHttpError(status))
        const msg = textMsg('alice', '做点事')
        for (let n = 0; n < HOLD_GIVE_UP; n++) {
          await expect(bridge.handle(msg)).resolves.toBe('hold')
        }
        await expect(bridge.handle(msg)).resolves.toBe('advance')
        expect(sends).toEqual([RECORD_FAILED_REPLY])
        expect(holdsOf(bridge).size).toBe(0)
        expect(errors.join('\n')).toContain('放弃并前进')
        expect(holdCountsTowardGiveUp(new InboundHttpError(status))).toBe(true)
      } finally {
        console.error = previous
      }
    },
  )

  it.each([
    ['503', new InboundHttpError(503)],
    ['429', new InboundHttpError(429)],
    ['network', new Error('connect ECONNREFUSED')],
    [
      'timeout',
      Object.assign(new Error('The operation was aborted due to timeout'), { name: 'TimeoutError' }),
    ],
  ])('keeps holding %s past the ceiling', async (_label, fail) => {
    const errors: unknown[] = []
    const previous = console.error
    console.error = (...args: unknown[]) => {
      errors.push(args.join(' '))
    }
    try {
      const { bridge, sends } = makeBridge(fail)
      const msg = textMsg('alice', '做点事')
      for (let n = 0; n <= HOLD_GIVE_UP; n++) {
        await expect(bridge.handle(msg)).resolves.toBe('hold')
      }
      expect(sends).toEqual([RECORD_FAILED_REPLY])
      expect(errors.join('\n')).not.toContain('放弃并前进')
      expect(holdCountsTowardGiveUp(fail)).toBe(false)
    } finally {
      console.error = previous
    }
  })

  it('clears the consecutive count when a 503 lands between 408s', async () => {
    const errors: unknown[] = []
    const previous = console.error
    console.error = (...args: unknown[]) => {
      errors.push(args.join(' '))
    }
    const scripted = [
      ...Array.from({ length: 10 }, () => new InboundHttpError(408)),
      ...Array.from({ length: 3 }, () => new InboundHttpError(503)),
      ...Array.from({ length: 11 }, () => new InboundHttpError(408)),
    ]
    let call = 0
    const sends: string[] = []
    const agent = {
      async recordInbound(): Promise<InboundRecord> {
        const err = scripted[call]
        call += 1
        if (err) throw err
        return { id: 'wechat:42', outcome: 'started' }
      },
    } as unknown as Agent24Client
    const sender = {
      async send(_user: string, _ctx: string, text: string): Promise<void> {
        sends.push(text)
      },
    } as unknown as Sender
    const bridge = new Bridge(agent, sender)
    const msg = textMsg('alice', '做点事')
    try {
      for (let n = 0; n < 10; n++) {
        await expect(bridge.handle(msg)).resolves.toBe('hold')
      }
      expect(holdsOf(bridge).get('42')).toBe(10)
      for (let n = 0; n < 3; n++) {
        await expect(bridge.handle(msg)).resolves.toBe('hold')
      }
      const sizeAfter503 = holdsOf(bridge).size
      for (let n = 0; n < 11; n++) {
        await expect(bridge.handle(msg)).resolves.toBe('hold')
      }
      expect(holdsOf(bridge).get('42')).toBe(11)
      expect(sizeAfter503).toBe(0)
      expect(errors.join('\n')).not.toContain('放弃并前进')
      expect(sends).toEqual([RECORD_FAILED_REPLY])
    } finally {
      console.error = previous
    }
  })

  it.each([400, 403, 413, 422])(
    'drops the hold entry when status %s advances',
    async (status) => {
      let calls = 0
      const sends: string[] = []
      const agent = {
        async recordInbound(): Promise<InboundRecord> {
          calls += 1
          if (calls <= 2) throw new InboundHttpError(408)
          throw new InboundHttpError(status)
        },
      } as unknown as Agent24Client
      const sender = {
        async send(_user: string, _ctx: string, text: string): Promise<void> {
          sends.push(text)
        },
      } as unknown as Sender
      const bridge = new Bridge(agent, sender)
      const msg = textMsg('alice', '做点事', { message_id: status })
      await expect(bridge.handle(msg)).resolves.toBe('hold')
      await expect(bridge.handle(msg)).resolves.toBe('hold')
      expect(holdsOf(bridge).size).toBe(1)
      await expect(bridge.handle(msg)).resolves.toBe('advance')
      expect(holdsOf(bridge).size).toBe(0)
      expect(sends).toEqual([RECORD_FAILED_REPLY])
    },
  )

  it.each([undefined, null])(
    'advances a message with no message_id (%s) without posting or sharing a key',
    async (message_id) => {
      const errors: unknown[] = []
      const previous = console.error
      console.error = (...args: unknown[]) => {
        errors.push(args.join(' '))
      }
      try {
        const { bridge, recorded, sends } = makeBridge()
        const missing = textMsg('alice', '没 id', {
          message_id: message_id as unknown as number,
        })
        await expect(bridge.handle(missing)).resolves.toBe('advance')
        await expect(bridge.handle(missing)).resolves.toBe('advance')
        expect(recorded).toEqual([])
        expect(sends).toEqual([])
        expect(holdsOf(bridge).size).toBe(0)
        expect(errors.join('\n')).toContain('缺少 message_id')
        expect(errors.join('\n')).not.toContain('undefined')
        await expect(bridge.handle(textMsg('alice', '有 id'))).resolves.toBe('advance')
        expect(recorded.map((event) => event.id)).toEqual(['wechat:42'])
      } finally {
        console.error = previous
      }
    },
  )

  it('sends 记录失败 once for the same message id', async () => {
    const { bridge, sends } = makeBridge(new InboundHttpError(500))
    const msg = textMsg('alice', '做点事')
    await expect(bridge.handle(msg)).resolves.toBe('hold')
    await expect(bridge.handle(msg)).resolves.toBe('hold')
    expect(sends).toEqual([RECORD_FAILED_REPLY])
  })

  it('stops on legacy capability mode without the failure reply', async () => {
    const { bridge, sends } = makeBridge(new InboundHttpError(409, 'capability_mode_required'))
    await expect(bridge.handle(textMsg('alice', '做点事'))).resolves.toBe('capabilities')
    expect(sends).toEqual([])
  })
})

function productionSources(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name)
    if (statSync(path).isDirectory()) {
      if (name === 'testing') continue
      productionSources(path, out)
    } else if (name.endsWith('.ts') && !name.endsWith('.test.ts')) {
      out.push(path)
    }
  }
  return out
}
