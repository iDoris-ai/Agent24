import { describe, it, expect, vi } from 'vitest'
import { ModelCallLog, MAX_MODEL_CALLS_IN_MEMORY } from './model-call-log'
import type { ModelCallEnvelope } from '../shared/ipc-types'

function call(overrides: Partial<ModelCallEnvelope> = {}): ModelCallEnvelope {
  return {
    module: 'agentear',
    model_id: 'Qwen3.6-35B-A3B-MLX-8bit',
    tier: 'local',
    served_by: 'omlx',
    ok: true,
    latency_ms: 100,
    prompt_tokens: 10,
    completion_tokens: 5,
    ...overrides,
  }
}

describe('ModelCallLog', () => {
  it('snapshot() is empty before anything is ingested', () => {
    expect(new ModelCallLog().snapshot()).toEqual([])
  })

  it('ingest() appends to the snapshot, oldest first', () => {
    const log = new ModelCallLog()
    log.ingest(call({ model_id: 'a' }))
    log.ingest(call({ model_id: 'b' }))
    expect(log.snapshot().map((c) => c.model_id)).toEqual(['a', 'b'])
  })

  it('caps the snapshot at MAX_MODEL_CALLS_IN_MEMORY, dropping the oldest', () => {
    const log = new ModelCallLog()
    for (let i = 1; i <= MAX_MODEL_CALLS_IN_MEMORY + 3; i++) log.ingest(call({ model_id: `m${i}` }))
    const ids = log.snapshot().map((c) => c.model_id)
    expect(ids).toHaveLength(MAX_MODEL_CALLS_IN_MEMORY)
    expect(ids[ids.length - 1]).toBe(`m${MAX_MODEL_CALLS_IN_MEMORY + 3}`)
    expect(ids).not.toContain('m1')
  })

  it('subscribe() fires once per ingested call, in order, until unsubscribed', () => {
    const log = new ModelCallLog()
    const listener = vi.fn()
    const unsubscribe = log.subscribe(listener)
    log.ingest(call({ model_id: 'a' }))
    unsubscribe()
    log.ingest(call({ model_id: 'b' }))
    expect(listener).toHaveBeenCalledTimes(1)
    expect(listener).toHaveBeenCalledWith(call({ model_id: 'a' }))
  })
})
