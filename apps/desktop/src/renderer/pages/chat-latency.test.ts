import { describe, it, expect } from 'vitest'
import { formatReplySuffix, extractServerReported } from './chat-latency'

describe('formatReplySuffix', () => {
  it('shows model, tier (local), and only 总计 when firstTokenMs is null (non-streaming)', () => {
    expect(
      formatReplySuffix({ modelName: 'Qwen3.6-35B-A3B-MLX-8bit', tier: 'local', firstTokenMs: null, totalMs: 1834 }),
    ).toBe('Qwen3.6-35B-A3B-MLX-8bit · 本地 · 总计 1,834 ms')
  })

  it('shows 远端 for a remote tier', () => {
    expect(formatReplySuffix({ modelName: 'gpt-x', tier: 'remote', firstTokenMs: null, totalMs: 500 })).toBe(
      'gpt-x · 远端 · 总计 500 ms',
    )
  })

  it('shows both 首字 and 总计 when they differ (a genuinely streamed reply)', () => {
    expect(
      formatReplySuffix({ modelName: 'Qwen3.6-35B-A3B-MLX-8bit', tier: 'local', firstTokenMs: 412, totalMs: 1834 }),
    ).toBe('Qwen3.6-35B-A3B-MLX-8bit · 本地 · 首字 412 ms · 总计 1,834 ms')
  })

  it('omits 首字 when it equals 总计 (non-streaming: first token IS the whole reply)', () => {
    expect(
      formatReplySuffix({ modelName: 'm', tier: 'local', firstTokenMs: 900, totalMs: 900 }),
    ).toBe('m · 本地 · 总计 900 ms')
  })

  it('omits the model/tier segments entirely when unknown, keeping just 总计', () => {
    expect(formatReplySuffix({ modelName: null, tier: null, firstTokenMs: null, totalMs: 250 })).toBe('总计 250 ms')
  })

  it('omits tier alone when only the model name is known', () => {
    expect(formatReplySuffix({ modelName: 'm', tier: null, firstTokenMs: null, totalMs: 250 })).toBe(
      'm · 总计 250 ms',
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
