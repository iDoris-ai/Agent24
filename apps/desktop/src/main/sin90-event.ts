// Sin90 frames on the one events socket. The page refetches; the body is only a hint.

import type { ModuleEvent } from './agentear-events'

export interface Sin90Forward {
  kind: string
  payload: Record<string, unknown>
}

export function sin90Event(event: ModuleEvent): Sin90Forward | null {
  if (event.module !== 'sin90') return null
  return { kind: event.kind, payload: event.payload }
}
