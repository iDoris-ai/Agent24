/** ADR-038 D5 lifecycle. The same set the daemon's `ComponentState` serializes. */
export const COMPONENT_STATES = [
  'absent', 'installing', 'installed', 'starting', 'ready',
  'degraded', 'stopping', 'stopped', 'incompatible', 'failed',
] as const

export type ComponentState = (typeof COMPONENT_STATES)[number]

export function isComponentState(value: unknown): value is ComponentState {
  return typeof value === 'string' && (COMPONENT_STATES as readonly string[]).includes(value)
}
