import { beforeEach, describe, expect, it } from 'vitest'
import { documentUnchanged, noteApproved, noteSentBack, resetPatchOutcomesForTests } from './patch-outcome'

describe('patch outcome (M1, 0.6.82: 「已保存」 with nothing made)', () => {
  beforeEach(() => resetPatchOutcomesForTests())
  it('a run whose patch was sent back and none confirmed after changed no document', () => {
    noteSentBack('run_1')
    expect(documentUnchanged('run_1')).toBe(true)
    expect(documentUnchanged('run_2')).toBe(false)
    expect(documentUnchanged(undefined)).toBe(false)
  })
  it('a patch confirmed after it (the model retried right) clears it, and a later send-back does not undo that', () => {
    noteSentBack('run_1')
    noteApproved('run_1')
    expect(documentUnchanged('run_1')).toBe(false)
    noteSentBack('run_1')
    expect(documentUnchanged('run_1')).toBe(false)
  })
})
