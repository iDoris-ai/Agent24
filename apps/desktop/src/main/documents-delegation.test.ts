import { describe, expect, it, vi } from 'vitest'
import { withDocumentsDelegation } from './documents-delegation'

const refused = { ok: false, error: { code: 'module_delegation_required' } }

describe('documents delegation in service mode', () => {
  it('asks once for delegation, then makes the call again', async () => {
    const call = vi.fn().mockResolvedValueOnce(refused).mockResolvedValueOnce({ ok: true })
    const delegate = vi.fn().mockResolvedValue(true)
    expect(await withDocumentsDelegation(call, delegate)).toEqual({ ok: true })
    expect(call).toHaveBeenCalledTimes(2)
    expect(delegate).toHaveBeenCalledTimes(1)
  })

  it('keeps the refusal when the delegation is declined', async () => {
    const call = vi.fn().mockResolvedValue(refused)
    expect(await withDocumentsDelegation(call, async () => false)).toEqual(refused)
    expect(call).toHaveBeenCalledTimes(1)
  })

  it('does not ask for anything else', async () => {
    const delegate = vi.fn()
    for (const answer of [{ ok: true }, { ok: false, error: { code: 'not_found' } }, null]) {
      expect(await withDocumentsDelegation(async () => answer, delegate)).toEqual(answer)
    }
    expect(delegate).not.toHaveBeenCalled()
  })
})
