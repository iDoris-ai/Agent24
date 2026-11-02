// Read-only client for GET /api/v1/components. The host report POST is not
// referenced here: main calls it with the ProductHost credential, and the
// proxy allowlist does not forward it.

import { isComponentState, type ComponentState } from '../../shared/component-state'

export interface ComponentProvide {
  id: string
}

/** Host or redacted row. `form` and `reason` are present only on the host view. */
export interface ComponentView {
  id: string
  version: string | null
  state: ComponentState
  form?: string
  provides: ComponentProvide[]
  reason?: string
}

export class ComponentApiError extends Error {
  constructor(readonly code: 'backend_not_ready' | 'request_failed' | 'invalid_response') {
    super(code)
  }
}

const FORMS = ['resident', 'sidecar', 'cli', 'attach', 'remote', 'mcp'] as const

function object(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) throw new ComponentApiError('invalid_response')
  return value as Record<string, unknown>
}

function parseProvide(value: unknown): ComponentProvide {
  const row = object(value)
  if (typeof row.id !== 'string' || row.id.length === 0) throw new ComponentApiError('invalid_response')
  return { id: row.id }
}

export function parseComponentView(value: unknown): ComponentView {
  const row = object(value)
  if (typeof row.id !== 'string' || row.id.length === 0 || !isComponentState(row.state)) {
    throw new ComponentApiError('invalid_response')
  }
  if (row.version !== null && typeof row.version !== 'string') throw new ComponentApiError('invalid_response')
  if (!Array.isArray(row.provides)) throw new ComponentApiError('invalid_response')
  const view: ComponentView = {
    id: row.id,
    version: row.version,
    state: row.state,
    provides: row.provides.map(parseProvide),
  }
  if (row.form !== undefined) {
    if (typeof row.form !== 'string' || !(FORMS as readonly string[]).includes(row.form)) {
      throw new ComponentApiError('invalid_response')
    }
    view.form = row.form
  }
  if (row.reason !== undefined) {
    if (typeof row.reason !== 'string') throw new ComponentApiError('invalid_response')
    view.reason = row.reason
  }
  return view
}

export function parseComponentList(value: unknown): ComponentView[] {
  const row = object(value)
  if (!Array.isArray(row.components)) throw new ComponentApiError('invalid_response')
  return row.components.map(parseComponentView)
}

/** GET /api/v1/components. Throws ComponentApiError; never calls the report route. */
export async function listComponents(): Promise<ComponentView[]> {
  let response
  try {
    response = await window.agent24.backendProxy({ method: 'GET', path: '/api/v1/components' })
  } catch {
    throw new ComponentApiError('backend_not_ready')
  }
  if (!response.ok) throw new ComponentApiError('request_failed')
  return parseComponentList(response.data)
}
