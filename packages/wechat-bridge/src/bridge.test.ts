import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, it, expect } from 'vitest'
import { Bridge, RECORD_FAILED_REPLY, RECORDED_REPLY, canonicalStamp } from './bridge.js'
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

  it('records a finished user message and replies only 已记录', async () => {
    const { bridge, recorded, sends } = makeBridge()
    await bridge.handle(textMsg('alice', '查一下天气'))
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
    await bridge.handle(imageMsg('alice'))
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
    await bridge.handle(textMsg('alice', '做点事'))
    expect(sends).toEqual([RECORD_FAILED_REPLY])
    expect(sends.join('')).not.toContain('daemon down')
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
