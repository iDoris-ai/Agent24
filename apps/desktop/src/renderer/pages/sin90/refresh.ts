/** Kinds that mean “the lists may have changed”. The page refetches; it does not apply the body. */
export function sin90ShouldRefresh(kind: string): boolean {
  if (kind === 'connected' || kind === 'routine.fired') return true
  if (kind === 'routine.paused' || kind === 'routine.resumed' || kind === 'routine.retired') return true
  if (kind === 'proposal.submitted' || kind === 'proposal.applied' || kind === 'proposal.rejected') return true
  return kind.endsWith('.created') || kind.endsWith('.transitioned') || kind.endsWith('.updated')
}
