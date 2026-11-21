// M1 (0.6.82, General 11-21): a patch the card sent back (its anchor did not
// open the paragraph) and the model then answered 「已保存为新文档」 with nothing
// made. What the page saw of a run's patches is kept here, so the answer
// carries a line the model cannot talk away, as K2's 「没有检索知识库」 does.

type Outcome = 'sentBack' | 'approved'

const outcomes = new Map<string, Outcome>()

/** The card sent this run's patch back to the model. */
export function noteSentBack(runId: string): void {
  if (outcomes.get(runId) !== 'approved') outcomes.set(runId, 'sentBack')
}

/** The user confirmed a patch of this run (after any sent back). */
export function noteApproved(runId: string): void {
  outcomes.set(runId, 'approved')
}

/** A patch of this run was sent back and none was confirmed after it: the
 * document was not changed, whatever the answer says. */
export function documentUnchanged(runId: string | undefined | null): boolean {
  return !!runId && outcomes.get(runId) === 'sentBack'
}

export function resetPatchOutcomesForTests(): void {
  outcomes.clear()
}
