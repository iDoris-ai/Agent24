import { describe, expect, it, vi } from 'vitest'
import { knowledgePending } from './knowledge-pending'

describe('knowledgePending (M4 A7)', () => {
  it('writes a confirm item with the time, and clears it by the same id', () => {
    const store = { setPending: vi.fn(), clearPending: vi.fn() }
    const pending = knowledgePending(() => store, () => Date.UTC(2026, 10, 9, 8, 30), () => {})
    pending.set('chat_a', 'ked_x', 'Leave policy')
    expect(store.setPending).toHaveBeenCalledWith('chat_a', { id: 'ked_x', kind: 'confirm', label: 'Leave policy', at: '2026-11-09T08:30:00.000Z' })
    pending.clear('chat_a', 'ked_x')
    expect(store.clearPending).toHaveBeenCalledWith('chat_a', 'ked_x')
  })

  it('a write that fails is logged, not thrown, and says nothing of the label', () => {
    const log = vi.fn()
    const store = {
      setPending: vi.fn(() => { throw new Error('Unknown chat history id') }),
      clearPending: vi.fn(() => { throw new Error('io') }),
    }
    const pending = knowledgePending(() => store, Date.now, log)
    expect(() => pending.set('chat_gone', 'ked_x', 'Secret title')).not.toThrow()
    expect(() => pending.clear('chat_gone', 'ked_x')).not.toThrow()
    expect(log).toHaveBeenCalledTimes(2)
    expect(log.mock.calls.flat().join(' ')).not.toContain('Secret title')
  })
})
