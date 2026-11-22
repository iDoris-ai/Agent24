import { beforeEach, describe, it, expect } from 'vitest'
import { formatReplySuffix, extractServerReported } from './chat-latency'
import { setLocale } from '../i18n'

describe('formatReplySuffix English default', () => {
  it('shows English tier and timing labels', () => {
    expect(formatReplySuffix({ modelName: 'model', tier: 'local', firstTokenMs: null, totalMs: 250 }))
      .toBe('model · Local · Took 0.3 s')
  })
})

describe('formatReplySuffix', () => {
  beforeEach(() => { setLocale('zh') })
  it('shows model, tier (local), and only 总计 when firstTokenMs is null (non-streaming)', () => {
    expect(
      formatReplySuffix({ modelName: 'Qwen3.6-35B-A3B-MLX-8bit', tier: 'local', firstTokenMs: null, totalMs: 1834 }),
    ).toBe('Qwen3.6-35B-A3B-MLX-8bit · 本地 · 用时 1.8 秒')
  })

  it('shows 远端 for a remote tier', () => {
    expect(formatReplySuffix({ modelName: 'gpt-x', tier: 'remote', firstTokenMs: null, totalMs: 500 })).toBe(
      'gpt-x · 远端 · 用时 0.5 秒',
    )
  })

  it('shows both 首字 and 总计 when they differ (a genuinely streamed reply)', () => {
    expect(
      formatReplySuffix({ modelName: 'Qwen3.6-35B-A3B-MLX-8bit', tier: 'local', firstTokenMs: 412, totalMs: 1834 }),
    ).toBe('Qwen3.6-35B-A3B-MLX-8bit · 本地 · 首字 0.4 秒 · 用时 1.8 秒')
  })

  it('omits 首字 when it equals 总计 (non-streaming: first token IS the whole reply)', () => {
    expect(
      formatReplySuffix({ modelName: 'm', tier: 'local', firstTokenMs: 900, totalMs: 900 }),
    ).toBe('m · 本地 · 用时 0.9 秒')
  })

  it('omits the model/tier segments entirely when unknown, keeping just 总计', () => {
    expect(formatReplySuffix({ modelName: null, tier: null, firstTokenMs: null, totalMs: 250 })).toBe('用时 0.3 秒')
  })

  // UI polish 02 (David): 「总计 132,315 ms」 read as debug output.
  it('says a long wait in minutes and seconds, a short one in seconds', () => {
    const took = (ms: number) => formatReplySuffix({ modelName: null, tier: null, firstTokenMs: null, totalMs: ms })
    expect(took(132_315)).toBe('用时 2 分 12 秒')
    expect(took(42_400)).toBe('用时 42 秒')
    expect(took(9_960)).toBe('用时 10 秒')
    expect(took(60_000)).toBe('用时 1 分 0 秒')
    expect(took(59_700)).toBe('用时 1 分 0 秒')
  })

  it('omits tier alone when only the model name is known', () => {
    expect(formatReplySuffix({ modelName: 'm', tier: null, firstTokenMs: null, totalMs: 250 })).toBe(
      'm · 用时 0.3 秒',
    )
  })
})

describe('extractServerReported', () => {
  it('reads model_id/tier/latency_ms/first_token_ms when present and well-typed', () => {
    expect(
      extractServerReported({ model_id: 'm', tier: 'remote', latency_ms: 1200, first_token_ms: 300 }),
    ).toEqual({ modelId: 'm', tier: 'remote', totalMs: 1200, firstTokenMs: 300 })
  })

  it('treats today\'s actual ChatResponse shape ({message, usage}) as fully absent', () => {
    expect(extractServerReported({ message: { role: 'assistant', content: 'hi' }, usage: { total_tokens: 5 } })).toEqual(
      { modelId: null, tier: null, totalMs: null, firstTokenMs: null },
    )
  })

  it('never throws on null/non-object data, and ignores a wrongly-typed field', () => {
    expect(extractServerReported(null)).toEqual({ modelId: null, tier: null, totalMs: null, firstTokenMs: null })
    expect(extractServerReported({ tier: 'orbital' })).toEqual({
      modelId: null,
      tier: null,
      totalMs: null,
      firstTokenMs: null,
    })
  })
})
