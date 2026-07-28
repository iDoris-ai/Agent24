import { describe, it, expect } from 'vitest'
import { parseDecision } from './bridge.js'
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
